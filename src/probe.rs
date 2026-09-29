//! Asks a receiver for its name with the AirDrop `POST /Discover` request, as `sharingd` does.
//!
//! Receivers answer over TLS on their Bonjour port. They present self-signed certificates, so the
//! server certificate is not verified; nothing sensitive is sent.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use serde_json::Value;
use socket2::{Domain, Socket, Type};

/// Lets a socket use peer-to-peer interfaces such as `awdl0` (private `SO_RECV_ANYIF`).
/// Without it, connections to AWDL addresses time out.
const SO_RECV_ANYIF: i32 = 0x1104;
const SOL_SOCKET: i32 = 0xffff;

unsafe extern "C" {
    fn setsockopt(fd: i32, level: i32, name: i32, value: *const u8, len: u32) -> i32;
    fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
}

/// A receiver's `/Discover` answer.
#[derive(Debug, Clone)]
pub struct Receiver {
    /// The whole reply as JSON. Data values that hold JSON (like
    /// `ReceiverMediaCapabilities`) are parsed; other data becomes `"<N bytes>"`.
    pub reply: Value,
    /// The address that answered.
    pub via: String,
    pub response_ms: u64,
}

fn plist_to_json(v: &plist::Value) -> Value {
    use plist::Value as P;
    match v {
        P::String(s) => Value::from(s.as_str()),
        P::Boolean(b) => Value::from(*b),
        P::Integer(i) => i
            .as_signed()
            .map(Value::from)
            .or_else(|| i.as_unsigned().map(Value::from))
            .unwrap_or(Value::Null),
        P::Real(r) => Value::from(*r),
        P::Date(d) => Value::from(d.to_xml_format()),
        P::Array(a) => a.iter().map(plist_to_json).collect(),
        P::Dictionary(d) => d
            .iter()
            .map(|(k, v)| (k.clone(), plist_to_json(v)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        P::Data(bytes) => serde_json::from_slice(bytes)
            .unwrap_or_else(|_| Value::from(format!("<{} bytes>", bytes.len()))),
        _ => Value::Null,
    }
}

/// Parses `fe80::1%awdl0`, `fd00::1` or `192.168.0.2`.
fn socket_addr(address: &str, port: u16) -> Option<SocketAddr> {
    if let Some((ip, scope)) = address.split_once('%') {
        let ip = ip.parse().ok()?;
        let scope = std::ffi::CString::new(scope).ok()?;
        let index = unsafe { if_nametoindex(scope.as_ptr()) };
        return (index != 0).then(|| SocketAddrV6::new(ip, port, 0, index).into());
    }
    Some(SocketAddr::new(address.parse::<IpAddr>().ok()?, port))
}

fn connect(addr: &SocketAddr, timeout: Duration) -> std::io::Result<std::net::TcpStream> {
    let socket = Socket::new(Domain::for_address(*addr), Type::STREAM, None)?;
    let on: i32 = 1;
    let rc = unsafe {
        setsockopt(
            socket.as_raw_fd(),
            SOL_SOCKET,
            SO_RECV_ANYIF,
            (&raw const on).cast(),
            4,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    socket.connect_timeout(&(*addr).into(), timeout)?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    Ok(socket.into())
}

/// Reads one HTTP/1.1 response and returns its status and body. Handles `Content-Length` and
/// chunked bodies, because receivers keep the connection alive after answering.
fn read_response(r: &mut impl BufRead) -> Result<(u16, Vec<u8>), String> {
    let mut line = String::new();
    let io = |e: std::io::Error| e.to_string();
    if r.read_line(&mut line).map_err(io)? == 0 {
        return Err("closed the connection without replying".into());
    }
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line {line:?}"))?;
    let (mut length, mut chunked) = (None, false);
    loop {
        line.clear();
        r.read_line(&mut line).map_err(io)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            let v = v.trim();
            if k.eq_ignore_ascii_case("content-length") {
                length = v.parse::<usize>().ok();
            } else if k.eq_ignore_ascii_case("transfer-encoding") {
                chunked = v.eq_ignore_ascii_case("chunked");
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            line.clear();
            r.read_line(&mut line).map_err(io)?;
            let size = line.trim().split(';').next().unwrap_or_default();
            let size = usize::from_str_radix(size, 16).map_err(|_| "bad chunk size")?;
            if size == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            r.read_exact(&mut body[start..]).map_err(io)?;
            r.read_line(&mut line).map_err(io)?;
        }
    } else if let Some(n) = length {
        body.resize(n, 0);
        r.read_exact(&mut body).map_err(io)?;
    }
    Ok((status, body))
}

fn discover_at(addr: &SocketAddr, host: &str, timeout: Duration) -> Result<Value, String> {
    let tcp = connect(addr, timeout).map_err(|e| format!("connect {addr}: {e}"))?;
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .map_err(|e| e.to_string())?;
    let mut stream = tls
        .connect(host, tcp)
        .map_err(|e| format!("TLS with {addr}: {e}"))?;

    let mut body = Vec::new();
    plist::to_writer_binary(&mut body, &plist::Dictionary::new()).map_err(|e| e.to_string())?;
    let head = format!(
        "POST /Discover HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/octet-stream\r\n\
         Connection: keep-alive\r\nAccept: */*\r\nUser-Agent: AirDrop/1.0\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(&body))
        .map_err(|e| e.to_string())?;

    let (status, body) = read_response(&mut BufReader::new(stream))?;
    if status != 200 {
        return Err(format!("/Discover returned HTTP {status}"));
    }
    let reply: plist::Value = plist::from_bytes(&body).map_err(|e| e.to_string())?;
    Ok(plist_to_json(&reply))
}

/// Tries each address in order until one answers.
pub fn discover(
    addresses: &[String],
    port: u16,
    host: &str,
    timeout: Duration,
) -> Result<Receiver, String> {
    let mut last = "no usable address".to_owned();
    for a in addresses {
        let Some(addr) = socket_addr(a, port) else {
            continue;
        };
        let start = Instant::now();
        match discover_at(&addr, host, timeout) {
            Ok(reply) => {
                return Ok(Receiver {
                    reply,
                    via: a.clone(),
                    response_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        let (status, body) = read_response(&mut &raw[..]).unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"abcde"[..]));
    }

    #[test]
    fn content_length_response() {
        let raw = b"HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\nhi";
        let (status, body) = read_response(&mut &raw[..]).unwrap();
        assert_eq!((status, body.as_slice()), (404, &b"hi"[..]));
    }

    #[test]
    fn addresses() {
        assert!(socket_addr("fe80::1%lo0", 1).is_some());
        assert!(socket_addr("fe80::1%nope0", 1).is_none());
        assert_eq!(
            socket_addr("192.168.0.2", 8770),
            Some("192.168.0.2:8770".parse().unwrap())
        );
    }
}
