// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! `--public` (plan C-10): the pieces a hosted instance needs. A per-client-IP token bucket on
//! every request (429 past it) and a redaction pass over every JSON that leaves the server so
//! that no node address, URL, credential or operator path can appear in a response — node ids
//! are fine and stay. The RPC client already replaces its node's address by the id in error
//! strings (`rpc::RpcClient::scrub`); this is the belt to that brace.
//!
//! The resource caps — open WebSocket connections (503 past `MAX_WS`), events per
//! `/api/events` call (`EVENTS_CAP`), inbound WebSocket frame size — are unconditional (audit
//! H-12): they cost nothing locally. So is the `Origin` check on `/ws` and the redaction of
//! WebSocket frames (audit H-13).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

/// Requests per second one client IP may sustain (`RATE`) and the burst it may draw (`BURST`).
pub const RATE: f64 = 10.0;
pub const BURST: f64 = 40.0;
/// Open WebSocket connections at once.
pub const MAX_WS: usize = 64;
/// Events one `/api/events?since=` answer carries at most (oldest first, contiguous from `since`;
/// a client that gets exactly this many should ask again from the last `seq`).
pub const EVENTS_CAP: usize = 2000;
/// Buckets idle longer than this are forgotten.
const IDLE_SECS: f64 = 120.0;
/// Buckets kept at most (audit H-11): past this the least recently seen one is evicted on
/// insert, idle or not, so a client rotating addresses cannot grow the map.
pub const MAX_BUCKETS: usize = 4096;
/// Largest inbound WebSocket message accepted (the client sends nothing we read; tungstenite's
/// default is 64 MiB).
pub const WS_MAX_MESSAGE: usize = 64 * 1024;

/// An IPv4 or IPv6 network in CIDR notation (`--trusted-proxies`); a bare address is a /32 or /128.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl std::str::FromStr for Cidr {
    type Err = String;
    fn from_str(s: &str) -> Result<Cidr, String> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.trim().parse().map_err(|_| format!("{}: not an IP address or CIDR", s))?;
        let addr = addr.to_canonical();
        let bits = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.trim().parse::<u8>().ok().filter(|n| *n <= bits).ok_or_else(|| format!("{}: prefix must be 0..={}", s, bits))?,
            None => bits,
        };
        Ok(Cidr { addr, prefix })
    }
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix as u32) };
                (u32::from(n) & mask) == (u32::from(a) & mask)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix as u32) };
                (u128::from(n) & mask) == (u128::from(a) & mask)
            }
            _ => false,
        }
    }
}

pub struct PublicState {
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
    /// Peers whose `X-Forwarded-For` is believed (`--trusted-proxies`). Empty: the header is ignored.
    trusted_proxies: Vec<Cidr>,
}

impl Default for PublicState {
    fn default() -> Self {
        PublicState::new(Vec::new())
    }
}

impl PublicState {
    pub fn new(trusted_proxies: Vec<Cidr>) -> Self {
        PublicState { buckets: Mutex::new(HashMap::new()), trusted_proxies }
    }

    pub fn trusted(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|c| c.contains(ip))
    }

    /// Take one token for `ip`; `false` when the bucket is empty.
    pub fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if !b.contains_key(&ip) && b.len() >= MAX_BUCKETS {
            b.retain(|_, (_, last)| now.duration_since(*last).as_secs_f64() < IDLE_SECS);
            if b.len() >= MAX_BUCKETS {
                // Still full of live clients: the least recently seen one goes (LRU).
                if let Some(oldest) = b.iter().min_by_key(|(_, (_, last))| *last).map(|(k, _)| *k) {
                    b.remove(&oldest);
                }
            }
        }
        let (tokens, last) = b.entry(ip).or_insert((BURST, now));
        *tokens = (*tokens + now.duration_since(*last).as_secs_f64() * RATE).min(BURST);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn buckets(&self) -> usize {
        self.buckets.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The client's address behind `peer`: the peer itself, unless the peer is a trusted proxy
    /// and `xff` (its `X-Forwarded-For`) names hops — then the last hop that is not itself a
    /// trusted proxy, walking from the right (a proxy that appends, like nginx's
    /// `$proxy_add_x_forwarded_for`, leaves the client-supplied entries on the left, where they
    /// are never reached). Audit H-11.
    pub fn client_ip_behind(&self, peer: IpAddr, xff: Option<&str>) -> IpAddr {
        if !self.trusted(peer) {
            return peer;
        }
        let Some(xff) = xff else { return peer };
        for hop in xff.rsplit(',') {
            match hop.trim().parse::<IpAddr>() {
                Ok(ip) if self.trusted(ip) => continue,
                Ok(ip) => return ip.to_canonical(),
                Err(_) => return peer,
            }
        }
        peer
    }
}

