// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

// Yellowback health panel (plan §3.2.4, C3). Self-contained: builds its own <section> on init,
// fetches GET /api/yellowback once per new block (and on yb_state / statehash_mismatch events),
// renders with plain SVG. Follows the panel contract of app.js: `init(store)`, `render(ctx)`.
// Field names are the node's (doc/yellowback-rpc-contract.json): pFast/pMid/pSlow/pMint/pClaim
// in micro-USD, supplyCents, collateralZat, globalRatioBps, haltMask, claimHeight, …
import { h, svg, clear, fmtInt, tooltip, pqSchemeShort, shortAddr, PQ_SCHEMES, feeParamsText } from '../lib.js';

const $ = (id) => document.getElementById(id);
const usd = (micro) => (micro === null || micro === undefined ? '–' : `$${(micro / 1e6).toFixed(4)}`);
const yec = (zat) => (zat === null || zat === undefined ? '–' : (zat / 1e8).toLocaleString(undefined, { maximumFractionDigits: 2 }));
const yed = (cents) => (cents === null || cents === undefined ? '–' : (cents / 100).toLocaleString(undefined, { maximumFractionDigits: 2 }));
const pct = (bps) => (bps === null || bps === undefined ? '–' : `${(bps / 100).toFixed(1)} %`);
// The claim threshold θ and the emergency ratio a node reports (yed_getinfo.params). rpcversion 6 (the
// in-term claims line, upgrade/vault-in-term): θ 125 %, inTermClaims true, earlyRedeemFeeBps per class
// A/B/C; an rpcversion-5 node has no inTermClaims and the upgrade line's 110 %. rpcversion 7 (the
// post-quantum line, upgrade/quantum-vaults): PQ owners (ownerScheme / ownerHash / a 53-character ownerAddress),
// params.pq, a version-4 MINT payload. rpcversion 8 (the fee structure): params.fees (protocol fee bands of the
// debt at pMint, network $/KB at the tip price), maxMintCents, supplyCap, pMint on every vault row; feeMinZat /
// feeBps gone. 8, 7 and 6 are modelled, any other version or a mix of them warns.
const KNOWN_RPCVERSIONS = [8, 7, 6];
function thresholds(info) {
  const p = info?.params || {};
  return { theta: p.claimThresholdBps ?? 11000, emerg: p.attest?.emergencyRatioBps ?? 10500, inTerm: p.inTermClaims === true, fees: p.earlyRedeemFeeBps || [], claimDelay: p.claimDelay };
}
// A yed_listclaimable row is claimable now unless it says otherwise (rpcversion 6 lists in-term rows
// above θ with claimable false; a rpcversion-5 row has no field and is claimable).
const isClaimableRow = (c) => c.claimable !== false;
// Payload / TxLog type names as the node spells them (PayloadTypeName, src/yellowback/payload.cpp).
// rpcversion 5 adds claim_release / claim_cancel (the claim intent's two ends) and set_act (an attestor's heartbeat, removal or set equivocation).
const TYPE_SLOT = { mint: 's1', transfer: 's3', redeem: 's2', register: 's4', notice: 's6', equivocation: 's8', revive: 's5', claim_release: 's7', claim_cancel: 's5', set_act: 's4' };
const typeColour = (t, path) => `var(--${t === 'redeem' && path === 'claim' ? 's7' : TYPE_SLOT[t] || 's6'})`;
const typeLabel = (t) => ({ mint: 'MINT', transfer: 'SEND', redeem: 'REDEEM', register: 'ATTESTOR', notice: 'NOTICE', equivocation: 'EQUIVOCATION', revive: 'REVIVE', claim_release: 'CLAIM RELEASE', claim_cancel: 'CLAIM CANCELLED', set_act: 'SET ACT' }[t.tx_type || t.type] || (t.type || '').toUpperCase());
const txLabel = (t) => (t.type === 'redeem' && t.path === 'claim' ? 'CLAIM' : typeLabel(t));

let local = { data: null, fetchedSeq: -1, fetchedHeight: -1, seenSeq: 0, arming: [], mock: [] };   // this panel's own state
let inflight = null;
async function load() {
  if (inflight) return inflight;
  inflight = fetch('/api/yellowback').then((r) => r.json()).then((d) => { local.data = d; local.fetchedSeq = d.seq; local.fetchedHeight = d.tip?.height ?? -1; }).catch(console.error).finally(() => { inflight = null; });
  return inflight;
}

export function init() {
  // The section is built here so the shell (index.html) needs no change; under the fake DOM of
  // qa/ui-smoke.mjs there is no head/body, so the ids are registered on the fly.
  if (document.head && !document.querySelector('link[href="/ui/panels/health.css"]')) document.head.append(h('link', { rel: 'stylesheet', href: '/ui/panels/health.css' }));
  const main = document.querySelector?.('main.grid') || document.body;
  const section = h('section', { id: 'panel-health', class: 'panel span-2' },
    h('div', { class: 'panel-head' }, h('h2', {}, 'Yellowback health'), h('span', { id: 'yb-hint', class: 'hint' }, 'yed_getinfo · yed_getstats · yed_getprice · yed_gethistory · yed_listvaults · yed_listattestors · yed_getstatehash')),
    h('div', { id: 'yb-ribbon', class: 'yb-ribbon' }),
    h('div', { id: 'yb-alarm', class: 'yb-alarm', hidden: true }),
    h('div', { class: 'yb-grid' },
      h('div', { class: 'yb-card yb-wide' }, h('h3', {}, 'Timeline'), h('span', { class: 'hint' }, 'x = height · the vault upgrade, arming, and one lane per halt-mask bit (filled = set)'), svg('svg', { id: 'yb-lanes', class: 'plot', role: 'img', 'aria-label': 'state lanes' })),
      h('div', { class: 'yb-card yb-wide' }, h('h3', {}, 'Prices'), h('span', { class: 'hint' }, 'pFast / pMid / pSlow lines · pMint–pClaim band · fed price dotted (devnet mock-price)'), svg('svg', { id: 'yb-prices', class: 'plot', role: 'img', 'aria-label': 'price windows' }), h('div', { id: 'yb-price-legend', class: 'legend' }), h('div', { id: 'yb-fill', class: 'yb-fill' })),
      h('div', { class: 'yb-card' }, h('h3', {}, 'Supply and collateral'), h('div', { id: 'yb-supply-tiles', class: 'tiles' }), svg('svg', { id: 'yb-ratio', class: 'plot', role: 'img', 'aria-label': 'global ratio' }), h('div', { id: 'yb-ratio-hint', class: 'hint' }, 'global ratio (collateral ÷ supply at pClaim) vs the claim and emergency lines · bars: net supply change per block')),
      h('div', { class: 'yb-card' }, h('h3', {}, 'Vaults'), h('div', { id: 'yb-vault-tiles', class: 'tiles' }), svg('svg', { id: 'yb-vaults', class: 'plot', role: 'img', 'aria-label': 'vault scatter' }), h('div', { id: 'yb-vault-legend', class: 'legend' }), h('table', { id: 'yb-vault-table', class: 'table' })),
      h('div', { class: 'yb-card' }, h('h3', {}, 'Attestors'), h('table', { id: 'yb-attestors', class: 'table' })),
      h('div', { class: 'yb-card' }, h('h3', {}, 'Consistency'), h('div', { id: 'yb-consistency' })),
      h('div', { class: 'yb-card yb-wide' }, h('h3', {}, 'Yellowback transactions'), h('span', { class: 'hint' }, 'newest first · mempool rows carry the dry-run verdict (yed_validaterawtransaction)'), h('table', { id: 'yb-txs', class: 'table' }))),
  );
  if (main) main.append(section); else registerIds(section);
  load();
}

