// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! `--public` (plan C-10): the pieces a hosted instance needs. A per-client-IP token bucket on
//! every request (429 past it), a cap on open WebSocket connections (503 past it), a cap on
//! how many events one `/api/events` call returns, and a redaction pass over every JSON that
//! leaves the server so that no node address, URL, credential or operator path can appear in a
//! response — node ids are fine and stay. The RPC client already replaces its node's address by
//! the id in error strings (`rpc::RpcClient::scrub`); this is the belt to that brace.

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

pub struct PublicState {
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
    pub ws_open: AtomicUsize,
}

impl Default for PublicState {
    fn default() -> Self {
        PublicState { buckets: Mutex::new(HashMap::new()), ws_open: AtomicUsize::new(0) }
    }
}

impl PublicState {
    /// Take one token for `ip`; `false` when the bucket is empty.
    pub fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if b.len() > 4096 {
            b.retain(|_, (_, last)| now.duration_since(*last).as_secs_f64() < IDLE_SECS);
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

    /// Count a WebSocket in; `None` when the cap is reached. The guard counts it out on drop.
    pub fn ws_guard(self: &Arc<Self>) -> Option<WsGuard> {
        let prev = self.ws_open.fetch_add(1, Ordering::SeqCst);
        if prev >= MAX_WS {
            self.ws_open.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(WsGuard(self.clone()))
    }
}

pub struct WsGuard(Arc<PublicState>);

impl Drop for WsGuard {
    fn drop(&mut self) {
        self.0.ws_open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The client's address: the peer that connected (a reverse proxy in front should pass the real
/// client in `X-Forwarded-For`, whose first entry is taken when present; the README says so).
pub fn client_ip(req: &Request) -> Option<IpAddr> {
    if let Some(xff) = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(ip) = xff.split(',').next().and_then(|s| s.trim().parse().ok()) {
            return Some(ip);
        }
    }
    req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())
}

/// axum middleware: 429 when the client's bucket is empty. Installed only under `--public`.
pub async fn rate_limit(State(p): State<Arc<PublicState>>, req: Request, next: Next) -> Response {
    let ip = client_ip(&req).unwrap_or(IpAddr::from([0, 0, 0, 0]));
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
    fn ws_cap() {
        let p = Arc::new(PublicState::default());
        let guards: Vec<_> = (0..MAX_WS).map(|_| p.ws_guard().unwrap()).collect();
        assert!(p.ws_guard().is_none());
        drop(guards);
        let one = p.ws_guard();
        assert!(one.is_some());
        assert_eq!(p.ws_open.load(Ordering::SeqCst), 1);
        drop(one);
        assert_eq!(p.ws_open.load(Ordering::SeqCst), 0);
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
