//! The Yellowback health model (plan §3.2.4): per-node `yed_getstats` and `yed_getstatehash`,
//! one node's (the "leader", the lowest-id healthy Yellowback node) `yed_getprice`,
//! `yed_getactivation`, `yed_listminers`, `yed_listattestors`, `yed_listvaults`,
//! `yed_listclaimable`, the `yed_gethistory` timeline, and the Yellowback transactions the
//! classifier found (`classify.rs`) with their `yed_gettxinfo` / `yed_validaterawtransaction`
//! answers. Pure state + diffing: the collector feeds it RPC answers and publishes what it
//! returns. Field names follow `ycash-dd/doc/yellowback-rpc-contract.json`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::classify::Payload;
use crate::events::EventKind;
use crate::model::chain::Emitted;
use crate::rpc::{YedInfo, YedStateHash, YedStats};

/// `yed_gethistory` pages this many rows at most (`rpc/yellowback.cpp`, the contract's `args`).
pub const HISTORY_PAGE: u64 = 2016;
/// `yed_listvaults` page size.
pub const VAULT_PAGE: u64 = 100;
/// Rows of the timeline kept in `/api/snapshot` (the model keeps `keep`).
pub const SNAPSHOT_HISTORY: usize = 2016;
/// Yellowback transactions kept in `/api/snapshot` (newest first).
pub const SNAPSHOT_TXS: usize = 500;

/// The halt-mask bits as `yed_getstats.haltMask` names them (`src/yellowback/state.h`).
pub const HALT_BITS: [&str; 6] = ["NOT_ACTIVE", "NO_PRICE", "PARTICIPATION", "GLOBAL_RATIO", "DIVERGENCE", "ENFORCEMENT"];

/// One Yellowback transaction as the UI sees it, on a mempool entry, in a block's `yb.txs`
/// and in the `yb_tx` event. Field names are `yed_gettxinfo`'s; `wouldBeRejected` is
/// `yed_validaterawtransaction`'s (mempool only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct YbTx {
    pub txid: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub verdict: String,
    #[serde(rename = "feeZat", default)]
    pub fee_zat: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payee: Option<String>,
    #[serde(rename = "attestFeeZat", default)]
    pub attest_fee_zat: i64,
    #[serde(rename = "attestPayee", default, skip_serializing_if = "Option::is_none")]
    pub attest_payee: Option<String>,
    #[serde(default)]
    pub burned: i64,
    #[serde(rename = "yedIn", default)]
    pub yed_in: i64,
    #[serde(rename = "yedOut", default)]
    pub yed_out: i64,
    #[serde(rename = "residualZat", default)]
    pub residual_zat: i64,
    /// Mempool only: `yed_validaterawtransaction.wouldBeRejected`.
    #[serde(rename = "wouldBeRejected", default, skip_serializing_if = "Option::is_none")]
    pub would_be_rejected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    /// What the payload alone says (no RPC).
    pub payload: Payload,
    /// The node's whole answer (`yed_gettxinfo` once confirmed, else
    /// `yed_validaterawtransaction` + `yed_decodepayload` under `decoded`), when asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<Value>,
}

impl YbTx {
    /// Before any RPC: what the payload says.
    pub fn from_payload(txid: &str, payload: Payload) -> YbTx {
        YbTx {
            txid: txid.to_string(),
            tx_type: payload.tx_type.clone(),
            path: String::new(),
            verdict: String::new(),
            fee_zat: 0,
            payee: None,
            attest_fee_zat: 0,
            attest_payee: None,
            burned: 0,
            yed_in: 0,
            yed_out: 0,
            residual_zat: 0,
            would_be_rejected: None,
            height: None,
            payload,
            info: None,
        }
    }