// Fake-DOM fallback: make getElementById find this panel's ids without a document tree.
function registerIds(root) {
  const ids = new Map(); const idOf = (n) => n.id || n.attrs?.id || n.getAttribute?.('id'); const walk = (n) => { if (idOf(n)) ids.set(idOf(n), n); for (const c of n.children || []) walk(c); }; walk(root);
  const orig = document.getElementById.bind(document);
  document.getElementById = (id) => (ids.has(id) ? ids.get(id) : orig(id));
}

export function render({ store, now }) {
  const s = store.snap; if (!s) return;
  // New events since last look: a block or a state change means a refetch; price events with a mock price extend the dotted line.
  let refetch = false;
  for (const e of store.events) {
    if (e.seq <= local.seenSeq) continue;
    local.seenSeq = e.seq;
    if (e.kind === 'block' || e.kind === 'yb_state' || e.kind === 'statehash_mismatch' || e.kind === 'rejected_block' || e.kind === 'vault' || e.kind === 'attestor') refetch = true;
    if (e.kind === 'yb_tx' && !e.height) refetch = true;
    if (e.kind === 'yb_state' && e.field === 'attest.status') local.arming.push({ height: e.height, to: e.to });
    if (e.kind === 'price' && e.mockPrice !== undefined && e.mockPrice !== null) local.mock.push({ height: e.height ?? local.fetchedHeight, ts: e.ts, price: e.mockPrice });
  }
  const tip = s.chain?.majority?.height ?? -1;
  if (refetch || tip > local.fetchedHeight || !local.data) load();
  const d = local.data; if (!d) return;
  const y = d.yellowback, info = d.yedInfo?.[y.leader] || Object.values(d.yedInfo || {})[0];
  const stats = y.stats?.[y.leader] || Object.values(y.stats || {})[0];
  renderRibbon(y, info, stats, d);
  renderLanes(y, info, d);
  renderPrices(y, stats, d);
  renderSupply(y, stats, info);
  renderVaults(y, stats, d);
  renderAttestors(y);
  renderConsistency(y, d);
  renderTxs(y, s, d);
}

// ---------------------------------------------------------------- ribbon
function pill(label, on, level) { return h('span', { class: 'yb-pill', 'data-on': on ? '1' : '0', 'data-level': level || '' }, h('i'), label); }
function renderRibbon(y, info, stats, d) {
  const el = clear($('yb-ribbon')); if (!info) { el.append(h('span', { class: 'hint' }, 'no node with yed_getinfo')); return; }
  const up = info.upgrade, att = info.attest || {};
  // rpcversion 5: YED is the vault upgrade's rule module (consensus from activationHeight); the
  // enforcement / valve / sunset / abandonment / signalling state of v3 is gone (upgrade plan §6).
  const versions = [...new Set(Object.values(d.yedInfo || {}).map((i) => i.rpcversion))];
  el.append(...[
    pill(info.healthy ? 'healthy' : `unhealthy ${info.unhealthyReason || ''}`, info.healthy, info.healthy ? 'good' : 'critical'),
    // rpcversion 4 (hardening H-1): with mintRequiresArmed a mint at an unarmed R is invalid (mint-halted-unarmed)
    pill(info.mintRequiresArmed ? 'mint requires ARMED' : 'mint without ARMED', !!info.mintRequiresArmed, 'good'),
    versions.length === 1 && KNOWN_RPCVERSIONS.includes(versions[0]) ? null : pill(`rpcversion ${versions.join('/')} (chain-viz models ${KNOWN_RPCVERSIONS.join(' or ')})`, true, 'warning'),
    h('span', { class: 'yb-sep' }),
    h('span', { class: 'yb-state' }, h('b', {}, 'upgrade'), up ? ` ${up.name} ${(up.status || '?').toUpperCase()} ` : ' – ', h('span', { class: 'muted' }, up ? `branch ${up.branchId} · from ${up.activationHeight} · claim delay ${up.claimDelay} · attestor set ${(up.attestorSetId || '–').slice(0, 10)}…` : 'no upgrade object (a node before rpcversion 5)')),
    claimsState(info),
    pqState(info),
    feesState(info),
    h('span', { class: 'yb-state' }, h('b', {}, 'arming'), ` ${att.status || '?'} `, h('span', { class: 'muted' }, `${att.seatedCount ?? '–'} seated · pool ${att.poolSize ?? '–'} (fresh ${att.poolFresh ?? '–'}) · trigger ${att.triggerHeight ?? '–'} · arm ${att.armHeight ?? '–'}`)),
    h('span', { class: 'yb-state' }, h('b', {}, 'minting'), ` ${stats ? (stats.mintingAllowed ? 'allowed' : 'halted') : '–'} `, h('span', { class: 'muted' }, mintingWhy(info, stats))),
    noPriceState(y.noPrice),
    h('span', { class: 'yb-state' }, h('b', {}, 'index'), ` ${info.height ?? '–'}`, h('span', { class: 'muted' }, ` of ${info.chainHeight ?? '–'} · leader node ${y.leader ?? '–'}`)),
  ].filter(Boolean));
  const alarm = $('yb-alarm');
  if (y.statehash_agree === false) { alarm.hidden = false; clear(alarm).append(h('b', {}, 'STATE HASH MISMATCH'), ` healthy nodes on the same tip disagree on yed_getstatehash: `, ...Object.entries(y.statehash).map(([n, v]) => h('code', {}, `${n}:${v.statehash.slice(0, 10)} `))); }
  else alarm.hidden = true;
}

