# Contributing to Tessera

Thanks for helping. Bug reports, diagnosis rule ideas and pull requests are all welcome.

## Set up

You need Rust (stable), Node.js 22+, and the Tauri system dependencies for your OS:
<https://v2.tauri.app/start/prerequisites/>.

```sh
npm install
npm run tauri dev      # desktop app against your real kubeconfig
npm run dev            # browser only, with built-in demo data
cargo test -p tessera-core
```

## Ground rules

- **Read-only, always.** Tessera must never create, update, patch or delete
  cluster objects. Any PR that adds a write verb will be declined. Suggested
  fixes are shown as text and commands for the user to run.
- **No telemetry and no network calls** other than to the API servers in the
  user's kubeconfig.
- **Rules need evidence.** Every issue should say what was observed and give a
  command the user can run to confirm it. See `docs/architecture.md`.
- **Tests for the engine.** Changes in `tessera-core` need a unit test or a
  fixture test.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p tessera-core
npm run build
```

By contributing you agree that your contributions are licensed under the
Apache License 2.0.
