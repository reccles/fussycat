---
type: pattern
title: Agent Quilt
description: Design an agent around how its context window is assembled - stable, pinned, logged and live blocks ordered for prompt-cache reuse - instead of around personas.
tags: [agents, context-engineering, prompt-caching, progressive-disclosure, design-pattern]
status: draft
timestamp: 2026-10-06T01:20:00Z
---

# Agent Quilt

> A copy of the Agent Quilt pattern, kept here so this repository is self-contained. Its `timestamp` says which version this is.

**Intent:** treat an agent's context window as a quilt sewn from independent
**blocks**, each with its own source, lifetime and owner. The agent's job is to
gather exactly the blocks a problem needs; the harness's job is to lay them out
so the model sees the right things and the provider's prompt cache stays warm.

Persona-first design asks "who is this agent?". Quilt design asks "what is in
the window, where, and for how long?". The second question is the one that
decides cost, latency and answer quality.

## Problem

Most agents are built as a system prompt plus one append-only message log that
everything gets dumped into: instructions, tool results, fetched state,
retrieved documents, conversation. That single log conflates things with very
different lifetimes, which causes:

- **Context rot.** Quality drops as the window fills with stale or irrelevant
  material. Old tool results keep claiming attention long after they matter.
- **Stale state.** A server's status fetched ten turns ago is still in the log,
  next to (or instead of) the current status.
- **Wasted turns.** The agent spends tool calls re-fetching things the harness
  could have known it would need every turn.
- **Cache misses.** Small edits near the top of the window (a timestamp in the
  system prompt, a reordered tool list) invalidate the cache for everything
  after them.

## Forces

- **Too little vs too much.** Too little context and the agent can't solve the
  problem; too much and it rots. The target is the smallest sufficient set.
- **Caches are prefix-based.** Every major provider caches the longest
  unchanged *prefix* of a request (explicit breakpoints at Anthropic, automatic
  prefix caching at OpenAI, implicit/explicit context caching at Gemini).
  Changing anything at position *k* re-bills everything after *k*.
- **Determinism vs judgment.** Some context is always needed (current state of
  the system being operated on); some only the agent can decide it needs
  (which design doc applies). The first shouldn't cost a reasoning step.
- **Freshness vs stability.** Fresh data changes; stable data caches. You
  can't have both in the same block.

## Solution

### Vocabulary

| Term | Meaning |
|---|---|
| **Quilt** | The full context window for one model call, assembled from blocks. |
| **Block** | A unit of context with one source, one lifetime and one owner. In API terms a block usually maps onto one or more content blocks. |
| **Lifetime** | How often a block's content changes. Drives where it goes. |
| **Owner** | Who decides the block is present: the agent's author, the harness, or the agent itself. |
| **Seam** | The boundary between two blocks. Seams are where cache breakpoints go. |
| **Layout** | The order of blocks in the quilt. |

### Block kinds (by lifetime)

| Kind | Lifetime | Typical owner | Examples |
|---|---|---|---|
| **Stable** | Fixed for the agent's whole life | Author | System prompt, `AGENTS.md`, tool definitions |
| **Pinned** | Loaded on demand, then held unchanged | Agent | Knowledge docs pulled from an OKF index, a spec, a schema |
| **Log** | Append-only | Agent + user | Message history, tool calls and results |
| **Live** | Replaced every turn | Harness | Situation state: host status, open PRs, queue depth, clock |

"Static" vs "dynamic" is too coarse: a pinned knowledge doc is *static
content fetched dynamically*, and that combination is the interesting case.

### Layout rule: order by volatility

```
[ stable ][ pinned ][ log ............... ][ live ]
  never     rarely    append-only           every turn
  changes   changes   (prefix preserved)    (only the tail re-bills)
```

Most stable first, most volatile last. Each seam can carry a cache breakpoint,
so a change only invalidates the blocks after it:

- Editing **stable** re-bills everything. It should only happen on deploy.
- Appending to the **log** keeps the whole prefix cached.
- Replacing **live** re-bills only the live block itself.

Putting live state last has a second benefit: the freshest facts sit closest to
where the model generates, so they win over stale mentions earlier in the log.

### Live blocks: deterministic fetch instead of tool calls

