//! Wire protocol for HIP remoting over a byte stream (vsock), mirroring
//! `smolvm-cuda`'s `proto.rs` framing.
//!
//! Framing: every message is a `u32` little-endian length followed by the
//! payload. A request payload is a `u8` opcode then its args; a response
//! payload is an `i32` status (`hipError_t`, LE) then return data (present
//! only when `status == 0`). Transport-agnostic: it operates on any
//! [`Read`]/[`Write`], so host (AF_UNIX) and guest (AF_VSOCK) share one
//! definition.
//!
//! Scope of this first milestone: device query, flat memory management
//! (`hipMalloc`/`hipFree`/`hipMemcpy`), and synchronize. Module loading and
//! kernel launch (`hipModuleLoadData`/`hipModuleLaunchKernel`) are the next
//! milestone — see the crate-level docs for why they're scoped out here.
//!
//! Handle model: device pointers (`hipDeviceptr_t`) are passed by their real
//! value, matching CUDA's `CUdeviceptr` treatment, because a future kernel
//! launch's parameters must embed the device address by value.

use std::io::{self, Read, Write};

/// Maximum accepted message payload (256 MiB) — bounds a hostile/length field.
pub const MAX_MSG: usize = 256 * 1024 * 1024;

/// Request opcodes. Stable wire values — append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    Init = 0x01,
    DeviceGetCount = 0x02,
    DeviceGetName = 0x03,
    DeviceTotalMem = 0x04,
    MemAlloc = 0x10,
    MemFree = 0x11,
    MemcpyHtoD = 0x12,
    MemcpyDtoH = 0x13,
    MemcpyDtoD = 0x14,
    DeviceSynchronize = 0x20,
}

impl Op {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x01 => Op::Init,
            0x02 => Op::DeviceGetCount,
            0x03 => Op::DeviceGetName,
            0x04 => Op::DeviceTotalMem,
            0x10 => Op::MemAlloc,
            0x11 => Op::MemFree,
            0x12 => Op::MemcpyHtoD,
            0x13 => Op::MemcpyDtoH,
            0x14 => Op::MemcpyDtoD,
            0x20 => Op::DeviceSynchronize,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Request {
    Init,
    DeviceGetCount,
    DeviceGetName { device: i32 },
    DeviceTotalMem { device: i32 },
    MemAlloc { size: u64 },
    MemFree { dptr: u64 },
    MemcpyHtoD { dptr: u64, data: Vec<u8> },
    MemcpyDtoH { dptr: u64, size: u64 },
    MemcpyDtoD { dst: u64, src: u64, size: u64 },
    DeviceSynchronize,
}

impl Request {
    pub fn op(&self) -> Op {
        match self {
            Request::Init => Op::Init,
            Request::DeviceGetCount => Op::DeviceGetCount,
            Request::DeviceGetName { .. } => Op::DeviceGetName,
            Request::DeviceTotalMem { .. } => Op::DeviceTotalMem,
            Request::MemAlloc { .. } => Op::MemAlloc,
            Request::MemFree { .. } => Op::MemFree,
            Request::MemcpyHtoD { .. } => Op::MemcpyHtoD,
            Request::MemcpyDtoH { .. } => Op::MemcpyDtoH,
            Request::MemcpyDtoD { .. } => Op::MemcpyDtoD,
            Request::DeviceSynchronize => Op::DeviceSynchronize,
        }
    }
}

/// A response's return data, shaped per-op. Empty when the op has no payload
/// beyond the status code.
#[derive(Debug, Clone, Default)]
pub struct Response {
    pub status: i32,
    pub data: Vec<u8>,
}

fn write_u32(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn write_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn write_i32(w: &mut impl Write, v: i32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn read_i32(r: &mut impl Read) -> io::Result<i32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_le_bytes(b))
}
fn read_i32_field(r: &mut impl Read) -> io::Result<i32> {
    read_i32(r)
}

/// Read one length-prefixed frame's payload.
pub fn read_msg(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let len = read_u32(r)? as usize;
    if len > MAX_MSG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write one length-prefixed frame.
pub fn write_msg(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    write_u32(w, payload.len() as u32)?;
    w.write_all(payload)?;
    w.flush()
}

pub fn encode_request(req: &Request) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(req.op() as u8);
    match req {
        Request::Init | Request::DeviceGetCount | Request::DeviceSynchronize => {}
        Request::DeviceGetName { device } | Request::DeviceTotalMem { device } => {
            let _ = write_i32(&mut buf, *device);
        }
        Request::MemAlloc { size } => {
            let _ = write_u64(&mut buf, *size);
        }
        Request::MemFree { dptr } => {
            let _ = write_u64(&mut buf, *dptr);
        }
        Request::MemcpyHtoD { dptr, data } => {
            let _ = write_u64(&mut buf, *dptr);
            let _ = write_u64(&mut buf, data.len() as u64);
            buf.extend_from_slice(data);
        }
        Request::MemcpyDtoH { dptr, size } => {
            let _ = write_u64(&mut buf, *dptr);
            let _ = write_u64(&mut buf, *size);
        }
        Request::MemcpyDtoD { dst, src, size } => {
            let _ = write_u64(&mut buf, *dst);
            let _ = write_u64(&mut buf, *src);
            let _ = write_u64(&mut buf, *size);
        }
    }
    buf
}

pub fn decode_request(payload: &[u8]) -> io::Result<Request> {
    let mut cur = io::Cursor::new(payload);
    let mut opb = [0u8; 1];
    cur.read_exact(&mut opb)?;
    let op = Op::from_u8(opb[0])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown opcode"))?;
    Ok(match op {
        Op::Init => Request::Init,
        Op::DeviceGetCount => Request::DeviceGetCount,
        Op::DeviceGetName => Request::DeviceGetName {
            device: read_i32_field(&mut cur)?,
        },
        Op::DeviceTotalMem => Request::DeviceTotalMem {
            device: read_i32_field(&mut cur)?,
        },
        Op::MemAlloc => Request::MemAlloc {
            size: read_u64(&mut cur)?,
        },
        Op::MemFree => Request::MemFree {
            dptr: read_u64(&mut cur)?,
        },
        Op::MemcpyHtoD => {
            let dptr = read_u64(&mut cur)?;
            let len = read_u64(&mut cur)? as usize;
            let mut data = vec![0u8; len];
            cur.read_exact(&mut data)?;
            Request::MemcpyHtoD { dptr, data }
        }
        Op::MemcpyDtoH => Request::MemcpyDtoH {
            dptr: read_u64(&mut cur)?,
            size: read_u64(&mut cur)?,
        },
        Op::MemcpyDtoD => Request::MemcpyDtoD {
            dst: read_u64(&mut cur)?,
            src: read_u64(&mut cur)?,
            size: read_u64(&mut cur)?,
        },
        Op::DeviceSynchronize => Request::DeviceSynchronize,
    })
}

pub fn encode_response(resp: &Response) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + resp.data.len());
    let _ = write_i32(&mut buf, resp.status);
    buf.extend_from_slice(&resp.data);
    buf
}

pub fn decode_response(payload: &[u8]) -> io::Result<Response> {
    let mut cur = io::Cursor::new(payload);
    let status = read_i32(&mut cur)?;
    let mut data = Vec::new();
    cur.read_to_end(&mut data)?;
    Ok(Response { status, data })
}
