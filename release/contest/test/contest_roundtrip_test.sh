#!/usr/bin/env bash
# Docker-gated, opt-in end-to-end safety net for the whole-contest bundle:
# seed a tiny contest+roster+account slice in a REAL (operator-provided,
# schema-loaded) Postgres, export it with export-problems.sh, delete the
# slice, restore it with import-problems.sh, and prove:
#   (a) the restore round-trips row-for-row,
#   (b) re-importing the same bundle is idempotent (no dup rows/errors), and
#   (c) an existing ACTIVE user's password is NEVER clobbered by a re-import
#       (the load-bearing credential-safety property of the whole feature).
#
# This test never touches problem rows and only runs against a DB it has
# verified is problem-empty first, so it cannot destroy an operator's data.
# It self-skips (exit 0) whenever its prerequisites aren't met, so it is a
# no-op in CI.
#
# Usage:
#   BROCCOLI_TEST_DATABASE_URL=postgres://... bash contest_roundtrip_test.sh
#     BROCCOLI_TEST_DATABASE_URL must point at a DEDICATED, DISPOSABLE,
#     schema-loaded broccoli Postgres (i.e. the target server has booted at
#     least once so entity sync()/Migrator::up already created the schema).
#     Never point this at a database with real data: although the test only
#     seeds/deletes its own namespaced rows, it refuses to run at all unless
#     `problem` is already empty on the target -- use a throwaway DB.
set -euo pipefail

if ! command -v docker >/dev/null 2>&1; then
  echo "SKIP: docker not available"
  exit 0
fi
if [ -z "${BROCCOLI_TEST_DATABASE_URL:-}" ]; then
  echo "SKIP: set BROCCOLI_TEST_DATABASE_URL to a DEDICATED, DISPOSABLE schema-loaded broccoli DB to run the round-trip"
  exit 0
fi

here="$(cd "$(dirname "$0")" && pwd)"
export_sh="$here/../export-problems.sh"
import_sh="$here/../import-problems.sh"

T="$(mktemp -d)"
CONF="$T/config.toml"
cat > "$CONF" <<TOML
[database]
url = "${BROCCOLI_TEST_DATABASE_URL}"
TOML
CONN_ENV="$T/connection.env"
printf "BROCCOLI__DATABASE__URL='%s'\n" "$BROCCOLI_TEST_DATABASE_URL" > "$CONN_ENV"

#
# -q (quiet) is required in addition to -At: without it, psql still prints a
# command-completion tag ("INSERT 0 1") after non-SELECT statements even in
# unaligned-tuples-only mode, which would otherwise corrupt a `RETURNING id`
# capture (the id line plus a stray second line).
PSQL=(psql "$BROCCOLI_TEST_DATABASE_URL" -v ON_ERROR_STOP=1 -Atq)

# --- pre-flight: reachable AND problem-empty (this doubles as the
#     non-destructive guard -- we refuse to run against a DB that already
#     holds problem data, seeded or real). ---
NPROB="$("${PSQL[@]}" -c "SELECT count(*) FROM problem" 2>/dev/null || true)"
if [ -z "$NPROB" ] || [ "$NPROB" != "0" ]; then
  echo "SKIP: target DB is not problem-empty; point me at a dedicated schema-loaded DB"
  rm -rf "$T"
  exit 0
fi

# --- namespaced seed identifiers (unique per run) ---
SUFFIX="__rt_$$"
ROLE_NAME="${SUFFIX}_role"
UNAME="${SUFFIX}_u1"
CTITLE="${SUFFIX}"
HASH_A='$2b$__rt_a'
HASH_B='$2b$__rt_b'

SEED_UID=""
SEED_CID=""

cleanup() {
  local rc=$?
  set +e
  if [ -n "$SEED_CID" ]; then
    "${PSQL[@]}" -c "DELETE FROM contest_user WHERE contest_id=$SEED_CID" >/dev/null 2>&1
  fi
  if [ -n "$SEED_UID" ]; then
    "${PSQL[@]}" -c "DELETE FROM user_role WHERE user_id=$SEED_UID" >/dev/null 2>&1
  fi
  if [ -n "$SEED_CID" ]; then
    "${PSQL[@]}" -c "DELETE FROM contest WHERE id=$SEED_CID" >/dev/null 2>&1
  fi
  if [ -n "$SEED_UID" ]; then
    "${PSQL[@]}" -c "DELETE FROM \"user\" WHERE id=$SEED_UID" >/dev/null 2>&1
  fi
  "${PSQL[@]}" -c "DELETE FROM role WHERE name='$ROLE_NAME'" >/dev/null 2>&1
  rm -f "$CONF"
  rm -rf "$T"
  exit "$rc"
}
trap cleanup EXIT

assert_eq() {
  local desc="$1" expect="$2" got="$3"
  if [ "$got" != "$expect" ]; then
    echo "FAIL: $desc -- expected '$expect', got '$got'" >&2
    exit 1
  fi
}

# --- 1. seed a tiny contest+roster+account slice, capturing serial ids
#     (never hardcoded) via RETURNING. Only NOT-NULL columns without a DB
#     default are supplied explicitly (see entity/{contest,user,role,
#     contest_user,user_role}.rs). role is name-only (PK=name). ---
"${PSQL[@]}" -c "INSERT INTO role(name) VALUES('$ROLE_NAME')" >/dev/null

