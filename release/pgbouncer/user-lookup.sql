-- The password lookup a PgBouncer in front of Broccoli's database runs as
-- its auth_user (the app role) for every login it has no auth_file entry for:
--   auth_query = SELECT uname, phash FROM pgbouncer.user_lookup($1)
-- The server provisions a database role per plugin that uses SQL, with a
-- password it generates, so no auth_file can list them: PgBouncer has to
-- look each one up. pg_authid is readable only by a superuser, so a
-- superuser runs this file once, in Broccoli's database, with psql:
--   psql -v ON_ERROR_STOP=1 -v app=broccoli -U postgres -d broccoli -f user-lookup.sql
-- The function answers only for roles that may log in and are not
-- superusers, and only the app role may call it.
CREATE SCHEMA IF NOT EXISTS pgbouncer;
REVOKE ALL ON SCHEMA pgbouncer FROM PUBLIC;
CREATE OR REPLACE FUNCTION pgbouncer.user_lookup(username text, OUT uname text, OUT phash text)
  RETURNS record
  LANGUAGE sql STABLE SECURITY DEFINER
  SET search_path = pg_catalog
AS $$
  SELECT rolname::text, rolpassword::text
    FROM pg_authid
   WHERE rolname = username
     AND rolcanlogin
     AND NOT rolsuper
     AND (rolvaliduntil IS NULL OR rolvaliduntil > now())
$$;
REVOKE ALL ON FUNCTION pgbouncer.user_lookup(text) FROM PUBLIC;
GRANT USAGE ON SCHEMA pgbouncer TO :"app";
GRANT EXECUTE ON FUNCTION pgbouncer.user_lookup(text) TO :"app";
