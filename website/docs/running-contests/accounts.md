---
title: Accounts and sign-up
sidebar_label: Accounts and sign-up
sidebar_position: 2
---

# Accounts and sign-up

Contestants need an account before they can sign in. Admins create accounts
by default; self-registration is opened only when a contest wants it.

## Creating accounts

An admin with the user manage permission creates accounts on the **Users**
page, one at a time or as a bulk import of up to 100. A bulk import is all or
nothing: when any username is already taken, nothing is created and the error
lists every taken username.

A username names one account whatever its case. `Alice` and `alice` are the
same account, so the second cannot be created, and either spelling signs in.
The account keeps the case it was created with for display.

## Self-registration

Self-registration is closed on a fresh install. While it is closed, the sign-up
page tells visitors to ask the organisers for an account, and
`POST /api/v1/auth/register` answers `403 REGISTRATION_CLOSED`.

To open or close it, an admin with the user manage permission uses the
**Self-registration** switch on the **Users** page. The change applies to every
server at once, with no restart, so registration can be closed in the middle of
a contest. The same switch is `PUT /api/v1/admin/settings/registration` with
`{"open": true}` or `{"open": false}`, and `GET /api/v1/auth/registration`
reports the current state.

## Sign-in limits

One account may fail to sign in 10 times in any minute from one client address
(`auth.login_failure_limit` and `auth.login_failure_window_secs`), counted
across all servers. Only that account's own failures count against it, and a
successful sign-in clears them, so a venue whose contestants all reach the
server through one NAT address signs every one of them in, however many of them
mistype a password, with no setting changed. Each address may register 30 times
a minute. CLI
device codes have no rate: each address may have at most 1000 device codes that
a CLI is still waiting on. A code a contestant approves is collected at once,
and a code whose CLI was stopped or gave up stops counting a minute after its
last poll, so a whole venue can run `broccoli login` together, and run it again.
A client over a limit gets `429 RATE_LIMITED` with a `Retry-After` header that
gives the wait until a place opens; retrying sooner does not extend the wait.
