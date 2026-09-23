#!/usr/bin/env bash
# Reproduces the numbers in docs/benchmarks.md. Nothing here spends real provider quota: the Codex
# measurements read structured, read-only metadata (rate limits, doctor diagnostics) against an
# already-authenticated account; the Claude-involving measurements use a fake `claude` executable
# (crates/relay-cli/tests/benchmark.rs), never a real account.
#
#   scripts/benchmark.sh              # CLI startup latency + idle-supervision + handoff-latency
#   scripts/benchmark.sh --with-codex # the above, plus the two real-Codex app-server/doctor checks
#                                      # (needs RELAY_BENCH_CODEX_HOME set to a real, authenticated
#                                      # profile's CODEX_HOME, and a real `codex` on PATH)
#
# Always builds and measures a --release binary: a debug build's dispatch overhead is not
# representative of what Homebrew ships, and docs/benchmarks.md's numbers are release-build numbers.
set -euo pipefail
cd "$(dirname "$0")/.."

with_codex=0
for arg in "$@"; do
  case "$arg" in
    --with-codex) with_codex=1 ;;
    -h|--help)
      sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

echo "== building release binary =="
cargo build --release -p relay-cli

BIN="target/release/relay"

echo
echo "== 1. CLI startup latency =="
echo "-- cold-ish (first execution of this exact binary path since the last rebuild) --"
/usr/bin/time -l "$BIN" --version 2>&1 | sed -n '1,3p'
echo "-- warm, n=50 --"
python3 - "$BIN" <<'PYEOF'
import subprocess, sys, time
bin_path = sys.argv[1]
times = []
for _ in range(50):
    t0 = time.perf_counter()
    subprocess.run([bin_path, "--version"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    times.append(time.perf_counter() - t0)
times.sort()
n = len(times)
print(f"n={n} min={times[0]*1000:.2f}ms median={times[n//2]*1000:.2f}ms p90={times[int(n*0.9)]*1000:.2f}ms max={times[-1]*1000:.2f}ms")
PYEOF

echo
echo "== 2-4. idle supervision overhead + handoff latency (fake Claude fixture) =="
cargo test --release -p relay-cli --test benchmark -- --ignored --nocapture --test-threads=1

if [ "$with_codex" -eq 1 ]; then
  if [ -z "${RELAY_BENCH_CODEX_HOME:-}" ]; then
    echo
    echo "== 5. skipped: --with-codex needs RELAY_BENCH_CODEX_HOME set to a real CODEX_HOME ==" >&2
    exit 1
  fi
  echo
  echo "== 5a. real codex app-server account/rateLimits/read =="
  cargo test --release -p relay-provider-codex --lib \
    app_server::tests::bench_real_read_rate_limits -- --ignored --nocapture

  echo
  echo "== 5b. codex doctor --json (the slow, structured-but-comprehensive auth signal) =="
  time codex doctor --json > /dev/null
  echo "-- relay doctor against that same profile's project --"
  time "$BIN" doctor --project . > /dev/null || true
fi

echo
echo "Done. See docs/benchmarks.md for what these numbers mean and their known gaps."
