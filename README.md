# gh-governor
<!-- markdownlint-disable MD036 -->

## TODO

- [ ] Fix PR flow for `.github` changes so apply always creates/updates a PR when there are file diffs.
- [ ] Share one planning pipeline between plan/apply; apply should consume the same computed plan output.
- [ ] Add explicit apply summary that indicates which actions were executed and which were skipped.
- [ ] Add test scaffolding for end-to-end runs against a test org (recorded config, expected plan/apply effects).
- [ ] Add integration tests that exercise branch protection + `.github` file updates + labels in one run.
- [ ] Add diagnostics for mismatches between planned changes and post-apply state.
- [ ] Add support for branch rulesets and detect if there exists inconsistency or clashes if both are used.
- [ ] Add documentation in this readme about this tool, what it is for, how it's used etc.
- [ ] Document what scopes/permissions is needed to run the tool.

## E2E Tests

There exists a `e2e` binary to create repos from the config, seed them with an initial commit, run plan/apply, and verify the end state via the GitHub API (labels, repo settings, branch protection, and `.github` files). Logs are written to `target/e2e-logs`.

**Prerequisites**

- `GITHUB_TOKEN` is set and has admin rights in the test org.
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

Optional flags: `--verbose` to show plan/apply output, `--continue-on-fail` to run all steps before exiting non-zero, `--no-cleanup` to keep repos after the run.

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
