# Contributing to AtrisBridge

Thanks for helping improve AtrisBridge.

## Before opening a pull request

1. Keep changes focused and explain the user or engineering problem being solved.
2. Never include real customer projects, credentials, access tokens, `.env` files, certificates, or proprietary logs in tests or examples.
3. Add or update tests for sync-state, ignore, conflict, recovery, and deletion behavior whenever those areas change.
4. Run the relevant validation commands.

```bash
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## Pull request safety checklist

Any change capable of uploading, overwriting, renaming, moving, or deleting files must document:

- what is considered source-of-truth,
- how interruption/retry behaves,
- how conflicts are detected,
- whether the operation is recoverable,
- what happens when provider state is stale or unavailable.

Blind "latest timestamp wins" synchronization is not an acceptable default.
