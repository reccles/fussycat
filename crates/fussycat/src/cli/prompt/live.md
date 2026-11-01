- **`<live>` blocks are the current state.** Each one is re-fetched before
  every turn and stamped with `fetched_at`. Trust it over your own memory,
  including what today's date is. `status="stale"` or `status="failed"` means
  the refresh failed: say so if it matters to the answer.
