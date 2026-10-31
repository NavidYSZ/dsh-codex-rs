# Working on dsh-codex

This repository contains the standalone Rust agent and native macOS app. The
older plugin lives in `NavidYSZ/dsh-codex`; installing that plugin changes an
existing DeepSeek Harness installation.

- Keep the public repository current with completed, verified product changes.
  Update the README and changelog with user-visible behavior. The owner has
  requested GitHub publication of this app and the completed fixes.
- Preserve unrelated work in the checkout. Do not add personal configuration,
  credentials, local instance lists, build directories or development chat logs.
- Standard and Minimal retain their existing model contracts. PTC exposes only
  `run_code`; nested calls must use the normal tool policies and cancellation.
- Accepted input must remain visible and recoverable until a user message is
  committed or the user explicitly removes it. Queued follow-ups run FIFO.
- Compact PTC/active Goal work only at settled model/tool boundaries. Keep the
  latest batch, pending input and canonical Goal state; Stop must prevent
  continuation through terminal commits, compaction and flush.
- Browser diagnostics use temporary profiles and local test pages. Distinguish
  MCP discovery from actual interaction, bind results to the tested executable,
  and never silently use desktop control to work around a website failure.
- Run relevant regression checks, `cargo fmt --all -- --check`,
  `cargo clippy --locked --workspace --all-targets -- -D warnings`,
  `cargo test --locked --workspace` and the `ui/dev/*_check.mjs` scripts before
  a release. Test the app with its bundled runtime and a clean data directory.
- Publish release binaries built from the exact tagged source. Keep `main`
  current with verified changes; test/documentation maintenance may advance it
  beyond a release tag. Do not leave completed work only on a local branch.
  Never force-push or publish unfinished unrelated experiments.
- Installing a build must preserve existing user data. Check running tasks
  before restarting the installed app; a public build starts with a local
  instance only, while existing users keep their saved instances.
