#!/usr/bin/env bash
# No script that sets `pipefail` pipes into a reader that can stop before its
# input ends: `grep -q`, `-m` or `-l`, `head`, a `sed` that quits. Such a
# reader exits on its first match or line, the writer then dies of SIGPIPE or
# EPIPE whenever it had not finished writing, and under pipefail that failure
# becomes the pipeline's status: the check fails at random. Test the text
# instead (`grep -q pat <<<"$text"`, `[[ $text == *pat* ]]`), or capture a
# command's output first and test that. Scripts under release/ and scripts/ are
# checked, comments skipped.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

fail() {
  echo "FAIL: $*"
  exit 1
}

# A pipe into an early-exit reader: grep with -q/-m/-l (or --quiet,
# --max-count, --files-with-matches), head, or sed with a q command.
sq="'"
dq='"'
quit="([;$sq$dq[:space:]]|[0-9\$])q([;$sq$dq[:space:]]|\$)"
readers="\\|[[:space:]]*(grep([[:space:]]+-[A-Za-z]*[qml][A-Za-z]*|[[:space:]]+--(quiet|silent|max-count|files-with-matches))|head([[:space:]]|\$)|sed[[:space:]][^|]*$quit)"

# Pins the pattern to what it must catch and what it must leave alone.
for bad in 'echo "$x" | grep -q y' 'f | grep -Eqi y' 'f | grep -vq y' 'f | head -n 1' \
  'f | grep -m1 y' 'f |head -1' "f | sed -n '1p;1q'" 'f | grep --quiet y'; do
  grep -Eq "$readers" <<<"$bad" || fail "the pattern misses: $bad"
done
for good in 'grep -q y <<<"$x"' 'f | grep -c y' 'f | grep y >/dev/null' "f | sed -n 1p" \
  'head -c 24 /dev/urandom | base64' 'f | grep -E y | cut -d: -f1' "f | sed 's/a/q/'"; do
  if grep -Eq "$readers" <<<"$good"; then fail "the pattern refuses: $good"; fi
done

scripts=()
while IFS= read -r f; do
  scripts+=("$f")
done < <(find "$root/release" "$root/scripts" -type f -name '*.sh' | sort)
[ "${#scripts[@]}" -gt 20 ] || fail "found only ${#scripts[@]} scripts"

rc=0
checked=0
for f in "${scripts[@]}"; do
  # This file's own samples above are strings, not pipelines.
  [ "$f" = "$here/$(basename "$0")" ] && continue
  grep -Eq '^[^#]*set -[A-Za-z]*o[[:space:]]+pipefail|^[^#]*set -o pipefail' "$f" || continue
  checked=$((checked + 1))
  while IFS= read -r hit; do
    echo "FAIL: ${f#"$root"/}:$hit pipes into a reader that can exit early under pipefail"
    rc=1
  done < <(awk '!/^[[:space:]]*#/ { print FNR ": " $0 }' "$f" | grep -E "$readers" || true)
done
[ "$checked" -gt 20 ] || fail "only $checked scripts set pipefail"
[ "$rc" = 0 ] || exit 1
echo "PASS: no early-exit reader in a pipeline of $checked pipefail scripts"
