// Revenue panel (plan §3.2.5, C4): "what participation pays". Self-contained like health.js:
// builds its own <section> on init, fetches GET /api/revenue (by=block, payoutKey, attestor)
// once per new block, renders plain SVG + tables. Every YEC figure carries its USD at that
// height's pMint (C-9), labelled "at pMint". Field names are the API's (src/model/revenue.rs).
import { h, svg, clear, tooltip } from '../lib.js';

const $ = (id) => document.getElementById(id);
const yec = (zat) => (zat === null || zat === undefined ? '–' : (zat / 1e8).toLocaleString(undefined, { maximumFractionDigits: 4 }));
const usd = (v) => (v === null || v === undefined ? '–' : `$${Number(v).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`);
const money = (m) => (m ? `${yec(m.zat)} YEC · ${usd(m.usd)}${m.usdComplete === false ? ' (partial)' : ''}` : '–');
const addr = (a) => (a ? (a.length > 14 ? a.slice(0, 8) + '…' + a.slice(-4) : a) : '–');
// Categorical slots, fixed order (dataviz): stock coinbase s1, enforcement fee s2, attestor fee s3, release s8 (status-like: critical).
const SLOT = { stock: 'var(--s1)', enforcefee: 'var(--s2)', attestfee: 'var(--s3)', release: 'var(--critical)' };
const RANGES = [['50', 'last 50 blocks'], ['200', 'last 200'], ['all', 'whole window']];

let local = { block: null, pools: null, attestors: null, fetchedHeight: -1, seenSeq: 0, range: '50' };
let inflight = null;
function qs(by) {
  const tip = local.tipHeight ?? 0;
  const from = local.range === 'all' ? '' : `from=${Math.max(0, tip - Number(local.range) + 1)}&`;
  return `/api/revenue?${from}by=${by}`;
}
async function load() {
  if (inflight) return inflight;
  inflight = Promise.all(['block', 'payoutKey', 'attestor'].map((by) => fetch(qs(by)).then((r) => r.json())))
    .then(([b, p, a]) => { local.block = b; local.pools = p; local.attestors = a; local.fetchedHeight = b.tip?.height ?? -1; })
    .catch(console.error).finally(() => { inflight = null; });
  return inflight;
}

export function init(store) {
  if (document.head && !document.querySelector('link[href="/ui/panels/revenue.css"]')) document.head.append(h('link', { rel: 'stylesheet', href: '/ui/panels/revenue.css' }));
  const main = document.querySelector?.('main.grid') || document.body;
  const sel = h('select', { id: 'rv-range' }, ...RANGES.map(([v, l]) => h('option', { value: v }, l)));
  sel.addEventListener('change', (e) => { local.range = e.target.value; local.fetchedHeight = -1; store.dirty = true; });
  const section = h('section', { id: 'panel-revenue', class: 'panel span-2' },
    h('div', { class: 'panel-head' }, h('h2', {}, 'Revenue — what participation pays'), h('span', { class: 'hint' }, h('label', {}, 'range ', sel), h('span', { id: 'rv-hint' }, ' · attributed from yed_gettxinfo (payee, attestPayee) and the coinbase; USD at pMint'))),
    h('div', { id: 'rv-tiles', class: 'tiles' }),
    h('div', { class: 'rv-grid' },
      h('div', { class: 'rv-card rv-wide' }, h('h3', {}, 'Per block'), h('span', { class: 'hint' }, 'stock coinbase (subsidy + network fees) beside the Yellowback fees paid in that block; hover for the payees'), svg('svg', { id: 'rv-blocks', class: 'plot', role: 'img', 'aria-label': 'per-block revenue' }), h('div', { id: 'rv-blocks-legend', class: 'legend' })),
      h('div', { class: 'rv-card rv-wide' }, h('h3', {}, 'Per pool (payoutKey)'), h('span', { class: 'hint' }, 'a fee goes to a quoting key, not to the block winner: tags published vs times selected is the number to watch'), h('table', { id: 'rv-pools', class: 'table' })),
      h('div', { class: 'rv-card' }, h('h3', {}, 'Per attestor (bond key)'), h('table', { id: 'rv-attestors', class: 'table' })),
      h('div', { class: 'rv-card' }, h('h3', {}, 'Counterfactual — a non-participant'), h('div', { id: 'rv-cf' })),
      h('div', { class: 'rv-card rv-wide' }, h('h3', {}, 'Not enforced: collateral releases'), h('span', { class: 'hint' }, 'with enforcement off, the vault\'s OP_TRUE path lets whoever mines the spend take the collateral'), h('div', { id: 'rv-releases' }))),
  );
  if (main) main.append(section); else registerIds(section);
  load();
}

