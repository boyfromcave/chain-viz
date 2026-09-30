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
`--record <dir>` appends every event to `<dir>/session.jsonl` (see Sessions).

API: `GET /api/health`, `GET /api/snapshot`, `GET /api/events?since=<seq>`, `WS /ws`
(first frame `{"kind":"hello","seq":N}`). Event schema v1 is `src/events.rs`.

`--yolo`, `--lightwalletd`, `--public` and `--export` parse but are not implemented yet
(chunks C4, C7).

## Sessions: record and replay

```
chain-viz --devnet ~/yb-devnet --record ~/yb-devnet/viz     # appends ~/yb-devnet/viz/session.jsonl
chain-viz --replay ~/yb-devnet/viz/session.jsonl --speed 10  # no node; the same UI and API
chain-viz --replay session.jsonl --speed 0                   # as fast as possible
```

`session.jsonl` holds one event per line, exactly the `/api/events` shape (`src/events.rs`).
Line 1 of every run is a header:
`{"seq":1,"ts":…,"kind":"session","version":1,"chainViz":"0.1.0","nodes":["0",…],"chain":"regtest","started":…}`
— `version` is the event schema (`SCHEMA_VERSION`), `chainViz` the binary that wrote it. The file
is opened in append mode, so restarting with the same `--record` directory continues the same
file with a new header line and `seq` restarting at 1; replay treats each header as a run
boundary and waits no gap across it. A line that is not an event (a torn last line after a
crash) is skipped with a warning.

Replay rebuilds the model from the events alone (`src/replay.rs`), so the chain (main chain,
side blocks, orphans, per-node heads, majority), the mempool and the devnet heartbeat/sim are
as they were; `/api/health.replay = {file, pos, total, speed}` reports progress and the
snapshot grows as events are applied. What only RPC supplies is absent or partial in replay:
`yedInfo` is `{}`, `chain.tips` (raw `getchaintips` per node) is empty, `nodes[].up` is
`true` for every node named in the header, and a mempool tx's `present`/`firstSeen` know
only the first node that reported it. `--speed N` divides every inter-event gap by N.

The round-trip test (`tests/replay.rs`) replays `tests/fixtures/session-reorg2.jsonl`, recorded
from a live 8-node devnet (a `mine 2`, a 2-block invalidation on one node by the recording
script and three blocks mined there, one more block), and asserts the same `chain.main`
heights and hashes, tip, orphaned blocks and event count the live `/api/snapshot` and
`/api/health` reported.

## CI

`.github/workflows/ci.yml`: `cargo fmt --check`, build, test and `clippy -D warnings` on Ubuntu
and macOS with stable Rust on every push and pull request; a `v*` tag also builds release
binaries for linux x86_64 and macOS arm64 and attaches them to the GitHub release.
