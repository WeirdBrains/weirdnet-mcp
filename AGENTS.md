# weirdnet-mcp agents

The collective `AGENTS.md` in `WeirdBrains/weirdnet-agents` is mandatory and wins on conflict.

- Keep TAUR's MCP output byte-for-byte when changing shared behaviour. The `legacy_output_is_unchanged_from_0_5_3` test in `src/tests.rs` guards the legacy (`initialize`) era; change its golden strings only on purpose.
- Build outside the source checkout, with `CARGO_TARGET_DIR` on a scratch disk; clean targets afterwards.