/// Open WebSocket connections, counted whether or not `--public` is on (audit H-12).
#[derive(Default)]
pub struct WsCap {
    pub open: AtomicUsize,
}

impl WsCap {
    /// Count a WebSocket in; `None` when the cap is reached. The guard counts it out on drop.
    pub fn guard(self: &Arc<Self>) -> Option<WsGuard> {
        let prev = self.open.fetch_add(1, Ordering::SeqCst);
        if prev >= MAX_WS {
            self.open.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(WsGuard(self.clone()))
    }
}

pub struct WsGuard(Arc<WsCap>);

impl Drop for WsGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The client's address for the rate limit: the TCP peer, or — only when that peer is one of
/// `--trusted-proxies` — the client it forwards for (`PublicState::client_ip_behind`).
pub fn client_ip(p: &PublicState, req: &Request) -> Option<IpAddr> {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())?;
    let xff = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok());
    Some(p.client_ip_behind(peer, xff))
}

/// Is the WebSocket upgrade's `Origin` acceptable (audit H-13): absent (not a browser), or its
/// host equal to the request's `Host`, or listed in `--allow-origin` (`*` allows all). Browsers
/// always send `Origin` on a WebSocket handshake, so a hostile page on another origin is refused.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, allow: &[String]) -> bool {
    let Some(origin) = origin.map(str::trim) else { return true };
    if allow.iter().any(|a| a == "*" || a.eq_ignore_ascii_case(origin)) {
        return true;
    }
    let authority = origin.split_once("://").map(|(_, a)| a).unwrap_or(origin).trim_end_matches('/');
    match host.map(str::trim) {
        Some(h) if !h.is_empty() => authority.eq_ignore_ascii_case(h),
        _ => false,
    }
}

/// axum middleware: 429 when the client's bucket is empty. Installed only under `--public`.
pub async fn rate_limit(State(p): State<Arc<PublicState>>, req: Request, next: Next) -> Response {
    let ip = client_ip(&p, &req).unwrap_or(IpAddr::from([0, 0, 0, 0]));
    if !p.allow(ip) {
        return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")], "rate limited").into_response();
    }
    next.run(req).await
}

/// Does a string look like something an operator would not want published: a URL, a
/// `user@host` or `host:port` of a node, or a filesystem path.
pub fn is_sensitive(s: &str) -> bool {
    if s.contains("://") || s.contains('@') {
        return true;
    }
    if s.starts_with('/') || s.starts_with('~') || s.starts_with("C:\\") {
        return true;
    }
    // host:port — a numeric port after the last colon and a dotted or named host before it.
    if let Some((host, port)) = s.rsplit_once(':') {
        if !host.is_empty() && port.parse::<u16>().is_ok() && (host.contains('.') || host == "localhost" || host.starts_with('[')) {
            return true;
        }
    }
    false
}

