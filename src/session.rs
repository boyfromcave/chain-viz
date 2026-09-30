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
pub fn read_session(text: &str) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let e: Event = serde_json::from_str(line).map_err(|e| format!("line {}: {}", i + 1, e))?;
        match &e.kind {
            EventKind::Session { version, .. } if *version == SCHEMA_VERSION => {}
            EventKind::Session { version, .. } => return Err(format!("line {}: session version {} (want {})", i + 1, version, SCHEMA_VERSION)),
            _ if out.is_empty() => return Err("line 1 is not a session header".into()),
            _ => {}
        }
        out.push(e);
    }
    Ok(out)
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
