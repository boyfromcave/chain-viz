# chain-viz

chain-viz is a **read-only sidecar of `ycashd`** that shows, live, what a Ycash chain and its
Ycash Yellowback (YED) overlay are doing across many nodes at once: per-node tips and their
disagreement, the block DAG with side branches, reorgs and orphans as they happen, the mempool
with first-seen per node and each Yellowback transaction's verdict before a pool includes it,
the state of every Yellowback enforcement mechanism (activation, arming, halt mask, prices,
supply, collateral, vaults, attestors, state-hash agreement), and a **revenue ledger** that
attributes every enforcement fee and attestor fee to the key that received it — so a pool
operator or a prospective attestor can see what participating pays. It never holds a key,
never broadcasts, never calls a `yed_*` writer; a CI test enforces that. One static binary,
an embedded UI, no build step for the page.

![chain-viz on an 11-node devnet](docs/screenshot.png)

Plan and findings: `docs/plans/chain-viz-plan.md` in the yellowback workspace (§9 has the
status table and the C-F findings; `docs/mapping.md` §18 mirrors the node-side ones).

## Install and build

Rust 1.91.0 (`rust-toolchain.toml` pins the exact release; rustup fetches it), no system
dependencies (rustls for HTTP, pure-Rust ZMQ):

```
cargo build --release                                   # target/release/chain-viz
cargo test && cargo clippy --all-targets -- -D warnings # what CI runs
node qa/ui-smoke.mjs http://127.0.0.1:8480              # the UI panels under a fake DOM, against a live instance
```

Tagged releases (`v*`) carry linux x86_64 and macOS arm64 binaries (see CI below). If several
checkouts share `$CARGO_TARGET_DIR`, build a binary you are about to run under a private
target dir — another build replacing the file while it runs leaves the process stuck
(plan C-F18).

## Run against the devnet

`yellowback-devnet up` (in `ycash-dd/contrib/yellowback/devnet/`) **starts chain-viz itself**
when it finds a binary — `CHAINVIZ_BIN`, else `chain-viz` on `PATH`, else the workspace's
`chain-viz/target/release/chain-viz` then `target/debug/chain-viz` — as

```
chain-viz --devnet <dir> --listen 127.0.0.1:<32680 + portseed % 88> --record <dir>/chain-viz --pid-file <dir>/chain-viz.pid
```

and prints the URL in `up`'s summary; `status` says whether it is alive, `down` stops it,
`report` bundles the recorded `session.jsonl`, and `--no-viz` opts out. The devnet also
passes `-zmqpubhashblock`/`-zmqpubhashtx` to every node and records the endpoints in
`devnet.json` (`nodes["<n>"].zmq`), so chain-viz sees blocks and transactions the moment the
node has them. To run it by hand:

```
CHAINVIZ_BIN=$PWD/target/release/chain-viz yellowback-devnet up --role user --sim-profile fast
chain-viz --devnet ~/yb-devnet                 # or by hand: reads devnet.json for nodes, credentials, zmq
```

`--devnet` also reads `heartbeat.json`, `sim-stats.json`, `mock-price` and `attest-price-N`
beside `devnet.json`: the heartbeat rate sets the target block spacing, the fed prices are
drawn dotted on the price chart, and the personas' counts arrive as `devnet_sim` events.

To watch a fork and a reorg heal, invalidate a block **near the tip** on one node, mine past it
there, then reconsider it (`D` is the devnet command):

```
H=$(D cli --node 4 -- getblockhash $(( $(D cli --node 4 -- getblockcount) - 2 )))
D cli --node 4 -- invalidateblock $H      # node 4's chip falls off the majority; side branch appears
D cli --node 4 -- generate 3              # node 4 extends its own branch (not `mine`: that helper waits for all nodes to agree)
D cli --node 4 -- reconsiderblock $H      # node 4 rejoins the majority; its blocks become orphans
```

