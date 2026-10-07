# Contributing

## Development

Install the platform prerequisites in the [README](README.md), then run:

```sh
npm ci
npm run tauri dev
```

Keep English and Russian UI strings in sync. Model selection belongs to recordings; Settings contains application preferences and connection details. Archive changes must preserve existing audio and saved transcripts if they fail.

## Commits and pull requests

Use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) for commits and pull request titles:

```text
feat(models): add audio model search
fix(history): preserve the transcript after a failed retry
docs: clarify Linux setup
```

`fix` and `perf` trigger a patch release. `feat` triggers a minor release. A `!` after the type/scope or a `BREAKING CHANGE:` footer triggers a major release. Other commit types do not trigger a release on their own.

Run the frontend build and Rust checks before opening a pull request:

```sh
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

The release pipeline builds the exact tagged source for macOS universal, Linux x86_64, Windows x86_64 (NSIS), and Android arm64, then publishes only after every artifact and updater signature is verified. CI compiles Linux and Windows and builds Android without release secrets. Dependency caches are shared across compatible CI/release targets; application bundles and signing keys are not cached. See [release artifacts and caches](docs/releases.md). Never commit API tokens, signing keys, recording data, or generated build output.
