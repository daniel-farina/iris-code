#!/usr/bin/env bash
# E2E smoke test for mlx-code artifacts.
# - HTML files: ensure they have <html>...</html>, balanced braces/parens,
#   and reference Three.js (the game-suite invariant).
# - JS files: run `node --check` for syntax-level validation.
# - Reports per-file pass/fail and writes one summary JSONL row per run to
#   ~/.mlx-code/logs/smoke.jsonl.
#
# Usage:
#   bash tools/smoke/run_all.sh [path...]
#   (no path → defaults to ~/mlx-code-out/)

set -uo pipefail

DEFAULT_ROOT="$HOME/mlx-code-out"
ROOTS=("${@:-$DEFAULT_ROOT}")
LOG_DIR="$HOME/.mlx-code/logs"
mkdir -p "$LOG_DIR"
SMOKE_LOG="$LOG_DIR/smoke.jsonl"

PASS=0
FAIL=0
SKIP=0

bal() { tr -dc "$1" < "$2" 2>/dev/null | wc -c | tr -d ' '; }

check_html() {
  local f=$1
  local issues=""
  grep -qi "<html" "$f" || issues+=" no_<html>;"
  grep -qi "</html>" "$f" || issues+=" no_</html>;"
  local lc=$(bal '{' "$f"); local rc=$(bal '}' "$f")
  [[ "$lc" == "$rc" ]] || issues+=" braces_${lc}_vs_${rc};"
  local lp=$(bal '(' "$f"); local rp=$(bal ')' "$f")
  [[ "$lp" == "$rp" ]] || issues+=" parens_${lp}_vs_${rp};"
  if [[ "$f" == *snake3d.html || "$f" == *space-dogfight.html || "$f" == *voxel-world.html || "$f" == *airplane.html ]]; then
    grep -q "THREE\." "$f" || issues+=" no_THREE.*_ref;"
  fi
  echo "$issues"
}

check_js() {
  local f=$1
  if command -v node >/dev/null 2>&1; then
    if out=$(node --check "$f" 2>&1); then
      echo ""
    else
      echo " syntax:$(echo "$out" | head -1 | tr -d '\"')"
    fi
  else
    echo " (node not installed; skipped js check)"
  fi
}

ROW="{\"ts_unix\":$(date +%s),\"results\":["
SEP=""

for ROOT in "${ROOTS[@]}"; do
  ROOT_EXP=$(eval echo "$ROOT")
  if [[ ! -d "$ROOT_EXP" && ! -f "$ROOT_EXP" ]]; then
    echo "SKIP missing $ROOT_EXP"
    SKIP=$((SKIP+1))
    continue
  fi
  while IFS= read -r -d '' f; do
    if [[ "$f" == *.html ]]; then
      issues=$(check_html "$f")
    elif [[ "$f" == *.js || "$f" == *.mjs ]]; then
      issues=$(check_js "$f")
    else
      continue
    fi
    bn=$(basename "$f")
    if [[ -z "$issues" ]]; then
      echo "PASS  $f"
      PASS=$((PASS+1))
      ROW+="${SEP}{\"file\":\"${f//\"/\\\"}\",\"status\":\"pass\"}"
    else
      echo "FAIL  $f  -- $issues"
      FAIL=$((FAIL+1))
      esc_issues=$(echo "$issues" | sed 's/"/\\"/g')
      ROW+="${SEP}{\"file\":\"${f//\"/\\\"}\",\"status\":\"fail\",\"issues\":\"${esc_issues}\"}"
    fi
    SEP=","
  done < <(find "$ROOT_EXP" -type f \( -name '*.html' -o -name '*.js' -o -name '*.mjs' \) -print0 2>/dev/null)
done

ROW+="],\"pass\":$PASS,\"fail\":$FAIL,\"skip\":$SKIP}"
echo "$ROW" >> "$SMOKE_LOG"
echo ""
echo "─ SMOKE  pass=$PASS  fail=$FAIL  skip=$SKIP   logged to $SMOKE_LOG"
exit $(( FAIL > 0 ? 1 : 0 ))
