// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! Session files (`--record <dir>/session.jsonl`, `--replay <file>`): one JSON event per line,
//! the first line of every run a `session` header. The `Recorder` appends; `read_session`
//! parses. `session.jsonl` is opened in append mode, so a restart with the same `--record`
//! directory continues the same file with a new header line — a file may hold several runs,
//! each starting with `{"kind":"session",...}` and each with `seq` restarting at 1. Replay
//! treats every header as a run boundary (no gap is waited across it).
//!
//! Header, version 1 (`SCHEMA_VERSION` in `events.rs`):
//! `{"seq":1,"ts":…,"kind":"session","version":1,"chainViz":"0.1.0","nodes":["0",…],"chain":"regtest","started":…}`.

use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::events::{Event, EventKind, SCHEMA_VERSION};

/// The `session` header event for this run.
pub fn header(nodes: Vec<String>, chain: String) -> EventKind {
    EventKind::Session { version: SCHEMA_VERSION, chain_viz: env!("CARGO_PKG_VERSION").to_string(), nodes, chain, started: Some(crate::events::now()) }
}

/// `--record <dir>`: appends one JSON line per event to `<dir>/session.jsonl`.
pub struct Recorder {
    path: PathBuf,
    file: std::fs::File,
}

impl Recorder {
    pub fn open(dir: &Path) -> std::io::Result<Recorder> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("session.jsonl");
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Recorder { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&mut self, event: &Event) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()
    }
}

/// Parse a session file (blank lines skipped). Line 1 must be a `session` header of a version
/// this build reads; a later header (a restart appended to the same file) is kept in place.
/// Within a run the events are ordered by `seq` (files written before 0.1.0's bus took seq and
/// the write under one lock can hold a line or two out of order). A line that is not an event
/// (a torn last line after a crash, a duplicate key from an older writer) is skipped with a
/// warning, never fatal: the file is a log. Each line is read as a JSON value first so a
/// duplicate key (the last wins) does not fail the envelope's strict decoding.
pub fn read_session(text: &str) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(line).map_err(|e| e.to_string()).and_then(|v| serde_json::from_value::<Event>(v).map_err(|e| e.to_string()));
        let e = match parsed {
            Ok(e) => e,
            Err(err) if out.is_empty() => return Err(format!("line {}: {}", i + 1, err)),
            Err(err) => {
                skipped += 1;
                if skipped <= 5 {
                    warn!("session line {} skipped: {}", i + 1, err);
                }
                continue;
            }
        };
        match &e.kind {
            EventKind::Session { version, .. } if *version == SCHEMA_VERSION => {}
            EventKind::Session { version, .. } => return Err(format!("line {}: session version {} (want {})", i + 1, version, SCHEMA_VERSION)),
            _ if out.is_empty() => return Err("line 1 is not a session header".into()),
            _ => {}
        }
        out.push(e);
    }
    if skipped > 0 {
        warn!("session: {} line(s) skipped", skipped);
    }
    let mut run = 0u64;
    let keyed: Vec<(u64, u64)> = out
        .iter()
        .map(|e| {
            if matches!(e.kind, EventKind::Session { .. }) {
                run += 1;
            }
            (run, e.seq)
        })
        .collect();
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by_key(|&i| keyed[i]);
    Ok(order.into_iter().map(|i| out[i].clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_header_required() {
        assert!(read_session("{\"seq\":1,\"ts\":0,\"kind\":\"note\",\"text\":\"x\"}\n").is_err());
        let ok = "{\"seq\":0,\"ts\":0,\"kind\":\"session\",\"version\":1}\n{\"seq\":1,\"ts\":0,\"kind\":\"note\",\"text\":\"x\"}\n";
        assert_eq!(read_session(ok).unwrap().len(), 2);
        let two_runs = format!("{}{}", ok, ok);
        assert_eq!(read_session(&two_runs).unwrap().len(), 4, "a restart appends a second header");
        assert!(read_session("{\"seq\":0,\"ts\":0,\"kind\":\"session\",\"version\":99}\n").is_err());
        let swapped = "{\"seq\":1,\"ts\":0,\"kind\":\"session\",\"version\":1}\n{\"seq\":3,\"ts\":0,\"kind\":\"note\",\"text\":\"b\"}\n{\"seq\":2,\"ts\":0,\"kind\":\"note\",\"text\":\"a\"}\n";
        let seqs: Vec<u64> = read_session(swapped).unwrap().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        let torn = "{\"seq\":1,\"ts\":0,\"kind\":\"session\",\"version\":1}\n{\"seq\":2,\"ts\":0,\"height\":5,\"kind\":\"devnet_sim\",\"height\":5}\n{\"seq\":3,\"ts\":0,\"kind\":\"no";
        let got = read_session(torn).unwrap();
        assert_eq!(got.len(), 2, "duplicate key tolerated, torn last line skipped");
        assert_eq!(got[1].height, Some(5));
    }

    #[test]
    fn header_carries_versions() {
        let v = serde_json::to_value(header(vec!["0".into()], "regtest".into())).unwrap();
        assert_eq!(v["version"], SCHEMA_VERSION);
        assert_eq!(v["chainViz"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["chain"], "regtest");
        assert!(v["started"].is_number());
    }

    #[test]
    fn recorder_appends_and_flushes() {
        let dir = std::env::temp_dir().join(format!("chain-viz-rec-{}", std::process::id()));
        let e = Event { seq: 1, ts: 0.0, height: None, node: None, kind: header(vec![], "regtest".into()) };
        for _ in 0..2 {
            let mut r = Recorder::open(&dir).unwrap();
            r.write(&e).unwrap();
        }
        let text = std::fs::read_to_string(dir.join("session.jsonl")).unwrap();
        assert_eq!(read_session(&text).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
