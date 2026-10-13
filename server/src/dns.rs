//! Name lookups that work on the frame.
//!
//! A statically linked binary resolves names through /etc/resolv.conf, which
//! Android does not have, so the normal lookup fails there. When it does, ask
//! the DNS servers Android itself is using (or well-known public ones).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tracing::debug;

const PUBLIC_SERVERS: [&str; 2] = ["8.8.8.8", "1.1.1.1"];
const TYPE_A: u16 = 1;
const DNS_PORT: u16 = 53;
const RESOLVER_CONFIG: &str = "/etc/resolv.conf";
const ANSWER_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve "host:port" for the HTTP client.
pub fn resolve(netloc: &str) -> io::Result<Vec<SocketAddr>> {
    if system_lookup_works() {
        return netloc.to_socket_addrs().map(Iterator::collect);
    }
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, format!("bad address {netloc}"));
    let (host, port) = netloc.rsplit_once(':').ok_or_else(invalid)?;
    let port: u16 = port.parse().map_err(|_| invalid())?;
    ask(host, port, &servers())
}

/// Whether the ordinary lookup can work here. A statically linked binary reads its DNS
/// servers from this file; Android has none, and without it every lookup would wait many
/// seconds before failing.
fn system_lookup_works() -> bool {
    Path::new(RESOLVER_CONFIG).exists()
}

/// Ask each DNS server in turn for the host's addresses; the first useful answer wins.
fn ask(host: &str, port: u16, servers: &[SocketAddr]) -> io::Result<Vec<SocketAddr>> {
    let mut last_error = io::Error::new(io::ErrorKind::NotFound, format!("no DNS server could resolve {host}"));
    for &server in servers {
        match lookup(host, server) {
            Ok(ips) if !ips.is_empty() => {
                debug!(host, %server, ?ips, "resolved");
                return Ok(ips.into_iter().map(|ip| SocketAddr::new(IpAddr::V4(ip), port)).collect());
            }
            Ok(_) => {}
            Err(e) => {
                debug!(host, %server, error = %e, "DNS server did not answer");
                last_error = e;
            }
        }
    }
    Err(last_error)
}

/// Plugs [`resolve`] into the HTTP client.
pub struct FrameResolver;

impl reqwest::dns::Resolve for FrameResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let netloc = format!("{}:0", name.as_str());
        Box::pin(async move {
            let addrs = tokio::task::spawn_blocking(move || resolve(&netloc)).await??;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The servers a program beside this one should be told to ask, if it too would find none
/// by itself (see `system_lookup_works`); None where the ordinary lookup works.
pub fn servers_to_hand_on() -> Option<Vec<String>> {
    (!system_lookup_works()).then(|| servers().iter().map(|server| server.ip().to_string()).collect())
}

/// The DNS servers Android is using, then public ones as a fallback.
fn servers() -> Vec<SocketAddr> {
    let android = ["net.dns1", "net.dns2"].into_iter().filter_map(|prop| {
        let out = Command::new("getprop").arg(prop).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<Ipv4Addr>().ok()
    });
    let public = PUBLIC_SERVERS.iter().filter_map(|s| s.parse::<Ipv4Addr>().ok());
    android.chain(public).map(|ip| SocketAddr::new(IpAddr::V4(ip), DNS_PORT)).collect()
}

fn lookup(host: &str, server: SocketAddr) -> io::Result<Vec<Ipv4Addr>> {
    // Any 16 bits will do to match the reply to the question.
    let id = u16::try_from(std::process::id() % 65_521).unwrap_or(0) ^ u16::try_from(host.len()).unwrap_or(0);
    let query = build_query(id, host).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad host name"))?;
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_read_timeout(Some(ANSWER_TIMEOUT))?;
    socket.send_to(&query, server)?;
    let mut buffer = [0u8; 1500];
    let (len, _) = socket.recv_from(&mut buffer)?;
    parse_response(id, &buffer[..len]).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad DNS reply"))
}

/// A standard recursive query for the host's IPv4 addresses.
fn build_query(id: u16, host: &str) -> Option<Vec<u8>> {
    let mut packet = Vec::with_capacity(host.len() + 18);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]); // recursion desired, one question
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() {
            return None;
        }
        packet.push(u8::try_from(label.len()).ok()?);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&TYPE_A.to_be_bytes());
    packet.extend_from_slice(&[0, 1]); // class IN
    Some(packet)
}

/// Step over a (possibly compressed) name; returns the offset just past it.
fn skip_name(packet: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *packet.get(at)? as usize;
        if len & 0xc0 == 0xc0 {
            return Some(at + 2); // a pointer ends the name
        }
        at += 1 + len;
        if len == 0 {
            return Some(at);
        }
    }
}

