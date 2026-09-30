//! Where "something changed on node N" comes from. Polling is always on (it is the baseline that
//! also catches what a push channel misses); a ZMQ subscriber, when a node has one, wakes the
//! collector the moment a block or tx arrives instead of at the next tick.

pub mod poll;
pub mod zmq;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A nudge to the collector of one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wake {
    /// The poll interval elapsed.
    Tick,
    /// ZMQ `hashblock`.
    Block(String),
    /// ZMQ `hashtx`.
    Tx(String),
}

pub trait Source: Send {
    fn name(&self) -> String;
    /// Run forever, sending wakes; the collector owns the receiving end.
    fn spawn(self: Box<Self>, tx: mpsc::Sender<Wake>) -> JoinHandle<()>;
}
