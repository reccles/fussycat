#!/bin/sh
# Structural architecture evals S1-S11 (context/architecture.eval.md).
# Each check prints its violations; silence means pass. Exit 1 on any.
# The checks are called by name, through `check`, which shellcheck can't see.
# shellcheck disable=SC2317,SC2329  # older shellchecks (Debian 13) say SC2317
# The greps are approximate: review catches what they miss.
set -u
cd "$(dirname "$0")/.." || exit 2
command -v rg >/dev/null || { echo "arch-eval: needs ripgrep (rg)" >&2; exit 2; }

failed=0
check() {
  name=$1
  shift
  output=$("$@" 2>&1)
  if [ -n "$output" ]; then
    printf '%s FAILED:\n%s\n' "$name" "$output"
    failed=1
  fi
}

s1_core_dependency_allowlist() {
  cargo tree -p fussycat-core -e normal --depth 1 --prefix none | tail -n +2 |
    grep -Ev '^(serde|serde_json|thiserror) '
}
s2_core_touches_nothing_outside_itself() {
  rg -n 'std::(env|fs|net|process)|std::io::(stdin|stdout|stderr)|\b(e?print(ln)?|dbg)!|SystemTime::now|Instant::now' \
    crates/fussycat-core/src
}
s3_no_provider_names_in_core() {
  rg -n -i 'anthropic|openai|ollama|venice|cache_control' crates/fussycat-core/src
}
s4_http_only_in_providers() {
  rg -l 'reqwest|hyper|ureq' crates/fussycat/src | grep -v '/providers/'
}
s5_printing_only_in_cli_and_reporters() {
  rg -l '\b(e?print(ln)?)!|std::io::(stdout|stderr)' crates/fussycat/src |
    grep -Ev '/(cli|reporters)/|/main\.rs$'
}
s6_nothing_named_quilt() {
  grep -rn --include Cargo.toml -E '^name *= *".*quilt' crates Cargo.toml
}
s7_files_under_600_lines() {
  find crates -name '*.rs' -exec wc -l {} + | awk '$2 != "total" && $1 > 600'
}
s8_llm_library_stays_in_providers() {
  rg -l '\brig_core::|\brig_agent::' crates | grep -v '/providers/'
  rg -n 'rig-agent' --glob Cargo.toml .
}

s9_shell_only_in_the_shell_adapter() {
  rg -n '(Command::new|isolated)\("(sh|bash|zsh|dash)"' crates/fussycat/src | grep -v '/system/shell\.rs:'
}
s10_children_start_only_in_process() {
  rg -n 'Command::new|std::process::Command' crates/fussycat/src | grep -v '/system/process\.rs:'
}

# The names are private too, so they live outside the repo: a file of
# regexes, one per line, named by FUSSYCAT_PRIVATE_NAMES (relative to the
# repo root, or absolute). '#' lines are comments; a line '!PATH-REGEX'
# exempts the tracked files it matches. Skipped when the variable is unset.
s11_nothing_names_a_private_network() {
  list=${FUSSYCAT_PRIVATE_NAMES:-}
  [ -n "$list" ] || return 0
  [ -r "$list" ] || { echo "FUSSYCAT_PRIVATE_NAMES: can't read $list"; return 0; }
  names=$(grep -v -e '^#' -e '^!' -e '^$' "$list" | paste -sd '|' -)
  exempt=$(sed -n 's/^!//p' "$list" | paste -sd '|' -)
  [ -n "$names" ] || return 0
  git ls-files | grep -Ev "${exempt:-^\$}" | tr '\n' '\0' |
    xargs -0 rg -n -i -e "$names" /dev/null
}
check S1 s1_core_dependency_allowlist
check S2 s2_core_touches_nothing_outside_itself
check S3 s3_no_provider_names_in_core
check S4 s4_http_only_in_providers
check S5 s5_printing_only_in_cli_and_reporters
check S6 s6_nothing_named_quilt
check S7 s7_files_under_600_lines
check S8 s8_llm_library_stays_in_providers
check S9 s9_shell_only_in_the_shell_adapter
check S10 s10_children_start_only_in_process
check S11 s11_nothing_names_a_private_network
exit "$failed"
