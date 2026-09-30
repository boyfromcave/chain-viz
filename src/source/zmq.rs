//! ZMQ `hashblock` / `hashtx` subscriber (pure-Rust `zeromq`, no libzmq). Ycash 4.5 has the
//! publisher (`-zmqpubhashblock`, `-zmqpubhashtx`; `src/zmq/`) but no `getzmqnotifications`
//! RPC, so the endpoint comes from `--zmq <node>=<tcp url>` or `devnet.json`'s `zmq` map.
//! Reconnects with backoff; every message is a wake, the collector still verifies over RPC.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use zeromq::{Socket, SocketRecv};

use super::{Source, Wake};

pub struct ZmqSource {
    pub node: String,
    pub url: String,
}

impl Source for ZmqSource {
    fn name(&self) -> String {
        format!("zmq({})", self.url)
    }
    fn spawn(self: Box<Self>, tx: mpsc::Sender<Wake>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut backoff = 1u64;
            loop {
                match subscribe(&self.node, &self.url, &tx).await {
                    Ok(()) => return,
                    Err(e) => {
                        warn!(node = %self.node, "zmq {}: {} (retry in {}s)", self.url, e, backoff);
                        tokio::time::sleep(Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(30);
                    }
                }
            }
        })
    }
}

async fn subscribe(node: &str, url: &str, tx: &mpsc::Sender<Wake>) -> Result<(), String> {
    let mut socket = zeromq::SubSocket::new();
    socket.connect(url).await.map_err(|e| e.to_string())?;
    socket.subscribe("hashblock").await.map_err(|e| e.to_string())?;
    socket.subscribe("hashtx").await.map_err(|e| e.to_string())?;
    info!(node = %node, "zmq subscribed to {}", url);
    loop {
        let msg = socket.recv().await.map_err(|e| e.to_string())?;
        let frames: Vec<&[u8]> = msg.iter().map(|b| b.as_ref()).collect();
        if frames.len() < 2 {
            continue;
        }
        let topic = String::from_utf8_lossy(frames[0]).to_string();
        let body = hex::encode(frames[1]);
        debug!(node = %node, "zmq {} {}", topic, body);
        let wake = match topic.as_str() {
            "hashblock" => Wake::Block(body),
            "hashtx" => Wake::Tx(body),
            _ => continue,
        };
        if tx.send(wake).await.is_err() {
            return Ok(());
        }
    }
}
