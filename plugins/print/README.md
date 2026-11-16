# print: contest code printing

On-demand printing for ICPC-style contests. Contestants print a submission or
arbitrary pasted code; staff oversee a live queue; one or more **print
stations** (near the printers) pick up jobs, render pretty syntax-highlighted
PDFs, and print them.

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
- **Station tokens**: opaque secrets that authorize print stations. Add one per
  deployment (or per contest). A station presents it as
  `Authorization: PrintStation <token>`.

A **plugin-scoped** (global) station token authorizes a station for every
contest on the deployment; a **contest-scoped** token is limited to that
contest.

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

`pending_approval` → `pending` → `claimed` → `printing` → `done` / `failed`.
Staff can **approve**, **reprint** (re-queue), or **cancel** from the dashboard.
Concurrent stations are serialized by an atomic SQL claim, so a job is never
printed twice.
