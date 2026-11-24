---
title: Plugin settings
sidebar_label: Plugin settings
sidebar_position: 3
---

# Plugin settings

A plugin can expose settings that you fill in from the admin area. Depending
on the setting, the same value can be set at up to four levels of
specificity, from a value that covers the whole server down to one problem
inside one contest.

## The four levels

| Level | What it covers | Who can set it |
| --- | --- | --- |
| Global | every contest on the server | plugin manage |
| Problem | one problem, everywhere it appears | problem edit |
| Contest | one contest | contest manage |
| Contest problem | one problem inside one contest | contest manage |

## Which level wins

A contest problem value beats a contest value, which beats a problem value,
which beats the global value. The levels combine field by field: a level
sets only the fields you saved there, and every field it leaves unset comes
from the next broader level, down to the plugin's default. Say the global
compilation setting names gcc-14 for C++ and one problem changes only the
Python interpreter: that problem still compiles C++ with gcc-14.

Take the cooldown plugin's submission cooldown. A contest sets it to sixty
seconds, and one contest problem in that contest sets it to thirty seconds.
Every problem in the contest waits sixty seconds between submissions, except
the one contest problem with its own override, which waits thirty seconds.

Turning a setting off works the same way. The most specific level whose
on/off switch you set decides whether the feature is on, the global level
last, no matter what a broader level says. Disabling a setting at the
contest level turns it off for that whole contest even if the problem level
leaves it on, and its values then fall back to the global ones.

For every field you leave unset, the form shows the value the plugin will
actually use and the level it comes from, global values included. The server
works this out by the same rule the plugin reads its settings by, so what the
form shows is what judging uses.

## Where to set them

In the admin area, open Plugins, pick the plugin, and open its settings. The
form shows a plugin's settings even before you have saved anything, because
the schema comes from the plugin itself, not from a saved row. Contest and
contest problem values are set from the contest's own settings, not from the
Plugins page.

:::note[Defaults are not stored]

A level you have not set stays at its default and does not override a
broader level. Only a value you save counts as set.

:::

## Values the server refuses

Every setting has a type and often a range, and the server checks each value
when you save it. A value of the wrong type, out of range, or not among a
setting's choices is not saved, and the form marks the field the server
refused. A saved value therefore always means what the form showed.

A plugin can also check a value against the rest of what the server holds:
whether a subtask's test cases exist, whether a qualifier has enough places
for its problems. Such a check never refuses a save, since the test data or
the problems may be what needs to change. It is listed as a config issue of
the problem or the contest until one of them changes. A save is refused only
for what is wrong with the value by itself, such as two subtasks with the
same name.

If a plugin is not loaded on the server you are using (it failed to load
there), that server cannot tell what the plugin would say about its
settings. The config issues then ask you to try again rather than showing
none, and any problem the plugin checks stays marked as having issues until
a server that loads the plugin checks it.
