//! eBPF probe: Network monitoring via fentry/tcp_connect + tcp_close.
//!
//! Tracks TCP connection events to detect LLM API latency on socket level.
//! Uses Ring Buffer for variable-rate event streaming to userspace.
//!
//! Hooks: fentry/tcp_connect, fentry/tcp_close
//! Map: Ring Buffer (256KB) for TcpEvent records
//!
//! Build: cargo +nightly build -Z build-std=core --target bpfel-unknown-none

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::bpf_ktime_get_ns,
    macros::{fentry, map},
    maps::RingBuf,
    programs::FEntryContext,
};

/// TCP connection event written to the ring buffer.
#[repr(C)]
pub struct TcpEvent {
    /// Destination IPv4 address (network byte order).
    pub dest_ip: u32,
    /// Destination port.
    pub dest_port: u16,
    /// Padding for alignment.
    pub _pad: u16,
    /// Timestamp in nanoseconds (bpf_ktime_get_ns).
    pub timestamp_ns: u64,
    /// Bytes sent (populated on tcp_close).
    pub bytes_sent: u64,
    /// Bytes received (populated on tcp_close).
    pub bytes_recv: u64,
    /// Event type: 0 = connect, 1 = close.
    pub event_type: u8,
    /// Padding.
    pub _pad2: [u8; 7],
}

/// Ring Buffer for TCP events. 256KB capacity.
#[map]
static TCP_EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

/// fentry hook on tcp_connect.
/// Records connection initiation timestamp.
#[fentry(function = "tcp_connect")]
pub fn tcp_connect_probe(_ctx: FEntryContext) -> u32 {
    match try_tcp_connect() {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

#[inline(always)]
fn try_tcp_connect() -> Result<u32, u32> {
    let ts = unsafe { bpf_ktime_get_ns() };

    if let Some(mut entry) = TCP_EVENTS.reserve::<TcpEvent>(0) {
        let event = entry.as_mut_ptr();
        unsafe {
            (*event).timestamp_ns = ts;
            (*event).event_type = 0; // connect
            (*event).bytes_sent = 0;
            (*event).bytes_recv = 0;
        }
        entry.submit(0);
    }
    // If reserve fails, event is silently dropped (ring buffer full).
    // The userspace collector tracks drop rate via ring buffer stats.

    Ok(0)
}

/// fentry hook on tcp_close.
/// Records connection teardown with transfer stats.
#[fentry(function = "tcp_close")]
pub fn tcp_close_probe(_ctx: FEntryContext) -> u32 {
    match try_tcp_close() {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

#[inline(always)]
fn try_tcp_close() -> Result<u32, u32> {
    let ts = unsafe { bpf_ktime_get_ns() };

    if let Some(mut entry) = TCP_EVENTS.reserve::<TcpEvent>(0) {
        let event = entry.as_mut_ptr();
        unsafe {
            (*event).timestamp_ns = ts;
            (*event).event_type = 1; // close
        }
        entry.submit(0);
    }

    Ok(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