SEED_UID="$("${PSQL[@]}" -c "INSERT INTO \"user\"(username, password, created_at, credentials_changed_at) VALUES('$UNAME', '$HASH_A', now(), now()) RETURNING id")"
"${PSQL[@]}" -c "INSERT INTO user_role(user_id, role) VALUES($SEED_UID, '$ROLE_NAME')" >/dev/null

SEED_CID="$("${PSQL[@]}" -c "INSERT INTO contest(title, description, start_time, end_time, created_at, updated_at) VALUES('$CTITLE', '', now(), now(), now(), now()) RETURNING id")"
"${PSQL[@]}" -c "INSERT INTO contest_user(contest_id, user_id, registered_at) VALUES($SEED_CID, $SEED_UID, now())" >/dev/null

# --- 2. export the contest into a bundle (no --with-secrets: prove the
#     no-clobber property survives even a blank-password bundle) ---
bash "$export_sh" --config "$CONF" --contest "$SEED_CID" --out "$T/b.tar.gz" >"$T/export.log" 2>&1 \
  || { cat "$T/export.log" >&2; echo "FAIL: export-problems.sh" >&2; exit 1; }
mkdir -p "$T/bundle"
tar -xzf "$T/b.tar.gz" -C "$T/bundle"

# --- 3. baseline: the seed is present exactly once ---
assert_eq "baseline contest"      1 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest WHERE id=$SEED_CID")"
assert_eq "baseline contest_user" 1 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest_user WHERE contest_id=$SEED_CID")"
assert_eq "baseline user"         1 "$("${PSQL[@]}" -c "SELECT count(*) FROM \"user\" WHERE id=$SEED_UID")"

# --- 4. delete the seeded slice, FK-safe (children first), and prove it's
#     actually gone -- this is what makes step 5 a real restore, not a no-op ---
"${PSQL[@]}" -c "DELETE FROM contest_user WHERE contest_id=$SEED_CID" >/dev/null
"${PSQL[@]}" -c "DELETE FROM user_role WHERE user_id=$SEED_UID" >/dev/null
"${PSQL[@]}" -c "DELETE FROM contest WHERE id=$SEED_CID" >/dev/null
"${PSQL[@]}" -c "DELETE FROM \"user\" WHERE id=$SEED_UID" >/dev/null
"${PSQL[@]}" -c "DELETE FROM role WHERE name='$ROLE_NAME'" >/dev/null

assert_eq "post-delete contest"      0 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest WHERE id=$SEED_CID")"
assert_eq "post-delete contest_user" 0 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest_user WHERE contest_id=$SEED_CID")"
assert_eq "post-delete user"         0 "$("${PSQL[@]}" -c "SELECT count(*) FROM \"user\" WHERE id=$SEED_UID")"

# --- 5. restore from the bundle directory (no --truncate: target is
#     problem-empty, so the non-empty guard doesn't even apply here) ---
bash "$import_sh" --env-file "$CONN_ENV" --bundle "$T/bundle" >"$T/import1.log" 2>&1 \
  || { cat "$T/import1.log" >&2; echo "FAIL: import-problems.sh (restore)" >&2; exit 1; }

assert_eq "restored contest"      1 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest WHERE id=$SEED_CID")"
assert_eq "restored contest_user" 1 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest_user WHERE contest_id=$SEED_CID")"
assert_eq "restored user"         1 "$("${PSQL[@]}" -c "SELECT count(*) FROM \"user\" WHERE id=$SEED_UID")"
# Fresh insert from a bundle exported without --with-secrets: password is
# blank. That's expected -- there was no live account to protect yet.
assert_eq "restored user password blank" "" "$("${PSQL[@]}" -c "SELECT password FROM \"user\" WHERE id=$SEED_UID")"

# --- 6. re-import the SAME bundle: idempotent, counts unchanged, no error ---
bash "$import_sh" --env-file "$CONN_ENV" --bundle "$T/bundle" >"$T/import2.log" 2>&1 \
  || { cat "$T/import2.log" >&2; echo "FAIL: import-problems.sh (re-import)" >&2; exit 1; }

assert_eq "re-import contest"      1 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest WHERE id=$SEED_CID")"
assert_eq "re-import contest_user" 1 "$("${PSQL[@]}" -c "SELECT count(*) FROM contest_user WHERE contest_id=$SEED_CID")"
assert_eq "re-import user"         1 "$("${PSQL[@]}" -c "SELECT count(*) FROM \"user\" WHERE id=$SEED_UID")"

# --- 7. NO-CLOBBER PROOF (load-bearing): simulate a live account by giving
#     the now-active user a real password, then re-import the same
#     (blank-password) bundle again. ON CONFLICT (username) WHERE
#     deleted_at IS NULL DO NOTHING must skip the row entirely. ---
"${PSQL[@]}" -c "UPDATE \"user\" SET password='$HASH_B' WHERE id=$SEED_UID" >/dev/null

bash "$import_sh" --env-file "$CONN_ENV" --bundle "$T/bundle" >"$T/import3.log" 2>&1 \
  || { cat "$T/import3.log" >&2; echo "FAIL: import-problems.sh (no-clobber re-import)" >&2; exit 1; }

assert_eq "active user password never clobbered" "$HASH_B" "$("${PSQL[@]}" -c "SELECT password FROM \"user\" WHERE id=$SEED_UID")"

echo "PASS: contest round-trip"
