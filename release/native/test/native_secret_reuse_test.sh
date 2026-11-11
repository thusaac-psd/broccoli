#!/usr/bin/env bash
# Re-running install-native.sh infra-server keeps the secrets its config.toml
# already holds: the readers it uses recover every secret from a config
# rendered from the shipped template, and only --rotate-secrets regenerates.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
inst="$here/../install-native.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*"; exit 1; }

# shellcheck source=/dev/null
. <(grep -E '^toml_(string|url_password)\(\)' "$inst")
sed -e 's|@DB_URL@|postgres://broccoli:DbPw123@10.0.0.10:5432/broccoli|' \
    -e 's|@MQ_URL@|redis://:RedisPw456@10.0.0.10:6379|' \
    -e 's|@S3_ACCESS_KEY@|AccessKey7|; s|@S3_SECRET_KEY@|SecretKey8|' \
    -e 's|@JWT_SECRET@|JwtSecret9|; s|@ADMIN_PASSWORD@|AdminPw10|' \
    "$here/../config/config.toml.tmpl" > "$T/config.toml"

[ "$(toml_url_password "$T/config.toml" postgres)" = DbPw123 ] || fail "database password not recovered"
[ "$(toml_url_password "$T/config.toml" redis)" = RedisPw456 ] || fail "redis password not recovered"
[ "$(toml_string "$T/config.toml" access_key)" = AccessKey7 ] || fail "S3 access key not recovered"
[ "$(toml_string "$T/config.toml" secret_key)" = SecretKey8 ] || fail "S3 secret key not recovered"
[ "$(toml_string "$T/config.toml" jwt_secret)" = JwtSecret9 ] || fail "JWT secret not recovered"
[ "$(toml_string "$T/config.toml" admin_password)" = AdminPw10 ] || fail "admin password not recovered"

grep -q 'if \[ "\$ROTATE_SECRETS" = 0 \] && \[ -f "\$conf" \]' "$inst" || fail "installer does not reuse existing secrets"
grep -q -- '--rotate-secrets) ROTATE_SECRETS=1' "$inst" || fail "installer has no --rotate-secrets"
if grep -qE '(^[[:space:]]*|; )(db_pw|redis_pw|jwt)="\$\(gen' "$inst"; then fail "a secret is still regenerated unconditionally"; fi
echo "PASS: native infra re-runs keep their secrets unless asked to rotate"
