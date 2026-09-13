//! HIP remoting protocol + client/host backend for smolvm guests.
//!
//! Milestone 1 (this crate, as scaffolded): device query, flat memory
//! management, synchronize — forwarded over vsock to a real `libamdhip64.so`
//! on the host, verified against ROCm 7.2.4 on `gfx1151` (AMD Strix Halo).
//!
//! Explicitly out of scope for this milestone (tracked as the next one):
//! - Module loading / kernel launch (`hipModuleLoadData`, `hipModuleLaunchKernel`)
//!   — the hardest part of `smolvm-cuda`'s equivalent, needing code-object
//!   loading and launch-parameter marshaling.
//! - hipBLAS / MIOpen / RCCL coverage (CUDA's cuBLAS/cuDNN/NCCL analogues).
//! - The host daemon binary and its per-VM worker-process isolation
//!   (`smolvm-cuda`'s `src/cuda_daemon.rs` equivalent) — not wired into the
//!   main `smolvm` CLI yet. This crate is a library only; nothing spawns it.
//! - Zero-copy guest-RAM advertisement (`smolvm-cuda`'s `procmem`).
//!
//! See `smolvm-cuda` for the mature reference implementation this mirrors.

pub mod client;
pub mod proto;

#[cfg(feature = "host")]
pub mod host;
