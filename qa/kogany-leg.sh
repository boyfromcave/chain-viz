#!/usr/bin/env bash
# Copyright (c) 2026 The Ycash developers
# Distributed under the MIT software license, see the accompanying
# file LICENSE or https://www.opensource.org/licenses/mit-license.php .
#
# The kogany leg (workspace kogany plan W9-c; report-only): chain-viz, polling (kogany has no ZMQ
# and no -blocknotify, kogany plan K-17), with a regtest kogany node as its only node.
#   qa/kogany-leg.sh
# env: KOGANYD (kogany's koganyd --features ycash: kogany's koganyd-bin artifact), YCASHD (ycash6's
#      ycashd: ycash6's node-bin), CHAINVIZ_BIN (default target/release/chain-viz), KOGANY_RIG_DIR
#      (default a temporary directory), PYTHON (default python3), NODE (default node), CV_PORT
#      (default 8494), CV_LEG_NODE (kogany, default; or sidecar: the same checks with the ycash6
#      sidecar as the only node, the reference column)
# qa/kogany-rig.py brings up kogany and its ycash6 test rig (a ycashd `connect=` kogany that mines
# past the Vault activation; the YED module live on both with one attestor set id). chain-viz then
# watches kogany alone, and the checks are: /api/health ok with the node up and its tip kogany's
# best block; the node's Yellowback flag true and its vault panel available (vault_getinfo); no
# node error; three more blocks followed; the UI smoke (qa/ui-smoke.mjs) against the live instance.
# Exit 0 when every check passes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PY="${PYTHON:-python3}"
# ZAKURAD: the variable's name before kogany's daemon became koganyd (kogany dd834da8b).
KOGANYD="${KOGANYD:-${ZAKURAD:-}}"
: "${KOGANYD:?KOGANYD: kogany koganyd (--features ycash)}"
: "${YCASHD:?YCASHD: ycash6 ycashd}"
CV="${CHAINVIZ_BIN:-$ROOT/target/release/chain-viz}"
DIR="${KOGANY_RIG_DIR:-$(mktemp -d)/rig}"
PORT="${CV_PORT:-8494}"
rig() { "$PY" "$ROOT/qa/kogany-rig.py" --dir "$DIR" "$@"; }
cvpid=
cleanup() { [ -n "$cvpid" ] && kill "$cvpid" 2>/dev/null; rig down || true; }
trap cleanup EXIT
rig up --koganyd "$KOGANYD" --ycashd "$YCASHD"
case "${CV_LEG_NODE:-kogany}" in
  kogany) IDX=1 ;; sidecar) IDX=0 ;; *) echo "kogany-leg: CV_LEG_NODE is kogany or sidecar" >&2; exit 2 ;;
esac
url=$("$PY" -c 'import json,sys; e=json.load(open(sys.argv[1]))["rpc"][sys.argv[2]]; print("http://%s:%s@127.0.0.1:%d/" % (e["user"], e["password"], e["port"]))' "$DIR/rig.json" "$IDX")
"$CV" --nodes "$url" --listen "127.0.0.1:$PORT" --poll 1 > "$DIR/chain-viz.log" 2>&1 &
cvpid=$!
status=0
"$PY" - "$DIR" "$PORT" "$ROOT/qa/kogany-rig.py" "$IDX" <<'PYEOF' || status=1
import base64, json, subprocess, sys, time, urllib.request
rigdir, port, rigpy, idx = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
base = 'http://127.0.0.1:%d' % port
rig = json.load(open(rigdir + '/rig.json'))
def node(method, *params):
    body = json.dumps({'jsonrpc': '1.0', 'id': 1, 'method': method, 'params': list(params)}).encode()
    e = rig['rpc'][idx]
    auth = base64.b64encode(('%s:%s' % (e['user'], e['password'])).encode()).decode()
    req = urllib.request.Request(e['url'], data=body, headers={'content-type': 'application/json',
                                                                'authorization': 'Basic ' + auth})
    return json.loads(urllib.request.urlopen(req, timeout=30).read())['result']
def get(path):
    return json.loads(urllib.request.urlopen(base + path, timeout=10).read())
fails = []
def check(name, ok, detail=''):
    print('%s %s%s' % ('pass' if ok else 'FAIL', name, (': ' + str(detail)[:300]) if detail else ''))
    if not ok:
        fails.append(name)
def settle(best, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            h = get('/api/health')
            if (h.get('tip') or {}).get('hash') == best and h.get('nodesUp') == 1:
                return
        except Exception:
            pass
        time.sleep(0.5)
for round_ in ('start', 'after 3 blocks'):
    if round_ != 'start':
        subprocess.run([sys.executable, rigpy, '--dir', rigdir, 'mine', '3'], check=True)
    best = {'hash': node('getbestblockhash'), 'height': node('getblockcount')}
    settle(best['hash'])
    health, snap = get('/api/health'), get('/api/snapshot')
    n = (snap.get('nodes') or [{}])[0]
    check('%s: health ok, node up' % round_, health.get('ok') and health.get('nodesUp') == 1,
          'ok %s, nodesUp %s' % (health.get('ok'), health.get('nodesUp')))
    tip = health.get('tip') or {}
    check('%s: tip = the node best block %d' % (round_, best['height']),
          tip.get('hash') == best['hash'] and tip.get('height') == best['height'], tip)
    check('%s: Yellowback flag true' % round_, n.get('yellowback') is True, n.get('yellowback'))
    vault = snap.get('vault') or {}
    check('%s: vault panel available' % round_, n.get('vault') is True and vault.get('available') is True,
          'nodes[0].vault %r, vault.available %r' % (n.get('vault'), vault.get('available')))
    check('%s: no node error' % round_, not n.get('error'), n.get('error') or '')
calls = (get('/api/health').get('rpcCalls') or {}).get('0', {})
print('chain-viz RPC calls on the node: ' + ', '.join('%s %d' % kv for kv in sorted(calls.items())))
sys.exit(1 if fails else 0)
PYEOF
"${NODE:-node}" "$ROOT/qa/ui-smoke.mjs" "http://127.0.0.1:$PORT" || status=1
exit $status
