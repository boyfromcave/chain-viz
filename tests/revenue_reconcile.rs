//! Plan §5 C4: the ledger reconciles with the node. `tests/fixtures/revenue-blocks.json` holds
//! a run of devnet blocks (`getblock … 2`), each with `getblocksubsidy`, `yed_gettag` and the
//! `yed_gettxinfo` of every Yellowback tx, plus the sums the recorder computed from the node's
//! own answers. Σ `enforcefee` over the range must equal Σ `feeZat`; likewise `attestfee` and
//! `residual`; the coinbase rows must add up to the coinbase outputs.

use chain_viz::classify::find_payload;
use chain_viz::model::revenue::{Kind, RevenueModel};
use chain_viz::model::yellowback::YbTx;
use chain_viz::rpc::{BlockFull, BlockSubsidy};
use serde_json::Value;

fn fixture() -> Value {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/revenue-blocks.json")).expect("fixture");
    serde_json::from_str(&text).expect("json")
}

/// Feed the fixture into a fresh model the way the collector does; returns the model and the
/// number of Yellowback txs attributed.
fn build(f: &Value, enforcing: bool) -> (RevenueModel, usize) {
    let mut m = RevenueModel::new(5000);
    let mut n = 0;
    for entry in f["blocks"].as_array().unwrap() {
        let b: BlockFull = serde_json::from_value(entry["block"].clone()).unwrap();
        let s: BlockSubsidy = serde_json::from_value(entry["subsidy"].clone()).unwrap();
        m.set_subsidy(b.height, &s);
        let yb: Vec<(String, YbTx)> = b.tx.iter().filter_map(|t| find_payload(t).map(|p| (t.txid.clone(), YbTx::from_payload(&t.txid, p)))).collect();
        let ids: Vec<String> = yb.iter().map(|(id, _)| id.clone()).collect();
        assert!(m.wants_block(&b.hash, b.height));
        m.on_block(&b, &ids);
        m.on_tag(b.height, &b.hash, &entry["tag"]);
        for (id, mut t) in yb {
            let info = &entry["gettxinfo"][&id];
            assert!(info.is_object(), "{}: the recorder found no yed_gettxinfo for a tx the classifier calls Yellowback", id);
            t.apply(info, Some(b.height));
            let ev = m.on_yb_tx(b.height, &t, enforcing);
            assert!(m.on_yb_tx(b.height, &t, enforcing).is_empty(), "idempotent");
            let _ = ev;
            n += 1;
        }
    }
    (m, n)
}

#[test]
fn ledger_sums_equal_the_nodes() {
    let f = fixture();
    let (from, to) = (f["from"].as_u64().unwrap(), f["to"].as_u64().unwrap());
    let (m, n) = build(&f, true);
    let e = &f["expected"];
    assert_eq!(n as u64, e["ybTxs"].as_u64().unwrap(), "Yellowback txs found");
    let q = m.query(from, to, "block", &|_| None, &[], &[]);
    let t = &q["totals"];
    assert_eq!(t["enforcefee"]["zat"], e["feeZat"], "Σ enforcefee = Σ yed_gettxinfo.feeZat");
    assert_eq!(t["attestfee"]["zat"], e["attestFeeZat"], "Σ attestfee = Σ yed_gettxinfo.attestFeeZat");
    assert_eq!(t["residual"]["zat"], e["residualZat"], "Σ residual = Σ yed_gettxinfo.residualZat");
    assert_eq!(t["blocks"], e["blocks"]);
    assert_eq!(t["ybTxs"], e["ybTxs"]);
    let coinbase = t["subsidy"]["zat"].as_i64().unwrap() + t["netfee"]["zat"].as_i64().unwrap() + t["subsidyOther"]["zat"].as_i64().unwrap();
    assert_eq!(coinbase, e["coinbaseZat"].as_i64().unwrap(), "coinbase rows add up to the coinbase outputs");
    assert_eq!(t["subsidy"]["zat"], e["minerSubsidyZat"], "the miner claimed the whole subsidy");
    assert_eq!(t["subsidyOther"]["zat"], e["otherSubsidyZat"], "the fund's share is attributed by its address");
    assert_eq!(t["collateralRelease"]["zat"], 0, "enforcement on: no release rows");
    // every row's payee is an address the node spelled (fee rows) or a coinbase address
    for r in q["rows"].as_array().unwrap() {
        let p = r["payee"].as_str().unwrap();
        assert!(!p.is_empty() && !p.starts_with("hash160:") && p != "unknown", "{:?}", r);
    }
    // per-payee: Σ over the pool groups equals the totals; a pool's coinbase address rolls up under its tag
    let miners: Vec<Value> = Vec::new();
    let pools = m.query(from, to, "payoutKey", &|_| None, &miners, &[]);
    let g = pools["groups"].as_array().unwrap();
    assert_eq!(g.iter().map(|g| g["enforcefee"]["zat"].as_i64().unwrap()).sum::<i64>(), e["feeZat"].as_i64().unwrap());
    assert_eq!(g.iter().map(|g| g["blocksMined"].as_u64().unwrap()).sum::<u64>(), e["blocks"].as_u64().unwrap(), "every block is mined by some pool");
    for p in g {
        assert!(p["payee"].as_str().unwrap().starts_with("sm"), "{}", p["payee"]);
        assert_eq!(p["tags"], p["blocksMined"], "on the devnet every block carries its miner's tag: tags == blocks mined once aliases are resolved");
    }
    // USD: with pMint = $2 every zat figure doubles in USD, labelled at pMint
    let priced = m.query(from, to, "block", &|_| Some(2_000_000), &[], &[]);
    let fee = priced["totals"]["enforcefee"]["zat"].as_i64().unwrap() as f64 / 1e8 * 2.0;
    assert!((priced["totals"]["enforcefee"]["usd"].as_f64().unwrap() - fee).abs() < 1e-6);
    assert_eq!(priced["priceLabel"], "at pMint");
    assert_eq!(priced["totals"]["enforcefee"]["usdComplete"], true);
}

#[test]
fn enforcement_off_attributes_the_release() {
    // With enforcing = false every vault spend (redeem/claim) in the fixture yields a
    // collateral_release row to the spend's largest non-fee output.
    let f = fixture();
    let (m, _) = build(&f, false);
    let (from, to) = (f["from"].as_u64().unwrap(), f["to"].as_u64().unwrap());
    let q = m.query(from, to, "block", &|_| None, &[], &[]);
    let redeems = f["blocks"].as_array().unwrap().iter().flat_map(|b| b["gettxinfo"].as_object().unwrap().values()).filter(|i| i["type"] == "redeem").count();
    assert_eq!(q["noEnforcement"]["rows"].as_array().unwrap().len(), redeems);
    if redeems > 0 {
        assert!(q["totals"]["collateralRelease"]["zat"].as_i64().unwrap() > 0);
    }
    let _ = Kind::CollateralRelease;
}
