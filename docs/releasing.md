# Releasing

A release is one pull request, then one tag. The tag starts
[the release workflow](../.github/workflows/release.yml). The workflow builds
the platform packages, signs their build provenance, and publishes a draft
only after it verifies every uploaded asset. Complete [owner setup](#owner-setup)
before the first release with this workflow.

The examples use version 0.1.7. Put your version in its place.

## 1. Check `main`

```sh
git switch main
git pull --prune
git fetch origin --tags
git status --short
gh run list --branch main --limit 3
gh api 'repos/tzfm/rustel/dependabot/alerts?state=open' --jq length
gh api 'repos/tzfm/rustel/code-scanning/alerts?state=open' --jq length
```

`git status` prints nothing. Each run shows `success`. Each alert count is `0`.

## 2. Check the installers

rustel.cc serves `rustelup/install` and `rustelup/install.ps1`. The release
workflow includes these reviewed files as signed GitHub assets. It does not
deploy the website. Deploy them from the reviewed commit with LF line endings.

```sh
bash .github/scripts/check-installers.sh
```

The command prints nothing when both files are equal. A mismatch or a failed
download blocks the release. Check the cause before changing either copy.
The Installer integrity workflow runs this same check daily and on manual
dispatch. Enable Actions failure notifications for its owner.

## 3. Make the release commit

```sh
git switch -c fix/prepare-v0.1.7
cargo xtask release 0.1.7 --dry-run
cargo xtask release 0.1.7
git tag -d v0.1.7
```

`--dry-run` prints the changes and writes nothing. The command accepts
`patch`, `minor`, `major` or `X.Y.Z`. The command sets the workspace version,
moves the `Unreleased` notes under `## v0.1.7`, and makes one local commit and
one local tag. Delete the tag. The commit to tag exists only after the merge.

## 4. Edit the release notes

The `## v0.1.7` section of `CHANGELOG.md` is the text of the GitHub release.

- Delete the `Commit draft` list, or rewrite its lines for readers.
- Put changes to the sound under `### Sound changes`.
- Write each link as a full URL. A relative link breaks on the release page.

```sh
bash .github/scripts/release-notes.sh v0.1.7
git commit --all --amend --no-edit
```

The first command prints the release text.

## 5. Open the pull request

```sh
git log --format=fuller -1
git push -u origin fix/prepare-v0.1.7
gh pr create --title "Prepare the v0.1.7 release" --body "Version 0.1.7 and its release notes."
```

The name and the e-mail address in the first output become public. `main`
accepts changes only through a pull request, so a push to `main` fails. Merge
the pull request with a squash merge when the checks pass. The release commit changes `Cargo.toml` and
`Cargo.lock`, so the full check matrix runs. The markdown-only skip does not
apply.

## 6. Tag the merged commit

```sh
git switch main
git pull --prune
git log --oneline -1
git ls-remote --tags origin refs/tags/v0.1.7
git tag -a v0.1.7 -m 'Release v0.1.7'
git push origin v0.1.7
```

`git log` shows the release pull request as the last commit. `git ls-remote`
prints nothing. The repository restricts new tag refs. Only accounts with
tag-creation bypass can push a release tag.
Never move or delete a tag after the push. To correct a
release, make a new patch release.

Wait for the `ci.yml` push run on this exact `main` commit to succeed before
pushing the tag. The release gate requires a `vX.Y.Z` tag that matches the
workspace version, still points to the workflow commit, and is reachable
from `main`. It requires a successful CI push run for that SHA, Miri
included. A later push to `main` cancels a push run that is still going, so
rerun a cancelled run for the release commit. An ancestor check proves main
membership, not human approval. The ruleset and publishing environment
provide approval when configured below.

Review the run's tag, SHA, source changes and build results in the `release`
environment approval screen. The publishing job checks the tag and CI again
after approval and immediately before publication. A tag push uses the
workflow stored at that tag, so tag protections and review of workflow
changes remain essential.

## 7. Check the release

```sh
gh run list --workflow release.yml --limit 1
gh release view v0.1.7
gh api repos/tzfm/rustel/releases/tags/v0.1.7 --jq .immutable
```

The run shows `success`. The release holds one archive for each platform and
one `.sha256` file for each archive and installer. Each archive holds `rustel`
(`rustel.exe` on Windows), `LICENSE`, `NOTICE.md` and the `crates/*/LICENSE-*` notices.
`rustelup` installs the binary from these archives.

The release also includes one `.sigstore.json` bundle per archive,
`installers.sigstore.json`, and the four files from `rustelup/`. See
[independent verification](../rustelup/README.md#verify-release-provenance).
The immutable field must be `true` after owner setup.

## What the workflow verifies

Each build signs its archive on the GitHub-hosted runner that built it.
The installer job signs the four scripts from the same commit. Both use
[`actions/attest` v4](https://github.com/actions/attest/tree/1e69f48acb82d1966a394da916b4c1698aa569d6),
pinned to the verified upstream commit. These jobs have `contents: read`,
`id-token: write` and `attestations: write`. Storage records are disabled,
so no `artifact-metadata: write` permission is needed. No private signing
key is stored. Every external action in release, ordinary CI and heavy CI
is SHA-pinned. Local reusable workflows use the same trusted revision as
their caller. Review and update the pins when upstream fixes are needed.

Only the publishing job has `contents: write`. It has no signing permission
and compiles or executes no downloaded release binary. It verifies the exact
asset inventory, checksums and signatures with GitHub CLI. It also compares
the four installer assets with the `rustelup/` files of the release commit,
because a build job can sign files too. Verification pins
the repository, release workflow, source tag, source commit and signer commit,
and rejects self-hosted runner identities. There is no continue-on-error path.

Publication follows this order:

```text
main + successful CI -> build + attest -> environment approval
  -> verify files -> upload draft -> download + verify files
  -> recheck tag + CI -> publish
```

The bundles are untrusted inputs until the CLI verifies them. Checksums remain
for corruption detection and existing installer compatibility. This workflow
requires provenance for every new release it publishes. It does not add
automatic signature verification to the Bash or PowerShell updater.

## When a step fails

- `cargo xtask release` stops with an error. The command keeps its files, its
  commit and its tag. Read `git status`, `git log -1` and `git tag --list 'v*'`.
  To start again, restore the three files:
  `git restore --staged --worktree -- Cargo.toml Cargo.lock CHANGELOG.md`
- The tag is on `origin` already. Stop. Do not force the push.
- The tag has no matching changelog section. GitHub then writes the release
  notes from the repository history.
- The CI push run has not succeeded. Finish or fix CI, then rerun the release
  workflow for the same unmoved tag. Do not bypass the check.
- An attestation, checksum or identity check fails. Stop and inspect the
  build and asset. Do not publish it by hand or remove verification.
- Draft upload or downloaded-asset verification fails. The draft can remain
  for inspection. The workflow stops when the tag already has a release or a
  draft, so it never updates an existing one. After confirming that the draft
  is unpublished, the owner can delete it without deleting its tag, then
  rerun. Never alter a published immutable release.

## Owner setup

These are manual settings. A repository change does not apply them.

### Immutable releases

In `tzfm/rustel`, open Settings, General, then the Releases section. Select
Enable release immutability. This applies to future releases only. It does
not change v0.1.6. GitHub locks the release assets and associated tag when
the draft is published. Upload and verify every asset before that point.
See [GitHub's exact setting](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes).

For organization enforcement, open organization Settings, Repository,
General, Releases. Change No policy to Selected repositories and select
`rustel`, or select All repositories if that is the owner's intended scope.

### Publishing environment

In repository Settings, Environments, create `release` before enabling the
workflow. Select deployment branches and tags, add a Tag rule `v*`, and add
no branch rules. Set a required reviewer. Use an independent maintainer,
enable Prevent self-review, and disable administrator bypass of protection
rules when an independent reviewer is available. The reviewer must inspect
the exact SHA and workflow changes before approval.

With one maintainer, self-review prevention would block all releases. A
required self-approval with that option off adds a deliberate pause but does
not stop an attacker who controls the same account. Protect the account with
a hardware security key. Independent review is the smallest extra trust boundary
when a second maintainer is available. Do not use a second account owned by
the same person as a claim of independent review.

After configuration, open Settings, Secrets and variables, Actions,
Variables. Add repository variable `RELEASE_PUBLISH_ENABLED` with value
`true`. An unset or different value stops the workflow before signing.
This switch records owner readiness. It does not prove environment rules
exist. GitHub can create a named environment without protection rules, so
the name alone is not an approval control. See
[environment protection](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments).

### Branch and tag rules

Set these rulesets:

| Ruleset | Settings |
| --- | --- |
| `main` | Require a pull request. Require the `CI result` check from GitHub Actions and an up-to-date branch. Block force pushes and deletion. Add no bypass actors. |
| `main`, with a second maintainer | Also require one approval, dismiss stale approvals, require approval of the most recent push and require resolved review threads. |
| `v*` tag creation | Restrict creation. Put only the role or team that makes releases on the bypass list. |
| `v*` tag changes | Restrict updates and deletion. Add no bypass actors. |

A solo maintainer cannot approve their own PR, so `main` needs zero
approvals until a second maintainer joins. Until then, a stolen owner
account can merge a change that passes CI. When a second maintainer joins,
require review of release workflows, installer files and deployment
configuration.

The two tag rulesets let the owner create a tag without also bypassing the
update and deletion rules. Immutable releases add protection after publication.
An administrator can still edit repository rules. Limit administrator access
and monitor rule changes. A workflow cannot prevent a write-capable account
from creating a release outside that workflow. Limit write access too.

### rustel.cc deployment checklist

- Use a hardware security key or a passkey for GitHub, the hosting account,
  DNS provider and domain registrar wherever supported.
  Keep recovery codes offline. Check recovery email and fallback methods.
- Use a dedicated site deployment identity. Prefer short-lived OIDC access
  where the host supports it. Otherwise scope its token to this one site
  and rotate it. Give it no DNS, registrar or repository administration access.
- Deploy only from reviewed `main` commits. Copy `rustelup/install` and
  `rustelup/install.ps1` directly to their current URLs without editing or
  line-ending conversion. Record the source SHA in the host's deployment log.
  Keep preview deployments and untrusted PR jobs away from production credentials.
- Limit production deployment permissions and protect the host's deployment
  environment. Remove unused tokens, SSH keys, integrations and collaborators.
  Keep any manual emergency deployment path under the same account controls.
- Enable registrar transfer lock, renewal alerts and DNS-change alerts.
  Separate DNS editing rights from site deployment rights. Use DNSSEC if the
  registrar and DNS provider support it. It does not protect a compromised host.
- Run `bash .github/scripts/check-installers.sh` after each deployment. Keep
  the daily Installer integrity workflow enabled, subscribe to its failures,
  and inspect hosting, GitHub, DNS and registrar audit alerts.
- On unexplained drift, pause deployment, save the affected files and logs,
  revoke the affected credentials and restore files from a reviewed commit.
  Check releases and recent access before resuming installation guidance.

The site is outside this repository's deployment workflow. This checklist
and the drift check do not enforce host permissions. A daily check can miss
short-lived or selectively served malicious content. A compromised website
can execute arbitrary code through `curl | bash` or `irm | iex` before any
release verification. Attestations cannot repair that bootstrap trust gap.

### Vulnerability reports

The Security tab of the repository offers "Report a vulnerability". When the
button is missing, enable private vulnerability reporting with
[GitHub's guide](https://docs.github.com/en/code-security/security-advisories/working-with-repository-security-advisories/configuring-private-vulnerability-reporting-for-a-repository)
before a release.
