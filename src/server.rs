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
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ui/{*path}", get(ui_file))
        .route("/api/health", get(health))
        .route("/api/snapshot", get(snapshot))
        .route("/api/yellowback", get(yellowback))
        .route("/api/events", get(events))
        .route("/ws", get(ws))
        .with_state(state)
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
    let m = s.model.read().await;
    let majority = m.chain.majority();
    let up = m.nodes.values().filter(|n| n.up).count();
    let rpc: BTreeMap<String, BTreeMap<String, u64>> = s.clients.iter().map(|c| (c.id().to_string(), c.counter.snapshot())).collect();
    Json(json!({
        "ok": up > 0,
        "nodes": m.nodes.len(),
        "nodesUp": up,
        "tip": majority.as_ref().map(|h| json!({"height": h.height, "hash": h.hash})),
        "agreeing": majority.as_ref().map(|h| h.nodes.len()),
        "disagreeing": majority.as_ref().map(|h| h.disagreeing.clone()),
        "seq": s.bus.last_seq(),
        "version": env!("CARGO_PKG_VERSION"),
        "chain": m.chain_name,
        "rpcCalls": rpc,
        "replay": s.replay.as_ref().map(|r| r.json()),
    }))
}

async fn snapshot(State(s): State<Arc<AppState>>) -> Json<Value> {
    let m = s.model.read().await;
    Json(serde_json::to_value(m.snapshot(s.bus.last_seq(), SNAPSHOT_BLOCKS)).unwrap_or(Value::Null))
}

/// The health panel's slice: the `yellowback` section plus, per main-chain block, its `yb`
/// view (tag, miner, txs) — much smaller than the whole snapshot, fetched once per block.
async fn yellowback(State(s): State<Arc<AppState>>) -> Json<Value> {
    let m = s.model.read().await;
    let chain = m.chain.snapshot(SNAPSHOT_BLOCKS);
    let blocks: Vec<Value> = chain.main.iter().map(|b| json!({"hash": b.hash, "height": b.height, "time": b.time, "txCount": b.tx_count, "yb": b.yb})).collect();
    Json(json!({
        "seq": s.bus.last_seq(),
        "tip": chain.majority.as_ref().map(|h| json!({"height": h.height, "hash": h.hash})),
        "yedInfo": m.yed_info,
        "yellowback": m.yellowback.snapshot(m.leader(), &m.healthy()),
        "blocks": blocks,
    }))
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: u64,
}

async fn events(State(s): State<Arc<AppState>>, Query(q): Query<Since>) -> Json<Value> {
    Json(serde_json::to_value(s.bus.since(q.since)).unwrap_or(Value::Null))
}

async fn ws(State(s): State<Arc<AppState>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| ws_session(socket, s))
}

async fn ws_session(mut socket: WebSocket, s: Arc<AppState>) {
    let mut rx = s.bus.subscribe();
    let hello = json!({"kind": "hello", "seq": s.bus.last_seq(), "version": env!("CARGO_PKG_VERSION")});
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(e) => {
                    let Ok(text) = serde_json::to_string(&e) else { continue };
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
