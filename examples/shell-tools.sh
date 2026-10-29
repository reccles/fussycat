#!/bin/sh
# Deny by default, then a grant. With no --tool the model gets no tools at
# all, so it cannot know the time (expect it to say so). --tool 'NAME=COMMAND'
# lets it run COMMAND: the command is yours, exactly as written, and the tool
# takes no arguments, so nothing the model says ever reaches a shell. --tool
# repeats.
#
# The check: with one turn allowed, a tool call would exhaust the budget
# (exit 3, see limits-and-exit-codes.sh). Exiting 0 means the model answered
# without calling anything.
set -eu
. "$(dirname "$0")/lib.sh"

say "no grants: the model has no tools, so it cannot know the time"
answer=$(echo "What time is it?" | fussycat --max-turns 1)
show_answer "$answer"

say "granting 'now=date -u' so the model can find out the time"
answer=$(echo "What time is it?" | fussycat --tool 'now=date -u')
show_answer "$answer"