With the heartbeat running the majority stays ahead, so it is node 4 that reorgs back. To make the
*others* reorg instead, `D heartbeat stop`, `generate 5` on node 4, then `D heartbeat start`: one
`reorg` row per node appears as they move onto node 4's longer branch.

Never invalidate more than 99 blocks deep (plan finding C-F33). Both node versions refuse a reorg
longer than `MAX_REORG_LENGTH` = 99 with a shutdown (v4.5.0 `src/main.cpp:3772`, 6.20.0
`src/main.cpp:4673-4688`), and a wallet node's witness cache holds `WITNESS_CACHE_SIZE` =
`MAX_REORG_LENGTH + 1` = 100 entries at both (v4.5.0 `src/wallet/wallet.h:282`, 6.20.0
`src/wallet/wallet.h:89`, `src/main.h:66`). On v4.5.0 the 101st disconnect asserts in every wallet
(`src/wallet/wallet.cpp:1737`); on 6.20.0 only in a wallet that has seen Sprout or Sapling notes
(`src/wallet/wallet.cpp:3596`). On macOS an aborted node then hangs beyond `kill -9` until
`down`/reboot.

## Run against your own node

```
chain-viz --nodes http://127.0.0.1:8832 --datadir ~/.ycash     # cookie auth, one mainnet node
chain-viz --nodes http://user:pass@127.0.0.1:8832
chain-viz --nodes http://127.0.0.1:8832,http://10.0.0.2:8832 --rpcuser u --rpcpassword p
```

Then open the printed `http://127.0.0.1:8480`. Credentials for a `--nodes` URL without userinfo
come from, in order: `--cookie <path>` (a Bitcoin-style cookie file, `user:password` on one
line), `--datadir <dir>` (its `.cookie`, also `regtest/.cookie` and `testnet3/.cookie`), else
`--rpcuser`/`--rpcpassword`. The cookie is what `ycashd` writes when it runs without
`-rpcuser`, so a mainnet node needs no password in any config or command line.

The node needs:

* `-yellowback -experimentalfeatures` for the Yellowback overlay (`yed_getinfo` and the rest).
  A stock node without them is watched as chain + mempool only (`nodes[].yellowback = false`)
  and still counts in the tip-agreement view.
* **no `-prune`**: blocks are fetched by hash with `getblock`, and a reorg walks back to the
  fork point.
* **`-txindex` is not required.** Confirmed transactions are read through `getblock <hash> 2`,
  which carries them in full without an index; `getrawtransaction` is only ever asked for a
  transaction in a node's mempool.
* optionally `-zmqpubhashblock=tcp://127.0.0.1:28332 -zmqpubhashtx=tcp://127.0.0.1:28332` (one
  socket serves both topics), wired with `--zmq 0=tcp://127.0.0.1:28332`. Ycash has no
  `getzmqnotifications`, so the endpoint cannot be discovered. Polling is always on; ZMQ only
  wakes the poller early.

Polling defaults to 1 s on regtest and 5 s otherwise, read from `getblockchaininfo.chain` of
the first node that answers. Memory is bounded by `--keep` (default 5000 blocks below the
highest head; older blocks, their txids and their events are evicted; the revenue rollups are
kept), not by uptime.

## CLI reference

