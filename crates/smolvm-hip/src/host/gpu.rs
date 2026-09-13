//! Real HIP backend via `dlopen` of `libamdhip64.so`, mirroring
//! `smolvm_cuda::host::gpu`'s approach for the real CUDA driver.
//!
//! Verified against ROCm 7.2.4 on `gfx1151` (AMD Strix Halo). Search order
//! matches `SMOLVM_HIP_LIB` first, then the plain soname (dlopen's own
//! ldconfig search), then the default ROCm install path — the last of which
//! stock distro packaging doesn't put on the loader path.

use super::HipBackend;
use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::os::raw::{c_char, c_int, c_void};

const HIP_MEMCPY_HOST_TO_DEVICE: c_int = 1;
const HIP_MEMCPY_DEVICE_TO_HOST: c_int = 2;
const HIP_MEMCPY_DEVICE_TO_DEVICE: c_int = 3;

type HipInitFn = unsafe extern "C" fn(c_int) -> c_int;
type HipGetDeviceCountFn = unsafe extern "C" fn(*mut c_int) -> c_int;
type HipDeviceGetNameFn = unsafe extern "C" fn(*mut c_char, c_int, c_int) -> c_int;
type HipDeviceTotalMemFn = unsafe extern "C" fn(*mut u64, c_int) -> c_int;
type HipMallocFn = unsafe extern "C" fn(*mut *mut c_void, u64) -> c_int;
type HipFreeFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type HipMemcpyFn = unsafe extern "C" fn(*mut c_void, *const c_void, u64, c_int) -> c_int;
type HipDeviceSynchronizeFn = unsafe extern "C" fn() -> c_int;

fn candidate_paths() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("SMOLVM_HIP_LIB") {
        v.push(p);
    }
    v.push("libamdhip64.so".to_string()); // plain soname: dlopen's own ldconfig search
    v.push("/opt/rocm/lib/libamdhip64.so".to_string()); // default ROCm install path
    v
}

/// Real HIP backend, dlopened on first use. Device pointers returned to
/// callers are the real host device addresses (`hipMalloc`'s own return
/// value) — same handle model `smolvm_cuda` uses for `CUdeviceptr`, so a
/// future kernel-launch op can embed them by value.
pub struct RealHipBackend {
    lib: Library,
    // Tracks live allocation sizes so `memcpy_d2h` knows how much to read
    // back without the caller re-stating it via a separate call.
    allocations: HashMap<u64, u64>,
}

impl RealHipBackend {
    pub fn load() -> Result<Self, String> {
        let mut last_err = String::new();
        for path in candidate_paths() {
            match unsafe { Library::new(&path) } {
                Ok(lib) => {
                    return Ok(RealHipBackend {
                        lib,
                        allocations: HashMap::new(),
                    })
                }
                Err(e) => last_err = format!("{path}: {e}"),
            }
        }
        Err(format!("could not dlopen libamdhip64.so: {last_err}"))
    }

    fn sym<T>(&self, name: &[u8]) -> Result<Symbol<'_, T>, i32> {
        unsafe { self.lib.get::<T>(name).map_err(|_| 999) }
    }
}

impl HipBackend for RealHipBackend {
    fn init(&mut self) -> Result<(), i32> {
        let f: Symbol<HipInitFn> = self.sym(b"hipInit\0")?;
        match unsafe { f(0) } {
            0 => Ok(()),
            e => Err(e),
        }
    }

    fn device_get_count(&mut self) -> Result<i32, i32> {
        let f: Symbol<HipGetDeviceCountFn> = self.sym(b"hipGetDeviceCount\0")?;
        let mut count: c_int = 0;
        match unsafe { f(&mut count) } {
            0 => Ok(count),
            e => Err(e),
        }
    }

    fn device_get_name(&mut self, device: i32) -> Result<String, i32> {
        let f: Symbol<HipDeviceGetNameFn> = self.sym(b"hipDeviceGetName\0")?;
        let mut buf = [0i8; 256];
        match unsafe { f(buf.as_mut_ptr(), buf.len() as c_int, device) } {
            0 => {
                let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
                Ok(cstr.to_string_lossy().into_owned())
            }
            e => Err(e),
        }
    }

    fn device_total_mem(&mut self, device: i32) -> Result<u64, i32> {
        let f: Symbol<HipDeviceTotalMemFn> = self.sym(b"hipDeviceTotalMem\0")?;
        let mut bytes: u64 = 0;
        match unsafe { f(&mut bytes, device) } {
            0 => Ok(bytes),
            e => Err(e),
        }
    }

    fn mem_alloc(&mut self, size: u64) -> Result<u64, i32> {
        let f: Symbol<HipMallocFn> = self.sym(b"hipMalloc\0")?;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        match unsafe { f(&mut ptr, size) } {
            0 => {
                let addr = ptr as u64;
                self.allocations.insert(addr, size);
                Ok(addr)
            }
            e => Err(e),
        }
    }

    fn mem_free(&mut self, dptr: u64) -> Result<(), i32> {
        let result = {
            let f: Symbol<HipFreeFn> = self.sym(b"hipFree\0")?;
            unsafe { f(dptr as *mut c_void) }
        };
        self.allocations.remove(&dptr);
        match result {
            0 => Ok(()),
            e => Err(e),
        }
    }

    fn memcpy_h2d(&mut self, dptr: u64, data: &[u8]) -> Result<(), i32> {
        let f: Symbol<HipMemcpyFn> = self.sym(b"hipMemcpy\0")?;
        match unsafe {
            f(
                dptr as *mut c_void,
                data.as_ptr() as *const c_void,
                data.len() as u64,
                HIP_MEMCPY_HOST_TO_DEVICE,
            )
        } {
            0 => Ok(()),
            e => Err(e),
        }
    }

    fn memcpy_d2h(&mut self, dptr: u64, size: u64) -> Result<Vec<u8>, i32> {
        let f: Symbol<HipMemcpyFn> = self.sym(b"hipMemcpy\0")?;
        let mut buf = vec![0u8; size as usize];
        match unsafe {
            f(
                buf.as_mut_ptr() as *mut c_void,
                dptr as *const c_void,
                size,
                HIP_MEMCPY_DEVICE_TO_HOST,
            )
        } {
            0 => Ok(buf),
            e => Err(e),
        }
    }

    fn memcpy_d2d(&mut self, dst: u64, src: u64, size: u64) -> Result<(), i32> {
        let f: Symbol<HipMemcpyFn> = self.sym(b"hipMemcpy\0")?;
        match unsafe {
            f(
                dst as *mut c_void,
                src as *const c_void,
                size,
                HIP_MEMCPY_DEVICE_TO_DEVICE,
            )
        } {
            0 => Ok(()),
            e => Err(e),
        }
    }

    fn device_synchronize(&mut self) -> Result<(), i32> {
        let f: Symbol<HipDeviceSynchronizeFn> = self.sym(b"hipDeviceSynchronize\0")?;
        match unsafe { f() } {
            0 => Ok(()),
            e => Err(e),
        }
    }
}
