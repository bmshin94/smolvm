//! Content-addressed checkpoint directories. Each directory owns hard links to
//! every object it needs; deleting the cache or an older checkpoint cannot
//! invalidate a newer one. Export materializes a standalone pack on demand.

use serde::{Deserialize, Serialize};
use smolvm_pack::format::PackManifest;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path};

const CHUNK_SIZE: usize = 1024 * 1024;
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const INDEX: &str = "checkpoint.json";
const CAPTURE_MARKER: &str = ".capture-owner";
const CAPTURE_MAGIC: &[u8] = b"smolvm-checkpoint-capture-v1\n";
const OBJECT_STAGING_PREFIX: &str = ".checkpoint-object-";
const OBJECT_FLUSH_BATCH: usize = 8;
const OBJECT_FLUSH_STACK: usize = 256 * 1024;

struct PendingObject {
    temp: tempfile::NamedTempFile,
    hash: String,
    // Keep the original bytes for exact verification if another writer wins
    // publication. At most eight 1 MiB inputs are retained per ingest call.
    expected: Vec<u8>,
    compressed_size: usize,
}

fn sync_pending_objects(
    pending: &[PendingObject],
    sync: &(impl Fn(&File) -> io::Result<()> + Sync),
) -> io::Result<()> {
    if pending.len() > OBJECT_FLUSH_BATCH {
        return Err(invalid("checkpoint flush batch exceeds its bound"));
    }
    if pending.len() <= 1 {
        for object in pending {
            sync(object.temp.as_file())?;
        }
        return Ok(());
    }
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(pending.len());
        let mut failure = None;
        for object in pending {
            match std::thread::Builder::new()
                .name("checkpoint-flush".into())
                .stack_size(OBJECT_FLUSH_STACK)
                .spawn_scoped(scope, || sync(object.temp.as_file()))
            {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        // Join every started job, including after an error, before dropping
        // temporary files or allowing the caller to publish any reference.
        for handle in handles {
            let result = handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("checkpoint flush worker panicked")));
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredFile {
    path: String,
    size: u64,
    mode: u32,
    // None represents zeros, including a page that was nonzero previously.
    chunks: Vec<Option<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    chunk_size: usize,
    manifest: PackManifest,
    files: Vec<StoredFile>,
}

#[derive(Default, Debug)]
pub(crate) struct WriteStats {
    pub new_bytes: u64,
    pub new_logical_bytes: u64,
    pub reused_bytes: u64,
    pub zero_bytes: u64,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

#[test]
fn checkpoint_hashes_match_existing_sha256_objects() {
    use sha2::{Digest, Sha256};
    let bytes: Vec<_> = (0..CHUNK_SIZE + 1)
        .map(|index| ((index * 73 + index / 127) & 255) as u8)
        .collect();
    for len in [0, 1, 55, 56, 63, 64, 65, 1024, CHUNK_SIZE, CHUNK_SIZE + 1] {
        assert_eq!(
            digest(&bytes[..len]),
            hex::encode(Sha256::digest(&bytes[..len]))
        );
    }
    assert_eq!(
        digest(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Make a freshly written object durable enough to publish.
///
/// On macOS `File::sync_all` is `fcntl(F_FULLFSYNC)`: a flush of the whole
/// drive's write cache, not just this file. Measured on APFS it costs ~4.9 ms
/// per call, so a 1.5 GB incremental capture (~6,000 objects) spent ~30 s of
/// its 64 s doing device flushes one object at a time. A plain `fsync` (~0.2 ms)
/// gets the object's data and metadata to the drive, and [`Writer::finish`]
/// issues the one `F_FULLFSYNC` — through the index's `sync_all` — that pushes
/// the drive cache to stable storage for every object written before it. The
/// index is what makes objects reachable, so nothing can reference an object
/// that the final flush did not cover. Other platforms keep `sync_all`, where
/// it is already a per-file operation.
fn sync_object(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()
    }
}

fn decode_object(path: &Path, size: usize) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > (CHUNK_SIZE + 128 * 1024) as u64
    {
        return Err(invalid("checkpoint object type or length mismatch"));
    }
    let mut bytes = Vec::with_capacity(size);
    let mut decoder = zstd::stream::read::Decoder::new(File::open(path)?)?;
    decoder.window_log_max(23)?;
    decoder.take(size as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() != size {
        return Err(invalid("checkpoint object checksum mismatch"));
    }
    Ok(bytes)
}

fn read_object(path: &Path, hash: &str, size: usize) -> io::Result<Vec<u8>> {
    let bytes = decode_object(path, size)?;
    if digest(&bytes) != hash {
        return Err(invalid("checkpoint object checksum mismatch"));
    }
    Ok(bytes)
}

/// Capture already computed the object key from `expected`. Exact byte
/// equality verifies the existing object against that same input without
/// hashing it twice. Restore has no trusted input and must use read_object.
fn verify_object_matches(path: &Path, expected: &[u8]) -> io::Result<()> {
    let bytes = decode_object(path, expected.len())?;
    if bytes != expected {
        return Err(invalid("checkpoint object checksum mismatch"));
    }
    Ok(())
}

/// A capture writes into a private staging directory and publishes it only
/// after the RAM stream, CPU/device state and disk snapshots have completed.
pub(crate) struct Writer {
    _lock: File,
    cache: std::path::PathBuf,
    objects: std::path::PathBuf,
    failed: bool,
    pub stats: WriteStats,
}

impl Writer {
    pub fn new(cache: &Path, directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(cache)?;
        let lock = cache_lock(cache, false)?;
        let mut marker = File::options()
            .write(true)
            .create_new(true)
            .open(directory.join(CAPTURE_MARKER))?;
        marker.write_all(CAPTURE_MAGIC)?;
        marker.sync_all()?;
        let cache = cache.join("objects");
        fs::create_dir_all(&cache)?;
        let objects = directory.join("objects");
        fs::create_dir(&objects)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if fs::metadata(&cache)?.dev() != fs::metadata(&objects)?.dev() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "checkpoint output and store must be on the same filesystem",
                ));
            }
        }
        Ok(Self {
            _lock: lock,
            cache,
            objects,
            failed: false,
            stats: WriteStats::default(),
        })
    }

    pub fn ingest(
        &mut self,
        path: &str,
        size: u64,
        mode: u32,
        source: &mut impl Read,
    ) -> io::Result<StoredFile> {
        if self.failed {
            return Err(invalid("checkpoint writer previously failed"));
        }
        self.failed = true;
        let result = self.ingest_inner(path, size, mode, source);
        if result.is_ok() {
            self.failed = false;
        }
        result
    }

    fn ingest_inner(
        &mut self,
        path: &str,
        size: u64,
        mode: u32,
        source: &mut impl Read,
    ) -> io::Result<StoredFile> {
        if !safe_relative(path) || size > MAX_BYTES {
            return Err(invalid("invalid checkpoint file path or size"));
        }
        let mut chunks = Vec::new();
        let mut remaining = size;
        let mut buffer = vec![0; CHUNK_SIZE];
        let mut pending: Vec<PendingObject> = Vec::with_capacity(OBJECT_FLUSH_BATCH);
        while remaining != 0 {
            let count = remaining.min(CHUNK_SIZE as u64) as usize;
            source.read_exact(&mut buffer[..count])?;
            let bytes = &buffer[..count];
            if bytes.iter().all(|b| *b == 0) {
                self.stats.zero_bytes += count as u64;
                chunks.push(None);
            } else {
                let hash = digest(bytes);
                let cached = self.cache.join(&hash);
                if let Some(object) = pending.iter().find(|object| object.hash == hash) {
                    if object.expected != bytes {
                        return Err(invalid("checkpoint object checksum mismatch"));
                    }
                    self.stats.reused_bytes += count as u64;
                    chunks.push(Some(hash));
                    remaining -= count as u64;
                    continue;
                }
                match verify_object_matches(&cached, bytes) {
                    Ok(_) => {
                        self.stats.reused_bytes += count as u64;
                        self.link_object(&hash, bytes)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let mut temp = tempfile::Builder::new()
                            .prefix(OBJECT_STAGING_PREFIX)
                            .tempfile_in(&self.cache)?;
                        let compressed = zstd::bulk::compress(bytes, 3)?;
                        temp.write_all(&compressed)?;
                        pending.push(PendingObject {
                            temp,
                            hash: hash.clone(),
                            expected: bytes.to_vec(),
                            compressed_size: compressed.len(),
                        });
                        if pending.len() == OBJECT_FLUSH_BATCH {
                            self.flush_pending(&mut pending, &sync_object)?;
                        }
                    }
                    Err(error) => return Err(error),
                }
                chunks.push(Some(hash));
            }
            remaining -= count as u64;
        }
        self.flush_pending(&mut pending, &sync_object)?;
        Ok(StoredFile {
            path: path.into(),
            size,
            mode: mode & 0o777,
            chunks,
        })
    }

    fn link_object(&self, hash: &str, expected: &[u8]) -> io::Result<()> {
        let linked = self.objects.join(hash);
        match fs::hard_link(self.cache.join(hash), &linked) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                verify_object_matches(&linked, expected)
            }
            // No cross-device copy fallback: every checkpoint owns its links.
            Err(error) => Err(error),
        }
    }

    fn flush_pending(
        &mut self,
        pending: &mut Vec<PendingObject>,
        sync: &(impl Fn(&File) -> io::Result<()> + Sync),
    ) -> io::Result<()> {
        sync_pending_objects(pending, sync)?;
        for object in pending.drain(..) {
            let cached = self.cache.join(&object.hash);
            match object.temp.persist_noclobber(&cached) {
                Ok(_) => {
                    self.stats.new_bytes += object.compressed_size as u64;
                    self.stats.new_logical_bytes += object.expected.len() as u64;
                }
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                    verify_object_matches(&cached, &object.expected)?;
                    self.stats.reused_bytes += object.expected.len() as u64;
                }
                Err(error) => return Err(error.error),
            }
            self.link_object(&object.hash, &object.expected)?;
        }
        Ok(())
    }

    /// Consume the libkrun stream before accepting its final success response.
    pub fn ingest_memory(
        &mut self,
        source: &mut impl Read,
        max_size: u64,
    ) -> io::Result<StoredFile> {
        let mut header = [0; 16];
        source.read_exact(&mut header)?;
        if &header[..8] != b"SMOLRAM1" {
            return Err(invalid("runtime does not support checkpoint RAM streaming"));
        }
        let size = u64::from_le_bytes(header[8..].try_into().unwrap());
        if size == 0 || size > max_size.min(MAX_BYTES) {
            return Err(invalid(
                "checkpoint RAM stream exceeds configured memory layout",
            ));
        }
        self.ingest("checkpoint/memory.bin", size, 0o600, source)
    }

    pub fn ingest_tree(&mut self, root: &Path) -> io::Result<Vec<StoredFile>> {
        fn visit(
            writer: &mut Writer,
            root: &Path,
            dir: &Path,
            result: &mut Vec<StoredFile>,
        ) -> io::Result<()> {
            let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    visit(writer, root, &entry.path(), result)?;
                } else if kind.is_file() {
                    #[cfg(unix)]
                    let mode = {
                        use std::os::unix::fs::PermissionsExt;
                        entry.metadata()?.permissions().mode()
                    };
                    #[cfg(not(unix))]
                    let mode = 0o600;
                    let path = entry.path();
                    let relative = path
                        .strip_prefix(root)
                        .map_err(|_| invalid("checkpoint path escaped staging"))?;
                    let relative = relative
                        .to_str()
                        .ok_or_else(|| invalid("non-UTF8 checkpoint asset"))?;
                    result.push(writer.ingest_file(relative, mode, &mut File::open(&path)?)?);
                } else {
                    return Err(invalid("checkpoint staging contains a non-regular asset"));
                }
            }
            Ok(())
        }
        let mut files = Vec::new();
        visit(self, root, root, &mut files)?;
        Ok(files)
    }

    // Disk images and templates may be tens of GiB logically but mostly holes.
    // Preserve those holes without reading and scanning their logical zeros.
    fn ingest_file(&mut self, path: &str, mode: u32, source: &mut File) -> io::Result<StoredFile> {
        let size = source.metadata()?.len();
        if !safe_relative(path) || size > MAX_BYTES {
            return Err(invalid("invalid checkpoint asset"));
        }
        let mut offset = 0;
        let mut chunks = Vec::new();
        #[cfg(unix)]
        let mut seek_sparse = true;
        while offset < size {
            let count = (size - offset).min(CHUNK_SIZE as u64);
            #[cfg(unix)]
            let hole = if seek_sparse {
                use std::os::fd::AsRawFd;
                let data = unsafe {
                    libc::lseek(source.as_raw_fd(), offset as libc::off_t, libc::SEEK_DATA)
                };
                if data >= 0 {
                    data as u64 >= offset + count
                } else {
                    let error = io::Error::last_os_error();
                    match error.raw_os_error() {
                        Some(libc::ENXIO) => true,
                        Some(libc::EINVAL) | Some(libc::ENOTSUP) => {
                            seek_sparse = false;
                            false
                        }
                        _ => return Err(error),
                    }
                }
            } else {
                false
            };
            #[cfg(not(unix))]
            let hole = false;
            if hole {
                chunks.push(None);
                self.stats.zero_bytes += count;
            } else {
                source.seek(SeekFrom::Start(offset))?;
                chunks.extend(self.ingest(path, count, mode, source)?.chunks);
            }
            offset += count;
        }
        Ok(StoredFile {
            path: path.into(),
            size,
            mode: mode & 0o777,
            chunks,
        })
    }

    pub fn finish(
        &mut self,
        directory: &Path,
        manifest: PackManifest,
        files: Vec<StoredFile>,
    ) -> io::Result<WriteStats> {
        if self.failed {
            return Err(invalid("cannot finish a failed checkpoint writer"));
        }
        let index = Index {
            version: VERSION,
            chunk_size: CHUNK_SIZE,
            manifest,
            files,
        };
        validate_index(&index)?;
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(directory.join(INDEX))?;
        serde_json::to_writer(&mut file, &index)?;
        file.sync_all()?;
        File::open(&self.objects)?.sync_all()?;
        File::open(&self.cache)?.sync_all()?;
        File::open(directory)?.sync_all()?;
        Ok(std::mem::take(&mut self.stats))
    }
}

