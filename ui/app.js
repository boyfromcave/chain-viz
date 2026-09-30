// chain-viz UI: one store, one render loop, panels as modules. Loads /api/snapshot, opens /ws,
// applies events incrementally, and resyncs from /api/events?since= after a lag or a reconnect
// (exponential backoff). Chain topology events additionally schedule a debounced snapshot
// refresh, because the server's DAG (majority head, side statuses) is authoritative.
import { now } from './lib.js';
import * as header from './panels/header.js';
import * as chain from './panels/chain.js';
import * as mempool from './panels/mempool.js';
import * as events from './panels/events.js';
import * as health from './panels/health.js';
import * as revenue from './panels/revenue.js';

export const store = {
  snap: null,            // last /api/snapshot (chain, mempool, nodes, devnet, yedInfo)
  seq: 0,                // last applied event seq
  conn: 'connecting',    // connecting | open | reconnecting | closed
  events: [],            // tail of the event stream (newest last), ≤ MAX_EVENTS
  reorgs: [],            // reorg events, newest first
  rejected: new Set(),   // block hashes some node rejected
  disagreeSince: {},     // node id → wall-clock when it last started disagreeing
  yb: {},                // txid → yb fields seen in yb_tx events (mempool colouring)
  dirty: true,
};
const MAX_EVENTS = 500;
const panels = [header, chain, mempool, events, health, revenue];

// ---------------------------------------------------------------- incremental apply
function findBlock(hash) {
  const c = store.snap?.chain; if (!c) return null;
  return c.main.find((b) => b.hash === hash) || c.side.find((b) => b.hash === hash) || null;
}

export function applyEvent(e, historical = false) {
  if (!historical && e.seq && e.seq <= store.seq) return;      // duplicate from a resync overlap
  if (!historical && e.seq) store.seq = e.seq;
  store.events.push(e);
  if (store.events.length > MAX_EVENTS) store.events.splice(0, store.events.length - MAX_EVENTS);
  if (historical) {   // the server's backlog from before this page opened: logs only, the snapshot already reflects it
    if (e.kind === 'reorg') store.reorgs.push(e);
    if (e.kind === 'rejected_block') store.rejected.add(e.hash);
    if (e.kind === 'yb_tx') store.yb[e.txid] = { type: e.type, verdict: e.verdict, feeZat: e.feeZat, payee: e.payee };
    store.dirty = true;
    return;
  }
  const s = store.snap;
  if (!s) return;
  switch (e.kind) {
    case 'tip':
      if (e.node) s.chain.heads[e.node] = { height: e.height, hash: e.hash };
      touchNode(e.node, e.ts);
      break;
    case 'block': {
      const last = s.chain.main[s.chain.main.length - 1];
      if (!findBlock(e.hash) && last && e.prev === last.hash) {
        s.chain.main.push({ hash: e.hash, height: e.height, prev: e.prev, time: e.time, txCount: e.txCount, size: e.size,
          chainwork: e.chainwork, status: 'main', nodes: e.nodes || [], seen: e.ts, yb: e.yb, miner: e.miner, tag: e.tag });
        if (s.chain.main.length > 200) s.chain.main.shift();
      } else scheduleRefresh();
      break;
    }
    case 'block_side':
      if (!findBlock(e.hash)) s.chain.side.push({ hash: e.hash, height: e.height, prev: e.prev, time: e.time, txCount: e.txCount,
        size: e.size, chainwork: e.chainwork, status: 'side', nodes: e.nodes || [], seen: e.ts, sideStatus: e.status, branchlen: e.branchlen });
      scheduleRefresh();
      break;
    case 'orphaned': { const b = findBlock(e.hash); if (b) b.status = 'orphaned'; scheduleRefresh(); break; }
    case 'reorg': store.reorgs.unshift(e); if (store.reorgs.length > 100) store.reorgs.pop(); scheduleRefresh(); break;
    case 'rejected_block': store.rejected.add(e.hash); { const b = findBlock(e.hash); if (b) b.rejected = true; } break;
    case 'mempool_add': {
      let t = s.mempool.txs.find((x) => x.txid === e.txid);
      if (!t) { t = { txid: e.txid, size: e.size, fee: e.fee, time: e.time, firstSeen: {}, present: [], depends: e.depends || [] }; s.mempool.txs.push(t); }
      if (e.node) { t.firstSeen[e.node] ??= e.ts; if (!t.present.includes(e.node)) t.present.push(e.node); }
      if (store.yb[e.txid]) t.yb = store.yb[e.txid];
      recountMempool(); scheduleRefresh();
      break;
    }
    case 'mempool_remove': {   // emitted once, when no node holds the tx any more
      const i = s.mempool.txs.findIndex((x) => x.txid === e.txid);
      if (i >= 0) s.mempool.txs.splice(i, 1);
      recountMempool(); scheduleRefresh();   // per-node presence only changes in the snapshot
      break;
    }
    case 'yb_tx': {
      store.yb[e.txid] = { type: e.type, verdict: e.verdict, feeZat: e.feeZat, payee: e.payee };
      const t = s.mempool.txs.find((x) => x.txid === e.txid); if (t) t.yb = store.yb[e.txid];
      break;
    }
    case 'devnet_heartbeat': s.devnet.heartbeat = stripEnvelope(e); break;
    case 'devnet_sim': s.devnet.sim = stripEnvelope(e); break;
    case 'note': if (e.node) touchNode(e.node, e.ts, /went away|unreachable|down|error/i.test(e.text) ? false : undefined); break;
    default: break;
  }
  store.dirty = true;
}

