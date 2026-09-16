//! Measurement-only RAM-asset prototype, not a VM checkpoint implementation.

use super::safe_unpack;
use crate::artifact_writer::ArtifactWriter;
use crate::assets::ZSTD_LEVEL;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

struct ExactStream<R> {
    source: R,
    remaining: u64,
}

impl<R: Read> Read for ExactStream<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.remaining.min(buffer.len() as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let read = self.source.read(&mut buffer[..count])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short RAM stream",
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

#[test]
fn bounded_stream_rejects_truncation_and_preserves_following_reply() {
    let mut short = ExactStream {
        source: &b"short"[..],
        remaining: 16,
    };
    assert_eq!(
        io::copy(&mut short, &mut io::sink()).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    let mut valid = ExactStream {
        source: io::Cursor::new(b"RAM!OK saved\n"),
        remaining: 4,
    };
    assert_eq!(io::copy(&mut valid, &mut io::sink()).unwrap(), 4);
    assert_eq!(valid.source.position(), 4);
    let mut reply = String::new();
    valid.source.read_to_string(&mut reply).unwrap();
    assert_eq!(reply, "OK saved\n");
}

fn device_sectors_written() -> u64 {
    fs::read_to_string("/sys/class/block/nvme0n1/stat")
        .unwrap()
        .split_whitespace()
        .nth(6)
        .unwrap()
        .parse()
        .unwrap()
}

fn scan_extents(reader: &mut impl Read, logical: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut buffer = [0_u8; 65536];
    let mut extents: Vec<(u64, u64)> = Vec::new();
    let mut offset = 0;
    while offset < logical {
        let count = (logical - offset).min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..count])?;
        if buffer[..count].iter().any(|byte| *byte != 0) {
            match extents.last_mut() {
                Some((start, len)) if *start + *len == offset => *len += count as u64,
                _ => {
                    if extents.len() == 65536 {
                        return Err(io::Error::other("prototype sparse map limit exceeded"));
                    }
                    extents.push((offset, count as u64));
                }
            }
        }
        offset += count as u64;
    }
    extents.push((logical, 0));
    Ok(extents)
}

fn append_sparse<W: Write>(
    archive: &mut tar::Builder<W>,
    source: &mut (impl Read + Seek),
    logical: u64,
    extents: &[(u64, u64)],
) -> io::Result<()> {
    let stored: u64 = extents.iter().map(|(_, len)| *len).sum();
    let mut header = tar::Header::new_gnu();
    header.set_path("checkpoint/memory.bin")?;
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::GNUSparse);
    header.set_size(stored);
    let gnu = header.as_gnu_mut().unwrap();
    gnu.set_real_size(logical);
    for ((offset, len), slot) in extents.iter().zip(gnu.sparse.iter_mut()) {
        slot.set_offset(*offset);
        slot.set_length(*len);
    }
    gnu.set_is_extended(extents.len() > 4);
    header.set_cksum();
    archive.get_mut().write_all(header.as_bytes())?;
    let rest = &extents[extents.len().min(4)..];
    for (index, chunk) in rest.chunks(21).enumerate() {
        let mut extra = tar::GnuExtSparseHeader::new();
        for ((offset, len), slot) in chunk.iter().zip(extra.sparse_mut().iter_mut()) {
            slot.set_offset(*offset);
            slot.set_length(*len);
        }
        extra.set_is_extended((index + 1) * 21 < rest.len());
        archive.get_mut().write_all(extra.as_bytes())?;
    }
    for (offset, len) in extents {
        source.seek(SeekFrom::Start(*offset))?;
        io::copy(
            &mut ExactStream {
                source: &mut *source,
                remaining: *len,
            },
            archive.get_mut(),
        )?;
    }
    let padding = (512 - stored % 512) % 512;
    archive.get_mut().write_all(&[0; 512][..padding as usize])?;
    Ok(())
}

