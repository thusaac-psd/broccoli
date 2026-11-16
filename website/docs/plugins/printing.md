---
title: Printing
sidebar_label: Printing
sidebar_position: 1
---

import ThemedImage from '@theme/ThemedImage';

# Printing

Contestants request a printout of their code from the web interface. Volunteers
watch a live queue, and a small program running on a printer-side computer turns
each request into a clean, syntax-highlighted PDF and prints it.

## How it works

Printing has three parts.

1. **The plugin**, inside the Broccoli server. It holds the queue and decides who
   prints what. It never talks to a printer itself.
2. **The web interface** that contestants and volunteers already use. Contestants
   request prints from it, and volunteers run the queue from it.
3. **Print stations**. A print station is an ordinary computer placed next to a
   printer, running a small program called `print-client`. It does the actual
   rendering and printing.

The server and the print stations are kept separate on purpose. The server runs
in a sandbox that can only return data, never a PDF or a printer command, so the
part that produces paper has to run on a machine you control near the printer.
This separation also means printing keeps working if a station briefly loses its
network connection. Jobs wait in the queue until a station picks them up.

A single print station can drive several printers, and it can serve more than one
contest server at the same time.

<ThemedImage
  alt="Browsers talk to the Broccoli server, which holds the queue. Print stations poll the server, then render and print the PDF."
  sources={{ light: '/img/print-architecture.svg', dark: '/img/print-architecture.dark.svg' }}
/>

## Turn on printing

Printing is configured in the print plugin's settings. In the admin area, open
**Plugins**, find the print plugin, then open its **Configure** dialog. The
settings below can be applied globally to every contest, or to one contest at a
time.

