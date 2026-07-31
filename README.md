# Github governing tooling
<!-- markdownlint-disable MD036 -->

`gh-governor` is a Rust CLI for managing many GitHub repositories in an org using shared configuration sets. It can plan and apply changes to labels, repo settings, branch protection, and files under `.github/`.

## Why use it

- Keep org-wide repo settings consistent.
- Apply shared label and `.github` file sets across many repos.
- Generate an initial config from existing repos to get started quickly.
- Plan changes before applying.

## Concepts

- **Config base**: The directory that contains `gh-governor-conf.{toml,yml,json}` and `config-sets/`.
- **Sets**: Reusable configuration units under `config-sets/<set-name>/`.
- **Repos**: Each repo lists which sets it uses (plus optional defaults).
- **Plan vs apply**: `plan` shows changes; `apply` performs changes.

## Configuration

The main config file is named `gh-governor-conf.{toml,yml,json}` and looks like:

```toml
org = "my-org"
default_sets = ["core"]

[[repos]]
name = "repo1"
sets = ["strict-protection"]

[[repos]]
name = "repo2"
sets = ["docs"]
```

Each set lives under `config-sets/<set-name>/` and can include:

- `labels.{toml,yml,json}`: Label definitions keyed by name.
- `repo-settings.{toml,yml,json}`: Repo settings (including branch protection).
- `.github/**`: Files that should exist in the repo (issue templates, workflows, etc).

Example label file (TOML):

```toml
[bug]
color = "d73a4a"
description = "Something isn't working"

[feature]
color = "0e8a16"
description = "New feature or request"
```

Example repo settings (TOML):

```toml
[pull_requests]
allow_merge_commit = false
allow_rebase_merge = false
allow_squash_merge = true
delete_branch_on_merge = true

[branch_protection]

[[branch_protection.rules]]
pattern = "main"

[branch_protection.rules.required_status_checks]
strict = true
contexts = ["ci"]
```

## CLI usage

Plan changes:

```sh
gh-governor --config-base ./example-conf/toml plan
```

Apply changes:

```sh
gh-governor --config-base ./example-conf/toml apply
```

Generate configs from existing repos:

```sh
gh-governor generate --org my-org --repos repo1,repo2 --output ./generated-conf
```

Notes:

- `GITHUB_TOKEN` must be set (classic PAT recommended).
- Required scopes (classic PAT):
  - Plan/Generate (read-only): `repo` for private repositories, plus `read:org`
    if needed. Public repositories do not require a repository scope for reads.
  - Apply (write): `repo`; the token owner must also have repository admin
    access to update settings such as branch protection.
  - Apply (workflows): add `workflow` if you manage `.github/workflows/*` files (the tool will warn if missing).
- `plan` and `apply` use `--config-base` to select `toml`, `yml`, or `json` config directories.
- `.github/ISSUE_TEMPLATE/config.yml` is synthesized when issue templates exist.

### CLI options

Main binary (`gh-governor`):

- `--token <TOKEN>`: GitHub token (defaults to `GITHUB_TOKEN`).
- `-v`, `--verbose`: extra details for blocked label removals.
- `plan`:
  - `--repo <NAME>` (repeatable): limit to specific repos.
  - `--config-base <PATH>`: config root (default `.`).
- `apply`:
  - `--repo <NAME>` (repeatable): limit to specific repos.
  - `--config-base <PATH>`: config root (default `.`).
- `generate`:
  - `--repos <NAME[,NAME...]>`: repos to harvest (required).
  - `--org <ORG>`: org to read from (required).
  - `--output <PATH>`: output directory (defaults to `./generated-conf-<org>`).
  - `--format <toml|yml|json>`: output format (default `toml`).

## E2E Tests

There exists a `e2e` binary to create repos from the config, seed them with an initial commit, run plan/apply, and verify the end state via the GitHub API (labels, repo settings, branch protection, and `.github` files). Logs are written to `target/e2e-logs`.

**Prerequisites**

- `GITHUB_TOKEN` is set and has admin rights in the test org.
- `delete_repo` scope is required if you use e2e cleanup.
- Test org exists. Has been tested with: <https://github.com/orgs/bwestlin-testing>
- `example-conf` contains `toml/`, `yml/`, `json/` sub-folders with valid configs.

**Run**

```sh
cargo run --bin e2e -- \
  --org bwestlin-testing \
  --config-base example-conf/toml \
  --logs target/e2e-logs \
  --verbose \
  run
```

Note: the e2e runner executes the `gh-governor` binary located next to the `e2e` binary under `target` (debug or release).

**Cleanup**

```sh
cargo run --bin e2e -- \
  --org bwestlin-testing \
  --config-base example-conf/toml \
  --logs target/e2e-logs \
  cleanup
```

**Other formats**

- Use `--config-base example-conf/yml` or `--config-base example-conf/json`.

### E2E runner options

- `--org <ORG>`: test org (required).
- `--token <TOKEN>`: token (defaults to `GITHUB_TOKEN`).
- `--config-base <PATH>`: config root (required).
- `--logs <PATH>`: log directory (default `target/e2e-logs`).
- `-v`, `--verbose`: show detailed output from steps.
- `--continue-on-fail`: run all steps even if a step fails.
- `--no-cleanup`: keep repos after run.
- `--no-build`: skip `gh-governor` build step.
- Subcommands: `run`, `cleanup`.
