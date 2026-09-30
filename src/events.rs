//! Event schema v1: the contract between the collector, the UI, the recorder (`session.jsonl`)
//! and the tests. Owned by C1; later chunks add variants by APPENDING to `EventKind` and keep
//! the `#[serde(tag = "kind", rename_all = "snake_case")]` shape.
//!
//! Wire form of one event (one line of `session.jsonl`, one element of `/api/events`, one WS
//! frame): `{"seq":12,"ts":1727600000.123,"height":331,"node":"2","kind":"block", ...fields}`.
//! `height` and `node` are omitted when absent. Line 1 of a session file is a `session` header
//! (`session.rs`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;

/// One event as the UI sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: u64,
    /// Wall clock, seconds since the Unix epoch (fractional).
    pub ts: f64,
    /// Height the event is about (the block's, the tip's, the tx's inclusion height), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    /// Node id the event was observed on (`devnet.json` key or the `--nodes` index), when specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// A block as an event carries it (also the shape of `block_side`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BlockEvent {
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    pub time: u64,
    #[serde(rename = "txCount")]
    pub tx_count: u32,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chainwork: Option<String>,
    /// Ids of the nodes that reported this block by the time the event was emitted.
    #[serde(default)]
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// Line 1 of a session file.
    Session {
        version: u32,
        /// The chain-viz that wrote the file (`CARGO_PKG_VERSION`).
        #[serde(rename = "chainViz", default)]
        chain_viz: String,
        #[serde(default)]
        nodes: Vec<String>,
        #[serde(default)]
        chain: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started: Option<f64>,
    },
    /// A block first seen on any node's best chain. `height` = the block's height, `node` = first reporter.
    Block(BlockEvent),
    /// A block first seen only as a side branch (`getchaintips` `valid-fork`/`valid-headers`/…)
    /// on some node. `status` is the getchaintips status.
    BlockSide {
        #[serde(flatten)]
        block: BlockEvent,
        status: String,
        branchlen: u64,
    },
    /// A block that was on some node's best chain and has since been left for another branch
    /// (it is now a side tip, or gone from the tips altogether).
    Orphaned {
        hash: String,
    },
    /// A node's tip moved to a block that is not a child of its previous tip.
    Reorg {
        depth: u64,
        from: String,
        to: String,
        #[serde(rename = "toHeight")]
        to_height: u64,
    },
    /// A node's best tip changed (every change, including plain extension).
    Tip {
        hash: String,
    },
    MempoolAdd {
        txid: String,
        size: u64,
        fee: f64,
        /// The node's own `time` field (seconds since the epoch, when the node saw it).
        time: u64,
        #[serde(default)]
        depends: Vec<String>,
    },
    MempoolRemove {
        txid: String,
        /// `mined` when the txid is in a block the model knows, else `dropped`.
        reason: String,
    },
    /// A Yellowback transaction (C3 fills this in; field names follow `yed_gettxinfo`).
    YbTx {
        txid: String,
        #[serde(rename = "type")]
        tx_type: String,
        verdict: String,
        #[serde(rename = "feeZat", default)]
        fee_zat: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payee: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        info: Option<Value>,
    },
    /// A `yed_getinfo`/`yed_getstats` field changed on a node (`field` is the JSON name).
    YbState {
        field: String,
        from: Value,
        to: Value,
    },
    /// A price snapshot (C3).
    Price(Value),
    /// A `yed_getstats` snapshot (C3).
    Stats(Value),
    /// A block a node rejected under enforcement, with `yed_getblockverdict`'s answer.
    RejectedBlock {
        hash: String,
        verdict: Value,
    },
    /// Two healthy nodes on the same tip disagree on `yed_getstatehash` (C3).
    StatehashMismatch {
        hash: String,
        hashes: Value,
    },
    Attestor(Value),
    Vault(Value),
    /// The devnet's `heartbeat.json`.
    DevnetHeartbeat(Value),
    /// The devnet's `sim-stats.json`.
    DevnetSim(Value),
    /// A yolo `/status` sample (C4).
    YoloStatus(Value),
    /// Free text from the collector (a node went away, a source switched, …).
    Note {
        text: String,
    },
    /// One revenue-ledger row (C4, `model/revenue.rs`); the envelope's `height` is the row's.
    /// The ledger kind is `entry` (the envelope's `kind` is the event's).
    Revenue {
        txid: String,
        vout: u32,
        entry: crate::model::revenue::Kind,
        zat: i64,
        payee: String,
        /// At the row height's `pMint` when known at emission ("at pMint"); `/api/revenue` recomputes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usd: Option<f64>,
        #[serde(rename = "refHeight", default, skip_serializing_if = "Option::is_none")]
        ref_height: Option<u64>,
    },
}

impl Event {
    pub fn kind_name(&self) -> &'static str {
        match &self.kind {
            EventKind::Session { .. } => "session",
            EventKind::Block(_) => "block",
            EventKind::BlockSide { .. } => "block_side",
            EventKind::Orphaned { .. } => "orphaned",
            EventKind::Reorg { .. } => "reorg",
            EventKind::Tip { .. } => "tip",
            EventKind::MempoolAdd { .. } => "mempool_add",
            EventKind::MempoolRemove { .. } => "mempool_remove",
            EventKind::YbTx { .. } => "yb_tx",
            EventKind::YbState { .. } => "yb_state",
            EventKind::Price(_) => "price",
            EventKind::Stats(_) => "stats",
            EventKind::RejectedBlock { .. } => "rejected_block",
            EventKind::StatehashMismatch { .. } => "statehash_mismatch",
            EventKind::Attestor(_) => "attestor",
            EventKind::Vault(_) => "vault",
            EventKind::DevnetHeartbeat(_) => "devnet_heartbeat",
            EventKind::DevnetSim(_) => "devnet_sim",
            EventKind::YoloStatus(_) => "yolo_status",
            EventKind::Note { .. } => "note",
            EventKind::Revenue { .. } => "revenue",
        }
    }
}

/// Seconds since the epoch, fractional.
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub use crate::session::{read_session, Recorder};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape() {
        let e = Event { seq: 3, ts: 1.5, height: Some(7), node: Some("2".into()), kind: EventKind::Reorg { depth: 2, from: "a".into(), to: "b".into(), to_height: 9 } };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "reorg");
        assert_eq!(v["depth"], 2);
        assert_eq!(v["node"], "2");
        assert_eq!(v["toHeight"], 9);
        let back: Event = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
        let e = Event { seq: 0, ts: 0.0, height: None, node: None, kind: EventKind::Note { text: "x".into() } };
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("height"), "{}", s);
        assert!(!s.contains("node"), "{}", s);
    }
}
