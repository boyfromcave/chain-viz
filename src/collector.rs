// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! One task per node: on every wake (poll tick or ZMQ), check the best block, walk any new
//! blocks back to a known ancestor, refresh `getchaintips`, diff the mempool, and watch
//! `yed_getinfo`'s counters. Everything it learns goes into the shared `Model` and out through
//! the `Bus`. Plan §7's load budget: per node per interval one `getbestblockhash`, one
//! `getrawmempool true`, one `yed_getinfo`; `getblock` once per new hash; `getchaintips` only
//! when the head moved.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::bus::Bus;
use crate::classify;
use crate::events::{now, EventKind};
use crate::model::chain::{ChainModel, ChainSnapshot, Emitted};
use crate::model::mempool::{MempoolModel, MempoolSnapshot};
use crate::model::revenue::{usd, RevenueModel};
use crate::model::yellowback::{BlockYb, YbTx, YellowbackModel, YellowbackSnapshot, HISTORY_PAGE, VAULT_PAGE};
use crate::rpc::{Block, BlockFull, NodeConfig, RpcClient, RpcError, YedInfo};
use crate::source::poll::PollSource;
use crate::source::zmq::ZmqSource;
use crate::source::{Source, Wake};

/// How far back the first fetch of a node walks when the model is empty.
pub const BACKFILL: u64 = 20;
/// `getchaintips` is refreshed on every head move and every this many polls otherwise.
pub const TIPS_EVERY: u64 = 10;
/// Unenriched blocks a `yed_*` node catches up on per poll.
pub const ENRICH_PER_POLL: usize = 8;

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
    /// The Yellowback health model (C3).
    pub yellowback: YellowbackModel,
    /// The revenue ledger (C4).
    pub revenue: RevenueModel,
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
    pub yellowback: YellowbackSnapshot,
    /// Cumulative revenue rollups (`model/revenue.rs`), never evicted.
    pub revenue: Value,
}

