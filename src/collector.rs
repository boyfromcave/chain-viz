//! One task per node: on every wake (poll tick or ZMQ), check the best block, walk any new
//! blocks back to a known ancestor, refresh `getchaintips`, diff the mempool, and watch
//! `yed_getinfo`'s counters. Everything it learns goes into the shared `Model` and out through
//! the `Bus`. Plan §7's load budget: per node per interval one `getbestblockhash`, one
//! `getrawmempool true`, one `yed_getinfo`; `getblock` once per new hash; `getchaintips` only
//! when the head moved.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::bus::Bus;
use crate::events::{now, EventKind};
use crate::model::chain::{ChainModel, ChainSnapshot, Emitted};
use crate::model::mempool::{MempoolModel, MempoolSnapshot};
use crate::rpc::{Block, NodeConfig, RpcClient, RpcError, YedInfo};
use crate::source::poll::PollSource;
use crate::source::zmq::ZmqSource;
use crate::source::{Source, Wake};

/// How far back the first fetch of a node walks when the model is empty.
pub const BACKFILL: u64 = 20;
/// `getchaintips` is refreshed on every head move and every this many polls otherwise.
pub const TIPS_EVERY: u64 = 10;

/// What the snapshot says about one node (never its URL or credentials).
#[derive(Debug, Clone, Serialize)]
pub struct NodeView {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub sources: Vec<String>,
    /// `true` once the node answered; `false` after a transport error.
    pub up: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `yed_getinfo` works on this node (a stock node says `false`).
    pub yellowback: Option<bool>,
    #[serde(rename = "lastSeen")]
    pub last_seen: f64,
}

/// The whole in-memory picture. Later chunks add `yellowback` and `revenue` beside these.
#[derive(Default)]
pub struct Model {
    pub chain_name: String,
    pub chain: ChainModel,
    pub mempool: MempoolModel,
    pub nodes: BTreeMap<String, NodeView>,
    /// The last `yed_getinfo` per node, whole (C3 models it; C1 only diffs the counters).
    pub yed_info: BTreeMap<String, YedInfo>,
    pub devnet: BTreeMap<String, Value>,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub version: &'static str,
    pub schema: u32,
    pub seq: u64,
    pub ts: f64,
    #[serde(rename = "chainName")]
    pub chain_name: String,
    pub nodes: Vec<NodeView>,
    pub chain: ChainSnapshot,
    pub mempool: MempoolSnapshot,
    #[serde(rename = "yedInfo")]
    pub yed_info: BTreeMap<String, YedInfo>,
    pub devnet: BTreeMap<String, Value>,
}

impl Model {
    pub fn snapshot(&self, seq: u64, blocks: usize) -> Snapshot {
        Snapshot {
            version: env!("CARGO_PKG_VERSION"),
            schema: crate::events::SCHEMA_VERSION,
            seq,
            ts: now(),
            chain_name: self.chain_name.clone(),
            nodes: self.nodes.values().cloned().collect(),
            chain: self.chain.snapshot(blocks),
            mempool: self.mempool.snapshot(),
            yed_info: self.yed_info.clone(),
            devnet: self.devnet.clone(),
        }
    }
}

pub struct Collector {
    pub model: Arc<RwLock<Model>>,
    pub bus: Arc<Bus>,
    pub clients: Vec<RpcClient>,
    pub poll: Duration,
    pub devnet_dir: Option<PathBuf>,
    /// Serializes each node's first fetch so only the first one walks `BACKFILL` blocks.
    pub backfill: tokio::sync::Mutex<()>,
}

impl Collector {
    pub fn start(self: Arc<Self>) {
        for client in self.clients.clone() {
            let this = self.clone();
            tokio::spawn(async move { this.run_node(client).await });
        }
        if let Some(dir) = self.devnet_dir.clone() {
            let this = self.clone();
            tokio::spawn(async move { this.watch_devnet(dir).await });
        }
    }

