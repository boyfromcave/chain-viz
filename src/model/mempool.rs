//! The mempool across nodes: one entry per txid with the first-seen time per node and the set of
//! nodes that currently hold it (§3.2.2's cross-node mark), fed a fresh `getrawmempool true`
//! per node per poll.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::events::EventKind;
use crate::model::chain::Emitted;
use crate::rpc::MempoolEntry;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MempoolTx {
    pub txid: String,
    pub size: u64,
    pub fee: f64,
    /// The earliest node-reported `time`.
    pub time: u64,
    /// Node id → wall-clock time chain-viz first saw it there.
    #[serde(rename = "firstSeen")]
    pub first_seen: BTreeMap<String, f64>,
    /// Nodes whose last snapshot held it.
    pub present: BTreeSet<String>,
    #[serde(default)]
    pub depends: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MempoolSnapshot {
    pub txs: Vec<MempoolTx>,
    pub count: usize,
    pub bytes: u64,
    #[serde(rename = "feeTotal")]
    pub fee_total: f64,
    /// Number of nodes that have reported at least once.
    pub nodes: usize,
}

#[derive(Debug, Default)]
pub struct MempoolModel {
    txs: HashMap<String, MempoolTx>,
    reported: BTreeSet<String>,
}

impl MempoolModel {
    pub fn get(&self, txid: &str) -> Option<&MempoolTx> {
        self.txs.get(txid)
    }
    pub fn len(&self) -> usize {
        self.txs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    /// `is_mined(txid)` decides the `mempool_remove` reason once no node holds the tx.
    pub fn on_snapshot(&mut self, node: &str, entries: &BTreeMap<String, MempoolEntry>, now: f64, is_mined: impl Fn(&str) -> bool) -> Vec<Emitted> {
        let mut out = Vec::new();
        self.reported.insert(node.to_string());
        for (txid, e) in entries {
            match self.txs.get_mut(txid) {
                Some(tx) => {
                    tx.present.insert(node.to_string());
                    tx.first_seen.entry(node.to_string()).or_insert(now);
                    if e.time != 0 && (tx.time == 0 || e.time < tx.time) {
                        tx.time = e.time;
                    }
                }
                None => {
                    let mut tx = MempoolTx { txid: txid.clone(), size: e.size, fee: e.fee, time: e.time, first_seen: BTreeMap::new(), present: BTreeSet::new(), depends: e.depends.clone() };
                    tx.first_seen.insert(node.to_string(), now);
                    tx.present.insert(node.to_string());
                    self.txs.insert(txid.clone(), tx);
                    out.push(Emitted {
                        height: None,
                        node: Some(node.to_string()),
                        kind: EventKind::MempoolAdd { txid: txid.clone(), size: e.size, fee: e.fee, time: e.time, depends: e.depends.clone() },
                    });
                }
            }
        }
        let mut gone = Vec::new();
        for tx in self.txs.values_mut() {
            if !entries.contains_key(&tx.txid) {
                tx.present.remove(node);
                if tx.present.is_empty() {
                    gone.push(tx.txid.clone());
                }
            }
        }
        gone.sort();
        for txid in gone {
            self.txs.remove(&txid);
            let reason = if is_mined(&txid) { "mined" } else { "dropped" };
            out.push(Emitted { height: None, node: Some(node.to_string()), kind: EventKind::MempoolRemove { txid, reason: reason.to_string() } });
        }
        out
    }

    pub fn snapshot(&self) -> MempoolSnapshot {
        let mut txs: Vec<MempoolTx> = self.txs.values().cloned().collect();
        txs.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.txid.cmp(&b.txid)));
        MempoolSnapshot {
            count: txs.len(),
            bytes: txs.iter().map(|t| t.size).sum(),
            fee_total: txs.iter().map(|t| t.fee).sum(),
            nodes: self.reported.len(),
            txs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(size: u64, time: u64) -> MempoolEntry {
        MempoolEntry { size, fee: 0.00001, time, ..Default::default() }
    }

    #[test]
    fn add_cross_node_remove() {
        let mut m = MempoolModel::default();
        let mut a = BTreeMap::new();
        a.insert("t1".to_string(), entry(200, 100));
        let ev = m.on_snapshot("0", &a, 1.0, |_| false);
        assert!(matches!(ev[0].kind, EventKind::MempoolAdd { ref txid, .. } if txid == "t1"));
        assert!(m.on_snapshot("1", &a, 2.0, |_| false).is_empty(), "second node: no new event");
        assert_eq!(m.get("t1").unwrap().present.len(), 2);
        assert_eq!(m.get("t1").unwrap().first_seen["1"], 2.0);
        let empty = BTreeMap::new();
        assert!(m.on_snapshot("0", &empty, 3.0, |_| true).is_empty(), "still on node 1");
        let ev = m.on_snapshot("1", &empty, 4.0, |t| t == "t1");
        assert!(matches!(ev[0].kind, EventKind::MempoolRemove { ref reason, .. } if reason == "mined"));
        assert!(m.is_empty());
    }
}
