#!/usr/bin/env bash
# Context blocks: what the model sees besides the task.
#  - --stable FILE: read once, before the first turn, and laid out ahead of
#    the task. It repeats, and takes any file, including the output of a
#    command through process substitution, --stable <(cmd).
#  - --live 'cmd': re-run before every turn and shown to the model after the
#    log, with its fetch time. -v prints a line per turn, so you can watch it
#    refresh while the cached prefix grows.
#
# This is the one bash example: <(cmd) is a bash/zsh feature, not POSIX sh.
# In plain sh, write the command's output to a temp file and pass that instead.
#
# The checks: facts that are only in the stable blocks reach the answer, and
# a live block that fails renders as failed: it never aborts the turn.
set -eu
. "$(dirname "$0")/lib.sh"

brief=$(mktemp)
trap 'rm -f "$brief"' EXIT
echo "The project codeword is MARMALADE." >"$brief"
# Once, in English: a second date call could land after midnight, and %A is
# localised ("lundi") while the model may well answer "Monday".
today=$(LC_ALL=C date -u +%A)

say "two stable blocks: a file, and <(echo ...) via process substitution"
answer=$(echo "What is the codeword, and what day of the week is it? One line." |
  fussycat --stable "$brief" --stable <(echo "Today is $today."))
show_answer "$answer"

# The facts are only in the blocks, so finding them proves the blocks arrived.
grep -qi marmalade <<<"$answer" || fail "the file block's codeword is missing"
grep -qi "$today" <<<"$answer" || fail "the <(echo) block's weekday is missing"

say "a live clock, refreshed every turn (see the turn lines on stderr)"
answer=$(echo "Call the uptime tool, then call it again in a separate turn, then tell me the time from the live block." |
  fussycat -v --live 'date -u +%H:%M:%S' --tool 'uptime=uptime')
show_answer "$answer"

say "a live block that fails is shown as failed; the run still answers"
answer=$(echo "What does the live block say? One sentence." |
  fussycat -v --live 'echo "sensor offline" >&2; exit 3')
show_answer "$answer"
