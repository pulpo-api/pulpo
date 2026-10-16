# Contributing to Pulpo

Thanks for helping! Pulpo is early, so the best contributions right now are ideas, issues and small, focused pull requests.

## Ground rules

- Be kind. Assume good intent.
- Open an issue before large changes so we can agree on the approach.
- Keep pull requests small and focused on one thing.

## Developer Certificate of Origin (DCO)

Every commit must be signed off, certifying that you wrote the code or have the right to submit it under the project licenses ([developercertificate.org](https://developercertificate.org)).

```bash
git commit -s -m "feat(http): add request timeout"
```

This adds a line like `Signed-off-by: Your Name <you@example.com>` to the commit message.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org): `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, with an optional scope (`feat(grpc): ...`).

## Before you push

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/desktop && npm run build
```

## Project principles

- No accounts, no forced cloud, no telemetry by default.
- Collections are plain, reviewable files in git.
- Secrets never go into the workspace files.
- Every protocol sits behind the same engine interface.

## License

By contributing, you agree that your contributions are dual licensed under MIT OR Apache-2.0, as described in the [README](README.md#license).
