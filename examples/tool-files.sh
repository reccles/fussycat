#!/bin/sh
# Tool files: --tool FILE.toml declares a tool the model couldn't guess from
# its name, with a description, typed inputs, or the program's own --help.
# The program runs from an argv list, never through a shell. --tool takes
# several values, so a folder of tool files works: --tool tools/*.toml
#
# A typed string can't start with `-` unless its input sets
# allow_leading_dash, and a pattern must match the whole value.
set -eu
. "$(dirname "$0")/lib.sh"

tools="$fussycat_repo/examples/tools"
PATH="$tools:$PATH" # pigglywiggly, a made-up program, lives next to its tool files

say "a broken tool file is refused before any model call (exit 2)"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
printf 'name = "oops"\nrun = ["pigglywiggly", "-p", "{recipe}"]\n' >"$scratch/oops.toml"
expect_exit 2 fussycat --tool "$scratch/oops.toml" </dev/null

say "an option as a choice needs allow_leading_dash = true, so it's never by accident (exit 2)"
printf 'name = "ls"\ndescription = "d"\nrun = ["ls", "{flag}"]\n[input.flag]\ntype = "string"\nenum = ["-l"]\n' >"$scratch/dash.toml"
expect_exit 2 fussycat --tool "$scratch/dash.toml" </dev/null

say "narrow: scale-recipe.toml, typed inputs filling a fixed command line"
answer=$(echo "I'm making pancakes for 6. What do I need to buy?" |
  fussycat -v --tool "$tools/scale-recipe.toml")
show_answer "$answer"

say "broad: pigglywiggly.toml, the model builds the call from pigglywiggly --help"
answer=$(echo "How much of everything do I need to make porridge for 3 people?" |
  fussycat -v --tool "$tools/pigglywiggly.toml")
show_answer "$answer"
