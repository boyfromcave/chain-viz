# chain-viz

A **read-only** sidecar of `ycashd` that shows, live, what a Ycash chain and its Yellowback (YED)
overlay are doing across many nodes at once: per-node tips and their disagreement, the block
DAG with side branches, reorgs and orphans as they happen, the mempool with first-seen per node,
and (later chunks) the Yellowback health, prices, vaults and the revenue of participating. It
never holds a key, never broadcasts, never calls a `yed_*` writer (`tests/readonly_gate.rs`
enforces that).

Plan: `docs/plans/chain-viz-plan.md` in the yellowback workspace.

## Build

Rust stable (`rust-toolchain.toml`), no system dependencies (rustls, pure-Rust ZMQ):

```
cargo build --release
cargo test && cargo clippy --all-targets -- -D warnings
```

## Run

Against a `yellowback-devnet`:

```
chain-viz --devnet ~/yb-devnet            # reads devnet.json for nodes + credentials
```

Against your own node(s):

```
chain-viz --nodes http://user:pass@127.0.0.1:8832
chain-viz --nodes http://127.0.0.1:8832,http://10.0.0.2:8832 --rpcuser u --rpcpassword p
```

Then open the printed `http://127.0.0.1:8480`. Polling defaults to 1 s on regtest, 5 s otherwise
(`--poll`); a node with `-zmqpubhashblock`/`-zmqpubhashtx` can be wired with
`--zmq <id>=tcp://host:port` (Ycash has no `getzmqnotifications`, so it cannot be discovered).
`--record <dir>` appends every event to `<dir>/session.jsonl`.

API: `GET /api/health`, `GET /api/snapshot`, `GET /api/events?since=<seq>`, `WS /ws`
(first frame `{"kind":"hello","seq":N}`). Event schema v1 is `src/events.rs`.

`--replay`, `--speed`, `--yolo`, `--lightwalletd`, `--public` and `--export` parse but are not
implemented yet (chunks C4, C6, C7).