// rpcversion 7 (the quantum line): yed_getinfo.params.pq, the owner schemes MINT-3 admits at the next block, the
// Falcon activation height (-1 = never) and the size-priced fee rate wallets charge (max(DEFAULT_FEE, rate × size)).
function pqState(info) {
  const pq = info?.params?.pq; if (!pq) return null;
  const schemes = (pq.schemes || []).map((n) => PQ_SCHEMES[n] || `scheme ${n}`).join(' + ') || '–';
  const falcon = pq.falconActive ? 'Falcon ACTIVE' : pq.falconHeight >= 0 ? `Falcon from ${pq.falconHeight}` : 'Falcon off';
  return h('span', { class: 'yb-state', title: 'yed_getinfo.params.pq: schemes, falconActive, falconHeight, feeRateZatPerKB; MINT payload version ' + (info.params?.attest?.mintPayloadVersion ?? '–') },
    h('b', {}, 'post-quantum'), ` ${schemes} `, h('span', { class: 'muted' }, `${falcon} · fee ${pq.feeRateZatPerKB ?? '–'} zat/KB`));
}

// rpcversion 8: the fee structure and the mint limits (yed_getinfo.params.fees, maxMintCents, supplyCap).
function feesState(info) {
  const t = feeParamsText(info?.params); if (!t) return null;
  return h('span', { class: 'yb-state', title: `yed_getinfo.params.fees / maxMintCents / supplyCap (rpcversion 8)\n${t.protocol}\nearly-redeem ${t.early} of the debt at pMint\n${t.network}\n${t.supply}` },
    h('b', {}, 'fees'), ` ${t.protocol} `, h('span', { class: 'muted' }, `· ${t.network} · ${t.supply}`));
}

// The claim rule's parameters (rpcversion 6, in-term IT-7 / IT-9): whether a claim is valid in term, θ, the
// early-redeem fee per class and the claim delay.
function claimsState(info) {
  const t = thresholds(info);
  const fees = t.fees.length ? t.fees.map((b, i) => `${'ABC'[i] || i} ${b / 100} %`).join(' / ') : '–';
  // rpcversion 8 (IT-9'): the early-redeem fee is of the debt at the vault's pMint, before it of the collateral
  const base = info?.params?.fees ? 'of the debt' : 'of the collateral';
  return h('span', { class: 'yb-state', title: 'yed_getinfo.params: inTermClaims, claimThresholdBps, earlyRedeemFeeBps, claimDelay' }, h('b', {}, 'claims'), t.inTerm ? ' IN TERM ' : ' after term ',
    h('span', { class: 'muted' }, `θ ${pct(t.theta)} · early-redeem fee ${fees} ${t.fees.length ? base : ''} · claim delay ${t.claimDelay ?? '–'}`));
}

// Why minting is halted, or which classes are open. An unarmed halt (H-1) sets no halt-mask bit:
// the node reports mintingAllowed false and mintableClasses empty while the tip is not ARMED.
function mintingWhy(info, stats) {
  if (!stats) return '';
  if (stats.haltMask?.length) return stats.haltMask.join(' ');
  if (!stats.mintingAllowed && info.mintRequiresArmed && info.attest && info.attest.armed === false) return 'UNARMED (mint-halted-unarmed)';
  if (!stats.mintingAllowed && info.supplyCapReached) return 'supply cap reached';
  if (stats.mintableClasses) return stats.mintableClasses.length ? 'classes ' + stats.mintableClasses.join('') : 'no class mintable';
  return '';
}
// The NO_PRICE-hours counter: blocks with the NO_PRICE halt bit over the last window of the timeline.
function noPriceState(np) {
  if (!np || !np.covered) return h('span', { class: 'yb-state' }, h('b', {}, 'NO_PRICE'), ' –');
  const short = np.covered < np.window ? ` · timeline covers ${np.covered}` : '';
  return h('span', { class: 'yb-state', title: `heights ${np.fromHeight}–${np.toHeight}; hours at the 75 s target spacing` }, h('b', {}, 'NO_PRICE'), ` ${np.hours.toFixed(1)} h `, h('span', { class: 'muted' }, `${np.blocks} of the last ${np.window} blocks${short}`));
}
// The node's verdict ids worth a sentence in the transaction table's tooltip.
const VERDICT_TEXT = {
  'mint-halted-unarmed': 'invalid: the mint\'s reference height was not ARMED while mintRequiresArmed (hardening H-1, MINT-4)',
  'mint-halted-no-price': 'invalid: no price at the reference height (MINT-4)',
  'mint-halted-global-ratio': 'invalid: global-ratio halt and the class minimum is below the recapitalisation floor (MINT-4)',
  'mint-halted-divergence': 'invalid: price divergence halt at the reference height (MINT-4)',
};

