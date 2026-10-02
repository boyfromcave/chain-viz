// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! `--export <dir>`: a static snapshot the UI opens with no server. The directory gets
//! `snapshot.json`, `events.json` and `health.json` (the three API answers, redacted as under
//! `--public`), the embedded `ui/` files, `ui/data.js` (the same three answers as one global,
//! so the page works from `file://` where `fetch` of a sibling file does not) and an
//! `index.html` rewritten to load relative paths plus `ui/data.js` and `ui/static.js` ahead of
//! the app. `static.js` answers the app's `/api/*` fetches from the global and `app.js` skips
//! the WebSocket when it sees it (connection state `static`).
//!
//! Written once the model has a tip, then every `EVERY` seconds and once more at shutdown; each
//! file lands by rename so a static host never serves a torn one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::{info, warn};

use crate::events::now;
use crate::public;
use crate::server::{ui_files, AppState};

/// Seconds between rewrites while running.
pub const EVERY: u64 = 30;

/// The rewritten shell: relative `ui/` paths and the two static scripts before the app module.
pub fn static_index(index: &str) -> String {
    let rel = index.replace("\"/ui/", "\"ui/").replace("'/ui/", "'ui/");
    let inject = "<script src=\"ui/data.js\"></script>\n<script src=\"ui/static.js\"></script>\n";
    match rel.find("<script type=\"module\"") {
        Some(i) => format!("{}{}{}", &rel[..i], inject, &rel[i..]),
        None => rel.replace("</body>", &format!("{}</body>", inject)),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Write the whole export to `dir` from the given API values (already redacted by the caller).
pub fn write(dir: &Path, snapshot: &Value, events: &Value, health: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("ui").join("panels"))?;
    write_atomic(&dir.join("snapshot.json"), snapshot.to_string().as_bytes())?;
    write_atomic(&dir.join("events.json"), events.to_string().as_bytes())?;
    write_atomic(&dir.join("health.json"), health.to_string().as_bytes())?;
    let data = json!({"snapshot": snapshot, "events": events, "health": health, "exported": now()});
    write_atomic(&dir.join("ui").join("data.js"), format!("window.CHAIN_VIZ_STATIC = {};\n", data).as_bytes())?;
    for (path, bytes) in ui_files() {
        if path == "index.html" {
            write_atomic(&dir.join("index.html"), static_index(std::str::from_utf8(bytes).unwrap_or("")).as_bytes())?;
        } else {
            let p = dir.join("ui").join(&path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write_atomic(&p, bytes)?;
        }
    }
    Ok(())
}

/// One export from the live state, redacted whether or not `--public` is on: a static copy is
/// made to be handed around.
pub async fn write_from(state: &AppState, dir: &Path) -> std::io::Result<()> {
    let mut snapshot = state.snapshot_json().await;
    let mut events = state.events_json(0);
    let mut health = state.health_json().await;
    for v in [&mut snapshot, &mut events, &mut health] {
        public::redact(v);
    }
    write(dir, &snapshot, &events, &health)
}

/// The writer task: first export once there is a tip (or after 30 s regardless), then every
/// `EVERY` seconds. `final_export` is for the shutdown path.
pub fn spawn(state: Arc<AppState>, dir: PathBuf) {
    tokio::spawn(async move {
        for _ in 0..30 {
            if state.model.read().await.chain.majority().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        loop {
            match write_from(&state, &dir).await {
                Ok(()) => info!("exported to {}", dir.display()),
                Err(e) => warn!("export {}: {}", dir.display(), e),
            }
            tokio::time::sleep(Duration::from_secs(EVERY)).await;
        }
    });
}

pub async fn final_export(state: &AppState, dir: &Path) {
    match write_from(state, dir).await {
        Ok(()) => info!("final export to {}", dir.display()),
        Err(e) => warn!("export {}: {}", dir.display(), e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_rewritten() {
        let out = static_index("<html><link href=\"/ui/style.css\"><script type=\"module\" src=\"/ui/app.js\"></script></html>");
        assert!(!out.contains("/ui/"));
        assert!(out.contains("href=\"ui/style.css\""));
        let data = out.find("ui/data.js").unwrap();
        let shim = out.find("ui/static.js").unwrap();
        let app = out.find("ui/app.js").unwrap();
        assert!(data < shim && shim < app, "data, then shim, then the app");
    }
}
