---
title: Contestant CLI
sidebar_label: Contestant CLI
sidebar_position: 1
---

# Contestant CLI

`broccoli` is the command line tool for contestants. From your terminal you log
in, read problems, test and submit solutions, ask clarifications, and watch a
contest unfold live. It is one file with nothing to install around it, and it
behaves the same on Linux, macOS, and Windows.

## Install

Download the build for your system from the [downloads page](../downloads.md),
then make it runnable.

On macOS and Linux, mark the file runnable and move it onto your path as
`broccoli`.

```bash
chmod +x broccoli-cli-linux-x86_64
mv broccoli-cli-linux-x86_64 /usr/local/bin/broccoli
```

On Windows, rename the file to `broccoli.exe` and keep it somewhere easy to find.
You run it from a terminal, so double clicking does nothing useful. Check that it
runs.

```bash
broccoli --version
```

### Build from source

If there is no build for your system, or you want the newest code, build it with
Rust.

```bash
git clone https://github.com/THUSAAC-PSD/broccoli
cargo install --path broccoli/packages/contestant-cli
```

This installs the same `broccoli` command into your Cargo bin folder.

### Turn on tab completion

Generate a completion script for your shell and load it. Broccoli supports bash,
zsh, fish, PowerShell, and elvish.

```bash
broccoli completions zsh > ~/.broccoli-completions.zsh
echo 'source ~/.broccoli-completions.zsh' >> ~/.zshrc
```

## Log in

Point the CLI at your contest server. The address comes from whoever runs your
contest.

```bash
broccoli login --server https://judge.example.com
```

This opens your browser to authorize, then keeps you signed in for later
commands. Your login is saved on this machine, so you do it once per server.
Without `--server`, Broccoli talks to a local server at `http://localhost:3000`,
so pass your contest's address. Confirm who you are at any time.

```bash
broccoli whoami
```

On a machine with no browser, such as one you reached over SSH, Broccoli prints a
link instead. Open it on any device, authorize, and paste the token back. You can
ask for this directly.

```bash
broccoli login --server https://judge.example.com --no-browser
```

If your contest signs you in with a username and password rather than the browser
flow, pass them and Broccoli skips the browser.

```bash
broccoli login --server https://judge.example.com -u alice -p secret
```

## Find your contest

List the contests you can see.

```bash
broccoli contest list
```

Each contest has a number and a name, and you can use either one wherever a
command asks for a contest. Open one to see its problems, its timing, and whether
you are registered.

```bash
broccoli contest info "Spring Round"
```

If a contest needs registration, register before you submit. You can leave one
with `broccoli contest unregister` until it starts. After that your registration
stays, so the standings keep your results.

```bash
broccoli contest register "Spring Round"
```

## Read a problem

List the problems in a contest.

```bash
broccoli contest problems "Spring Round"
```

Each problem has a label such as `A`, a number, and a title, and any of them
names the problem. Download one to save its statement and sample cases as files
in the current folder, ready to read offline and test against.

```bash
broccoli contest problems "Spring Round" -p A
```

## Test before you submit

Run your solution against the sample cases first. Point at your source file and
the problem.

```bash
broccoli test sol.cpp -c "Spring Round" -p A
```

Broccoli reads the language from the file extension, fetches the samples, runs
each one, and shows you which passed, with a diff for any that did not. Add
`--local` to run on your own machine instead of the server, which is faster and
works offline once you have downloaded the problem.

```bash
broccoli test sol.cpp -c "Spring Round" -p A --local
```

Try your own input with `-i`, which runs the file once and prints what it
produced.

```bash
broccoli test sol.cpp --local -i input.txt
```

## Submit

Send your solution to a problem.

```bash
broccoli submit sol.cpp -c "Spring Round" -p A
```

Broccoli detects the language, submits, and prints a submission number. Override
the language with `-l` when you need to. Add `-w` to wait for the verdict right
there instead of checking it later.

```bash
broccoli submit sol.cpp -c "Spring Round" -p A -w
```

After your first submit in a folder, Broccoli remembers the contest, problem, and
language in a small `.broccoli` file, so you can repeat with just the file name.

```bash
broccoli submit sol.cpp
```

For a problem that lives outside any contest, submit with `--no-contest` and the
problem's own id.

```bash
broccoli submit sol.cpp --no-contest -p 42
```

## Check a submission

See how a submission did. With no number, Broccoli lets you pick from your recent
ones.