// ---------------------------------------------------------------- timeline lanes
const LM = { l: 92, r: 12, t: 6, b: 18 };
function xScale(rows, W) { const h0 = rows[0].height, h1 = rows[rows.length - 1].height; return (hh) => LM.l + ((hh - h0) / Math.max(1, h1 - h0)) * (W - LM.l - LM.r); }
function heightAxis(el, rows, x, W, H) {
  const h0 = rows[0].height, h1 = rows[rows.length - 1].height, n = Math.max(1, Math.min(8, Math.floor((W - LM.l) / 70)));
  const step = Math.max(1, Math.ceil((h1 - h0) / n));
  for (let hh = h0; hh <= h1; hh += step) el.append(svg('text', { class: 'tick', x: x(hh), y: H - 4, 'text-anchor': 'middle' }, String(hh)));
}
function renderLanes(y, info, d) {
  const el = clear($('yb-lanes')); const rows = y.history || []; if (rows.length < 2) { el.append(svg('text', { class: 'empty', x: 20, y: 20 }, 'timeline: waiting for yed_gethistory')); return; }
  const bits = y.haltBits || [];
  const lanes = ['upgrade', 'arming', ...bits];
  const LH = 14, W = el.clientWidth || 800, H = LM.t + lanes.length * LH + LM.b;
  el.setAttribute('viewBox', `0 0 ${W} ${H}`); el.style.height = `${H}px`;
  const x = xScale(rows, W);
  lanes.forEach((name, i) => el.append(svg('text', { class: 'tick', x: LM.l - 6, y: LM.t + i * LH + 10, 'text-anchor': 'end' }, name)));
  const dx = Math.max(1, (W - LM.l - LM.r) / (rows.length - 1));
  // upgrade lane: active from yed_getinfo.upgrade.activationHeight (U-22), pending before
  const actH = info?.upgrade?.activationHeight;
  for (const r of rows) {
    const on = actH !== undefined && actH !== null && r.height >= actH;
    el.append(svg('rect', { x: x(r.height) - dx / 2, y: LM.t + 1, width: dx + 0.5, height: LH - 3, fill: on ? 'var(--good)' : 'var(--mark)', 'fill-opacity': on ? 0.55 : 0.85 }));
  }
  // arming lane: current status back to the first known change (yb_state events); before that, unknown = hatched
  const armColour = { UNARMED: 'var(--mark)', TRIGGERED: 'var(--s4)', ARMED: 'var(--good)' };
  const changes = [...local.arming].sort((a, b) => a.height - b.height);
  const first = rows[0].height, last = rows[rows.length - 1].height;
  let segStart = first, status = changes.length ? null : info?.attest?.status;
  const seg = (from, to, st) => el.append(svg('rect', { x: x(from) - dx / 2, y: LM.t + LH + 1, width: Math.max(1, x(to) - x(from) + dx), height: LH - 3, fill: st ? armColour[st] || 'var(--mark)' : 'var(--grid)', 'fill-opacity': 0.6 }));
  for (const c of changes) { seg(segStart, c.height, status); segStart = c.height; status = c.to; }
  seg(segStart, last, status || info?.attest?.status);
  if (info?.attest?.armHeight && info.attest.armHeight >= first && info.attest.armHeight <= last) el.append(svg('line', { class: 'yb-marker', x1: x(info.attest.armHeight), x2: x(info.attest.armHeight), y1: LM.t, y2: LM.t + 2 * LH }));
  // halt-mask lanes
  bits.forEach((bit, i) => {
    for (const r of rows) if ((r.haltMask || []).includes(bit)) el.append(svg('rect', { x: x(r.height) - dx / 2, y: LM.t + (2 + i) * LH + 1, width: dx + 0.5, height: LH - 3, fill: 'var(--critical)', 'fill-opacity': 0.7 }));
  });
  for (let i = 1; i < lanes.length; i++) el.append(svg('line', { class: 'gridline', x1: LM.l, x2: W - LM.r, y1: LM.t + i * LH - 1, y2: LM.t + i * LH - 1 }));
  heightAxis(el, rows, x, W, H);
  // refused (YED-invalid) blocks as ticks under the lanes
  for (const r of y.rejected || []) { const b = (d.blocks || []).find((bb) => bb.hash === r.hash); if (b) el.append(svg('line', { class: 'yb-rejected', x1: x(b.height), x2: x(b.height), y1: LM.t, y2: H - LM.b })); }
  hoverRows(el, rows, x, W, H, (r) => `height ${r.height}\nupgrade ${actH !== undefined && actH !== null ? (r.height >= actH ? 'active' : 'pending') : '–'}\nhaltMask ${(r.haltMask || []).join(' ') || 'none'}\ntagged ${r.tagged ?? '–'} quote ${r.quote ?? '–'}`);
}
function hoverRows(el, rows, x, W, H, text) {
  const hit = svg('rect', { x: LM.l, y: 0, width: W - LM.l - LM.r, height: H, fill: 'transparent' });
  let cur = null;
  hit.addEventListener('mousemove', (e) => {
    const r = el.getBoundingClientRect(); const px = (e.clientX - r.left) * (W / r.width);
    let best = rows[0]; for (const rr of rows) if (Math.abs(x(rr.height) - px) < Math.abs(x(best.height) - px)) best = rr;
    cur = best;
  });
  tooltip(hit, () => (cur ? text(cur) : ''));
  el.append(hit);
}