fn cache_lock(cache: &Path, exclusive: bool) -> io::Result<File> {
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join(".lock"))?;
    if exclusive {
        lock.lock()?;
    } else {
        lock.lock_shared()?;
    }
    Ok(lock)
}

/// Remove cache objects that no retained checkpoint references. Captures hold
/// a shared lock until all hard links are durable; pruning takes it exclusively.
#[cfg(unix)]
pub fn prune(cache: &Path) -> io::Result<u64> {
    let _lock = cache_lock(cache, true)?;
    let staging = cache.join("staging");
    if staging.is_dir() {
        for entry in fs::read_dir(&staging)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".checkpoint-")
                && fs::read(entry.path().join(CAPTURE_MARKER)).ok().as_deref()
                    == Some(CAPTURE_MAGIC)
            {
                // No capture can hold this store's shared lock here. These are
                // unpublished directories left by a terminated capture.
                fs::remove_dir_all(entry.path())?;
            }
        }
        File::open(&staging)?.sync_all()?;
    }
    let objects = cache.join("objects");
    let mut removed = 0;
    for entry in fs::read_dir(&objects)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(OBJECT_STAGING_PREFIX) {
            removed += entry.metadata()?.len();
            fs::remove_file(entry.path())?;
            continue;
        }
        if name.len() != 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            continue;
        }
        let meta = entry.metadata()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() == 1 {
                fs::remove_file(entry.path())?;
                removed += meta.len();
            }
        }
    }
    File::open(&objects)?.sync_all()?;
    Ok(removed)
}