    async fn run_node(&self, client: RpcClient) {
        let node = client.id().to_string();
        let (tx, mut rx) = mpsc::channel::<Wake>(64);
        let mut sources: Vec<Box<dyn Source>> = vec![Box::new(PollSource { node: node.clone(), interval: self.poll })];
        if let Some(url) = client.node().zmq.clone() {
            sources.push(Box::new(ZmqSource { node: node.clone(), url }));
        }
        let names: Vec<String> = sources.iter().map(|s| s.name()).collect();
        {
            let mut m = self.model.write().await;
            m.nodes.insert(node.clone(), NodeView { id: node.clone(), role: client.node().role.clone(), sources: names.clone(), up: false, error: None, yellowback: None, last_seen: 0.0 });
        }
        info!(node = %node, "sources: {}", names.join(", "));
        for s in sources {
            s.spawn(tx.clone());
        }
        let mut st = NodeState::default();
        while let Some(wake) = rx.recv().await {
            let mut want_head = matches!(wake, Wake::Tick | Wake::Block(_));
            let mut want_mempool = true;
            // Coalesce whatever queued while we were busy.
            while let Ok(w) = rx.try_recv() {
                want_head |= matches!(w, Wake::Tick | Wake::Block(_));
                want_mempool |= matches!(w, Wake::Tick | Wake::Tx(_));
            }
            let result = self.step(&client, &mut st, want_head, want_mempool).await;
            let mut m = self.model.write().await;
            if let Some(v) = m.nodes.get_mut(&node) {
                match &result {
                    Ok(()) => {
                        if !v.up {
                            if v.last_seen > 0.0 {
                                self.bus.publish(None, Some(node.clone()), EventKind::Note { text: format!("node {} is back", node) });
                            }
                            v.up = true;
                        }
                        v.error = None;
                        v.last_seen = now();
                        v.yellowback = st.yed_enabled;
                    }
                    Err(e) => {
                        if v.up || v.error.is_none() {
                            warn!(node = %node, "{}", e);
                            self.bus.publish(None, Some(node.clone()), EventKind::Note { text: format!("node {}: {}", node, e) });
                        }
                        v.up = false;
                        v.error = Some(e.to_string());
                    }
                }
            }
        }
    }

