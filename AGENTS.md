# fussycat

A unix-philosophy agent CLI: task on stdin, answer on stdout, every
capability granted on the command line. [`context/INTENT.md`](context/INTENT.md)
is the tie-breaker for design questions; read it first. The model and its
language are in `context/domain-design.spec.md`.

**Quilt** is the pattern fussycat implements. Don't name a crate, binary or
command "quilt" (Debian's patch tool owns it); internal types are fine.

## Gates

Run before handing back work, and state the results in the pull request:

```sh
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/arch-eval.sh    # the structural checks; silent on pass
```

`context/architecture.eval.md` explains each check, and the review questions
to ask of a diff. Never run the paid tests (`cargo test -- --ignored`) unless
asked: they spend real money.

## Rules that aren't obvious from the code

- Stdout is the answer and nothing else. Only the CLI and reporters print.
- Secrets never go in argv, the environment or a file: a key comes from a
  command, read through a pipe. Follow that for any new credential.
- Your machine stays out of the repo: hosts, URLs and key commands belong in
  your shell's environment, never in a tracked file.
- No source file over 600 lines; split along real seams.
