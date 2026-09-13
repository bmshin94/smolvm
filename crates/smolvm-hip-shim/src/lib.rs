//! Drop-in `libamdhip64.so.7` for smolvm guests: a subset of the HIP Runtime
//! API, implemented by marshaling calls over smolvm's HIP RPC to the host.
//! Mirrors `smolvm-cuda-shim`'s structure and conventions; see that crate for
//! the mature reference this is scaffolded from.
//!
//! Install this library at `/usr/lib/libamdhip64.so.7` inside a guest (or
//! point `LD_LIBRARY_PATH` at it) and programs using this milestone's op set
//! — device query, `hipMalloc`/`hipFree`/`hipMemcpy`, `hipDeviceSynchronize`
//! — run on the host GPU with no code changes.
//!
//! Transport is chosen by `SMOLVM_HIP_RPC`:
//!   * unset / `vsock` — AF_VSOCK to host CID 2, port 7001 (production, in-guest)
//!   * `tcp:HOST:PORT` — TCP (host-side testing against a loopback server)
//!   * `unix:/path`    — AF_UNIX (host-side testing against a real socket)
//!
//! NOT implemented (see the crate docs on `smolvm-hip` for the milestone
//! boundary): kernel launch and module loading
//! (`hipModuleLoadData`/`hipModuleLaunchKernel`), hipBLAS/MIOpen/RCCL, and
//! everything else in the HIP surface not listed above — unlike
//! `smolvm-cuda-shim`, there is no generated long-tail stub file yet, so an
//! unmodified image that calls an unimplemented entry point will fail to
//! resolve the symbol at load/dlopen time rather than get a graceful
//! NOT_SUPPORTED. Building that stub generator (`smolvm-hip-codegen`,
//! mirroring `smolvm-cuda-codegen`) is follow-up work once the core RPC path
//! here is exercised end-to-end.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use smolvm_hip::client::{Client, HipRpcError};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::io::{Read, Write};
use std::sync::Mutex;

// ---- hipError_t codes the shim produces locally --------------------------

const HIP_SUCCESS: c_int = 0;
const HIP_ERROR_INVALID_VALUE: c_int = 1;
const HIP_ERROR_OUT_OF_MEMORY: c_int = 2;
const HIP_ERROR_NOT_INITIALIZED: c_int = 3;
const HIP_ERROR_NO_DEVICE: c_int = 100;
const HIP_ERROR_NOT_SUPPORTED: c_int = 801;

const HIP_MEMCPY_HOST_TO_DEVICE: c_int = 1;
const HIP_MEMCPY_DEVICE_TO_HOST: c_int = 2;
const HIP_MEMCPY_DEVICE_TO_DEVICE: c_int = 3;

fn hip_err(e: HipRpcError) -> c_int {
    match e {
        HipRpcError::Hip(code) => code,
        HipRpcError::Io(_) => HIP_ERROR_NO_DEVICE,
        HipRpcError::Protocol(_) => HIP_ERROR_NOT_SUPPORTED,
    }
}

// ---- transport -------------------------------------------------------------

enum Stream {
    #[cfg(target_os = "linux")]
    Vsock(vsock::VsockStream),
    Tcp(std::net::TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(target_os = "linux")]
            Stream::Vsock(s) => s.read(buf),
            Stream::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(target_os = "linux")]
            Stream::Vsock(s) => s.write(buf),
            Stream::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Stream::Vsock(s) => s.flush(),
            Stream::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
        }
    }
}

fn connect() -> Result<Stream, c_int> {
    let spec = std::env::var("SMOLVM_HIP_RPC").unwrap_or_default();
    if let Some(addr) = spec.strip_prefix("tcp:") {
        return std::net::TcpStream::connect(addr)
            .map(|s| {
                let _ = s.set_nodelay(true);
                Stream::Tcp(s)
            })
            .map_err(|_| HIP_ERROR_NO_DEVICE);
    }
    #[cfg(unix)]
    if let Some(path) = spec.strip_prefix("unix:") {
        return std::os::unix::net::UnixStream::connect(path)
            .map(Stream::Unix)
            .map_err(|_| HIP_ERROR_NO_DEVICE);
    }
    #[cfg(target_os = "linux")]
    {
        // smolvm's reserved HIP port on the host CID (smolvm_protocol::ports::HIP).
        const HOST_CID: u32 = 2;
        const HIP_PORT: u32 = 7001;
        vsock::VsockStream::connect_with_cid_port(HOST_CID, HIP_PORT)
            .map(Stream::Vsock)
            .map_err(|_| HIP_ERROR_NO_DEVICE)
    }
    #[cfg(not(target_os = "linux"))]
    Err(HIP_ERROR_NO_DEVICE)
}

