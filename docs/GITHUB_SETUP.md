# GitHub Setup

This file records the intended GitHub repository settings for
`wnsdy95/SOMA-Public`.

Some settings can be applied through `gh` after authentication and repository
permissions are available.

## Repository Metadata

```bash
gh repo edit wnsdy95/SOMA-Public \
  --description "Local memory, context, and trust-boundary layer for cloud LLM coding agents" \
  --homepage "https://github.com/wnsdy95/SOMA-Public" \
  --enable-issues \
  --enable-wiki=false \
  --enable-projects=false
```

Recommended topics:

```bash
gh repo edit wnsdy95/SOMA-Public \
  --add-topic rust \
  --add-topic sqlite \
  --add-topic mcp \
  --add-topic llm \
  --add-topic ai-agents \
  --add-topic context-engineering \
  --add-topic memory
```

Recommended labels:

```bash
gh label create bug --color d73a4a --description "Something is not working" --force
gh label create enhancement --color a2eeef --description "New feature or improvement" --force
gh label create documentation --color 0075ca --description "Documentation change" --force
gh label create research --color 8e44ad --description "Research note, paper, or design hypothesis" --force
gh label create security --color b60205 --description "Security-sensitive work" --force
gh label create dependencies --color 0366d6 --description "Dependency update" --force
gh label create rust --color dea584 --description "Rust code or tooling" --force
gh label create github-actions --color 2088ff --description "GitHub Actions or CI" --force
gh label create needs-triage --color fbca04 --description "Needs maintainer triage" --force
gh label create good-first-issue --color 7057ff --description "Good first contribution" --force
```

## Branch Protection

After the initial `main` push and first CI run, protect `main`:

```bash
gh api --method PUT \
  -H "Accept: application/vnd.github+json" \
  /repos/wnsdy95/SOMA-Public/branches/main/protection \
  --input - <<'JSON'
{
  "required_status_checks": {
    "strict": true,
    "contexts": ["rust"]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "required_approving_review_count": 1,
    "require_last_push_approval": false
  },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "required_conversation_resolution": true
}
JSON
```

If GitHub reports that the required check context name differs, inspect the
completed workflow check name and update `contexts`.

## Security Features

Public GitHub repositories can use Dependabot alerts and secret scanning. Enable
what the account/plan exposes:

```bash
gh api --method PUT /repos/wnsdy95/SOMA-Public/vulnerability-alerts
gh api --method PUT /repos/wnsdy95/SOMA-Public/automated-security-fixes

gh api --method PATCH \
  -H "Accept: application/vnd.github+json" \
  /repos/wnsdy95/SOMA-Public \
  --input - <<'JSON'
{
  "has_issues": true,
  "has_projects": false,
  "has_wiki": false,
  "security_and_analysis": {
    "secret_scanning": { "status": "enabled" },
    "secret_scanning_push_protection": { "status": "enabled" }
  }
}
JSON
```

Dependabot version updates are configured in `.github/dependabot.yml`.

If the account or repository plan rejects a `security_and_analysis` field, keep
Dependabot alerts enabled and verify secret scanning from the GitHub web UI.

## Initial Push

```bash
git remote add origin https://github.com/wnsdy95/SOMA-Public.git
git add .
git commit -m "Prepare SOMA public release"
git push -u origin main
```
