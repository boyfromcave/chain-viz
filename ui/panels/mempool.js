// Mempool panel (plan §3.2.2): bubbles by fee rate × time in mempool (radius ~ size), grey unless the
// entry carries `yb.type` (C3), tiles for count/bytes/fees/median age, a >2-block flag, and a dashed
// red ring for a tx present on some nodes but not all.
import { h, svg, clear, fmtInt, fmtBytes, fmtSecs, fmtYec, median, tooltip, YB_TYPE_SLOT, slotVar } from '../lib.js';
import { spacing } from './header.js';

const $ = (id) => document.getElementById(id);
const M = { l: 44, r: 12, t: 10, b: 26 };

const age = (t, now) => now - Math.min(t.time || now, ...Object.values(t.firstSeen || {}).concat([Infinity]));
const feeRate = (t) => (t.size ? (t.fee * 1e8) / t.size : 0);   // zat per byte (fee is in YEC)
const colour = (t) => (t.yb?.type ? slotVar(YB_TYPE_SLOT[t.yb.type] || 's6') : 'var(--mark)');

export function render({ store, now }) {
  const s = store.snap; if (!s) return;
  const m = s.mempool, sp = spacing(store), nodesUp = s.nodes.filter((n) => n.up).length;
  const txs = m.txs.map((t) => ({ t, age: age(t, now), rate: feeRate(t), partial: nodesUp > 1 && t.present.length < nodesUp, stale: age(t, now) > 2 * sp }));
  const med = median(txs.map((x) => x.age));
  const stale = txs.filter((x) => x.stale).length, partial = txs.filter((x) => x.partial).length;
  const yed = txs.filter((x) => x.t.yb).length;
  const tiles = clear($('mempool-stats'));
  const tile = (label, value, flag) => h('div', { class: 'tile', 'data-flag': flag ? '1' : '0' }, h('div', { class: 'label' }, label), h('div', { class: 'value' }, value));
  tiles.append(tile('tx', fmtInt(m.count)), tile('bytes', fmtBytes(m.bytes)), tile('fees YEC', fmtYec(m.feeTotal)),
    tile('median age', fmtSecs(med)), tile('> 2 blocks', String(stale), stale > 0), tile('partial', String(partial), partial > 0), tile('Yellowback', String(yed)));

  const el = clear($('mempool-plot'));
  const W = el.clientWidth || 480, H = 240;
  el.setAttribute('viewBox', `0 0 ${W} ${H}`);
  if (!txs.length) { el.append(svg('text', { class: 'empty', x: W / 2, y: H / 2, 'text-anchor': 'middle' }, 'mempool empty')); clear($('mempool-legend')); clear($('mempool-table')); return; }
  const maxAge = Math.max(2 * sp, ...txs.map((x) => x.age)) * 1.05;
  const maxRate = Math.max(1, ...txs.map((x) => x.rate)) * 1.1;
  const x = (a) => M.l + (a / maxAge) * (W - M.l - M.r), y = (r) => H - M.b - (r / maxRate) * (H - M.t - M.b);
  for (const v of ticks(maxRate, 4)) {
    el.append(svg('line', { class: 'gridline', x1: M.l, x2: W - M.r, y1: y(v), y2: y(v) }));
    el.append(svg('text', { class: 'tick', x: M.l - 4, y: y(v) + 3, 'text-anchor': 'end' }, fmtInt(Math.round(v))));
  }
  for (const v of ticks(maxAge, 5)) el.append(svg('text', { class: 'tick', x: x(v), y: H - M.b + 12, 'text-anchor': 'middle' }, fmtSecs(v)));
  el.append(svg('line', { class: 'axis', x1: M.l, x2: W - M.r, y1: H - M.b, y2: H - M.b }));
  el.append(svg('text', { class: 'tick', x: W - M.r, y: H - 2, 'text-anchor': 'end' }, 'time in mempool →'));
  el.append(svg('text', { class: 'tick', x: M.l - 4, y: M.t, 'text-anchor': 'end' }, 'zat/B'));
  // the 2-block line and the median
  el.append(svg('line', { class: 'median', x1: x(2 * sp), x2: x(2 * sp), y1: M.t, y2: H - M.b }));
  el.append(svg('text', { class: 'median-label', x: x(2 * sp) + 3, y: M.t + 10 }, '2 blocks'));
  if (med !== undefined) { el.append(svg('line', { class: 'median', x1: x(med), x2: x(med), y1: M.t, y2: H - M.b })); el.append(svg('text', { class: 'median-label', x: x(med) + 3, y: H - M.b - 4 }, `median ${fmtSecs(med)}`)); }
  const maxSize = Math.max(1, ...txs.map((v) => v.t.size));
  for (const v of txs) {
    const r = 4 + 8 * Math.sqrt(v.t.size / maxSize);
    const dot = svg('circle', { class: `dot${v.partial ? ' partial' : ''}${v.stale ? ' stale' : ''}`, cx: x(v.age), cy: y(v.rate), r, fill: colour(v.t), 'fill-opacity': 0.85 });
    tooltip(dot, () => `${v.t.txid}\n${v.t.yb ? v.t.yb.type + ' ' + (v.t.yb.verdict || '') + '\n' : ''}${fmtBytes(v.t.size)}  fee ${fmtYec(v.t.fee)} YEC  ${v.rate.toFixed(1)} zat/B\nage ${fmtSecs(v.age)}${v.stale ? ' (> 2 blocks)' : ''}\npresent on ${v.t.present.length}/${nodesUp} nodes: ${v.t.present.join(', ')}${v.partial ? '  ← not everywhere' : ''}${v.t.depends?.length ? '\ndepends ' + v.t.depends.length : ''}`);
    el.append(dot);
  }
  const legend = clear($('mempool-legend'));
  legend.append(h('span', {}, h('i', { style: 'background:var(--mark)' }), 'ordinary tx'));
  for (const type of [...new Set(txs.map((v) => v.t.yb?.type).filter(Boolean))]) legend.append(h('span', {}, h('i', { style: `background:${slotVar(YB_TYPE_SLOT[type] || 's6')}` }), type));
  legend.append(h('span', {}, h('i', { class: 'ring' }), 'not on every node'), h('span', {}, 'size ~ area'));
  const table = clear($('mempool-table'));
  table.append(h('tr', {}, ...['txid', 'type', 'size', 'fee', 'zat/B', 'age', 'nodes'].map((c) => h('th', {}, c))));
  for (const v of txs.sort((a, b) => b.rate - a.rate).slice(0, 100)) table.append(h('tr', {}, h('td', {}, v.t.txid.slice(0, 16) + '…'), h('td', {}, v.t.yb?.type || ''), h('td', {}, fmtBytes(v.t.size)), h('td', {}, fmtYec(v.t.fee)), h('td', {}, v.rate.toFixed(1)), h('td', {}, fmtSecs(v.age)), h('td', {}, `${v.t.present.length}/${nodesUp}`)));
}

function ticks(max, n) {
  const step = niceStep(max / n); const out = [];
  for (let v = 0; v <= max; v += step) out.push(v);
  return out;
}
function niceStep(raw) { const p = Math.pow(10, Math.floor(Math.log10(raw || 1))); const f = raw / p; return (f < 1.5 ? 1 : f < 3.5 ? 2 : f < 7.5 ? 5 : 10) * p; }
