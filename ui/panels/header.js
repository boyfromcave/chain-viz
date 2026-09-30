// Header: chain name, nodes up, tip, time since last block (large, coloured past 2× and 4× the
// target spacing), connection state, seq.
import { fmtSecs, short } from '../lib.js';

const $ = (id) => document.getElementById(id);

// Target block spacing: the devnet heartbeat's rate on regtest, else Ycash's 75 s.
export function spacing(store) {
  const rate = store.snap?.devnet?.heartbeat?.rate;
  if (store.snap?.chainName === 'regtest' && rate > 0) return rate;
  return 75;
}

export function lastBlockAt(store) {
  const main = store.snap?.chain?.main || [];
  const tip = main[main.length - 1];
  if (!tip) return undefined;
  return tip.seen || tip.time;   // wall-clock chain-viz saw it, else the block's own timestamp
}

export function render({ store, majority, now }) {
  const s = store.snap;
  $('chain-name').textContent = s?.chainName || '…';
  const conn = $('conn'); conn.dataset.state = store.conn; conn.textContent = store.conn;
  if (!s) return;
  const up = s.nodes.filter((n) => n.up).length;
  $('node-count').textContent = `${up} / ${s.nodes.length}`;
  const tipText = majority ? `${majority.height} · ${short(majority.hash)}` : '–';
  $('tip').textContent = majority && majority.disagreeing.length ? `${tipText} (${majority.disagreeing.length} disagree)` : tipText;
  $('seq').textContent = String(store.seq);
  const sp = spacing(store); const at = lastBlockAt(store);
  const since = at === undefined ? undefined : now - at;
  const el = $('since'); el.textContent = fmtSecs(since);
  el.dataset.level = since === undefined ? '0' : since > 4 * sp ? '2' : since > 2 * sp ? '1' : '0';
  el.title = since === undefined ? '' : since > 4 * sp ? 'more than 4× the target spacing' : since > 2 * sp ? 'more than 2× the target spacing' : 'within 2× the target spacing';
  $('spacing').textContent = `(target ${sp} s${s.devnet?.heartbeat?.rate ? ', heartbeat' : ''})`;
}
