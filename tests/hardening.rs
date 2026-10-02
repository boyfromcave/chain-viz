// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! C7: `--keep` eviction of the chain model and the event window, and — against the real
//! binary, no node needed — `--public` never letting a credential, node address or path out
//! through `/api/health`, `/api/snapshot`, `/api/events`, the log or an `--export`, while the
//! rate limit answers 429 past the burst.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use chain_viz::bus::Bus;
use chain_viz::events::EventKind;
use chain_viz::model::chain::ChainModel;
use chain_viz::rpc::Block;

fn block(height: u64) -> Block {
    serde_json::from_value(json!({
        "hash": format!("{:064x}", height + 1),
        "height": height,
        "previousblockhash": if height == 0 { Value::Null } else { json!(format!("{:064x}", height)) },
        "tx": [format!("tx{}", height)],
        "chainwork": format!("{:064x}", height + 1),
        "time": 1_700_000_000 + height,
    }))
    .unwrap()
}

#[test]
fn keep_evicts_blocks_txids_and_events() {
    let keep = 10;
    let mut model = ChainModel::new(keep);
    let bus = Bus::new(100_000, None);
    let path: Vec<Block> = (0..5).map(block).collect();
    bus.publish_all(model.on_new_head("0", &path, 0.0));
    for h in 5..40 {
        bus.publish_all(model.on_new_head("0", &[block(h)], h as f64));
        if let Some(floor) = model.floor() {
            bus.evict_below(floor);
        }
    }
    let floor = model.floor().expect("a floor once the head is past keep");
    assert_eq!(floor, 39 - keep);
    assert!(model.len() <= keep as usize + 1, "{} blocks kept for keep {}", model.len(), keep);
    assert!(!model.knows(&block(3).hash), "block 3 evicted");
    assert!(model.knows(&block(39).hash) && model.knows(&block(floor).hash));
    assert!(!model.is_mined("tx3"), "an evicted block's txids leave the mined set");
    assert!(model.is_mined("tx39"));
    let events = bus.since(0);
    assert!(!events.is_empty());
    for e in &events {
        if let Some(h) = e.height {
            assert!(h >= floor, "event {:?} at height {} below the floor {}", e.kind, h, floor);
        }
    }
    assert!(events.iter().any(|e| matches!(e.kind, EventKind::Block(_))));
    // A heightless event published after the window's start stays.
    bus.publish(None, None, EventKind::Note { text: "kept".into() });
    bus.evict_below(floor);
    assert!(bus.since(0).iter().any(|e| matches!(&e.kind, EventKind::Note { text } if text == "kept")));
}

/// Run the binary with `args`, wait for its `listening on` line, return (child, base url).
fn spawn(args: &[&str]) -> (std::process::Child, String, std::thread::JoinHandle<String>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_chain-viz")).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn chain-viz");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut s = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            s.push_str(&line);
            s.push('\n');
        }
        s
    });
    let mut base = String::new();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(url) = line.strip_prefix("listening on ") {
            base = url.trim().to_string();
            break;
        }
    }
    assert!(!base.is_empty(), "no listening line");
    (child, base, log)
}

fn get(url: &str) -> (u16, String) {
    let r = reqwest::blocking::Client::new().get(url).send().expect("GET");
    (r.status().as_u16(), r.text().unwrap_or_default())
}

fn assert_clean(what: &str, text: &str) {
    for secret in ["hunter2", "alice", "localhost", "secret-node.example"] {
        assert!(!text.contains(secret), "{} leaks {:?}:\n{}", what, secret, text);
    }
}

#[test]
fn public_mode_leaks_nothing_and_rate_limits() {
    let dir = std::env::temp_dir().join(format!("chain-viz-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Two unreachable nodes with a password and a hostname that must never appear anywhere.
    let (mut child, base, log) = spawn(&[
        "--nodes",
        "http://alice:hunter2@localhost:1,http://secret-node.example:1",
        "--rpcuser",
        "alice",
        "--rpcpassword",
        "hunter2",
        "--public",
        "--listen",
        "127.0.0.1:0",
        "--poll",
        "0.2",
        "--export",
        dir.to_str().unwrap(),
        "--log",
        "debug",
    ]);
    std::thread::sleep(Duration::from_millis(1500));
    let (st, health) = get(&format!("{}/api/health", base));
    assert_eq!(st, 200);
    let (_, snapshot) = get(&format!("{}/api/snapshot", base));
    let (_, events) = get(&format!("{}/api/events?since=0", base));
    let h: Value = serde_json::from_str(&health).unwrap();
    assert_eq!(h["public"], true);
    assert_eq!(h["nodes"], 2);
    assert_eq!(h["nodesUp"], 0);
    let s: Value = serde_json::from_str(&snapshot).unwrap();
    let err = s["nodes"][0]["error"].as_str().expect("the unreachable node reports an error");
    assert!(err.contains("node 0") || err == "[redacted]", "the error names the node by id: {}", err);
    let ev: Vec<Value> = serde_json::from_str(&events).unwrap();
    assert!(ev.iter().any(|e| e["kind"] == "note"), "the failure was published as a note");
    for (what, text) in [("/api/health", &health), ("/api/snapshot", &snapshot), ("/api/events", &events)] {
        assert_clean(what, text);
    }
    // The burst is 40 tokens: 60 quick requests must see a 429.
    let mut limited = 0;
    for _ in 0..60 {
        if get(&format!("{}/api/health", base)).0 == 429 {
            limited += 1;
        }
    }
    assert!(limited > 0, "no 429 in 60 quick requests");
    // Stop (SIGTERM: the final export runs) and read the log.
    let _ = Command::new("kill").arg(child.id().to_string()).status();
    let _ = child.wait();
    let log = log.join().unwrap();
    assert!(log.contains("public mode"), "log:\n{}", log);
    assert!(log.contains("node 0") || log.contains("node 1"), "the collector logged its failure by node id:\n{}", log);
    assert_clean("stderr log", &log);
    // The export: shell, ui, data and the three json files, all clean.
    for f in ["index.html", "snapshot.json", "events.json", "health.json", "ui/data.js", "ui/static.js", "ui/app.js", "ui/panels/chain.js"] {
        let p = dir.join(f);
        assert!(p.is_file(), "export lacks {}", f);
        assert_clean(&format!("export {}", f), &std::fs::read_to_string(&p).unwrap());
    }
    let index = std::fs::read_to_string(dir.join("index.html")).unwrap();
    assert!(!index.contains("\"/ui/"), "export index uses relative paths");
    assert!(index.contains("ui/data.js") && index.contains("ui/static.js"));
    let _ = std::fs::remove_dir_all(&dir);
}