On every loop the harness fetches the situation (e.g. state of the hosts the
agent manages) and *replaces* the live block. The agent never has to decide
to look, and old snapshots never pile up in the log.

| | Tool call | Live block |
|---|---|---|
| Who decides | Agent (costs a reasoning step + round-trip) | Harness (free for the agent) |
| Where the result lives | In the log, forever | Replaced each turn |
| Staleness | Goes stale silently | Always current |
| Cost | Only when called | Tokens every turn |

Use a live block when the state is **small, needed on most turns, and cheap to
fetch**. Otherwise keep it a tool. Label live content with when it was fetched
so the model can reconcile it against older mentions in the log.

### Pinned blocks: progressive disclosure

Knowledge is pulled in stages, not preloaded: read a root index, pick a
folder, read its index, pick a document. The
[Open Knowledge Format](https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing)
(one concept per markdown file, frontmatter for metadata, an `index.md` per
folder) is built for this.

Two ways to implement the pull:

1. **Via tools.** The agent reads files; results land in the log. Simple, but
   knowledge then ages and compacts like any other log content.
2. **Via a reserved pinned region.** The agent asks the harness to pin a
   document (possibly chosen through a knowledge graph); the harness places it
   in the pinned region where it stays stable and cached.

Pinning mid-session inserts content *before* the log, which invalidates the
log's cache once. That is the core cache trade-off the agent manages:

- **Append** (into the log) when the knowledge is needed briefly.
- **Pin** when it will be used for many more turns; batch several pins into
  one change so the log is re-billed once, not once per document.
- **Unpin** when the task moves on, ideally at a point where the log is being
  compacted anyway.

### Sub-agents are forks, not personas

There are no personas chatting with each other over an agent-to-agent protocol.
An agent **forks** a sub-agent when a task would consume a lot of context
relative to the size of its answer: scanning logs, searching a codebase,
reading a long document.

- The fork's quilt is built from the parent's blocks: typically the same
  stable blocks (so its first call **hits the parent's cache**), a chosen
  subset of pinned blocks, and a fresh task block instead of the parent's log.
- The fork returns a compact result, which lands in the parent's log as one
  block. The fork's own log is thrown away.
- The parent never sees the fork's noise; the context cost stays in the fork.

## Consequences

**Benefits**

- Lower cost and latency from high cache hit rates, by construction rather
  than by luck.
- Less context rot: state is replaced instead of accumulated; knowledge is
  loaded when needed and dropped when not.
- Fewer wasted turns: the agent reasons about the problem, not about
  fetching the obvious.
- Context becomes inspectable: each block has a name, owner and size, so you
  can see where the tokens go.

**Costs**

- The harness is more complex than "system prompt + list of messages".
- Layout discipline is required; one volatile field in a stable block (the
  classic: today's date in the system prompt) silently defeats caching.
- Live blocks cost tokens every turn, even when unused.
- Two sources of truth can disagree (an old tool result in the log vs the live
  block). Timestamps and the "freshest last" layout mitigate this.

## Anti-patterns

- **Everything in the log.** The default design this pattern replaces.
- **Volatile data in stable blocks:** timestamps, counters, per-request IDs
  in the system prompt.
- **Polling via tools:** the agent calling `get_status` every turn. That is a
  live block in disguise.
- **Persona committees:** splitting one task across "architect", "coder" and
  "reviewer" personas that pass messages, when what's needed is one agent
  with the right blocks (and forks for bulky sub-tasks).
- **Preloading the library:** pinning every knowledge doc "just in case".
  Pull through the index instead.

## Related

- [Agent Quilt Framework](agent-quilt-framework.md) - language-agnostic spec
  for frameworks that make this pattern cheap to apply.
- Retrieval-augmented generation (RAG): retrieval into the log. A quilt
  generalises it by also deciding the lifetime and position of what's
  retrieved.
- Open Knowledge Format: the storage shape for pinned knowledge.

## Open questions

- **Compaction** as a block operation: summarise a log range into a new block
  at a seam, so the prefix before it stays cached?
- **Agent-facing controls:** are `pin`/`unpin`/`fork` explicit tools, or does
  the harness infer them from what the agent reads?
- **Budgets:** should each block kind get a token budget the harness enforces?
- **Measuring "just enough":** what signal tells us a quilt was too thin or
  too thick for a task?
