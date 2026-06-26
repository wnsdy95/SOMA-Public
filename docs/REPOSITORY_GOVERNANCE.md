# Repository Governance

SOMA is published as a portfolio/research project. The repository should remain
easy to inspect, build, and cite.

## Maintainer Defaults

- primary branch: `main`
- code license: Apache-2.0
- documentation license: CC BY 4.0
- supported version: latest `main`
- public issue tracker: bugs, features, documentation, research notes
- private security channel: GitHub Security Advisories

## Public Repo Scope

Included:

- runtime source
- public README and research summary
- minimal compile-time eval snapshots
- adapter examples
- security/contribution policy

Excluded:

- private `.soma` databases
- API keys and `.env` files
- private dogfood logs
- internal planning histories
- throw-away spikes
- legacy prototypes not needed for public build

## Labels

Recommended labels:

- `bug`
- `enhancement`
- `documentation`
- `research`
- `security`
- `dependencies`
- `rust`
- `github-actions`
- `needs-triage`
- `good first issue`

## Release Notes

A release should summarize:

- user-facing commands or APIs changed
- ContextEnvelope or TaskFrame contract changes
- memory lifecycle or trust-boundary changes
- client integration changes
- validation performed
