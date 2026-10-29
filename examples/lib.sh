# shellcheck shell=sh
# Shared setup for the examples. Source it, don't run it:  . "$(dirname "$0")/lib.sh"
# POSIX sh, so both the sh and the bash examples can source it.

# say: tell the reader what the example shows (stderr, so stdout stays the answer).
say() { printf '== %s\n' "$*" >&2; }

# fail: stop the example with a message and a non-zero exit.
fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

# expect_exit CODE COMMAND...: run COMMAND, fail unless it exits with CODE.
expect_exit() {
  expected=$1
  shift
  status=0
  "$@" || status=$?
  [ "$status" -eq "$expected" ] || fail "expected exit $expected, got $status"
  say "exit $status, as expected"
}

# show_answer ANSWER: fail if fussycat answered with nothing, else print it.
show_answer() {
  [ -n "$1" ] || fail "fussycat printed no answer"
  printf '%s\n' "$1"
}

# Every example lives in examples/, so the repo is one level up from the script.
: "${fussycat_repo:=$(cd "$(dirname "$0")/.." && pwd)}"

# Your machine's settings (an Ollama elsewhere, key commands) live outside the
# repo, in a dev env file: see examples/dev.env.example. Then the defaults;
# anything already set in the environment wins.
dev_env=${FUSSYCAT_DEV_ENV:-$HOME/.config/fussycat/dev.env}
# shellcheck source=/dev/null
[ ! -r "$dev_env" ] || . "$dev_env"
: "${FUSSYCAT_OLLAMA_URL:=http://localhost:11434}"
: "${FUSSYCAT_MODEL:=ollama:gpt-oss:20b}"
export FUSSYCAT_OLLAMA_URL FUSSYCAT_MODEL

# The examples are free: they run on Ollama, and only providers.sh and the
# end of limits-and-exit-codes.sh name a paid model, behind a key command. A
# paid default would turn every example into a bill, so refuse it.
case $FUSSYCAT_MODEL in
  ollama:*) ;;
  *) fail "the examples run on Ollama, but FUSSYCAT_MODEL is $FUSSYCAT_MODEL (set it to ollama:NAME)" ;;
esac

# fussycat: this checkout, built once here, so an example never tests some
# other release that happens to be installed. FUSSYCAT_BIN picks another
# binary on purpose. Its folder goes first on PATH too, for the tools and the
# sandbox that run fussycat by name (isolation.sh).
if [ -z "${FUSSYCAT_BIN:-}" ]; then
  cargo build -q --manifest-path "$fussycat_repo/Cargo.toml" || fail "cargo build failed"
  FUSSYCAT_BIN=${CARGO_TARGET_DIR:-$fussycat_repo/target}/debug/fussycat
fi
[ -x "$FUSSYCAT_BIN" ] || fail "no fussycat binary at $FUSSYCAT_BIN"
PATH="$(dirname "$FUSSYCAT_BIN"):$PATH"
fussycat() { "$FUSSYCAT_BIN" "$@"; }