    /// Fold a `yed_gettxinfo` (confirmed) or `yed_validaterawtransaction` (mempool) answer in.
    pub fn apply(&mut self, info: &Value, height: Option<u64>) {
        let s = |k: &str| info.get(k).and_then(Value::as_str).map(str::to_string);
        let i = |k: &str| info.get(k).and_then(Value::as_i64).unwrap_or(0);
        if let Some(t) = s("type") {
            if !t.is_empty() && t != "none" {
                self.tx_type = t;
            }
        }
        self.path = s("path").unwrap_or_default();
        self.verdict = s("verdict").unwrap_or_default();
        self.fee_zat = i("feeZat");
        self.payee = s("payee");
        self.attest_fee_zat = i("attestFeeZat");
        self.attest_payee = s("attestPayee");
        self.burned = i("burned");
        self.yed_in = i("yedIn");
        self.yed_out = i("yedOut");
        self.residual_zat = i("residualZat");
        self.would_be_rejected = info.get("wouldBeRejected").and_then(Value::as_bool);
        self.height = height.or_else(|| info.get("height").and_then(Value::as_u64).filter(|h| *h > 0));
        self.info = Some(info.clone());
    }

    pub fn event(&self) -> EventKind {
        EventKind::YbTx {
            txid: self.txid.clone(),
            tx_type: self.tx_type.clone(),
            verdict: self.verdict.clone(),
            fee_zat: self.fee_zat,
            payee: self.payee.clone(),
            info: Some(serde_json::to_value(self).unwrap_or(Value::Null)),
        }
    }
}