function registerIds(root) {
  const ids = new Map(); const idOf = (n) => n.id || n.attrs?.id || n.getAttribute?.('id'); const walk = (n) => { if (idOf(n)) ids.set(idOf(n), n); for (const c of n.children || []) walk(c); }; walk(root);
  const orig = document.getElementById.bind(document);
  document.getElementById = (id) => (ids.has(id) ? ids.get(id) : orig(id));
}

export function render({ store }) {
  const s = store.snap; if (!s) return;
  let refetch = false;
  for (const e of store.events) {
    if (e.seq <= local.seenSeq) continue;
    local.seenSeq = e.seq;
    if (e.kind === 'block' || e.kind === 'revenue' || e.kind === 'reorg') refetch = true;
  }
  const tip = s.chain?.majority?.height ?? -1;
  local.tipHeight = tip;
  if (refetch || tip > local.fetchedHeight || !local.block) load();
  const b = local.block; if (!b) return;
  renderTiles(b, s);
  renderBlocks(b);
  renderPools(local.pools);
  renderAttestors(local.attestors);
  renderCounterfactual(b);
  renderReleases(b);
  $('rv-hint').textContent = ` · blocks ${b.from}–${b.to} (window ${b.window?.from ?? '–'}–${b.window?.to ?? '–'}) · ${b.totals.ybTxs} Yellowback txs · USD ${b.priceLabel}`;
}

function tile(label, value, sub, flag) { return h('div', { class: 'tile', 'data-flag': flag ? '1' : '0' }, h('div', { class: 'label' }, label), h('div', { class: 'value' }, value), sub ? h('div', { class: 'sub' }, sub) : null); }
function renderTiles(b, s) {
  const t = b.totals, el = clear($('rv-tiles'));
  const stock = { zat: t.subsidy.zat + t.netfee.zat, usd: (t.subsidy.usd ?? 0) + (t.netfee.usd ?? 0) };
  const enforcing = (b.enforcing || []).every(Boolean);
  el.append(
    tile('enforcement fees', `${yec(t.enforcefee.zat)} YEC`, `${usd(t.enforcefee.usd)} ${b.priceLabel}`),
    tile('attestor fees', `${yec(t.attestfee.zat)} YEC`, `${usd(t.attestfee.usd)} ${b.priceLabel}`),
    tile('stock coinbase', `${yec(stock.zat)} YEC`, `subsidy + network fees · ${usd(stock.usd)} ${b.priceLabel}`),
    tile('Yellowback share', t.blocks ? `${(100 * (t.enforcefee.zat + t.attestfee.zat) / Math.max(1, stock.zat + t.enforcefee.zat + t.attestfee.zat)).toFixed(2)} %` : '–', 'of all miner-side revenue in range'),
    tile('collateral released', `${yec(t.collateralRelease.zat)} YEC`, enforcing ? 'enforcement on everywhere' : 'ENFORCEMENT OFF on some node', t.collateralRelease.zat > 0),
    tile('pMint now', s.yellowback?.stats ? `$${(Object.values(s.yellowback.stats)[0]?.pMint / 1e6).toFixed(4)}` : '–', 'the price every USD figure here uses, per height'),
  );
}

