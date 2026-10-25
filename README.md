# chain-viz

chain-viz is a **read-only sidecar of `ycashd`** that shows, live, what a Ycash chain is doing
across many nodes at once: per-node tips and their disagreement, the block DAG with side
branches, reorgs and orphans as they happen, and the mempool with first-seen per node. On nodes
that run Yellowback it also shows the Yellowback state, the vaults and signer sets under it, and
a **revenue ledger** that attributes every pool fee and attestor fee to the key that received
it, so a pool operator or a prospective attestor can see what participating pays. It never
holds a key, never broadcasts and never calls an RPC that writes; a CI test enforces that. One
static binary, an embedded UI, no build step for the page.

## Ycash vaults, Yellowback and YED

chain-viz is one component of the [Ycash vaults and Yellowback](https://github.com/boyfromcave/yellowback)
project: a proposed Ycash network upgrade that adds **post-quantum vaults**, YEC locked on chain
under rules every node checks, released only with the approval of a bonded **signer set**, after a
delay during which the release can be cancelled, and always recoverable by its owner if the signers
go silent. **Ycash Yellowback (YED)** is a dollar backed by YEC in vaults (`1 YED = $1`); its
YEC/USD price comes from mining pools and from **attestors**, the bonded members of Yellowback's
signer set who sign prices.

> **Status: in development.** The upgrade runs on regtest and on a one-laptop devnet only. No
> activation height is set on mainnet or testnet, the upgrade has not been adopted by the Ycash
> Foundation, and there is no external audit. So the Yellowback and vault panels show data on a
> local test network only.

- **What chain-viz shows:** YED prices, supply, collateral, vaults, claims and each Yellowback
  transaction's verdict before a pool includes it; the vault upgrade itself (signer sets, vaults
  and pending releases of every application, by tag); and:
  - **Fees:** the protocol fee bands (in dollars of the debt, at each vault's mint price `pMint`),
    the network fee in dollars per KB at the attested price with its floor and cap, the maximum
    mint and the supply cap, and what pools and attestors actually earn.
  - **Attestors:** each member's bond and seat weight, evicted members, the bond minimum against
    its floor and cap, and when a departing member's bond unlocks after the exit delay.
  - **Post-quantum:** each vault owner's scheme (SLH-DSA) and 53-character address, Falcon and
    SLH-DSA signature bytes per block, and post-quantum payees by their `sq…` (plain YEC) address.
- **Node it needs:** any `ycashd` v4.5.0 or 6.20.0 for the chain and mempool views; for the
  Yellowback and vault panels, a `ycashd` built from the `upgrade/quantum-vaults` branch of
  [ycash-dd](https://github.com/boyfromcave/ycash-dd) or
  [ycash6](https://github.com/boyfromcave/ycash6) (this line joins `upgrade/vault` at release),
  speaking Yellowback `rpcversion` 8 (7 and 6 are still modelled; see [rpcversion](#rpcversion)).
- **Try it:** the node's devnet starts chain-viz for you; see
  [Run against the devnet](#run-against-the-devnet).

![chain-viz on an 11-node devnet](docs/screenshot.png)

## Install and build

Rust 1.91.0 (`rust-toolchain.toml` pins the exact release; rustup fetches it), no system
dependencies (rustls for HTTP, pure-Rust ZMQ):

```
cargo build --release                                   # target/release/chain-viz
cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked   # what CI runs
cargo deny --locked check                               # advisories, licences, sources (CI too)
node qa/ui-smoke.mjs http://127.0.0.1:8480              # the UI panels under a fake DOM, against a live instance
```

Tagged releases (`v*`) carry linux x86_64 and macOS arm64 binaries (see CI below). If several
checkouts share `$CARGO_TARGET_DIR`, build a binary you are about to run under a private
target dir — another build replacing the file while it runs leaves the process stuck.

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

Never invalidate more than 99 blocks deep. Both node versions refuse a reorg
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

* for the Yellowback and vault panels, a node built from `upgrade/quantum-vaults` with the upgrade and the
  network's YED attestor set configured (no enabling flag; on regtest
  `-nuparams=6d5b7a31:<h> -yellowbackattestorset=<setid>`). A node without the `yed_*` RPCs is
  watched as chain + mempool only (`nodes[].yellowback = false`) and still counts in the
  tip-agreement view.
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
| `--yolo <URL[,URL…]>`, `--lightwalletd <URL>` | Parsed and ignored with a warning: the pool and lightwalletd lanes are not implemented. |

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
  momentarily exceed `nodesUp`: wait for both when scripting.
* **`GET /api/snapshot`** — the whole model, what the UI loads on open:
  `{version, schema, seq, ts, chainName, nodes[{id,role,sources,up,error,yellowback,lastSeen}],
  chain{heads, majority, main[≤200 BlockInfo, newest last], side[], tips, blockCount},
  mempool{txs[{txid,size,fee,time,firstSeen,present,depends,yb?}], count, bytes, feeTotal, nodes},
  yedInfo{<node>: yed_getinfo}, yellowback{…}, revenue{totals, byPayee, aliases, window},
  devnet{heartbeat, sim}, vault{…as /api/vaults}}`. `BlockInfo` is `{hash, height, prev, time, txCount, size,
  chainwork, status: main|side|orphaned, nodes, seen, txids?, yb?{tag, miner, rejected, txs}}`.
* **`GET /api/events?since=<seq>`** — the event log from a sequence number, oldest first,
  each `{seq, ts, height?, node?, kind, …}`. Kinds (schema v1, `src/events.rs`): `session`,
  `block`, `block_side`, `orphaned`, `reorg{depth,from,to,toHeight}`, `tip`, `mempool_add`,
  `mempool_remove`, `yb_tx{txid,type,verdict,feeZat,payee,…}`, `yb_state{field,from,to}`,
  `price`, `stats`, `statehash_mismatch`, `attestor`, `vault`, `revenue{entry,zat,payee,usd}`,
  `rejected_block{hash,verdict}` (a side tip `getchaintips` calls `invalid` whose
  `yed_getblockverdict` says `blockInvalid`), `vault_spend{outpoint,kind,action,selector,txid,
  height,returnedTo,inferred,tag,…}`, `set_act{txid,height,type,setid}`,
  `set_state{setid,member,field,from,to}`, `devnet_heartbeat`, `devnet_sim`, `note{text}`.
* **`WS /ws`** — the same events pushed. First frame `{"kind":"hello","seq","version"}`; on a
  slow client `{"kind":"lagged","dropped","seq"}`, after which resync via `/api/events?since=`.
* **`GET /api/yellowback`** — the health panel's slice, fetched once per block:
  `{seq, tip, yedInfo, yellowback{leader, stats{<node>: yed_getstats}, statehash{<node>},
  statehash_agree, price, attestPrices, activation (yed_getactivation: the upgrade object), haltBits, miners[], attestors[], vaults[],
  claimable[], history[], historyFrom, txs[], rejected[], mockPrice,
  noPrice{window, fromHeight, toHeight, covered, blocks, hours}},
  blocks[{hash,height,time,txCount,yb}]}`.
* **`GET /api/revenue?from=<h>&to=<h>&by=payoutKey|attestor|block`** — the ledger rolled up
  over `[from, to]` (default: the whole kept window):
  `{from, to, by, window{from,to}, priceLabel: "at pMint",
  totals{subsidy|subsidy_other|netfee|enforcefee|attestfee|residual: {zat, usd, usdComplete}, blocks, ybTxs},
  groups[…], counterfactual{zat, usd, feeOutputs, resolved, unresolved, noEligible, label, method},
  rows[≤5000 {height,txid,vout,kind,zat,payee,usd,refHeight?}],
  rowsTruncated, seq, tip, pMintNow}`. A `payoutKey` group is
  `{payee, aliases, blocksMined, tags, timesSelected, enforcefee, subsidy, netfee,
  stockCoinbase, perTagZat, perBlockZat, miner}`; an `attestor` group carries the bond and the
  realised yield; a `block` group the rows of that height.
* **`GET /api/vaults`** — the vault primitive (`src/model/vault.rs`), refreshed once per block:
  `{available, info: vault_getinfo{branchid, activationheight, active, height, dbtip, sets,
  vaults, intents, lockedvalue, statehash}, attestorSetId, nextHeight, sets[set_getinfo + rate{limited,
  limit, used, basis, usedBps} + yed], vaults[vault_list V rows], intents[vault_list I rows +
  blocksToRelease], byTag[{tag, tagtext, vaults, intents, valuezat}], spends[≤200], acts[≤200],
  seq, tip, leader}`. `rate.limit` = `epochbasis × ratelimitbps / 10000`; `blocksToRelease` =
  `matureheight − nextHeight`; `yed` marks the set `yed_getinfo.upgrade.attestorSetId` names.

## The panels

**Header** — chain name, nodes up, majority tip, connection state, and the time since the last
block, large, coloured past 2× and 4× the target spacing (the devnet heartbeat's rate on
regtest, else Ycash's 75 s).

**Chain** — the block DAG newest right: the main row on top, side and orphaned blocks hanging
below their fork point; each block shows height, short hash, tx count, the tag's price and
the quoting `payoutKey`, and a red edge if a node refused it as YED-invalid. One chip per
node with its tip; chips off the majority are highlighted — this is the fork-risk indicator,
and it is why chain-viz watches many nodes at once. Below: a reorg log (`reorg` is per node,
so one devnet reorg is one row per node) and three **derived** gauges, labelled as such:
*fork* = disagreeing nodes × seconds disagreeing (clocked from when the page first saw it),
*orphan* = extra blocks at duplicate heights in the last 20, *reorg* = deepest side branch,
on a scale of 6 blocks.

**Mempool** — bubbles by fee rate × time in mempool (radius ~ size). Grey for ordinary
transactions; coloured by type for Yellowback ones (`mint`, `transfer`, `redeem`, claim =
`redeem` with path `claim`, `register`, `notice`, `equivocation`, `revive`). A Yellowback
transaction that the node **would reject** at the current tip
(`yed_validaterawtransaction`) is shown red before any pool includes it. Tiles: count, bytes,
fees, median age, transactions older than 2 blocks (a pool filtering it, or a fee below
policy), *partial* = present on some nodes but not all (propagation, or a stock node refusing
what it does not understand).

**Yellowback health** — a state ribbon (`healthy`, `mintRequiresArmed`, a warning pill when a node reports an `rpcversion` other than 6, 7 or 8 or the nodes mix them, the post-quantum parameters (`params.pq`: owner schemes, the Falcon height, `feeRateZatPerKB`; rpcversion 7), the fee structure (`params.fees`: the protocol fee bands of the debt at `pMint` with the floor and attestor share, the network $/KB rates at the attested tip price with the floor and cap; `params.maxMintCents`; `params.supplyCap` with the per-vault share; rpcversion 8), the claim rule (`claims IN TERM` when `params.inTermClaims`, θ `claimThresholdBps`, the early-redeem fee `earlyRedeemFeeBps` per class A/B/C — of the debt at `pMint` from rpcversion 8, of the collateral before — the claim delay), the vault
upgrade from `yed_getinfo.upgrade` (status, branch ID, activation height, claim delay, the YED
attestor set), arming `UNARMED → TRIGGERED → ARMED`, minting with its reason — a halt-mask bit, or
`UNARMED` when `mintRequiresArmed` holds and the tip is not ARMED (no bit is set then; the node
reports `mintingAllowed` false and `mintableClasses` empty) — and a NO_PRICE-hours counter: the
blocks with the NO_PRICE bit among the timeline's last 1,152 (one day at 75 s), as hours at the
target spacing, with how many of those heights the timeline holds); a timeline by height with lanes for the upgrade (active from its activation height), arming,
the four halt-mask bits rpcversion 5 can set and refused-block ticks, backfilled from `yed_gethistory`; prices (`pFast`/`pMid`/`pSlow`, the
`pMint`–`pClaim` band, the fed price dotted on the devnet, window fill bars); supply and
collateral tiles with the global ratio against the node's claim threshold θ and emergency lines (125 % and
105 % on the in-term line); a vault scatter (ratio × blocks to `lockHeight` when claims are valid in term, to
`claimHeight` otherwise; vaults claimable now ringed) and a vault table (ratio, the claimable-at price
`underwaterAt`, and `CLAIMABLE now` or `listed, above θ` for a `yed_listclaimable` row with `claimable: false`,
and on rpcversion 7 the owner: its post-quantum scheme and 53-character `ownerAddress`, a vault identifier, never a YED receive address;
with the per-vault cancel limit (CAN-2, CAN-3; additive, `rpcversion` unchanged) a `cancel needs` column, read-only: `cancelsRequired`
co-signers of distinct current attestor-set members for a cancel in the next block, `cancelCount` and `cancelQuietUntil` once
the vault was cancelled, and how many owner residuals the position holds (`heldResiduals`, released at close, or from
`cancelQuietUntil` while no claim could pass), the outpoints on hover; a `yed_listclaimable` row's own cancel-limit fields are
preferred when the node sends them, ycash-dd/ycash6 `fix/cancel-fields-2`);
the attestor table (on rpcversion 8 with the attestor rules: the seat weight `seatWeight` ÷ 65,536 in YEC, an
`evicted` mark on a WITHDRAWN record evicted from the full set, and the bond exit: a WITHDRAWN record whose bond is
not frozen or spent unlocks at `statusHeight + bondMaturity`, not before its `bondLocktime`), with the ribbon's
`attestors` state (the computed `bondMinZat` against its floor and cap, the seats and margins, the exit delay); state-hash
agreement per node and the refused blocks with their verdicts; the Yellowback
transaction table (a refused mint's verdict, e.g. `mint-halted-unarmed`, carries an explanation
on hover; `claim_release`, `claim_cancel` and `set_act` rows are the claim intent's two ends and
an attestor's set acts); vault tiles with `CLAIMING` (a claim intent pending) and, only when an
older VOID vault is listed, the void count.

**Vault primitive** — tiles from `vault_getinfo` (upgrade active and from which height, branch
ID, sets, vaults, intents, locked YEC, state hash); the signer sets (`set_list`, and
`set_getinfo` for the members of up to 16 sets) with seats, thresholds, open/admitted,
current/active members, `dormant`/`released`, wind-down, and the rate epoch as a bar
(unlocked this epoch against `epochbasis × ratelimitbps`); each set's member table (status,
current, live, last act, bond, frozen, and for a member that left unfrozen the bond exit `exit + maturity`:
`set_getinfo` reports no exit height, so no number is shown); the YED attestor set starred; vaults and intents by
application tag; pending intents with blocks to release and whether a cancel can still land;
the spends of every V and I output in the blocks chain-viz fetched, from the spending input's
selector (V: unlock / owner / owner-released / app; I: release / **cancel** / owner-released),
a cancel showing the vault output its value went back to; and the `YV` set acts. An outpoint
that leaves `vault_list` without a spend seen in a fetched block (chain-viz was not running, or
the block was walked before the outpoint was listed) is classified from the listing alone and
marked *inferred*: an intent replaced by a vault with its `origin` script before maturity is a
cancel, one gone at or after maturity a release. Two ratios on this panel are priced differently by the node on purpose:
the global-ratio tile is at `pMint`, the scatter at `pClaim`, and they disagree for about one
slow window after a price move.

**Revenue** — see the next section.

**Events** — the tail of the event stream, newest first, filterable by kind.

## What the revenue numbers are, and are not

The ledger (`src/model/revenue.rs`) is a list of attributed outputs, one row per
`(height, txid, vout, kind, zat, payee)`, built from the coinbase and from `yed_gettxinfo` of
every Yellowback transaction in a block. It is **attributed, not estimated**:

* **Pool fees go to a quoting key, not to the block winner.** `enforcefee` rows carry
  `yed_gettxinfo.payee` — one key in E(R), the set of keys with a quote tag in the last 100
  blocks, chosen by the transaction builder. A pool with 5 % of hashpower that quotes every
  block is in E(R) for every fee on the network; a pool with 40 % that does not quote earns
  none. The per-pool table shows blocks mined, tags published, times selected, fees earned,
  fee per tag and per block, beside the stock coinbase — the marginal revenue of quoting.
* **Stock coinbase.** `subsidy` and `netfee` are the miner's coinbase outputs (net fees =
  coinbase value − `getblocksubsidy.miner`); `subsidy_other` is what the coinbase pays to
  someone else (regtest's founders output), matched by `getblocksubsidy.foundersaddress` on
  v4.5.0 and, on ycashd 6.20.0, which does not report it, by the output equal to `founders`
  (a tie goes to the fund address last attributed). A pool's coinbase address is aliased to the
  `payoutKey` of the tag on the same block, so its blocks roll up under its quoting key;
  a block without a tag stays under its coinbase address.
* **Attestor fees** (`attestfee`, once ARMED a share of the pool fee: 25 % on regtest, 50 % in
  the launch parameters) go to the bond key
  in `attestPayee`. The per-attestor yield is fees ÷ bond over the range, **realised, not
  promised**.
* **USD figures are at `pMint`** — the protocol's own mint price at that row's height
  (`yed_gethistory[].pMint`, or the current `yed_getstats.pMint` for the mempool), never an
  external feed; every total says `at pMint` and `usdComplete: false` when a height's price was
  unknown.
* **The counterfactual** ("a quoting pool of any size would have expected ≈ X YEC in this
  range") is Σ `fee / (|E(R)| + 1)` over the pool-fee outputs, with E(R) from
  `yed_getfeepayee` at each fee's `refHeight`, assuming uniform selection. Accuracy weighting makes the realised figure for an honest quoter higher, so the number is a floor.
  `resolved`/`unresolved`/`noEligible` say how many fee outputs it covers; a `refHeight` the
  node refuses or a window nobody quoted in is counted, not guessed.
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

chain-viz binds **127.0.0.1:8480** unless told otherwise. There is no hosted public instance
yet. Two ways to let others in:

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
`yed_getstatehash`) and the vault primitive's read RPCs (`vault_getinfo`, `set_list`,
`set_getinfo`, `vault_list`; `vault_decodescript` is wrapped but the panel decodes acts and
selectors itself from `getblock … 2`). It never calls `yed_setquote`, `yed_addattestation`, any wallet-table
`yed_*`, `generate`, `submitblock`, `sendrawtransaction`, `getblocktemplate` or any wallet RPC.
`tests/readonly_gate.rs` greps `src/` for every one of those names — code,
strings and comments — and fails the build on a match. A user who wants to act is sent to
YecWallet or the CLI.

## rpcversion

chain-viz models `yed_*` at **rpcversion 8** on this branch (`upgrade/quantum-vaults`, the post-quantum line with
the fee structure; `KNOWN_RPC_VERSION` in `src/rpc.rs`) and at 7 and 6 (`MODELLED_RPC_VERSIONS`). rpcversion 8 (the
workspace's `docs/plans/yellowback-quantum-fees-spec.md`; ycash-dd `doc/yellowback-rpc.md`, "The fee structure"):
the pool fee is a protocol fee in dollars of the debt by band at the vault's stored `pMint` (`params.fees.protocol`),
network fees are dollars per KB at the attested tip price (`params.fees.network`), `params.maxMintCents` and
`params.supplyCap` are new and every vault row carries `pMint` (the health panel's vault table and scatter show it);
`params.feeMinZat` / `params.feeBps` are gone, and `yed_getfeepayee` takes no `collateralZat` (the collector passes
it only to a node below 8, `feepayee_args`). rpcversion 7 (the workspace's
`docs/plans/yellowback-quantum-rpc-delta.md`): every vault owner is a post-quantum key (`ownerScheme`, `ownerHash`
and a 53-character `ownerAddress`; `ownerPubKey` / `ownerKeyId` are gone), a MINT's payload is version 4 (owner =
scheme ‖ key hash at payload offsets 17..49; every other type stays version 3, and `src/classify.rs` accepts a MINT
at either version, so it observes both lines), `yed_getinfo.params.pq` carries the schemes, the Falcon height and
the size-priced fee rate; the vault primitive lists `owner` (a pqkeyid) and `ownerscheme`, to which `/api/vaults`
adds `owneraddress` and `ownerschemename`. A `TX_PQPKH` output (`20 <keyHash> 51|52 c2`) is a payee by its plain-YEC
PQ address (`sq…`/`tq…`/`rq…`, what `getblock` spells), a `TX_PQCHANNEL` output by `pqchannel:` and its client's;
an input carrying the chunked PQ signature stack counts toward a block's `pqBytes` (its share of the block is in the
block tooltip) and an owner spend's `pqscheme` (`src/pq.rs`). rpcversion 6 is the in-term claims line
(`upgrade/vault-in-term`; rpcversion 5 is the vault upgrade): `yed_listclaimable` lists every
ACTIVE vault whose claim branch is open, in term too, and a row with `claimable: false` is listed but not
claimable (above θ; its `underwaterAt` is the price at which it becomes so; a row without the field, from an
rpcversion-5 node, is claimable), `yed_getinfo.params` carries `inTermClaims`, `claimThresholdBps` and
`earlyRedeemFeeBps`. From rpcversion 5: `yed_getinfo` with `upgrade`,
`yed_getactivation` as the upgrade object, `yed_getblockverdict` with `verdict`. Unlike the
wallets, which refuse a node whose `rpcversion` is not exactly theirs (they would build
transactions from shapes they do not know), chain-viz is an observer and never refuses a node:
an older node still parses (fields chain-viz does not model land in `extra`; it has no
`upgrade`), and the
health ribbon shows a warning pill naming the versions when any node is at neither 6 nor 7, or the nodes mix them.

## The RPC budget

Per node and poll: one `getbestblockhash`, one `getrawmempool true`, one `yed_getinfo`;
`getblock` once per new hash (one node claims each block before enriching it, so eight nodes
reporting the same tip cost one walk); `getchaintips` on a head move and every tenth poll;
`yed_gettag` once per block and `yed_gettxinfo` once per Yellowback transaction, both on the
one node that claimed it; `yed_gethistory` once, by the leader, at start; `yed_listvaults`
paged and only when the vault counts change; `getblocksubsidy` once per block;
`yed_getfeepayee` once per distinct `refHeight`; on the vault leader (the lowest-id node that
answers `vault_getinfo`), per block: one `vault_getinfo`, one `set_list`, one `set_getinfo` per
set (at most 16), and `vault_list` only when `vault_getinfo.statehash` moved. A ZMQ `hashtx` also wakes the mempool fetch,
once per transaction entering the mempool and once per transaction of a connected block.
`/api/health.rpcCalls` counts every method per node; `tests/yb_budget.rs` asserts the counts on
a recorded run, and `qa/soak.sh <url> <pid> <minutes>` samples RSS, `seq` and the RPC rate of a
running instance and fails on a rising RSS, an uneven rate or RPC-failure notes.

## CI

`.github/workflows/ci.yml`: `cargo fmt --check`, build, test and `clippy -D warnings` on
Ubuntu and macOS with the pinned Rust release on every push and pull request, plus
`cargo deny check` (`deny.toml`: RustSec advisories and yanked crates, licence allow-list,
crates.io as the only source) and `cargo audit`; every action is pinned to a commit SHA and
the workflow token is read-only. A `v*` tag runs `.github/workflows/release.yml`, which builds
release binaries for linux x86_64 and macOS arm64 and attaches them to the GitHub release (only
its publish job can write). A dispatch with `tier=nightly` adds the `devnet` job: a five-node
regtest devnet of the node line matching the branch, with chain-viz and the UI smoke
(`qa/ui-smoke.mjs`); its `ycashd` comes only from a push, schedule or dispatch run of that node
branch's `yellowback-tests` (never a pull-request run), and it saves no cache. Security scanning
is in [SECURITY-CI.md](SECURITY-CI.md). The node's nightly
(`ycash-dd/.github/workflows/yellowback-tests.yml`) checks out and builds this repository at a
pinned commit and runs `qa/rpc-tests/yellowback_chainviz.py` against it on a 3-node regtest.

## Findings

Every trap met while building chain-viz (node quirks, devnet facts, design corrections) is
recorded in the plan (`docs/plans/chain-viz-plan.md`) in the project workspace,
`yellowback-workspace`.
