#!/bin/sh
# The simplest run: the task on stdin, the answer on stdout. No grants, so the
# model has no tools. --model and --ollama-url override FUSSYCAT_MODEL and
# FUSSYCAT_OLLAMA_URL.
#
# Because diagnostics go to stderr, fussycat is also a filter: build the task
# with other commands, and send the answer on to the next one. Only the answer
# flows down the pipe.
set -eu
. "$(dirname "$0")/lib.sh"

say "a plain question: task on stdin, answer on stdout"
answer=$(echo "In one sentence: what is a fussycat in quilting?" | fussycat)
show_answer "$answer"

say "a filter: a task built from ls, the answer piped through wc"
words=$({
  echo "These are fussycat's example scripts. Describe them in one sentence:"
  ls "$fussycat_repo/examples"
} | fussycat | wc -w | tr -d ' ')

[ "$words" -gt 0 ] || fail "nothing came down the pipe"
say "the answer was $words words long"