// ---------------------------------------------------------------- per block: grouped thin bars
const M = { l: 56, r: 12, t: 10, b: 22 };
function ticks(lo, hi, n) { const raw = (hi - lo) / n, p = Math.pow(10, Math.floor(Math.log10(raw || 1))), f = raw / p, step = (f < 1.5 ? 1 : f < 3.5 ? 2 : f < 7.5 ? 5 : 10) * p; const out = []; for (let v = Math.ceil(lo / step) * step; v <= hi; v += step) out.push(v); return out; }
function renderBlocks(b) {
  const el = clear($('rv-blocks')); const rows = b.groups || [];
  if (!rows.length) { el.append(svg('text', { class: 'empty', x: 20, y: 20 }, 'no blocks in range yet')); return; }
  const W = el.clientWidth || 800, H = 220; el.setAttribute('viewBox', `0 0 ${W} ${H}`);
  const stockOf = (r) => r.subsidy.zat + r.netfee.zat, ybOf = (r) => r.enforcefee.zat + r.attestfee.zat;
  const hi = Math.max(1, ...rows.map((r) => Math.max(stockOf(r), ybOf(r)))) / 1e8 * 1.08;
  const yy = (v) => H - M.b - (v / hi) * (H - M.t - M.b);
  const slot = (W - M.l - M.r) / rows.length, bw = Math.min(24, Math.max(2, (slot - 2) / 2 - 1));
  for (const v of ticks(0, hi, 4)) { el.append(svg('line', { class: 'gridline', x1: M.l, x2: W - M.r, y1: yy(v), y2: yy(v) })); el.append(svg('text', { class: 'tick', x: M.l - 4, y: yy(v) + 3, 'text-anchor': 'end' }, `${v} YEC`)); }
  const step = Math.max(1, Math.ceil(rows.length / Math.max(1, Math.floor((W - M.l) / 60))));
  rows.forEach((r, i) => {
    const x0 = M.l + i * slot + 1;
    const bar = (x, v, colour) => { if (v <= 0) return null; const y = yy(v / 1e8); return svg('rect', { x, y, width: bw, height: Math.max(1, H - M.b - y), fill: colour, rx: 2 }); };
    const g = svg('g', { class: 'rv-bar' });
    const st = bar(x0, stockOf(r), SLOT.stock); if (st) g.append(st);
    // Yellowback fees stacked: enforcement fee, then the attestor fee with a 2px surface gap
    if (r.enforcefee.zat > 0) g.append(svg('rect', { x: x0 + bw + 2, y: yy(r.enforcefee.zat / 1e8), width: bw, height: Math.max(1, H - M.b - yy(r.enforcefee.zat / 1e8)), fill: SLOT.enforcefee, rx: 2 }));
    if (r.attestfee.zat > 0) { const y1 = yy((r.enforcefee.zat + r.attestfee.zat) / 1e8), y0 = yy(r.enforcefee.zat / 1e8) - 2; g.append(svg('rect', { x: x0 + bw + 2, y: y1, width: bw, height: Math.max(1, y0 - y1), fill: SLOT.attestfee, rx: 2 })); }
    if (r.collateralRelease.zat > 0) g.append(svg('rect', { x: x0, y: M.t, width: 2 * bw + 2, height: 4, fill: SLOT.release }));
    const hit = svg('rect', { x: x0 - 1, y: M.t, width: slot, height: H - M.t - M.b, fill: 'transparent' });
    tooltip(hit, () => `block ${r.height}  miner ${addr(r.miner)}${r.tag ? '  tag ' + addr(r.tag) : '  (no tag)'}\nsubsidy ${yec(r.subsidy.zat)}  net fees ${yec(r.netfee.zat)}  fund ${yec(r.subsidyOther.zat)} YEC\nenforcement fee ${yec(r.enforcefee.zat)} YEC (${usd(r.enforcefee.usd)})  attestor fee ${yec(r.attestfee.zat)} YEC (${usd(r.attestfee.usd)})\n${(r.ybTxs || []).map((t) => `${t.type}${t.path ? '/' + t.path : ''} ${t.txid.slice(0, 10)}… → ${addr(t.payee)}${t.attestPayee ? ' + ' + addr(t.attestPayee) : ''}`).join('\n') || 'no Yellowback tx'}${r.collateralRelease.zat > 0 ? '\nCOLLATERAL RELEASED ' + yec(r.collateralRelease.zat) + ' YEC' : ''}\npMint ${r.priceUsed ? '$' + (r.priceUsed / 1e6).toFixed(4) : '–'}`);
    g.append(hit); el.append(g);
    if (i % step === 0) el.append(svg('text', { class: 'tick', x: x0 + bw, y: H - 6, 'text-anchor': 'middle' }, String(r.height)));
  });
  el.append(svg('line', { class: 'axis', x1: M.l, x2: W - M.r, y1: H - M.b, y2: H - M.b }));
  const lg = clear($('rv-blocks-legend'));
  for (const [n, c] of [['stock coinbase', SLOT.stock], ['enforcement fee', SLOT.enforcefee], ['attestor fee', SLOT.attestfee], ['collateral release (enforcement off)', SLOT.release]]) lg.append(h('span', {}, h('i', { style: `background:${c}` }), n));
}

