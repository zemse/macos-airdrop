//! Finds Apple devices active on AWDL, whether or not they offer AirDrop to us.
//!
//! Pings the IPv6 all-nodes group on `awdl0` and collects who answers. Every device with its
//! peer-to-peer Wi-Fi up replies, including Contacts Only receivers that hide their AirDrop
//! service from us, and devices using AWDL for AirPlay, Sidecar or Universal Control.

use std::mem::MaybeUninit;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

const SO_RECV_ANYIF: i32 = 0x1104;
const SOL_SOCKET: i32 = 0xffff;
const ICMP6_ECHO_REQUEST: u8 = 128;
const ICMP6_ECHO_REPLY: u8 = 129;
const ALL_NODES: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);

unsafe extern "C" {
    fn setsockopt(fd: i32, level: i32, name: i32, value: *const u8, len: u32) -> i32;
    fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
}

/// Addresses (`fe80::…%awdl0`) of every other device answering on AWDL within `wait`
/// (this Mac does not answer its own ping). Pings a few times because AWDL drops packets while it hops channels.
pub fn awdl_neighbours(wait: Duration) -> Result<Vec<String>, String> {
    let index = unsafe { if_nametoindex(c"awdl0".as_ptr()) };
    if index == 0 {
        return Err("no awdl0 interface".into());
    }
    let io = |e: std::io::Error| format!("AWDL presence probe: {e}");
    // An ICMPv6 datagram socket needs no root; the kernel fills in the checksum.
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::ICMPV6)).map_err(io)?;
    let on: i32 = 1;
    unsafe {
        setsockopt(
            socket.as_raw_fd(),
            SOL_SOCKET,
            SO_RECV_ANYIF,
            (&raw const on).cast(),
            4,
        )
    };
    socket.set_multicast_if_v6(index).map_err(io)?;
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(io)?;

    let target = SockAddr::from(SocketAddrV6::new(ALL_NODES, 0, 0, index));
    let id = (std::process::id() & 0xffff) as u16;
    let rounds = 3u16;
    let mut found: Vec<String> = Vec::new();
    let start = Instant::now();
    let mut sent = 0u16;
    while start.elapsed() < wait {
        let due = u32::from(sent) * wait.as_millis() as u32 / u32::from(rounds);
        if sent < rounds && start.elapsed().as_millis() as u32 >= due {
            let [i0, i1] = id.to_be_bytes();
            let [s0, s1] = sent.to_be_bytes();
            let echo = [ICMP6_ECHO_REQUEST, 0, 0, 0, i0, i1, s0, s1];
            let _ = socket.send_to(&echo, &target);
            sent += 1;
        }
        let mut buf = [MaybeUninit::<u8>::uninit(); 256];
        let Ok((n, from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        let reply: Vec<u8> = buf[..n]
            .iter()
            .map(|b| unsafe { b.assume_init() })
            .collect();
        if reply.len() < 6 || reply[0] != ICMP6_ECHO_REPLY || reply[4..6] != id.to_be_bytes() {
            continue;
        }
        if let Some(v6) = from.as_socket_ipv6() {
            let addr = format!("{}%awdl0", v6.ip());
            if !found.contains(&addr) {
                found.push(addr);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    /// Needs real AWDL neighbours: `cargo test -- --ignored --nocapture awdl`.
    #[test]
    #[ignore]
    fn awdl_live() {
        let found = super::awdl_neighbours(std::time::Duration::from_millis(1500)).unwrap();
        println!("AWDL responders: {found:?}");
        assert!(
            !found.is_empty(),
            "expected at least one device nearby on AWDL"
        );
    }
}