| Flag | Meaning |
|---|---|
| `--devnet <DIR>` | Read `<DIR>/devnet.json` for the nodes, credentials and ZMQ endpoints; read the devnet's heartbeat, sim and price files beside it. |
| `--nodes <URL[,URL…]>` | Node RPC URLs, `http://[user:pass@]host:port`, comma separated; ids are the position (`0`, `1`, …) after any `--devnet` nodes. |
| `--rpcuser <USER>`, `--rpcpassword <PASSWORD>` | Applied to every `--nodes` URL without userinfo. |
| `--cookie <PATH>` | A node's RPC cookie file; ahead of `--rpcuser`. Conflicts with `--datadir`. |
| `--datadir <DIR>` | Reads `<DIR>/.cookie` (also `regtest/.cookie`, `testnet3/.cookie`). |
| `--zmq <ID=URL>` | A node's ZMQ endpoint, repeatable (`--zmq 0=tcp://127.0.0.1:28332`). |
| `--listen <ADDR>` | HTTP/WS bind address, default `127.0.0.1:8480`. A non-loopback bind is refused without `--public` (or `--i-know-this-is-exposed`). |
| `--poll <SECS>` | Poll interval; default 1 on regtest, 5 otherwise. |
| `--record <DIR>` | Append every event to `<DIR>/session.jsonl`. |
| `--replay <FILE>` | Serve from a recorded session; no node. |
| `--speed <N>` | Replay speed multiplier, default 1; `0` = as fast as possible. |
| `--keep <BLOCKS>` | Blocks kept in the model, default 5000. |
| `--public` | Public mode: per-IP rate limit and no node address, URL, credential or path in any response. |
| `--trusted-proxies <CIDR>` | Reverse proxies (IP or CIDR, repeatable or comma separated) whose `X-Forwarded-For` names the client for the `--public` rate limit; from any other peer the header is ignored. |
| `--allow-origin <ORIGIN>` | Browser origins (`scheme://host[:port]`, repeatable; `*` = any) allowed to open `/ws` besides the request's own `Host`. |
| `--i-know-this-is-exposed` | Serve a non-loopback `--listen` without `--public`: no rate limit, no redaction. |
| `--export <DIR>` | Write a static copy (`index.html`, `ui/`, the three API answers) every 30 s and at shutdown. |
| `--pid-file <PATH>` | Write the pid there; removed on exit. |
| `--rpc-concurrency <N>` | In-flight RPC calls allowed per node, default 2. |
| `--log <LEVEL>` | `error`, `warn`, `info` (default), `debug`, `trace`, or a `tracing` filter. Logs go to stderr; stdout carries only `listening on http://…`. |
| `--yolo <URL[,URL…]>`, `--lightwalletd <URL>` | Parsed and ignored with a warning: the pool and lightwalletd lanes of plan §3.4 are not implemented. |

