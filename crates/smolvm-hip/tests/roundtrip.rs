//! End-to-end proof that client -> wire protocol -> host dispatch -> real
//! `libamdhip64.so` actually works, not just compiles. Runs a tiny TCP-based
//! host server in a thread (the vsock transport itself is exercised by
//! `smolvm-hip-shim`, which cannot easily run in a unit test); everything
//! below the transport is the real production path.

#![cfg(feature = "gpu")]

use smolvm_hip::client::Client;
use smolvm_hip::host::gpu::RealHipBackend;
use smolvm_hip::host::{dispatch, HipBackend};
use smolvm_hip::proto::{decode_request, encode_response, read_msg, write_msg};
use std::net::{TcpListener, TcpStream};

fn spawn_host() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut backend: Box<dyn HipBackend> =
            Box::new(RealHipBackend::load().expect("dlopen libamdhip64.so"));
        loop {
            let payload = match read_msg(&mut stream) {
                Ok(p) => p,
                Err(_) => break, // client disconnected
            };
            let req = decode_request(&payload).expect("decode request");
            let resp = dispatch(backend.as_mut(), req);
            write_msg(&mut stream, &encode_response(&resp)).unwrap();
        }
    });
    addr
}

#[test]
fn full_stack_round_trip_on_real_gpu() {
    let addr = spawn_host();
    let stream = TcpStream::connect(addr).unwrap();
    let mut client = Client::new(stream);

    client.init().expect("hipInit over RPC");

    let count = client
        .device_get_count()
        .expect("hipGetDeviceCount over RPC");
    assert!(count >= 1, "expected at least one HIP device, got {count}");

    let name = client
        .device_get_name(0)
        .expect("hipDeviceGetName over RPC");
    assert!(!name.is_empty(), "device name should not be empty");
    eprintln!("device 0: {name}");

    let total_mem = client
        .device_total_mem(0)
        .expect("hipDeviceTotalMem over RPC");
    assert!(total_mem > 0, "expected nonzero total device memory");

    // Real allocate -> upload -> download -> compare, round-tripped through
    // the RPC layer end to end.
    let payload: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let dptr = client
        .mem_alloc(payload.len() as u64)
        .expect("hipMalloc over RPC");
    client
        .memcpy_h2d(dptr, &payload)
        .expect("hipMemcpy H2D over RPC");
    let back = client
        .memcpy_d2h(dptr, payload.len() as u64)
        .expect("hipMemcpy D2H over RPC");
    assert_eq!(payload, back, "round-tripped bytes must match exactly");

    client
        .device_synchronize()
        .expect("hipDeviceSynchronize over RPC");
    client.mem_free(dptr).expect("hipFree over RPC");
}
