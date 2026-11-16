# shellcheck shell=bash
# Env-file values, shared by every installer (release/install.sh, the airgap
# setup and install scripts, and the native installer) so they all write and
# read env files the same way.
#
# An env file is read twice: by `docker compose --env-file` and by bash
# (`set -a; . file`). Single quotes are the only quoting both treat as fully
# literal: no `$` interpolation, no `#` comment, no escapes. A single quote or
# a newline inside a value has no form both agree on, so such a value is
# refused with an error instead of being written in a form one reader
# misparses.

# env_quote VALUE: print VALUE single-quoted, or fail if it cannot be.
env_quote() {
  case "$1" in
    *"'"* | *$'\n'* | *$'\r'*)
      echo "error: env values cannot contain single quotes or newlines" >&2
      return 1
      ;;
  esac
  printf "'%s'" "$1"
}

# env_line KEY VALUE: print one KEY='VALUE' line. Use it inside a
# `{ ...; } > file` group so a refused value stops the script under set -e.
env_line() {
  local quoted
  quoted="$(env_quote "$2")" || { echo "error: cannot write $1" >&2; return 1; }
  printf '%s=%s\n' "$1" "$quoted"
}

# env_unquote RAW: the value an env-file right-hand side stands for, as
# Compose reads it. Reads the single-quoted form written above, and unquoted
# values, in which `$` is doubled and a trailing ` # comment` is dropped.
env_unquote() {
  local raw="$1"
  case "$raw" in
    \'*\') raw="${raw#\'}"; printf '%s' "${raw%\'}" ;;
    \"*\") raw="${raw#\"}"; printf '%s' "${raw%\"}" ;;
    *)
      raw="${raw%% \#*}"
      printf '%s' "${raw//'$$'/'$'}"
      ;;
  esac
}

# env_get FILE KEY: KEY's value in FILE, or "" when it is absent or still a
# change-me placeholder from an example file.
env_get() {
  local file="$1" key="$2" line val
  [ -f "$file" ] || return 0
  line="$(grep -E "^${key}=" "$file" | head -n 1)" || true
  val="$(env_unquote "${line#*=}")"
  case "$val" in
    *change-me*) ;;
    *) printf '%s\n' "$val" ;;
  esac
}

# env_set FILE KEY VALUE: replace KEY's line in FILE, or append one.
env_set() {
  local file="$1" key="$2" line tmp
  line="$(env_line "$key" "$3")" || return 1
  [ -f "$file" ] || : > "$file"
  tmp="$(mktemp "$file.XXXXXX")"
  if grep -qE "^${key}=" "$file"; then
    LINE="$line" awk -v k="$key" 'index($0, k "=") == 1 { print ENVIRON["LINE"]; next } { print }' "$file" > "$tmp"
  else
    cat "$file" > "$tmp"
    printf '%s\n' "$line" >> "$tmp"
  fi
  chmod --reference="$file" "$tmp" 2>/dev/null || chmod 0600 "$tmp"
  mv "$tmp" "$file"
}

# url_encode VALUE: percent-encode VALUE for the userinfo part of a
# postgres:// or redis:// URL, so a password holding `@`, `#`, `/`, `?` or `%`
# survives as itself.
url_encode() {
  local LC_ALL=C s="$1" out="" c i
  for ((i = 0; i < ${#s}; i++)); do
    c="${s:i:1}"
    case "$c" in
      [A-Za-z0-9._~-]) out="$out$c" ;;
      *) printf -v c '%%%02X' "'$c"; out="$out$c" ;;
    esac
  done
  printf '%s' "$out"
}

# postgres_url USER PASSWORD HOST PORT DB and redis_url PASSWORD HOST PORT:
# the connection URLs every installer writes, credentials percent-encoded.
postgres_url() {
  printf 'postgres://%s:%s@%s:%s/%s' "$(url_encode "$1")" "$(url_encode "$2")" "$3" "$4" "$5"
}

redis_url() {
  printf 'redis://:%s@%s:%s' "$(url_encode "$1")" "$2" "$3"
}

# env_problem NAME VALUE: print why VALUE cannot be NAME's setting, or nothing
# when it can. A value is refused when it is empty or still a change-me
# placeholder from an example file; a connection URL also has to have the
# shape its client parses (the database, MQ and S3 settings, by name).
env_problem() {
  local name="$1" value="$2"
  case "$value" in
    "") echo "$name is not set"; return ;;
    *change-me*) echo "$name still holds a change-me placeholder from an example file"; return ;;
  esac
  case "$name" in
    *__DATABASE__URL)
      [[ "$value" =~ ^postgres(ql)?://[^@/]+@[^/@:]+(:[0-9]+)?/[^/?]+(\?.*)?$ ]] \
        || echo "$name must look like postgres://USER:PASSWORD@HOST:PORT/DATABASE"
      ;;
    *__MQ__URL)
      [[ "$value" =~ ^rediss?://([^@/]*@)?[^/@:]+(:[0-9]+)?(/[0-9]*)?$ ]] \
        || echo "$name must look like redis://:PASSWORD@HOST:PORT"
      ;;
    *__OBJECT_STORAGE__ENDPOINT)
      [[ "$value" =~ ^https?://[^/@:]+(:[0-9]+)?/?$ ]] \
        || echo "$name must look like http://HOST:PORT"
      ;;
  esac
}

# env_role_settings ROLE OBJECT_STORAGE: the secrets and URLs a node of ROLE
# (infra, server, worker, single-host, gateway) cannot run without.
# OBJECT_STORAGE is 1 when the node uses the S3 blob store.
env_role_settings() {
  local role="$1" s3="$2"
  case "$role" in
    infra | single-host)
      echo POSTGRES_USER POSTGRES_PASSWORD POSTGRES_DB BROCCOLI_DB_USER BROCCOLI_DB_PASSWORD \
        REDIS_PASSWORD BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
      [ "$s3" != 1 ] || echo BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET \
        BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      ;;
    server | worker)
      echo BROCCOLI__DATABASE__URL BROCCOLI__MQ__URL
      [ "$role" != server ] || echo BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
      [ "$s3" != 1 ] || echo BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT \
        BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      ;;
    gateway) echo BROCCOLI_UPSTREAMS ;;
    *) echo "env_role_settings: unknown role '$role'" >&2; return 2 ;;
  esac
}

# env_require NAME...: fail, listing every problem, unless each named shell
# variable holds a usable value (see env_problem).
env_require() {
  local name problem problems=""
  for name in "$@"; do
    problem="$(env_problem "$name" "${!name:-}")"
    [ -z "$problem" ] || problems="$problems  $problem"$'\n'
  done
  [ -n "$problems" ] || return 0
  printf 'error: these settings need a real value:\n%s' "$problems" >&2
  return 1
}

# env_require_role ROLE: env_require the settings a ROLE node needs, read from
# the shell variables (source the node's env files first). Every installer
# calls this before it starts anything, so a node never comes up on a
# placeholder secret or an address it cannot reach.
env_require_role() {
  local s3=0 names
  [ "${BROCCOLI__STORAGE__BACKEND:-}" != object_storage ] || s3=1
  names="$(env_role_settings "$1" "$s3")" || return 2
  # shellcheck disable=SC2086 # names is a list of variable names
  env_require $names
}