SIGINT or SIGTERM stops it (the devnet's `down` sends SIGTERM); `--export` writes once more on
the way out.

## API

All JSON. Under `--public` every response is redacted (URLs, `user@host`, `host:port`, paths;
node ids stay); WebSocket frames are redacted always. `/api/events` returns at most 2000 events
per call and at most 64 WebSocket connections are open at once, `--public` or not.

* **`GET /api/health`** — for a monitor or a test to assert on:
  `{ok, nodes, nodesUp, tip{height,hash}, agreeing, disagreeing[], seq, version, chain,
  rpcCalls{<node>{<method>: count}}, replay{file,pos,total,speed}|null, public, wsOpen}`.
  `ok` is "at least one node up"; `tip`/`agreeing` describe the majority head. `agreeing` can
  momentarily exceed `nodesUp` (plan C-F27): wait for both when scripting.
* **`GET /api/snapshot`** — the whole model, what the UI loads on open:
  `{version, schema, seq, ts, chainName, nodes[{id,role,sources,up,error,yellowback,lastSeen}],
  chain{heads, majority, main[≤200 BlockInfo, newest last], side[], tips, blockCount},
  mempool{txs[{txid,size,fee,time,firstSeen,present,depends,yb?}], count, bytes, feeTotal, nodes},
  yedInfo{<node>: yed_getinfo}, yellowback{…}, revenue{totals, byPayee, aliases, window},
  devnet{heartbeat, sim}}`. `BlockInfo` is `{hash, height, prev, time, txCount, size,
  chainwork, status: main|side|orphaned, nodes, seen, txids?, yb?{tag, miner, rejected, txs}}`.
* **`GET /api/events?since=<seq>`** — the event log from a sequence number, oldest first,
  each `{seq, ts, height?, node?, kind, …}`. Kinds (schema v1, `src/events.rs`): `session`,
  `block`, `block_side`, `orphaned`, `reorg{depth,from,to,toHeight}`, `tip`, `mempool_add`,
  `mempool_remove`, `yb_tx{txid,type,verdict,feeZat,payee,…}`, `yb_state{field,from,to}`,
  `price`, `stats`, `statehash_mismatch`, `attestor`, `vault`, `revenue{entry,zat,payee,usd}`,
  `rejected_block{hash,verdict}`, `devnet_heartbeat`, `devnet_sim`, `note{text}`.
* **`WS /ws`** — the same events pushed. First frame `{"kind":"hello","seq","version"}`; on a
  slow client `{"kind":"lagged","dropped","seq"}`, after which resync via `/api/events?since=`.
* **`GET /api/yellowback`** — the health panel's slice, fetched once per block:
  `{seq, tip, yedInfo, yellowback{leader, stats{<node>: yed_getstats}, statehash{<node>},
  statehash_agree, price, attestPrices, activation, haltBits, miners[], attestors[], vaults[],
  claimable[], history[], historyFrom, txs[], rejected[], mockPrice},
  blocks[{hash,height,time,txCount,yb}]}`.
* **`GET /api/revenue?from=<h>&to=<h>&by=payoutKey|attestor|block`** — the ledger rolled up
  over `[from, to]` (default: the whole kept window):
  `{from, to, by, window{from,to}, priceLabel: "at pMint",
  totals{subsidy|subsidy_other|netfee|enforcefee|attestfee|collateral_release|residual: {zat, usd, usdComplete}, blocks, ybTxs},
  groups[…], counterfactual{zat, usd, feeOutputs, resolved, unresolved, noEligible, label, method},
  noEnforcement{rows, total, label}, rows[≤5000 {height,txid,vout,kind,zat,payee,usd,refHeight?}],
  rowsTruncated, seq, tip, enforcing[], pMintNow}`. A `payoutKey` group is
  `{payee, aliases, blocksMined, tags, timesSelected, enforcefee, subsidy, netfee,
  stockCoinbase, perTagZat, perBlockZat, miner}`; an `attestor` group carries the bond and the
  realised yield; a `block` group the rows of that height.

## The panels

**Header** — chain name, nodes up, majority tip, connection state, and the time since the last
block, large, coloured past 2× and 4× the target spacing (the devnet heartbeat's rate on
regtest, else Ycash's 75 s).

**Chain** — the block DAG newest right: the main row on top, side and orphaned blocks hanging
below their fork point; each block shows height, short hash, tx count, the tag's price and
signal bit and the quoting `payoutKey`, and a red edge if any node rejected it. One chip per
node with its tip; chips off the majority are highlighted — this is the fork-risk indicator,
and it is why chain-viz watches many nodes at once. Below: a reorg log (`reorg` is per node,
so one devnet reorg is one row per node) and three **derived** gauges, labelled as such:
*fork* = disagreeing nodes × seconds disagreeing (clocked from when the page first saw it),
*orphan* = extra blocks at duplicate heights in the last 20, *reorg* = deepest side branch
within the work-valve depth (6).

**Mempool** — bubbles by fee rate × time in mempool (radius ~ size). Grey for ordinary
transactions; coloured by type for Yellowback ones (`mint`, `transfer`, `redeem`, claim =
`redeem` with path `claim`, `register`, `notice`, `equivocation`, `revive`). A Yellowback
transaction that an enforcing node **would reject** at the current tip
(`yed_validaterawtransaction`) is shown red before any pool includes it. Tiles: count, bytes,
fees, median age, transactions older than 2 blocks (a pool filtering it, or a fee below
policy), *partial* = present on some nodes but not all (propagation, or a stock node refusing
what it does not understand).

**Yellowback health** — a state ribbon (`healthy`, `enforcing`, `valveTripped`, `sunset`,
`abandoned`, activation `SIGNALING → LOCKED_IN → ACTIVE`, arming `UNARMED → TRIGGERED →
ARMED`); a timeline by height with lanes for activation, arming, the six halt-mask bits and
rejected-block ticks, backfilled from `yed_gethistory`; prices (`pFast`/`pMid`/`pSlow`, the
`pMint`–`pClaim` band, the fed price dotted on the devnet, window fill bars); supply and
collateral tiles with the global ratio against the 110 % and 105 % lines; a vault scatter
(ratio × blocks to `claimHeight`, claimable vaults ringed); the attestor table; state-hash
agreement per node and the rejected/suppressed counters with verdicts; the Yellowback
transaction table. Two ratios on this panel are priced differently by the node on purpose:
the global-ratio tile is at `pMint`, the scatter at `pClaim`, and they disagree for about one
slow window after a price move (plan C-F9).

**Revenue** — see the next section.

**Events** — the tail of the event stream, newest first, filterable by kind.

## What the revenue numbers are, and are not

The ledger (`src/model/revenue.rs`) is a list of attributed outputs, one row per
`(height, txid, vout, kind, zat, payee)`, built from the coinbase and from `yed_gettxinfo` of
every Yellowback transaction in a block. It is **attributed, not estimated**:

* **Enforcement fees go to a quoting key, not to the block winner.** `enforcefee` rows carry
  `yed_gettxinfo.payee` — one key in E(R), the set of keys with a quote tag in the last 100
  blocks, chosen by the transaction builder. A pool with 5 % of hashpower that quotes every
  block is in E(R) for every fee on the network; a pool with 40 % that does not quote earns
  none. The per-pool table shows blocks mined, tags published, times selected, fees earned,
  fee per tag and per block, beside the stock coinbase — the marginal revenue of quoting.
* **Stock coinbase.** `subsidy` and `netfee` are the miner's coinbase outputs (net fees =
  coinbase value − `getblocksubsidy.miner`); `subsidy_other` is what the coinbase pays to
  someone else (regtest's founders output), matched by `getblocksubsidy.foundersaddress` on
  v4.5.0 and, since ycashd 6.20.0 no longer reports it, by the output equal to `founders`
  (a tie goes to the fund address last attributed). A pool's coinbase address is aliased to the
  `payoutKey` of the tag on the same block, so its blocks roll up under its quoting key;
  a block without a tag stays under its coinbase address.
* **Attestor fees** (`attestfee`, 25 % of the enforcement fee once ARMED) go to the bond key
  in `attestPayee`. The per-attestor yield is fees ÷ bond over the range, **realised, not
  promised**.
* **USD figures are at `pMint`** — the protocol's own mint price at that row's height
  (`yed_gethistory[].pMint`, or the current `yed_getstats.pMint` for the mempool), never an
  external feed; every total says `at pMint` and `usdComplete: false` when a height's price was
  unknown.
* **The counterfactual** ("a quoting pool of any size would have expected ≈ X YEC in this
  range") is Σ `fee / (|E(R)| + 1)` over the enforcement-fee outputs, with E(R) from
  `yed_getfeepayee` at each fee's `refHeight`, assuming uniform selection. Accuracy weighting
  (FEE-W) makes the realised figure for an honest quoter higher, so the number is a floor.
  `resolved`/`unresolved`/`noEligible` say how many fee outputs it covers; a `refHeight` the
  node refuses or a window nobody quoted in is counted, not guessed.
* **Releases under no enforcement.** When `enforcing` is false (sunset, valve, abandonment)
  vault collateral can be taken through the `OP_TRUE` path; `collateral_release` rows show who
  took it — the cost of not enforcing.
* A test (`tests/revenue_reconcile.rs`) asserts that Σ `enforcefee` over a block range equals
  Σ `feeZat` of the same blocks' `yed_gettxinfo` rows, likewise `attestfee`, on 37 recorded
  devnet blocks; the same sums were checked live against `/api/revenue`.

## Sessions: record and replay

```
chain-viz --devnet ~/yb-devnet --record ~/yb-devnet/viz     # appends ~/yb-devnet/viz/session.jsonl
chain-viz --replay ~/yb-devnet/viz/session.jsonl --speed 10  # no node; the same UI and API
chain-viz --replay session.jsonl --speed 0                   # as fast as possible
```

`session.jsonl` holds one event per line, exactly the `/api/events` shape. Line 1 of every run
is a header:
`{"seq":1,"ts":…,"kind":"session","version":1,"chainViz":"0.1.0","nodes":["0",…],"chain":"regtest","started":…}`
— `version` is the event schema (`SCHEMA_VERSION`), `chainViz` the binary that wrote it. The
file is opened in append mode, so restarting with the same `--record` directory continues it
with a new header and `seq` restarting at 1; replay treats each header as a run boundary and
waits no gap across it. A line that is not an event (a torn last line after a crash) is skipped
with a warning.

Replay rebuilds the model from the events alone (`src/replay.rs`): the chain (main, side,
orphans, per-node heads, majority), the mempool, the devnet heartbeat/sim and the revenue rows
are as they were; `/api/health.replay = {file, pos, total, speed}` reports progress. What only
RPC supplies is absent or partial in replay: `yedInfo` is `{}`, `chain.tips` is empty,
`nodes[].up` is `true` for every node in the header, and a mempool tx's `present`/`firstSeen`
know only the first node that reported it. `tests/replay.rs` replays a session recorded from a
live 8-node devnet reorg and asserts the same chain, tip, orphans and event count the live API
reported. The devnet's `report` bundles the session, and `yellowback_devnet_roles.py` records
one when `CHAINVIZ_BIN` is set.

## Hosting

chain-viz binds **127.0.0.1:8480** unless told otherwise. A hosted public instance is wanted
(plan C-10); where is still the owner's call. Two ways to let others in:

**A reverse proxy** in front of the loopback instance, terminating TLS and forwarding `/ws`
as a WebSocket. nginx:

```
location / {
    proxy_pass         http://127.0.0.1:8480;
    proxy_http_version 1.1;
    proxy_set_header   Host $host;
    proxy_set_header   Upgrade $http_upgrade;
    proxy_set_header   Connection "upgrade";
    proxy_set_header   X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

Caddy: `reverse_proxy 127.0.0.1:8480` (WebSockets, `Host` and `X-Forwarded-For` come by
default). Start chain-viz as `--public --trusted-proxies 127.0.0.1` behind either: the
`X-Forwarded-For` a proxy appends is believed only from a peer named in `--trusted-proxies`,
and of its entries the last one not itself a trusted proxy is the client (nginx appends the
peer it saw; a client-supplied first entry is never reached). From any other peer the header
is ignored and the peer is the client. The proxy must pass `Host` through (the `Host $host`
line above): a browser's `/ws` upgrade is accepted only when its `Origin` host equals the
request's `Host` — or is listed in `--allow-origin` — so a page on another site cannot read
the stream.

**`--public`** for an instance that faces the network — required for a non-loopback
`--listen` (refused otherwise; `--i-know-this-is-exposed` overrides). It

* rate-limits every request per client IP (10 req/s, burst 40, then `429` with `Retry-After`;
  the client is the peer, or behind `--trusted-proxies` the one `X-Forwarded-For` names); the
  bucket table is bounded (4096, least recently seen evicted);
* redacts every string in every response that looks like a URL, a `user@host`, a `host:port`
  or a filesystem path — so no node address, credential, `--replay` file name or devnet path
  leaves the server. Node ids stay.

`--public` or not, open WebSocket connections are capped at 64 (`503` past that), inbound
WebSocket messages at 64 KiB, `/api/events` at 2000 events per call (ask again from the last
`seq` when you got exactly 2000), `/ws` refuses a cross-origin browser, and WebSocket frames
are redacted. RPC credentials are never logged and an RPC error never carries the node's
address (the client replaces it by `node <id>` before the error becomes a `note` event or a
log line); a `--nodes` URL that speaks plain `http://` to a non-loopback host logs a warning
at start (the password travels in clear: use an ssh tunnel). `tests/hardening.rs` runs the
binary with a password and a hostname that must not appear in `/api/health`, `/api/snapshot`,
`/api/events`, `/ws`, the log or an export, and checks the `429`, the `X-Forwarded-For`
rules, the `Origin` check and the refused bind.

**A static copy** for a host with no server at all:

```
chain-viz --devnet ~/yb-devnet --export ~/www/chain-viz
```

writes, every 30 s and at shutdown, `index.html`, `ui/` and `snapshot.json`, `events.json`,
`health.json` (redacted as under `--public`) plus `ui/data.js` holding the same three as one
global. The page opens from any static host (`python3 -m http.server`, S3, GitHub Pages) with
connection state `static`; `qa/ui-smoke.mjs <dir>` checks an export without a browser. Straight
from `file://` it works where the browser allows module scripts there (Firefox; not Chrome).

## The read-only guarantee

chain-viz calls only the stock read RPCs (`getbestblockhash`, `getblock`, `getblockhash`,
`getchaintips`, `getrawmempool`, `getrawtransaction`, `getmempoolinfo`, `getblocksubsidy`,
`getblockchaininfo`, …) and the read-only `yed_*` RPCs (`yed_getinfo`, `yed_getstats`,
`yed_getprice`, `yed_gethistory`, `yed_getactivation`, `yed_listminers`, `yed_gettag`,
`yed_gettxinfo`, `yed_decodepayload`, `yed_validaterawtransaction`, `yed_getblockverdict`,
`yed_listvaults`, `yed_listclaimable`, `yed_listattestors`, `yed_getfeepayee`,
`yed_getstatehash`). It never calls `yed_setquote`, `yed_addattestation`, any wallet-table
`yed_*`, `generate`, `submitblock`, `sendrawtransaction`, `getblocktemplate` or any wallet RPC
(plan §4.2). `tests/readonly_gate.rs` greps `src/` for every one of those names — code,
strings and comments — and fails the build on a match. A user who wants to act is sent to
YecWallet or the CLI.

## The RPC budget

Per node and poll: one `getbestblockhash`, one `getrawmempool true`, one `yed_getinfo`;
`getblock` once per new hash (one node claims each block before enriching it, so eight nodes
reporting the same tip cost one walk); `getchaintips` on a head move and every tenth poll;
`yed_gettag` once per block and `yed_gettxinfo` once per Yellowback transaction, both on the
one node that claimed it; `yed_gethistory` once, by the leader, at start; `yed_listvaults`
paged and only when the vault counts change; `getblocksubsidy` once per block;
`yed_getfeepayee` once per distinct `refHeight`. A ZMQ `hashtx` also wakes the mempool fetch,
once per transaction entering the mempool and once per transaction of a connected block.
`/api/health.rpcCalls` counts every method per node; `tests/yb_budget.rs` asserts the counts on
a recorded run, and `qa/soak.sh <url> <pid> <minutes>` samples RSS, `seq` and the RPC rate of a
running instance and fails on a rising RSS, an uneven rate or RPC-failure notes.

## CI

`.github/workflows/ci.yml`: `cargo fmt --check`, build, test and `clippy -D warnings` on
Ubuntu and macOS with the pinned Rust release on every push and pull request, plus
`cargo deny check` (`deny.toml`: RustSec advisories and yanked crates, licence allow-list,
crates.io as the only source) and `cargo audit`; every action is pinned to a commit SHA and
the workflow token is read-only except in the release job. A `v*` tag also builds
release binaries for linux x86_64 and macOS arm64 and attaches them to the GitHub release. The
node's nightly (`ycash-dd/.github/workflows/yellowback-tests.yml`) checks out and builds this
repository and runs `qa/rpc-tests/yellowback_chainviz.py` against it on a 3-node regtest.

## Findings

Every trap met while building chain-viz — node quirks, devnet facts, design corrections — is a
`C-F` row in the plan's §9 (`docs/plans/chain-viz-plan.md`), and the ones that are facts about
the node or the devnet are mirrored in `docs/mapping.md` §18.
