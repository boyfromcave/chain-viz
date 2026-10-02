// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

// Events panel: the tail of the event stream, newest first, filterable by kind.
import { h, clear, fmtTime } from '../lib.js';

const $ = (id) => document.getElementById(id);
let filter = '', known = new Set(), lastSeq = -1, lastFilter = null;

export function init(store) {
  $('event-filter').addEventListener('change', (e) => { filter = e.target.value; store.dirty = true; });
}

function summary(e) {
  const rest = { ...e }; for (const k of ['seq', 'ts', 'kind', 'node', 'height']) delete rest[k];
  switch (e.kind) {
    case 'block': return `${e.hash.slice(-8)} ${e.txCount} tx ${e.size} B`;
    case 'block_side': return `${e.hash.slice(-8)} ${e.status} branchlen ${e.branchlen}`;
    case 'orphaned': return e.hash.slice(-8);
    case 'reorg': return `depth ${e.depth} ${e.from.slice(-8)} → ${e.to.slice(-8)} @${e.toHeight}`;
    case 'tip': return e.hash.slice(-8);
    case 'mempool_add': return `${e.txid.slice(0, 10)}… ${e.size} B fee ${e.fee}`;
    case 'mempool_remove': return `${e.txid.slice(0, 10)}… ${e.reason}`;
    case 'yb_tx': return `${e.type} ${e.verdict} ${e.txid.slice(0, 10)}…`;
    case 'yb_state': return `${e.field}: ${JSON.stringify(e.from)} → ${JSON.stringify(e.to)}`;
    case 'note': return e.text;
    case 'revenue': return `${e.entry} ${(e.zat / 1e8).toFixed(4)} YEC → ${(e.payee || '').slice(0, 10)}…${e.usd !== undefined ? ' $' + Number(e.usd).toFixed(2) : ''}`;
    default: { const s = JSON.stringify(rest); return s.length > 120 ? s.slice(0, 117) + '…' : s; }
  }
}

export function render({ store }) {
  const sel = $('event-filter');
  for (const e of store.events) if (!known.has(e.kind)) { known.add(e.kind); sel.append(h('option', { value: e.kind }, e.kind)); }
  const seq = store.events.length ? store.events[store.events.length - 1].seq : 0;
  if (seq === lastSeq && filter === lastFilter) return;
  lastSeq = seq; lastFilter = filter;
  const el = clear($('event-log'));
  const list = store.events.filter((e) => !filter || e.kind === filter).slice(-200).reverse();
  for (const e of list) {
    el.append(h('li', {}, h('span', { class: 't' }, `${String(e.seq).padStart(5)} ${fmtTime(e.ts)} `), h('span', { class: 'k' }, e.kind),
      h('span', { class: 'n' }, e.node !== undefined ? ` node${e.node}` : ''), e.height !== undefined ? ` @${e.height} ` : ' ', summary(e)));
  }
}