// ---- global state ------------------------------------------------------------

static CLIENT: Mutex<Option<Client<Stream>>> = Mutex::new(None);

fn with_client<T>(
    f: impl FnOnce(&mut Client<Stream>) -> Result<T, HipRpcError>,
) -> Result<T, c_int> {
    let mut guard = CLIENT.lock().unwrap();
    if guard.is_none() {
        let stream = connect()?;
        *guard = Some(Client::new(stream));
    }
    let client = guard.as_mut().unwrap();
    f(client).map_err(hip_err)
}

// ---- HIP Runtime API entry points ---------------------------------------------

#[no_mangle]
pub extern "C" fn hipInit(_flags: c_int) -> c_int {
    match with_client(|c| c.init()) {
        Ok(()) => HIP_SUCCESS,
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipGetDeviceCount(count: *mut c_int) -> c_int {
    if count.is_null() {
        return HIP_ERROR_INVALID_VALUE;
    }
    match with_client(|c| c.device_get_count()) {
        Ok(n) => {
            unsafe { *count = n };
            HIP_SUCCESS
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipDeviceGetName(name: *mut c_char, len: c_int, device: c_int) -> c_int {
    if name.is_null() || len <= 0 {
        return HIP_ERROR_INVALID_VALUE;
    }
    match with_client(|c| c.device_get_name(device)) {
        Ok(s) => {
            let bytes = s.as_bytes();
            let n = bytes.len().min(len as usize - 1);
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), name as *mut u8, n);
                *name.add(n) = 0;
            }
            HIP_SUCCESS
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipDeviceTotalMem(bytes: *mut u64, device: c_int) -> c_int {
    if bytes.is_null() {
        return HIP_ERROR_INVALID_VALUE;
    }
    match with_client(|c| c.device_total_mem(device)) {
        Ok(b) => {
            unsafe { *bytes = b };
            HIP_SUCCESS
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipMalloc(ptr: *mut *mut c_void, size: u64) -> c_int {
    if ptr.is_null() {
        return HIP_ERROR_INVALID_VALUE;
    }
    match with_client(|c| c.mem_alloc(size)) {
        Ok(dptr) => {
            unsafe { *ptr = dptr as *mut c_void };
            HIP_SUCCESS
        }
        Err(HIP_ERROR_OUT_OF_MEMORY) => HIP_ERROR_OUT_OF_MEMORY,
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipFree(ptr: *mut c_void) -> c_int {
    match with_client(|c| c.mem_free(ptr as u64)) {
        Ok(()) => HIP_SUCCESS,
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipMemcpy(
    dst: *mut c_void,
    src: *const c_void,
    size_bytes: u64,
    kind: c_int,
) -> c_int {
    let result = match kind {
        HIP_MEMCPY_HOST_TO_DEVICE => {
            let data = unsafe { std::slice::from_raw_parts(src as *const u8, size_bytes as usize) };
            with_client(|c| c.memcpy_h2d(dst as u64, data))
        }
        HIP_MEMCPY_DEVICE_TO_HOST => {
            with_client(|c| c.memcpy_d2h(src as u64, size_bytes)).map(|data| unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst as *mut u8, data.len());
            })
        }
        HIP_MEMCPY_DEVICE_TO_DEVICE => {
            with_client(|c| c.memcpy_d2d(dst as u64, src as u64, size_bytes))
        }
        _ => return HIP_ERROR_NOT_SUPPORTED,
    };
    match result {
        Ok(()) => HIP_SUCCESS,
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipDeviceSynchronize() -> c_int {
    match with_client(|c| c.device_synchronize()) {
        Ok(()) => HIP_SUCCESS,
        Err(e) => e,
    }
}

#[no_mangle]
pub extern "C" fn hipGetErrorString(error: c_int) -> *const c_char {
    // 'static C strings; safe to hand back a raw pointer.
    let s: &'static CStr = match error {
        HIP_SUCCESS => c"hipSuccess",
        HIP_ERROR_INVALID_VALUE => c"hipErrorInvalidValue",
        HIP_ERROR_OUT_OF_MEMORY => c"hipErrorOutOfMemory",
        HIP_ERROR_NOT_INITIALIZED => c"hipErrorNotInitialized",
        HIP_ERROR_NO_DEVICE => c"hipErrorNoDevice",
        HIP_ERROR_NOT_SUPPORTED => c"hipErrorNotSupported (smolvm-hip-shim: not yet implemented)",
        _ => c"hipErrorUnknown",
    };
    s.as_ptr()
}
