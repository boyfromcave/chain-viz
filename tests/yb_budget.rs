//! Plan §7's load budget, asserted on `/api/health.rpcCalls` recorded from a live run against
//! the devnet (`tests/fixtures/yb-budget.json`, with the counts the budget is measured against
//! taken from the same run's `/api/snapshot`). The recording script is in the plan's C3 notes;
//! re-record after changing what the collector asks per block.

use serde_json::Value;
use std::collections::BTreeMap;

fn fixture() -> Value {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/yb-budget.json")).expect("fixture");
    serde_json::from_str(&text).expect("json")
}

fn counts(f: &Value) -> BTreeMap<String, BTreeMap<String, u64>> {
    serde_json::from_value(f["rpcCalls"].clone()).expect("rpcCalls")
}

fn total(c: &BTreeMap<String, BTreeMap<String, u64>>, method: &str) -> u64 {
    c.values().map(|m| m.get(method).copied().unwrap_or(0)).sum()
}

#[test]
fn per_node_per_poll_budget() {
    let f = fixture();
    let c = counts(&f);
    let blocks = f["blocksKnown"].as_u64().unwrap();
    for (node, m) in &c {
        let polls = m.get("getbestblockhash").copied().unwrap_or(0);
        let g = |k: &str| m.get(k).copied().unwrap_or(0);
        // one getrawmempool per poll tick, plus one per ZMQ `hashtx` wake (the devnet passes
        // -zmqpubhashtx; each new tx wakes the mempool diff early), and at most one yed_getinfo
        // per poll (+1 for the first probe)
        assert!(g("getrawmempool") <= 2 * polls + 1, "node {}: getrawmempool {} > 2 x polls {}", node, g("getrawmempool"), polls);
        assert!(g("yed_getinfo") <= polls + 1, "node {}: yed_getinfo {} > polls {} + 1", node, g("yed_getinfo"), polls);
        // getblock once per new hash: no node fetches more blocks than the model knows
        assert!(g("getblock") <= blocks + 1, "node {}: getblock {} > blocks {}", node, g("getblock"), blocks);
        // C4: getblocksubsidy at most once per block the node fetched (cached per height)
        assert!(g("getblocksubsidy") <= g("getblock"), "node {}: getblocksubsidy {} > getblock {}", node, g("getblocksubsidy"), g("getblock"));
        // the per-block yed_* reads happen only on head moves, which are at most the blocks fetched
        assert!(g("yed_getstats") <= g("getblock"), "node {}: yed_getstats {} > getblock {}", node, g("yed_getstats"), g("getblock"));
        assert!(g("yed_getstatehash") <= g("getblock"), "node {}", node);
        // getchaintips: on head moves and every 10th poll
        assert!(g("getchaintips") <= g("getblock") + polls / 10 + 2, "node {}: getchaintips {}", node, g("getchaintips"));
    }
}

#[test]
fn network_wide_yellowback_budget() {
    let f = fixture();
    let c = counts(&f);
    let blocks = f["blocksKnown"].as_u64().unwrap();
    let yb_txs = f["ybTxsKnown"].as_u64().unwrap();
    let adds = f["mempoolAdds"].as_u64().unwrap();
    let history = f["history"].as_u64().unwrap();
    let nodes = c.len() as u64;
    // one yed_gettag per block across ALL nodes (the claim), plus a small slack for blocks whose
    // tag was fetched and evicted or side blocks that were later promoted
    assert!(total(&c, "yed_gettag") <= blocks + 2, "yed_gettag {} for {} blocks", total(&c, "yed_gettag"), blocks);
    // one yed_gettxinfo per Yellowback tx once confirmed (a tx seen in the mempool first is asked once more)
    assert!(total(&c, "yed_gettxinfo") <= 2 * yb_txs + 1, "yed_gettxinfo {} for {} txs", total(&c, "yed_gettxinfo"), yb_txs);
    // getrawtransaction once per mempool tx, across all nodes; decode + validate once per Yellowback mempool tx
    assert!(total(&c, "getrawtransaction") <= adds + 1, "getrawtransaction {} for {} mempool adds", total(&c, "getrawtransaction"), adds);
    assert!(total(&c, "yed_validaterawtransaction") <= adds + 1);
    assert!(total(&c, "yed_decodepayload") <= adds + 1);
    // the timeline backfill: one node, ceil(rows / 2016) pages
    assert!(total(&c, "yed_gethistory") <= history.div_ceil(2016) + 1, "yed_gethistory {} for {} rows", total(&c, "yed_gethistory"), history);
    // the leader's chain-wide reads: at most one per block, from one node, plus a startup slack of one per node
    for m in ["yed_getprice", "yed_getactivation", "yed_listminers", "yed_listattestors", "yed_listclaimable"] {
        assert!(total(&c, m) <= blocks + nodes, "{} {} for {} blocks", m, total(&c, m), blocks);
        let callers = c.values().filter(|x| x.get(m).copied().unwrap_or(0) > 0).count();
        assert!(callers <= 2, "{} asked on {} nodes (leader + at most one startup contender)", m, callers);
    }
    // vaults only when the vault counts changed (each MINT/REDEEM/CLAIM block at most) plus the first fetch
    let vault_blocks = f["blocksWithYbTxs"].as_u64().unwrap();
    assert!(total(&c, "yed_listvaults") <= vault_blocks + nodes, "yed_listvaults {} for {} blocks with Yellowback txs", total(&c, "yed_listvaults"), vault_blocks);
}
