#!/usr/bin/env bash
# Import a broccoli-problems (or broccoli-contest) archive/bundle (made by
# export-problems.sh) into a FRESH contest server: restores the problem
# tables -- and, for a contest bundle, the contest row/roster/accounts too --
# FK-safe, then pushes the referenced blobs into the contest object store.
# No internet / no aws-cli needed.
#
# Prerequisites on the target:
#   - the Broccoli server has started at least once (it auto-creates the schema
#     and registers its plugins on boot)
#   - the target's config.toml (native) or connection.env (Docker, via
#     --env-file) points at the contest Postgres + SeaweedFS
#   - psql on this host (postgresql-client)
#
# Credential safety: a contest bundle's user rows are restored with
# ON CONFLICT (username) WHERE deleted_at IS NULL DO NOTHING, so an existing
# active user's real password hash is NEVER overwritten -- even if the bundle
# was exported without --with-secrets (blanked password). Only brand-new
# users get inserted.
#
# Usage:
#   ./import-problems.sh (ARCHIVE.tar.gz | --bundle DIR)
#                        [--config /data/broccoli/config/config.toml | --env-file connection.env]
#                        [--truncate] [--wipe-dependents] [--yes]
#                        [--with-secrets] [--dry-run]
#   ARCHIVE.tar.gz    : a tarball made by export-problems.sh (mutually
#                       exclusive with --bundle; exactly one is required).
#   --bundle DIR      : restore directly from an already-unpacked bundle
#                       directory instead of a tarball. DIR is used in place
#                       and is never deleted -- only import-problems.sh's own
#                       scratch files are cleaned up.
#   --truncate        : wipe existing problem rows on the target first (needed if
#                       the target already has problems; default refuses a
#                       non-empty target). Refuses if submissions, code runs or
#                       contest-problem links exist on the target.
#   --wipe-dependents : with --truncate, ALSO wipe rows that reference the
#                       problems: submission (+judgements/test case results),
#                       code_run (+results) and contest_problem links.
#   --with-secrets    : accepted for symmetry with export-problems.sh; has no
#                       effect on the restore -- the user upsert is always
#                       ON CONFLICT ... DO NOTHING, so it never overwrites an
#                       existing active user's password regardless of this
#                       flag. Whether real password hashes are present in the
#                       bundle is decided at export time.
#   --dry-run         : print the restore.sql that would be run and exit,
#                       touching nothing but the bundle dir (if any) and
#                       stdout -- no config read, no DB, no tar, no s3copy.py.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ARCHIVE=""
BUNDLE_DIR=""
CONFIG="/data/broccoli/config/config.toml"
ENV_FILE=""
TRUNCATE=0
WIPE_DEPENDENTS=0
YES=0
WITH_SECRETS=0
DRY_RUN=0
usage() { echo "usage: ./import-problems.sh (ARCHIVE.tar.gz | --bundle DIR) [--config C | --env-file F] [--truncate] [--wipe-dependents] [--yes] [--with-secrets] [--dry-run]" >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle) BUNDLE_DIR="$2"; shift 2 ;;
    --with-secrets) WITH_SECRETS=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --config) CONFIG="$2"; shift 2 ;;
    --env-file) ENV_FILE="$2"; shift 2 ;;
    --truncate) TRUNCATE=1; shift ;;
    --wipe-dependents) WIPE_DEPENDENTS=1; shift ;;
    --yes) YES=1; shift ;;
    --) shift ;;
    -*) echo "unknown arg: $1" >&2; exit 2 ;;
    *)
      if [ -z "$ARCHIVE" ]; then ARCHIVE="$1"; shift
      else echo "unexpected arg: $1" >&2; exit 2
      fi
      ;;
  esac
done
if [ -n "$ARCHIVE" ] && [ -n "$BUNDLE_DIR" ]; then
  echo "specify either ARCHIVE.tar.gz or --bundle DIR, not both" >&2; exit 2
fi
if [ -z "$ARCHIVE" ] && [ -z "$BUNDLE_DIR" ]; then
  usage; exit 2
fi

