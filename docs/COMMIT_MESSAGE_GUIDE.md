# Commit Message Guide

SOMA uses a Conventional Commit style because it is readable for humans,
friendly to release tooling, and familiar to contributors who have worked with
modern open-source projects.

## Format

```text
<type>(<optional-scope>): <short imperative summary>
```

Examples:

```text
feat(memory): promote verified L2 candidates
fix(context): block unverified cloud claims
docs(readme): document dashboard startup
security(trust): require tool verification before L3 promotion
```

Use `feat`, not `feature`. `feat` is the Conventional Commits spelling and is
the form used by many release tools.

## Types

| Type | Use for |
| --- | --- |
| `feat` | New user-visible capability |
| `fix` | Bug fix |
| `docs` | Documentation-only change |
| `test` | Tests-only change |
| `refactor` | Behavior-preserving code change |
| `perf` | Performance improvement |
| `ci` | GitHub Actions or release automation |
| `build` | Dependencies, packaging, or build system |
| `chore` | Maintenance that does not affect runtime behavior |
| `security` | Trust, privacy, or security hardening |
| `release` | Release or version metadata |
| `revert` | Revert a prior change |

## Subject Rules

- Write the subject in imperative mood: `add`, `fix`, `document`, `require`.
- Keep it concise. Aim for 50 characters and stay under 72 when practical.
- Do not end the subject with a period.
- Prefer a concrete scope when it helps reviewers: `memory`, `context`,
  `trust`, `cli`, `mcp`, `dashboard`, `docs`, `ci`, or `release`.

Good:

```text
fix(context): block unverified cloud claims
docs(readme): document dashboard startup
```

Avoid:

```text
fixed bug
feature: new memory thing
update stuff.
```

## Body Rules

Add a body when the change is non-trivial, changes behavior, changes public
documentation, or touches high-risk SOMA semantics.

The body should explain:

- what changed
- why the change exists
- how it was validated
- whether trust, memory lifecycle, ContextEnvelope, TaskFrame, or client proof
  semantics changed

For memory, trust, ContextEnvelope, TaskFrame, or client-proof changes, include
the evidence rule that the change preserves or modifies.

Example:

```text
fix(trust): keep cloud output as draft claims

Cloud-generated claims now remain below the L3/L4 promotion boundary until a
tool result, user confirmation, local observation, or correction verifies them.

Validated with cargo test -p soma --lib.
```

## Breaking Changes

Use `!` after the type or scope, and include a `BREAKING CHANGE:` footer.

```text
feat(context)!: rename envelope evidence fields

BREAKING CHANGE: MCP clients must read `relevant_memory` instead of the old
experimental `recent_evidence` field.
```

## Local Template

Install the repository commit template:

```bash
git config commit.template .gitmessage
```

After that, `git commit` opens with SOMA's expected commit format and examples.

## Influences

This policy follows the shape of widely used open-source guidance:

- [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/)
- [Angular commit message guidelines](https://github.com/angular/angular/blob/main/CONTRIBUTING.md#commit-message-format)
- [Kubernetes commit message guidance](https://github.com/kubernetes/community/blob/main/contributors/guide/pull-requests.md#commit-message-guidelines)
- [Git project patch submission guidance](https://github.com/git/git/blob/master/Documentation/SubmittingPatches)
