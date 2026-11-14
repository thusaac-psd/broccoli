---
title: Setting up a contest
sidebar_label: Setting up a contest
sidebar_position: 1
---

# Setting up a contest

Create a contest from the admin area under Contests. Four times decide who
sees it and when it accepts submissions, and the contest type decides how it
is judged and ranked.

## Activation decides who sees the contest

| Time | What it does |
| --- | --- |
| Activate time | contestants see the contest from this time on |
| Start time | submissions open |
| End time | submissions close |
| Deactivate time | contestants stop seeing the contest from this time on |

The times must come in this order. Activate and deactivate time are optional,
and they work differently when you leave them empty.

A contest without an activate time is a draft. Only people who manage contests
see it, whatever its start and end time say, so a contest that is never
activated is never visible to contestants. The contest list in the admin area
marks it as Draft (not activated). Use this while you prepare problems and
settings, then set an activate time, or press Activate now in the contest
form, when contestants should see it.

A contest with an activate time in the future shows as Scheduled until then.
A contest without a deactivate time stays visible after it ends.

## The contest type is fixed at the first submission

The contest type picks the plugin that judges and ranks the contest, such as
ICPC or IOI. You can change it until the contest receives its first
submission. From then on the contest form shows it read only, and the server
refuses a change, because every submission of a contest is judged and ranked
under the same rules.
