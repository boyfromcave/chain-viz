//! `--replay <file> [--speed N]`: the server with no node. The events of a recorded session are
//! applied to the same `Model` and published on the same `Bus` the collector would use, so the
//! UI and the API are unchanged; only what RPC alone supplies (`yedInfo`, `chain.tips`, the
//! per-node `up`/`lastSeen`, cross-node mempool presence) is absent or reconstructed from the
//! events (see `apply`). Inter-event gaps are waited ÷ `speed`; `--speed 0` is as fast as
//! possible; a `session` header inside the file (a restart) is a run boundary and waits nothing.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

use crate::bus::Bus;
use crate::collector::{Model, NodeView};
use crate::events::{Event, EventKind};
use crate::rpc::MempoolEntry;

/// What `/api/health.replay` reports.
pub struct ReplayStatus {
    pub file: String,
    pub pos: AtomicU64,
    pub total: u64,
    pub speed: f64,
}

impl ReplayStatus {
    pub fn new(file: String, total: u64, speed: f64) -> ReplayStatus {
        ReplayStatus { file, pos: AtomicU64::new(0), total, speed }
    }
    pub fn json(&self) -> Value {
        json!({"file": self.file, "pos": self.pos.load(Ordering::Relaxed), "total": self.total, "speed": self.speed})
    }
    pub fn done(&self) -> bool {
        self.pos.load(Ordering::Relaxed) >= self.total
    }
}

/// Apply one recorded event to the model, the way the collector's RPC path would have.
pub fn apply(model: &mut Model, e: &Event) {
    let node = e.node.as_deref();
    match &e.kind {
        EventKind::Session { nodes, chain, .. } => {
            if !chain.is_empty() {
                model.chain_name = chain.clone();
            }
            for id in nodes {
                model.nodes.entry(id.clone()).or_insert_with(|| NodeView { id: id.clone(), role: None, sources: vec!["replay".into()], up: true, error: None, yellowback: None, last_seen: e.ts });
            }
        }
        EventKind::Block(b) => {
            if let Some(h) = e.height {
                model.chain.apply_block(node, h, b, e.ts);
            }
        }
        EventKind::BlockSide { block, .. } => {
            if let Some(h) = e.height {
                model.chain.apply_block_side(h, block, e.ts);
            }
        }
        EventKind::Orphaned { hash } => model.chain.apply_orphaned(hash),
        EventKind::Tip { hash } => {
            if let (Some(n), Some(h)) = (node, e.height) {
                model.chain.apply_tip(n, h, hash);
                if let Some(v) = model.nodes.get_mut(n) {
                    v.last_seen = e.ts;
                }
            }
        }
        EventKind::Reorg { .. } => {} // the tip and orphaned events around it carry the change
        EventKind::MempoolAdd { txid, size, fee, time, depends } => {
            let entry = MempoolEntry { size: *size, fee: *fee, time: *time, depends: depends.clone(), ..Default::default() };
            model.mempool.apply_add(node.unwrap_or(""), txid, &entry, e.ts);
        }
        EventKind::MempoolRemove { txid, .. } => model.mempool.apply_remove(txid),
        EventKind::DevnetHeartbeat(v) => {
            model.devnet.insert("heartbeat".into(), v.clone());
        }
        EventKind::DevnetSim(v) => {
            model.devnet.insert("sim".into(), v.clone());
        }
        // Yellowback, price, stats, attestor, vault and yolo events reach the UI through the bus
        // and `/api/events`; their models (C3, C4) can add their own arm here.
        _ => {}
    }
    if let Some(n) = node {
        if !model.nodes.contains_key(n) {
            model.nodes.insert(n.to_string(), NodeView { id: n.to_string(), role: None, sources: vec!["replay".into()], up: true, error: None, yellowback: None, last_seen: e.ts });
        }
    }
}

/// Feed `events` into `model` and `bus`, waiting each inter-event gap ÷ `speed` (0 = no wait).
/// Every event is republished with its recorded `ts` and a fresh `seq`.
pub async fn run(model: Arc<RwLock<Model>>, bus: Arc<Bus>, events: Vec<Event>, speed: f64, status: Arc<ReplayStatus>) {
    let mut prev_ts: Option<f64> = None;
    let mut nodes = BTreeSet::new();
    for e in &events {
        let boundary = matches!(e.kind, EventKind::Session { .. });
        if let (Some(p), false, true) = (prev_ts, boundary, speed > 0.0) {
            let gap = (e.ts - p).max(0.0) / speed;
            if gap > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(gap)).await;
            }
        }
        prev_ts = Some(e.ts);
        {
            let mut m = model.write().await;
            apply(&mut m, e);
        }
        if let Some(n) = &e.node {
            nodes.insert(n.clone());
        }
        bus.publish_at(e.ts, e.height, e.node.clone(), e.kind.clone());
        status.pos.fetch_add(1, Ordering::Relaxed);
    }
    let tip = model.read().await.chain.majority();
    info!("replay done: {} events, {} nodes, tip {}", events.len(), nodes.len(), tip.map(|t| format!("{} {}", t.height, t.hash)).unwrap_or_else(|| "none".into()));
}

/// Apply a whole session without waiting (tests, `--export`).
pub fn apply_all(model: &mut Model, events: &[Event]) {
    for e in events {
        apply(model, e);
    }
}
