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
