//! The fallback (and safety net): a tick per `--poll` seconds.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{Source, Wake};

pub struct PollSource {
    pub node: String,
    pub interval: Duration,
}

impl Source for PollSource {
    fn name(&self) -> String {
        format!("poll({}s)", self.interval.as_secs_f64())
    }
    fn spawn(self: Box<Self>, tx: mpsc::Sender<Wake>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if tx.send(Wake::Tick).await.is_err() {
                    return;
                }
            }
        })
    }
}