/// The IPv4 addresses in a reply to query `id`; None if the packet is not a valid reply to it.
fn parse_response(id: u16, packet: &[u8]) -> Option<Vec<Ipv4Addr>> {
    let u16_at = |at: usize| Some(u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]));
    if u16_at(0)? != id || packet.get(2)? & 0x80 == 0 {
        return None;
    }
    let (questions, answers) = (u16_at(4)?, u16_at(6)?);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(packet, at)? + 4;
    }
    let mut ips = Vec::new();
    for _ in 0..answers {
        at = skip_name(packet, at)?;
        let (kind, len) = (u16_at(at)?, u16_at(at + 8)? as usize);
        let data = packet.get(at + 10..at + 10 + len)?;
        if kind == TYPE_A && len == 4 {
            ips.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        at += 10 + len;
    }
    Some(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_encodes_the_name_as_labels() {
        let query = build_query(0xbeef, "photos.app.goo.gl").unwrap();
        let expected: &[u8] =
            b"\xbe\xef\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x06photos\x03app\x03goo\x02gl\x00\x00\x01\x00\x01";
        assert_eq!(query, expected);
        assert_eq!(build_query(1, "example.com."), build_query(1, "example.com"));
    }

    #[test]
    fn query_rejects_names_that_cannot_be_encoded() {
        let long = "a".repeat(64);
        for host in ["", "a..b", ".", "caf\u{e9}.example", long.as_str()] {
            assert_eq!(build_query(1, host), None, "{host:?}");
        }
    }

    /// A reply to "a.example" with a CNAME followed by two A records, using name compression.
    fn reply(id: u16) -> Vec<u8> {
        let mut p = build_query(id, "a.example").unwrap();
        p[2] = 0x81; // response, recursion desired
        p[3] = 0x80; // recursion available
        p[7] = 3; // three answers
        p.extend_from_slice(b"\xc0\x0c\x00\x05\x00\x01\x00\x00\x00\x3c\x00\x04\x01b\xc0\x0e"); // CNAME b.example
        p.extend_from_slice(b"\xc0\x27\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04\x8e\xfa\x01\x02"); // A 142.250.1.2
        p.extend_from_slice(b"\x01b\x07example\x00\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04\x0a\x00\x00\x07"); // A 10.0.0.7, uncompressed name
        p
    }

    #[test]
    fn response_yields_the_a_records_and_skips_cnames() {
        assert_eq!(parse_response(7, &reply(7)), Some(vec![Ipv4Addr::new(142, 250, 1, 2), Ipv4Addr::new(10, 0, 0, 7)]));
    }

    #[test]
    fn response_with_no_answers_is_empty_not_an_error() {
        let mut p = build_query(7, "nothing.example").unwrap();
        p[2] = 0x81;
        assert_eq!(parse_response(7, &p), Some(vec![]));
    }

    #[test]
    fn replies_to_other_queries_and_damaged_packets_are_rejected() {
        let good = reply(7);
        assert_eq!(parse_response(8, &good), None, "wrong id");
        assert_eq!(parse_response(7, &build_query(7, "a.example").unwrap()), None, "a query, not a reply");
        assert_eq!(parse_response(7, &[]), None);
        for cut in [5, 12, 20, good.len() - 1, good.len() - 5] {
            assert_eq!(parse_response(7, &good[..cut]), None, "truncated at {cut}");
        }
    }

    /// A DNS server on a local port that answers every question with `reply(question)`.
    fn fake_server(reply: impl Fn(&[u8]) -> Vec<u8> + Send + 'static) -> SocketAddr {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let addr = socket.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 1500];
            while let Ok((len, from)) = socket.recv_from(&mut buffer) {
                let _ = socket.send_to(&reply(&buffer[..len]), from);
            }
        });
        addr
    }

    /// Turn a question into an answer carrying these addresses.
    fn answering(ips: &'static [[u8; 4]]) -> impl Fn(&[u8]) -> Vec<u8> {
        move |question| {
            let mut p = question.to_vec();
            p[2] = 0x81;
            p[3] = 0x80;
            p[7] = u8::try_from(ips.len()).unwrap();
            for ip in ips {
                p.extend_from_slice(b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04");
                p.extend_from_slice(ip);
            }
            p
        }
    }

    #[test]
    fn a_dns_server_is_asked_and_its_addresses_get_the_port() {
        let server = fake_server(answering(&[[142, 250, 1, 2], [142, 250, 1, 3]]));
        let found = ask("photos.example", 443, &[server]).unwrap();
        assert_eq!(found, ["142.250.1.2:443".parse().unwrap(), "142.250.1.3:443".parse().unwrap()]);
        assert_eq!(
            lookup("photos.example", server).unwrap(),
            [Ipv4Addr::new(142, 250, 1, 2), Ipv4Addr::new(142, 250, 1, 3)]
        );
    }

    #[test]
    fn the_next_dns_server_is_tried_when_one_is_useless() {
        let garbage = fake_server(|_| b"not dns".to_vec());
        let knows_nothing = fake_server(answering(&[]));
        let good = fake_server(answering(&[[10, 0, 0, 7]]));
        assert_eq!(ask("a.example", 80, &[garbage, knows_nothing, good]).unwrap(), ["10.0.0.7:80".parse().unwrap()]);
    }

    #[test]
    fn failing_to_resolve_reports_why() {
        let garbage = fake_server(|_| b"not dns".to_vec());
        assert_eq!(ask("a.example", 80, &[garbage]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let knows_nothing = fake_server(answering(&[]));
        assert_eq!(ask("a.example", 80, &[knows_nothing]).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(ask("a.example", 80, &[]).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(lookup("bad..name", garbage).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn the_dns_servers_to_ask_always_include_the_public_fallbacks() {
        let servers = servers();
        assert!(servers.contains(&"8.8.8.8:53".parse().unwrap()) && servers.contains(&"1.1.1.1:53".parse().unwrap()));
    }

    #[tokio::test]
    async fn the_http_client_resolves_through_this_module() {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        let addrs: Vec<SocketAddr> = FrameResolver.resolve(name).await.unwrap().collect();
        assert!(addrs.iter().any(|addr| addr.ip().is_loopback()));
    }

    #[test]
    fn addresses_and_local_names_resolve_without_asking_dns() {
        assert_eq!(resolve("127.0.0.1:8080").unwrap(), vec!["127.0.0.1:8080".parse().unwrap()]);
        assert!(resolve("no-port").is_err());
        // A failed lookup must not spoil the ones after it.
        assert!(resolve("localhost:80").unwrap().iter().all(|addr| addr.ip().is_loopback() && addr.port() == 80));
    }
}
