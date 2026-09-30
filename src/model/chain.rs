//! The block DAG across nodes: per-node heads, per-node `getchaintips`, and the fork / orphan /
//! reorg detection of plan §3.3, derived purely from what each node reports. No consensus
//! opinion: where nodes disagree the model shows the disagreement; the "main chain" is only the
//! majority head (ties → most work), used for layout.
//!
//! The collector feeds it two things per node: `on_new_head` with the path of blocks it fetched
//! from the last known ancestor to the node's new best block, and `on_chain_tips` with a fresh
//! `getchaintips`. Both return the events to publish; the tests drive them from recorded
//! fixtures (`tests/fixtures/chain-*.json`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::events::{BlockEvent, EventKind};
use crate::model::yellowback::BlockYb;
use crate::rpc::{Block, ChainTip};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    /// On some node's best chain (now or once).
    Main,
    /// Known only from `getchaintips` as a side tip.
    Side,
    /// Was on a best chain, no node's best chain contains it any more.
    Orphaned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockInfo {
    pub hash: String,
    pub height: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    pub time: u64,
    #[serde(rename = "txCount")]
    pub tx_count: u32,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chainwork: Option<String>,
    pub status: BlockStatus,
    /// Nodes that have had this block on their best chain.
    pub nodes: BTreeSet<String>,
    /// First wall-clock time chain-viz saw it.
    pub seen: f64,
    /// Present only for blocks the collector fetched in full (not side tips).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub txids: Vec<String>,
    /// The Yellowback view (C3): tag, miner, rejected, its Yellowback transactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yb: Option<BlockYb>,
}

