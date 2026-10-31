# Tool files

`--tool 'name=cmd'` is great when the model only needs to trigger something.
When it needs to pass something *in*, write a TOML file and grant it with
`--tool FILE.toml` (`--tool tools/*.toml` grants a folder of them).

A tool file runs a program from a list of arguments, never through a shell,
and checks every argument before anything runs. So whatever text the model
sends can't turn into a command, or even sneak in an option.

A file may start with `version = 1`, the format it's written for. Without
it, it's read as version 1; a file for a newer format is refused (exit 2)
rather than half-understood.

## The narrow shape: typed inputs

Most tools should look like this: one specific job, with typed inputs. Here's
a recipe scaler that knows two recipes and won't cook for more than fifty:

```toml
name = "scale_recipe"
description = "Scales a recipe and prints the shopping list."
run = ["pigglywiggly", "-p", "{recipe}", "-n", "{servings}"]

[input.recipe]
type = "string"
description = "The recipe to scale."
enum = ["pancakes", "porridge"]

[input.servings]
type = "integer"
description = "How many people to feed."
minimum = 1
maximum = 50
```

Each `{placeholder}` becomes exactly one argument. Ask for pancakes for four
and fussycat runs `pigglywiggly -p pancakes -n 4`. Ask for five hundred
servings, or a recipe called `--delete-everything`, and nothing runs: the
model gets back a message saying what was wrong, and can try again.

| Input key | Means |
|---|---|
| `type` | `string`, `integer`, `number` or `boolean` |
| `description` | what the model reads about the input |
| `enum` | the only values allowed |
| `minimum`, `maximum` | bounds for a number; a negative number is a value, so `minimum = 0` refuses one |
| `pattern` | a regex the **whole** value must match (anchored, unlike JSON Schema's, because it guards argv) |
| `allow_leading_dash` | let a string start with `-`; off by default, because the program would read it as an option |

Use this shape for anything with side effects.

## The broad shape: the program's own help

Sometimes you'd rather hand over a whole program and let the model read the
manual:

```toml
name = "pigglywiggly"
description = "A recipe scaler that prints shopping lists."
help = ["pigglywiggly", "--help"]
run = ["pigglywiggly", "{args...}"]
```

fussycat runs the `help` command once, when it loads the file, and adds its
output to the tool's description, so the model learns the options the way
you would. That grants *any* invocation, options included, which is fine for
a program that only reads and prints. The help text rides in every request,
so it's cut at 8 KiB, and it has to finish within `--command-timeout`.

## Passing settings through

Tools start with a clean environment: `PATH`, `HOME`, the locale and a few
more. A tool that needs a setting asks for it by name, and gets only that.
Here's a fun one, fussycat as a tool, so the model can get a second opinion
from another model:

```toml
name = "second_opinion"
description = "Asks another model a question and returns its answer."
# "$1" hands the question over as data to printf, never as code.
run = ["sh", "-c", 'printf "%s\n" "$1" | fussycat', "second_opinion", "{question}"]
env = ["FUSSYCAT_MODEL", "FUSSYCAT_ANTHROPIC_KEY_COMMAND"]

[input.question]
type = "string"
description = "The question, self-contained: the other model sees nothing else."
allow_leading_dash = true   # safe: "$1" is never an option to printf
```

The child fussycat needs to know which model to use and how to get its key,
so `env` passes those two along, and nothing else. Pass key *commands*,
never keys: the child reads its key through a pipe, just like its parent.

All three are in [`examples/tools/`](../examples/tools/), with pigglywiggly
itself, and [`examples/tool-files.sh`](../examples/tool-files.sh) and
[`examples/isolation.sh`](../examples/isolation.sh) run them.
