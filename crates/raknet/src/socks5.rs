//! Minimal SOCKS5 (RFC 1928) UDP ASSOCIATE client — enough to tunnel the SA-MP UDP game traffic
//! through an authenticated proxy so the bot connects from the proxy's IP instead of the host's.
//!
//! Flow: TCP-connect the proxy, negotiate username/password auth (RFC 1929), send a `UDP ASSOCIATE`
//! request, and keep the TCP control connection open for the association's lifetime. Game datagrams
//! are then sent to the returned relay address, each prefixed with the SOCKS5 UDP request header
//! ([`wrap_udp`]); relayed replies carry the same header, stripped by [`unwrap_udp`]. IPv4 only —
//! SA-MP servers and these proxies are all v4.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{RaknetError, Result};

/// SOCKS5 proxy connection details.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub addr: SocketAddr,
    pub username: String,
    pub password: String,
}

/// An established UDP association. Hold `control` open for the association's lifetime (the proxy
/// tears the association down when the TCP control connection closes).
pub struct UdpAssociation {
    /// Where to send wrapped datagrams (the proxy's UDP relay).
    pub relay: SocketAddr,
    /// The TCP control connection — kept alive by holding it; never read/written after setup.
    pub control: TcpStream,
}

fn proxy_err(msg: &str) -> RaknetError {
    RaknetError::Proxy(msg.to_string())
}

/// Perform the SOCKS5 username/password handshake and a `UDP ASSOCIATE`, returning the relay address
/// and the control connection to keep open.
pub async fn udp_associate(cfg: &ProxyConfig) -> Result<UdpAssociation> {
    let mut s = TcpStream::connect(cfg.addr).await?;

    // Greeting — offer only username/password auth (method 0x02).
    s.write_all(&[0x05, 0x01, 0x02]).await?;
    let mut method = [0u8; 2];
    s.read_exact(&mut method).await?;
    if method[0] != 0x05 || method[1] != 0x02 {
        return Err(proxy_err("proxy did not accept username/password auth"));
    }

    // RFC 1929 username/password sub-negotiation.
    if cfg.username.len() > 255 || cfg.password.len() > 255 {
        return Err(proxy_err("proxy credentials too long"));
    }
    let mut auth = Vec::with_capacity(3 + cfg.username.len() + cfg.password.len());
    auth.push(0x01);
    auth.push(cfg.username.len() as u8);
    auth.extend_from_slice(cfg.username.as_bytes());
    auth.push(cfg.password.len() as u8);
    auth.extend_from_slice(cfg.password.as_bytes());
    s.write_all(&auth).await?;
    let mut auth_reply = [0u8; 2];
    s.read_exact(&mut auth_reply).await?;
    if auth_reply[1] != 0x00 {
        return Err(proxy_err("proxy rejected credentials"));
    }

    // UDP ASSOCIATE. We don't know our post-NAT source yet, so request with 0.0.0.0:0 and let the
    // proxy accept datagrams from wherever our client socket ends up.
    s.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(proxy_err("UDP ASSOCIATE was refused"));
    }
    let relay = match head[3] {
        0x01 => {
            let mut a = [0u8; 6];
            s.read_exact(&mut a).await?;
            let ip = Ipv4Addr::new(a[0], a[1], a[2], a[3]);
            let port = u16::from_be_bytes([a[4], a[5]]);
            SocketAddr::V4(SocketAddrV4::new(ip, port))
        }
        0x03 => {
            let len = {
                let mut l = [0u8; 1];
                s.read_exact(&mut l).await?;
                l[0] as usize
            };
            let mut rest = vec![0u8; len + 2];
            s.read_exact(&mut rest).await?;
            return Err(proxy_err(
                "proxy returned a domain relay address (unsupported)",
            ));
        }
        0x04 => {
            return Err(proxy_err(
                "proxy returned an IPv6 relay address (unsupported)",
            ))
        }
        _ => return Err(proxy_err("proxy returned an unknown relay address type")),
    };
    // A 0.0.0.0 bind address means "reuse the proxy's IP with this port".
    let relay = match relay {
        SocketAddr::V4(v4) if v4.ip().is_unspecified() => SocketAddr::new(cfg.addr.ip(), v4.port()),
        other => other,
    };

    Ok(UdpAssociation { relay, control: s })
}

/// Prepend the SOCKS5 UDP request header (RFC 1928 §7) addressing `dst` (IPv4). The proxy strips it
/// and forwards `payload` to `dst`.
pub fn wrap_udp(dst: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + payload.len());
    out.extend_from_slice(&[0x00, 0x00, 0x00]); // RSV(2) + FRAG(1)=0 (no fragmentation)
    match dst {
        SocketAddr::V4(v4) => {
            out.push(0x01);
            out.extend_from_slice(&v4.ip().octets());
            out.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            out.push(0x04);
            out.extend_from_slice(&v6.ip().octets());
            out.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// Strip the SOCKS5 UDP header from a relayed datagram, returning the inner payload. `None` if the
/// header is malformed or the datagram is a fragment (FRAG != 0, which we never send and don't
/// reassemble).
pub fn unwrap_udp(datagram: &[u8]) -> Option<&[u8]> {
    // RSV(2) FRAG(1) ATYP(1) DST.ADDR DST.PORT(2) DATA
    if datagram.len() < 4 || datagram[2] != 0x00 {
        return None;
    }
    let header = match datagram[3] {
        0x01 => 4 + 4 + 2,
        0x04 => 4 + 16 + 2,
        0x03 => 4 + 1 + (*datagram.get(4)? as usize) + 2,
        _ => return None,
    };
    datagram.get(header..)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_then_unwrap_round_trips_ipv4() {
        let dst: SocketAddr = "80.66.82.87:7777".parse().unwrap();
        let payload = b"\xffhello raknet";
        let wrapped = wrap_udp(dst, payload);
        // 3 (rsv+frag) + 1 (atyp) + 4 (ip) + 2 (port) = 10-byte header.
        assert_eq!(&wrapped[..10], &[0, 0, 0, 1, 80, 66, 82, 87, 0x1e, 0x61]);
        assert_eq!(unwrap_udp(&wrapped), Some(&payload[..]));
    }

    #[test]
    fn unwrap_rejects_fragments_and_short() {
        assert_eq!(unwrap_udp(&[0, 0, 1, 1, 0, 0, 0, 0, 0, 0]), None); // FRAG != 0
        assert_eq!(unwrap_udp(&[0, 0]), None);
    }
}