// ---------------------------------------------------------------- prices
const PM = { l: 64, r: 12, t: 10, b: 20 };
function line(el, pts, cls, colour, dashed) {
  if (pts.length < 2) return;
  el.append(svg('path', { class: cls, d: pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(''), fill: 'none', stroke: colour, 'stroke-width': 2, 'stroke-dasharray': dashed ? '3 4' : null }));
}
function renderPrices(y, stats, d) {
  const el = clear($('yb-prices')); const rows = (y.history || []).filter((r) => r.pMid !== null && r.pMid !== undefined);
  if (rows.length < 2) { el.append(svg('text', { class: 'empty', x: 20, y: 20 }, 'prices: waiting for history')); return; }
  const W = el.clientWidth || 800, H = 200; el.setAttribute('viewBox', `0 0 ${W} ${H}`);
  const x = xScale(rows, W);
  const mocks = local.mock.filter((m) => m.height >= rows[0].height);
  if (y.mockPrice !== undefined && y.mockPrice !== null) mocks.push({ height: rows[rows.length - 1].height, price: y.mockPrice });
  const vals = rows.flatMap((r) => [r.pFast, r.pMid, r.pSlow, r.pMint, r.pClaim]).filter((v) => v).concat(mocks.map((m) => m.price * 1e6));
  const lo = Math.min(...vals) * 0.97, hi = Math.max(...vals) * 1.03;
  const yy = (v) => H - PM.b - ((v - lo) / Math.max(1, hi - lo)) * (H - PM.t - PM.b);
  for (const v of ticks(lo, hi, 4)) { el.append(svg('line', { class: 'gridline', x1: PM.l, x2: W - PM.r, y1: yy(v), y2: yy(v) })); el.append(svg('text', { class: 'tick', x: PM.l - 4, y: yy(v) + 3, 'text-anchor': 'end' }, usd(v))); }
  // band pMint..pClaim
  const band = rows.filter((r) => r.pMint && r.pClaim);
  if (band.length > 1) el.append(svg('path', { d: band.map((r, i) => `${i ? 'L' : 'M'}${x(r.height).toFixed(1)},${yy(r.pMint).toFixed(1)}`).join('') + band.slice().reverse().map((r) => `L${x(r.height).toFixed(1)},${yy(r.pClaim).toFixed(1)}`).join('') + 'Z', fill: 'var(--s1)', 'fill-opacity': 0.12, stroke: 'none' }));
  line(el, rows.filter((r) => r.pFast).map((r) => [x(r.height), yy(r.pFast)]), 'l', 'var(--s1)');
  line(el, rows.filter((r) => r.pMid).map((r) => [x(r.height), yy(r.pMid)]), 'l', 'var(--s2)');
  line(el, rows.filter((r) => r.pSlow).map((r) => [x(r.height), yy(r.pSlow)]), 'l', 'var(--s3)');
  const mp = mocks.sort((a, b) => a.height - b.height).map((m) => [x(Math.max(rows[0].height, m.height)), yy(m.price * 1e6)]);
  if (mp.length === 1) mp.unshift([x(rows[Math.max(0, rows.length - 2)].height), mp[0][1]]);
  line(el, mp, 'l', 'var(--ink-2)', true);
  // direct labels at the right edge
  const lastRow = rows[rows.length - 1];
  // (spread apart when the three windows agree, so the labels never overprint each other)
  const labels = [['pFast', 'var(--s1)'], ['pMid', 'var(--s2)'], ['pSlow', 'var(--s3)']].filter(([k]) => lastRow[k]).map(([k, c]) => ({ k, c, y: yy(lastRow[k]) - 3 })).sort((a, b) => a.y - b.y);
  for (let i = 1; i < labels.length; i++) labels[i].y = Math.max(labels[i].y, labels[i - 1].y + 11);
  for (let i = labels.length - 1; i >= 0; i--) { const top = i + 1 < labels.length ? labels[i + 1].y - 11 : H - PM.b - 2; labels[i].y = Math.min(labels[i].y, top); }
  for (const l of labels) el.append(svg('text', { class: 'yb-label', x: W - PM.r - 2, y: l.y, 'text-anchor': 'end', fill: l.c }, `${l.k} ${usd(lastRow[l.k])}`));
  heightAxis(el, rows, x, W, H);
  hoverRows(el, rows, x, W, H, (r) => `height ${r.height}\npFast ${usd(r.pFast)}  pMid ${usd(r.pMid)}  pSlow ${usd(r.pSlow)}\npMint ${usd(r.pMint)}  pClaim ${usd(r.pClaim)}${r.sigmaMultBps ? `\nsigmaMult ${pct(r.sigmaMultBps)}` : ''}`);
  const lg = clear($('yb-price-legend'));
  for (const [n, c] of [['pFast', 'var(--s1)'], ['pMid', 'var(--s2)'], ['pSlow', 'var(--s3)']]) lg.append(h('span', {}, h('i', { style: `background:${c}` }), n));
  lg.append(h('span', {}, h('i', { style: 'background:var(--s1);opacity:.25' }), 'pMint–pClaim band'), h('span', {}, h('i', { class: 'dotted' }), `fed price${y.mockPrice ? ' $' + Number(y.mockPrice).toFixed(4) : ''}`));
  if (y.attestPrices && Object.keys(y.attestPrices).length) lg.append(h('span', { class: 'muted' }, `attestors fed: ${Object.entries(y.attestPrices).map(([n, p]) => `${n}:$${Number(p).toFixed(2)}`).join(' ')}`));
  // window fill
  const fill = clear($('yb-fill')); const f = y.price?.fill || {};
  for (const w of ['fast', 'mid', 'slow']) {
    const v = f[w]; if (!v) continue;
    const ok = v.quoteTags >= v.minFill;
    fill.append(h('div', { class: 'yb-fillrow', 'data-ok': ok ? '1' : '0' }, h('span', { class: 'label' }, `${w} ${v.quoteTags}/${v.window}`), h('div', { class: 'bar' }, h('i', { style: `width:${(100 * v.quoteTags) / v.window}%` }), h('b', { style: `left:${(100 * v.minFill) / v.window}%`, title: `minFill ${v.minFill}` }))));
  }
  if (y.price) fill.append(h('div', { class: 'hint' }, `at ${y.price.height}: xMint ${usd(y.price.xMint)} xClaim ${usd(y.price.xClaim)} · ${y.price.armed ? 'armed' : 'unarmed'} (${y.price.attestStatus || '–'}) · seated ${(y.price.seated || []).length}${y.price.pinnedKeys?.length ? ' · pinned ' + y.price.pinnedKeys.length : ''}`));
}
function ticks(lo, hi, n) { const raw = (hi - lo) / n, p = Math.pow(10, Math.floor(Math.log10(raw || 1))), f = raw / p, step = (f < 1.5 ? 1 : f < 3.5 ? 2 : f < 7.5 ? 5 : 10) * p; const out = []; for (let v = Math.ceil(lo / step) * step; v <= hi; v += step) out.push(v); return out; }

