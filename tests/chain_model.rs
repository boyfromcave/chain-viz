//! The chain model driven by fixtures recorded from a real `yellowback-devnet` (regtest, three
//! of its eight nodes: 0 user, 2 pool, 3 pool). Each step holds, per node, `getbestblockhash`,
//! the `getblock … 1` path new to the fixture (oldest first) and `getchaintips` — exactly what
//! the collector feeds the model. No node is needed to run these.
//!
//! * `chain-agree.json`: three nodes agreeing, two blocks mined.
//! * `chain-reorg2.json`: node 3 invalidates two blocks and mines three; the others reorg depth 2.
//! * `chain-orphan.json`: node 3 mines a competing block at the tip height (others see it as
//!   `valid-headers`), then the majority extends and node 3 abandons its block.

use std::collections::HashMap;

use serde::Deserialize;

use chain_viz::events::EventKind;
use chain_viz::model::chain::{ChainModel, Emitted};
use chain_viz::rpc::{Block, ChainTip};

#[derive(Deserialize)]
struct Fixture {
    steps: Vec<Step>,
}
#[derive(Deserialize)]
struct Step {
    label: String,
    nodes: Vec<NodeRecord>,
}
#[derive(Deserialize)]
struct NodeRecord {
    node: String,
    best: String,
    blocks: Vec<Block>,
    chaintips: Vec<ChainTip>,
}

struct Run {
    model: ChainModel,
    blocks: HashMap<String, Block>,
    events: Vec<Vec<Emitted>>,
}

fn load(name: &str) -> Fixture {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e))).unwrap()
}

/// Feed the first `until` steps the way the collector would: the path when the head moved,
/// then that node's tips (every step stands for a poll on which tips were refreshed).
fn run_until(name: &str, until: usize) -> Run {
    let fx = load(name);
    let mut r = Run { model: ChainModel::new(5000), blocks: HashMap::new(), events: Vec::new() };
    for (i, step) in fx.steps.iter().take(until).enumerate() {
        let mut emitted = Vec::new();
        for n in &step.nodes {
            for b in &n.blocks {
                r.blocks.insert(b.hash.clone(), b.clone());
            }
            let moved = r.model.head(&n.node).map(|h| h.hash != n.best).unwrap_or(true);
            if moved {
                let path: Vec<Block> = if n.blocks.is_empty() { vec![r.blocks[&n.best].clone()] } else { n.blocks.clone() };
                assert_eq!(path.last().unwrap().hash, n.best, "{} step {} node {}", name, step.label, n.node);
                emitted.extend(r.model.on_new_head(&n.node, &path, 1000.0 + i as f64));
            }
            emitted.extend(r.model.on_chain_tips(&n.node, n.chaintips.clone(), 1000.0 + i as f64));
        }
        r.events.push(emitted);
    }
    r
}

fn run(name: &str) -> Run {
    run_until(name, usize::MAX)
}

fn kinds(events: &[Emitted]) -> Vec<&'static str> {
    events.iter().map(|e| kind_name(&e.kind)).collect()
}

fn kind_name(k: &EventKind) -> &'static str {
    match k {
        EventKind::Block(_) => "block",
        EventKind::BlockSide { .. } => "block_side",
        EventKind::Orphaned { .. } => "orphaned",
        EventKind::Reorg { .. } => "reorg",
        EventKind::Tip { .. } => "tip",
        _ => "other",
    }
}

fn count(events: &[Emitted], kind: &str) -> usize {
    kinds(events).iter().filter(|k| **k == kind).count()
}

