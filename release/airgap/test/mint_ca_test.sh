#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
mint="$here/../ca/mint-ca.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
bash "$mint" --out "$T" --days 30

[ -f "$T/root.crt" ] || { echo "FAIL: root.crt missing"; exit 1; }
[ -f "$T/root.key" ] || { echo "FAIL: root.key missing"; exit 1; }

perms="$(stat -c '%a' "$T/root.key")"
[ "$perms" = "600" ] || { echo "FAIL: root.key perms $perms != 600"; exit 1; }

txt="$(openssl x509 -in "$T/root.crt" -noout -text)"
grep -q 'CA:TRUE' <<<"$txt"                    || { echo "FAIL: root.crt not a CA cert"; exit 1; }
grep -q 'Basic Constraints: critical' <<<"$txt" || { echo "FAIL: basicConstraints not critical"; exit 1; }
grep -q 'Key Usage: critical' <<<"$txt"         || { echo "FAIL: keyUsage not critical"; exit 1; }
grep -q 'Certificate Sign' <<<"$txt"            || { echo "FAIL: keyCertSign missing"; exit 1; }
grep -q 'NIST CURVE: P-256' <<<"$txt"           || { echo "FAIL: key is not ECDSA P-256"; exit 1; }
# pathlen:0 — this root signs the one server leaf directly; it must never be able
# to mint intermediate sub-CAs (would widen the trust surface off one stolen key).
grep -q 'pathlen:0' <<<"$txt"                   || { echo "FAIL: basicConstraints missing pathlen:0 (root can mint sub-CAs)"; exit 1; }

# the CA out dir itself is created 0700 so root.key is never even momentarily
# exposed to group/world between mkdir and the 0600 chmod (default umask 022
# would otherwise leave the parent dir 0755).
sub="$T/nested-ca"
bash "$mint" --out "$sub" --days 30 >/dev/null
dperm="$(stat -c '%a' "$sub")"
[ "$dperm" = "700" ] || { echo "FAIL: CA out dir perms $dperm != 700"; exit 1; }

# input validation: subject DN components must reject injection. '/' is the
# openssl -subj RDN separator and a newline would splice extra DN lines — either
# could forge O=/CN= fields, so both are refused before openssl runs.
if bash "$mint" --out "$T/inj-cn" --cn 'evil/CN=Injected' --days 30 >/dev/null 2>&1; then
  echo "FAIL: mint-ca accepted a --cn with a '/' RDN separator"; exit 1; fi
[ -e "$T/inj-cn/root.crt" ] && { echo "FAIL: mint-ca wrote a cert for an injected --cn"; exit 1; } || true
if bash "$mint" --out "$T/inj-org" --org "$(printf 'a\nO=Injected')" --days 30 >/dev/null 2>&1; then
  echo "FAIL: mint-ca accepted a --org containing a newline"; exit 1; fi
[ -e "$T/inj-org/root.crt" ] && { echo "FAIL: mint-ca wrote a cert for a newline --org"; exit 1; } || true
# a legitimate custom org/cn (spaces allowed) still works
bash "$mint" --out "$T/custom" --org "Custom LAN Org" --cn "custom.root.ca" --days 30 >/dev/null \
  || { echo "FAIL: mint-ca rejected a legitimate custom --org/--cn"; exit 1; }
grep -q 'custom.root.ca' <<<"$(openssl x509 -in "$T/custom/root.crt" -noout -subject)" \
  || { echo "FAIL: custom CN not present in cert subject"; exit 1; }

echo "PASS: mint-ca produces a CA cert + 0600 key"
