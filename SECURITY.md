# Security Policy

soothfast is a Rust workspace: a library facade (`soothfast`), proc-macros
(`soothfast-macros`), a registry (`soothfast-registry`), measurement/docs/spec/
report engines, and a CLI (`cargo-soothfast`). This policy covers all of those
components.

## Supported Versions

Security fixes land on the latest released line only. If you are on an older
version, the fix is to upgrade.

| Component                        | Version         | Supported          |
| -------------------------------- | --------------- | ------------------ |
| All published crates             | latest release  | :white_check_mark: |
| All published crates             | older releases  | :x:                |
| `master` branch                  | latest          | :white_check_mark: |

## Reporting a Vulnerability

**Please do not report security issues through public GitHub issues,
discussions, or pull requests.**

Report privately through GitHub's built-in flow (preferred):

1. Go to the [**Security** tab](https://github.com/Verdenroz/soothfast/security/advisories)
   → **Report a vulnerability**, or open
   <https://github.com/Verdenroz/soothfast/security/advisories/new> directly.
2. Describe the issue, affected crate/version, and impact.

If you cannot use GitHub Private Vulnerability Reporting, email
**harveytseng2@gmail.com** with `SECURITY` in the subject.

Please include, where possible:

- The affected crate and version.
- A description of the vulnerability and its impact.
- Steps to reproduce or a proof of concept.
- Any suggested remediation.

## What to Expect

This is a small, volunteer-maintained project, so timelines are best-effort:

- **Acknowledgement** within 3 business days.
- **Initial assessment** (accepted / needs-info / declined, with reasoning)
  within 7 days.
- For accepted reports: we coordinate a fix and a patched release, publish a
  GitHub Security Advisory, and request a CVE through GitHub where warranted.
- We credit reporters in the advisory unless you ask to remain anonymous.
- We ask for coordinated disclosure — please give us a reasonable window
  (target: 90 days) before any public disclosure.

## Scope

In scope:

- All published crates in this workspace and their proc-macro code paths.
- The `cargo-soothfast` CLI, including code it executes on behalf of users
  (bench runners, git worktree operations, generated doc tests).
- The build/release supply chain (CI workflows, crates.io publishing).

Out of scope:

- The `demo`, `demo-server`, and `spikes/*` crates (never published;
  dogfooding fixtures only).
- Issues requiring a pre-compromised host, malicious local environment, or
  physical access.
- Resource exhaustion caused by benchmarking untrusted code — running
  `cargo soothfast` on a repository implies trusting that repository's build
  scripts and benches, exactly as `cargo test` does.
- Reports generated solely by automated scanners without a demonstrated,
  exploitable impact.

Thank you for helping keep soothfast and its users safe.
