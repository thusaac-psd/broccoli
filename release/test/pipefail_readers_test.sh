#!/usr/bin/env bash
# No bash under release/, scripts/, or a workflow's or composite action's run
# blocks pipes into a reader that can stop before its input ends: `grep -q`,
# `-m` or `-l`, `head`, a `sed` that quits, an `awk` program that exits, or
# `read`. Such a reader exits on its first match or line, the writer then dies
# of SIGPIPE or EPIPE whenever it had not finished writing, and under pipefail
# that failure becomes the pipeline's status: the check fails at random. Test
# the text instead (`grep -q pat <<<"$text"`, `[[ $text == *pat* ]]`), capture
# a command's output first and test that, or let the reader consume all of its
# input (an awk program keeps its first match in a flag instead of exiting).
# Every file is checked, not only those that set pipefail themselves: a library
# runs under the pipefail of the script that sources it, and a workflow or
# composite action step's `shell: bash` sets pipefail. A bash script is one
# named *.sh or whose shebang runs bash; comments are skipped.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

fail() {
  echo "FAIL: $*"
  exit 1
}

# A pipe into an early-exit reader: grep with -q/-m/-l (or --quiet,
# --max-count, --files-with-matches), head, sed with a q command, awk with an
# exit, or read (alone, after `IFS=`, or opening a `{ ...; }` group).
sq="'"
dq='"'
quit="([;$sq$dq[:space:]]|[0-9\$])q([;$sq$dq[:space:]]|\$)"
word_end="([^A-Za-z0-9_]|\$)"
readers="(^|[^|])\\|[[:space:]]*(grep([[:space:]]+-[A-Za-z]*[qml][A-Za-z]*|[[:space:]]+--(quiet|silent|max-count|files-with-matches))|head([[:space:]]|\$)|sed[[:space:]][^|]*$quit|awk[[:space:]][^|]*(^|[^A-Za-z0-9_])exit$word_end|(\\{[[:space:]]*)?(IFS=[^[:space:]]*[[:space:]]+)?read$word_end)"

# Pins the pattern to what it must catch and what it must leave alone.
for bad in 'echo "$x" | grep -q y' 'f | grep -Eqi y' 'f | grep -vq y' 'f | head -n 1' \
  'f | grep -m1 y' 'f |head -1' "f | sed -n '1p;1q'" 'f | grep --quiet y' \
  "f | awk '/y/ { print; exit }'" "f | awk -F: '{print \$2; exit}'" \
  "f | awk '{ if (\$1) exit 1 }'" 'f | read -r x' 'f | IFS= read -r x' \
  'f | { read -r x; echo "$x"; }' 'f|head -1'; do
  grep -Eq "$readers" <<<"$bad" || fail "the pattern misses: $bad"
done
for good in 'grep -q y <<<"$x"' 'f | grep -c y' 'f | grep y >/dev/null' "f | sed -n 1p" \
  'head -c 24 /dev/urandom | base64' 'f | grep -E y | cut -d: -f1' "f | sed 's/a/q/'" \
  "f | awk '/y/ && !done { print; done = 1 }'" "f | awk '{ print \$exitcode }'" \
  'f | while IFS= read -r x; do :; done' 'f | xargs -r readlink' 'f | tr a b' \
  '[ -n "$x" ] || read -r y <<<"$x"' 'f || head -n 1 g'; do
  if grep -Eq "$readers" <<<"$good"; then fail "the pattern refuses: $good"; fi
done

is_bash() {
  case "$1" in *.sh) return 0 ;; esac
  local first
  IFS= read -r first <"$1" || true
  [[ $first =~ ^#!.*(/|[[:space:]])bash([[:space:]]|$) ]]
}

files=()
while IFS= read -r f; do
  if is_bash "$f"; then files+=("$f"); fi
done < <(find "$root/release" "$root/scripts" -type f ! -name '*.md' | sort)
nscripts="${#files[@]}"
[ "$nscripts" -gt 20 ] || fail "found only $nscripts bash scripts"
while IFS= read -r f; do
  files+=("$f")
done < <(find "$root/.github/workflows" "$root/.github/actions" -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)
[ "${#files[@]}" -gt "$nscripts" ] || fail "found no workflow"
grep -qx "$root/release/test/bin/docker" < <(printf '%s\n' "${files[@]}") \
  || fail "release/test/bin/docker is not scanned"
grep -qx "$root/.github/actions/setup-isolate/action.yml" < <(printf '%s\n' "${files[@]}") \
  || fail "the composite actions are not scanned"

rc=0
for f in "${files[@]}"; do
  # This file's own samples above are strings, not pipelines.
  [ "$f" = "$here/$(basename "$0")" ] && continue
  while IFS= read -r hit; do
    echo "FAIL: ${f#"$root"/}:$hit pipes into a reader that can exit early under pipefail"
    rc=1
  done < <(awk '!/^[[:space:]]*#/ { print FNR ": " $0 }' "$f" | grep -E "$readers" || true)
done
[ "$rc" = 0 ] || exit 1
echo "PASS: no early-exit reader in a pipeline of $nscripts bash scripts and $((${#files[@]} - nscripts)) workflows and actions"
