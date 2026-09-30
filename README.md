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
(`--poll`, read from `getblockchaininfo.chain` of the first node that answers); a node with
`-zmqpubhashblock`/`-zmqpubhashtx` can be wired with `--zmq <id>=tcp://host:port` (Ycash has no
`getzmqnotifications`, so it cannot be discovered). `--record <dir>` appends every event to
`<dir>/session.jsonl` (see Sessions).

Credentials for `--nodes` URLs without userinfo come from, in order: `--cookie <path>` (a
Bitcoin-style cookie file, `user:password` on one line), `--datadir <dir>` (its `.cookie`,
also `regtest/.cookie` and `testnet3/.cookie`), else `--rpcuser`/`--rpcpassword`. A cookie is
what `ycashd` writes when it runs without `-rpcuser`, so a mainnet node needs no password in
any config or command line:

```
chain-viz --nodes http://127.0.0.1:8832 --datadir ~/.ycash
```

API: `GET /api/health`, `GET /api/snapshot`, `GET /api/events?since=<seq>`, `WS /ws`
(first frame `{"kind":"hello","seq":N}`). Event schema v1 is `src/events.rs`.

`--yolo` and `--lightwalletd` parse but are not implemented yet (chunk C4).

### The node

chain-viz needs, on each node it watches:

* `-yellowback -experimentalfeatures` for the Yellowback overlay (`yed_getinfo` and the rest);
  a stock node without them is watched as chain + mempool only (`nodes[].yellowback = false`);
* **no `-prune`**: blocks are fetched by hash with `getblock`, and a reorg walks back to the
  fork point;
* **`-txindex` is not required.** chain-viz calls `getrawtransaction` only for transactions
  that are in a node's mempool; anything confirmed is read through `getblock <hash> 2`, which
  carries the full transactions without an index (`src/rpc.rs` has one `get_raw_transaction`
  and the collector uses it for nothing confirmed).

Load per node (plan §7): one `getbestblockhash`, one `getrawmempool true` and one `yed_getinfo`
per poll; `getblock` once per new hash; `getchaintips` on a head move and every tenth poll.
`/api/health.rpcCalls` counts every method per node so a monitor can watch the rate.

### Memory: `--keep`

The chain model keeps the newest `--keep` blocks (default 5000) below the highest head and
evicts the rest — their `BlockInfo`, their txids in the `mined` set, and every event at a
lower height from the `/api/events` window — on each head move; rollups a later chunk adds
(revenue) are kept. A mainnet run is therefore bounded by `--keep`, not by uptime.
`qa/soak.sh <url> <pid> <minutes>` samples RSS (`ps -o rss`), `seq` and the `rpcCalls` sum of a
running instance and fails on a rising RSS, an uneven RPC rate or RPC-failure notes — run it
against a devnet with `heartbeat rate 2` and a small `--keep` (see the script header).

## Hosting

chain-viz binds **127.0.0.1:8480** unless told otherwise. Two ways to let others in:

**A reverse proxy** in front of the loopback instance, terminating TLS and forwarding `/ws`
as a WebSocket. nginx:

```
location / {
    proxy_pass         http://127.0.0.1:8480;
    proxy_http_version 1.1;
    proxy_set_header   Upgrade $http_upgrade;
    proxy_set_header   Connection "upgrade";
    proxy_set_header   X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

Caddy: `reverse_proxy 127.0.0.1:8480` (WebSockets and `X-Forwarded-For` come by default).

**`--public`** for an instance that faces the network (with or without a proxy;
`--listen 0.0.0.0:8480` warns when `--public` is off). It

* rate-limits every request per client IP (10 req/s, burst 40, then `429` with `Retry-After`;
  the first `X-Forwarded-For` entry is the client when a proxy sets it, else the peer);
* caps open WebSocket connections at 64 (`503` past that) and `/api/events` at 2000 events per
  call (oldest first, contiguous from `since`; ask again from the last `seq` when you got
  exactly 2000);
* redacts every string in every response and WebSocket frame that looks like a URL, a
  `user@host`, a `host:port` or a filesystem path — so no node address, credential,
  `--replay` file name or devnet path leaves the server. Node **ids** stay (`"0"`…`"7"`, or
  the `--nodes` index): they are how the UI names nodes.

Whether or not `--public` is on, RPC credentials are never logged and an RPC error never
carries the node's address (the client replaces it by `node <id>` before the error becomes a
`note` event or a log line). `tests/hardening.rs` runs the binary with a password and a
hostname that must not appear in `/api/health`, `/api/snapshot`, `/api/events`, the log or
an export, and checks the `429`.

**A static copy** for a host with no server at all:

```
chain-viz --devnet ~/yb-devnet --export ~/www/chain-viz
```

writes, every 30 s and once more at shutdown, `index.html`, `ui/` and `snapshot.json`,
`events.json`, `health.json` (redacted as under `--public`) plus `ui/data.js` holding the
same three as one global. The page opens from any static host (`python3 -m http.server` in
the directory, S3, GitHub Pages) and shows the exported state with connection state
`static`; `qa/ui-smoke.mjs <dir>` checks an export without a browser. Opening `index.html`
straight from `file://` works where the browser allows module scripts there (Firefox does;
Chrome does not, and serves nothing — use the one-line http server).

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
