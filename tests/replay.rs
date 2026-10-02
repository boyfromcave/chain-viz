// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! Round trip: `tests/fixtures/session-reorg2.jsonl` was recorded with `--record` from a live
//! `yellowback-devnet` (8 nodes, `--portseed 47`): a `mine 2`, then two blocks invalidated on
//! node 3 by the recording script (outside this repo) and three mined there, so every other
//! node reorgs depth 2 and two blocks are orphaned, then one more block. `session-reorg2.expected.json`
//! is what the live `/api/health` and `/api/snapshot.chain.main` said at the end of that run.
//! Replaying the file through `replay::apply` must rebuild the same main chain, tip, orphans
//! and event count.

use std::sync::Arc;

use serde::Deserialize;

use chain_viz::bus::Bus;
use chain_viz::collector::Model;
use chain_viz::events::EventKind;
use chain_viz::model::chain::BlockStatus;
use chain_viz::replay::{self, ReplayStatus};
use chain_viz::session::read_session;

#[derive(Deserialize)]
struct Expected {
    seq: u64,
    tip: Head,
    main: Vec<Head>,
    orphaned: Vec<String>,
    #[serde(rename = "blockCount")]
    block_count: usize,
}
#[derive(Deserialize, PartialEq, Debug)]
struct Head {
    height: u64,
    hash: String,
}

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e))
}

fn check(model: &Model, seq: u64) {
    let want: Expected = serde_json::from_str(&fixture("session-reorg2.expected.json")).unwrap();
    assert_eq!(seq, want.seq, "event count");
    let snap = model.chain.snapshot(200);
    let main: Vec<Head> = snap.main.iter().map(|b| Head { height: b.height, hash: b.hash.clone() }).collect();
    assert_eq!(main, want.main, "chain.main");
    let tip = snap.majority.expect("majority");
    assert_eq!(Head { height: tip.height, hash: tip.hash }, want.tip);
    assert_eq!(tip.nodes.len(), 8, "all nodes on the majority head");
    assert!(tip.disagreeing.is_empty());
    let mut orphaned: Vec<String> = snap.side.iter().filter(|b| b.status == BlockStatus::Orphaned).map(|b| b.hash.clone()).collect();
    orphaned.sort();
    let mut want_orphaned = want.orphaned;
    want_orphaned.sort();
    assert_eq!(orphaned, want_orphaned, "orphaned side blocks");
    assert_eq!(snap.block_count, want.block_count);
    assert_eq!(model.nodes.len(), 8);
    assert_eq!(model.chain_name, "regtest");
}

#[test]
fn replay_rebuilds_the_live_model() {
    let events = read_session(&fixture("session-reorg2.jsonl")).unwrap();
    assert!(matches!(&events[0].kind, EventKind::Session { version: 1, chain_viz, .. } if !chain_viz.is_empty()));
    assert_eq!(events.iter().filter(|e| matches!(e.kind, EventKind::Reorg { depth: 2, .. })).count(), 8, "every node reorged depth 2");
    let mut model = Model::default();
    replay::apply_all(&mut model, &events);
    check(&model, events.len() as u64);
}

#[tokio::test]
async fn replay_run_publishes_every_event_with_its_recorded_ts() {
    let events = read_session(&fixture("session-reorg2.jsonl")).unwrap();
    let total = events.len() as u64;
    let model = Arc::new(tokio::sync::RwLock::new(Model::default()));
    let bus = Arc::new(Bus::new(100_000, None));
    let status = Arc::new(ReplayStatus::new("fixture".into(), total, 0.0));
    let mut rx = bus.subscribe();
    replay::run(model.clone(), bus.clone(), events.clone(), 0.0, status.clone()).await;
    assert!(status.done());
    assert_eq!(status.json()["pos"], total);
    assert_eq!(bus.last_seq(), total);
    let first = rx.recv().await.unwrap();
    assert_eq!(first.seq, 1);
    assert_eq!(first.ts, events[0].ts, "the recorded timestamp is kept");
    assert_eq!(bus.since(0).len(), total as usize);
    let m = model.read().await;
    check(&m, bus.last_seq());
}
