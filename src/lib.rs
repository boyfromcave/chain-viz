//! chain-viz: a read-only sidecar of `ycashd` that shows, live, what the chain and the
//! Yellowback (YED) overlay are doing, across many nodes at once. See the plan
//! (`docs/plans/chain-viz-plan.md` in the workspace) and `README.md`.

pub mod auth;
pub mod bus;
pub mod collector;
pub mod events;
pub mod export;
pub mod model;
pub mod public;
pub mod replay;
pub mod rpc;
pub mod server;
pub mod session;
pub mod source;
