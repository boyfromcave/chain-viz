//! Every file under `ui/` is embedded and served (200, right content type) by the router, at `/`
//! and `/ui/<path>`; the page shell references only paths that exist. No node needed.

use std::sync::Arc;

use chain_viz::bus::Bus;
use chain_viz::collector::Model;
use chain_viz::server::{router, ui_paths, AppState};

async fn serve() -> String {
    let state = Arc::new(AppState { model: Arc::new(tokio::sync::RwLock::new(Model::default())), bus: Arc::new(Bus::new(64, None)), clients: Vec::new(), replay: None });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    format!("http://{}", addr)
}

#[tokio::test]
async fn every_ui_file_is_served() {
    let base = serve().await;
    let paths = ui_paths();
    for must in ["index.html", "app.js", "style.css", "lib.js", "panels/chain.js", "panels/mempool.js", "panels/events.js", "panels/header.js"] {
        assert!(paths.iter().any(|p| p == must), "ui/{} missing from the embedded dir: {:?}", must, paths);
    }
    let client = reqwest::Client::new();
    for p in &paths {
        let r = client.get(format!("{}/ui/{}", base, p)).send().await.unwrap();
        assert_eq!(r.status(), 200, "/ui/{}", p);
        let ct = r.headers()[reqwest::header::CONTENT_TYPE].to_str().unwrap().to_string();
        let want = match p.rsplit('.').next() {
            Some("js") => "text/javascript",
            Some("css") => "text/css",
            Some("html") => "text/html",
            Some("svg") => "image/svg+xml",
            _ => "",
        };
        assert!(ct.starts_with(want), "/ui/{}: content-type {}", p, ct);
        assert!(!r.text().await.unwrap().is_empty(), "/ui/{} is empty", p);
    }
    let index = client.get(format!("{}/", base)).send().await.unwrap();
    assert_eq!(index.status(), 200);
    let html = index.text().await.unwrap();
    assert!(html.contains("<!doctype html>"));
    // Every /ui/… reference in the shell resolves to an embedded file.
    for m in html.match_indices("/ui/") {
        let rest = &html[m.0 + 4..];
        let end = rest.find(['"', '\'', ')']).unwrap_or(rest.len());
        let p = &rest[..end];
        assert!(paths.iter().any(|x| x == p), "index.html references /ui/{} which is not embedded", p);
    }
    assert_eq!(client.get(format!("{}/ui/nope.js", base)).send().await.unwrap().status(), 404);
    // Static ES-module imports inside ui/ resolve to embedded files too.
    for p in &paths {
        if !p.ends_with(".js") {
            continue;
        }
        let src = client.get(format!("{}/ui/{}", base, p)).send().await.unwrap().text().await.unwrap();
        let dir = p.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        for line in src.lines().filter(|l| l.starts_with("import ")) {
            let Some(from) = line.split(" from ").nth(1) else { continue };
            let target = from.trim().trim_end_matches(';').trim_matches(|c| c == '\'' || c == '"');
            let joined = normalize(&format!("{}/{}", dir, target));
            assert!(paths.contains(&joined), "{}: import {} -> {} not embedded", p, target, joined);
        }
    }
}

fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}
