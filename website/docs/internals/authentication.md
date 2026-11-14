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

Ending a session deletes all its refresh tokens with it. The user's other
sessions, in other browsers or the CLI, stay signed in. A client without a
cookie, like the CLI, logs out by sending its access token.

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