    async fn step(&self, client: &RpcClient, st: &mut NodeState, want_head: bool, want_mempool: bool) -> Result<(), RpcError> {
        let node = client.id().to_string();
        if st.chain_name.is_none() {
            let info = client.get_blockchain_info().await?;
            let mut m = self.model.write().await;
            if m.chain_name.is_empty() {
                m.chain_name = info.chain.clone();
            }
            st.chain_name = Some(info.chain);
        }
        if want_head {
            let best = client.get_best_block_hash().await?;
            st.ticks += 1;
            // Tips are refreshed when the head moved and every TIPS_EVERY polls besides: a
            // competing block that reaches a node as a header only (`valid-headers`) moves no
            // head anywhere, and getchaintips walks the whole block index, so not every poll.
            if st.head.as_deref() != Some(&best) || st.ticks % TIPS_EVERY == 0 {
                let path = if st.head.as_deref() != Some(&best) {
                    let first = st.head.is_none();
                    let guard = if first { Some(self.backfill.lock().await) } else { None };
                    let path = self.fetch_path(client, &best, first).await?;
                    drop(guard);
                    path
                } else {
                    Vec::new()
                };
                let tips = client.get_chain_tips().await?;
                let emitted = {
                    let mut m = self.model.write().await;
                    let mut ev = if path.is_empty() { Vec::new() } else { m.chain.on_new_head(&node, &path, now()) };
                    ev.extend(m.chain.on_chain_tips(&node, tips.clone(), now()));
                    ev
                };
                for e in &emitted {
                    if let EventKind::Reorg { depth, from, to, .. } = &e.kind {
                        info!(node = %node, "reorg depth {} {} -> {}", depth, short(from), short(to));
                    }
                }
                self.bus.publish_all(emitted);
                st.head = Some(best);
                st.side_tips = tips.iter().filter(|t| t.status != "active").map(|t| t.hash.clone()).collect();
            }
            if st.yed_enabled != Some(false) {
                match client.yed_getinfo().await {
                    Ok(info) => {
                        st.yed_enabled = Some(true);
                        self.on_yed_info(client, st, info).await?;
                    }
                    Err(e) if e.is_method_not_found() => {
                        st.yed_enabled = Some(false);
                        debug!(node = %node, "no yed_getinfo: a stock node");
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if want_mempool {
            let entries = client.get_raw_mempool().await?;
            let emitted = {
                let mut m = self.model.write().await;
                let Model { chain, mempool, .. } = &mut *m;
                mempool.on_snapshot(&node, &entries, now(), |t| chain.is_mined(t))
            };
            self.bus.publish_all(emitted);
        }
        Ok(())
    }

    /// The blocks from the last one the model knows (exclusive) to `best`, oldest first.
    async fn fetch_path(&self, client: &RpcClient, best: &str, first: bool) -> Result<Vec<Block>, RpcError> {
        let limit = if first { BACKFILL } else { u64::MAX };
        let mut path = Vec::new();
        let mut hash = best.to_string();
        loop {
            let b = client.get_block(&hash).await?;
            let prev = b.previousblockhash.clone();
            path.push(b);
            match prev {
                Some(p) if path.len() as u64 <= limit && !self.model.read().await.chain.knows(&p) => hash = p,
                _ => break,
            }
        }
        path.reverse();
        Ok(path)
    }

    async fn on_yed_info(&self, client: &RpcClient, st: &mut NodeState, info: YedInfo) -> Result<(), RpcError> {
        let node = client.id().to_string();
        let mut emitted = Vec::new();
        let prev = self.model.read().await.yed_info.get(&node).cloned();
        if let Some(prev) = &prev {
            let fields: [(&str, Value, Value); 7] = [
                ("rejectedBlocks", prev.rejected_blocks.into(), info.rejected_blocks.into()),
                ("suppressedBlocks", prev.suppressed_blocks.into(), info.suppressed_blocks.into()),
                ("healthy", prev.healthy.into(), info.healthy.into()),
                ("enforcing", prev.enforcing.into(), info.enforcing.into()),
                ("valveTripped", prev.valve_tripped.into(), info.valve_tripped.into()),
                ("sunset", prev.sunset.into(), info.sunset.into()),
                ("abandoned", prev.abandoned.into(), info.abandoned.into()),
            ];
            for (field, from, to) in fields {
                if from != to {
                    emitted.push(Emitted { height: Some(info.height), node: Some(node.clone()), kind: EventKind::YbState { field: field.into(), from, to } });
                }
            }
        }
        let rejected_grew = prev.map(|p| info.rejected_blocks > p.rejected_blocks).unwrap_or(false);
        self.model.write().await.yed_info.insert(node.clone(), info.clone());
        if rejected_grew {
            // Ask the verdict of every side tip this node holds that we have not asked about.
            let tips: Vec<String> = st.side_tips.iter().filter(|h| !st.verdicts.contains(*h)).cloned().collect();
            for hash in tips {
                match client.yed_getblockverdict(&hash).await {
                    Ok(v) => {
                        st.verdicts.insert(hash.clone());
                        if v.block_invalid {
                            let height = self.model.read().await.chain.block(&hash).map(|b| b.height);
                            emitted.push(Emitted { height, node: Some(node.clone()), kind: EventKind::RejectedBlock { hash, verdict: serde_json::to_value(&v).unwrap_or(Value::Null) } });
                        }
                    }
                    Err(e) => warn!(node = %node, "yed_getblockverdict {}: {}", short(&hash), e),
                }
            }
        }
        self.bus.publish_all(emitted);
        Ok(())
    }

    /// `--devnet`: `heartbeat.json` and `sim-stats.json` beside `devnet.json`, re-read every
    /// poll; a change is an event.
    async fn watch_devnet(&self, dir: PathBuf) {
        let mut last: BTreeMap<&str, Value> = BTreeMap::new();
        loop {
            for (name, file) in [("heartbeat", "heartbeat.json"), ("sim", "sim-stats.json")] {
                let Ok(text) = tokio::fs::read_to_string(dir.join(file)).await else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if last.get(name) == Some(&v) {
                    continue;
                }
                last.insert(name, v.clone());
                self.model.write().await.devnet.insert(name.to_string(), v.clone());
                let height = v.get("height").and_then(Value::as_u64);
                let kind = if name == "heartbeat" { EventKind::DevnetHeartbeat(v) } else { EventKind::DevnetSim(v) };
                self.bus.publish(height, None, kind);
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

#[derive(Default)]
struct NodeState {
    chain_name: Option<String>,
    head: Option<String>,
    ticks: u64,
    yed_enabled: Option<bool>,
    side_tips: Vec<String>,
    verdicts: HashSet<String>,
}

pub fn short(hash: &str) -> &str {
    let n = hash.len();
    if n > 12 {
        &hash[n - 12..]
    } else {
        hash
    }
}

/// Build one client per configured node.
pub fn clients(nodes: &[NodeConfig], concurrency: usize) -> Vec<RpcClient> {
    nodes.iter().map(|n| RpcClient::new(n.clone(), concurrency)).collect()
}
