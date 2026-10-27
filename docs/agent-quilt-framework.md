---
type: design
title: Agent Quilt Framework
description: Language-agnostic spec for frameworks that implement the Agent Quilt pattern - block kinds, operations and their guarantees, provider cache mapping, forks and per-turn reporting. Implementations live in their own projects.
tags: [agents, context-engineering, prompt-caching, framework, spec]
status: proposal
timestamp: 2026-10-06T03:24:00Z
---

# Agent Quilt Framework

> A copy of the Agent Quilt Framework spec, kept here so this repository is self-contained. Its `timestamp` says which version this is.

What any framework implementing the [Agent Quilt](agent-quilt.md) pattern must
do, independent of language or runtime. Read the pattern first; this document
only covers the framework.

This is a **spec, not an API**. It names concepts and the guarantees each
operation gives. Implementations (a Unity/C# agent, a Rust agent, ...) choose
their own types, names and idioms, live in their own repositories, and link
back here. "MUST", "SHOULD" and "MAY" are used as in RFC 2119.

## Goals

1. **Blocks are the unit of design.** Declaring a block (its kind, source and
   name) is the main thing an agent author does.
2. **Cache placement is automatic.** Authors declare lifetimes; the framework
   orders blocks and places cache breakpoints at seams.
3. **Provider-agnostic.** The same quilt renders for any provider. Explicit
   caching APIs are used where they exist; elsewhere the layout produces a
   stable prefix.
4. **Observable.** Every call reports tokens and cache behaviour per block, so
   layout mistakes show up as numbers.

## Non-goals

- Personas, role-play prompts, or agent-to-agent chat protocols.
- A prompt templating language. Blocks hold content; how it's produced is
  the author's business.
- Owning the whole tool-execution loop beyond what forks need.
- A specific programming language, runtime or concurrency model.

## Concepts

| Concept | Is |
|---|---|
| **Block** | A named unit of context: kind, content, token count, and (for live blocks) a fetch source. Names are unique within a quilt. |
| **Quilt** | The ordered set of blocks for one agent. Owns the layout rule and assembles one request per turn. |
| **Adapter** | Renders a quilt into one provider's request format and places cache breakpoints. One per provider. |
| **Fork** | A child quilt derived from a parent's blocks plus a task; yields one result block. |
| **Turn report** | Per-block tokens and cache behaviour for one model call. |

Block kinds are defined in the pattern: **stable**, **pinned**, **log**,
**live**.

## Layout invariants

- **L1.** Blocks MUST be laid out in kind order: stable, pinned, log, live.
  Within a kind, declaration order is kept.
- **L2.** A quilt that declares blocks out of kind order MUST be rejected when
  it's built, not silently reordered.
- **L3.** Rendering MUST be deterministic: the same blocks with the same
  content produce byte-identical request prefixes (stable key order, no
  generated IDs or timestamps outside live blocks).
- **L4.** Stable block content MUST NOT change during a session. Changing it
  means starting a new session.

## Operations

Each operation lists its guarantees. How it is exposed (method, message,
component, command) is up to the implementation.

**`turn(input)`** - run one model call.
1. Refresh every live block (see `refresh`).
2. Append `input` to the log.
3. Render through the adapter and call the model.
4. Append the response (and any tool calls and results) to the log.
5. Produce a turn report.

**`refresh(live block)`** - fetch and replace a live block's content.
- MUST run before every `turn`. MAY run concurrently across live blocks.
- MUST record when the content was fetched and render it with the content.
- If a fetch fails, the block MUST render as stale (last good content plus
  its age and the error) rather than disappear.

**`append(log, entry)`** - add to the log.
- MUST NOT modify earlier log entries. That is what keeps the prefix cached.

**`pin(document)` / `unpin(document)`** - add or remove a pinned block.
- A pinned block's content MUST NOT change until it's unpinned.
- Pins and unpins requested during one turn MUST be applied together before
  the next render, so the log's cache is invalidated once, not once per
  document.
- Pinning an already-pinned document MUST be a no-op.

**`compact(log range)`** - replace a range of log entries with a summary.
- MUST only replace a contiguous range ending at a seam or at the end of the
  log; entries before the range MUST stay byte-identical.
- SHOULD be reported in the turn report, since it re-bills what follows.

**`fork(blocks, task)`** - run a sub-agent; see [Forks](#forks).

## Standard blocks

Implementations SHOULD provide these, because they cover nearly every agent:

| Block | Kind | Behaviour |
|---|---|---|
| Text | stable | Literal content, e.g. the system prompt. |
| File | stable | A file such as `AGENTS.md`, read once at start-up. |
| Tools | stable | Tool definitions in a fixed, sorted order. |
| Knowledge | pinned | Reads an OKF index tree; exposes the root index to the agent; supports `pin`/`unpin` by path. |
| Message log | log | Append-only history with `compact`. |
| State | live | Structured state from a fetch source, refreshed each turn, rendered with its fetch time. |

## Provider adapters

| Provider | Mapping | Caching |
|---|---|---|
| Anthropic | stable + pinned -> `tools` and `system`; log -> `messages`; live -> appended to the final user turn | Explicit `cache_control` breakpoints at the most valuable seams (the API limits how many) |
| OpenAI | stable + pinned -> leading system/developer messages; log -> messages; live -> final message | Automatic prefix caching; the adapter keeps the prefix byte-identical |
| Gemini | stable + pinned -> system instruction / cached content | Implicit caching, or explicit cached-content resources for large pinned sets |

- **A1.** When a provider limits breakpoints, the adapter MUST rank seams by
  tokens protected and place breakpoints there first: typically after stable,
  after pinned, and at the end of the log.
- **A2.** Provider-specific details MUST stay inside the adapter; nothing
  above it changes when the provider does.

Provider caching rules change; check each provider's current documentation
when implementing an adapter.

## Forks

A fork is a new quilt built from:

1. A selection of the parent's stable and pinned blocks, by name.
2. Optional extra blocks the parent creates for it.
3. A task block in place of the parent's log.

- **F1.** Selected parent blocks MUST render byte-identically in the fork, so
  the fork's first call can reuse the parent's cache.
- **F2.** The fork returns exactly one result block, which is appended to the
  parent's log.
- **F3.** The fork's own log MUST NOT enter the parent's context. It MAY be
  kept for debugging.

## Turn report

Each model call MUST report:

- tokens per block and per kind;
- cache read and cache write tokens, where the provider reports them;
- the **first seam where the prefix diverged** from the previous call, and
  which block caused it ("cache broke at `agents`: content changed");
- live-block fetch time and any fetch failures.

The divergence line is the most useful debugging signal in the framework: it
turns an invisible cost problem into an obvious one.

## Implementations

| Implementation | Language / runtime | Repository | Status |
|---|---|---|---|
| Unity agent | C# / Unity | (link to add) | planned |
| Rust agent | Rust | (link to add) | possible |

Implementations SHOULD link back to this document from their `AGENTS.md` and
state which version (`timestamp`) of the spec they follow.

## Open questions

- **Conformance tests:** language-neutral fixtures (a quilt definition in,
  expected request layout and breakpoints out, as JSON) that every
  implementation runs. Worth it once there are two implementations.
- **Compaction policy:** time-, size- or task-boundary-triggered? Does a
  summary become a pinned block or stay in the log?
- **Who drives pinning:** explicit agent tools (`pin`, `unpin`) or harness
  heuristics from what the agent reads? Possibly both, behind one operation.
- **Knowledge graph:** is folder-and-index progressive disclosure enough, or
  does the Knowledge block need a graph (tags, links) to choose documents?
