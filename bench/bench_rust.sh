#!/usr/bin/env bash
# Rust benchmark: reproduces bench.ps1's matrix for the Rust `ft` binary.
# Same corpora, same channels (20/100/200 Mbit at 0/150ms via a Python WAN proxy),
# same metric (efficiency = goodput / channel capacity). Run on an idle machine.
set -u
REPO="C:/project/github/folder-transfer"
FT="$REPO/rust/target/release/ft.exe"
PROXY="$REPO/bench/proxy.py"
ROOT="C:/ft-bench-rust"
CONN="$ROOT/conn.json"; DST="$ROOT/dst"
PY=python
rm -rf "$ROOT"; mkdir -p "$ROOT"

echo "Building corpora under $ROOT ..."
RATIO=$("$PY" "$REPO/bench/build_corpora.py" "$ROOT" | sed -n 's/^RATIO=//p')
echo "compressible deflate ratio = ${RATIO}x"

IDX=0
MBPS=""
run_cell() { # corpus streams delay_ms rate_mbps
  local corpus="$1" streams="$2" delay="$3" rate="$4"
  IDX=$((IDX+1)); local sp=$((8800+IDX)) pp=$((9000+IDX))
  local so="$ROOT/srv$IDX.out" po="$ROOT/pxy$IDX.out" co="$ROOT/cli$IDX.out"
  rm -rf "$DST" "$CONN"
  "$FT" serve "$corpus" --streams "$streams" --port "$sp" --server-host 127.0.0.1 \
    --no-firewall --idle-seconds 600 --stall-timeout 600 --client-out "$CONN" > "$so" 2>&1 &
  local SRV=$!
  for _ in $(seq 1 400000); do [ -f "$CONN" ] && grep -q '"token"' "$CONN" 2>/dev/null && break; done
  local connport=$sp PXY=""
  if [ "$delay" -gt 0 ] || [ "$(awk "BEGIN{print ($rate>0)}")" = 1 ]; then
    "$PY" "$PROXY" --listen "$pp" --target-port "$sp" --delay-ms "$delay" --rate-mbps "$rate" > "$po" 2>&1 &
    PXY=$!
    for _ in $(seq 1 400000); do grep -q '^proxy ' "$po" 2>/dev/null && break; done
    connport=$pp
  fi
  "$FT" get --config "$CONN" --server 127.0.0.1 --port "$connport" --to "$DST" --streams "$streams" > "$co" 2>&1
  MBPS=$(sed -n 's/.*@ \([0-9.][0-9.]*\) MB\/s.*/\1/p' "$co" | head -1)
  [ -n "$PXY" ] && kill "$PXY" 2>/dev/null
  kill "$SRV" 2>/dev/null; wait "$SRV" 2>/dev/null
  [ -z "$MBPS" ] && MBPS="FAIL"
  echo "    cell$IDX corpus=$(basename "$corpus") streams=$streams delay=${delay} rate=${rate} -> ${MBPS} MB/s"
}

eff() { # mbps cap
  [ "$1" = "FAIL" ] && { echo "FAIL"; return; }
  awk "BEGIN{printf \"%.0f%%\", 100*$1/$2}"
}

echo "== small files (10000 x 4 KB), 4 streams =="
run_cell "$ROOT/tiny"  4 0  0    ; S_LAN=$MBPS
run_cell "$ROOT/tiny"  4 0  2.5  ; S_20=$MBPS
run_cell "$ROOT/tiny"  4 75 2.5  ; S_20L=$MBPS
run_cell "$ROOT/tiny"  4 0  12.5 ; S_100=$MBPS
run_cell "$ROOT/tiny"  4 75 12.5 ; S_100L=$MBPS
run_cell "$ROOT/tiny"  4 0  25   ; S_200=$MBPS
run_cell "$ROOT/tiny"  4 75 25   ; S_200L=$MBPS

echo "== large incompressible (4 MB random files), 4 streams =="
run_cell "$ROOT/inc_l" 4 0  0    ; I_LAN=$MBPS
run_cell "$ROOT/inc_s" 4 0  2.5  ; I_20=$MBPS
run_cell "$ROOT/inc_s" 4 75 2.5  ; I_20L=$MBPS
run_cell "$ROOT/inc_m" 4 0  12.5 ; I_100=$MBPS
run_cell "$ROOT/inc_m" 4 75 12.5 ; I_100L=$MBPS
run_cell "$ROOT/inc_l" 4 0  25   ; I_200=$MBPS
run_cell "$ROOT/inc_l" 4 75 25   ; I_200L=$MBPS

echo "== large compressible (4 MB text files), 4 streams =="
run_cell "$ROOT/cmp_l" 4 0  0    ; C_LAN=$MBPS
run_cell "$ROOT/cmp_s" 4 0  2.5  ; C_20=$MBPS
run_cell "$ROOT/cmp_s" 4 75 2.5  ; C_20L=$MBPS
run_cell "$ROOT/cmp_m" 4 0  12.5 ; C_100=$MBPS
run_cell "$ROOT/cmp_m" 4 75 12.5 ; C_100L=$MBPS
run_cell "$ROOT/cmp_l" 4 0  25   ; C_200=$MBPS
run_cell "$ROOT/cmp_l" 4 75 25   ; C_200L=$MBPS

echo "== LAN single-stream (streams=1) for the modes comparison =="
run_cell "$ROOT/tiny"  1 0 0 ; S_LAN1=$MBPS
run_cell "$ROOT/inc_l" 1 0 0 ; I_LAN1=$MBPS
run_cell "$ROOT/cmp_l" 1 0 0 ; C_LAN1=$MBPS

echo
echo "RESULTS_BEGIN"
echo "## Efficiency by data type, channel and ping (Rust ft, release; 4 streams + adaptive)"
echo
echo "| data type | 20 Mbit | 20 Mbit +150ms | 100 Mbit | 100 Mbit +150ms | 200 Mbit | 200 Mbit +150ms |"
echo "|---|---|---|---|---|---|---|"
echo "| small files (10000 x 4 KB) | $(eff "$S_20" 2.5) | $(eff "$S_20L" 2.5) | $(eff "$S_100" 12.5) | $(eff "$S_100L" 12.5) | $(eff "$S_200" 25) | $(eff "$S_200L" 25) |"
echo "| large, incompressible (4 MB, random) | $(eff "$I_20" 2.5) | $(eff "$I_20L" 2.5) | $(eff "$I_100" 12.5) | $(eff "$I_100L" 12.5) | $(eff "$I_200" 25) | $(eff "$I_200L" 25) |"
echo "| large, compressible (4 MB, text ${RATIO}x) | $(eff "$C_20" 2.5) | $(eff "$C_20L" 2.5) | $(eff "$C_100" 12.5) | $(eff "$C_100L" 12.5) | $(eff "$C_200" 25) | $(eff "$C_200L" 25) |"
echo
echo "## LAN (loopback) raw goodput, MB/s"
echo
echo "| data type | single-stream | 4 streams |"
echo "|---|---|---|"
echo "| small files (10000 x 4 KB) | ${S_LAN1} | ${S_LAN} |"
echo "| large, incompressible | ${I_LAN1} | ${I_LAN} |"
echo "| large, compressible | ${C_LAN1} | ${C_LAN} |"
echo "RESULTS_END"
rm -rf "$ROOT" 2>/dev/null
