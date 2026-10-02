#!/usr/bin/env bash
# Copyright (c) 2026 The Ycash developers
# Distributed under the MIT software license, see the accompanying
# file LICENSE or https://www.opensource.org/licenses/mit-license.php .
#
# Soak: sample a running chain-viz for N minutes and assert flat memory and a steady RPC rate.
#   qa/soak.sh <url> <pid> <minutes> [interval-secs]
# Every interval: RSS (ps -o rss), /api/health seq, the sum of rpcCalls, and the count of `note`
# events since the last sample that read like an RPC failure. At the end:
#   * RSS in the last half of the run never exceeds 1.15 x the RSS at the end of the first quarter
#     (+ 4 MB of slack for allocator noise) — the model and the event window are bounded (--keep);
#   * every per-interval rpcCalls delta is within [0.5, 2] x the median delta — no error storm,
#     no stall;
#   * RPC failures are rare: at most 2 sample intervals see any, and at most 3 per node in all (a
#     host-wide hiccup makes one note per node at once; a storm makes them every interval).
# Meant for a devnet with a fast heartbeat (`yellowback-devnet heartbeat rate 2`) and a small
# `--keep` so eviction actually runs, e.g.
#   chain-viz --devnet ~/yb-devnet --keep 50 --listen 127.0.0.1:8497 & qa/soak.sh http://127.0.0.1:8497 $! 20
set -euo pipefail
url=${1:?url}; pid=${2:?pid}; minutes=${3:?minutes}; every=${4:-30}
py=${PYTHON:-python3}
samples=$(( minutes * 60 / every ))
out=${SOAK_OUT:-/dev/stdout}
nodes=$(curl -sf "$url/api/health" | "$py" -c 'import json,sys; print(json.load(sys.stdin)["nodes"])')
echo "t,rss_kb,seq,rpc_calls,rpc_notes" > "$out"
last_seq=0
for i in $(seq 1 "$samples"); do
  sleep "$every"
  kill -0 "$pid" 2>/dev/null || { echo "chain-viz pid $pid is gone" >&2; exit 1; }
  rss=$(ps -o rss= -p "$pid" | tr -d ' ')
  health=$(curl -sf "$url/api/health") || { echo "health failed" >&2; exit 1; }
  read -r seq calls <<< "$("$py" -c 'import json,sys; h=json.load(sys.stdin); print(h["seq"], sum(sum(m.values()) for m in h["rpcCalls"].values()))' <<< "$health")"
  notes=$(curl -sf "$url/api/events?since=$last_seq" | "$py" -c 'import json,sys,re; print(sum(1 for e in json.load(sys.stdin) if e.get("kind")=="note" and re.search(r"rpc|unreachable|error", e.get("text",""), re.I)))')
  last_seq=$seq
  echo "$(( i * every )),$rss,$seq,$calls,$notes" >> "$out"
done
"$py" - "$out" "$nodes" <<'EOF'
import csv, statistics, sys
nodes = int(sys.argv[2])
rows = [dict(t=int(r["t"]), rss=int(r["rss_kb"]), seq=int(r["seq"]), calls=int(r["rpc_calls"]), notes=int(r["rpc_notes"])) for r in csv.DictReader(open(sys.argv[1]))]
n = len(rows); ok = True
def check(cond, msg):
    global ok
    print(("ok   " if cond else "FAIL ") + msg)
    ok = ok and cond
base = rows[max(0, n // 4 - 1)]["rss"]
tail = [r["rss"] for r in rows[n // 2:]]
check(max(tail) <= base * 1.15 + 4096, f"rss: first-quarter {base} KB, last-half max {max(tail)} KB (min {min(tail)}), final {rows[-1]['rss']} KB")
deltas = [b["calls"] - a["calls"] for a, b in zip(rows, rows[1:])]
med = statistics.median(deltas) if deltas else 0
bad = [d for d in deltas if not (0.5 * med <= d <= 2 * med) or d <= 0]
check(med > 0 and not bad, f"rpcCalls per interval: median {med}, min {min(deltas) if deltas else '-'}, max {max(deltas) if deltas else '-'}, outliers {bad}")
notes = sum(r["notes"] for r in rows); bursts = sum(1 for r in rows if r["notes"])
check(bursts <= 2 and notes <= 3 * nodes, f"rpc-failure notes over the run: {notes} across {bursts} interval(s), {nodes} nodes")
print(f"{n} samples, seq {rows[0]['seq']} -> {rows[-1]['seq']}, rpcCalls {rows[0]['calls']} -> {rows[-1]['calls']}")
sys.exit(0 if ok else 1)
EOF
