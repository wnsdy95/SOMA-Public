# Contributing

Thanks for taking SOMA seriously. This project is both software and a research
artifact, so contributions should preserve the core invariant:

> Learning means a state transition with evidence. Cloud output alone is never
> durable memory.

## Development Setup

```bash
cargo build -p soma
cargo test -p soma --lib
cargo fmt -- --check
```

Optional dashboard check:

```bash
cargo build -p soma --features dashboard
```

## Pull Request Rules

Before opening a PR:

- keep the change focused
- update README/docs when public behavior changes
- add or adjust tests for memory lifecycle, trust, projection, or client changes
- run the validation commands in the PR template
- do not include local `.soma` databases, private dogfood logs, credentials, or
  private planning documents

PRs that affect trust, learning, ContextEnvelope, TaskFrame, or client proof
must explain the evidence rule they preserve or change.

## Commit Messages

SOMA uses a Conventional Commit style with a small project-specific type set:

```text
<type>(<optional-scope>): <short imperative summary>
```

Use `feat`, not `feature`, so release tooling and outside contributors see the
standard open-source convention immediately.

Common examples:

```text
feat(memory): promote verified L2 candidates
fix(context): block unverified cloud claims
docs(readme): document dashboard startup
security(trust): require tool verification before L3 promotion
```

Allowed types are `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `ci`,
`build`, `chore`, `security`, `release`, and `revert`.

Keep the subject imperative, concise, and without a final period. Add a body
when the change is non-trivial. For memory, trust, ContextEnvelope, TaskFrame,
or client-proof changes, the body should explain the evidence rule and the
validation path.

See [Commit message guide](docs/COMMIT_MESSAGE_GUIDE.md). To install the local
template, run `git config commit.template .gitmessage`.

## Coding Standards

- Rust code is formatted with `rustfmt`.
- Use existing module patterns before adding new abstractions.
- Deterministic baselines should remain available even when optional cognitive
  modules are enabled.
- Avoid silent mutation. Prefer explicit review, verification, and audit paths.

## Documentation Standards

Public documentation should be useful to an outside reader:

- explain intent, not just commands
- distinguish implemented behavior from planned work
- cite evidence and lifecycle boundaries when describing learning
- keep private operational history out of the public repo

## License

Code contributions are accepted under Apache-2.0. Documentation contributions
are accepted under CC BY 4.0 unless otherwise noted.