// ---------------------------------------------------------------- per pool
function renderPools(p) {
  const t = clear($('rv-pools')); if (!p) return;
  t.append(h('tr', {}, ...['payoutKey', 'blocks mined', 'tags', 'selected', 'enforcement fees', 'USD at pMint', 'per tag', 'per block mined', 'stock coinbase', 'quoting', 'accuracy'].map((c) => h('th', {}, c))));
  const max = Math.max(1, ...(p.groups || []).map((g) => g.enforcefee.zat));
  for (const g of p.groups || []) {
    const m = g.miner || {};
    t.append(h('tr', {},
      h('td', { class: 'mono', title: `${g.payee}${g.aliases?.length ? '\ncoinbase addresses: ' + g.aliases.join(', ') : ''}` }, addr(g.payee), g.aliases?.length ? h('span', { class: 'muted' }, ` +${g.aliases.length}`) : null),
      h('td', {}, String(g.blocksMined)), h('td', {}, String(g.tags)), h('td', {}, String(g.timesSelected)),
      h('td', {}, h('span', { class: 'rv-barcell' }, h('i', { style: `width:${(100 * g.enforcefee.zat) / max}%;background:${SLOT.enforcefee}` }), `${yec(g.enforcefee.zat)} YEC`)),
      h('td', {}, usd(g.enforcefee.usd)),
      h('td', {}, g.perTagZat === null || g.perTagZat === undefined ? '–' : yec(g.perTagZat)), h('td', {}, g.perBlockZat === null || g.perBlockZat === undefined ? '–' : yec(g.perBlockZat)),
      h('td', {}, `${yec(g.stockCoinbase.zat)} YEC`),
      h('td', {}, m.payoutAddress ? h('span', { class: 'rv-status', 'data-s': m.eligible ? 'OK' : 'NO' }, `${m.eligible ? 'eligible' : 'not eligible'} · ${m.quoteTags ?? '–'} quote tags`) : h('span', { class: 'muted' }, 'not quoting')),
      h('td', {}, m.accuracyBps !== undefined ? `${(m.accuracyBps / 100).toFixed(1)} %` : '–')));
  }
  if (!(p.groups || []).length) t.append(h('tr', {}, h('td', { colspan: 11, class: 'muted' }, 'no pools seen in range')));
}

// ---------------------------------------------------------------- per attestor
function renderAttestors(a) {
  const t = clear($('rv-attestors')); if (!a) return;
  t.append(h('tr', {}, ...['bond key', 'selected', 'fees', 'USD at pMint', 'bond', 'realised yield'].map((c) => h('th', {}, c))));
  for (const g of a.groups || []) {
    const at = g.attestor || {};
    t.append(h('tr', {}, h('td', { class: 'mono', title: g.payee }, addr(g.payee), at.seq !== undefined ? h('span', { class: 'muted' }, ` #${at.seq} ${at.status || ''}`) : null),
      h('td', {}, String(g.timesSelected)), h('td', {}, `${yec(g.attestfee.zat)} YEC`), h('td', {}, usd(g.attestfee.usd)), h('td', {}, g.bondZat ? `${yec(g.bondZat)} YEC` : '–'),
      h('td', { title: g.yieldLabel }, g.realisedYieldBps === null || g.realisedYieldBps === undefined ? '–' : `${(g.realisedYieldBps / 100).toFixed(2)} % realised`)));
  }
  if (!(a.groups || []).length) t.append(h('tr', {}, h('td', { colspan: 6, class: 'muted' }, 'no attestor fees in range (fees are paid only when ARMED)')));
  else t.append(h('tr', {}, h('td', { colspan: 6, class: 'muted' }, 'yield = fees received ÷ bond posted over this range: realised, not promised')));
}

// ---------------------------------------------------------------- counterfactual (C-7)
function renderCounterfactual(b) {
  const el = clear($('rv-cf')); const c = b.counterfactual; if (!c) return;
  el.append(h('div', { class: 'rv-hero' }, `≈ ${yec(c.zat)} YEC`), h('div', { class: 'sub' }, `${usd(c.usd)} ${b.priceLabel}`),
    h('p', {}, c.label + '.'),
    h('p', { class: 'hint' }, `${c.feeOutputs} fee outputs in range · E(R) resolved for ${c.resolved}${c.unresolved ? `, ${c.unresolved} pending` : ''}${c.noEligible ? `, ${c.noEligible} with no eligible payee` : ''}`),
    h('p', { class: 'hint' }, c.method));
}

// ---------------------------------------------------------------- no-enforcement releases
function renderReleases(b) {
  const el = clear($('rv-releases')); const n = b.noEnforcement; if (!n) return;
  if (!(n.rows || []).length) { el.append(h('div', { class: 'muted' }, `none in range — ${(b.enforcing || []).every(Boolean) ? 'every node is enforcing' : 'some node is NOT enforcing; releases appear here when a vault is spent through OP_TRUE'}`)); return; }
  const t = h('table', { class: 'table' }, h('tr', {}, ...['height', 'txid', 'vout', 'taken by', 'YEC', 'USD at pMint'].map((c) => h('th', {}, c))));
  for (const r of n.rows) t.append(h('tr', { 'data-bad': '1' }, h('td', {}, String(r.height)), h('td', { class: 'mono' }, r.txid.slice(0, 12) + '…'), h('td', {}, String(r.vout)), h('td', { class: 'mono' }, addr(r.payee)), h('td', {}, yec(r.zat)), h('td', {}, usd(r.usd))));
  el.append(h('div', { class: 'rv-verdict' }, `${yec(n.total.zat)} YEC (${usd(n.total.usd)}) of collateral left vaults without enforcement — ${n.label}`), t);
}
