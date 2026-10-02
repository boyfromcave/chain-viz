// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

// Chain panel (plan §3.2.1): block DAG newest-right (main row on top, side/orphaned blocks hanging
// below their fork point), per-node head chips with disagreement highlighted, reorg log, and the
// three derived risk gauges: fork = disagreeing nodes × seconds disagreeing; orphan = extra blocks
// at duplicate heights in the last window; reorg = deepest side branch within the work-valve depth (6).
import { h, svg, clear, short, fmtInt, fmtBytes, fmtSecs, fmtTime, tooltip } from '../lib.js';
import { spacing } from './header.js';

const COL = 104, ROW = 74, BW = 92, BH = 58, PAD = 12;
const WINDOW = 20;            // heights considered by the orphan gauge
const VALVE_DEPTH = 6;        // index.cpp:575-600, the work-valve depth
const $ = (id) => document.getElementById(id);
let lastKey = '';

export function render(ctx) {
  renderChips(ctx);
  renderDag(ctx);
  renderGauges(ctx);
  renderReorgs(ctx);
}

function renderChips({ store, majority }) {
  const s = store.snap; if (!s) return;
  const el = clear($('heads'));
  for (const n of s.nodes) {
    const hd = s.chain.heads[n.id];
    const agree = !hd || !majority ? 'unknown' : hd.hash === majority.hash ? 'yes' : 'no';
    const chip = h('span', { class: 'chip', 'data-agree': agree, 'data-up': String(n.up) },
      h('span', { class: 'id' }, `${n.role ? n.role + ' ' : ''}node${n.id}`),
      h('span', { class: 'h' }, hd ? String(hd.height) : '?'),
      h('span', { class: 'hash' }, hd ? short(hd.hash) : ''));
    const behind = hd && majority ? majority.height - hd.height : 0;
    if (agree === 'no' && behind > 0) chip.append(h('span', { class: 'h' }, `−${behind}`));
    tooltip(chip, () => `node${n.id}${n.role ? ' (' + n.role + ')' : ''}\nup: ${n.up}${n.error ? ' — ' + n.error : ''}\nsources: ${n.sources.join(', ')}\nyellowback: ${n.yellowback ?? '?'}\nhead: ${hd ? hd.height + ' ' + hd.hash : '?'}\n${agree === 'no' ? 'DISAGREES with the majority head' : agree === 'yes' ? 'on the majority head' : ''}`);
    el.append(chip);
  }
}

// Layout: column = height − minHeight (so a side block sits under the main block of its height);
// main row 0; side/orphaned blocks take the first free row at their column, chained branches share a row.
function layout(chain) {
  const blocks = [...chain.main.map((b) => ({ ...b, status: b.status === 'orphaned' ? 'orphaned' : 'main' })), ...chain.side];
  if (!blocks.length) return { nodes: [], cols: 0, rows: 1 };
  const minH = Math.min(...blocks.map((b) => b.height));
  const byHash = new Map(blocks.map((b) => [b.hash, b]));
  const occupied = new Map();  // col → Set(rows)
  const take = (col, row) => { if (!occupied.has(col)) occupied.set(col, new Set()); occupied.get(col).add(row); };
  const pos = new Map();
  for (const b of chain.main) { const col = b.height - minH; pos.set(b.hash, { col, row: 0 }); take(col, 0); }
  const side = [...chain.side].sort((a, b) => a.height - b.height);
  for (const b of side) {
    if (pos.has(b.hash)) continue;
    const col = b.height - minH;
    const parent = b.prev && pos.get(b.prev);
    let row = parent && parent.row > 0 ? parent.row : 1;
    while (occupied.get(col)?.has(row)) row++;
    pos.set(b.hash, { col, row }); take(col, row);
  }
  const nodes = blocks.map((b) => ({ b, ...pos.get(b.hash), parent: b.prev ? pos.get(b.prev) : undefined, parentKnown: b.prev ? byHash.has(b.prev) : false }));
  const cols = Math.max(...nodes.map((n) => n.col)) + 1;
  const rows = Math.max(...nodes.map((n) => n.row)) + 1;
  return { nodes, cols, rows };
}