/// The Yellowback view of one block (`BlockInfo.yb`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BlockYb {
    /// `yed_gettag`: `{found, kind, version, signal, priceMicroUsd, sourceMask, payoutAddress}`
    /// (the plan's `payoutKey` is `payoutAddress` on the wire; the UI reads that name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<Value>,
    /// The tag's `payoutAddress`: the pool whose coinbase carried it (the block's miner when
    /// the miner quotes; a block without a tag has no attributable miner here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miner: Option<String>,
    /// Some node rejected this block under enforcement (`rejected_block`).
    #[serde(default)]
    pub rejected: bool,
    /// The Yellowback transactions in it, in block order.
    #[serde(default)]
    pub txs: Vec<YbTx>,
    /// `yed_gettag` was asked (a stock node can classify but not tag).
    #[serde(default)]
    pub tagged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectedBlock {
    pub hash: String,
    pub node: String,
    pub verdict: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct YellowbackSnapshot {
    /// The node whose chain-wide answers (`price`, `vaults`, …) are shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// `yed_getstats` per node.
    pub stats: BTreeMap<String, YedStats>,
    /// `yed_getstatehash` per node.
    pub statehash: BTreeMap<String, YedStateHash>,
    /// `true` when every healthy node on the majority hash agrees; `false` on a mismatch;
    /// `null` when fewer than two nodes answered.
    pub statehash_agree: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation: Option<Value>,
    pub miners: Vec<Value>,
    pub attestors: Vec<Value>,
    pub vaults: Vec<Value>,
    pub claimable: Vec<Value>,
    /// `yed_gethistory` rows by height, oldest first, the last `SNAPSHOT_HISTORY`.
    pub history: Vec<Value>,
    #[serde(rename = "historyFrom")]
    pub history_from: Option<u64>,
    /// Newest first, at most `SNAPSHOT_TXS`.
    pub txs: Vec<YbTx>,
    pub rejected: Vec<RejectedBlock>,
    /// `--devnet`: `mock-price` (the pools' fed price, USD) and `attest-price-N` per attestor.
    #[serde(rename = "mockPrice", skip_serializing_if = "Option::is_none")]
    pub mock_price: Option<f64>,
    #[serde(rename = "attestPrices")]
    pub attest_prices: BTreeMap<String, f64>,
    #[serde(rename = "haltBits")]
    pub halt_bits: Vec<&'static str>,
}

#[derive(Debug)]
pub struct YellowbackModel {
    pub stats: BTreeMap<String, YedStats>,
    pub statehash: BTreeMap<String, YedStateHash>,
    /// Block hashes a mismatch was already raised for.
    alarmed: BTreeSet<String>,
    pub price: Option<Value>,
    pub activation: Option<Value>,
    pub miners: Vec<Value>,
    pub attestors: Vec<Value>,
    pub vaults: Vec<Value>,
    pub claimable: Vec<Value>,
    /// The vault counts the current `vaults` list was fetched at.
    vault_counts: Option<[u64; 4]>,
    pub history: BTreeMap<u64, Value>,
    /// The height the backfill reached (exclusive of what live rows added), `None` until done.
    pub backfilled_to: Option<u64>,
    pub txs: HashMap<String, YbTx>,
    /// Newest-first order of `txs` for the snapshot.
    order: Vec<String>,
    /// Txids whose outputs were looked at (ordinary or Yellowback), so no tx is fetched twice.
    checked: BTreeSet<String>,
    pub rejected: Vec<RejectedBlock>,
    pub mock_price: Option<f64>,
    pub attest_prices: BTreeMap<String, f64>,
    keep: u64,
}

impl Default for YellowbackModel {
    fn default() -> Self {
        YellowbackModel::new(5000)
    }
}

impl YellowbackModel {
    pub fn new(keep: u64) -> YellowbackModel {
        YellowbackModel {
            stats: BTreeMap::new(),
            statehash: BTreeMap::new(),
            alarmed: BTreeSet::new(),
            price: None,
            activation: None,
            miners: Vec::new(),
            attestors: Vec::new(),
            vaults: Vec::new(),
            claimable: Vec::new(),
            vault_counts: None,
            history: BTreeMap::new(),
            backfilled_to: None,
            txs: HashMap::new(),
            order: Vec::new(),
            checked: BTreeSet::new(),
            rejected: Vec::new(),
            mock_price: None,
            attest_prices: BTreeMap::new(),
            keep,
        }
    }

    /// The extra `yb_state` fields C1's collector does not diff: activation and arming status,
    /// `attest.armed`.
    pub fn on_info(&self, node: &str, prev: Option<&YedInfo>, info: &YedInfo) -> Vec<Emitted> {
        let Some(prev) = prev else { return Vec::new() };
        let mut out = Vec::new();
        let pairs = [
            ("activation.status", prev.activation.get("status"), info.activation.get("status")),
            ("attest.status", prev.attest.get("status"), info.attest.get("status")),
            ("attest.armed", prev.attest.get("armed"), info.attest.get("armed")),
            ("activation.signalCount", prev.activation.get("signalCount"), info.activation.get("signalCount")),
        ];
        for (field, from, to) in pairs {
            if from != to && !field.ends_with("signalCount") {
                out.push(Emitted {
                    height: Some(info.height),
                    node: Some(node.to_string()),
                    kind: EventKind::YbState { field: field.into(), from: from.cloned().unwrap_or(Value::Null), to: to.cloned().unwrap_or(Value::Null) },
                });
            }
        }
        out
    }

    /// A fresh `yed_getstats` from `node`. Halt-mask bits and `mintingAllowed` that changed are
    /// `yb_state` events (per node); the leader's stats are also the `stats` event and a live
    /// timeline row.
    pub fn on_stats(&mut self, node: &str, stats: YedStats, leader: bool) -> Vec<Emitted> {
        let mut out = Vec::new();
        let height = Some(stats.height);
        if let Some(prev) = self.stats.get(node) {
            let before: BTreeSet<&str> = prev.halt_mask.iter().map(String::as_str).collect();
            let after: BTreeSet<&str> = stats.halt_mask.iter().map(String::as_str).collect();
            if before != after {
                out.push(Emitted {
                    height,
                    node: Some(node.to_string()),
                    kind: EventKind::YbState { field: "haltMask".into(), from: json!(prev.halt_mask), to: json!(stats.halt_mask) },
                });
            }
            if prev.minting_allowed != stats.minting_allowed {
                out.push(Emitted {
                    height,
                    node: Some(node.to_string()),
                    kind: EventKind::YbState { field: "mintingAllowed".into(), from: prev.minting_allowed.into(), to: stats.minting_allowed.into() },
                });
            }
        }
        if leader {
            let mut v = serde_json::to_value(&stats).unwrap_or(Value::Null);
            let prev = self.history.get(&stats.height.saturating_sub(1)).cloned();
            if let (Some(o), Some(p)) = (v.as_object_mut(), prev.as_ref().and_then(Value::as_object)) {
                if let Some(x) = p.get("supplyCents").and_then(Value::as_i64) {
                    o.insert("supplyDeltaCents".into(), (stats.supply_cents - x).into());
                }
                if let Some(x) = p.get("collateralZat").and_then(Value::as_i64) {
                    o.insert("collateralDeltaZat".into(), (stats.collateral_zat - x).into());
                }
            }
            self.history.insert(stats.height, history_row_from_stats(&stats));
            out.push(Emitted { height, node: Some(node.to_string()), kind: EventKind::Stats(v) });
        }
        self.stats.insert(node.to_string(), stats);
        self.evict();
        out
    }

    /// Do the vault counts differ from those the vault list was last fetched at?
    pub fn vaults_stale(&self, stats: &YedStats) -> bool {
        self.vault_counts != Some(counts(stats))
    }

    pub fn on_price(&mut self, node: &str, mut price: Value) -> Emitted {
        if let Some(o) = price.as_object_mut() {
            if let Some(m) = self.mock_price {
                o.insert("mockPrice".into(), json!(m));
            }
            if !self.attest_prices.is_empty() {
                o.insert("attestPrices".into(), json!(self.attest_prices));
            }
        }
        let height = price.get("height").and_then(Value::as_u64);
        self.price = Some(price.clone());
        Emitted { height, node: Some(node.to_string()), kind: EventKind::Price(price) }
    }

    /// `yed_getstatehash` from `node`; `healthy` says which nodes' hashes count. Two healthy
    /// nodes on the same block hash with different state hashes raise `statehash_mismatch` once
    /// per block hash.
    pub fn on_statehash(&mut self, node: &str, h: YedStateHash, healthy: &BTreeSet<String>) -> Option<Emitted> {
        self.statehash.insert(node.to_string(), h.clone());
        let same: BTreeMap<&String, &String> = self.statehash.iter().filter(|(n, s)| s.blockhash == h.blockhash && healthy.contains(*n)).map(|(n, s)| (n, &s.statehash)).collect();
        let distinct: BTreeSet<&String> = same.values().copied().collect();
        if distinct.len() > 1 && self.alarmed.insert(h.blockhash.clone()) {
            return Some(Emitted {
                height: Some(h.height),
                node: None,
                kind: EventKind::StatehashMismatch { hash: h.blockhash.clone(), hashes: json!(same) },
            });
        }
        None
    }

    /// `null` until two healthy nodes are on the same hash.
    pub fn statehash_agree(&self, healthy: &BTreeSet<String>) -> Option<bool> {
        let mut by_block: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (n, s) in &self.statehash {
            if healthy.contains(n) {
                by_block.entry(&s.blockhash).or_default().insert(&s.statehash);
            }
        }
        let counted: Vec<&BTreeSet<&str>> = by_block.values().collect();
        if self.statehash.iter().filter(|(n, _)| healthy.contains(*n)).count() < 2 {
            return None;
        }
        Some(counted.iter().all(|s| s.len() <= 1))
    }

    /// A fresh `yed_listvaults` (every status, all pages) at the given counts. Status changes
    /// per `txid:vout` are `vault` events.
    pub fn on_vaults(&mut self, node: &str, vaults: Vec<Value>, at: &YedStats) -> Vec<Emitted> {
        let key = |v: &Value| format!("{}:{}", v.get("txid").and_then(Value::as_str).unwrap_or(""), v.get("vout").and_then(Value::as_u64).unwrap_or(0));
        let before: HashMap<String, String> = self.vaults.iter().map(|v| (key(v), status_of(v))).collect();
        let mut out = Vec::new();
        for v in &vaults {
            let k = key(v);
            let now = status_of(v);
            let was = before.get(&k).cloned();
            if was.as_deref() != Some(now.as_str()) {
                let mut ev = v.clone();
                if let Some(o) = ev.as_object_mut() {
                    o.insert("vault".into(), k.into());
                    o.insert("from".into(), was.map(Value::from).unwrap_or(Value::Null));
                    o.insert("to".into(), now.into());
                }
                out.push(Emitted { height: Some(at.height), node: Some(node.to_string()), kind: EventKind::Vault(ev) });
            }
        }
        self.vaults = vaults;
        self.vault_counts = Some(counts(at));
        out
    }

    /// A fresh `yed_listattestors`; status changes per `seq` are `attestor` events.
    pub fn on_attestors(&mut self, node: &str, height: u64, list: Vec<Value>) -> Vec<Emitted> {
        let seq = |v: &Value| v.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let before: HashMap<u64, String> = self.attestors.iter().map(|v| (seq(v), status_of(v))).collect();
        let mut out = Vec::new();
        for a in &list {
            let now = status_of(a);
            let was = before.get(&seq(a)).cloned();
            if was.as_deref() != Some(now.as_str()) {
                let mut ev = a.clone();
                if let Some(o) = ev.as_object_mut() {
                    o.insert("from".into(), was.map(Value::from).unwrap_or(Value::Null));
                    o.insert("to".into(), now.into());
                }
                out.push(Emitted { height: Some(height), node: Some(node.to_string()), kind: EventKind::Attestor(ev) });
            }
        }
        self.attestors = list;
        out
    }

    pub fn on_history(&mut self, rows: Vec<Value>) {
        for r in rows {
            if let Some(h) = r.get("height").and_then(Value::as_u64) {
                self.history.insert(h, r);
            }
        }
        self.evict();
    }

    /// Where the backfill should start: the kept window below `tip`, but not before `start`.
    pub fn backfill_from(&self, tip: u64, start: u64) -> u64 {
        tip.saturating_sub(self.keep).max(start).max(1)
    }

    /// Remember a classified transaction (mempool or block). Returns whether it was new.
    pub fn upsert_tx(&mut self, tx: YbTx) -> bool {
        let new = !self.txs.contains_key(&tx.txid);
        if new {
            self.order.insert(0, tx.txid.clone());
            if self.order.len() > SNAPSHOT_TXS * 4 {
                if let Some(old) = self.order.pop() {
                    self.txs.remove(&old);
                }
            }
        }
        self.txs.insert(tx.txid.clone(), tx);
        new
    }

    /// `true` the first time a txid is offered: the caller then fetches and classifies it.
    pub fn check(&mut self, txid: &str) -> bool {
        if self.checked.len() > 50_000 {
            self.checked.clear();
        }
        self.checked.insert(txid.to_string())
    }

    pub fn tx(&self, txid: &str) -> Option<&YbTx> {
        self.txs.get(txid)
    }

    pub fn note_rejected(&mut self, hash: &str, node: &str, verdict: Value) {
        if !self.rejected.iter().any(|r| r.hash == hash && r.node == node) {
            self.rejected.push(RejectedBlock { hash: hash.to_string(), node: node.to_string(), verdict });
        }
    }

    fn evict(&mut self) {
        let Some(top) = self.history.keys().next_back().copied() else { return };
        if top > self.keep {
            let floor = top - self.keep;
            self.history = self.history.split_off(&floor);
        }
    }

    pub fn snapshot(&self, leader: Option<String>, healthy: &BTreeSet<String>) -> YellowbackSnapshot {
        let history: Vec<Value> = self.history.values().rev().take(SNAPSHOT_HISTORY).cloned().collect::<Vec<_>>().into_iter().rev().collect();
        YellowbackSnapshot {
            leader,
            stats: self.stats.clone(),
            statehash: self.statehash.clone(),
            statehash_agree: self.statehash_agree(healthy),
            price: self.price.clone(),
            activation: self.activation.clone(),
            miners: self.miners.clone(),
            attestors: self.attestors.clone(),
            vaults: self.vaults.clone(),
            claimable: self.claimable.clone(),
            history_from: history.first().and_then(|r| r.get("height").and_then(Value::as_u64)),
            history,
            txs: self.order.iter().take(SNAPSHOT_TXS).filter_map(|t| self.txs.get(t).cloned()).collect(),
            rejected: self.rejected.clone(),
            mock_price: self.mock_price,
            attest_prices: self.attest_prices.clone(),
            halt_bits: HALT_BITS.to_vec(),
        }
    }
}

fn counts(s: &YedStats) -> [u64; 4] {
    [s.active_vaults, s.void_vaults, s.closed_vaults, s.claimed_vaults]
}

fn status_of(v: &Value) -> String {
    v.get("status").and_then(Value::as_str).unwrap_or("").to_string()
}

/// A timeline row in `yed_gethistory`'s shape, from a live `yed_getstats` (the fields both
/// carry; `signalCount`/`tagged`/`quote`/`activation` are absent from a live row).
pub fn history_row_from_stats(s: &YedStats) -> Value {
    let mut o = Map::new();
    o.insert("height".into(), s.height.into());
    o.insert("live".into(), true.into());
    for (k, v) in [("pFast", s.p_fast), ("pMid", s.p_mid), ("pSlow", s.p_slow), ("pMint", s.p_mint), ("pClaim", s.p_claim), ("supplyCents", s.supply_cents), ("collateralZat", s.collateral_zat), ("issuedZat", s.issued_zat)] {
        o.insert(k.into(), v.into());
    }
    o.insert("globalRatioBps".into(), if s.global_ratio_bps == 0 && s.supply_cents == 0 { Value::Null } else { s.global_ratio_bps.into() });
    o.insert("haltMask".into(), json!(s.halt_mask));
    if let Some(m) = s.extra.get("sigmaMultBps") {
        o.insert("sigmaMultBps".into(), m.clone());
    }
    Value::Object(o)
}

/// A vault's collateral ratio in bps at `p_claim` (`UnderwaterAt`, `rpc/yellowback.cpp:218`:
/// underwater when `collateralZat * pClaim < mintedCents * claimThresholdBps * COIN`).
pub fn vault_ratio_bps(collateral_zat: i64, minted_cents: i64, p_claim: i64) -> Option<i64> {
    if collateral_zat <= 0 || minted_cents <= 0 || p_claim <= 0 {
        return None;
    }
    Some(((collateral_zat as i128 * p_claim as i128) / (minted_cents as i128 * 100_000_000)) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(height: u64, halt: &[&str], active: u64) -> YedStats {
        serde_json::from_value(json!({"height": height, "haltMask": halt, "mintingAllowed": halt.is_empty(), "activeVaults": active, "pClaim": 50000000, "supplyCents": 100})).unwrap()
    }

    #[test]
    fn halt_mask_changes_are_state_events() {
        let mut m = YellowbackModel::default();
        assert_eq!(m.on_stats("0", stats(10, &[], 0), true).len(), 1, "first: stats event only");
        let ev = m.on_stats("0", stats(11, &["DIVERGENCE"], 0), true);
        let fields: Vec<&str> = ev.iter().filter_map(|e| if let EventKind::YbState { field, .. } = &e.kind { Some(field.as_str()) } else { None }).collect();
        assert_eq!(fields, ["haltMask", "mintingAllowed"]);
        assert!(matches!(ev.last().unwrap().kind, EventKind::Stats(_)));
        assert_eq!(m.history.len(), 2);
        assert!(m.vaults_stale(&stats(11, &[], 1)));
        assert!(m.on_stats("1", stats(11, &["DIVERGENCE"], 0), false).is_empty(), "follower, first sample");
    }

    #[test]
    fn statehash_mismatch_once_per_block() {
        let mut m = YellowbackModel::default();
        let healthy: BTreeSet<String> = ["0", "2", "3"].iter().map(|s| s.to_string()).collect();
        let h = |n: &str, s: &str| YedStateHash { height: 5, blockhash: "b".into(), statehash: s.into() };
        assert!(m.on_statehash("0", h("0", "aa"), &healthy).is_none());
        assert_eq!(m.statehash_agree(&healthy), None);
        assert!(m.on_statehash("2", h("2", "aa"), &healthy).is_none());
        assert_eq!(m.statehash_agree(&healthy), Some(true));
        // an unhealthy node's hash never counts
        assert!(m.on_statehash("1", h("1", "zz"), &healthy).is_none());
        let ev = m.on_statehash("3", h("3", "bb"), &healthy).expect("mismatch");
        assert!(matches!(ev.kind, EventKind::StatehashMismatch { ref hash, .. } if hash == "b"));
        assert_eq!(m.statehash_agree(&healthy), Some(false));
        assert!(m.on_statehash("3", h("3", "bb"), &healthy).is_none(), "raised once");
    }

    #[test]
    fn vault_and_attestor_diffs() {
        let mut m = YellowbackModel::default();
        let s = stats(20, &[], 1);
        let v = |st: &str| json!({"txid": "t", "vout": 0, "status": st});
        assert_eq!(m.on_vaults("0", vec![v("ACTIVE")], &s).len(), 1);
        assert!(!m.vaults_stale(&s));
        assert!(m.on_vaults("0", vec![v("ACTIVE")], &s).is_empty());
        let ev = m.on_vaults("0", vec![v("CLAIMED")], &s);
        assert!(matches!(&ev[0].kind, EventKind::Vault(x) if x["from"] == "ACTIVE" && x["to"] == "CLAIMED" && x["vault"] == "t:0"));
        let a = |st: &str| json!({"seq": 1, "status": st});
        assert_eq!(m.on_attestors("0", 20, vec![a("ELIGIBLE")]).len(), 1);
        assert!(m.on_attestors("0", 21, vec![a("ELIGIBLE")]).is_empty());
        assert_eq!(m.on_attestors("0", 22, vec![a("EJECTED")]).len(), 1);
    }

    #[test]
    fn ratio_matches_underwater_at() {
        // contract sample: 251.25628141 YEC at pClaim 2 000 000 against 100 000 cents;
        // UnderwaterAt = ceil(100000 * 11000 * 1e8 / 25125628141) = 437 800.
        let bps = vault_ratio_bps(25_125_628_141, 100_000, 2_000_000).unwrap();
        assert_eq!(bps, 5025);
        assert_eq!(vault_ratio_bps(25_125_628_141, 100_000, 437_800).unwrap(), 1100, "UnderwaterAt rounds up: at that price the vault is exactly at the threshold");
        assert_eq!(vault_ratio_bps(0, 1, 1), None);
    }

    #[test]
    fn tx_apply_and_order() {
        let mut m = YellowbackModel::default();
        let p = crate::classify::decode(&[0x59, 0x42, 0x03, 0x07], 0).unwrap();
        let mut t = YbTx::from_payload("x", p.clone());
        t.apply(&json!({"type": "equivocation", "verdict": "ok", "feeZat": 5, "payee": "sm1", "wouldBeRejected": false, "height": 0}), None);
        assert_eq!((t.fee_zat, t.would_be_rejected, t.height), (5, Some(false), None));
        assert!(m.upsert_tx(t.clone()));
        assert!(!m.upsert_tx(t));
        assert!(m.upsert_tx(YbTx::from_payload("y", p)));
        let snap = m.snapshot(None, &BTreeSet::new());
        assert_eq!(snap.txs.iter().map(|t| t.txid.as_str()).collect::<Vec<_>>(), ["y", "x"]);
    }
}
