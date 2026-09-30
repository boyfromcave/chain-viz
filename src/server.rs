//! axum: `GET /` (the embedded `ui/`), `/api/health`, `/api/snapshot`, `/api/events?since=`,
//! `WS /ws`. Binds loopback by default (`--listen`). Nothing in a response carries a node URL
//! or credential.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use crate::bus::Bus;
use crate::collector::Model;
use crate::public::{self, PublicState};
use crate::replay::ReplayStatus;
use crate::rpc::RpcClient;

static UI: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/ui");

/// Blocks in `/api/snapshot`'s main chain.
pub const SNAPSHOT_BLOCKS: usize = 200;

pub struct AppState {
    pub model: Arc<tokio::sync::RwLock<Model>>,
    pub bus: Arc<Bus>,
    pub clients: Vec<RpcClient>,
    /// Set under `--replay`: `/api/health.replay = {file, pos, total, speed}`.
    pub replay: Option<Arc<ReplayStatus>>,
    /// Set under `--public` (`public.rs`): rate limit, WS cap, events cap, redaction.
    pub public: Option<Arc<PublicState>>,
}

impl AppState {
    /// The last step of every JSON response: under `--public`, redact (`public::redact`).
    fn finish(&self, mut v: Value) -> Json<Value> {
        if self.public.is_some() {
            public::redact(&mut v);
        }
        Json(v)
    }
    /// `/api/health` as a value (also what `--export` writes).
    pub async fn health_json(&self) -> Value {
        let m = self.model.read().await;
        let majority = m.chain.majority();
        let up = m.nodes.values().filter(|n| n.up).count();
        let rpc: BTreeMap<String, BTreeMap<String, u64>> = self.clients.iter().map(|c| (c.id().to_string(), c.counter.snapshot())).collect();
        json!({
            "ok": up > 0,
            "nodes": m.nodes.len(),
            "nodesUp": up,
            "tip": majority.as_ref().map(|h| json!({"height": h.height, "hash": h.hash})),
            "agreeing": majority.as_ref().map(|h| h.nodes.len()),
            "disagreeing": majority.as_ref().map(|h| h.disagreeing.clone()),
            "seq": self.bus.last_seq(),
            "version": env!("CARGO_PKG_VERSION"),
            "chain": m.chain_name,
            "rpcCalls": rpc,
            "replay": self.replay.as_ref().map(|r| r.json()),
            "public": self.public.is_some(),
            "wsOpen": self.public.as_ref().map(|p| p.ws_open.load(std::sync::atomic::Ordering::Relaxed)),
        })
    }
    /// `/api/snapshot` as a value (also what `--export` writes).
    pub async fn snapshot_json(&self) -> Value {
        let m = self.model.read().await;
        serde_json::to_value(m.snapshot(self.bus.last_seq(), SNAPSHOT_BLOCKS)).unwrap_or(Value::Null)
    }
    /// `/api/events?since=` as a value: capped at `public::EVENTS_CAP` under `--public`.
    pub fn events_json(&self, since: u64) -> Value {
        let mut list = self.bus.since(since);
        if self.public.is_some() {
            list.truncate(public::EVENTS_CAP);
        }
        serde_json::to_value(list).unwrap_or(Value::Null)
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let r = Router::new()
        .route("/", get(index))
        .route("/ui/{*path}", get(ui_file))
        .route("/api/health", get(health))
        .route("/api/snapshot", get(snapshot))
        .route("/api/yellowback", get(yellowback))
        .route("/api/revenue", get(revenue))
        .route("/api/events", get(events))
        .route("/ws", get(ws));
    let r = match &state.public {
        Some(p) => r.layer(axum::middleware::from_fn_with_state(p.clone(), public::rate_limit)),
        None => r,
    };
    r.with_state(state)
}

/// Every embedded `ui/` file as (relative path, bytes), for `--export`.
pub fn ui_files() -> Vec<(String, &'static [u8])> {
    ui_paths().into_iter().filter_map(|p| UI.get_file(&p).map(|f| (p, f.contents()))).collect()
}

/// Every path embedded from `ui/` (relative, `/`-separated), for the served-files test.
pub fn ui_paths() -> Vec<String> {
    fn walk(dir: &Dir<'static>, out: &mut Vec<String>) {
        for f in dir.files() {
            out.push(f.path().to_string_lossy().replace('\\', "/"));
        }
        for d in dir.dirs() {
            walk(d, out);
        }
    }
    let mut out = Vec::new();
    walk(&UI, &mut out);
    out.sort();
    out
}

async fn index() -> Response {
    serve_ui("index.html")
}

async fn ui_file(Path(path): Path<String>) -> Response {
    serve_ui(&path)
}

fn serve_ui(path: &str) -> Response {
    match UI.get_file(path) {
        Some(f) => {
            let mime = match path.rsplit('.').next() {
                Some("html") => "text/html; charset=utf-8",
                Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("json") => "application/json",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                _ => "application/octet-stream",
            };
            ([(header::CONTENT_TYPE, mime)], f.contents()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn health(State(s): State<Arc<AppState>>) -> Json<Value> {
    let v = s.health_json().await;
    s.finish(v)
}

async fn snapshot(State(s): State<Arc<AppState>>) -> Json<Value> {
    let v = s.snapshot_json().await;
    s.finish(v)
}

/// The health panel's slice: the `yellowback` section plus, per main-chain block, its `yb`
/// view (tag, miner, txs) — much smaller than the whole snapshot, fetched once per block.
async fn yellowback(State(s): State<Arc<AppState>>) -> Json<Value> {
    let m = s.model.read().await;
    let chain = m.chain.snapshot(SNAPSHOT_BLOCKS);
    let blocks: Vec<Value> = chain.main.iter().map(|b| json!({"hash": b.hash, "height": b.height, "time": b.time, "txCount": b.tx_count, "yb": b.yb})).collect();
    let v = json!({
        "seq": s.bus.last_seq(),
        "tip": chain.majority.as_ref().map(|h| json!({"height": h.height, "hash": h.hash})),
        "yedInfo": m.yed_info,
        "yellowback": m.yellowback.snapshot(m.leader(), &m.healthy()),
        "blocks": blocks,
    });
    drop(m);
    s.finish(v)
}

#[derive(Deserialize, Default)]
struct RevenueQuery {
    from: Option<u64>,
    to: Option<u64>,
    #[serde(default)]
    by: String,
}

/// `GET /api/revenue?from=<h>&to=<h>&by=payoutKey|attestor|block` (plan §3.5, C4): the ledger
/// rolled up over `[from, to]` (default: the whole kept window), USD at each height's `pMint`.
async fn revenue(State(s): State<Arc<AppState>>, Query(q): Query<RevenueQuery>) -> Json<Value> {
    let m = s.model.read().await;
    let (wf, wt) = m.revenue.window().unwrap_or((0, 0));
    let from = q.from.unwrap_or(wf);
    let to = q.to.unwrap_or(wt);
    let by = if q.by.is_empty() { "payoutKey" } else { q.by.as_str() };
    let mut v = m.revenue.query(from, to, by, &|h| m.p_mint_at(h), &m.yellowback.miners, &m.yellowback.attestors);
    if let Some(o) = v.as_object_mut() {
        o.insert("seq".into(), s.bus.last_seq().into());
        o.insert("tip".into(), m.chain.majority().map(|h| json!({"height": h.height, "hash": h.hash})).unwrap_or(Value::Null));
        o.insert("enforcing".into(), json!(m.yed_info.values().map(|i| i.enforcing).collect::<Vec<_>>()));
        o.insert("pMintNow".into(), m.yellowback.stats.get(m.leader().as_deref().unwrap_or("")).map(|st| st.p_mint).into());
    }
    drop(m);
    s.finish(v)
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: u64,
}

async fn events(State(s): State<Arc<AppState>>, Query(q): Query<Since>) -> Json<Value> {
    s.finish(s.events_json(q.since))
}

async fn ws(State(s): State<Arc<AppState>>, upgrade: WebSocketUpgrade) -> Response {
    // Under --public the connection is counted before the upgrade; the guard lives with the session.
    let guard = match &s.public {
        Some(p) => match p.ws_guard() {
            Some(g) => Some(g),
            None => return (StatusCode::SERVICE_UNAVAILABLE, "too many websocket connections").into_response(),
        },
        None => None,
    };
    upgrade.on_upgrade(move |socket| async move {
        let _guard = guard;
        ws_session(socket, s).await
    })
}

async fn ws_session(mut socket: WebSocket, s: Arc<AppState>) {
    let redact = s.public.is_some();
    let mut rx = s.bus.subscribe();
    let hello = json!({"kind": "hello", "seq": s.bus.last_seq(), "version": env!("CARGO_PKG_VERSION")});
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(e) => {
                    let text = if redact {
                        let Ok(mut v) = serde_json::to_value(&e) else { continue };
                        public::redact(&mut v);
                        v.to_string()
                    } else {
                        let Ok(text) = serde_json::to_string(&e) else { continue };
                        text
                    };
                    if socket.send(Message::Text(text.into())).await.is_err() { return; }
                }
                Err(RecvError::Lagged(n)) => {
                    debug!("ws client lagged {} events", n);
                    let note = json!({"kind": "lagged", "dropped": n, "seq": s.bus.last_seq()});
                    if socket.send(Message::Text(note.to_string().into())).await.is_err() { return; }
                }
                Err(RecvError::Closed) => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            }
        }
    }
}
