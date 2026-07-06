//! Minimal SOCKS5 (RFC 1928) UDP ASSOCIATE client — enough to tunnel the SA-MP UDP game traffic
//! through an authenticated proxy so the bot connects from the proxy's IP instead of the host's.
//!
//! Flow: TCP-connect the proxy, negotiate username/password auth (RFC 1929), send a `UDP ASSOCIATE`
//! request, and keep the TCP control connection open for the association's lifetime. Game datagrams
//! are then sent to the returned relay address, each prefixed with the SOCKS5 UDP request header
//! ([`wrap_udp`]); relayed replies carry the same header, stripped by [`unwrap_udp`]. IPv4 only —
//! SA-MP servers and these proxies are all v4.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

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
    let mut s = socks5_handshake(cfg).await?;

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

/// TCP-connect the proxy and complete the SOCKS5 username/password handshake (RFC 1928 + 1929),
/// returning the authenticated control stream ready for a command (ASSOCIATE / CONNECT).
async fn socks5_handshake(cfg: &ProxyConfig) -> Result<TcpStream> {
    let mut s = TcpStream::connect(cfg.addr).await?;
    // Greeting — offer only username/password auth (method 0x02).
    s.write_all(&[0x05, 0x01, 0x02]).await?;
    let mut method = [0u8; 2];
    s.read_exact(&mut method).await?;
    if method[0] != 0x05 || method[1] != 0x02 {
        return Err(proxy_err("proxy did not accept username/password auth"));
    }
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
    Ok(s)
}

/// SOCKS5 CONNECT (TCP, cmd 0x01) to `target` through the proxy, returning the tunnelled stream.
async fn socks5_connect(cfg: &ProxyConfig, target: SocketAddrV4) -> Result<TcpStream> {
    let mut s = socks5_handshake(cfg).await?;
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&target.ip().octets());
    req.extend_from_slice(&target.port().to_be_bytes());
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(proxy_err("SOCKS5 CONNECT was refused"));
    }
    // Consume the bound address so the stream is positioned at the tunnelled data.
    match head[3] {
        0x01 => {
            let mut a = [0u8; 6];
            s.read_exact(&mut a).await?;
        }
        0x03 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            let mut rest = vec![0u8; l[0] as usize + 2];
            s.read_exact(&mut rest).await?;
        }
        0x04 => {
            let mut a = [0u8; 18];
            s.read_exact(&mut a).await?;
        }
        _ => return Err(proxy_err("SOCKS5 CONNECT returned an unknown address type")),
    }
    Ok(s)
}

/// Whitelist our source IP on the SA-MP server's anti-DDoS filter by making an HTTP `GET /` to the
/// server's port 80 with the launcher's `User-Agent: Arizona PC`. Arizona whitelists the source IP of
/// this request, and the subsequent UDP game handshake from the same IP is then accepted (the UDP
/// query ping alone is not enough for a fresh IP). Through a proxy the request goes via SOCKS5 CONNECT
/// so the PROXY's exit IP is the one whitelisted — this is what lets a proxied bot connect at all.
pub async fn http_whitelist(server_ip: Ipv4Addr, proxy: Option<&ProxyConfig>) -> Result<()> {
    let target = SocketAddrV4::new(server_ip, 80);
    let mut s = match proxy {
        Some(cfg) => socks5_connect(cfg, target).await?,
        None => TcpStream::connect(SocketAddr::V4(target)).await?,
    };
    let req = format!(
        "GET / HTTP/1.1\r\nHost: {server_ip}\r\nUser-Agent: Arizona PC\r\nAccept: */*\r\nAccept-Encoding: deflate, gzip\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await?;
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).await.unwrap_or(0);
    let status = String::from_utf8_lossy(&buf[..n]);
    tracing::info!(status = %status.lines().next().unwrap_or("").trim(), "anti-DDoS whitelist ping (HTTP :80, UA=Arizona PC)");
    Ok(())
}