# --- locate the bundle contents (WORK) and the manifest, without touching the
#     DB/config/s3 yet -- --dry-run needs only this much. ---
CLEANUP_WORK=0
WORK=""
MANIFEST_JSON=""
if [ -n "$BUNDLE_DIR" ]; then
  [ -d "$BUNDLE_DIR" ] || { echo "bundle dir not found: $BUNDLE_DIR" >&2; exit 2; }
  [ -f "$BUNDLE_DIR/manifest.json" ] || { echo "not a broccoli bundle (no manifest.json): $BUNDLE_DIR" >&2; exit 2; }
  WORK="$BUNDLE_DIR"
  MANIFEST_JSON="$(cat "$WORK/manifest.json")"
elif [ "$DRY_RUN" = 1 ]; then
  # dry-run + a tarball: peek the manifest without extracting the archive.
  # export-problems.sh tars with `-C "$WORK" .`, so members are stored as
  # "./manifest.json"; try that form first, then the bare name for tarballs
  # made another way.
  [ -f "$ARCHIVE" ] || { echo "archive not found: $ARCHIVE" >&2; exit 2; }
  MANIFEST_JSON="$(tar -xOzf "$ARCHIVE" ./manifest.json 2>/dev/null || tar -xOzf "$ARCHIVE" manifest.json 2>/dev/null || true)"
  [ -n "$MANIFEST_JSON" ] || { echo "not a broccoli-problems archive (no manifest.json)" >&2; exit 2; }
else
  [ -f "$ARCHIVE" ] || { echo "archive not found: $ARCHIVE" >&2; exit 2; }
  WORK="$(mktemp -d)"
  CLEANUP_WORK=1
  tar -xzf "$ARCHIVE" -C "$WORK"
  [ -f "$WORK/manifest.json" ] || { echo "not a broccoli-problems archive (no manifest.json)" >&2; exit 2; }
  MANIFEST_JSON="$(cat "$WORK/manifest.json")"
fi

