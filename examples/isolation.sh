#!/bin/sh
# Isolation, from the inside and the outside.
#  - Inside: every tool and --live command gets a clean environment. Your
#    keys and tokens stay with fussycat unless a tool file names them (env).
#  - Outside: fussycat doesn't sandbox; it's built to be sandboxed. Here it runs
#    under macOS's sandbox-exec with no web access and a read-only home.
set -eu
. "$(dirname "$0")/lib.sh"

tools="$fussycat_repo/examples/tools"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

say "a tool sees no secrets: it saves its environment, and fussycat's fake keys aren't in it"
export VENICE_API_KEY=fussycat-example-fake-key GITHUB_TOKEN=fussycat-example-fake-token
# The path is single-quoted inside the command, so a TMPDIR with spaces works.
answer=$(echo "Call the peek tool once, then say done." |
  fussycat -v --tool "peek=env > '$scratch/env.txt' && echo saved")
unset VENICE_API_KEY GITHUB_TOKEN
show_answer "$answer"
[ -s "$scratch/env.txt" ] || fail "the model never called peek"
if grep -q fussycat-example-fake "$scratch/env.txt"; then fail "a key reached the tool"; fi
say "the tool saw only (sh adds PWD, SHLVL and _ itself): $(cut -d= -f1 "$scratch/env.txt" | sort | tr '\n' ' ')"

# lib.sh put this checkout's fussycat first on PATH, so the child is the same build.
say "a child fussycat as a tool: second-opinion.toml passes on just what the child needs"
answer=$(echo "Use second_opinion to ask what the capital of Australia is. Reply with its answer." |
  fussycat -v --tool "$tools/second-opinion.toml")
show_answer "$answer"
case $answer in *Canberra*) ;; *) fail "expected Canberra from the child" ;; esac

if ! command -v sandbox-exec >/dev/null; then
  say "skipped the sandbox: no sandbox-exec (it's macOS); see examples/sandbox/macos.sb"
  exit 0
fi
profile="$fussycat_repo/examples/sandbox/macos.sb"
say "in the sandbox, the web is unreachable"
if sandbox-exec -D HOME="$HOME" -f "$profile" curl -sS -m 5 -o /dev/null https://example.com 2>/dev/null; then
  fail "the sandbox let curl reach the web"
fi
say "but fussycat still reaches Ollama and answers, and its tools inherit the sandbox"
answer=$(echo "Call the fetch tool, then say in one sentence whether it worked." |
  sandbox-exec -D HOME="$HOME" -f "$profile" "$FUSSYCAT_BIN" -v --tool 'fetch=curl -sS -m 5 https://example.com')
show_answer "$answer"
