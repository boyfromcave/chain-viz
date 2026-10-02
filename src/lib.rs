// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! chain-viz: a read-only sidecar of `ycashd` that shows, live, what the chain and the
//! Yellowback (YED) overlay are doing, across many nodes at once. See the plan
//! (`docs/plans/chain-viz-plan.md` in the workspace) and `README.md`.

pub mod auth;
pub mod bus;
pub mod classify;
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
