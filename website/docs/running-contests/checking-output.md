---
title: Checking output
sidebar_label: Checking output
sidebar_position: 4
---

# Checking output

Every problem has a checker format. It decides how a solution's output is
compared with the expected output of each test. Pick it in the problem form
under Checker format.

## The default compares lines

A new problem starts on `lines`, and the server uses `lines` for a problem
created without one. It compares the output line by line and ignores
whitespace at the end of each line, Windows line endings, and blank lines at
the end of the output. An expected output typed as `3` therefore matches a
solution that prints `3` followed by a newline, which is what `print(3)` and
`cout << 3 << endl` do.

## Formats

| Format | Accepts when |
| --- | --- |
| `lines` | every line matches after trailing whitespace and trailing blank lines are dropped |
| `exact` | the output and the expected output are the same bytes, newlines included |
| `tokens` | the same words appear in the same order, however they are spaced or split across lines |
| `tokens-case-insensitive` | as `tokens`, ignoring upper and lower case |
| `tokens-float` | as `tokens`, with numbers compared within a tolerance |
| `testlib` | the problem's own testlib checker accepts |
| `none` | always, after a clean run, without comparing anything |

Use `exact` only when the precise bytes matter, for example a problem about
output formatting. Under `exact` an expected output entered without its final
newline turns every correct solution into Wrong Answer.

## Tolerances for numbers

`tokens-float` accepts a number when it differs from the expected one by no
more than the larger of two limits. The absolute tolerance is a fixed amount
and defaults to `1e-9`. The relative tolerance is a fraction of the larger of
the two numbers and defaults to `1e-6`. Words that are not numbers still have
to match exactly.

To change them for one problem, open the problem's plugin settings and fill
in the `checker` settings of the standard checkers plugin. A value below zero or
one that is not a number is refused when you save it.