impl BlockInfo {
    fn from_block(b: &Block, seen: f64) -> BlockInfo {
        BlockInfo {
            hash: b.hash.clone(),
            height: b.height,
            prev: b.previousblockhash.clone(),
            time: b.time,
            tx_count: b.tx.len() as u32,
            size: b.size,
            chainwork: if b.chainwork.is_empty() { None } else { Some(b.chainwork.clone()) },
            status: BlockStatus::Main,
            nodes: BTreeSet::new(),
            seen,
            txids: b.tx.clone(),
            yb: None,
        }
    }
    pub fn event(&self) -> BlockEvent {
        BlockEvent {
            hash: self.hash.clone(),
            prev: self.prev.clone(),
            time: self.time,
            tx_count: self.tx_count,
            size: self.size,
            chainwork: self.chainwork.clone(),
            nodes: self.nodes.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Head {
    pub height: u64,
    pub hash: String,
}

/// One event with the height and node it is about (the bus adds seq and ts).
#[derive(Debug, Clone, PartialEq)]
pub struct Emitted {
    pub height: Option<u64>,
    pub node: Option<String>,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct MajorityHead {
    pub height: u64,
    pub hash: String,
    /// Nodes on this head.
    pub nodes: Vec<String>,
    /// Nodes on another head (the fork-risk indicator).
    pub disagreeing: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainSnapshot {
    pub heads: BTreeMap<String, Head>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub majority: Option<MajorityHead>,
    /// The majority head's chain, newest last, at most `limit` blocks.
    pub main: Vec<BlockInfo>,
    /// Every block not on the majority chain within the kept window, by height.
    pub side: Vec<BlockInfo>,
    pub tips: BTreeMap<String, Vec<ChainTip>>,
    #[serde(rename = "blockCount")]
    pub block_count: usize,
}

#[derive(Debug)]
pub struct ChainModel {
    blocks: HashMap<String, BlockInfo>,
    heads: BTreeMap<String, Head>,
    tips: BTreeMap<String, Vec<ChainTip>>,
    /// Txids of every kept block (for the mempool's `mined` reason).
    mined: HashSet<String>,
    keep: u64,
}

impl Default for ChainModel {
    fn default() -> Self {
        ChainModel::new(5000)
    }
}

impl ChainModel {
    pub fn new(keep: u64) -> ChainModel {
        ChainModel { blocks: HashMap::new(), heads: BTreeMap::new(), tips: BTreeMap::new(), mined: HashSet::new(), keep }
    }

    pub fn knows(&self, hash: &str) -> bool {
        self.blocks.contains_key(hash)
    }
    pub fn block(&self, hash: &str) -> Option<&BlockInfo> {
        self.blocks.get(hash)
    }
    pub fn block_mut(&mut self, hash: &str) -> Option<&mut BlockInfo> {
        self.blocks.get_mut(hash)
    }
    /// Fully fetched blocks whose Yellowback view is still unenriched (`yb.tagged == false`),
    /// newest first, at most `limit` (C3's catch-up when a stock node walked the backfill).
    pub fn untagged(&self, limit: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self.blocks.values().filter(|b| b.yb.as_ref().map(|y| !y.tagged).unwrap_or(false)).map(|b| (b.hash.clone(), b.height)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v.truncate(limit);
        v
    }
    pub fn head(&self, node: &str) -> Option<&Head> {
        self.heads.get(node)
    }
    pub fn heads(&self) -> &BTreeMap<String, Head> {
        &self.heads
    }
    pub fn is_mined(&self, txid: &str) -> bool {
        self.mined.contains(txid)
    }
    pub fn len(&self) -> usize {
        self.blocks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// `node`'s best block is now `path.last()`; `path` is oldest-first and `path[0].prev` is
    /// either known to the model or the first block the collector could reach. Returns the
    /// `block`, `tip` and `reorg` events, in that order.
    pub fn on_new_head(&mut self, node: &str, path: &[Block], seen: f64) -> Vec<Emitted> {
        let mut out = Vec::new();
        let Some(new_head) = path.last() else { return out };
        for b in path {
            let (fresh, promoted) = match self.blocks.get_mut(&b.hash) {
                Some(info) => {
                    let promoted = info.status != BlockStatus::Main;
                    info.status = BlockStatus::Main;
                    if info.txids.is_empty() && !b.tx.is_empty() {
                        info.txids = b.tx.clone();
                    }
                    if info.prev.is_none() {
                        info.prev = b.previousblockhash.clone();
                    }
                    info.nodes.insert(node.to_string());
                    (false, promoted)
                }
                None => {
                    let mut info = BlockInfo::from_block(b, seen);
                    info.nodes.insert(node.to_string());
                    self.blocks.insert(b.hash.clone(), info);
                    (true, false)
                }
            };
            for t in &b.tx {
                self.mined.insert(t.clone());
            }
            if fresh || promoted {
                let info = &self.blocks[&b.hash];
                out.push(Emitted { height: Some(b.height), node: Some(node.to_string()), kind: EventKind::Block(info.event()) });
            }
        }
        let old = self.heads.insert(node.to_string(), Head { height: new_head.height, hash: new_head.hash.clone() });
        out.push(Emitted { height: Some(new_head.height), node: Some(node.to_string()), kind: EventKind::Tip { hash: new_head.hash.clone() } });
        if let Some(old) = old {
            if old.hash != new_head.hash && !self.is_ancestor(&old.hash, &new_head.hash) {
                let depth = self.reorg_depth(&old.hash, &new_head.hash);
                out.push(Emitted {
                    height: Some(new_head.height),
                    node: Some(node.to_string()),
                    kind: EventKind::Reorg { depth, from: old.hash.clone(), to: new_head.hash.clone(), to_height: new_head.height },
                });
                out.extend(self.mark_orphans());
            }
        }
        self.evict();
        out
    }

    /// A fresh `getchaintips` from `node`: unknown non-active tips become `block_side` events;
    /// blocks that were main and no longer sit under any head become `orphaned`.
    pub fn on_chain_tips(&mut self, node: &str, tips: Vec<ChainTip>, seen: f64) -> Vec<Emitted> {
        let mut out = Vec::new();
        for t in &tips {
            if t.status == "active" || self.blocks.contains_key(&t.hash) {
                continue;
            }
            let info = BlockInfo {
                hash: t.hash.clone(),
                height: t.height,
                prev: None,
                time: 0,
                tx_count: 0,
                size: 0,
                chainwork: None,
                status: BlockStatus::Side,
                nodes: BTreeSet::new(),
                seen,
                txids: Vec::new(),
                yb: None,
            };
            let ev = info.event();
            self.blocks.insert(t.hash.clone(), info);
            out.push(Emitted { height: Some(t.height), node: Some(node.to_string()), kind: EventKind::BlockSide { block: ev, status: t.status.clone(), branchlen: t.branchlen } });
        }
        self.tips.insert(node.to_string(), tips);
        out.extend(self.mark_orphans());
        out
    }

    /// Is `a` an ancestor of (or equal to) `b`, following `prev` pointers the model knows?
    fn is_ancestor(&self, a: &str, b: &str) -> bool {
        let Some(target) = self.blocks.get(a) else { return false };
        let mut cur = b.to_string();
        loop {
            if cur == a {
                return true;
            }
            match self.blocks.get(&cur) {
                Some(info) if info.height > target.height => match &info.prev {
                    Some(p) => cur = p.clone(),
                    None => return false,
                },
                _ => return false,
            }
        }
    }

    /// Blocks from `from` back to (excluding) the common ancestor with `to`. If the model has no
    /// common ancestor in its window, the whole known `from` line counts.
    fn reorg_depth(&self, from: &str, to: &str) -> u64 {
        let mut to_line = HashSet::new();
        let mut cur = Some(to.to_string());
        while let Some(h) = cur {
            to_line.insert(h.clone());
            cur = self.blocks.get(&h).and_then(|b| b.prev.clone());
        }
        let mut depth = 0;
        let mut cur = Some(from.to_string());
        while let Some(h) = cur {
            if to_line.contains(&h) {
                break;
            }
            depth += 1;
            cur = self.blocks.get(&h).and_then(|b| b.prev.clone());
        }
        depth
    }

    /// Every block under any node's head (the union of best chains within the window).
    fn live_set(&self) -> HashSet<String> {
        let mut live = HashSet::new();
        for head in self.heads.values() {
            let mut cur = Some(head.hash.clone());
            while let Some(h) = cur {
                if !live.insert(h.clone()) {
                    break;
                }
                cur = self.blocks.get(&h).and_then(|b| b.prev.clone());
            }
        }
        live
    }

    fn mark_orphans(&mut self) -> Vec<Emitted> {
        let live = self.live_set();
        let mut out = Vec::new();
        let mut hashes: Vec<(u64, String)> = self.blocks.values().filter(|b| b.status == BlockStatus::Main && !live.contains(&b.hash)).map(|b| (b.height, b.hash.clone())).collect();
        hashes.sort();
        for (height, hash) in hashes {
            if let Some(b) = self.blocks.get_mut(&hash) {
                b.status = BlockStatus::Orphaned;
            }
            out.push(Emitted { height: Some(height), node: None, kind: EventKind::Orphaned { hash } });
        }
        out
    }

    fn evict(&mut self) {
        let Some(top) = self.heads.values().map(|h| h.height).max() else { return };
        if top <= self.keep {
            return;
        }
        let floor = top - self.keep;
        let gone: Vec<String> = self.blocks.values().filter(|b| b.height < floor).map(|b| b.hash.clone()).collect();
        for h in gone {
            if let Some(b) = self.blocks.remove(&h) {
                for t in b.txids {
                    self.mined.remove(&t);
                }
            }
        }
    }

    /// The head most nodes are on; ties → the most work (chainwork, then height).
    pub fn majority(&self) -> Option<MajorityHead> {
        let mut by_hash: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (node, head) in &self.heads {
            by_hash.entry(&head.hash).or_default().push(node.clone());
        }
        let work = |h: &str| -> (usize, u64) {
            let b = self.blocks.get(h);
            (b.and_then(|b| b.chainwork.as_ref()).map(|w| w.trim_start_matches('0').len()).unwrap_or(0), b.map(|b| b.height).unwrap_or(0))
        };
        let (hash, nodes) = by_hash.into_iter().max_by(|(ha, na), (hb, nb)| {
            na.len().cmp(&nb.len()).then_with(|| {
                let (wa, wb) = (work(ha), work(hb));
                wa.cmp(&wb).then_with(|| self.blocks.get(*ha).and_then(|b| b.chainwork.clone()).cmp(&self.blocks.get(*hb).and_then(|b| b.chainwork.clone())))
            })
        })?;
        let height = self.heads.values().find(|h| h.hash == hash).map(|h| h.height)?;
        let disagreeing = self.heads.iter().filter(|(_, h)| h.hash != hash).map(|(n, _)| n.clone()).collect();
        Some(MajorityHead { height, hash: hash.to_string(), nodes, disagreeing })
    }

    pub fn snapshot(&self, limit: usize) -> ChainSnapshot {
        let majority = self.majority();
        let mut main = Vec::new();
        let mut on_main = HashSet::new();
        if let Some(m) = &majority {
            let mut cur = Some(m.hash.clone());
            while let Some(h) = cur {
                if main.len() >= limit {
                    break;
                }
                let Some(b) = self.blocks.get(&h) else { break };
                on_main.insert(h.clone());
                main.push(b.clone());
                cur = b.prev.clone();
            }
            main.reverse();
        }
        let floor = main.first().map(|b| b.height).unwrap_or(0);
        let mut side: Vec<BlockInfo> = self.blocks.values().filter(|b| !on_main.contains(&b.hash) && b.height >= floor).cloned().collect();
        side.sort_by(|a, b| a.height.cmp(&b.height).then_with(|| a.hash.cmp(&b.hash)));
        ChainSnapshot { heads: self.heads.clone(), majority, main, side, tips: self.tips.clone(), block_count: self.blocks.len() }
    }
}
