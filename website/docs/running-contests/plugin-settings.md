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

A contest problem value beats a contest value, which beats a problem value.
These three levels combine automatically, and whichever one is most specific
wins. Global works differently. The plugin that defines a setting reads a
global value on its own, so a global value applies wherever that plugin's
own code chooses to use it, not as an automatic fallback under the other
three levels.

Take the cooldown plugin's submission cooldown. A contest sets it to sixty
seconds, and one contest problem in that contest sets it to thirty seconds.
Every problem in the contest waits sixty seconds between submissions, except
the one contest problem with its own override, which waits thirty seconds.

Turning a setting off works the same way, for the levels where the admin
form offers an on or off switch, which are contest and problem. Whichever
level is the most specific one you have set decides whether the feature is
on or off, no matter what a broader level says. Disabling a setting at the
contest level turns it off for that whole contest even if the problem level
leaves it on.

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
