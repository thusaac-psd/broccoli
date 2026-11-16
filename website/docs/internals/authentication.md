---
title: Authentication
sidebar_label: Authentication
sidebar_position: 10
---

# Authentication

A signed-in client holds a short-lived access token, a JWT it sends as
`Authorization: Bearer`, and a long-lived refresh token it trades for a new
access token (`POST /auth/refresh` with the HttpOnly cookie, or
`POST /auth/cli-refresh` with the token in the body). Every access token is
minted by `services::auth_tokens::issue_access_token` from the user's current
roles and permissions in the database.

## Sessions

Each sign-in is a session, a row of `auth_session`: a login opens one, and so
does a device-flow approval. Every access token names its session (the `sid`
claim) and every refresh token belongs to one. A refresh keeps the session,
and `POST /auth/cli-token` adds a refresh token to the caller's session.

A session ends when:

| Event | Sessions ended |
| --- | --- |
| `POST /auth/logout` | The one the refresh cookie belongs to and the one the bearer token names |
| A rotated refresh token is replayed past its grace window | That token's session |
| An admin resets the password, grants or revokes one of the user's roles, or deletes the account | Every session of the user |

Ending a session deletes all its refresh tokens with it, and every device code
it approved that the CLI has not collected yet. The user's other sessions, in
other browsers or the CLI, stay signed in. A client without a cookie, like the
CLI, logs out by sending its access token.

## Opening a session

A session rests on a check: the password a login verified, or the open session
that approved a device code (`device_authorization.approved_by`). The check
runs before the session exists, so a password reset, a role change or the
account's deletion can commit in between. `services::auth_tokens::open_session`,
the one way to open a session, takes what was checked (`SignIn`) and checks it
again in the statement that inserts the row, under a share lock on the user
row:

| Credential change | Result |
| --- | --- |
| Holds the user row when the sign-in arrives | The sign-in waits, finds the password replaced or the approving session ended, and opens nothing: the login is `401 INVALID_CREDENTIALS`, the device poll `expired_token` |
| Arrives while the sign-in holds the user row | The change waits for the sign-in to commit, then ends the new session with the user's others |

Every credential change takes the user row first (the owner lock,
`refresh_token::LOCK_OWNER_SQL`) and then the session rows, and a sign-in takes
them in the same order, so the two never deadlock. A device poll takes the
approving user's row before the code's own row for the same reason.

A device code's approval belongs to the session that approved it: the
`approved_by` foreign key deletes the approval with the session, so an approval
never outlives the browser sign-in that gave it.

## Freshness

A token is stale once something ends what it stood for: its session ends, the
user's password, roles or account change (`user.credentials_changed_at`), or
the user is deleted. `extractors::auth::fresh_user` is the one check, one indexed read,
and a stale token is `401 TOKEN_INVALID`, so the client refreshes and gets a
token carrying what the user holds now.

Which requests pay for the check follows from the request method:

| Request | Check |
| --- | --- |
| `POST`, `PUT`, `PATCH`, `DELETE` on a core route | `fresh_user`, through `AuthUser` |
| `GET`, `HEAD` on a core route | The signature and expiry alone |
| `GET /auth/me` | `fresh_user`, through `FreshAuthUser` |
| Any method on a plugin route (`/p/...`) | `fresh_user`, in the proxy |

`AuthUser` decides by the method, so a handler of a mutating route cannot opt
out by naming a cheaper extractor; submitting, running code and every other
request that spends quota or changes state refuses a stale token before the
handler runs. The integration test
`every_state_changing_route_refuses_a_stale_token` sends a stale token to every
authenticated mutating operation of the OpenAPI document.

A read keeps the stateless path, so the hot read routes make no database call
to authenticate. A stale token, a logged-out one included, therefore reads for
at most the access token's lifetime, `jwt::ACCESS_TOKEN_TTL` (5 minutes). A read that answers from the
caller's current account takes `FreshAuthUser`.
