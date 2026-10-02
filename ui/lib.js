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
