# AI output v2

AtrisBridge keeps AI-facing command and Git output bounded while preserving the most useful diagnostics when output exceeds the response budget.

- Fixed command profiles keep up to 1 MiB for stdout and 1 MiB for stderr.
- Git stdout/diff capture keeps up to 2 MiB; Git stderr capture keeps up to 64 KiB.
- Truncated streams preserve a small prefix and the newest tail, separated by an explicit AtrisBridge truncation marker.
- Structured Git reads still fail closed when their output is truncated, so a truncation marker is never parsed as repository metadata.
- `git_diff` can return bounded truncated text because its response already carries an explicit `truncated` flag.

These budgets remain below the remote MCP relay response ceiling after normal JSON/envelope overhead while providing substantially more build, test, and diff diagnostics than the previous first-bytes-only capture.