/// Ask a public STUN server, *through the UDP relay*, what source IP our UDP egresses from — i.e. the
/// exit IP the destination server actually sees for game packets. Best-effort: `None` on any failure.
/// Pairs with [`tcp_exit_ip`] to detect proxies that egress UDP from a different IP than TCP (e.g. SOAX
/// residential relays UDP via separate nodes), which silently breaks a server's source-IP whitelist.
pub async fn stun_exit_ip(socket: &UdpSocket, relay: SocketAddr) -> Option<Ipv4Addr> {
    let stun = tokio::net::lookup_host("stun.l.google.com:19302")
        .await
        .ok()?
        .find(|a| a.is_ipv4())?;
    // 20-byte STUN binding request: type=0x0001 Binding, length=0, magic cookie, 12-byte txn id.
    let mut req = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42];
    let txid: [u8; 12] = std::array::from_fn(|_| rand::random());
    req.extend_from_slice(&txid);
    let wrapped = wrap_udp(stun, &req);
    let mut buf = [0u8; 512];
    for _ in 0..3 {
        socket.send_to(&wrapped, relay).await.ok()?;
        if let Ok(Ok((n, from))) =
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await
        {
            if n > 0 && from.ip() == relay.ip() {
                if let Some(ip) = unwrap_udp(&buf[..n]).and_then(parse_xor_mapped_ipv4) {
                    return Some(ip);
                }
            }
        }
    }
    None
}

/// Parse an IPv4 `XOR-MAPPED-ADDRESS` (attr 0x0020, legacy 0x8020) out of a STUN success response.
fn parse_xor_mapped_ipv4(msg: &[u8]) -> Option<Ipv4Addr> {
    const COOKIE: u32 = 0x2112_A442;
    if msg.len() < 20 {
        return None;
    }
    let mut i = 20; // skip the fixed STUN header
    while i + 4 <= msg.len() {
        let atyp = u16::from_be_bytes([msg[i], msg[i + 1]]);
        let alen = u16::from_be_bytes([msg[i + 2], msg[i + 3]]) as usize;
        let val = msg.get(i + 4..i + 4 + alen)?;
        // family byte at val[1] == 0x01 marks IPv4; x-addr is the last 4 bytes XORed with the cookie.
        if (atyp == 0x0020 || atyp == 0x8020) && val.len() >= 8 && val[1] == 0x01 {
            let xaddr = u32::from_be_bytes([val[4], val[5], val[6], val[7]]) ^ COOKIE;
            return Some(Ipv4Addr::from(xaddr));
        }
        i += 4 + alen + (4 - alen % 4) % 4; // attributes are 4-byte aligned
    }
    None
}

/// Best-effort: the exit IP our *TCP* traffic egresses from, via a SOCKS5 CONNECT to a plaintext
/// IP-echo. `None` on any failure. The whitelist ([`http_whitelist`]) rides this same TCP exit, so
/// comparing it with [`stun_exit_ip`] reveals whether the whitelisted IP is the one the game UDP uses.
pub async fn tcp_exit_ip(cfg: &ProxyConfig) -> Option<Ipv4Addr> {
    let target = tokio::net::lookup_host("api.ipify.org:80")
        .await
        .ok()?
        .find_map(|a| match a {
            SocketAddr::V4(v) => Some(v),
            SocketAddr::V6(_) => None,
        })?;
    let mut s = socks5_connect(cfg, target).await.ok()?;
    s.write_all(
        b"GET / HTTP/1.1\r\nHost: api.ipify.org\r\nUser-Agent: Arizona PC\r\nConnection: close\r\n\r\n",
    )
    .await
    .ok()?;
    let mut resp = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut resp))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&resp);
    text.rsplit("\r\n\r\n").next()?.trim().parse().ok()
}