function renderDag({ store }) {
  const s = store.snap; if (!s) return;
  const key = JSON.stringify([s.chain.main.map((b) => b.hash + b.status + (b.rejected ? 'R' : '')), s.chain.side.map((b) => b.hash + b.status + (b.rejected ? 'R' : ''))]);
  if (key === lastKey) return;   // the DAG only re-renders on a topology change
  lastKey = key;
  const { nodes, cols, rows } = layout(s.chain);
  const el = clear($('dag'));
  const W = PAD * 2 + cols * COL, H = PAD * 2 + rows * ROW;
  el.setAttribute('viewBox', `0 0 ${W} ${H}`); el.setAttribute('width', W); el.setAttribute('height', H);
  const cx = (n) => PAD + n.col * COL, cy = (n) => PAD + n.row * ROW;
  const edges = svg('g'), boxes = svg('g');
  for (const n of nodes) {
    const x = cx(n), y = cy(n);
    if (n.parent) {
      const px = cx(n.parent) + BW, py = cy(n.parent) + BH / 2;
      edges.append(svg('path', { class: 'edge', d: `M${px},${py} C${px + 8},${py} ${x - 8},${y + BH / 2} ${x},${y + BH / 2}` }));
    } else if (n.b.prev && !n.parentKnown && n.row > 0) {
      edges.append(svg('path', { class: 'edge unknown', d: `M${x - 14},${y + BH / 2} L${x},${y + BH / 2}` }));
    }
    const g = svg('g', { class: `blk ${n.b.status}${n.b.rejected || store.rejected.has(n.b.hash) ? ' rejected' : ''}`, transform: `translate(${x},${y})` });
    g.append(svg('rect', { width: BW, height: BH }));
    g.append(svg('text', { x: 6, y: 14, 'font-weight': 600 }, String(n.b.height)));
    g.append(svg('text', { class: 'hash', x: BW - 6, y: 14, 'text-anchor': 'end' }, short(n.b.hash)));
    g.append(svg('text', { class: 'meta', x: 6, y: 28 }, `${n.b.txCount ?? '?'} tx · ${fmtBytes(n.b.size)}`));
    // Badge row: miner (from the tag's payoutKey or coinbase, C3 supplies `miner`), tag price/signal, or placeholders.
    let bx = 6;
    const badge = (text, cls) => {
      const w = Math.max(18, text.length * 5.6 + 8);
      g.append(svg('rect', { class: cls, x: bx, y: 36, width: w, height: 14 }));
      g.append(svg('text', { class: 'badge', x: bx + 4, y: 46 }, text));
      bx += w + 4;
    };
    const miner = n.b.miner || n.b.yb?.miner;
    const tag = n.b.tag || n.b.yb?.tag;
    badge(miner ? String(miner).slice(0, 8) : 'miner ?', 'miner-bg');
    if (tag) badge(`$${(tag.priceMicroUsd / 1e6).toFixed(3)}${tag.signal ? ' ↑' : ''}`, 'badge-bg');
    else badge('no tag', 'badge-bg');
    tooltip(g, () => `height ${n.b.height}  ${n.b.status}${n.b.sideStatus ? ' (' + n.b.sideStatus + ', branchlen ' + n.b.branchlen + ')' : ''}\n${n.b.hash}\nprev ${n.b.prev || '?'}\n${n.b.txCount} tx, ${fmtBytes(n.b.size)}\ntime ${fmtTime(n.b.time)}  seen ${n.b.seen ? fmtTime(n.b.seen) : '?'}\nnodes: ${(n.b.nodes || []).join(', ') || '–'}${miner ? '\nminer ' + miner : ''}${tag ? '\ntag price $' + tag.priceMicroUsd / 1e6 + ' signal ' + tag.signal : ''}${n.b.rejected ? '\nREJECTED by an enforcing node' : ''}`);
    boxes.append(g);
  }
  el.append(edges, boxes);
  const wrap = $('dag-wrap'); requestAnimationFrame(() => { wrap.scrollLeft = wrap.scrollWidth; });
}

function gauge(label, value, sub, frac, level) {
  return h('div', { class: 'gauge', 'data-level': String(level) },
    h('div', { class: 'label' }, label), h('div', { class: 'value' }, value), h('div', { class: 'sub' }, sub),
    h('div', { class: 'bar' }, h('i', { style: `width:${Math.round(Math.min(1, frac) * 100)}%` })),
    h('div', { class: 'derived' }, 'derived'));
}

function renderGauges({ store, majority, now }) {
  const s = store.snap; if (!s) return;
  const el = clear($('gauges'));
  const sp = spacing(store);
  // fork risk = nodes disagreeing × seconds disagreeing (the longest-running disagreement)
  const dis = majority ? majority.disagreeing : [];
  const longest = dis.length ? Math.max(...dis.map((id) => now - (store.disagreeSince[id] || now))) : 0;
  const fork = dis.length * longest;
  el.append(gauge('fork risk', dis.length ? `${dis.length} × ${fmtSecs(longest)}` : '0',
    dis.length ? `node${dis.join(', node')} off the majority head` : 'every node on one head', fork / (s.nodes.length * 4 * sp), fork === 0 ? 0 : fork > s.nodes.length * 2 * sp ? 2 : 1));
  // orphan risk = blocks at the same height within the last WINDOW heights
  const tipH = majority ? majority.height : 0;
  const count = new Map();
  for (const b of [...s.chain.main, ...s.chain.side]) if (b.height > tipH - WINDOW) count.set(b.height, (count.get(b.height) || 0) + 1);
  const dup = [...count.values()].reduce((a, c) => a + Math.max(0, c - 1), 0);
  el.append(gauge('orphan risk', String(dup), `competing blocks in the last ${WINDOW} heights`, dup / 5, dup === 0 ? 0 : dup >= 3 ? 2 : 1));
  // reorg risk = deepest side branch within the valve depth, with the valve state overlaid
  let deepest = 0;
  for (const tips of Object.values(s.chain.tips || {})) for (const t of tips) if (t.status !== 'active' && t.branchlen > 0 && t.branchlen <= VALVE_DEPTH) deepest = Math.max(deepest, t.branchlen);
  const valve = Object.values(s.yedInfo || {}).some((i) => i && (i.valveTripped === true || i.state === 'valveTripped'));
  el.append(gauge('reorg risk', `${deepest} / ${VALVE_DEPTH}`, valve ? 'work valve TRIPPED' : `deepest side branch ≤ valve depth${valve === false ? '' : ''}`, deepest / VALVE_DEPTH, valve ? 2 : deepest >= 3 ? 2 : deepest > 0 ? 1 : 0));
}

function renderReorgs({ store }) {
  const el = clear($('reorg-log'));
  if (!store.reorgs.length) el.append(h('li', { class: 'muted' }, 'none seen this session'));
  for (const r of store.reorgs.slice(0, 50)) {
    el.append(h('li', {}, h('span', { class: 't' }, fmtTime(r.ts) + ' '), h('span', { class: 'n' }, `node${r.node} `),
      `depth ${r.depth}: ${short(r.from)} → ${short(r.to)} @ ${fmtInt(r.toHeight)}`));
  }
}
