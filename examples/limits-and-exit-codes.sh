#!/bin/sh
# Every run ends, none floods the context or the bill, and the exit code says
# how it ended, so scripts and cron can tell outcomes apart:
#   0 answered
#   1 failed: the world got it wrong (a provider, a file, a key command)
#   2 usage error: what was typed is wrong (a flag, a setting, the task),
#     caught before anything runs
#   3 budget exhausted
#   130, 143 stopped by SIGINT, SIGTERM (128 + the signal),
#     with the command it was running killed
# The reason is on stderr; stdout stays empty when there is no answer. A
# reader that stops early (| head) doesn't change the code.
#
# The limits:
#   --max-turns N        stop after N model turns (exit 3)
#   --max-cost USD       stop once the provider-reported cost reaches USD (exit 3)
#   --max-tool-output B  cut each tool result to B bytes, telling the model so
#   --max-output-tokens N  ask for replies of at most N tokens; one cut off
#                        there is a failure, not half an answer (exit 1)
#   --command-timeout S  kill a tool or --live command still running after S
#                        seconds; the model is told it timed out
# The cost part is PAID (well under $0.01) and skips without FUSSYCAT_VENICE_KEY_COMMAND.
set -eu
. "$(dirname "$0")/lib.sh"

say "2: usage error, an empty task on stdin"
printf '' | expect_exit 2 fussycat

say "2: --max-cost needs a provider that reports cost; Ollama doesn't"
expect_exit 2 fussycat --max-cost 0.01 </dev/null

say "2: --command-timeout must be more than 0 seconds"
expect_exit 2 fussycat --command-timeout 0 </dev/null

say "1: failed, nothing is listening at --ollama-url (stderr says why: Connection refused)"
# A dropped connection is worth retrying, so by default fussycat tries 3 more
# times (1s, 2s, 4s apart). --retries 0 fails fast.
echo "Hello?" | expect_exit 1 fussycat --retries 0 --ollama-url http://127.0.0.1:9

say "1: --max-output-tokens 16, for five paragraphs: cut off, so nothing reaches stdout"
cut=$(echo "Write five paragraphs about the sea." | expect_exit 1 fussycat --max-output-tokens 16)
[ -z "$cut" ] || fail "a cut-off reply reached stdout: $cut"

say "3: --max-turns 1, but the task needs a tool call and then a second turn"
echo "Call the now tool, then tell me the time." |
  expect_exit 3 fussycat --max-turns 1 --tool 'now=date -u'

say "a tool that hangs, killed after 1 second: the model is told, and still answers"
answer=$(echo "Call the wait tool, then tell me what happened to it." |
  fussycat -v --tool 'wait=sleep 600' --command-timeout 1)
show_answer "$answer"

say "a tool that prints 100,000 lines, cut to 300 bytes: the model is told it saw only part"
answer=$(echo "Call the numbers tool. Did you see all of its output? Answer yes or no, and why." |
  fussycat -v --tool 'numbers=seq 100000' --max-tool-output 300)
show_answer "$answer"

if [ -z "${FUSSYCAT_VENICE_KEY_COMMAND:-}" ]; then
  say "skipped the cost limit: FUSSYCAT_VENICE_KEY_COMMAND is not set (it costs money)"
  exit 0
fi
keeps_calling() {
  echo "Call the now tool, then call it again, five times." |
    fussycat -v --model "venice:${FUSSYCAT_VENICE_MODEL:-claude-haiku-5-5}" \
      --tool 'now=date -u' --max-cost 0.000001
}
say "3: a model told to keep calling a tool, stopped by a \$0.000001 limit after one turn"
expect_exit 3 keeps_calling
