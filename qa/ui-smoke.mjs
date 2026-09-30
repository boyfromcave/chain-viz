#!/usr/bin/env node
// DOM-free smoke check of the UI: loads ui/app.js under a tiny fake DOM, points it at a running
// chain-viz (default http://127.0.0.1:8480), lets it fetch /api/snapshot and /api/events, renders
// every panel once, and asserts the rendered element counts against the snapshot. No browser.
//   node qa/ui-smoke.mjs [http://127.0.0.1:8491]
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import path from 'node:path';

const base = process.argv[2] || 'http://127.0.0.1:8480';
const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..', 'ui');

// ---- fake DOM: just enough for h()/svg()/clear()/getElementById/tooltip ----
class El {
  constructor(tag) { this.tagName = tag; this.children = []; this.attrs = {}; this.dataset = {}; this.style = {}; this.listeners = {}; this._text = ''; this.hidden = false; this.clientWidth = 600; this.scrollWidth = 0; }
  get firstChild() { return this.children[0]; }
  get lastChild() { return this.children[this.children.length - 1]; }
  append(...cs) { for (const c of cs) { const n = typeof c === 'string' ? new Text(c) : c; n.parentNode = this; this.children.push(n); } }
  prepend(c) { c.parentNode = this; this.children.unshift(c); }
  remove() { const p = this.parentNode; if (p) p.children.splice(p.children.indexOf(this), 1); }
  setAttribute(k, v) { this.attrs[k] = String(v); if (k === 'class') this.className = String(v); }
  getAttribute(k) { return this.attrs[k]; }
  addEventListener(k, f) { (this.listeners[k] ??= []).push(f); }
  set textContent(v) { this.children = []; this._text = String(v); }
  get textContent() { return this._text + this.children.map((c) => c.textContent).join(''); }
  set className(v) { this.attrs.class = v; } get className() { return this.attrs.class || ''; }
  querySelectorAll(sel) { const out = []; const cls = sel.replace(/^\./, ''); const walk = (n) => { for (const c of n.children) { if (c instanceof El) { if (sel.startsWith('.') ? c.className.split(/\s+/).includes(cls) : c.tagName === sel) out.push(c); walk(c); } } }; walk(this); return out; }
  set title(v) { this.attrs.title = v; }
  get offsetWidth() { return 100; } get offsetHeight() { return 40; }
}
class Text { constructor(t) { this.textContent = t; } remove() { const p = this.parentNode; if (p) p.children.splice(p.children.indexOf(this), 1); } }