```bash
broccoli status
```

Pass a number to jump straight to it.

```bash
broccoli status 12345
```

Print your recent submissions as a plain table with `--recent`. Picking from
recent needs to know the contest, which Broccoli reads from your `.broccoli` file
or your saved default.

```bash
broccoli status --recent
```

## Watch the contest live

Open a live dashboard for a contest.

```bash
broccoli watch "Spring Round"
```

It refreshes on its own and has three tabs, your submissions, the problems, and
clarifications, with a countdown to the end. Open any row for details, read a
problem in a pager, and ask a clarification without leaving the screen.

| Key                | What it does                                  |
| ------------------ | --------------------------------------------- |
| `Tab`, `←`, `→`    | Move between tabs                             |
| `1` `2` `3`        | Jump to submissions, problems, clarifications |
| `↑` `↓`, `j` `k`   | Move the selection                            |
| `Enter`            | Open the selected row                         |
| `o`                | Open a problem in your pager                  |
| `a`                | Ask a clarification                           |
| `r`                | Refresh now                                   |
| `q`, `Esc`         | Close a panel, or quit                        |

## Ask a clarification

Ask the organizers a question about a contest. Leave the question off to type it
interactively.

```bash
broccoli clarifications ask "Spring Round" "Is the input always sorted?"
```

Read the answers, and any announcements, with the list.

```bash
broccoli clarifications list "Spring Round"
```

## Set your defaults

Most commands take a contest with `-c` and a problem with `-p`. You can stop
repeating yourself in two ways.

A `.broccoli` file in your working folder sets the contest, problem, and language
for everything you run there. Broccoli writes one for you on your first submit,
and you can edit it by hand.

```toml
contest  = "Spring Round"
problem  = "A"
language = "cpp"
```

A saved default applies everywhere. Set the contest once and drop `-c` from then
on. The keys you can set are `contest`, `language`, and `server`, and you remove
one with `broccoli config unset`.

```bash
broccoli config set contest "Spring Round"
```

See everything Broccoli has saved, including where the file lives, with the bare
command.

```bash
broccoli config
```

Two environment variables override the saved values for a single run, which helps
in scripts. `BROCCOLI_URL` sets the server and `BROCCOLI_TOKEN` sets the login
token. Your login and your settings live in your account's configuration folder,
next to a cache of the problems you download. Run `broccoli config` to see the
exact path.

## Languages

Broccoli picks the language from your file extension, among the languages the
server offers. Set `-l`, or your `language` default, when you want a different
one. A server with the bundled standard-languages plugin offers the languages
below, and the organizers choose which of them a contest accepts.

| Extension                  | Language     |
| -------------------------- | ------------ |
| `.c`                       | `c`          |
| `.cpp` `.cc` `.cxx` `.c++` | `cpp`        |
| `.py`                      | `python3`    |
| `.java`                    | `java`       |
| `.kt`                      | `kotlin`     |
| `.rs`                      | `rust`       |
| `.go`                      | `go`         |
| `.pas` `.pp`               | `pascal`     |
| `.js`                      | `javascript` |

## Every command

Run `broccoli --help`, or add `--help` to any command, to see every option. Each
command also has a short alias.

| Command                       | Alias     | What it does                          |
| ----------------------------- | --------- | ------------------------------------- |
| `broccoli login`              | `li`      | Log in to a contest server            |
| `broccoli whoami`             | `me`      | Show who you are logged in as         |
| `broccoli contest list`       | `c ls`    | List contests                         |
| `broccoli contest info`       | `c i`     | Contest details and your status       |
| `broccoli contest register`   | `c reg`   | Register for a contest                |
| `broccoli contest unregister` | `c unreg` | Leave a contest                       |
| `broccoli contest problems`   | `c p`     | List or download problems             |
| `broccoli test`               | `t`       | Run the sample cases                  |
| `broccoli submit`             | `s`       | Submit a solution                     |
| `broccoli status`             | `st`      | Inspect a submission                  |
| `broccoli watch`              | `w`       | Live contest dashboard                |
| `broccoli clarifications list`| `clar ls` | Read clarifications                   |
| `broccoli clarifications ask` | `clar a`  | Ask a clarification                   |
| `broccoli config`             | `cfg`     | Show or change your defaults          |
| `broccoli completions`        |           | Print a shell completion script       |
| `broccoli prewarm`            |           | Warm caches for a fast first command  |
