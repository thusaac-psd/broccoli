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