/// Stored checkpoint creation and pruning require Linux or macOS.
#[cfg(not(unix))]
pub fn prune(_cache: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stored checkpoints require Linux or macOS",
    ))
}

fn validate_index(index: &Index) -> io::Result<()> {
    if index.version != VERSION || index.chunk_size != CHUNK_SIZE || index.files.len() > 100_000 {
        return Err(invalid("unsupported or oversized checkpoint index"));
    }
    let mut paths = HashSet::new();
    let mut total = 0_u64;
    for file in &index.files {
        total = total
            .checked_add(file.size)
            .ok_or_else(|| invalid("checkpoint size overflow"))?;
        if total > MAX_BYTES
            || !safe_relative(&file.path)
            || !paths.insert(&file.path)
            || file.chunks.len() as u64 != file.size.div_ceil(CHUNK_SIZE as u64)
            || file.mode & !0o777 != 0
        {
            return Err(invalid("invalid checkpoint file index"));
        }
        for hash in file.chunks.iter().flatten() {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid("invalid checkpoint object digest"));
            }
        }
    }
    Ok(())
}

fn read_index(directory: &Path) -> io::Result<Index> {
    let path = directory.join(INDEX);
    if fs::symlink_metadata(&path)?.len() > 256 * 1024 * 1024 {
        return Err(invalid("checkpoint index too large"));
    }
    let index: Index = serde_json::from_reader(File::open(path)?)?;
    validate_index(&index)?;
    Ok(index)
}

