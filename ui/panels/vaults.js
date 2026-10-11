// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

// Vault primitive panel (upgrade plan §12 P6): signer sets, vaults and intents by tag, their
// spends (a cancel is an intent spent back into its vault before maturity) and the `YV` acts.
// Self-contained like health.js: builds its own <section>, fetches GET /api/vaults once per new
// block and on vault_spend / set_act / set_state events. Field names are the node's
// (ycash-dd doc/vault-rpc.md: lowercase), plus the collector's `rate`, `yed`, `blocksToRelease`.
import { h, clear, fmtInt, pqSchemeShort, shortAddr } from '../lib.js';

const $ = (id) => document.getElementById(id);
const yec = (v) => (v === null || v === undefined ? '–' : Number(v).toLocaleString(undefined, { maximumFractionDigits: 8 }));
const yecZat = (z) => (z === null || z === undefined ? '–' : yec(z / 1e8));
const id8 = (x) => (x ? x.slice(0, 8) + '…' : '–');
const op = (o) => (o ? `${o.slice(0, 10)}…:${o.split(':')[1] ?? ''}` : '–');

let local = { data: null, fetchedHeight: -1, seenSeq: 0 };
let inflight = null;
async function load() {
  if (inflight) return inflight;
  inflight = fetch('/api/vaults').then((r) => r.json()).then((d) => { if (!d) return; local.data = d; local.fetchedHeight = d.tip?.height ?? -1; }).catch(console.error).finally(() => { inflight = null; });
  return inflight;
}

export function init() {
  if (document.head && !document.querySelector('link[href="/ui/panels/vaults.css"]')) document.head.append(h('link', { rel: 'stylesheet', href: '/ui/panels/vaults.css' }));
  const main = document.querySelector?.('main.grid') || document.body;
  const section = h('section', { id: 'panel-vaults', class: 'panel span-2' },
    h('div', { class: 'panel-head' }, h('h2', {}, 'Vault primitive'), h('span', { id: 'vp-hint', class: 'hint' }, 'vault_getinfo · set_list · set_getinfo · vault_list — read-only')),
    h('div', { id: 'vp-tiles', class: 'tiles' }),
    h('div', { class: 'vp-grid' },
      h('div', { class: 'vp-card vp-wide' }, h('h3', {}, 'Signer sets'), h('span', { class: 'hint' }, 'predicates at the next block · rate: unlocked this epoch vs epochBasis × rateLimitBps · ★ = the YED attestor set (yed_getinfo.upgrade.attestorSetId)'), h('table', { id: 'vp-sets', class: 'table' }), h('div', { id: 'vp-members' })),
      h('div', { class: 'vp-card' }, h('h3', {}, 'By application tag'), h('table', { id: 'vp-tags', class: 'table' })),
      h('div', { class: 'vp-card' }, h('h3', {}, 'Pending intents'), h('span', { class: 'hint' }, 'blocks to release = matureheight − next height; cancellable while the delay runs'), h('table', { id: 'vp-intents', class: 'table' })),
      h('div', { class: 'vp-card vp-wide' }, h('h3', {}, 'Vaults'), h('table', { id: 'vp-vaults', class: 'table' })),
      h('div', { class: 'vp-card vp-wide' }, h('h3', {}, 'Spends and cancels'), h('span', { class: 'hint' }, 'from the spending input\'s selector in each fetched block; "inferred" = read from vault_list alone'), h('table', { id: 'vp-spends', class: 'table' })),
      h('div', { class: 'vp-card vp-wide' }, h('h3', {}, 'Set acts'), h('table', { id: 'vp-acts', class: 'table' }))),
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
    if (e.kind === 'block' || e.kind === 'vault_spend' || e.kind === 'set_act' || e.kind === 'set_state' || e.kind === 'reorg') refetch = true;
  }
  const tip = s.chain?.majority?.height ?? -1;
  if (refetch || tip > local.fetchedHeight || !local.data) load();
  const d = local.data; if (!d) return;
  renderTiles(d);
  renderSets(d);
  renderTags(d);
  renderIntents(d);
  renderVaults(d);
  renderSpends(d);
  renderActs(d);
}

function tile(label, value, sub, flag) { return h('div', { class: 'tile', 'data-flag': flag ? '1' : '0' }, h('div', { class: 'label' }, label), h('div', { class: 'value' }, value), sub ? h('div', { class: 'sub' }, sub) : null); }
const yedMark = (d, setid) => (setid && setid === d.attestorSetId ? h('span', { class: 'vp-yed', title: 'the YED attestor set' }, '★ YED') : null);