// ---------------------------------------------------------------- supply / collateral / ratio
function tile(label, value, sub, flag) { return h('div', { class: 'tile', 'data-flag': flag ? '1' : '0' }, h('div', { class: 'label' }, label), h('div', { class: 'value' }, value), sub ? h('div', { class: 'sub' }, sub) : null); }
function renderSupply(y, stats, info) {
  const t = thresholds(info);
  const hint = $('yb-ratio-hint'); if (hint) { clear(hint); hint.append(`global ratio (collateral ÷ supply at pClaim) vs the ${pct(t.theta)} claim and ${pct(t.emerg)} emergency lines · bars: net supply change per block`); }
  const tiles = clear($('yb-supply-tiles')); if (!stats) return;
  const rows = y.history || []; const prev = rows.length > 1 ? rows[rows.length - 2] : null;
  const dS = prev ? stats.supplyCents - prev.supplyCents : null, dC = prev ? stats.collateralZat - prev.collateralZat : null;
  tiles.append(tile('supply YED', yed(stats.supplyCents), dS !== null ? `${dS >= 0 ? '+' : ''}${yed(dS)} this block` : ''),
    tile('collateral YEC', yec(stats.collateralZat), dC !== null ? `${dC >= 0 ? '+' : ''}${yec(dC)} this block` : ''),
    tile('global ratio', pct(stats.globalRatioBps), 'at pClaim', stats.globalRatioBps !== null && stats.globalRatioBps < t.theta),
    tile('unbacked YED', yed(stats.unbackedCents), '', stats.unbackedCents > 0));
  const el = clear($('yb-ratio')); const pts = rows.filter((r) => r.globalRatioBps !== null && r.globalRatioBps !== undefined);
  if (pts.length < 2) { el.append(svg('text', { class: 'empty', x: 20, y: 20 }, 'no supply yet')); return; }
  const W = el.clientWidth || 400, H = 160; el.setAttribute('viewBox', `0 0 ${W} ${H}`);
  const x = xScale(rows, W);
  const hi = Math.max(t.theta + 1000, ...pts.map((r) => r.globalRatioBps)) * 1.05, lo = 0;
  const yy = (v) => H - PM.b - ((v - lo) / (hi - lo)) * (H - PM.t - PM.b);
  for (const v of ticks(lo, hi, 4)) { el.append(svg('line', { class: 'gridline', x1: PM.l, x2: W - PM.r, y1: yy(v), y2: yy(v) })); el.append(svg('text', { class: 'tick', x: PM.l - 4, y: yy(v) + 3, 'text-anchor': 'end' }, pct(v))); }
  // the two reference lines sit 5 % apart: label one above, one below, so they never collide
  for (const [v, c, l, dy] of [[t.theta, 'var(--serious)', `${pct(t.theta)} claim`, -3], [t.emerg, 'var(--critical)', `${pct(t.emerg)} emergency`, 11]]) { el.append(svg('line', { x1: PM.l, x2: W - PM.r, y1: yy(v), y2: yy(v), stroke: c, 'stroke-width': 1, 'stroke-dasharray': '4 3' })); el.append(svg('text', { class: 'yb-label', x: PM.l + 2, y: yy(v) + dy, fill: c }, l)); }
  // net supply change bars (baseline mid-height, tiny)
  const deltas = rows.map((r, i) => (i ? r.supplyCents - rows[i - 1].supplyCents : 0)); const maxD = Math.max(1, ...deltas.map(Math.abs));
  const dx = Math.max(1, (W - PM.l - PM.r) / rows.length);
  rows.forEach((r, i) => { const dv = deltas[i]; if (!dv) return; const hh = (Math.abs(dv) / maxD) * 30; el.append(svg('rect', { x: x(r.height) - dx / 2 + 1, y: dv > 0 ? H - PM.b - hh : H - PM.b, width: Math.max(1, dx - 2), height: hh, fill: dv > 0 ? 'var(--s3)' : 'var(--s2)', 'fill-opacity': 0.8 })); });
  line(el, pts.map((r) => [x(r.height), yy(r.globalRatioBps)]), 'l', 'var(--s1)');
  heightAxis(el, rows, x, W, H);
  hoverRows(el, rows, x, W, H, (r) => `height ${r.height}\nsupply ${yed(r.supplyCents)} YED  collateral ${yec(r.collateralZat)} YEC\nglobal ratio ${pct(r.globalRatioBps)}`);
}

