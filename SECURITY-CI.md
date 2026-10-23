# Security CI

chain-viz runs three security workflows beside `ci.yml` (whose `audit` job already gates Rust
advisories, licences and sources with cargo-deny and cargo-audit on every push and PR). Scanner results
go to the run's step summary and, as SARIF, to *Security → Code scanning*. Only Semgrep and Dependabot
open PRs, and only a bounded number of them.

| Leg | Workflow / job | Runs on | Gate | Results |
|---|---|---|---|---|
| Semgrep (Rust, JavaScript, HTML, shell, workflows, secrets; Trail of Bits `rs`, `javascript`, `generic`, `yaml/github-actions`; local rules) | `semgrep.yml` | PR (diff-aware), push, nightly 02:17 UTC | PR fails on a new **critical** finding | code scanning `semgrep`; at most four rolling PRs `semgrep/<bucket>` |
| CodeQL (`rust`, `javascript-typescript`, `actions`; `security-extended`, build-mode none) | `codeql.yml` | PR, push to `upgrade/vault`, weekly | report only | code scanning `/language:<lang>` |
| zizmor (every workflow and `dependabot.yml`) | `secops.yml` `zizmor` | PR, push to `upgrade/vault`, nightly 03:43 UTC | fails on any finding | code scanning `zizmor` |
| gitleaks | `secops.yml` `gitleaks` | PR (its commits), push / nightly / dispatch (full history) | fails on a leak | code scanning `gitleaks` |
| OSV-Scanner (`Cargo.lock`, `contrib/semgrep/requirements.txt`, `contrib/secops/zizmor-requirements.txt`) | `secops.yml` `osv-scanner` | PR, push to `upgrade/vault`, nightly | fails on a known vulnerability | code scanning `osv-scanner` |
| cargo-deny + cargo-audit | `ci.yml` `audit` (existing) | every push and PR | fails on an advisory, a disallowed licence or source | job log |
| Dependabot | `.github/dependabot.yml` | GitHub | security updates only, one grouped PR per ecosystem (cargo, github-actions, pip) | PR |

There is no npm leg: the UI is plain ES modules with no dependencies and no lockfile, and `qa/` uses
`node:` built-ins only. Add `npm audit --omit=dev --audit-level=high` to `secops.yml` with the first
`package-lock.json`.

## The read-only contract, as rules

chain-viz is a strictly read-only sidecar. Two local Semgrep rule files (`contrib/semgrep/rules/`,
tests beside them: `semgrep --test contrib/semgrep/rules`) back the existing `tests/readonly_gate.rs`:

- `chainviz.readonly.writer-rpc` (critical): a Rust string literal in `src/` naming a node RPC that
  writes — `sendrawtransaction`, `submitblock`, `generate*`, wallet senders and key RPCs, the set/vault
  act and spend RPCs, and **any `yed_*` that is not a `get`/`list`/`decode`/`validate` reader**, so a
  new mutator is caught before the gate's list is updated.
- `chainviz.ui.html-sink` (high): `innerHTML` / `outerHTML` / `insertAdjacentHTML` / `document.write` /
  `createContextualFragment` / `DOMParser` fed anything but a string literal in `ui/` or `qa/`. The UI
  renders node-supplied strings with `textContent` and `ui/lib.js` `h()` only.
- `chainviz.ui.code-from-string` (medium): `eval`, `new Function`, string `setTimeout`/`setInterval`.

## Accepting a finding

Every acceptance carries a reason a reviewer can check:

- **Semgrep**: `contrib/semgrep/accepted.json` (by fingerprint, or rule over paths), or `exclude_rules` /
  `audit_rules` in `contrib/semgrep/config.json`. See `contrib/semgrep/README.md`.
- **gitleaks**: add the finding's fingerprint to `.gitleaksignore` with a comment saying why the value is
  public (test vector, public key, RFC sample). A real secret is rotated, never ignored.
- **OSV-Scanner**: an `[[IgnoredVulns]]` entry in `osv-scanner.toml` with `reason` and an `ignoreUntil`
  at most 90 days out. Cargo advisories are fixed by a lockfile bump, not ignored.
- **zizmor**: fix the workflow; a deliberate exception goes in `.github/zizmor.yml` with a comment.
- **CodeQL**: dismiss the alert in code scanning with a reason.

## Repository settings

- *Settings → Actions → General → Allow GitHub Actions to create and approve pull requests* (the Semgrep
  bucket PRs), and optionally the secret `SEMGREP_PR_TOKEN` so CI runs on them (`contrib/semgrep/README.md`).
- Code scanning must be available (free for public repositories; GitHub Advanced Security for private).
- *Settings → Code security*: enable Dependabot alerts and Dependabot security updates.