#[test]
fn scanned_sparse_map_roundtrips_fragmented_and_allocated_zero_pages() {
    for chunks in [1, 3, 61] {
        let mut bytes = vec![0; chunks * 65536 + 13];
        for index in (0..chunks).step_by(2) {
            bytes[index * 65536] = 42;
        }
        let logical = bytes.len() as u64;
        let mut source = io::Cursor::new(&bytes);
        let extents = scan_extents(&mut source, logical).unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        append_sparse(&mut archive, &mut source, logical, &extents).unwrap();
        let packed = archive.into_inner().unwrap();
        let destination = tempfile::tempdir().unwrap();
        safe_unpack(&mut tar::Archive::new(&packed[..]), destination.path()).unwrap();
        assert_eq!(
            fs::read(destination.path().join("checkpoint/memory.bin")).unwrap(),
            bytes
        );
    }
    assert_eq!(
        scan_extents(&mut &b"short"[..], 65536).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
#[ignore = "multi-GiB RAM-asset feasibility measurement; not an end-to-end VM test"]
fn streamed_memory_asset_roundtrip() {
    let probe_root =
        std::env::var("SMOLVM_ASSET_PROBE_ROOT").unwrap_or_else(|_| "/var/tmp".to_string());
    let root = tempfile::tempdir_in(&probe_root).unwrap();
    let resident = std::env::var("SMOLVM_ASSET_PROBE_MIB")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(1024)
        * 1024
        * 1024;
    assert!(resident > 0);
    let logical = 4 * resident;
    let source_path = root.path().join("source-memory");
    let mut source = File::create(&source_path).unwrap();
    source.set_len(logical).unwrap();
    let mut buffer = vec![0_u8; 1024 * 1024];
    for block in 0..resident / buffer.len() as u64 {
        for (index, word) in buffer.chunks_exact_mut(8).enumerate() {
            // Deterministic, non-cryptographic incompressible synthetic input.
            let mut x = block * 131072 + index as u64 + 0x9e3779b97f4a7c15;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
            word.copy_from_slice(&(x ^ (x >> 31)).to_le_bytes());
        }
        source.write_all(&buffer).unwrap();
    }
    source.sync_all().unwrap();
    drop(source);
    let orders = if std::env::var_os("SMOLVM_ASSET_PROBE_MATCHED_SCAN").is_some() {
        vec![vec![4, 3], vec![3, 4], vec![4, 3]]
    } else if std::env::var_os("SMOLVM_ASSET_PROBE_SCAN").is_some() {
        vec![vec![0, 3], vec![3, 0], vec![0, 3]]
    } else {
        vec![vec![0, 1, 2], vec![1, 2, 0], vec![2, 0, 1]]
    };
    for (repetition, order) in orders.into_iter().enumerate() {
        for mode in order {
            let streaming = mode != 0 && mode != 4;
            let row = tempfile::tempdir_in(root.path()).unwrap();
            let before = device_sectors_written();
            let started = Instant::now();
            let mut scan_ms = 0;
            if mode == 4 {
                let extents =
                    scan_extents(&mut File::open(&source_path).unwrap(), logical).unwrap();
                assert_eq!(extents, vec![(0, resident), (logical, 0)]);
                scan_ms = started.elapsed().as_millis();
            }
            let snapshot = row.path().join("memory.bin");
            let snapshot_started = Instant::now();
            let snapshot_ms = if streaming {
                0
            } else {
                let mut output = File::create(&snapshot).unwrap();
                output.set_len(logical).unwrap();
                let copied = io::copy(
                    &mut File::open(&source_path).unwrap().take(resident),
                    &mut output,
                )
                .unwrap();
                assert_eq!(copied, resident);
                output.sync_all().unwrap();
                snapshot_started.elapsed().as_millis()
            };
            let pack_started = Instant::now();
            let output_path = row.path().join("memory.tar.zst");
            let writer = ArtifactWriter::create(&output_path, true).unwrap();
            let mut encoder = zstd::stream::Encoder::new(writer, ZSTD_LEVEL).unwrap();
            let workers = std::thread::available_parallelism().unwrap().get().min(4);
            if workers > 1 {
                encoder.multithread(workers as u32).unwrap();
            }
            let mut archive = tar::Builder::new(encoder);
            if mode == 3 {
                let mut input = File::open(&source_path).unwrap();
                let scan_started = Instant::now();
                let extents = scan_extents(&mut input, logical).unwrap();
                scan_ms = scan_started.elapsed().as_millis();
                append_sparse(&mut archive, &mut input, logical, &extents).unwrap();
            } else if streaming {
                let mut header = tar::Header::new_gnu();
                header.set_size(logical);
                header.set_mode(0o600);
                if mode == 2 {
                    // Optimistic floor: this synthetic extent map is already known.
                    // Real VM extent discovery/transport is NOT measured here.
                    header.set_entry_type(tar::EntryType::GNUSparse);
                    header.set_size(resident);
                    let gnu = header.as_gnu_mut().unwrap();
                    gnu.set_real_size(logical);
                    gnu.sparse[0].set_offset(0);
                    gnu.sparse[0].set_length(resident);
                    gnu.sparse[1].set_offset(logical);
                    gnu.sparse[1].set_length(0);
                }
                header.set_cksum();
                let mut input = ExactStream {
                    source: File::open(&source_path).unwrap(),
                    remaining: if mode == 2 { resident } else { logical },
                };
                archive
                    .append_data(&mut header, "checkpoint/memory.bin", &mut input)
                    .unwrap();
                assert_eq!(input.remaining, 0);
            } else {
                archive
                    .append_path_with_name(&snapshot, "checkpoint/memory.bin")
                    .unwrap();
            }
            let file = archive
                .into_inner()
                .unwrap()
                .finish()
                .unwrap()
                .finish()
                .unwrap();
            let pack_ms = pack_started.elapsed().as_millis();
            let flush_started = Instant::now();
            file.sync_all().unwrap();
            File::open(row.path()).unwrap().sync_all().unwrap();
            let capture_flush_ms = flush_started.elapsed().as_millis();
            let capture_ms = started.elapsed().as_millis();
            let captured_sectors = device_sectors_written() - before;
            let artifact_bytes = file.metadata().unwrap().len();
            let snapshot_allocated = if streaming {
                0
            } else {
                fs::metadata(&snapshot).unwrap().blocks() * 512
            };
            let destination = row.path().join("restored");
            fs::create_dir(&destination).unwrap();
            let restore_started = Instant::now();
            let decoder = zstd::stream::Decoder::new(File::open(&output_path).unwrap()).unwrap();
            safe_unpack(&mut tar::Archive::new(decoder), &destination).unwrap();
            let extract_ms = restore_started.elapsed().as_millis();
            let restore_flush_started = Instant::now();
            let restored_path = destination.join("checkpoint/memory.bin");
            let restored = File::open(&restored_path).unwrap();
            restored.sync_all().unwrap();
            File::open(destination.join("checkpoint"))
                .unwrap()
                .sync_all()
                .unwrap();
            File::open(&destination).unwrap().sync_all().unwrap();
            let restore_flush_ms = restore_flush_started.elapsed().as_millis();
            let restore_ms = restore_started.elapsed().as_millis();
            let restored_allocated = restored.metadata().unwrap().blocks() * 512;
            assert_eq!(restored.metadata().unwrap().len(), logical);
            assert!(restored_allocated < resident + 64 * 1024 * 1024);
            let mut actual = restored;
            let mut expected = File::open(&source_path).unwrap();
            let mut comparison = vec![0; buffer.len()];
            let mut offset = 0;
            while offset < logical {
                actual.read_exact(&mut buffer).unwrap();
                expected.read_exact(&mut comparison).unwrap();
                assert!(buffer == comparison, "RAM mismatch at {offset}");
                offset += buffer.len() as u64;
            }
            println!(
                "{}",
                serde_json::json!({"probe":"RAM assets only, not VM checkpoint", "repetition":repetition+1,
                "probe_root":probe_root,
                "mode":(["materialized", "dense_stream", "known_sparse_stream", "scanned_sparse_stream", "scanned_materialized"][mode]),
                "streaming":streaming, "logical_bytes":logical, "resident_bytes":resident,
                "snapshot_ms":snapshot_ms, "scan_ms":scan_ms, "capture_ms":capture_ms, "restore_ms":restore_ms,
                "pack_including_stream_scan_ms":pack_ms, "capture_flush_ms":capture_flush_ms,
                "extract_ms":extract_ms, "restore_flush_ms":restore_flush_ms,
                "artifact_bytes":artifact_bytes, "snapshot_allocated":snapshot_allocated,
                "restored_allocated":restored_allocated, "device_capture_write_bytes":captured_sectors*512,
                "verification":"full logical byte comparison passed"})
            );
        }
    }
}