/// Read and validate the object index before allocating restore resources.
pub fn read_manifest(directory: &Path) -> io::Result<PackManifest> {
    let manifest = read_index(directory)?.manifest;
    if manifest.checkpoint.is_none() {
        return Err(invalid("stored checkpoint has no live-state manifest"));
    }
    Ok(manifest)
}

/// Restore into a fresh private directory; never map writable VM memory from
/// an object shared with a retained checkpoint.
pub fn materialize(directory: &Path, output: &Path) -> io::Result<PackManifest> {
    let index = read_index(directory)?;
    fs::create_dir(output)?;
    for entry in &index.files {
        let destination = output.join(&entry.path);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| invalid("missing asset parent"))?,
        )?;
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let mut remaining = entry.size;
        for hash in &entry.chunks {
            let count = remaining.min(CHUNK_SIZE as u64) as usize;
            if let Some(hash) = hash {
                file.write_all(&read_object(
                    &directory.join("objects").join(hash),
                    hash,
                    count,
                )?)?;
            } else {
                file.seek(SeekFrom::Current(count as i64))?;
            }
            remaining -= count as u64;
        }
        file.set_len(entry.size)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(entry.mode))?;
        }
    }
    Ok(index.manifest)
}

pub(crate) fn logical_size(file: &StoredFile) -> u64 {
    file.size
}

/// Atomically publish without replacing an existing checkpoint, even when
/// another capture races with the initial existence check.
#[cfg(unix)]
pub(crate) fn publish(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let src = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let dst = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(src.as_ptr(), dst.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = -1;
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn publish(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stored checkpoints require Linux or macOS",
    ))
}

