#!/usr/bin/env bash
# The panic ratchet (README, "Project policy": no panics, ever).
#
# Counts every construct that can panic at run time in the library code of
# each crate (clippy's panic lints; test code is not linted), in the three
# builds that matter: default features, the server (ssr, axum) feature set, and
# the browser (hydrate, wasm32). Fails if any crate has more sites than
# `panic-baseline.txt` records. When you remove panics, run with `--update`
# and commit the lower numbers; the numbers may only go down.
#
# Usage: scripts/panic-ratchet.sh [--update] [--sites]
#   --update  rewrite panic-baseline.txt with the current counts
#   --sites   also print every site (file:line lint)
set -euo pipefail
cd "$(dirname "$0")/.."

UPDATE=0
SITES=0
for arg in "$@"; do
  case "$arg" in
    --update) UPDATE=1 ;;
    --sites) SITES=1 ;;
    *) echo "unknown flag $arg" >&2; exit 2 ;;
  esac
done

LINTS=(unwrap_used expect_used panic unreachable todo unimplemented
       indexing_slicing arithmetic_side_effects unwrap_in_result)
FLAGS=()
for l in "${LINTS[@]}"; do FLAGS+=(-W "clippy::$l"); done
LINT_JSON=$(printf '"clippy::%s",' "${LINTS[@]}")
LINT_JSON="[${LINT_JSON%,}]"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "== clippy: default features" >&2
cargo clippy --workspace --lib --bins --message-format=json -q -- "${FLAGS[@]}" \
  > "$WORK/default.jsonl" 2>"$WORK/default.err" || { cat "$WORK/default.err" >&2; exit 1; }
echo "== clippy: server (ssr, axum)" >&2
cargo clippy -p halyard --features axum --lib --message-format=json -q -- "${FLAGS[@]}" \
  > "$WORK/ssr.jsonl" 2>"$WORK/ssr.err" || { cat "$WORK/ssr.err" >&2; exit 1; }
echo "== clippy: browser (hydrate, wasm32)" >&2
cargo clippy -p halyard --no-default-features \
  --features hydrate --target wasm32-unknown-unknown --lib --message-format=json -q -- "${FLAGS[@]}" \
  > "$WORK/hydrate.jsonl" 2>"$WORK/hydrate.err" || { cat "$WORK/hydrate.err" >&2; exit 1; }

# One line per distinct site: crate <TAB> lint <TAB> file:line:col. The crate
# is the directory that holds `src/` (or the build script's directory).
jq -r --argjson lints "$LINT_JSON" '
  select(.reason == "compiler-message")
  | .message as $m
  | ($m.code.code // "") as $code
  | select($lints | index($code))
  | ($m.spans[] | select(.is_primary)) as $s
  | select($s.file_name | startswith("/") | not)
  | ($s.file_name | if test("/src/") then (split("/src/")[0] | split("/") | last)
                    else (split("/") | .[-2]) end) as $crate
  | "\($crate)\t\($code | sub("clippy::"; ""))\t\($s.file_name):\($s.line_start):\($s.column_start)"
' "$WORK"/*.jsonl | sort -u > "$WORK/sites.tsv"

cut -f1 "$WORK/sites.tsv" | sort | uniq -c | awk '{print $2, $1}' > "$WORK/counts.txt"

if [ "$SITES" = "1" ]; then
  awk -F'\t' '{print $3, $2}' "$WORK/sites.tsv" | sort
fi

printf '%-34s %8s %8s\n' crate now baseline
total_now=0
total_base=0
failed=0
touch panic-baseline.txt
# awk, not grep: an empty baseline must not end the script under `set -e`.
all_crates=$( (awk 'NF {print $1}' "$WORK/counts.txt"; awk '!/^#/ && NF {print $1}' panic-baseline.txt) | sort -u )
for c in $all_crates; do
  now=$(awk -v c="$c" '$1 == c {print $2}' "$WORK/counts.txt"); now=${now:-0}
  base=$(awk -v c="$c" '!/^#/ && $1 == c {print $2}' panic-baseline.txt); base=${base:-0}
  mark=""
  if [ "$now" -gt "$base" ]; then mark="  <-- more than the baseline"; failed=1; fi
  if [ "$now" -lt "$base" ]; then mark="  (down $((base - now)))"; fi
  printf '%-34s %8s %8s%s\n' "$c" "$now" "$base" "$mark"
  total_now=$((total_now + now))
  total_base=$((total_base + base))
done
printf '%-34s %8s %8s\n' TOTAL "$total_now" "$total_base"
echo
echo "by lint:"
cut -f2 "$WORK/sites.tsv" | sort | uniq -c | sort -nr

if [ "$UPDATE" = "1" ]; then
  {
    echo "# Panic sites per crate in library code (scripts/panic-ratchet.sh)."
    echo "# May only go down. Regenerate with: scripts/panic-ratchet.sh --update"
    cat "$WORK/counts.txt"
  } > panic-baseline.txt
  echo "panic-baseline.txt updated ($total_now sites)."
  exit 0
fi
if [ "$failed" = "1" ]; then
  echo "panic ratchet: a crate gained panic sites. Handle the error instead (a typed" >&2
  echo "Result, a logged recovery), or explain in review why the baseline must rise." >&2
  exit 1
fi
echo "panic ratchet: ok"