// ---------------------------------------------------------------- vault scatter
function renderVaults(y, stats, d) {
  const tiles = clear($('yb-vault-tiles')); if (!stats) return;
  const info = d.yedInfo?.[y.leader] || Object.values(d.yedInfo || {})[0], t = thresholds(info);
  // rpcversion 6: yed_listclaimable lists in-term vaults too; claimable false = listed, not claimable (above θ).
  const rows = y.claimable || [], now = rows.filter(isClaimableRow), listed = rows.length - now.length;
  // rpcversion 5: a claim is an intent (status CLAIMING until released or cancelled); no VOID vault
  // is produced after the upgrade, so the void tile shows only when an older one is listed.
  const claiming = (y.vaults || []).filter((v) => v.status === 'CLAIMING').length;
  tiles.append(tile('active', fmtInt(stats.activeVaults)), tile('claiming', fmtInt(claiming), 'claim intents pending', claiming > 0), tile('claimable now', fmtInt(now.length), listed ? `${fmtInt(listed)} listed above θ` : '', now.length > 0), tile('closed', fmtInt(stats.closedVaults)), tile('claimed', fmtInt(stats.claimedVaults)));
  if (stats.voidVaults > 0) tiles.append(tile('void', fmtInt(stats.voidVaults), voidSub(y)));
  const el = clear($('yb-vaults')); const tip = d.tip?.height ?? stats.height, pClaim = stats.pClaim;
  const claimable = new Set(now.map((c) => c.vault)), listedSet = new Set(rows.filter((c) => !isClaimableRow(c)).map((c) => c.vault));
  // In term (rpcversion 6) the claim opens at θ, not at claimHeight: x is the blocks left in the term (to lockHeight).
  const xField = t.inTerm ? 'lockHeight' : 'claimHeight';
  const vs = (y.vaults || []).filter((v) => v.status === 'ACTIVE' && v.mintedCents > 0 && v.collateralZat > 0).map((v) => ({ v, ratio: (v.collateralZat * pClaim) / (v.mintedCents * 1e8), toClaim: v[xField] - tip, key: `${v.txid}:${v.vout}` }));
  renderVaultTable(vs, claimable, listedSet, rows, t);
  if (!vs.length) { el.append(svg('text', { class: 'empty', x: 20, y: 20 }, 'no active vaults')); clear($('yb-vault-legend')); return; }
  const W = el.clientWidth || 400, H = 200; el.setAttribute('viewBox', `0 0 ${W} ${H}`);
  const M = { l: 52, r: 12, t: 10, b: 24 };
  const xs = vs.map((p) => p.toClaim), xlo = Math.min(0, ...xs) - 2, xhi = Math.max(10, ...xs) + 2;
  const ylo = 0, yhi = Math.max(t.theta + 1000, ...vs.map((p) => p.ratio)) * 1.08;
  const x = (v) => M.l + ((v - xlo) / (xhi - xlo)) * (W - M.l - M.r), yy = (v) => H - M.b - ((v - ylo) / (yhi - ylo)) * (H - M.t - M.b);
  for (const v of ticks(ylo, yhi, 4)) { el.append(svg('line', { class: 'gridline', x1: M.l, x2: W - M.r, y1: yy(v), y2: yy(v) })); el.append(svg('text', { class: 'tick', x: M.l - 4, y: yy(v) + 3, 'text-anchor': 'end' }, pct(v))); }
  for (const v of ticks(xlo, xhi, 5)) el.append(svg('text', { class: 'tick', x: x(v), y: H - 6, 'text-anchor': 'middle' }, String(Math.round(v))));
  el.append(svg('text', { class: 'tick', x: W - M.r, y: H - M.b - 4, 'text-anchor': 'end' }, `blocks to ${xField} →`));
  el.append(svg('line', { class: 'axis', x1: x(0), x2: x(0), y1: M.t, y2: H - M.b }));
  for (const [v, c] of [[t.theta, 'var(--serious)'], [t.emerg, 'var(--critical)']]) el.append(svg('line', { x1: M.l, x2: W - M.r, y1: yy(v), y2: yy(v), stroke: c, 'stroke-width': 1, 'stroke-dasharray': '4 3' }));
  const cls = { A: 'var(--s1)', B: 'var(--s2)', C: 'var(--s3)' };
  for (const p of vs) {
    const isC = claimable.has(p.key), under = p.ratio < t.theta;
    const dot = svg('circle', { cx: x(p.toClaim), cy: yy(p.ratio), r: isC ? 7 : 5, fill: cls[p.v.termClass] || 'var(--s6)', 'fill-opacity': 0.85, stroke: isC ? 'var(--critical)' : under ? 'var(--serious)' : 'var(--surface)', 'stroke-width': isC ? 3 : 2 });
    tooltip(dot, () => `vault ${p.key.slice(0, 16)}…  class ${p.v.termClass}\nratio ${pct(p.ratio)} at pClaim ${usd(pClaim)}  (claimable at ${usd(p.v.underwaterAt)}, θ ${pct(t.theta)})\nminted ${yed(p.v.mintedCents)} YED  collateral ${yec(p.v.collateralZat)} YEC${p.v.pMint ? '  pMint ' + usd(p.v.pMint) : ''}\nmint ${p.v.mintHeight} → lock ${p.v.lockHeight} → claim ${p.v.claimHeight} (${p.toClaim} blocks to ${xField})${p.v.noticed ? '\nNOTICED at ' + p.v.noticeHeight : ''}${isC ? '\nCLAIMABLE now' : listedSet.has(p.key) ? '\nlisted, not claimable (above θ)' : ''}`);
    el.append(dot);
  }
  const lg = clear($('yb-vault-legend'));
  for (const c of ['A', 'B', 'C']) lg.append(h('span', {}, h('i', { style: `background:${cls[c]}` }), `class ${c}`));
  lg.append(h('span', {}, h('i', { class: 'ring' }), 'claimable now (yed_listclaimable claimable)'), h('span', { class: 'muted' }, `y = collateral ÷ debt at pClaim ${usd(pClaim)} (claims use pClaim; the global ratio tile uses pMint ${usd(stats.pMint)}); dashed ${pct(t.theta)} / ${pct(t.emerg)}${t.inTerm ? ' · in-term claims: a vault is claimable whenever it is under θ' : ''}`));
}

// One row per active vault: its claimable-at price (underwaterAt, the price at which it falls under θ) and
// its claim state; a listed row of yed_listclaimable with claimable false is "listed" (in term, above θ).
function renderVaultTable(vs, claimable, listedSet, rows, t) {
  const tb = clear($('yb-vault-table')); if (!tb) return;
  // rpcversion 8: pMint, the vault's stored mint price every fee of it is struck at (FEE-1', RED-3', IT-9')
  tb.append(h('tr', {}, ...['vault', 'class', 'ratio', 'mint price', 'claimable at', 'lock', 'claim', 'owner'].map((c) => h('th', {}, c))));
  const byKey = new Map(rows.map((c) => [c.vault, c]));
  for (const p of [...vs].sort((a, b) => a.ratio - b.ratio)) {
    const row = byKey.get(p.key), state = claimable.has(p.key) ? 'CLAIMABLE now' : listedSet.has(p.key) ? 'listed, above θ' : '–';
    tb.append(h('tr', { 'data-bad': claimable.has(p.key) ? '1' : '0' }, h('td', {}, h('code', {}, `${p.key.slice(0, 10)}…`)), h('td', {}, p.v.termClass || '–'), h('td', {}, pct(p.ratio)),
      h('td', { title: 'pMint: the price this vault was minted at (rpcversion 8)' }, usd(row?.pMint ?? p.v.pMint)), h('td', {}, usd(row?.underwaterAt ?? p.v.underwaterAt)), h('td', {}, String(p.v.lockHeight ?? '–')), h('td', {}, state), ownerCell(p.v)));
  }
  if (!vs.length) tb.append(h('tr', {}, h('td', { colspan: 8, class: 'muted' }, 'no active vaults')));
}

// The vault owner (rpcversion 7): its post-quantum scheme and PQ address, a vault identifier and never a YED
// receive address (quantum clients C-10). An older node's row has neither.
function ownerCell(v) {
  if (v.ownerScheme === undefined && !v.ownerAddress) return h('td', { class: 'muted' }, '–');
  return h('td', { class: 'mono', title: `vault owner (post-quantum): ${PQ_SCHEMES[v.ownerScheme] || 'scheme ' + v.ownerScheme}\n${v.ownerAddress || ''}\nkey hash ${v.ownerHash || '–'}` },
    h('span', { class: 'yb-pq' }, pqSchemeShort(v.ownerScheme)), ' ', shortAddr(v.ownerAddress));
}

