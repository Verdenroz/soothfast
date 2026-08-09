## What changed

<!-- Summarize the change. Link any related issue with "Closes #123". -->

## Why

<!-- The problem this solves or the motivation behind it. -->

## How was this tested

<!-- `make check` and `make gate` output, new/updated tests, manual verification steps. -->

## Checklist

- [ ] `make check` passes (fmt, clippy `-D warnings`, `cargo test --workspace`)
- [ ] `make gate BASE=master` passes, or any intentional cost change is explained above
- [ ] Tests added/updated for new behavior (not required for docs/CI-only changes)
- [ ] Public API items have `///` doc comments
- [ ] No new dependency, or its justification is included above (see [Dependency Policy](../CONTRIBUTING.md#dependency-policy))
- [ ] Docs (`README.md`, `docs/`, `soothfast:bind`/`soothfast:claim` markers) updated if behavior changed
