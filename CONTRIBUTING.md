# Contributing to Clive

First off, thank you for considering contributing to Clive! 🎉

Contributions of all kinds are welcome — bug reports, feature requests,
documentation improvements, and code. This guide explains how to get set up and
what we expect in a pull request.

By participating in this project you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to Contribute

- **Report a bug** — open an [issue](https://github.com/SedarOlmez94/clive/issues)
  with steps to reproduce, expected vs. actual behavior, and your environment
  (`clive --version`, OS, Ollama version, model).
- **Suggest a feature** — open an issue describing the problem you want solved.
  Check the [Roadmap](README.md#roadmap) first to avoid duplicates.
- **Improve docs** — typos, clearer examples, and better explanations are always
  appreciated.
- **Submit code** — see below.

## Development Setup

You need a recent Rust toolchain (**1.85+**) and [Ollama](https://ollama.com/)
installed.

```bash
# Clone your fork
git clone https://github.com/YOUR_USERNAME/clive.git
cd clive

# Build
cargo build

# Run locally without installing
cargo run -- doctor

# Run the test suite
cargo test
```

## Submitting Changes

1. **Fork** the repository and create a branch:
   ```bash
   git checkout -b feature/your-feature-name
   ```
2. **Make your change**, adding tests for new behavior where practical.
3. **Run the full check suite** — CI runs the same commands, so please make sure
   they pass locally first:
   ```bash
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
4. **Commit** with a clear message (see convention below).
5. **Push** to your fork and open a **Pull Request** describing what changed and
   why.

## Code Style

- **Formatting:** `cargo fmt` (rustfmt defaults). CI checks `cargo fmt --check`.
- **Linting:** `cargo clippy --all-targets -- -D warnings` must pass with **zero
  warnings**.
- Prefer clear, well-documented functions. Public-facing behavior changes should
  update the `README.md`.
- Keep changes focused; unrelated refactors are best in separate PRs.

## Testing

- Add unit tests in the `#[cfg(test)]` module in `src/main.rs` for new logic.
- Use descriptive names, e.g. `resolve_model_name_prefers_local_global_installed_default`.
- Tests must not depend on a running Ollama server or the network; mock or stub
  external calls (see the existing `pull_model_with_runner` test for an example).

## Commit Message Convention

Use clear, imperative messages:

- `Add: stdin piping support for chat`
- `Fix: stream thinking tokens so reasoning models do not appear to hang`
- `Docs: expand troubleshooting section`

## Reporting Security Issues

Please **do not** open public issues for security vulnerabilities. See
[SECURITY.md](SECURITY.md) for how to report them privately.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE).
