## Summary

- 

## Why This Change Exists

- 

## Trust And Memory Boundary

- [ ] This change does not let cloud output become durable memory without user/tool/test/local verification.
- [ ] Any ContextEnvelope or TaskFrame projection remains evidence-backed and scoped.
- [ ] Privacy/redaction behavior is unchanged or explicitly tested.

## Validation

- [ ] `cargo fmt -- --check`
- [ ] `cargo test -p soma --lib`
- [ ] `cargo build -p soma --features dashboard`
- [ ] Other:

## Review Notes

- 
