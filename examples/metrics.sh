#!/bin/sh
# Metrics: a dd-style summary on stderr (-q silences it), and --metrics FILE
# appending one JSON line per turn plus one per run, for jq. Counts are what
# the provider reported; anything it didn't report is null, never 0.
#
# The checks: the prefix starts with the tools and fussycat's core prompt,
# `(core)`, which every run has; and a multi-turn run reads that prefix from
# the provider's cache on later turns (cache_read > 0), the point of the quilt.
set -eu
. "$(dirname "$0")/lib.sh"
command -v jq >/dev/null || fail "this example needs jq"

metrics=$(mktemp)
trap 'rm -f "$metrics"' EXIT

say "two tool turns and an answer; turn reports go to $metrics"
answer=$(echo "Call the uptime tool, then call it again in a separate turn, then summarise the load in one sentence." |
  fussycat --tool 'uptime=uptime' --live 'date -u' --metrics "$metrics")
show_answer "$answer"

say "cached prompt tokens per turn, from the JSONL"
jq -r 'select(.type == "turn") | "turn \(.turn): \(.usage.input) in, \(.usage.cache_read) cached"' "$metrics" >&2

prefix=$(jq -rs '[.[] | select(.type == "turn")] | last | [.prefix[].block] | join(" ")' "$metrics")
say "prefix above the log: $prefix"
case $prefix in "(tools) (core)"*) ;; *) fail "the prefix doesn't start with (tools) (core)" ;; esac

cached=$(jq -s '[.[] | select(.type == "turn")] | last | .usage.cache_read // 0' "$metrics")
[ "$cached" -gt 0 ] || fail "the last turn read nothing from the cache"
[ "$(jq -s '[.[] | select(.type == "summary")] | length' "$metrics")" -eq 1 ] ||
  fail "expected one summary line"
say "last turn read $cached prompt tokens from the cache"
