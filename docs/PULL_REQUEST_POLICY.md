# Pull Request Policy

This document defines the expected merge bar for SOMA.

## Required Checks

Every PR should pass:

```bash
cargo fmt -- --check
cargo test -p soma --lib
cargo build -p soma --features dashboard
```

The GitHub `CI / rust` workflow mirrors this policy.

`cargo clippy -p soma --all-targets` is run as an advisory check. It should be
improved over time, but existing public snapshot warnings do not block the first
portfolio release.

## Review Requirements

PRs should be reviewed before merge. For solo-maintainer portfolio work, this
can be self-review plus passing CI, but the PR description must still explain:

- what changed
- why the change exists
- how it was validated
- whether trust, memory lifecycle, projection, or client proof semantics changed

## High-Risk Areas

Changes in these areas require extra care:

- cloud output capture
- claim records and verification events
- L2 to L3 promotion
- L3 decay/forgetting
- L4 semantic fact promotion
- ContextEnvelope projection
- TaskFrame privacy projection
- MCP tools that write state
- client binding proof and release hardening

For these areas, a PR should include tests or an explicit reason tests are not
possible yet.

## Merge Rules

Recommended branch rule for `main`:

- require pull request before merge
- require at least one approval when collaborators are present
- require conversation resolution
- require status checks: `CI / rust`
- require branch to be up to date before merge
- require linear history
- block force pushes
- block deletions

## Commit Style

Prefer concise imperative commits:

```text
Add trust audit status check
Document dashboard setup
Fix L2 promotion preview wording
```

Keep generated files, private logs, local DBs, and unrelated formatting churn out
of commits.
