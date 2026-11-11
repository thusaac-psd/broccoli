# broccoli (contestant CLI)

`broccoli-contestant-cli` installs the **`broccoli`** command — the tool
contestants use to log in, submit and test solutions, browse problems, ask
clarifications, and watch a contest live from the terminal.

> Building Broccoli **plugins** instead? That's the separate `broccoli-dev`
> command (`packages/dev-cli`).

## Install

In the monorepo, build the optimized binary (small + fast to cold-load — see
[Performance](#performance--cold-start)):

```bash
cargo build -p broccoli-contestant-cli --profile release-cli
# binary at: target/release-cli/broccoli
```

A plain `cargo build --release -p broccoli-contestant-cli` works too; the
`release-cli` profile adds size-optimization + LTO on top, shrinking the binary
to ~2.8 MB so it cold-loads faster on contest machines.

## Quick start

```bash
broccoli login                       # opens your browser
broccoli contest list                # see available contests
broccoli contest info <id|name>      # details + your registration status
broccoli submit sol.cpp -p A -c <id> # submit problem A
broccoli test sol.cpp -p A -c <id>   # run the sample cases first
broccoli status                      # pick a recent submission to inspect
broccoli watch <id|name>             # live contest dashboard (TUI)
```

Most commands take a **contest by id or name** and a **problem by id, label
(`A`), 1-based index, or title** — see [References](#references).

## Commands

### Auth — `login`, `whoami`

```bash
broccoli login                          # browser callback flow
broccoli login -u alice -p '<password>' # direct username/password (no browser)
broccoli login --server http://host:3000
broccoli whoami                         # who am I logged in as? (alias: me)
```

Credentials are stored in `~/.config/broccoli/credentials.json`. Override per
command with `BROCCOLI_URL` / `BROCCOLI_TOKEN`, or `--server` / `--token`.

### The inner loop — `test`, `submit`, `status`

```bash
broccoli test sol.cpp -p A -c <id>      # run the problem's sample cases
broccoli test sol.cpp -p A --local      # run locally (uses cached samples offline)
broccoli test sol.cpp -i input.txt      # run against your own input

broccoli submit sol.cpp -p A -c <id>    # submit (language auto-detected)
broccoli submit sol.cpp -p A -w         # …and watch for the verdict
broccoli submit sol.cpp -p 1 --no-contest  # submit to a standalone problem

broccoli status                         # interactive picker of your recent subs
broccoli status <submission-id>         # one submission's detail
broccoli status --recent                # plain table (good for piping)
```

A failed `test` sample prints a line-by-line **diff** (`- expected` / `+ got`)
so you see _where_ it differs.

### Browsing — `contest`

```bash
broccoli contest list
broccoli contest info <id|name>
broccoli contest register <id|name>
broccoli contest unregister <id|name>
broccoli contest problems <id|name>           # list problems
broccoli contest problems <id|name> -p A       # download statement + samples into ./<problem>/
```

### Clarifications — `clarifications`

```bash
broccoli clarifications list <id|name>
broccoli clarifications ask <id|name> "Is N ≤ 1e9?"
broccoli clarifications ask <id|name>          # prompts for the question
```

### Live dashboard — `watch`

```bash
broccoli watch <id|name>
```

A real-time TUI with three tabs (Submissions / Problems / Clarifications):

- Verdicts, time, and memory update live; a fresh **Accepted** flashes and jumps
  into view.
- The **Clarifications** tab shows an unread badge and flashes when a reply or
  announcement arrives — so you don't miss a rule change.
- The countdown turns amber under 5 min and red under 1 min.
- **`a`** opens a box to ask a clarification without leaving the dashboard.
- Keys: `1`-`3`/`Tab` switch tabs · `↑↓`/`jk` select · `Enter` open detail · `a`
  ask · `r` force-refresh · `q` quit. In a detail overlay: `jk` scroll · `g`/`G`
  or `Home`/`End` jump · `o` open a problem statement in your editor (vim if
  available, else `$PAGER`/less).

### Config — `config`

```bash
broccoli config show
broccoli config set contest 3
broccoli config set language python
broccoli config unset contest
```

### Utilities — `completions`, `prewarm`

```bash
broccoli completions <bash|zsh|fish|powershell|elvish>   # see Shell completions
broccoli prewarm                                          # see Performance
```

## Aliases

Designed for muscle memory under contest pressure — the four single letters go
to the hot inner loop, and `status` deliberately never gets `s` (so a fat-finger
can't turn a status check into a submit). The visible aliases:

| Command          | Alias  |     | Subcommand            | Alias   |
| ---------------- | ------ | --- | --------------------- | ------- |
| `submit`         | `s`    |     | `contest list`        | `ls`    |
| `test`           | `t`    |     | `contest info`        | `i`     |
| `contest`        | `c`    |     | `contest register`    | `reg`   |
| `status`         | `st`   |     | `contest unregister`  | `unreg` |
| `clarifications` | `clar` |     | `contest problems`    | `p`     |
| `config`         | `cfg`  |     | `clarifications list` | `ls`    |
| `watch`          | `w`    |     | `clarifications ask`  | `a`     |
| `whoami`         | `me`   |     |                       |         |

So the inner loop is `t sol.cpp -p A` → `s sol.cpp -p A` → `st`, and browsing is
`c p` / `c i 3` / `c ls`. (Extra hidden forms like `sub`, `stat`, `con`, `cl`
also work; run `broccoli --help` for the full list.)

## References

- **Contest**: a numeric id, or a (case-insensitive) match of the contest
  **title**.
- **Problem** (within a contest): the **label** (`A`), the numeric **problem
  id**, a **1-based index** into the problem list, or the **title**.

`broccoli submit sol.cpp -p A` and `-p 1` and `-p "Two Sum"` all resolve to the
same problem.

## Configuration & environment

Per-user config lives at `~/.config/broccoli/config.toml`; contest admins can
pre-seed machine-wide defaults at `/etc/broccoli/config.toml` (user values win).

```toml
# ~/.config/broccoli/config.toml
contest  = "3"            # default contest when -c is omitted
language = "cpp"          # default language
server   = "https://judge.example.com"
tls      = "webpki"       # webpki (default) | system | insecure  — see TLS

[runtimes]                # how to run each language locally (for `test --local`)
python = "python3.12"
```

Environment overrides:

| Variable                   | Purpose                                                                         |
| -------------------------- | ------------------------------------------------------------------------------- |
| `BROCCOLI_URL`             | Server URL (overrides config / saved credentials)                               |
| `BROCCOLI_TOKEN`           | Access token (with `BROCCOLI_URL`, skips `login`)                               |
| `BROCCOLI_TLS`             | TLS trust mode (see below) — overrides the `tls` config key                     |
| `NO_COLOR` / `FORCE_COLOR` | Disable / force ANSI colors                                                     |
| `PAGER`                    | Viewer used by `o` in the watch problem overlay (defaults to vim → less → more) |

## TLS

The client uses **rustls** (no OpenSSL dependency). By default it trusts the
machine's OS certificate store **and** its bundled Mozilla roots, so it works
both with a server whose internal CA is installed in the OS store (the air-gap
contestant install does this) and with public-CA servers on machines whose OS
store is missing or stale. Switch trust mode at runtime (no rebuild) via
`BROCCOLI_TLS` or the `tls` config key:

| Mode                  | When to use                                                                                                |
| --------------------- | ---------------------------------------------------------------------------------------------------------- |
| `default` _(default)_ | Almost always: public-CA HTTPS, an internal CA in the OS store, or plain HTTP.                             |
| `webpki`              | Trust only the bundled Mozilla roots, ignoring the OS store.                                               |
| `system`              | Use the OS verifier alone (on macOS and Windows it also applies the OS's revocation and policy checks).    |
| `insecure`            | A throwaway **self-signed** dev server you control. Skips verification (prints a warning). **Not secure.** |

```bash
BROCCOLI_TLS=webpki broccoli submit sol.cpp -p A    # ignore the OS cert store
```

## Shell completions

```bash
# bash
broccoli completions bash | sudo tee /etc/bash_completion.d/broccoli >/dev/null
# zsh  (ensure the dir is on your $fpath, then restart the shell)
broccoli completions zsh  > ~/.zfunc/_broccoli
# fish
broccoli completions fish > ~/.config/fish/completions/broccoli.fish
# PowerShell  (append to your profile)
broccoli completions powershell >> $PROFILE
```

## Performance / cold start

The first command on a fresh machine pays cold costs (binary page-in, DNS, TLS
handshake). Two things help:

- **Ship the `release-cli` binary** (`--profile release-cli`) — it's ~2.8 MB, so
  it cold-loads fast even over a networked home directory.
- **`broccoli prewarm`** — pages the binary into the OS cache and primes
  DNS/TCP/TLS to the configured server (with a connectivity check). Run it once
  at machine setup or just before the contest:

  ```bash
  broccoli prewarm
  # → Warming up https://judge.example.com … ready (42 ms)
  ```
