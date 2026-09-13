//! Host-side HIP backend trait + dispatch, mirroring `smolvm_cuda::host`.
//!
//! [`HipBackend`] is the seam between the wire protocol and whatever actually
//! owns the GPU. The `gpu` feature adds [`gpu::RealHipBackend`], which dlopens
//! the real `libamdhip64.so` and forwards to it; without that feature this
//! module still compiles (for host-less test builds), it just has no backend
//! to dispatch to.

use crate::proto::{Request, Response};

#[cfg(feature = "gpu")]
pub mod gpu;

/// What a HIP backend must answer. One instance is expected per host GPU
/// daemon process (see `smolvm-cuda`'s per-VM worker-process isolation model
/// for where this eventually plugs into the daemon — not yet wired up here).
pub trait HipBackend {
    fn init(&mut self) -> Result<(), i32>;
    fn device_get_count(&mut self) -> Result<i32, i32>;
    fn device_get_name(&mut self, device: i32) -> Result<String, i32>;
    fn device_total_mem(&mut self, device: i32) -> Result<u64, i32>;
    fn mem_alloc(&mut self, size: u64) -> Result<u64, i32>;
    fn mem_free(&mut self, dptr: u64) -> Result<(), i32>;
    fn memcpy_h2d(&mut self, dptr: u64, data: &[u8]) -> Result<(), i32>;
    fn memcpy_d2h(&mut self, dptr: u64, size: u64) -> Result<Vec<u8>, i32>;
    fn memcpy_d2d(&mut self, dst: u64, src: u64, size: u64) -> Result<(), i32>;
    fn device_synchronize(&mut self) -> Result<(), i32>;
}

/// Dispatch one decoded [`Request`] against a backend, producing the
/// [`Response`] to encode back to the guest.
pub fn dispatch(backend: &mut dyn HipBackend, req: Request) -> Response {
    fn ok(data: Vec<u8>) -> Response {
        Response { status: 0, data }
    }
    fn err(status: i32) -> Response {
        Response {
            status,
            data: Vec::new(),
        }
    }
    match req {
        Request::Init => match backend.init() {
            Ok(()) => ok(Vec::new()),
            Err(e) => err(e),
        },
        Request::DeviceGetCount => match backend.device_get_count() {
            Ok(n) => ok(n.to_le_bytes().to_vec()),
            Err(e) => err(e),
        },
        Request::DeviceGetName { device } => match backend.device_get_name(device) {
            Ok(name) => ok(name.into_bytes()),
            Err(e) => err(e),
        },
        Request::DeviceTotalMem { device } => match backend.device_total_mem(device) {
            Ok(bytes) => ok(bytes.to_le_bytes().to_vec()),
            Err(e) => err(e),
        },
        Request::MemAlloc { size } => match backend.mem_alloc(size) {
            Ok(dptr) => ok(dptr.to_le_bytes().to_vec()),
            Err(e) => err(e),
        },
        Request::MemFree { dptr } => match backend.mem_free(dptr) {
            Ok(()) => ok(Vec::new()),
            Err(e) => err(e),
        },
        Request::MemcpyHtoD { dptr, data } => match backend.memcpy_h2d(dptr, &data) {
            Ok(()) => ok(Vec::new()),
            Err(e) => err(e),
        },
        Request::MemcpyDtoH { dptr, size } => match backend.memcpy_d2h(dptr, size) {
            Ok(data) => ok(data),
            Err(e) => err(e),
        },
        Request::MemcpyDtoD { dst, src, size } => match backend.memcpy_d2d(dst, src, size) {
            Ok(()) => ok(Vec::new()),
            Err(e) => err(e),
        },
        Request::DeviceSynchronize => match backend.device_synchronize() {
            Ok(()) => ok(Vec::new()),
            Err(e) => err(e),
        },
    }
}