const html = readFileSync(path.join(root, 'index.html'), 'utf8');
const ids = new Map();
for (const m of html.matchAll(/id="([^"]+)"/g)) ids.set(m[1], new El('div'));
globalThis.document = {
  createElement: (t) => new El(t), createElementNS: (_ns, t) => new El(t),
  getElementById: (id) => { if (!ids.has(id)) throw new Error(`index.html has no element with id="${id}"`); return ids.get(id); },
};
globalThis.window = { innerWidth: 1200, innerHeight: 800 };
globalThis.location = { protocol: 'http:', host: base.replace(/^https?:\/\//, '') };
globalThis.requestAnimationFrame = (f) => { rafQueue.push(f); return 0; };
const rafQueue = [];
globalThis.WebSocket = class { constructor(url) { this.url = url; wsInstances.push(this); } close() {} };
const wsInstances = [];
const realFetch = globalThis.fetch;
globalThis.fetch = (u) => realFetch(u.startsWith('http') ? u : base + u);

// ---- load the app (module side effects: loadSnapshot → connect → loop) ----
const app = await import(pathToFileURL(path.join(root, 'app.js')).href);
// wait for the initial snapshot to land
for (let i = 0; i < 50 && !app.store.snap; i++) await new Promise((r) => setTimeout(r, 100));
if (!app.store.snap) throw new Error(`no snapshot from ${base}`);
// simulate the ws hello → resync, then run the render loop once
const ws = wsInstances[0]; if (!ws) throw new Error('app did not open a WebSocket');
ws.onopen();
ws.onmessage({ data: JSON.stringify({ kind: 'hello', seq: app.store.seq }) });
await new Promise((r) => setTimeout(r, 800));
for (let i = 0; i < 3 && rafQueue.length; i++) rafQueue.shift()(performance.now());   // loop() re-queues itself

const s = app.store.snap;
const fail = (m) => { console.error('FAIL', m); process.exitCode = 1; };
const ok = (m) => console.log('ok  ', m);
const chips = ids.get('heads').children.length;
chips === s.nodes.length ? ok(`${chips} head chips for ${s.nodes.length} nodes`) : fail(`chips ${chips} != nodes ${s.nodes.length}`);
const disagreeing = ids.get('heads').children.filter((c) => c.dataset['agree'] === 'no' || c.attrs['data-agree'] === 'no').length;
ok(`${disagreeing} chip(s) marked disagreeing (snapshot says ${s.chain.majority?.disagreeing?.length ?? '?'})`);
const blks = ids.get('dag').querySelectorAll('.blk').length, want = s.chain.main.length + s.chain.side.length;
blks === want ? ok(`DAG draws ${blks} blocks (${s.chain.main.length} main + ${s.chain.side.length} side)`) : fail(`DAG blocks ${blks} != ${want}`);
const sideBlks = ids.get('dag').querySelectorAll('.blk').filter((g) => /side|orphaned/.test(g.className)).length;
ok(`${sideBlks} side/orphaned block(s) drawn below the main row`);
const edges = ids.get('dag').querySelectorAll('.edge').length; ok(`${edges} edges`);
const gauges = ids.get('gauges').children.length; gauges === 3 ? ok('3 risk gauges, all labelled derived: ' + ids.get('gauges').children.every((g) => g.textContent.includes('derived'))) : fail(`gauges ${gauges}`);
for (const g of ids.get('gauges').children) console.log('     ', g.children[0].textContent, '=', g.children[1].textContent, '—', g.children[2].textContent);
const reorgs = app.store.reorgs.length; ok(`reorg log: ${reorgs} entries, list has ${ids.get('reorg-log').children.length} rows`);
const dots = ids.get('mempool-plot').querySelectorAll('.dot').length;
dots === s.mempool.count ? ok(`mempool plot: ${dots} dots for ${s.mempool.count} txs`) : fail(`dots ${dots} != mempool ${s.mempool.count}`);
console.log('     ', ids.get('mempool-stats').children.map((t) => t.children[0].textContent + ' ' + t.children[1].textContent).join(' | '));
ok(`event log: ${ids.get('event-log').children.length} rows, ${ids.get('event-filter').children.length} kinds in the filter`);
console.log('     header:', ['chain-name', 'node-count', 'tip', 'since', 'spacing', 'conn'].map((i) => `${i}=${ids.get(i).textContent}`).join(' '), 'since-level', ids.get('since').dataset.level);
// revenue panel (C4): its tables follow /api/revenue's groups
{
  const rv = await (await fetch(base + '/api/revenue?by=payoutKey')).json();
  const el = (id) => document.getElementById(id);   // the panel registers its own ids (it builds its section itself)
  const pools = el('rv-pools').children.length;
  pools === rv.groups.length + 1 ? ok(`revenue: ${rv.groups.length} pool rows (+ header) for ${rv.groups.length} payoutKeys; enforcement fees ${rv.totals.enforcefee.zat} zat = ${rv.totals.enforcefee.usd?.toFixed(2)} USD ${rv.priceLabel}`) : fail(`revenue pool rows ${pools} != groups ${rv.groups.length} + 1`);
  const bars = el('rv-blocks').querySelectorAll('.rv-bar').length;
  const blk = await (await fetch(base + '/api/revenue?by=block')).json();
  const want = Math.min(50, blk.groups.length);
  bars === want ? ok(`revenue: ${bars} per-block bar groups (range: last 50 of ${blk.groups.length})`) : fail(`revenue bars ${bars} != ${want}`);
  console.log('     ', el('rv-tiles').children.map((t) => t.children[0].textContent + ' ' + t.children[1].textContent).join(' | '));
  console.log('     ', el('rv-cf').textContent.slice(0, 160));
}
// reconnect path: closing the socket must schedule a reconnect with backoff
ws.onclose(); await new Promise((r) => setTimeout(r, 600));
wsInstances.length >= 2 ? ok(`reconnect: ${wsInstances.length} sockets opened after a close`) : fail('no reconnect after close');
app.store.conn === 'reconnecting' || app.store.conn === 'open' ? ok(`conn state ${app.store.conn}`) : fail(`conn ${app.store.conn}`);
process.exit(process.exitCode || 0);