pub(crate) fn rotate_session(username: &str, ttl_secs: u64) -> String {
    // Split off the existing `-session-…` suffix (if any); everything before it (e.g.
    // `<key>-country-RU`) is preserved. Whether we re-emit a `-ttl-<n>` segment is provider-specific:
    // the `95.141.242.12` pool uses `-session-<id>-ttl-<n>`, whereas SOAX (`proxy.soax.com`) rejects an
    // unknown `-ttl-` param and only accepts a bare `-session-<id>`. Mirror the input: keep the ttl only
    // if the caller's username already carried one.
    let (base, keep_ttl) = match username.find("-session-") {
        Some(i) => (&username[..i], username[i..].contains("-ttl-")),
        None => (username, false),
    };
    const CH: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let sid: String = (0..10)
        .map(|_| CH[(rand::random::<u8>() as usize) % CH.len()] as char)
        .collect();
    if keep_ttl {
        format!("{base}-session-{sid}-ttl-{ttl_secs}")
    } else {
        format!("{base}-session-{sid}")
    }
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

#[cfg(test)]
mod whitelist_live_tests {
    use super::*;

    // Live end-to-end check of the HTTP-:80 anti-DDoS whitelist through a SOCKS5 proxy: whitelist the
    // proxy's exit IP, then UDP-associate and confirm the game server replies to a `SAMP …i` query.
    // Credentials come from env (never committed). Run:
    //   RAKNET_TEST_PROXY=ip:port:user:pass RAKNET_TEST_SERVER=ip:port \
    //     cargo test -p raknet whitelist_then_udp -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn whitelist_then_udp_query_gets_a_reply() {
        let (Ok(pspec), Ok(sspec)) = (
            std::env::var("RAKNET_TEST_PROXY"),
            std::env::var("RAKNET_TEST_SERVER"),
        ) else {
            eprintln!("set RAKNET_TEST_PROXY / RAKNET_TEST_SERVER to run");
            return;
        };
        let p: Vec<&str> = pspec.splitn(4, ':').collect();
        let cfg = ProxyConfig {
            addr: format!("{}:{}", p[0], p[1]).parse().unwrap(),
            username: p[2].to_string(),
            password: p[3].to_string(),
        };
        let server: SocketAddrV4 = sspec.parse().unwrap();

        http_whitelist(*server.ip(), Some(&cfg))
            .await
            .expect("http whitelist failed");
        let assoc = udp_associate(&cfg).await.expect("udp associate failed");
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let mut q = Vec::from(*b"SAMP");
        q.extend_from_slice(&server.ip().octets());
        q.extend_from_slice(&server.port().to_le_bytes());
        q.push(b'i');
        let wrapped = wrap_udp(SocketAddr::V4(server), &q);
        for _ in 0..4 {
            let _ = sock.send_to(&wrapped, assoc.relay).await;
        }
        let mut buf = [0u8; 512];
        let res =
            tokio::time::timeout(std::time::Duration::from_secs(6), sock.recv_from(&mut buf)).await;
        let (n, _) = res
            .expect("NO reply in 6s — whitelist/proxy did not work")
            .expect("recv error");
        assert!(n > 0);
        eprintln!("OK: server replied {n} bytes through the proxy after the HTTP :80 whitelist");
    }

    // Live: rotate the session id (fresh exit each try) until an exit relays the game UDP — mirrors
    // establish_proxied(). RAKNET_TEST_PROXY must be a session-based username.
    #[tokio::test]
    #[ignore]
    async fn rotation_finds_a_working_exit() {
        let (Ok(pspec), Ok(sspec)) = (
            std::env::var("RAKNET_TEST_PROXY"),
            std::env::var("RAKNET_TEST_SERVER"),
        ) else {
            return;
        };
        let p: Vec<&str> = pspec.splitn(4, ':').collect();
        let (addr, base_user, pass) = (
            format!("{}:{}", p[0], p[1]).parse().unwrap(),
            p[2].to_string(),
            p[3].to_string(),
        );
        let server: SocketAddrV4 = sspec.parse().unwrap();
        let mut found = None;
        for attempt in 0..15 {
            let cfg = ProxyConfig {
                addr,
                username: rotate_session(&base_user, 86_400),
                password: pass.clone(),
            };
            let _ = http_whitelist(*server.ip(), Some(&cfg)).await;
            let Ok(assoc) = udp_associate(&cfg).await else {
                continue;
            };
            let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
            let mut q = Vec::from(*b"SAMP");
            q.extend_from_slice(&server.ip().octets());
            q.extend_from_slice(&server.port().to_le_bytes());
            q.push(b'i');
            let wrapped = wrap_udp(SocketAddr::V4(server), &q);
            for _ in 0..4 {
                let _ = sock.send_to(&wrapped, assoc.relay).await;
            }
            let mut buf = [0u8; 512];
            if let Ok(Ok((n, _))) =
                tokio::time::timeout(std::time::Duration::from_secs(4), sock.recv_from(&mut buf))
                    .await
            {
                if n > 0 {
                    found = Some((attempt, n));
                    break;
                }
            }
        }
        let (attempt, n) = found.expect("no working exit in 15 rotations");
        eprintln!(
            "OK: found a UDP-relaying exit on rotation #{attempt} (server replied {n} bytes)"
        );
    }
}
