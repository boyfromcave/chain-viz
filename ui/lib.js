// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

// Small DOM/SVG helpers shared by every panel. No framework, no build step.
const SVG = 'http://www.w3.org/2000/svg';

export function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === null) continue;
    if (k === 'class') el.className = v;
    else if (k.startsWith('data-')) el.setAttribute(k, v);
    else if (k in el) el[k] = v;
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) if (c !== undefined && c !== null) el.append(c);
  return el;
}

export function svg(tag, attrs = {}, ...children) {
  const el = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) if (v !== undefined && v !== null) el.setAttribute(k, v);
  for (const c of children.flat()) if (c !== undefined && c !== null) el.append(c);
  return el;
}

export function clear(el) { while (el.firstChild) el.firstChild.remove(); return el; }

export const short = (hash) => (hash ? hash.slice(-8) : '');
export const fmtInt = (n) => (n === undefined || n === null ? '–' : Number(n).toLocaleString());
export function fmtBytes(b) {
  if (b === undefined || b === null) return '–';
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1048576).toFixed(2)} MB`;
}
export function fmtSecs(s) {
  if (s === undefined || s === null || !isFinite(s)) return '–';
  s = Math.max(0, Math.round(s));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} m ${String(s % 60).padStart(2, '0')} s`;
  return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, '0')} m`;
}
export const fmtTime = (ts) => new Date(ts * 1000).toLocaleTimeString([], { hour12: false });
export const fmtYec = (v) => (v === undefined || v === null ? '–' : Number(v).toFixed(8).replace(/0+$/, '').replace(/\.$/, ''));
export const now = () => Date.now() / 1000;

export function median(xs) {
  if (!xs.length) return undefined;
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

// Yellowback tx types → categorical slots (fixed order, never cycled; unknown types fold to "other").
// Keys are the node's own type names (yed_gettxinfo / classify.rs, lowercase); a CLAIM is
// `redeem` with path "claim", so callers pass ybSlotKey(yb) rather than the bare type (C-F10).
export const YB_TYPE_SLOT = {
  mint: 's1', transfer: 's3', redeem: 's2', claim: 's7', sweep: 's5', void: 's8',
  register: 's4', equivocation: 's4', revive: 's4', notice: 's6',
};
export const ybSlotKey = (yb) => (yb?.type === 'redeem' && yb?.path === 'claim' ? 'claim' : yb?.type);
export const slotVar = (slot) => `var(--${slot})`;

// Post-quantum owner schemes (the quantum line, rpcversion 7): yed_getvault.ownerScheme, vault_list ownerscheme.
export const PQ_SCHEMES = { 1: 'SLH-DSA-SHA2-128s', 2: 'FN-DSA-512' };
export const pqSchemeShort = (n) => ({ 1: 'SLH-DSA', 2: 'FN-DSA' }[n] || (n === undefined || n === null ? '–' : `scheme ${n}`));
// A 53-character PQ address shortened for a table cell (the full one goes in the title).
// The fee structure (rpcversion 8, yed_getinfo.params.fees / maxMintCents / supplyCap; ycash-dd
// doc/yellowback-rpc.md "The fee structure"): the protocol fee is dollars of the debt by band (like tax
// brackets) at the vault's stored pMint, the network fee dollars per KB at the attested tip price. Pure
// text, null on a node before rpcversion 8 (no params.fees).
const dollars = (cents) => `$${(cents / 100).toLocaleString('en-US', { maximumFractionDigits: 2 })}`;
export function feeParamsText(p) {
  const f = p?.fees; if (!f) return null;
  const pr = f.protocol || {}, net = f.network || {};
  const bands = (pr.bands || []).map((b, i, a) => `${b.bps / 100} % ${b.uptoCents === null || b.uptoCents === undefined ? `above ${a[i - 1] ? dollars(a[i - 1].uptoCents) : '$0'}` : `to ${dollars(b.uptoCents)}`}`).join(' · ');
  const protocol = `protocol ${bands || '–'}, floor ${pr.floorCents === undefined ? '–' : dollars(pr.floorCents)} at ${pr.priceSource || 'pMint'}, attestor share ${pr.attestFeeBps === undefined ? '–' : pr.attestFeeBps / 100 + ' %'}`;
  const usdKB = (m) => (m === undefined || m === null ? '–' : `$${(m / 1e6).toFixed(2)}`);
  const at = net.priceMicroUsd ? `at $${(net.priceMicroUsd / 1e6).toFixed(4)}` : `no price: fallback ${net.fallbackZatPerKB ?? '–'} zat/KB`;
  const yb = net.yellowback || {}, plain = net.plain || {};
  const cap = yb.maxTxFeeZat === undefined ? '–' : `${(yb.maxTxFeeZat / 1e8).toLocaleString('en-US', { maximumFractionDigits: 8 })} YEC`;
  const network = `network plain ${usdKB(plain.usdPerKBMicro)}/KB (${plain.zatPerKB ?? '–'} zat/KB), vault/YED ${usdKB(yb.usdPerKBMicro)}/KB (${yb.zatPerKB ?? '–'} zat/KB) ${at}, floor ${net.floorZatPerKB ?? '–'} zat/KB, cap ${cap}`;
  const sc = p.supplyCap || {};
  const supply = `max mint ${p.maxMintCents === undefined ? '–' : dollars(p.maxMintCents)} · supply cap ${sc.capCents === null || sc.capCents === undefined ? '–' : dollars(sc.capCents)} (circulating ${sc.circulatingZat === undefined ? '–' : (sc.circulatingZat / 1e8).toLocaleString('en-US', { maximumFractionDigits: 0 }) + ' YEC'}) · one vault ≤ ${sc.maxVaultCents === null || sc.maxVaultCents === undefined ? `max(${(sc.maxVaultShareBps ?? 0) / 100} % of the cap, ${sc.vaultShareFloorCents === undefined ? '–' : dollars(sc.vaultShareFloorCents)})` : dollars(sc.maxVaultCents)}`;
  return { protocol, network, supply, early: (f.earlyRedeemFeeBps || []).map((b, i) => `${'ABC'[i] || i} ${b / 100} %`).join(' / ') };
}
// The attestor rules (owner 2026-10-11, D-Q-30; rpcversion 8, no bump; ycash-dd doc/yellowback-rpc.md
// yed_getinfo / yed_listattestors / yed_withdrawbond). Pure, every field optional: an older node lacks them.
const yecText = (zat) => (zat === undefined || zat === null ? '–' : `${(zat / 1e8).toLocaleString('en-US', { maximumFractionDigits: 8 })} YEC`);
// params.attest's bond minimum: bondMinZat is the floor a SET_JOIN in the next block must meet,
// clamp(⌈collateralZat · bondShareBps / (10⁴ · nSlots)⌉, bondMinFloorZat, bondMinCapZat). Before the rule
// bondMinZat was a constant (no floor/cap fields).
export function bondMinText(attest) {
  if (!attest || attest.bondMinZat === undefined) return null;
  const now = `bond min ${yecText(attest.bondMinZat)}`;
  if (attest.bondMinFloorZat === undefined) return { short: now, long: `${now} (a constant on this node)` };
  const at = attest.bondMinZat <= attest.bondMinFloorZat ? 'at the floor' : attest.bondMinZat >= attest.bondMinCapZat ? 'at the cap' : 'collateral share';
  const share = attest.bondShareBps === undefined ? '–' : `${attest.bondShareBps / 100} %`;
  return {
    short: `${now} (${at}) · floor ${yecText(attest.bondMinFloorZat)} · cap ${yecText(attest.bondMinCapZat)}`,
    long: `${now} for a join in the next block (${at}): clamp(${share} of YED vault collateral ÷ slots, floor ${yecText(attest.bondMinFloorZat)}, cap ${yecText(attest.bondMinCapZat)})`,
  };
}
// The seat rule: `seats` seats ranked by seatWeight (bond × a log tenure factor 1.0 → 1.9 at seatAgeCap), a seated
// attestor counting ×(1 + seatMarginBps); a join into the full set evicts the non-live member, else the least seat
// weight when the new bond beats it by evictMarginBps.
export function seatRuleText(attest) {
  if (!attest || attest.seats === undefined) return null;
  const pc = (b) => (b === undefined ? '–' : `${b / 100} %`);
  return `${attest.seats} seats · tenure unit ${attest.seatAgeUnit ?? '–'} blocks, cap ${attest.seatAgeCap ?? '–'} (×1.9) · seated margin ${pc(attest.seatMarginBps)} · eviction margin ${pc(attest.evictMarginBps)}`;
}
// yed_listattestors.seatWeight: a decimal string in Q16 (zat × 65,536); null when absent or not an integer.
export function seatWeightZat(a) {
  const s = a?.seatWeight;
  if (typeof s !== 'string' || !/^\d+$/.test(s)) return null;
  return Number(BigInt(s) / 65536n);
}
// The bond exit delay: a WITHDRAWN record (it left with set_leave, or was evicted) whose bond is not frozen and
// not yet spent is spendable from statusHeight + bondMaturity (the set's maturity), and not before its own CLTV
// bondLocktime (yed_withdrawbond's bond-locked). null for any other record, or when the node gives no
// bondMaturity. `locked`: the next block is still before the unlock height.
export function attestorExit(a, attest, nextHeight) {
  if (!a || a.status !== 'WITHDRAWN' || a.bondFrozen || (a.bondSpentHeight !== null && a.bondSpentHeight !== undefined)) return null;
  if (attest?.bondMaturity === undefined || a.statusHeight === undefined || a.statusHeight === null) return null;
  const unlockAt = Math.max(a.statusHeight + attest.bondMaturity, a.bondLocktime ?? 0);
  return { unlockAt, locked: nextHeight === undefined || nextHeight === null ? null : nextHeight < unlockAt };
}
export const shortAddr = (a) => (a ? (a.length > 20 ? `${a.slice(0, 8)}…${a.slice(-6)}` : a) : '–');

// One shared tooltip. show(html-free text, x, y).
const tip = () => document.getElementById('tooltip');
export function tooltip(el, textFn) {
  el.addEventListener('mousemove', (e) => {
    const t = tip(); t.textContent = textFn(); t.hidden = false;
    const x = Math.min(e.clientX + 12, window.innerWidth - t.offsetWidth - 8);
    const y = Math.min(e.clientY + 12, window.innerHeight - t.offsetHeight - 8);
    t.style.left = `${x}px`; t.style.top = `${y}px`;
  });
  el.addEventListener('mouseleave', () => { tip().hidden = true; });
}
