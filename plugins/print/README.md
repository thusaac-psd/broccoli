# print: contest code printing

On-demand printing for ICPC-style contests. Contestants print a contest
submission, or pasted code where the contest allows it (`allow_arbitrary`),
within a per-contestant job quota (`max_jobs_per_user`); staff oversee a live
queue; one or more **print stations** (near the printers) pick up jobs, render
pretty syntax-highlighted PDFs, and print them.

```
 contestant / staff (web)            print station(s)  (plugins/print/client)
        │  JSON over HTTP (JWT)              │  Authorization: PrintStation <token>
        ▼                                    ▼  (polls, claims, prints, reports)
 ┌──────────────────────────────────────────────────────────────┐
 │  broccoli server  ──  print plugin (this dir, WASM)            │
 │   • print_job / print_station tables (created on startup)      │
 │   • atomic claim → correct across many servers & stations      │
 └──────────────────────────────────────────────────────────────┘
```

Because plugin HTTP responses are JSON only, **PDF rendering and physical
printing live in the native client** (`client/`), not the plugin. The plugin is
the queue + coordination layer.

## Layout

| Path        | What                                                                |
| ----------- | ------------------------------------------------------------------- |
| `src/`      | WASM plugin: queue persistence, station coordination, HTTP handlers |
| `frontend/` | React UI: contestant print buttons, staff print-queue dashboard     |
| `client/`   | Native cross-platform print-station binary (`print-client`)         |

## Build

```bash
just build-plugin plugins/print --install      # WASM + web bundle
just build-print-client                         # native client (release)
just test-plugin plugins/print                  # plugin unit tests
```

## Configure a contest (staff)

In the contest's admin config, open **Printing**:

- **Enable printing**: master switch.
- **Require staff approval**: when on, jobs wait as _Awaiting approval_ until a
  staff member approves them on the **Print Queue** page (`/print-queue`).
- **Max pages per job**: jobs whose estimated page count exceeds this are
  rejected at submit time.
- **Header banner**: optional text shown on every printout (e.g. the contest
  name).
- **Print stations**: one entry per station, a name and an opaque token. A
  station presents its token as `Authorization: PrintStation <token>`, and the
  server records its claims, heartbeats and reports under that entry's name, so
  give every station its own token.

A **plugin-scoped** (global) station serves every contest on the deployment; a
**contest-scoped** station is limited to that contest. The queue shows a station
qualified by where it is configured, `global/<name>` or `contest-<id>/<name>`,
so a contest's station can never act as a global one of the same name.

## Run a print station

On the machine attached to a printer, grab the `broccoli-print-client` binary
for your platform from the GitHub release (or `just build-print-client`), then:

```bash
print-client setup     # interactive: detects printers, asks for server URL(s) + token
print-client doctor    # verifies connectivity + sends an optional test page
print-client run       # poll, claim, render, print
```

`print-client.toml` supports **multiple servers** (independent deployments or
one load-balanced URL) and **multiple printers** driven by one station:

```toml
station  = "room-a-desk-1"
location = "Room A"
poll_interval_secs = 3
max_pages = 10
paper = "A4"
font_size = 9.0
banner = "Regionals 2026"

[[server]]
url   = "http://judge.local:3000"
token = "PRINTSTATION-xxxxxxxx"

[[printer]]
name  = "main"
os_id = "HP_LaserJet_Room_A"      # CUPS / Windows queue name
# command = "lp -d {printer} {file}"   # optional override
# command = "folder:/var/spool/contest" # or a folder sink (no real printer)
```

### Printing backends

- **macOS / Linux:** CUPS via `lp` (printers enumerated with `lpstat`). CUPS
  prints silently to any installed printer, including USB and driverless queues.
- **Windows:** a bundled copy of **SumatraPDF**, a silent command-line printer.
  It ships inside the client and is extracted to
  `%LOCALAPPDATA%\broccoli-print\` on first use, so there is no separate install
  and no print dialog to click. SumatraPDF is GPLv3; see
  `client/assets/windows/SumatraPDF-LICENSE.txt`.
- **Configurable command template** (`{printer}`, `{file}`) for anything else.
- **Folder sink** (`folder:/path`) writes the PDF to a directory, a universal
  fallback, great for testing.

Render a local file to verify output without a server:

```bash
print-client test-print solution.cpp --out preview.pdf   # write PDF
print-client test-print solution.cpp --printer main      # actually print
```

### Fonts & Unicode

The client embeds **Sarasa Fixed SC** (`client/assets/`, SIL OFL), a fixed-width
font where Latin is half-width and CJK is full-width. Combined with
display-width-aware layout, it prints **Chinese/Japanese/Korean**, Greek,
Cyrillic, and accented Latin with correct monospace alignment, plus per-token
syntax highlighting. The embedded font adds ~25 MB to the binary; rendered PDFs
are written to a local temp file and sent straight to the printer (never over
the network), so their size is not a concern. Characters the font lacks (e.g.
emoji) print as a missing-glyph box rather than vanishing.

## Job lifecycle

`pending_approval` → `pending` → `claimed` → `printing` → `done` / `failed`, and
`canceled` from any state before `done`. The moves are:

| Move     | Who                    | From                                    | To         |
| -------- | ---------------------- | --------------------------------------- | ---------- |
| approve  | staff                  | `pending_approval`                      | `pending`  |
| cancel   | staff                  | any state except `done` / `canceled`    | `canceled` |
| reprint  | staff                  | `done`, `failed`                        | `pending`  |
| pin      | staff                  | `pending`, `done`, `failed`             | `pending`  |
| claim    | any station            | `pending`, or a claim older than 10 min | `claimed`  |
| printing | the station holding it | `claimed`                               | `printing` |
| done     | the station holding it | `claimed`, `printing`                   | `done`     |
| failed   | the station holding it | `claimed`, `printing`                   | `failed`   |

A move from any other state answers 409. Concurrent stations are serialized by
an atomic SQL claim, so a job is never printed twice. A station that crashes
while holding a job loses it after the 10-minute claim lease: another station
may then claim it, after which the first station's reports are refused. Until
that happens, the holding station may still report however late. The client
reports `printing` before it prints and does not print a job whose `printing`
report is refused. Requeueing and claiming clear the previous run's claim, page
count and error.
