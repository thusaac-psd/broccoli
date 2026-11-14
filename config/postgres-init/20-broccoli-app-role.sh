#!/bin/sh
# Run once by the postgres image, as its superuser, on an empty volume: the
# servers and workers log in as BROCCOLI_DB_USER, which owns the database and
# holds CREATEROLE and nothing more (it provisions each plugin's own role).
set -eu
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
  -v app="${BROCCOLI_DB_USER:-broccoli}" -v pw="${BROCCOLI_DB_PASSWORD:?}" -v db="$POSTGRES_DB" <<'SQL'
CREATE ROLE :"app" LOGIN CREATEROLE PASSWORD :'pw';
ALTER DATABASE :"db" OWNER TO :"app";
SQL