// The void tile's sub-line: how many listed VOID vaults are unarmed mints (H-1).
function voidSub(y) {
  const unarmed = (y.vaults || []).filter((v) => v.status === 'VOID' && v.voidReason === 'mint-halted-unarmed').length;
  return unarmed ? `${unarmed} unarmed (H-1)` : '';
}

// ---------------------------------------------------------------- attestors
function renderAttestors(y) {
  const t = clear($('yb-attestors'));
  t.append(h('tr', {}, ...['seq', 'status', 'bond YEC', 'seated', 'pinned', 'fresh', 'weight', 'last bundle', 'registered'].map((c) => h('th', {}, c))));
  for (const a of y.attestors || []) t.append(h('tr', { 'data-status': a.status }, h('td', {}, String(a.seq)), h('td', {}, h('span', { class: 'yb-status', 'data-s': a.status }, a.status)), h('td', {}, yec(a.bondZat)), h('td', {}, a.seated ? `since ${a.seatedSince}` : 'no'), h('td', {}, a.pinned ? 'yes' : 'no'), h('td', {}, a.poolFresh ? 'yes' : 'no'), h('td', {}, String(a.weight ?? '–')), h('td', {}, String(a.lastBundleHeight ?? '–')), h('td', {}, String(a.registerHeight ?? '–'))));
  if (!(y.attestors || []).length) t.append(h('tr', {}, h('td', { colspan: 9, class: 'muted' }, 'no attestors registered')));
}

// ---------------------------------------------------------------- consistency, rejected, miners
function renderConsistency(y, d) {
  const el = clear($('yb-consistency'));
  const nodes = Object.entries(y.statehash || {});
  const agree = y.statehash_agree;
  el.append(h('div', { class: 'yb-agree', 'data-agree': agree === null || agree === undefined ? 'na' : agree ? 'yes' : 'no' }, agree === false ? 'state hash MISMATCH' : agree ? `state hash agrees on ${nodes.length} nodes` : 'state hash: fewer than two healthy nodes'));
  const byHash = {}; for (const [n, v] of nodes) (byHash[v.statehash] ??= []).push(`${n}@${v.height}`);
  for (const [hsh, ns] of Object.entries(byHash)) el.append(h('div', { class: 'mono' }, h('code', {}, hsh.slice(0, 16) + '…'), ' ', ns.join(' ')));
  const infos = Object.entries(d.yedInfo || {});
  const rej = (y.rejected || []).length;
  el.append(h('div', { class: 'tiles' }, tile('refused blocks', String(rej), 'YED-invalid (yed_getblockverdict on an invalid tip)', rej > 0), tile('quoting pools', String((y.miners || []).length), (y.miners || []).filter((m) => m.eligible).length + ' eligible')));
  for (const r of y.rejected || []) el.append(h('div', { class: 'mono yb-verdict' }, `node ${r.node} refused ${r.hash.slice(0, 12)}…: ${r.verdict?.reason || r.verdict?.verdict || JSON.stringify(r.verdict)}`));
  const unhealthy = infos.filter(([, i]) => !i.healthy);
  for (const [n, i] of unhealthy) el.append(h('div', { class: 'yb-verdict' }, `node ${n} unhealthy: ${i.unhealthyReason || ''}`));
}

// ---------------------------------------------------------------- transactions
function renderTxs(y, s, d) {
  const t = clear($('yb-txs'));
  t.append(h('tr', {}, ...['height', 'txid', 'type', 'path', 'verdict', 'YED in', 'YED out', 'burned', 'fee YEC', 'payee', 'attest fee', 'attest payee', 'mempool'].map((c) => h('th', {}, c))));
  const inMempool = new Set((s.mempool?.txs || []).map((x) => x.txid));
  // the model orders by first sight (a tx seen in the mempool keeps its slot once mined): sort by height here, mempool rows first
  const txs = (y.txs || []).slice().sort((a, b) => (b.height ?? Number.MAX_SAFE_INTEGER) - (a.height ?? Number.MAX_SAFE_INTEGER));
  for (const tx of txs.slice(0, 60)) {
    const mp = inMempool.has(tx.txid) || !tx.height;
    const bad = tx.wouldBeRejected === true || (tx.verdict && tx.verdict !== 'ok' && tx.verdict !== 'unindexed');
    t.append(h('tr', { 'data-bad': bad ? '1' : '0' },
      h('td', {}, tx.height ? String(tx.height) : h('span', { class: 'muted' }, 'mempool')),
      h('td', { class: 'mono' }, tx.txid.slice(0, 12) + '…'),
      h('td', {}, h('span', { class: 'yb-type', style: `--c:${typeColour(tx.type, tx.path)}` }, txLabel(tx))),
      h('td', {}, tx.path || ''), h('td', {}, h('span', { class: 'yb-status', 'data-s': bad ? 'BAD' : 'OK', title: VERDICT_TEXT[tx.verdict] || '' }, tx.verdict || (tx.wouldBeRejected === undefined ? '…' : ''))),
      h('td', {}, yed(tx.yedIn)), h('td', {}, yed(tx.yedOut)), h('td', {}, yed(tx.burned)), h('td', {}, yec(tx.feeZat)), h('td', { class: 'mono' }, tx.payee ? tx.payee.slice(0, 10) + '…' : ''),
      h('td', {}, tx.attestFeeZat ? yec(tx.attestFeeZat) : ''), h('td', { class: 'mono' }, tx.attestPayee ? tx.attestPayee.slice(0, 10) + '…' : ''),
      h('td', {}, mp ? (tx.wouldBeRejected === true ? h('b', { class: 'yb-bad' }, 'would be rejected') : tx.wouldBeRejected === false ? 'would confirm' : '') : '')));
  }
  if (!(y.txs || []).length) t.append(h('tr', {}, h('td', { colspan: 13, class: 'muted' }, 'no Yellowback transactions seen yet')));
  const seen = (d.blocks || []).filter((b) => b.yb?.txs?.length).length;
  $('yb-hint').textContent = `leader node ${y.leader ?? '–'} · ${(y.txs || []).length} Yellowback txs known · ${seen} of the last ${(d.blocks || []).length} blocks carry one`;
}
