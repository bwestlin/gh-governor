# Github government tooling

## TODO

- [ ] Fix PR flow for `.github` changes so apply always creates/updates a PR when there are file diffs.
- [ ] Share one planning pipeline between plan/apply; apply should consume the same computed plan output.
- [ ] Add explicit apply summary that indicates which actions were executed and which were skipped.
- [ ] Add test scaffolding for end-to-end runs against a test org (recorded config, expected plan/apply effects).
- [ ] Add integration tests that exercise branch protection + `.github` file updates + labels in one run.
- [ ] Add diagnostics for mismatches between planned changes and post-apply state.
- [ ] Add support for branch rulesets and detect if there exists inconsistency or clashes if both are used.
- [ ] Add documentation in this readme about this tool, what it is for, how it's used etc.