/// Replace every sensitive string in `v`, recursively, by `"[redacted]"`. Keys are left alone.
pub fn redact(v: &mut Value) {
    match v {
        Value::String(s) => {
            if is_sensitive(s) {
                *s = "[redacted]".into();
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact),
        Value::Object(o) => o.values_mut().for_each(redact),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn bucket_refills() {
        let p = PublicState::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..BURST as usize {
            assert!(p.allow_at(ip, t0));
        }
        assert!(!p.allow_at(ip, t0));
        assert!(p.allow_at("10.0.0.2".parse().unwrap(), t0), "another client has its own bucket");
        assert!(p.allow_at(ip, t0 + Duration::from_millis(150)), "1.5 tokens back after 150 ms");
        assert!(!p.allow_at(ip, t0 + Duration::from_millis(150)));
    }

    #[test]
    fn buckets_are_bounded() {
        let p = PublicState::default();
        let t0 = Instant::now();
        for i in 0..(MAX_BUCKETS as u32 + 500) {
            assert!(p.allow_at(IpAddr::from(u32::to_be_bytes(0x0a00_0000 + i)), t0 + Duration::from_millis(i as u64)));
        }
        assert_eq!(p.buckets(), MAX_BUCKETS, "LRU eviction past the cap, idle or not");
        // The first (oldest) client was evicted, the latest survives.
        assert!(!p.has(IpAddr::from([10, 0, 0, 0])));
        assert!(p.has(IpAddr::from(u32::to_be_bytes(0x0a00_0000 + MAX_BUCKETS as u32 + 499))));
    }

    impl PublicState {
        fn has(&self, ip: IpAddr) -> bool {
            self.buckets.lock().unwrap().contains_key(&ip)
        }
    }

    #[test]
    fn cidr_parses_and_matches() {
        let c: Cidr = "10.0.0.0/8".parse().unwrap();
        assert!(c.contains("10.255.1.2".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert!(c.contains("::ffff:10.1.1.1".parse().unwrap()), "v4-mapped peers count as v4");
        let one: Cidr = "192.168.1.5".parse().unwrap();
        assert_eq!(one.prefix, 32);
        assert!(one.contains("192.168.1.5".parse().unwrap()) && !one.contains("192.168.1.6".parse().unwrap()));
        let v6: Cidr = "fd00::/8".parse().unwrap();
        assert!(v6.contains("fd12::1".parse().unwrap()) && !v6.contains("fe80::1".parse().unwrap()));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("nope".parse::<Cidr>().is_err());
        assert!("0.0.0.0/0".parse::<Cidr>().unwrap().contains("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn xff_only_from_trusted_proxies_last_untrusted_hop() {
        let peer: IpAddr = "203.0.113.9".parse().unwrap();
        let proxy: IpAddr = "127.0.0.1".parse().unwrap();
        // No trusted proxies: the header is ignored, the peer is the client.
        let none = PublicState::default();
        assert_eq!(none.client_ip_behind(peer, Some("10.1.1.1")), peer);
        // A trusted loopback proxy appending to a client-supplied header: the rightmost untrusted
        // hop (what the proxy saw) wins, never the spoofed first entry.
        let p = PublicState::new(vec!["127.0.0.0/8".parse().unwrap(), "10.0.0.0/8".parse().unwrap()]);
        assert_eq!(p.client_ip_behind(proxy, Some("1.2.3.4, 203.0.113.9")), peer);
        assert_eq!(p.client_ip_behind(proxy, Some("1.2.3.4, 203.0.113.9, 10.0.0.2")), peer, "an inner trusted hop is skipped");
        assert_eq!(p.client_ip_behind(proxy, Some("203.0.113.9")), peer);
        assert_eq!(p.client_ip_behind(proxy, None), proxy);
        assert_eq!(p.client_ip_behind(proxy, Some("garbage")), proxy);
        assert_eq!(p.client_ip_behind(proxy, Some("10.0.0.3")), proxy, "every hop trusted: the peer");
        // An untrusted peer never gets to name a client, even with a plausible header.
        assert_eq!(p.client_ip_behind(peer, Some("1.2.3.4")), peer);
    }

    #[test]
    fn origin_check() {
        let none: &[String] = &[];
        assert!(origin_allowed(None, Some("127.0.0.1:8480"), none), "no Origin: not a browser");
        assert!(origin_allowed(Some("http://127.0.0.1:8480"), Some("127.0.0.1:8480"), none));
        assert!(origin_allowed(Some("https://Viz.Example.org"), Some("viz.example.org"), none));
        assert!(!origin_allowed(Some("http://127.0.0.1:9999"), Some("127.0.0.1:8480"), none), "another local port is another origin");
        assert!(!origin_allowed(Some("http://evil.example"), Some("127.0.0.1:8480"), none));
        assert!(!origin_allowed(Some("http://evil.example"), None, none));
        assert!(!origin_allowed(Some("null"), Some("127.0.0.1:8480"), none));
        let allow = vec!["https://viz.example.org".to_string()];
        assert!(origin_allowed(Some("https://viz.example.org"), Some("127.0.0.1:8480"), &allow));
        assert!(!origin_allowed(Some("http://viz.example.org"), Some("127.0.0.1:8480"), &allow), "scheme is part of the origin");
        assert!(origin_allowed(Some("http://anything"), None, &["*".to_string()]));
    }

    #[test]
    fn ws_cap() {
        let p = Arc::new(WsCap::default());
        let guards: Vec<_> = (0..MAX_WS).map(|_| p.guard().unwrap()).collect();
        assert!(p.guard().is_none());
        drop(guards);
        let one = p.guard();
        assert!(one.is_some());
        assert_eq!(p.open.load(Ordering::SeqCst), 1);
        drop(one);
        assert_eq!(p.open.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn redacts_addresses_not_hashes() {
        let mut v = json!({
            "hash": "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
            "txid": "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
            "url": "http://user:pw@10.0.0.7:8832",
            "err": "rpc transport: error sending request for url (http://localhost:1/)",
            "host": "seed.example.org:8833",
            "v6": "[::1]:8832",
            "file": "/Users/op/yb/session.jsonl",
            "id": "3", "role": "pool", "n": 12, "when": "12:30:00",
            "nested": {"list": ["tcp://127.0.0.1:31000", "ok"]},
        });
        redact(&mut v);
        assert_eq!(v["hash"].as_str().unwrap().len(), 64);
        assert_eq!(v["txid"].as_str().unwrap().len(), 64);
        for k in ["url", "err", "host", "v6", "file"] {
            assert_eq!(v[k], "[redacted]", "{}", k);
        }
        assert_eq!(v["id"], "3");
        assert_eq!(v["role"], "pool");
        assert_eq!(v["when"], "12:30:00", "a clock is not a host:port");
        assert_eq!(v["nested"]["list"], json!(["[redacted]", "ok"]));
    }
}
