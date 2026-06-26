# Security Policy

SOMA is a local memory and context layer for cloud LLM workflows. Security
reports are taken seriously because the project handles local work history,
ContextEnvelope projection, adapter capture, and trust-boundary decisions.

## Supported Versions

The public repository currently supports the latest `main` branch only.

## Reporting A Vulnerability

Please do not open a public issue for vulnerabilities.

Use GitHub Security Advisories:

https://github.com/wnsdy95/SOMA-Public/security/advisories/new

Include:

- affected command, MCP tool, or file path
- reproduction steps
- impact summary
- whether private local data, ContextEnvelope projection, trust promotion, or
  client binding proof is involved
- any proposed mitigation

## Security Boundaries

SOMA is local-first, but it is not a sandbox or a data-loss-prevention product.

Expected boundaries:

- local SQLite state remains local unless explicitly projected or exported
- cloud output is captured as `cloud_draft` and must not become durable L3/L4
  memory without user/tool/test/local verification
- TaskFrame cloud projection is separate from local full TaskFrame state
- secret-like projection should fail closed before cloud-facing release paths
- private editor/client proof requires observed evidence, not merely config files

## Out Of Scope

- vulnerabilities in third-party LLM providers
- prompts or model behavior not mediated by SOMA code
- local machine compromise outside SOMA
- intentionally running SOMA against untrusted local files or untrusted shell
  commands without operator review