| Setting                    | What it does                                                                                                                                       |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Enable printing**        | The master switch. Off by default.                                                                                                                |
| **Require approval**       | When on, every request waits as _Awaiting approval_ until a volunteer approves it. Leave it off to send prints straight to the printers.           |
| **Max pages per job**      | A request whose estimated length is over this limit is rejected the moment a contestant submits it, with a clear message. This guards against an accidental 900-page print. |
| **Header banner**          | Optional text printed at the top of every page, usually the contest name.                                                                          |
| **Print stations**         | One entry per print station, a name and a token. See [Print stations](#print-stations) below.                                                       |

### Print stations

Each print station gets its own entry in the **Print stations** list: a name,
which the print queue shows next to every job the station claims, and a token,
a password that lets that station fetch and print jobs. You choose the token.
Any long, hard-to-guess string works, such as the output of a password
generator. Give every station its own token, because the server knows a station
only by the entry whose token it presents.

There are two kinds of entry.

- A **contest station**, listed in a contest's Printing section, works only for
  that contest. The queue shows it as `contest-<id>/<name>`.
- A **global station**, listed in the plugin's deployment-wide settings, works
  for every contest on the server. This is convenient when one print desk serves
  a whole event. The queue shows it as `global/<name>`.

:::warning[Treat tokens like passwords]

Anyone holding a token can fetch the code contestants submitted for printing.
Share tokens over a trusted channel, and replace them between events.

:::

## Set up a print station

Do this once on each machine that will print, before the contest starts.

You will need a computer next to the printer with the printer already installed
and working in the operating system, meaning you can already print a normal test
page from it. You will also need the contest server's address and a station
token.

### Get the program

Download the `broccoli-print-client` build for your operating system from the
project's GitHub Releases page. It is a single self-contained file with nothing
to install.

- On **Windows**, keep the `.exe` somewhere easy to find. You run it from a
  terminal, so double-clicking does not help.
- On **macOS** and **Linux**, mark the file as runnable once. Open a terminal in
  the download folder, run `chmod +x broccoli-print-client`, then run it with
  `./broccoli-print-client`.

The commands below are written as `print-client` for brevity. Use whatever you
named the downloaded file.

### Run the setup wizard

```bash
print-client setup
```

The wizard finds the printers installed on the machine, asks for the server
address and a station token, lets you name the station and its printers, and
writes a configuration file named `print-client.toml` next to the program. For
most setups this is all the configuration you need.

### The configuration file

`print-client.toml` is a plain text file you can open and edit. A typical one is
shown below. Note that a single station can list several servers and several
printers.

```toml
station  = "room-a-desk-1"   # a name you choose; shown in the volunteer dashboard
location = "Room A"          # optional; used to route jobs to a specific room
poll_interval_secs = 3       # how often to check the server for new jobs
max_pages = 10               # safety cap enforced at the station too
paper = "A4"                 # or "Letter"
font_size = 9.0
banner = "Regionals 2026"    # printed at the top of every page

[[server]]
url   = "http://judge.local:3000"
token = "the-station-token-you-configured"

[[printer]]
name  = "main"                         # a label for this printer
os_id = "HP_LaserJet_Room_A"           # the printer's name in the operating system
```

The `os_id` is the printer's name as the operating system knows it. On Windows
you find it under Settings, then Bluetooth & devices, then Printers & scanners.
On macOS and Linux you run `lpstat -p` in a terminal to list printer names. The
setup wizard fills this in for you, so you usually will not need to look it up by
hand.

### Check it before the contest

```bash
print-client doctor
```

`doctor` confirms each server is reachable and your token is accepted, lists the
printers it found, and offers to send a real test page to each one. Run it once
after setup. If the test page comes out, you are ready.

### Run it on contest day

```bash
print-client run
```

Leave this running for the whole contest. It polls the server, claims jobs,
renders them, and prints them, logging each one as it goes. If the machine
reboots, start it again. Anything not yet printed is still waiting in the queue.

:::tip[Try it without a printer]

Point a printer entry at a folder instead of hardware to preview output, or to
rehearse the whole flow before the printer arrives.

```toml
[[printer]]
name    = "preview"
command = "folder:/Users/you/Desktop/prints"   # PDFs are written here, not printed
```

You can also render a local file directly.

```bash
print-client test-print solution.cpp --out preview.pdf
```

:::

## How jobs reach the printer

The print station has several ways to send a PDF to a printer, so it works across
operating systems and unusual setups. The station ships with everything it needs,
so volunteers do not install anything extra.

- On **macOS** and **Linux** it uses the system's built-in printing (CUPS). If
  the printer prints from the operating system, it prints from here.
- On **Windows** it prints through a bundled copy of SumatraPDF that travels
  inside the station. There is no separate install and no print dialog to click.
- For anything unusual, such as label printers or raw queues, you can supply your
  own command with `{printer}` and `{file}` placeholders, for example
  `command = "lp -d {printer} {file}"`.
- Setting `command = "folder:/path"` writes the PDF to a folder instead of
  printing. This is useful for testing, archiving, or a fully manual workflow.

## What contestants see

Once printing is enabled, contestants get a **Print** button on their submission
results, and a print option near the code editor for pasting or uploading
arbitrary code. After requesting a print they see its status, queued, printing,
or printed, so they know whether to walk to the print desk. If approval is
required, their request shows as awaiting approval until a volunteer releases it.

## Running the queue

Volunteers get a **Print Queue** dashboard. It lists every job with the
contestant, problem, file, page count, and live status, and it updates on its own
as jobs move. From here a volunteer can do the following.

- **Approve** a job that is awaiting approval.
- **Reprint** a job that came out wrong, which sends it back into the queue.
- **Cancel** a job that should not print.
- **Pin** a job, or several, to a specific printer. This is handy for sending a
  room's prints to the printer in that room.
- **View** the code that will be printed before releasing it.

A separate panel shows your print stations and whether each one is online, so you
can spot a station that has dropped off before contestants start asking where
their printout is.

## The job lifecycle

<ThemedImage
  alt="A job moves from pending approval to pending, claimed, printing, and done; it can fail or be canceled, and finished jobs can be reprinted."
  sources={{ light: '/img/print-lifecycle.svg', dark: '/img/print-lifecycle.dark.svg' }}
/>

A new job starts at `pending`, or at `pending_approval` if the contest requires
approval, in which case a volunteer's approval moves it to `pending`. A print
station then claims it, prints it, and marks it `done`. A render or printer
problem sends it to `failed`. A volunteer can cancel a job before it finishes, and
any finished job can be reprinted, which returns it to the queue.

When several stations are running, the server hands each job to exactly one of
them using an atomic claim, so the same job is never printed twice, even if two
stations ask for work at the same instant.

## Languages and fonts

Printouts support far more than English. The station embeds a fixed-width font
covering Chinese, Japanese, and Korean as well as Greek, Cyrillic, and accented
Latin, and lays them out with correct monospace alignment and per-token syntax
highlighting. Characters the font does not have, such as emoji, print as a
placeholder box rather than vanishing.

## Troubleshooting

| Symptom                              | Likely cause and fix                                                                                                       |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| `doctor` cannot reach the server     | Wrong server address, or the machine is not on the contest network. Check the `url` in `print-client.toml`.                |
| `doctor` says the token was rejected | The token is not in the contest's (or the global) **Print stations** list, or it has a typo. Copy it again.                |
| A printer is not found               | The `os_id` does not match the operating system's printer name. Run `lpstat -p` (macOS, Linux) or check Printers & scanners (Windows). |
| Jobs sit at _Awaiting approval_      | **Require approval** is on. Approve them from the Print Queue, or turn the setting off.                                     |
| A request is rejected as too long    | It is over **Max pages per job**. Raise the limit, or ask the contestant to trim the file.                                 |
| Non-Latin characters look wrong      | Usually fine thanks to the embedded font. A placeholder box means the font lacks that exact glyph, such as an emoji.       |
