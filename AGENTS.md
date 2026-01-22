# AGENTS.md
<!-- markdownlint-disable MD024 -->

## GENERAL

### Priorities

1. Keep changes minimal and focused.
2. Preserve existing formatting and style.

### Communication

- Be concise in summaries.
- Ask before making structural refactors.

### Documentation

- Keep the documentation updated when making changes.

## RUST specifics

### Priorities

1. Prefer Rust idioms already used in this repo.

### Testing & linting

- Run `cargo check` and `cargo clippy` after Rust changes.
- Do not run tests that require network access.

### Structure

- New modules should live under existing folders (no new top-level crates).
- Avoid introducing new dependencies unless necessary.
  - When introducing dependencies try to avoid introducing multiple version of dependencies in the dependency-tree.

### Error handling

- Use `thiserror` for error handling.
- Avoid `.unwrap()` and `.expect()` unless it's in test code.

### Formatting

- Conform to `rustfmt` with:
  - `imports_granularity = "Item"`.
  - `group_imports = StdExternalCrate`.
- Always run `cargo fmt` after making changes in rust code.