# The generated SQL always lives in its own scratch file -- never inside
# $WORK, since $WORK may be the caller's own --bundle directory and must
# never be written into or removed by us.
SQL="$(mktemp)"
cleanup() {
  rm -f "$SQL"
  [ "$CLEANUP_WORK" = 1 ] && rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

echo ">> manifest:"; sed 's/^/   /' <<<"$MANIFEST_JSON"

# Contest bundles additionally carry the contest row, its roster, and the
# roster's accounts/roles (Task 4's export --contest). The real export writes
# the tag under "format"; a test fixture may use "tag" -- match either.
CONTEST_BUNDLE=0
grep -q 'broccoli-contest/v1' <<<"$MANIFEST_JSON" && CONTEST_BUNDLE=1

if [ "$DRY_RUN" != 1 ]; then
  command -v psql >/dev/null || { echo "psql not found (install postgresql-client on this host)" >&2; exit 2; }

  if [ -n "$ENV_FILE" ]; then
    # A Docker install has no config.toml; its connection.env (or .env.infra)
    # carries the same settings.
    [ -f "$ENV_FILE" ] || { echo "env file not found: $ENV_FILE" >&2; exit 2; }
    # shellcheck source=../lib/env.sh
    . "$HERE/../lib/env.sh"
    dburl="$(env_get "$ENV_FILE" BROCCOLI__DATABASE__URL)"
    s3val(){ env_get "$ENV_FILE" "BROCCOLI__STORAGE__OBJECT_STORAGE__$(printf '%s' "$1" | tr 'a-z' 'A-Z')"; }
    src="$ENV_FILE"
  else
    [ -f "$CONFIG" ] || { echo "config not found: $CONFIG (pass --env-file connection.env on a Docker install)" >&2; exit 2; }
    dburl="$(awk '/^\[database\]/{f=1;next} /^\[/{f=0} f' "$CONFIG" | grep -E '^[[:space:]]*url' | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"$//')"
    s3sec="$(awk '/^\[storage.object_storage\]/{f=1;next} /^\[/{f=0} f' "$CONFIG")"
    s3val(){ printf '%s\n' "$s3sec" | grep -E "^[[:space:]]*$1[[:space:]]*=" | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"$//'; }
    src="$CONFIG"
  fi
  export S3_ENDPOINT="$(s3val endpoint)" S3_BUCKET="$(s3val bucket)" S3_REGION="$(s3val region)"
  export S3_ACCESS_KEY="$(s3val access_key)" S3_SECRET_KEY="$(s3val secret_key)"
  [ -n "$dburl" ] || { echo "could not read the database URL from $src" >&2; exit 2; }

  PSQL=(psql "$dburl" -v ON_ERROR_STOP=1 -At)

  # schema must exist (server booted at least once)
  have="$("${PSQL[@]}" -c "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('problem','test_case','plugin_config')")"
  [ "$have" = "3" ] || { echo "ERROR: target schema missing (problem/test_case/plugin_config). Start the server once first." >&2; exit 2; }

  existing="$("${PSQL[@]}" -c "SELECT count(*) FROM problem WHERE deleted_at IS NULL")"
  if [ "$existing" -gt 0 ] && [ "$TRUNCATE" = 0 ]; then
    echo "ERROR: target already has $existing problem(s). Re-run with --truncate to replace them." >&2
    exit 2
  fi
fi

# Tables that hold FK references onto problem/test_case (Postgres refuses a
# plain TRUNCATE of the problem tables unless they are truncated too, and we
# deliberately avoid CASCADE so nothing gets wiped implicitly). The contest
# path only ever adds rows via credential-safe upserts, so --truncate
# semantics are unchanged: it never touches role/user/contest/contest_user/etc.
PROBLEM_TABLES="test_case, problem_attachment, additional_file, problem"
DEPENDENT_TABLES="test_case_result, submission_judgement, submission, code_run_result, code_run, contest_problem"
if [ "$DRY_RUN" != 1 ] && [ "$TRUNCATE" = 1 ]; then
  nsub="$("${PSQL[@]}" -c "SELECT count(*) FROM submission")"
  ncr="$("${PSQL[@]}" -c "SELECT count(*) FROM code_run")"
  ncp="$("${PSQL[@]}" -c "SELECT count(*) FROM contest_problem")"
  if [ "$((nsub + ncr + ncp))" -gt 0 ] && [ "$WIPE_DEPENDENTS" = 0 ]; then
    {
      echo "ERROR: rows on the target reference the problems being replaced:"
      echo "         submission=$nsub code_run=$ncr contest_problem=$ncp"
      echo "       Replacing the problems would destroy them (plus their judgements"
      echo "       and per-test-case results). Refusing."
      echo "       Re-run with --truncate --wipe-dependents to wipe them too."
    } >&2
    exit 2
  fi
  if [ "$YES" = 0 ]; then
    echo "About to TRUNCATE these tables on the target and replace plugin_config:"
    echo "  $PROBLEM_TABLES"
    echo "  $DEPENDENT_TABLES"
    echo "  (currently: submissions=$nsub, code runs=$ncr, contest-problem links=$ncp)"
    printf "Type 'yes' to continue: "
    read -r ans; [ "$ans" = "yes" ] || { echo "aborted"; exit 1; }
  fi
fi

# --- build restore SQL (client-side \copy paths are relative to $WORK) ---
# emit_upsert: the DRY generalization of a temp-table + \copy + upsert block.
# Loads db/$table.dat into a LIKE-shaped temp table, then upserts it into
# $table with the given ON CONFLICT target/action.
emit_upsert() {
  local table="$1" conflict="$2" action="$3" tmp="_imp_${1}"
  {
    echo "CREATE TEMP TABLE $tmp (LIKE $table INCLUDING DEFAULTS);"
    echo "\\copy $tmp FROM 'db/$table.dat'"
    echo "INSERT INTO $table SELECT * FROM $tmp ON CONFLICT ($conflict) $action;"
  } >> "$SQL"
}
# emit_user_upsert: "user" is a reserved word (needs quoting) and its upsert
# is credential-safe by construction -- ON CONFLICT (username) WHERE
# deleted_at IS NULL DO NOTHING means an existing active user is always
# skipped, so a blanked (no --with-secrets) or stale password in the bundle
# can never clobber a live user's real credentials. Freshly inserted users
# get credentials_changed_at stamped to now() when the bundle didn't set it.
emit_user_upsert() {
  {
    echo "CREATE TEMP TABLE _imp_user (LIKE \"user\" INCLUDING DEFAULTS);"
    echo "\\copy _imp_user (id, username, password, created_at, deleted_at, credentials_changed_at) FROM 'db/user.dat'"
    echo "INSERT INTO \"user\" (id, username, password, created_at, deleted_at, credentials_changed_at)"
    echo "SELECT id, username, password, created_at, deleted_at, COALESCE(credentials_changed_at, now())"
    echo "FROM _imp_user"
    echo "ON CONFLICT (username) WHERE deleted_at IS NULL DO NOTHING;"
  } >> "$SQL"
}

: > "$SQL"
echo "BEGIN;" >> "$SQL"
if [ "$TRUNCATE" = 1 ]; then
  # No CASCADE: every affected table is named explicitly (dependents were
  # verified empty above unless --wipe-dependents was given), so an
  # unexpected FK from any other table aborts the transaction instead of
  # silently truncating it.
  echo "TRUNCATE $DEPENDENT_TABLES, $PROBLEM_TABLES RESTART IDENTITY;" >> "$SQL"
fi

if [ "$CONTEST_BUNDLE" = 1 ]; then
  echo "-- accounts (never clobber existing active credentials)" >> "$SQL"
  emit_upsert "role"            "name"              "DO NOTHING"
  emit_upsert "role_permission" "role, permission"  "DO NOTHING"
  emit_user_upsert
  emit_upsert "user_role"       "user_id, role"     "DO NOTHING"
fi

# FK-safe order: parent problem first, then children
echo "-- problem definitions" >> "$SQL"
echo "\\copy problem FROM 'db/problem.dat'" >> "$SQL"
echo "\\copy test_case FROM 'db/test_case.dat'" >> "$SQL"
echo "\\copy problem_attachment FROM 'db/problem_attachment.dat'" >> "$SQL"
echo "\\copy additional_file FROM 'db/additional_file.dat'" >> "$SQL"
# plugin_config: upsert via temp table (target may already hold default globals)
emit_upsert "plugin_config" "scope, ref_id, namespace" "DO UPDATE SET config = EXCLUDED.config, enabled = EXCLUDED.enabled, position = EXCLUDED.position, updated_at = EXCLUDED.updated_at"

# reset serial sequences so newly-created rows don't collide
echo "SELECT setval(pg_get_serial_sequence('problem','id'), GREATEST((SELECT COALESCE(max(id),1) FROM problem),1));" >> "$SQL"
echo "SELECT setval(pg_get_serial_sequence('test_case','id'), GREATEST((SELECT COALESCE(max(id),1) FROM test_case),1));" >> "$SQL"

if [ "$CONTEST_BUNDLE" = 1 ]; then
  echo "-- contest metadata + roster" >> "$SQL"
  emit_upsert "contest"         "id"                     "DO UPDATE SET title = EXCLUDED.title"
  emit_upsert "contest_problem" "contest_id, problem_id" "DO NOTHING"
  emit_upsert "contest_user"    "contest_id, user_id"    "DO NOTHING"
  echo "SELECT setval(pg_get_serial_sequence('contest','id'), GREATEST((SELECT COALESCE(max(id),1) FROM contest),1));" >> "$SQL"
  echo "SELECT setval(pg_get_serial_sequence('\"user\"','id'), GREATEST((SELECT COALESCE(max(id),1) FROM \"user\"),1));" >> "$SQL"
fi

echo "COMMIT;" >> "$SQL"

if [ "$DRY_RUN" = 1 ]; then
  cat "$SQL"
  exit 0
fi

echo ">> restoring database..."
( cd "$WORK" && psql "$dburl" -v ON_ERROR_STOP=1 -q -f "$SQL" )

echo ">> pushing blobs into object storage..."
NLOADED=0
if [ -d "$WORK/blobs" ] && [ -n "$(ls -A "$WORK/blobs" 2>/dev/null)" ]; then
  NLOADED="$(python3 "$HERE/s3copy.py" load --dir "$WORK/blobs")"
fi

# verify
GOTP="$("${PSQL[@]}" -c "SELECT count(*) FROM problem WHERE deleted_at IS NULL")"
GOTT="$("${PSQL[@]}" -c "SELECT count(*) FROM test_case")"
WANTP="$(grep -o '"problems": *[0-9]*' "$WORK/manifest.json" | grep -o '[0-9]*')"
echo ">> done: problems=$GOTP (manifest $WANTP), test_cases=$GOTT, blobs_loaded=$NLOADED"
[ "$GOTP" = "$WANTP" ] || { echo "WARN: problem count mismatch (target had pre-existing rows?)" >&2; }
