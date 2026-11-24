---
title: Judging
sidebar_label: Judging
sidebar_position: 1
---

# Judging

A submission picks its evaluator from the problem's `problem_type` through the
evaluator registry (`dispatch.rs:15-20`), runs the prepared operations, and
produces a verdict and score for each test. The contest format plugin (ICPC or
IOI) then aggregates those per test results into one submission verdict and a
contest score.

## Verdict and score are separate

A `TestCaseVerdict` carries a `verdict` field and a `score` field
(`evaluate.rs:516-527`), and neither substitutes for the other. Under the batch
evaluator the score comes from the resolved checker through
`interpret_fused_result`: the `interpret` call returns a score, and that score
lands on the verdict as is (`interpret.rs:22-143`). The no checker path
(`checker_format == "none"`, the custom "run code" case) scores a flat `1.0`
pass and compares nothing. So under batch, the checker score is the only way
partial credit reaches the aggregator.

The communication evaluator, used by interactive problems, computes its own
fractional score directly from the interactor's stdout, bypassing
`interpret_fused_result` and the checker abstraction. It parses the
manager's first stdout line as an `f64`, clamps it to `[0.0, 1.0]`, and writes
it straight onto `TestCaseVerdict.score`. A capped score below `1.0` still
yields `Verdict::WrongAnswer`, not a partial Accepted
(`communication-evaluator/src/interpret.rs:263-299`).

A manager that fails on its own (exits non-zero, dies of its own signal, or
prints no valid score) is the problem's failure, which stands
(`PROBLEM_FAILED`), however early the contestants exited: contestants
normally exit before the manager scores, so timing cannot tell a contestant
that left early from one that finished. The contestant is at fault only on
protocol evidence: the manager died of SIGPIPE writing to a contestant that
had exited (a wrong answer), or the manager scored the short interaction
itself, for example with `0`.

## Scoring is decoupled from task type

The evaluator is chosen by `problem_type` through the registry
(`dispatch.rs:15-20`), and that choice is orthogonal to the contest format.
The contest format plugin only decides how the per test verdicts and scores
it receives become a contest score: ICPC collapses them to all or nothing, IOI
sums them into subtask totals. Interactive problems route to their own
evaluator (`communication`); output only problems reuse the batch evaluator
(`batch`) rather than getting one of their own. Either way the evaluator
produces the same `TestCaseVerdict` shape, and ICPC or IOI aggregates it
identically.

## Contest format fidelity

| Area | ICPC | IOI |
| --- | --- | --- |
| Partial credit | none, all or nothing per test | yes, via subtasks and fractional checker scores |
| Subtasks | none | `group_min` (= CMS GroupMin for binary outcomes), `sum` (= CMS Sum), `group_mul` (= CMS GroupMul) |
| Missing CMS feature | not applicable | no GroupThreshold |
| Cross submission scoring | earliest accepted submission wins, not the last | `max_submission`, `sum_best_subtask` (CMS IOI 2017 plus), `best_tokened_or_last` |
| Tiebreak | team name order, not last accepted time | default `max_score_time`, time based, not classic equal rank |
| Built in checker credit | binary | binary, fractional credit only via a testlib points checker |