function stripEnvelope(e) { const v = { ...e }; delete v.seq; delete v.ts; delete v.kind; delete v.node; delete v.height; return v; }
function touchNode(id, ts, up = true) {
  const n = store.snap.nodes.find((x) => x.id === id); if (!n) return;
  n.lastSeen = ts; if (up !== undefined) n.up = up;
}
function recountMempool() {
  const m = store.snap.mempool;
  m.count = m.txs.length; m.bytes = m.txs.reduce((a, t) => a + (t.size || 0), 0); m.feeTotal = m.txs.reduce((a, t) => a + (t.fee || 0), 0);
}

// ---------------------------------------------------------------- derived: majority head
export function majority() {
  const heads = store.snap?.chain?.heads || {};
  const byHash = new Map();
  for (const [id, hd] of Object.entries(heads)) {
    const e = byHash.get(hd.hash) || { hash: hd.hash, height: hd.height, nodes: [] };
    e.nodes.push(id); byHash.set(hd.hash, e);
  }
  let best = null;
  for (const e of byHash.values()) if (!best || e.nodes.length > best.nodes.length || (e.nodes.length === best.nodes.length && e.height > best.height)) best = e;
  if (!best) return null;
  best.disagreeing = Object.keys(heads).filter((id) => heads[id].hash !== best.hash);
  const t = now();
  for (const id of Object.keys(heads)) {
    if (heads[id].hash !== best.hash) store.disagreeSince[id] ??= t; else delete store.disagreeSince[id];
  }
  return best;
}

// ---------------------------------------------------------------- transport
let refreshTimer = null;
function scheduleRefresh() { if (!refreshTimer) refreshTimer = setTimeout(() => { refreshTimer = null; loadSnapshot(); }, 400); }

async function loadSnapshot() {
  const r = await fetch('/api/snapshot'); if (!r.ok) throw new Error(`snapshot ${r.status}`);
  const snap = await r.json();
  for (const b of [...snap.chain.main, ...snap.chain.side]) if (store.rejected.has(b.hash)) b.rejected = true;
  for (const t of snap.mempool.txs) if (!t.yb && store.yb[t.txid]) t.yb = store.yb[t.txid];
  store.snap = snap;
  if (snap.seq > store.seq) store.seq = snap.seq;
  store.dirty = true;
}

let backlogDone = false;
async function resync() {
  if (!backlogDone) {   // once: whatever the server's bounded event log still holds (reorgs before the page opened)
    backlogDone = true;
    const r = await fetch('/api/events?since=0');
    if (r.ok) {
      const list = (await r.json()).filter((e) => e.seq <= store.seq);
      for (const e of list) applyEvent(e, true);
      store.reorgs.sort((a, b) => b.seq - a.seq);
    }
  }
  const r = await fetch(`/api/events?since=${store.seq}`); if (!r.ok) throw new Error(`events ${r.status}`);
  const list = await r.json();
  for (const e of list) applyEvent(e);
  await loadSnapshot();
}

let backoff = 500;
function connect() {
  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
  ws.onopen = () => { store.conn = 'open'; backoff = 500; store.dirty = true; };
  ws.onmessage = (m) => {
    const e = JSON.parse(m.data);
    if (e.kind === 'hello' || e.kind === 'lagged') { resync().catch(console.error); return; }
    applyEvent(e);
  };
  ws.onclose = () => {
    store.conn = 'reconnecting'; store.dirty = true;
    setTimeout(connect, backoff); backoff = Math.min(backoff * 2, 15000);
  };
  ws.onerror = () => ws.close();
}

// ---------------------------------------------------------------- render loop
function render() {
  const ctx = { store, majority: majority(), now: now() };
  for (const p of panels) p.render(ctx);
  store.dirty = false;
}
let last = 0;
function loop(t) {
  if (store.dirty || t - last > 1000) { render(); last = t; }   // the clocks tick once a second
  requestAnimationFrame(loop);
}

for (const p of panels) p.init?.(store);
loadSnapshot().catch((e) => { store.conn = 'closed'; console.error(e); }).finally(() => { connect(); requestAnimationFrame(loop); });