impl Model {
    /// Nodes whose `yed_getinfo` says `healthy` and that answered their last poll.
    pub fn healthy(&self) -> BTreeSet<String> {
        self.nodes.values().filter(|n| n.up && self.yed_info.get(&n.id).map(|i| i.healthy).unwrap_or(false)).map(|n| n.id.clone()).collect()
    }
    /// The lowest-id node not known to be stock or down: whose chain-wide answers are shown.
    /// Deterministic from the start (every node begins unknown), so the startup race between
    /// eleven first polls elects one leader, not several.
    pub fn leader(&self) -> Option<String> {
        self.nodes.values().filter(|n| n.yellowback != Some(false) && n.error.is_none()).map(|n| n.id.clone()).min_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX))
    }

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
            yellowback: self.yellowback.snapshot(self.leader(), &self.healthy()),
            revenue: self.revenue.snapshot(&|h| self.p_mint_at(h)),
        }
    }

    /// `pMint` at `height` from the timeline (`yed_gethistory` / live `yed_getstats` rows), the
    /// price every USD figure of the revenue view uses (C-9).
    /// The row at `height`, else the latest row below it (a block's coinbase rows are ledgered
    /// before that height's `yed_getstats` row lands; `yed_gethistory` fills the exact one later).
    pub fn p_mint_at(&self, height: u64) -> Option<i64> {
        self.yellowback.history.range(..=height).rev().find_map(|(_, r)| r.get("pMint").and_then(Value::as_i64).filter(|p| *p > 0))
    }

    /// Fill the `usd` of `revenue` events from the price known now.
    pub fn price_revenue(&self, emitted: &mut [Emitted]) {
        for e in emitted.iter_mut() {
            if let EventKind::Revenue { zat, usd: u, .. } = &mut e.kind {
                *u = e.height.and_then(|h| usd(*zat, self.p_mint_at(h)));
            }
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
        if st.yed_enabled.is_none() {
            // Know before the first block walk whether this node can enrich (yed_gettag,
            // yed_gettxinfo): one extra yed_getinfo, once per node.
            match client.yed_getinfo().await {
                Ok(_) => st.yed_enabled = Some(true),
                Err(e) if e.is_method_not_found() => st.yed_enabled = Some(false),
                Err(e) => return Err(e),
            }
            if let Some(v) = self.model.write().await.nodes.get_mut(&node) {
                v.yellowback = st.yed_enabled;
            }
        }
        if want_head {
            let best = client.get_best_block_hash().await?;
            st.ticks += 1;
            // Tips are refreshed when the head moved and every TIPS_EVERY polls besides: a
            // competing block that reaches a node as a header only (`valid-headers`) moves no
            // head anywhere, and getchaintips walks the whole block index, so not every poll.
            if st.head.as_deref() != Some(&best) || st.ticks % TIPS_EVERY == 0 {
                let full = if st.head.as_deref() != Some(&best) {
                    let first = st.head.is_none();
                    let guard = if first { Some(self.backfill.lock().await) } else { None };
                    let path = self.fetch_path(client, &best, first).await?;
                    drop(guard);
                    path
                } else {
                    Vec::new()
                };
                let path: Vec<Block> = full.iter().map(block_of).collect();
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
                if let Some(floor) = self.model.read().await.chain.floor() {
                    self.bus.evict_below(floor);
                }
                let moved = st.head.as_deref() != Some(&best);
                st.head = Some(best.clone());
                st.side_tips = tips.iter().filter(|t| t.status != "active").map(|t| t.hash.clone()).collect();
                // Yellowback: classify every block's transactions locally, then (on a node with
                // yed_*) one yed_gettag per block and one yed_gettxinfo per Yellowback tx.
                for b in &full {
                    self.classify_block(client, st, b).await?;
                }
                if moved && st.yed_enabled == Some(true) {
                    self.on_block_yed(client, st, &best).await?;
                }
            }
            if st.yed_enabled == Some(true) {
                self.enrich_pending(client).await?;
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
            let added: Vec<String> = emitted.iter().filter_map(|e| if let EventKind::MempoolAdd { txid, .. } = &e.kind { Some(txid.clone()) } else { None }).collect();
            self.bus.publish_all(emitted);
            self.classify_mempool(client, st, added).await?;
        }
        Ok(())
    }

    /// The Yellowback transactions of one fetched block: payload scan (no RPC), then
    /// `enrich_block` on a node with `yed_*`.
    async fn classify_block(&self, client: &RpcClient, st: &NodeState, b: &BlockFull) -> Result<(), RpcError> {
        {
            let mut m = self.model.write().await;
            let Model { chain, yellowback, .. } = &mut *m;
            let Some(info) = chain.block_mut(&b.hash) else { return Ok(()) };
            if info.yb.is_none() {
                let txs: Vec<YbTx> = b.tx.iter().filter_map(|t| classify::find_payload(t).map(|p| yellowback.tx(&t.txid).cloned().unwrap_or_else(|| YbTx::from_payload(&t.txid, p)))).collect();
                for t in &txs {
                    yellowback.upsert_tx(t.clone());
                }
                info.yb = Some(BlockYb { txs, ..Default::default() });
            }
        }
        self.revenue_block(client, b).await?;
        if st.yed_enabled == Some(true) {
            self.enrich_block(client, &b.hash, b.height).await?;
        }
        Ok(())
    }

    /// C4: the coinbase rows of a fetched block (`getblocksubsidy` once per height, any node),
    /// once per block hash.
    async fn revenue_block(&self, client: &RpcClient, b: &BlockFull) -> Result<(), RpcError> {
        let (wanted, need_subsidy) = {
            let m = self.model.read().await;
            (m.revenue.wants_block(&b.hash, b.height), !m.revenue.subsidy_known(b.height))
        };
        if !wanted {
            return Ok(());
        }
        let subsidy = if need_subsidy { Some(client.get_block_subsidy(Some(b.height)).await?) } else { None };
        let mut emitted = {
            let mut m = self.model.write().await;
            if let Some(s) = &subsidy {
                m.revenue.set_subsidy(b.height, s);
            }
            if !m.revenue.wants_block(&b.hash, b.height) {
                return Ok(());
            }
            let yb: Vec<String> = m.chain.block(&b.hash).and_then(|i| i.yb.as_ref()).map(|y| y.txs.iter().map(|t| t.txid.clone()).collect()).unwrap_or_default();
            let ev = m.revenue.on_block(b, &yb);
            for t in &yb {
                if let Some(tx) = m.yellowback.tx(t).cloned().filter(|t| t.info.is_some() && t.height == Some(b.height)) {
                    // enriched before this block was fetched here (another node's walk): attribute now
                    let enforcing = m.yed_info.get(client.id()).map(|i| i.enforcing).unwrap_or(true);
                    let more = m.revenue.on_yb_tx(b.height, &tx, enforcing);
                    let _ = more;
                }
            }
            ev
        };
        let m = self.model.read().await;
        m.price_revenue(&mut emitted);
        drop(m);
        self.bus.publish_all(emitted);
        Ok(())
    }

    /// On a node with `yed_*`: `yed_gettag` once per block hash and `yed_gettxinfo` once per
    /// Yellowback txid the block holds (cached by txid: a tx enriched in the mempool is asked
    /// again once, for its confirmed row).
    async fn enrich_block(&self, client: &RpcClient, hash: &str, height: u64) -> Result<(), RpcError> {
        let node = client.id().to_string();
        // Claim the block and each pending tx under the lock, so eleven node tasks seeing the
        // same block spend one yed_gettag and one yed_gettxinfo per tx between them.
        let (needs_tag, pending): (bool, Vec<YbTx>) = {
            let mut m = self.model.write().await;
            let Model { chain, yellowback, .. } = &mut *m;
            let Some(yb) = chain.block(hash).and_then(|b| b.yb.as_ref()) else { return Ok(()) };
            let needs_tag = !yb.tagged && yellowback.claim(hash);
            let pending: Vec<YbTx> = yb.txs.iter().filter(|t| t.height.is_none() || t.info.is_none()).cloned().collect();
            let pending = pending.into_iter().filter(|t| yellowback.claim(&t.txid)).collect();
            (needs_tag, pending)
        };
        if !needs_tag && pending.is_empty() {
            return Ok(());
        }
        let result = self.enrich_calls(client, hash, height, needs_tag, &pending).await;
        let mut m = self.model.write().await;
        m.yellowback.release(hash);
        for t in &pending {
            m.yellowback.release(&t.txid);
        }
        let (tag, done) = result?;
        let mut emitted = Vec::new();
        let mut attributed: Vec<YbTx> = Vec::new();
        {
            let Model { chain, yellowback, .. } = &mut *m;
            if let Some(info) = chain.block_mut(hash).and_then(|i| i.yb.as_mut()) {
                if let Some(tag) = tag {
                    info.miner = tag.get("payoutAddress").and_then(Value::as_str).map(str::to_string).filter(|_| tag.get("found").and_then(Value::as_bool).unwrap_or(false));
                    info.tag = Some(tag);
                    info.tagged = true;
                }
                for t in done {
                    if let Some(slot) = info.txs.iter_mut().find(|x| x.txid == t.txid) {
                        *slot = t.clone();
                    }
                    emitted.push(Emitted { height: Some(height), node: Some(node.clone()), kind: t.event() });
                    yellowback.upsert_tx(t.clone());
                    attributed.push(t);
                }
            }
        }
        // C4: the ledger rows of the tag (alias) and of each attributed transaction.
        {
            let enforcing = m.yed_info.get(&node).map(|i| i.enforcing).unwrap_or(true);
            if let Some(tag) = m.chain.block(hash).and_then(|b| b.yb.as_ref()).and_then(|y| y.tag.clone()) {
                m.revenue.on_tag(height, hash, &tag);
            }
            let mut rows = Vec::new();
            for t in &attributed {
                rows.extend(m.revenue.on_yb_tx(height, t, enforcing));
            }
            m.price_revenue(&mut rows);
            emitted.extend(rows);
        }
        drop(m);
        self.bus.publish_all(emitted);
        self.counterfactual(client).await
    }

    /// C4: `yed_getfeepayee R collat` once per refHeight the ledger's fee rows name, for |E(R)|
    /// (the counterfactual's denominator). A refusal (FEE-0, out of range) is remembered as
    /// "no eligible payee" rather than asked again.
    async fn counterfactual(&self, client: &RpcClient) -> Result<(), RpcError> {
        for _ in 0..8 {
            let Some((r, collat)) = self.model.read().await.revenue.next_ref() else { return Ok(()) };
            let n = match client.yed_getfeepayee(r, collat).await {
                Ok(v) => Some(v.get("eligible").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)),
                Err(RpcError::Node { message, .. }) => {
                    debug!(node = %client.id(), "yed_getfeepayee {}: {}", r, message);
                    None
                }
                Err(e) => return Err(e),
            };
            self.model.write().await.revenue.set_eligible(r, n);
        }
        Ok(())
    }

    /// The RPCs of `enrich_block`, outside the model lock.
    async fn enrich_calls(&self, client: &RpcClient, hash: &str, height: u64, needs_tag: bool, pending: &[YbTx]) -> Result<(Option<Value>, Vec<YbTx>), RpcError> {
        let node = client.id().to_string();
        let tag = if needs_tag { Some(client.yed_gettag(hash).await?) } else { None };
        let mut done = Vec::new();
        for t in pending {
            let mut t = t.clone();
            match client.yed_gettxinfo(&t.txid).await {
                Ok(info) => {
                    t.apply(&info, Some(height));
                    done.push(t);
                }
                Err(e) if matches!(e, RpcError::Node { .. }) => {
                    // The index has no row (a non-Yellowback tx by the node's rules, or a
                    // height the index has not applied yet): keep the payload-only view.
                    debug!(node = %node, "yed_gettxinfo {}: {}", short(&t.txid), e);
                    t.height = Some(height);
                    if t.verdict.is_empty() {
                        t.verdict = "unindexed".into();
                    }
                    done.push(t);
                }
                Err(e) => return Err(e),
            }
        }
        Ok((tag, done))
    }

    /// Blocks a stock node fetched (the backfill, or a block it saw first) that no `yed_*`
    /// node has enriched yet: a few per poll.
    async fn enrich_pending(&self, client: &RpcClient) -> Result<(), RpcError> {
        let pending = self.model.read().await.chain.untagged(ENRICH_PER_POLL);
        for (hash, height) in pending {
            self.enrich_block(client, &hash, height).await?;
        }
        Ok(())
    }

    /// New mempool txids: `getrawtransaction` once per txid (any node), payload scan; for a
    /// Yellowback tx on a node with `yed_*`, `yed_decodepayload` + `yed_validaterawtransaction`.
    async fn classify_mempool(&self, client: &RpcClient, st: &NodeState, added: Vec<String>) -> Result<(), RpcError> {
        let node = client.id().to_string();
        let mut todo: Vec<String> = Vec::new();
        {
            let mut m = self.model.write().await;
            for t in added {
                if m.yellowback.check(&t) {
                    todo.push(t);
                }
            }
            // Yellowback txs a stock node classified earlier and nobody validated yet.
            if st.yed_enabled == Some(true) {
                let Model { mempool, yellowback, .. } = &mut *m;
                for t in mempool.snapshot().txs {
                    if let Some(yb) = &t.yb {
                        if yb.info.is_none() && yellowback.raw_hex(&t.txid).is_some() && !todo.contains(&t.txid) {
                            todo.push(t.txid.clone());
                        }
                    }
                }
            }
        }
        for txid in todo {
            let raw = {
                let hex = self.model.read().await.yellowback.raw_hex(&txid);
                match hex {
                    Some(h) => h,
                    None => match client.get_raw_transaction(&txid).await {
                        Ok(tx) => {
                            if let Some(p) = classify::find_payload(&tx) {
                                let mut m = self.model.write().await;
                                let yb = YbTx::from_payload(&txid, p);
                                m.yellowback.upsert_tx(yb.clone());
                                m.yellowback.keep_raw(&txid, &tx.hex);
                                m.mempool.set_yb(&txid, yb);
                                tx.hex
                            } else {
                                continue;
                            }
                        }
                        Err(RpcError::Node { .. }) => continue, // gone from the mempool between the two calls
                        Err(e) => return Err(e),
                    },
                }
            };
            if st.yed_enabled != Some(true) {
                continue;
            }
            let decoded = client.yed_decodepayload(&raw).await.ok();
            let mut verdict = match client.yed_validaterawtransaction(&raw).await {
                Ok(v) => v,
                Err(RpcError::Node { message, .. }) => json!({"valid": false, "verdict": message}),
                Err(e) => return Err(e),
            };
            if let (Some(o), Some(d)) = (verdict.as_object_mut(), decoded) {
                o.insert("decoded".into(), d);
            }
            let event = {
                let mut m = self.model.write().await;
                let Some(mut yb) = m.yellowback.tx(&txid).cloned() else { continue };
                yb.apply(&verdict, None);
                yb.height = None;
                m.yellowback.upsert_tx(yb.clone());
                m.yellowback.drop_raw(&txid);
                m.mempool.set_yb(&txid, yb.clone());
                Emitted { height: None, node: Some(node.clone()), kind: yb.event() }
            };
            self.bus.publish_all(vec![event]);
        }
        Ok(())
    }

    /// Per block on a node with `yed_*`: `yed_getstats` and `yed_getstatehash`; the leader
    /// adds `yed_getprice`, `yed_getactivation`, `yed_listminers`, `yed_listattestors`,
    /// `yed_listclaimable`, `yed_listvaults` when the vault counts changed, and the one-time
    /// `yed_gethistory` backfill.
    async fn on_block_yed(&self, client: &RpcClient, st: &mut NodeState, best: &str) -> Result<(), RpcError> {
        let node = client.id().to_string();
        let leader = {
            let m = self.model.read().await;
            m.leader().as_deref() == Some(&node)
        };
        let stats = client.yed_getstats().await?;
        let mut emitted = Vec::new();
        let (stale, backfill_from) = {
            let mut m = self.model.write().await;
            emitted.extend(m.yellowback.on_stats(&node, stats.clone(), leader));
            let start = m.yed_info.get(&node).and_then(|i| i.params.get("startHeight")).and_then(Value::as_u64).unwrap_or(1);
            let from = if leader && !m.yellowback.backfilling {
                m.yellowback.backfilling = true;
                Some(m.yellowback.backfill_from(stats.height, start))
            } else {
                None
            };
            (m.yellowback.vaults_stale(&stats), from)
        };
        match client.yed_getstatehash().await {
            Ok(h) => {
                let mut m = self.model.write().await;
                let healthy = m.healthy();
                if let Some(e) = m.yellowback.on_statehash(&node, h, &healthy) {
                    if let EventKind::StatehashMismatch { hash, .. } = &e.kind {
                        warn!("statehash mismatch on {}", short(hash));
                    }
                    emitted.push(e);
                }
            }
            Err(e) if matches!(e, RpcError::Node { .. }) => debug!(node = %node, "yed_getstatehash: {}", e),
            Err(e) => return Err(e),
        }
        self.bus.publish_all(std::mem::take(&mut emitted));
        if !leader {
            return Ok(());
        }
        if let Some(from) = backfill_from {
            let to = stats.height;
            info!(node = %node, "yed_gethistory backfill {}..{}", from, to);
            let mut lo = from;
            while lo <= to {
                let hi = (lo + HISTORY_PAGE - 1).min(to);
                let rows = client.yed_gethistory(lo, hi).await?;
                self.model.write().await.yellowback.on_history(rows);
                lo = hi + 1;
            }
            self.model.write().await.yellowback.backfilled_to = Some(to);
        }
        let price = client.yed_getprice(None).await?;
        let activation = client.yed_getactivation().await?;
        let miners = client.yed_listminers().await?;
        let attestors = client.yed_listattestors().await?;
        let claimable = client.yed_listclaimable().await?;
        let vaults = if stale {
            let mut all = Vec::new();
            let mut skip = 0;
            loop {
                let page = client.yed_listvaults("", VAULT_PAGE, skip).await?;
                let n = page.len() as u64;
                all.extend(page);
                if n < VAULT_PAGE {
                    break;
                }
                skip += n;
            }
            Some(all)
        } else {
            None
        };
        {
            let mut m = self.model.write().await;
            emitted.push(m.yellowback.on_price(&node, price));
            m.yellowback.activation = Some(activation);
            m.yellowback.miners = miners;
            m.yellowback.claimable = claimable;
            emitted.extend(m.yellowback.on_attestors(&node, stats.height, attestors));
            if let Some(v) = vaults {
                emitted.extend(m.yellowback.on_vaults(&node, v, &stats));
            }
        }
        st.yed_blocks += 1;
        self.bus.publish_all(emitted);
        let _ = best;
        Ok(())
    }

    /// The blocks from the last one the model knows (exclusive) to `best`, oldest first.
    /// `getblock … 2`: one call per block, the decoded transactions feed the classifier.
    async fn fetch_path(&self, client: &RpcClient, best: &str, first: bool) -> Result<Vec<BlockFull>, RpcError> {
        let limit = if first { BACKFILL } else { u64::MAX };
        let mut path = Vec::new();
        let mut hash = best.to_string();
        loop {
            let b = client.get_block_full(&hash).await?;
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
        let rejected_grew = prev.as_ref().map(|p| info.rejected_blocks > p.rejected_blocks).unwrap_or(false);
        {
            let mut m = self.model.write().await;
            emitted.extend(m.yellowback.on_info(&node, prev.as_ref(), &info));
            m.yed_info.insert(node.clone(), info.clone());
        }
        if rejected_grew {
            // Ask the verdict of every side tip this node holds that we have not asked about.
            let tips: Vec<String> = st.side_tips.iter().filter(|h| !st.verdicts.contains(*h)).cloned().collect();
            for hash in tips {
                match client.yed_getblockverdict(&hash).await {
                    Ok(v) => {
                        st.verdicts.insert(hash.clone());
                        if v.block_invalid {
                            let verdict = serde_json::to_value(&v).unwrap_or(Value::Null);
                            let mut m = self.model.write().await;
                            let height = m.chain.block(&hash).map(|b| b.height);
                            if let Some(b) = m.chain.block_mut(&hash) {
                                b.yb.get_or_insert_with(BlockYb::default).rejected = true;
                            }
                            m.yellowback.note_rejected(&hash, &node, verdict.clone());
                            emitted.push(Emitted { height, node: Some(node.clone()), kind: EventKind::RejectedBlock { hash, verdict } });
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
                // The value is flattened into the event envelope: its `height` would be written twice.
                let mut v = v;
                if let Some(o) = v.as_object_mut() {
                    for k in ["seq", "ts", "height", "node", "kind"] {
                        o.remove(k);
                    }
                }
                let kind = if name == "heartbeat" { EventKind::DevnetHeartbeat(v) } else { EventKind::DevnetSim(v) };
                self.bus.publish(height, None, kind);
            }
            // The fed prices: `mock-price` (the pools) and `attest-price-N` (each attestor).
            let mut attest = BTreeMap::new();
            if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
                while let Ok(Some(e)) = rd.next_entry().await {
                    let name = e.file_name().to_string_lossy().to_string();
                    if let Some(n) = name.strip_prefix("attest-price-") {
                        if let Some(p) = read_price(&e.path()).await {
                            attest.insert(n.to_string(), p);
                        }
                    }
                }
            }
            let mock = read_price(&dir.join("mock-price")).await;
            let changed = {
                let mut m = self.model.write().await;
                let changed = m.yellowback.mock_price != mock || m.yellowback.attest_prices != attest;
                m.yellowback.mock_price = mock;
                m.yellowback.attest_prices = attest.clone();
                changed
            };
            if changed {
                self.bus.publish(None, None, EventKind::Price(json!({"source": "devnet", "mockPrice": mock, "attestPrices": attest})));
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

async fn read_price(path: &std::path::Path) -> Option<f64> {
    tokio::fs::read_to_string(path).await.ok()?.trim().parse().ok()
}

/// The header view of a `getblock … 2` answer.
fn block_of(b: &BlockFull) -> Block {
    Block {
        hash: b.hash.clone(),
        height: b.height,
        confirmations: b.confirmations,
        size: b.size,
        version: b.extra.get("version").and_then(Value::as_i64).unwrap_or(0),
        time: b.time,
        chainwork: b.chainwork.clone(),
        tx: b.tx.iter().map(|t| t.txid.clone()).collect(),
        previousblockhash: b.previousblockhash.clone(),
        nextblockhash: b.extra.get("nextblockhash").and_then(Value::as_str).map(str::to_string),
        extra: b.extra.clone(),
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
    /// Blocks this node ran the per-block `yed_*` reads for.
    yed_blocks: u64,
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
