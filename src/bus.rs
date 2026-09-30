//! The event bus: assigns `seq` and `ts`, keeps a bounded in-memory log for `/api/events?since`,
//! fans out to WebSocket clients over a broadcast channel and appends to the session recorder.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::broadcast;
use tracing::warn;

use crate::events::{now, Event, EventKind, Recorder};
use crate::model::chain::Emitted;

pub struct Bus {
    log: Mutex<VecDeque<Event>>,
    seq: AtomicU64,
    tx: broadcast::Sender<Event>,
    recorder: Mutex<Option<Recorder>>,
    max: usize,
}

impl Bus {
    pub fn new(max: usize, recorder: Option<Recorder>) -> Bus {
        let (tx, _) = broadcast::channel(4096);
        Bus { log: Mutex::new(VecDeque::new()), seq: AtomicU64::new(0), tx, recorder: Mutex::new(recorder), max }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn last_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    pub fn publish(&self, height: Option<u64>, node: Option<String>, kind: EventKind) -> Event {
        self.publish_at(now(), height, node, kind)
    }

    /// `publish` with an explicit timestamp (replay keeps the recorded one).
    pub fn publish_at(&self, ts: f64, height: Option<u64>, node: Option<String>, kind: EventKind) -> Event {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let event = Event { seq, ts, height, node, kind };
        {
            let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
            log.push_back(event.clone());
            while log.len() > self.max {
                log.pop_front();
            }
        }
        if let Some(rec) = self.recorder.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            if let Err(e) = rec.write(&event) {
                warn!("record: {}", e);
            }
        }
        let _ = self.tx.send(event.clone());
        event
    }

    pub fn publish_all(&self, emitted: Vec<Emitted>) {
        for e in emitted {
            self.publish(e.height, e.node, e.kind);
        }
    }

    /// Events with `seq > since`, oldest first (only what is still in the window).
    pub fn since(&self, since: u64) -> Vec<Event> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|e| e.seq > since).cloned().collect()
    }
}