function renderTiles(d) {
  const el = clear($('vp-tiles'));
  if (d.available === false) { el.append(h('span', { class: 'muted' }, 'no node answers vault_getinfo (the vault upgrade\'s RPCs are absent)')); return; }
  const i = d.info; if (!i) { el.append(h('span', { class: 'muted' }, 'waiting for vault_getinfo')); return; }
  el.append(
    tile('upgrade', i.active ? 'ACTIVE' : 'pending', `branch ${i.branchid} · from ${i.activationheight < 0 ? 'unscheduled' : i.activationheight}`),
    tile('sets', fmtInt(i.sets), d.attestorSetId ? `YED set ${id8(d.attestorSetId)}` : 'no YED attestor set'),
    tile('vaults', fmtInt(i.vaults)), tile('intents', fmtInt(i.intents), '', i.intents > 0),
    tile('locked YEC', yec(i.lockedvalue)),
    tile('state hash', i.statehash ? i.statehash.slice(0, 12) + '…' : '–', `db tip ${i.dbtip?.height ?? '–'} · leader node ${d.leader ?? '–'}`),
  );
  $('vp-hint').textContent = `vault_getinfo at ${i.height} · next height ${d.nextHeight ?? '–'} · ${(d.spends || []).filter((x) => x.action === 'cancel').length} cancel(s) seen`;
}

function rateCell(set) {
  const r = set.rate || {};
  if (!r.limited) return h('span', { class: 'muted' }, 'no limit');
  const pct = r.usedBps === null || r.usedBps === undefined ? 0 : Math.min(100, r.usedBps / 100);
  return h('span', { class: 'vp-rate', title: `epoch ${set.epoch}: ${yec(r.used)} of ${yec(r.limit)} YEC (basis ${yec(r.basis)} × ${set.ratelimitbps} bps); available ${yec(set.unlockavailable)}` },
    h('span', { class: 'bar', 'data-full': pct >= 100 ? '1' : '0' }, h('i', { style: `width:${pct}%` })), ` ${yec(r.used)} / ${yec(r.limit)}`);
}
function renderSets(d) {
  const t = clear($('vp-sets'));
  t.append(h('tr', {}, ...['set', 'seats', 'u / c / s', 'open', 'members', 'state', 'rate (epoch)', 'liveness', 'bond min', 'maturity', 'created', 'wind-down'].map((c) => h('th', {}, c))));
  for (const s of d.sets || []) {
    const state = s.released ? (s.dormant ? 'DORMANT' : 'RELEASED') : 'live';
    t.append(h('tr', { 'data-yed': s.yed ? '1' : '0' },
      h('td', { class: 'mono' }, id8(s.setid), ' ', yedMark(d, s.setid)),
      h('td', {}, String(s.seats)), h('td', {}, `${s.unlockthreshold} / ${s.cancelthreshold} / ${s.slashthreshold}`), h('td', {}, s.open ? 'yes' : 'no'),
      h('td', {}, `${s.current ?? '–'} current · ${s.active ?? '–'} active · ${s.members ?? '–'}`),
      h('td', {}, h('span', { class: 'vp-state', 'data-s': state }, state)),
      h('td', {}, rateCell(s)), h('td', {}, String(s.livenesswindow)), h('td', {}, yec(s.bondmin)), h('td', {}, String(s.maturity)),
      h('td', {}, String(s.createheight)), h('td', {}, s.winddownheight ? String(s.winddownheight) : '–')));
  }
  if (!(d.sets || []).length) t.append(h('tr', {}, h('td', { colspan: 12, class: 'muted' }, 'no signer sets')));
  const m = clear($('vp-members'));
  for (const s of d.sets || []) {
    if (!s.memberlist) continue;
    const mt = h('table', { class: 'table vp-memberlist' });
    mt.append(h('tr', {}, ...['member key', 'status', 'current', 'live', 'joined', 'last act', 'bond YEC', 'bond locktime', 'frozen'].map((c) => h('th', {}, c))));
    for (const x of s.memberlist) mt.append(h('tr', {},
      h('td', { class: 'mono' }, x.key.slice(0, 12) + '…'), h('td', {}, h('span', { class: 'vp-state', 'data-s': x.status }, x.status)),
      h('td', {}, x.current ? 'yes' : 'no'), h('td', {}, h('span', { class: 'vp-state', 'data-s': x.live ? 'live' : 'DORMANT' }, x.live ? 'live' : 'dormant')),
      h('td', {}, String(x.joinheight)), h('td', {}, String(x.lastact)), h('td', {}, yec(x.bondvalue)), h('td', {}, String(x.bondlocktime)), h('td', {}, x.bondfrozen ? h('b', { class: 'vp-bad' }, 'FROZEN') : 'no')));
    m.append(h('details', { open: s.yed || (d.sets || []).length <= 2 }, h('summary', {}, `members of ${id8(s.setid)}`, s.yed ? ' (YED attestors)' : '', ` — ${s.memberlist.length}`), mt));
  }
}

function renderTags(d) {
  const t = clear($('vp-tags'));
  t.append(h('tr', {}, ...['tag', 'vaults', 'intents', 'value YEC'].map((c) => h('th', {}, c))));
  for (const g of d.byTag || []) t.append(h('tr', {}, h('td', { class: 'mono' }, g.tagtext ? `${g.tagtext.replace(/\0/g, '')} (${g.tag})` : g.tag), h('td', {}, fmtInt(g.vaults)), h('td', {}, fmtInt(g.intents)), h('td', {}, yecZat(g.valuezat))));
  if (!(d.byTag || []).length) t.append(h('tr', {}, h('td', { colspan: 4, class: 'muted' }, 'no vaults or intents')));
}

