//! `classify.rs` against transactions recorded from a live devnet (`tests/fixtures/yb-txs.json`:
//! `getrawtransaction … 1` outputs plus the node's own `yed_gettxinfo`): the payload scan must
//! find every Yellowback tx and name its type the way the node does, and must reject the
//! ordinary transactions (coinbases, plain spends) in `tests/fixtures/plain-txs.json`.

use chain_viz::classify::{decode, find_payload, find_payload_in_scripts, op_return_data};
use chain_viz::rpc::RawTransaction;
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)).expect("fixture");
    serde_json::from_str(&text).expect("json")
}

fn raw(v: &Value) -> RawTransaction {
    serde_json::from_value(serde_json::json!({"txid": v["txid"], "hex": v["hex"], "vout": v["vout"]})).expect("RawTransaction")
}

#[test]
fn every_recorded_yellowback_tx_is_found_with_the_nodes_type() {
    let f = fixture("yb-txs.json");
    let txs = f["txs"].as_array().expect("txs");
    assert!(!txs.is_empty());
    let mut kinds = std::collections::BTreeSet::new();
    for t in txs {
        let p = find_payload(&raw(t)).unwrap_or_else(|| panic!("{}: no payload found", t["txid"]));
        let info = &t["gettxinfo"];
        let node_type = info["type"].as_str().expect("gettxinfo.type");
        assert_eq!(p.tx_type, node_type, "{}", t["txid"]);
        kinds.insert(t["kind"].as_str().unwrap().to_string());
        // the payload's own numbers agree with the index where both carry them
        match node_type {
            "mint" => {
                let minted: i64 = info["yedOut"].as_i64().unwrap();
                assert_eq!(p.cents, Some(minted as u64), "mint cents");
                assert!(p.fee_vout.is_some(), "a MINT names its fee output");
            }
            "transfer" | "redeem" => {
                let assigned: u64 = info["assigned"].as_array().unwrap().iter().map(|a| a["cents"].as_u64().unwrap()).sum();
                assert_eq!(p.cents, Some(assigned), "assigned cents");
                assert_eq!(p.assignments.len(), info["assigned"].as_array().unwrap().len());
            }
            _ => {}
        }
        assert_eq!(p.version, 3);
    }
    // the fixture must cover the tx kinds the devnet produces (record more with the script in the plan's C3 notes)
    for want in ["mint", "transfer", "register", "redeem/owner"] {
        assert!(kinds.contains(want), "fixture lacks a {} tx: have {:?}", want, kinds);
    }
}

#[test]
fn ordinary_transactions_are_not_yellowback() {
    let f = fixture("plain-txs.json");
    for t in f["txs"].as_array().expect("txs") {
        assert!(find_payload(&raw(t)).is_none(), "{} ({}) classified as Yellowback", t["txid"], t["note"]);
    }
}

#[test]
fn contract_sample_payload_hexes_decode() {
    // Synthetic bodies for the three types the devnet does not produce on its own: the codec's
    // fixed widths (payload.h:19-45) are the whole check for those.
    let mut notice = vec![0x59, 0x42, 0x03, 0x06];
    notice.extend_from_slice(&[0x11; 32]);
    notice.push(0);
    notice.extend_from_slice(&300u32.to_le_bytes());
    assert_eq!(decode(&notice, 1).unwrap().tx_type, "notice");
    assert_eq!(decode(&[0x59, 0x42, 0x03, 0x07], 1).unwrap().tx_type, "equivocation");
    let mut revive = vec![0x59, 0x42, 0x03, 0x08];
    revive.extend_from_slice(&[0; 74]);
    assert_eq!(decode(&revive, 1).unwrap().tx_type, "revive");
    // a 78-byte payload is pushed with OP_PUSHDATA1 (direct pushes stop at 75 bytes)
    let mut script = vec![0x6a, 0x4c, 78];
    script.extend_from_slice(&revive);
    assert_eq!(op_return_data(&script).map(|d| d.len()), Some(78));
    assert!(find_payload_in_scripts(&[vec![0x76, 0xa9], script]).is_some());
}