fn reorgs(events: &[Emitted]) -> Vec<(String, u64, String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Reorg { depth, from, to, .. } => Some((e.node.clone().unwrap(), *depth, from.clone(), to.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn three_nodes_agree() {
    let r = run("chain-agree.json");
    assert_eq!(r.events.len(), 3);
    // Step 0: the first node backfills four blocks, the other two only move their head.
    assert_eq!(count(&r.events[0], "block"), 4);
    assert_eq!(count(&r.events[0], "tip"), 3);
    for step in &r.events[1..] {
        assert_eq!(count(step, "block"), 1, "one new block per mined step");
        assert_eq!(count(step, "tip"), 3);
    }
    for step in &r.events {
        assert_eq!(count(step, "reorg"), 0);
        assert_eq!(count(step, "orphaned"), 0);
        assert_eq!(count(step, "block_side"), 0);
    }
    let m = r.model.majority().unwrap();
    assert_eq!(m.nodes, ["0", "2", "3"]);
    assert!(m.disagreeing.is_empty());
    let snap = r.model.snapshot(200);
    assert_eq!(snap.main.len(), 6);
    assert_eq!(snap.main.last().unwrap().hash, m.hash);
    assert!(snap.side.is_empty());
    assert!(snap.main.windows(2).all(|w| w[1].prev.as_deref() == Some(&w[0].hash) && w[1].height == w[0].height + 1));
    // The block event names the node that reported it first and carries the block's height.
    let first = r.events[1].iter().find(|e| matches!(e.kind, EventKind::Block(_))).unwrap();
    assert_eq!(first.node.as_deref(), Some("0"));
    assert_eq!(first.height, Some(snap.main[4].height));
}

#[test]
fn depth_two_reorg() {
    // Step 1: node 3 invalidated two blocks: its head fell back; that is a reorg of depth 2 on
    // node 3 alone; the two blocks stay live (nodes 0 and 2 still have them), nothing is orphaned,
    // and the majority is nodes 0 and 2 with node 3 disagreeing.
    let r1 = run_until("chain-reorg2.json", 2);
    let step1 = &r1.events[1];
    let re = reorgs(step1);
    assert_eq!(re.len(), 1, "{:?}", kinds(step1));
    assert_eq!((re[0].0.as_str(), re[0].1), ("3", 2));
    assert_eq!(count(step1, "orphaned"), 0);
    let (h0, h3) = (r1.model.head("0").unwrap().clone(), r1.model.head("3").unwrap().clone());
    assert_eq!(h0.height, h3.height + 2);
    let m = r1.model.majority().unwrap();
    assert_eq!(m.nodes, ["0", "2"]);
    assert_eq!(m.disagreeing, ["3"]);
    assert_eq!(m.hash, h0.hash);
    // Node 3 reports the branch it left as `invalid`; nodes 0 and 2 still call it active.
    let snap = r1.model.snapshot(200);
    assert!(snap.tips["3"].iter().any(|t| t.hash == h0.hash && t.status == "invalid"), "{:?}", snap.tips["3"]);
    assert!(snap.tips["0"].iter().any(|t| t.hash == h0.hash && t.status == "active"));

    // Step 2: node 3 mined three on its branch; nodes 0 and 2 reorg depth 2 to it, and the two
    // abandoned blocks are orphaned exactly once each.
    let r = run("chain-reorg2.json");
    let step2 = &r.events[2];
    assert_eq!(count(step2, "block"), 3, "{:?}", kinds(step2));
    let re = reorgs(step2);
    let mut who: Vec<&str> = re.iter().map(|r| r.0.as_str()).collect();
    who.sort();
    assert_eq!(who, ["0", "2"]);
    assert!(re.iter().all(|r| r.1 == 2), "{:?}", re);
    assert!(re.iter().all(|r| r.2 == h0.hash && r.3 == re[0].3), "same from/to on every node: {:?}", re);
    assert_eq!(re[0].3, r.model.head("3").unwrap().hash);
    let orphaned: Vec<u64> = step2.iter().filter(|e| matches!(e.kind, EventKind::Orphaned { .. })).map(|e| e.height.unwrap()).collect();
    assert_eq!(orphaned, [h3.height + 1, h3.height + 2]);
    let m = r.model.majority().unwrap();
    assert_eq!(m.nodes, ["0", "2", "3"]);
    assert_eq!(m.height, h3.height + 3);
    let snap = r.model.snapshot(200);
    assert_eq!(snap.side.iter().filter(|b| b.status == chain_viz::model::chain::BlockStatus::Orphaned).count(), 2);
    assert_eq!(snap.main.last().unwrap().hash, m.hash);
}

#[test]
fn competing_block_then_orphaned() {
    let r = run("chain-orphan.json");
    // Step 1: node 0's getchaintips (fed first) shows node 3's block as a `valid-headers` side
    // tip before node 3's own head reports it: a block_side, then promoted to block by node 3,
    // whose tip move is a reorg of depth 1.
    let step1 = &r.events[1];
    let k = kinds(step1);
    let side = k.iter().position(|x| *x == "block_side").expect("block_side");
    let block = k.iter().position(|x| *x == "block").expect("block");
    assert!(side < block, "{:?}", k);
    let side_ev = step1.iter().find(|e| matches!(e.kind, EventKind::BlockSide { .. })).unwrap();
    match &side_ev.kind {
        EventKind::BlockSide { status, branchlen, .. } => {
            assert_eq!(status, "valid-headers");
            assert_eq!(*branchlen, 1);
        }
        _ => unreachable!(),
    }
    let re = reorgs(step1);
    assert_eq!(re.len(), 1);
    assert_eq!((re[0].0.as_str(), re[0].1), ("3", 1));
    let competing = re[0].3.clone();
    assert_eq!(count(step1, "orphaned"), 0);
    assert_eq!(r.model.block(&competing).unwrap().status, chain_viz::model::chain::BlockStatus::Orphaned, "by the end it is orphaned");
    // Step 2: the majority extended; node 3 reorgs back (depth 1) and its block is orphaned.
    let step2 = &r.events[2];
    let re = reorgs(step2);
    assert_eq!(re.len(), 1, "{:?}", kinds(step2));
    assert_eq!((re[0].0.as_str(), re[0].1, re[0].2.as_str()), ("3", 1, competing.as_str()));
    let orphaned: Vec<String> = step2.iter().filter_map(|e| match &e.kind { EventKind::Orphaned { hash } => Some(hash.clone()), _ => None }).collect();
    assert_eq!(orphaned.as_slice(), std::slice::from_ref(&competing));
    let m = r.model.majority().unwrap();
    assert_eq!(m.nodes, ["0", "2", "3"]);
    // Tips stay per node: the same hash is `valid-fork` on node 3 and `valid-headers` on node 0.
    let snap = r.model.snapshot(200);
    let status = |node: &str| snap.tips[node].iter().find(|t| t.hash == competing).map(|t| t.status.clone());
    assert_eq!(status("3").as_deref(), Some("valid-fork"));
    assert_eq!(status("0").as_deref(), Some("valid-headers"));
    // Side blocks: the orphaned competing block and the earlier reorg's branch, of which only
    // the tip is known (getchaintips reports tips, not their ancestors).
    let side: Vec<(u64, chain_viz::model::chain::BlockStatus)> = snap.side.iter().map(|b| (b.height, b.status)).collect();
    assert_eq!(side.len(), 2, "{:?}", side);
    assert!(side.contains(&(m.height - 1, chain_viz::model::chain::BlockStatus::Orphaned)), "{:?}", side);
    assert!(side.contains(&(m.height - 2, chain_viz::model::chain::BlockStatus::Side)), "{:?}", side);
}
