# Contributing to slake

This project is intentionally small: one SQLite-backed CLI, one policy, one test suite. Keep changes focused and keep the execution contract predictable.

## Getting Started

1. Fork the repository.
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/slake.git`
3. Enter the repo and run setup: `cd slake && ./scripts/setup.sh`
5. Create a branch: `git checkout -b your-feature`

## Development Workflow

1. Make your changes.
2. Run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --locked`.
3. Update docs when the CLI contract or storage behavior changes.
4. Commit with clear, descriptive messages.
5. Push to your fork and open a Pull Request.

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/) style:

```
feat: add new parsing mode
fix: handle empty input gracefully
docs: update installation steps
chore: update dependencies
```

## Pull Request Process

1. Fill out the PR template.
2. Ensure CI passes.
3. Keep PRs focused; one logical behavior or tooling change per PR.
4. Explain any CLI or cooldown policy change with a before/after example.

## Release Process

1. Update `Cargo.toml` and `CHANGELOG.md` for the intended version.
2. Run `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --locked`.
3. Create and push a `vX.Y.Z` tag that matches `package.version` in `Cargo.toml`.
4. Let the release workflow package artifacts once it exists.

## Code Standards

- Keep the command-execution surface explicit and documented.
- Keep the storage schema boring and inspectable.
- Write tests for any CLI or cooldown behavior change.
- No secrets, credentials, or internal paths in code, docs, or examples.
- Favor explicit policy and documented tradeoffs over hidden behavior.

## Reporting Issues

Use the GitHub issue templates. For bugs, include the exact command, expected behavior, actual behavior, and whether a custom `--db` path was involved.

## License

By contributing, you agree that your contributions will be licensed under the same license as this project (AGPL-3.0).

---

Built by [Greyforge](https://greyforge.tech)
