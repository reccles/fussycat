#!/bin/sh
# Providers: the same agent, flags and quilt everywhere; only --model changes.
#  - --list-models PROVIDER: what can I run? One model per line: first the
#    exact --model value, then key=value facts the provider reports (context=,
#    tools=, prices in $ per million tokens). Unreported facts are left out.
#    Listing calls no model, so it's free, and Venice's needs no key.
#  - venice:NAME reports what each call cost, so the summary ends in dollars.
#  - anthropic:NAME is the one provider with explicit cache breakpoints:
#    fussycat marks its seams (the tools, the stable blocks, the end of the
#    log) and never the live blocks. It reports cache writes as well as
#    reads, and no dollars, so the summary shows tokens only.
#
# The Venice and Anthropic parts are PAID (about a cent a run together), so
# each skips unless fussycat can get its key, from a command:
#   FUSSYCAT_VENICE_KEY_COMMAND='pass show venice/api-key' examples/providers.sh
# (fussycat runs that command and reads the key from its pipe: the key itself
# is never in argv, the environment or a file.) FUSSYCAT_VENICE_MODEL and
# FUSSYCAT_ANTHROPIC_MODEL pick the models (claude-haiku-5-5 by default).
#
# The paid checks: with fussycat's domain spec as a stable block (a prefix
# over the providers' ~1024-token cache minimum), the last turn reads it from
# the cache; Venice reports a cost and Anthropic doesn't.
set -eu
. "$(dirname "$0")/lib.sh"

say "ollama: every model on the server"
fussycat --list-models ollama

say "venice: the 3 cheapest models that can call tools (a plain pipeline)"
fussycat --list-models venice |
  grep 'tools=yes' |
  sed -n 's|.* in=\$\([0-9.]*\)/M.*|\1 &|p' |
  sort -n | head -3 | cut -d' ' -f2-

say "the first field is a --model value, so a listing feeds a run"
model=$(fussycat --list-models ollama | grep 'tools=yes' | cut -d' ' -f1 | grep -x "$FUSSYCAT_MODEL") ||
  fail "$FUSSYCAT_MODEL is not listed with tools=yes"
answer=$(echo "Say hello in three words." | fussycat --model "$model")
show_answer "$answer"

metrics=$(mktemp)
trap 'rm -f "$metrics"' EXIT

# spec_run MODEL: a two-turn run with a big, cacheable prefix, into $metrics.
spec_run() {
  : >"$metrics"
  say "$1, with fussycat's domain spec as a stable block (a big, cacheable prefix)"
  answer=$(echo "Call the uptime tool, then say in one sentence what a Quilt is, per the spec." |
    fussycat --model "$1" --stable "$fussycat_repo/context/domain-design.spec.md" \
      --tool 'uptime=uptime' --metrics "$metrics")
  show_answer "$answer"
}
# turns FILTER: jq FILTER over the run's turn lines.
turns() { jq -s "[.[] | select(.type == \"turn\")] | $1" "$metrics"; }
summary_cost() { jq -s '.[] | select(.type == "summary") | .usage.cost_usd' "$metrics"; }

if [ -z "${FUSSYCAT_VENICE_KEY_COMMAND:-}" ]; then
  say "skipped venice: FUSSYCAT_VENICE_KEY_COMMAND is not set (it costs money)"
else
  command -v jq >/dev/null || fail "the paid parts need jq"
  spec_run "venice:${FUSSYCAT_VENICE_MODEL:-claude-haiku-5-5}"
  cached=$(turns 'last | .usage.cache_read // 0')
  cost=$(summary_cost)
  [ "$cached" -gt 0 ] || fail "the last turn read nothing from Venice's cache"
  jq -e -n "${cost:-0} > 0" >/dev/null 2>&1 || fail "Venice reported no cost"
  say "last turn read $cached prompt tokens from the cache; the run cost \$$cost"
fi

if [ -z "${FUSSYCAT_ANTHROPIC_KEY_COMMAND:-}" ]; then
  say "skipped anthropic: FUSSYCAT_ANTHROPIC_KEY_COMMAND is not set (it costs money)"
else
  command -v jq >/dev/null || fail "the paid parts need jq"
  spec_run "anthropic:${FUSSYCAT_ANTHROPIC_MODEL:-claude-haiku-5-5}"
  # The first turn writes the prefix, or reads it back if a run in the last
  # 5 minutes wrote it: either way it went through the cache.
  written=$(turns 'first | .usage.cache_write // 0')
  through=$(turns 'first | (.usage.cache_write // 0) + (.usage.cache_read // 0)')
  cached=$(turns 'last | .usage.cache_read // 0')
  cost=$(summary_cost)
  [ "$through" -gt 0 ] || fail "the first turn's prefix never went through Anthropic's cache"
  [ "$cached" -gt 0 ] || fail "the last turn read nothing from Anthropic's cache"
  [ "$cost" = null ] || fail "Anthropic reports no cost, yet the run says $cost"
  say "first turn: $through prompt tokens through the cache ($written newly written); the last turn read $cached"
fi
