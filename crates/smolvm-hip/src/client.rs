//! Guest-side HIP-RPC client: marshal `hip*` calls over a byte stream.
//!
//! Transport-agnostic — it takes any [`Read`]/[`Write`], so the guest binary
//! supplies an `AF_VSOCK` stream while tests supply an in-memory pipe. Each
//! method does one request→response round-trip and surfaces a non-zero
//! `hipError_t` as [`HipRpcError::Hip`]. Mirrors `smolvm_cuda::client`,
//! trimmed to this milestone's op set (see `proto` module docs).

use crate::proto::{decode_response, encode_request, read_msg, write_msg, Request};
use std::io::{self, Read, Write};

#[derive(Debug)]
pub enum HipRpcError {
    Io(io::Error),
    /// Non-zero `hipError_t` returned by the host runtime.
    Hip(i32),
    Protocol(&'static str),
}

impl std::fmt::Display for HipRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HipRpcError::Io(e) => write!(f, "hip-rpc io: {e}"),
            HipRpcError::Hip(c) => write!(f, "HIP error {c}"),
            HipRpcError::Protocol(m) => write!(f, "hip-rpc protocol: {m}"),
        }
    }
}
impl std::error::Error for HipRpcError {}
impl From<io::Error> for HipRpcError {
    fn from(e: io::Error) -> Self {
        HipRpcError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, HipRpcError>;

/// A connection to the host HIP daemon over any byte stream.
pub struct Client<S: Read + Write> {
    stream: S,
}

impl<S: Read + Write> Client<S> {
    pub fn new(stream: S) -> Self {
        Client { stream }
    }

    fn call(&mut self, req: Request) -> Result<Vec<u8>> {
        let payload = encode_request(&req);
        write_msg(&mut self.stream, &payload)?;
        let resp_payload = read_msg(&mut self.stream)?;
        let resp = decode_response(&resp_payload)
            .map_err(|_| HipRpcError::Protocol("malformed response"))?;
        if resp.status != 0 {
            return Err(HipRpcError::Hip(resp.status));
        }
        Ok(resp.data)
    }

    pub fn init(&mut self) -> Result<()> {
        self.call(Request::Init).map(|_| ())
    }

    pub fn device_get_count(&mut self) -> Result<i32> {
        let data = self.call(Request::DeviceGetCount)?;
        Ok(i32::from_le_bytes(
            data.get(..4)
                .and_then(|b| b.try_into().ok())
                .ok_or(HipRpcError::Protocol("short DeviceGetCount reply"))?,
        ))
    }

    pub fn device_get_name(&mut self, device: i32) -> Result<String> {
        let data = self.call(Request::DeviceGetName { device })?;
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    pub fn device_total_mem(&mut self, device: i32) -> Result<u64> {
        let data = self.call(Request::DeviceTotalMem { device })?;
        Ok(u64::from_le_bytes(
            data.get(..8)
                .and_then(|b| b.try_into().ok())
                .ok_or(HipRpcError::Protocol("short DeviceTotalMem reply"))?,
        ))
    }

    pub fn mem_alloc(&mut self, size: u64) -> Result<u64> {
        let data = self.call(Request::MemAlloc { size })?;
        Ok(u64::from_le_bytes(
            data.get(..8)
                .and_then(|b| b.try_into().ok())
                .ok_or(HipRpcError::Protocol("short MemAlloc reply"))?,
        ))
    }

    pub fn mem_free(&mut self, dptr: u64) -> Result<()> {
        self.call(Request::MemFree { dptr }).map(|_| ())
    }

    pub fn memcpy_h2d(&mut self, dptr: u64, data: &[u8]) -> Result<()> {
        self.call(Request::MemcpyHtoD {
            dptr,
            data: data.to_vec(),
        })
        .map(|_| ())
    }

    pub fn memcpy_d2h(&mut self, dptr: u64, size: u64) -> Result<Vec<u8>> {
        self.call(Request::MemcpyDtoH { dptr, size })
    }

    pub fn memcpy_d2d(&mut self, dst: u64, src: u64, size: u64) -> Result<()> {
        self.call(Request::MemcpyDtoD { dst, src, size })
            .map(|_| ())
    }

    pub fn device_synchronize(&mut self) -> Result<()> {
        self.call(Request::DeviceSynchronize).map(|_| ())
    }
}