/// Make a stored directory independently transportable as a single file.
pub fn export(directory: &Path, output: &Path) -> io::Result<u64> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "checkpoint output exists",
        ));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".checkpoint-export-")
        .tempdir_in(parent)?;
    let staging = temporary.path().join("staging");
    let manifest = materialize(directory, &staging)?;
    let collector =
        smolvm_pack::assets::AssetCollector::new(staging).map_err(|e| invalid(e.to_string()))?;
    let artifact = temporary.path().join("export.smolcheckpoint");
    let info = smolvm_pack::packer::Packer::new(manifest)
        .with_asset_collector(collector)
        .pack_artifact(&artifact)
        .map_err(|e| invalid(e.to_string()))?;
    File::open(&artifact)?.sync_all()?;
    publish(&artifact, output)?;
    Ok(info.total_size)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn prune_reclaims_only_owned_abandoned_captures() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let staging = cache.join("staging");
        fs::create_dir_all(&staging).unwrap();
        let abandoned = staging.join(".checkpoint-abandoned");
        fs::create_dir(&abandoned).unwrap();
        let unrelated = staging.join(".checkpoint-user-data");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join("keep"), b"user data").unwrap();
        let mut writer = Writer::new(&cache, &abandoned).unwrap();
        writer
            .ingest("memory", 32, 0o600, &mut &[1u8; 32][..])
            .unwrap();
        assert_eq!(fs::read_dir(cache.join("objects")).unwrap().count(), 1);
        drop(writer);
        fs::write(
            cache
                .join("objects")
                .join(format!("{OBJECT_STAGING_PREFIX}interrupted")),
            b"partial compressed object",
        )
        .unwrap();
        assert!(prune(&cache).unwrap() > 0);
        assert!(!abandoned.exists());
        assert!(unrelated.join("keep").exists());
        assert_eq!(fs::read_dir(cache.join("objects")).unwrap().count(), 0);
    }

    fn manifest() -> PackManifest {
        PackManifest::new(
            "vm://test".into(),
            "none".into(),
            "linux/amd64".into(),
            "linux/amd64".into(),
        )
    }

    fn capture(cache: &Path, directory: &Path, bytes: &[u8]) -> WriteStats {
        fs::create_dir(directory).unwrap();
        let mut writer = Writer::new(cache, directory).unwrap();
        let file = writer
            .ingest(
                "checkpoint/memory.bin",
                bytes.len() as u64,
                0o600,
                &mut &*bytes,
            )
            .unwrap();
        writer.finish(directory, manifest(), vec![file]).unwrap()
    }

    /// Cached objects must contain the exact original bytes before reuse.
    #[test]
    fn a_truncated_cached_object_is_refused_rather_than_reused() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let bytes = vec![0x5A; CHUNK_SIZE];
        let stats = capture(&cache, &root.path().join("first"), &bytes);
        assert_eq!(stats.new_logical_bytes, CHUNK_SIZE as u64);

        let hash = digest(&bytes);
        let object = cache.join("objects").join(&hash);
        assert!(object.is_file(), "object was not published under its hash");
        fs::write(&object, b"").unwrap();

        let again = root.path().join("second");
        fs::create_dir(&again).unwrap();
        let mut writer = Writer::new(&cache, &again).unwrap();
        let error = writer
            .ingest(
                "checkpoint/memory.bin",
                bytes.len() as u64,
                0o600,
                &mut &*bytes,
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    /// The positive half: an intact object is reused without being rewritten.
    #[test]
    fn an_intact_cached_object_is_reused_without_rewriting() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let bytes = vec![0x5A; CHUNK_SIZE];
        capture(&cache, &root.path().join("first"), &bytes);
        let stats = capture(&cache, &root.path().join("second"), &bytes);
        assert_eq!(stats.reused_bytes, CHUNK_SIZE as u64);
        assert_eq!(stats.new_bytes, 0);
    }

    #[test]
    fn unchanged_chunks_are_reused_and_old_checkpoints_remain_independent() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let first = root.path().join("first");
        let second = root.path().join("second");
        let mut bytes = vec![0x11; 4 * CHUNK_SIZE + 17];
        bytes[CHUNK_SIZE..2 * CHUNK_SIZE].fill(0x22);
        bytes[2 * CHUNK_SIZE..3 * CHUNK_SIZE].fill(0x33);
        bytes[3 * CHUNK_SIZE..4 * CHUNK_SIZE].fill(0x44);
        let original = bytes.clone();
        let initial = capture(&cache, &first, &bytes);
        assert_eq!(initial.new_logical_bytes, bytes.len() as u64);
        assert!(initial.new_bytes < initial.new_logical_bytes);
        bytes[CHUNK_SIZE..2 * CHUNK_SIZE].fill(0);
        bytes[2 * CHUNK_SIZE + 7] = 0xFE;
        let delta = capture(&cache, &second, &bytes);
        assert_eq!(delta.new_logical_bytes, CHUNK_SIZE as u64);
        assert_eq!(delta.zero_bytes, CHUNK_SIZE as u64);
        assert_eq!(delta.reused_bytes, (2 * CHUNK_SIZE + 17) as u64);
        materialize(&first, &root.path().join("restore-first")).unwrap();
        assert_eq!(
            fs::read(root.path().join("restore-first/checkpoint/memory.bin")).unwrap(),
            original
        );
        fs::remove_dir_all(&first).unwrap();
        fs::remove_dir_all(&cache).unwrap();
        materialize(&second, &root.path().join("restore-second")).unwrap();
        assert_eq!(
            fs::read(root.path().join("restore-second/checkpoint/memory.bin")).unwrap(),
            bytes
        );
    }

    #[test]
    fn truncated_capture_never_publishes_an_index_and_retry_works() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let incomplete = root.path().join("incomplete");
        fs::create_dir(&incomplete).unwrap();
        let mut writer = Writer::new(&cache, &incomplete).unwrap();
        let error = writer
            .ingest(
                "memory",
                (2 * CHUNK_SIZE) as u64,
                0o600,
                &mut &[1; CHUNK_SIZE][..],
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert!(!incomplete.join(INDEX).exists());
        assert!(writer.finish(&incomplete, manifest(), vec![]).is_err());
        assert_eq!(fs::read_dir(cache.join("objects")).unwrap().count(), 0);
        let retry = capture(&cache, &root.path().join("retry"), &vec![1; CHUNK_SIZE]);
        // An incomplete batch stays private and is discarded on input error.
        assert!(retry.new_bytes > 0);
        assert_eq!(retry.reused_bytes, 0);
    }

    fn pending_objects(writer: &Writer, count: usize) -> Vec<PendingObject> {
        (0..count)
            .map(|index| {
                let expected = vec![index as u8 + 1; CHUNK_SIZE];
                let compressed = zstd::bulk::compress(&expected, 3).unwrap();
                let mut temp = tempfile::Builder::new()
                    .prefix(OBJECT_STAGING_PREFIX)
                    .tempfile_in(&writer.cache)
                    .unwrap();
                temp.write_all(&compressed).unwrap();
                PendingObject {
                    temp,
                    hash: digest(&expected),
                    expected,
                    compressed_size: compressed.len(),
                }
            })
            .collect()
    }

    #[test]
    fn flush_batches_remain_private_until_every_job_succeeds() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let saved = root.path().join("saved");
        fs::create_dir(&saved).unwrap();
        let mut writer = Writer::new(&root.path().join("cache"), &saved).unwrap();
        let mut pending = pending_objects(&writer, OBJECT_FLUSH_BATCH);
        let entered = AtomicUsize::new(0);
        let cache = writer.cache.clone();
        writer
            .flush_pending(&mut pending, &|file| {
                entered.fetch_add(1, Ordering::SeqCst);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while entered.load(Ordering::SeqCst) != OBJECT_FLUSH_BATCH {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "flush jobs did not overlap",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert_eq!(fs::read_dir(saved.join("objects")).unwrap().count(), 0);
                assert!(fs::read_dir(&cache).unwrap().all(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(OBJECT_STAGING_PREFIX)));
                sync_object(file)
            })
            .unwrap();
        assert!(pending.is_empty());
        assert_eq!(
            fs::read_dir(saved.join("objects")).unwrap().count(),
            OBJECT_FLUSH_BATCH
        );
        assert_eq!(
            writer.stats.new_logical_bytes,
            (OBJECT_FLUSH_BATCH * CHUNK_SIZE) as u64
        );
    }

    #[test]
    fn flush_failure_joins_all_jobs_and_publishes_no_objects() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for panic_worker in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let saved = root.path().join("saved");
            fs::create_dir(&saved).unwrap();
            let mut writer = Writer::new(&root.path().join("cache"), &saved).unwrap();
            let mut pending = pending_objects(&writer, OBJECT_FLUSH_BATCH);
            let calls = AtomicUsize::new(0);
            let completed = AtomicUsize::new(0);
            let result = writer.flush_pending(&mut pending, &|file| {
                let index = calls.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    if panic_worker {
                        panic!("injected checkpoint flush panic");
                    }
                    return Err(io::Error::other("injected checkpoint flush failure"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                sync_object(file)?;
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), OBJECT_FLUSH_BATCH);
            assert_eq!(completed.load(Ordering::SeqCst), OBJECT_FLUSH_BATCH - 1);
            assert_eq!(fs::read_dir(saved.join("objects")).unwrap().count(), 0);
            assert!(!saved.join(INDEX).exists());
            drop(pending);
            assert_eq!(fs::read_dir(&writer.cache).unwrap().count(), 0);
        }
    }

    #[test]
    fn concurrent_publication_is_verified_against_original_bytes() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let saved = root.path().join("saved");
            fs::create_dir(&saved).unwrap();
            let mut writer = Writer::new(&root.path().join("cache"), &saved).unwrap();
            let mut pending = pending_objects(&writer, 2);
            let winner = writer.cache.join(&pending[0].hash);
            let mut bytes = pending[0].expected.clone();
            if corrupt {
                bytes[CHUNK_SIZE - 1] ^= 1;
            }
            fs::write(&winner, zstd::bulk::compress(&bytes, 3).unwrap()).unwrap();
            File::open(&winner).unwrap().sync_all().unwrap();
            let result = writer.flush_pending(&mut pending, &sync_object);
            assert_eq!(result.is_err(), corrupt);
            if corrupt {
                assert!(!saved.join(INDEX).exists());
                assert_eq!(fs::read_dir(saved.join("objects")).unwrap().count(), 0);
            } else {
                assert_eq!(writer.stats.reused_bytes, CHUNK_SIZE as u64);
                assert_eq!(writer.stats.new_logical_bytes, CHUNK_SIZE as u64);
            }
        }
    }

    #[test]
    fn full_and_partial_flush_batches_preserve_duplicates_and_zeros() {
        let root = tempfile::tempdir().unwrap();
        let saved = root.path().join("saved");
        let mut bytes = vec![0; (2 * OBJECT_FLUSH_BATCH + 3) * CHUNK_SIZE + 17];
        for (index, chunk) in bytes.chunks_mut(CHUNK_SIZE).enumerate() {
            chunk.fill(index as u8 + 1);
        }
        bytes[2 * CHUNK_SIZE..3 * CHUNK_SIZE].fill(1);
        bytes[4 * CHUNK_SIZE..5 * CHUNK_SIZE].fill(0);
        let stats = capture(&root.path().join("cache"), &saved, &bytes);
        assert_eq!(stats.reused_bytes, CHUNK_SIZE as u64);
        assert_eq!(stats.zero_bytes, CHUNK_SIZE as u64);
        materialize(&saved, &root.path().join("restored")).unwrap();
        assert_eq!(
            fs::read(root.path().join("restored/checkpoint/memory.bin")).unwrap(),
            bytes
        );
    }

    #[test]
    fn interrupted_later_batch_keeps_only_previously_durable_objects() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let incomplete = root.path().join("incomplete");
        fs::create_dir(&incomplete).unwrap();
        let mut writer = Writer::new(&cache, &incomplete).unwrap();
        let mut bytes = vec![0; (OBJECT_FLUSH_BATCH + 2) * CHUNK_SIZE];
        for (index, chunk) in bytes.chunks_mut(CHUNK_SIZE).enumerate() {
            chunk.fill(index as u8 + 1);
        }
        assert_eq!(
            writer
                .ingest(
                    "memory",
                    bytes.len() as u64 + 1,
                    0o600,
                    &mut bytes.as_slice()
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(writer.finish(&incomplete, manifest(), vec![]).is_err());
        assert!(!incomplete.join(INDEX).exists());
        assert_eq!(
            fs::read_dir(&writer.cache).unwrap().count(),
            OBJECT_FLUSH_BATCH
        );
        assert_eq!(
            fs::read_dir(&writer.objects).unwrap().count(),
            OBJECT_FLUSH_BATCH
        );
        drop(writer);
        let retry = root.path().join("retry");
        let stats = capture(&cache, &retry, &bytes);
        assert_eq!(stats.reused_bytes, (OBJECT_FLUSH_BATCH * CHUNK_SIZE) as u64);
        assert_eq!(stats.new_logical_bytes, (2 * CHUNK_SIZE) as u64);
        materialize(&retry, &root.path().join("restored")).unwrap();
        assert_eq!(
            fs::read(root.path().join("restored/checkpoint/memory.bin")).unwrap(),
            bytes
        );
    }

    #[test]
    fn oversized_flush_batch_starts_no_jobs() {
        let root = tempfile::tempdir().unwrap();
        let saved = root.path().join("saved");
        fs::create_dir(&saved).unwrap();
        let writer = Writer::new(&root.path().join("cache"), &saved).unwrap();
        let pending = pending_objects(&writer, OBJECT_FLUSH_BATCH + 1);
        assert!(sync_pending_objects(&pending, &|_| panic!("unbounded job started")).is_err());
    }

    #[test]
    fn reused_objects_require_exact_decoded_bytes() {
        let root = tempfile::tempdir().unwrap();
        let expected = vec![7; CHUNK_SIZE];
        let object = root.path().join(digest(&expected));
        for len in [CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1] {
            let mut contents = vec![7; len];
            if len == CHUNK_SIZE {
                contents[len - 1] = 8;
            }
            fs::write(&object, zstd::bulk::compress(&contents, 3).unwrap()).unwrap();
            assert!(verify_object_matches(&object, &expected).is_err());
            assert!(read_object(&object, &digest(&expected), expected.len()).is_err());
        }
        fs::write(&object, zstd::bulk::compress(&expected, 3).unwrap()).unwrap();
        verify_object_matches(&object, &expected).unwrap();
        assert_eq!(
            read_object(&object, &digest(&expected), expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn corruption_is_rejected_on_reuse_and_restore() {
        for corrupt in [
            vec![8; CHUNK_SIZE],
            zstd::bulk::compress(&vec![8; CHUNK_SIZE], 3).unwrap(),
        ] {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let saved = root.path().join("saved");
            let bytes = vec![7; CHUNK_SIZE];
            capture(&cache, &saved, &bytes);
            fs::write(cache.join("objects").join(digest(&bytes)), corrupt).unwrap();
            assert!(materialize(&saved, &root.path().join("restore")).is_err());
            let next = root.path().join("next");
            fs::create_dir(&next).unwrap();
            let mut writer = Writer::new(&cache, &next).unwrap();
            assert!(writer
                .ingest("memory", bytes.len() as u64, 0o600, &mut bytes.as_slice())
                .is_err());
            assert!(!next.join(INDEX).exists());
        }
    }

    #[test]
    fn concurrent_writers_share_objects_without_losing_checkpoints() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        std::thread::scope(|scope| {
            for n in 0..8 {
                let cache = &cache;
                let directory = root.path().join(n.to_string());
                scope.spawn(move || {
                    capture(cache, &directory, &vec![3; CHUNK_SIZE + 5]);
                    materialize(&directory, &directory.join("restore")).unwrap();
                });
            }
        });
        assert_eq!(fs::read_dir(cache.join("objects")).unwrap().count(), 2);
    }

    #[test]
    fn malformed_indices_and_streams_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let saved = root.path().join("saved");
        capture(&root.path().join("cache"), &saved, b"test");
        let mut index = read_index(&saved).unwrap();
        index.files[0].path = "../outside".into();
        assert!(validate_index(&index).is_err());
        index.files[0].path = "memory".into();
        index.files[0].chunks[0] = Some("../outside".into());
        assert!(validate_index(&index).is_err());
        index.files[0].chunks.clear();
        assert!(validate_index(&index).is_err());
        let dest = root.path().join("stream");
        fs::create_dir(&dest).unwrap();
        let mut writer = Writer::new(&root.path().join("cache"), &dest).unwrap();
        let mut wire = b"SMOLRAM1".to_vec();
        wire.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(writer.ingest_memory(&mut wire.as_slice(), 1024).is_err());
        assert!(writer
            .ingest_memory(&mut &b"ERR ENOTSUP\n"[..], 1024)
            .is_err());
    }

    #[test]
    fn sparse_assets_preserve_unaligned_extents_and_trailing_holes() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("sparse.bin");
        let mut source = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&source_path)
            .unwrap();
        source.set_len((16 * CHUNK_SIZE + 23) as u64).unwrap();
        source
            .seek(SeekFrom::Start((2 * CHUNK_SIZE + 9) as u64))
            .unwrap();
        source.write_all(b"nonzero extent").unwrap();
        let saved = root.path().join("saved");
        fs::create_dir(&saved).unwrap();
        let mut writer = Writer::new(&root.path().join("cache"), &saved).unwrap();
        let entry = writer.ingest_file("disk.bin", 0o600, &mut source).unwrap();
        let stats = writer.finish(&saved, manifest(), vec![entry]).unwrap();
        assert_eq!(stats.new_logical_bytes, CHUNK_SIZE as u64);
        materialize(&saved, &root.path().join("restore")).unwrap();
        assert_eq!(
            fs::read(source_path).unwrap(),
            fs::read(root.path().join("restore/disk.bin")).unwrap()
        );
    }

    #[test]
    fn publish_does_not_replace_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(publish(&source, &destination).is_err());
        assert!(source.is_dir());
    }

    #[test]
    fn pruning_preserves_live_checkpoints_and_reclaims_deleted_ones() {
        let root = tempfile::tempdir().unwrap();
        let saved = root.path().join("saved");
        let cache = root.path().join("cache");
        capture(&cache, &saved, &vec![4; CHUNK_SIZE]);
        assert_eq!(prune(&cache).unwrap(), 0);
        materialize(&saved, &root.path().join("restore")).unwrap();
        fs::remove_dir_all(saved).unwrap();
        assert!(prune(&cache).unwrap() > 0);
        assert_eq!(prune(&cache).unwrap(), 0);
    }

    #[test]
    fn standalone_export_contains_exact_bytes_without_store() {
        let root = tempfile::tempdir().unwrap();
        let saved = root.path().join("saved");
        let cache = root.path().join("cache");
        let bytes = vec![0xA5; CHUNK_SIZE + 23];
        capture(&cache, &saved, &bytes);
        let artifact = root.path().join("export.smolcheckpoint");
        export(&saved, &artifact).unwrap();
        fs::remove_dir_all(&cache).unwrap();
        fs::remove_dir_all(&saved).unwrap();
        let footer = smolvm_pack::packer::read_footer_from_sidecar(&artifact).unwrap();
        let restored = root.path().join("restore");
        smolvm_pack::extract::extract_sidecar(&artifact, &restored, &footer, false, false).unwrap();
        assert_eq!(
            fs::read(restored.join("checkpoint/memory.bin")).unwrap(),
            bytes
        );
    }
}