function renderIntents(d) {
  const t = clear($('vp-intents'));
  t.append(h('tr', {}, ...['intent', 'tag', 'value YEC', 'matures', 'blocks to release', 'cancellable', 'cancel set'].map((c) => h('th', {}, c))));
  for (const x of d.intents || []) t.append(h('tr', { 'data-mature': x.mature ? '1' : '0' },
    h('td', { class: 'mono' }, op(x.outpoint)), h('td', {}, x.tagtext || x.tag), h('td', {}, yec(x.value)), h('td', {}, String(x.matureheight)),
    h('td', {}, x.blocksToRelease > 0 ? String(x.blocksToRelease) : h('b', {}, 'releasable')), h('td', {}, x.cancellable ? 'yes' : 'no'), h('td', { class: 'mono' }, id8(x.cancelsetid), ' ', yedMark(d, x.cancelsetid))));
  if (!(d.intents || []).length) t.append(h('tr', {}, h('td', { colspan: 7, class: 'muted' }, 'no pending intents')));
}

function renderVaults(d) {
  const t = clear($('vp-vaults'));
  t.append(h('tr', {}, ...['vault', 'tag', 'value YEC', 'set', 'cancel set', 'delay', 'owner', 'owner height', 'app height', 'confirmed'].map((c) => h('th', {}, c))));
  for (const x of (d.vaults || []).slice(0, 100)) t.append(h('tr', {},
    h('td', { class: 'mono' }, op(x.outpoint)), h('td', {}, x.tagtext || x.tag), h('td', {}, yec(x.value)), h('td', { class: 'mono' }, id8(x.setid), ' ', yedMark(d, x.setid)),
    h('td', { class: 'mono' }, x.cancelsetid === x.setid ? 'same' : id8(x.cancelsetid)), h('td', {}, String(x.delay)), ownerCell(x), h('td', {}, String(x.ownerheight)), h('td', {}, x.appheight ? String(x.appheight) : 'off'), h('td', {}, String(x.height))));
  if (!(d.vaults || []).length) t.append(h('tr', {}, h('td', { colspan: 10, class: 'muted' }, 'no vaults')));
}

const ACTION_TEXT = { unlock: 'UNLOCK → intents', release: 'RELEASE', cancel: 'CANCEL', owner: 'OWNER', 'owner-released': 'OWNER (released set)', app: 'APP (module)', spent: 'spent' };
function renderSpends(d) {
  const t = clear($('vp-spends'));
  t.append(h('tr', {}, ...['height', 'action', 'of', 'outpoint', 'tag', 'value YEC', 'spending tx', 'value back to / re-locked', ''].map((c) => h('th', {}, c))));
  for (const x of (d.spends || []).slice(0, 60)) t.append(h('tr', { 'data-action': x.action },
    h('td', {}, String(x.height ?? '–')), h('td', {}, h('span', { class: 'vp-action', 'data-a': x.action }, ACTION_TEXT[x.action] || x.action)), h('td', {}, x.kind),
    h('td', { class: 'mono' }, op(x.outpoint)), h('td', {}, x.tagtext || x.tag || ''), h('td', {}, yecZat(x.valuezat)), h('td', { class: 'mono' }, x.txid ? x.txid.slice(0, 12) + '…' : '–'),
    h('td', { class: 'mono' }, x.returnedTo ? op(x.returnedTo) : ''), h('td', { class: 'muted' }, x.inferred ? 'inferred' : x.selector ? `selector ${x.selector}${x.pqscheme ? ' · ' + pqSchemeShort(x.pqscheme) + ' signature' : ''}` : '')));
  if (!(d.spends || []).length) t.append(h('tr', {}, h('td', { colspan: 9, class: 'muted' }, 'no vault or intent spends seen yet')));
}

function renderActs(d) {
  const t = clear($('vp-acts'));
  t.append(h('tr', {}, ...['height', 'act', 'set', 'txid'].map((c) => h('th', {}, c))));
  for (const a of (d.acts || []).slice(0, 40)) t.append(h('tr', {}, h('td', {}, String(a.height)), h('td', {}, a.type), h('td', { class: 'mono' }, id8(a.setid), ' ', yedMark(d, a.setid)), h('td', { class: 'mono' }, a.txid.slice(0, 12) + '…')));
  if (!(d.acts || []).length) t.append(h('tr', {}, h('td', { colspan: 4, class: 'muted' }, 'no set acts seen yet')));
}

// The owner of a V or I (the quantum line, doc/vault-rpc.md "Owners"): its post-quantum scheme and PQ address
// (`owneraddress`, added by the collector from the pqkeyid `owner`).
function ownerCell(x) {
  if (!x.owner) return h('td', { class: 'muted' }, '–');
  return h('td', { class: 'mono', title: `owner ${x.ownerschemename || 'scheme ' + x.ownerscheme}\n${x.owneraddress || ''}\npqkeyid ${x.owner}` }, pqSchemeShort(x.ownerscheme), ' ', shortAddr(x.owneraddress || x.owner));
}
