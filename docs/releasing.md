# Releasing

Review the release's `## vX.Y.Z` section in `CHANGELOG.md` and run the
[contribution checks](../CONTRIBUTING.md#before-opening-a-pull-request) before
pushing its tag. A tag beginning with `v` triggers
[the release workflow](../.github/workflows/release.yml), which builds the
platform packages and publishes a GitHub release.

The release body uses the matching changelog heading and its text through the
next level-two heading. Subheadings, including **Sound changes**, stay in the
body. When the tag has no matching section, GitHub generates the release notes
from the repository history. Check extraction locally with:

```sh
bash .github/scripts/release-notes.sh v0.1.0
bash .github/scripts/tests/release-notes.sh
```

Each platform archive contains `rustel` (`rustel.exe` on Windows), `LICENSE`,
`NOTICE.md`, and the third-party notices `crates/*/LICENSE-*` that `NOTICE.md`
cites. The workflow verifies these members before uploading the
archive, then publishes SHA-256 sidecars for the downloads. `rustelup` installs
the binary (`rustelup` on Unix, `rustelup.ps1` on Windows); the license and
third-party notices remain available in the published release archive. Serve
`rustelup/install` and `rustelup/install.ps1` as
`https://rustel.cc/install` and `https://rustel.cc/install.ps1`.

The release workflow does not publish these two bootstrap files. Before you
announce a release, make sure that each URL returns the same bytes as the file
in this repository:

```sh
curl -fsSL https://rustel.cc/install | cmp - rustelup/install
curl -fsSL https://rustel.cc/install.ps1 | cmp - rustelup/install.ps1
```

Each command prints nothing when the two files are equal.

Before the public release, a repository administrator must enable **Private
vulnerability reporting** in the public repository's security settings. Verify
that the **Security** tab offers **Report a vulnerability**, following
[GitHub's enablement guide](https://docs.github.com/en/code-security/security-advisories/working-with-repository-security-advisories/configuring-private-vulnerability-reporting-for-a-repository).
A private repository cannot provide this reporting route. Treat unavailable
private reporting as a release blocker, and do not ask reporters to publish
vulnerability details in issues.

---

## Preparing a release locally

`cargo xtask release <patch|minor|major|X.Y.Z>` prepares a release **locally**.
It changes the workspace version and local package entries in `Cargo.lock`,
turns the current `Unreleased` changelog prose into a versioned section, adds
a draft of first-parent commit subjects, then creates a local commit and
annotated `vX.Y.Z` tag. It never pushes or uploads anything.

## Before preparing

1. Check out the intended release branch and fetch current tags:
   `git fetch origin --tags`. Confirm the branch includes the intended changes
   and is clean with `git status --short`.
2. Complete the test and review gates for the release: verify CI on the
   intended commit, run the relevant workspace tests, check platform builds,
   inspect the installer and update path, and review user-facing changes in
   `CHANGELOG.md`. The command validates the manifest with Cargo but does not
   replace these gates.
3. Run `cargo test -p xtask` after changing release tooling.

`patch`, `minor`, and `major` increase the current workspace version. An
explicit `X.Y.Z` must be newer than the current version and the latest local
release tag. The first release may use the current workspace version if no
release tags exist; for example, `cargo xtask release 0.1.0`. Pre-release and
build suffixes and leading zeroes are not accepted. The command requires a
clean worktree and a branch, and refuses an existing tag or inconsistent
local version history.

## Preview and prepare

```sh
cargo xtask release 0.1.0 --dry-run
cargo xtask release 0.1.0
```

`--dry-run` prints the exact proposed line changes without editing repository
files, making a commit, or creating a tag. The regular command stages only
`Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`. It uses the highest earlier
`vX.Y.Z` tag as the changelog baseline and requires that tag to be an ancestor
of the branch. If there is
no earlier tag, the draft covers first-parent commits from repository start.
Keep the existing changelog prose. The release workflow publishes the whole
`## vX.Y.Z` section as the release body, and the commit draft is part of that
section. The draft holds unedited commit subjects, including merge subjects
that name the source account and branch. Rewrite the draft for readers, or
delete it, before you push the tag. This review may require amending the
local release commit and recreating its **local, unpublished** tag. Check the
final commit and tag point to the same reviewed content:

```sh
git show --stat --oneline HEAD
git show v0.1.0 --no-patch
git diff v0.1.0 HEAD
```

If you amend the release commit, delete only its unpublished local tag with
`git tag -d v0.1.0`, amend, then recreate it with
`git tag -a v0.1.0 -m 'Release v0.1.0'`. Never rewrite a tag already pushed.

## Publish explicitly

A push publishes every commit that the branch or tag reaches. Each commit
carries an author identity, a committer identity and a message. The release
tool does not set an identity, so the release commit and the annotated tag
carry the name and e-mail address that Git is configured with. Confirm that
`origin` is the repository you intend to publish to. Then read the identities
and messages of the commits that `origin` does not have:

```sh
git remote get-url origin
git log --format=fuller HEAD --not --remotes=origin
```

Push only if you intend to publish every name, address and message shown.

After review and passing gates, verify the target tag does not exist on the
remote, then push the release commit and tag explicitly:

```sh
git ls-remote --tags origin 'refs/tags/v0.1.0'
git push origin HEAD
git push origin v0.1.0
```

Pushing the tag triggers `.github/workflows/release.yml`, which builds
platform archives and uploads them to the GitHub release. Confirm every build
and uploaded artifact before announcing the release. The workflow does not
run from `cargo xtask release` itself.

## Recover from a failed preparation

The command reports the failed step and leaves its local files, commit, or tag
in place. It never discards work or force-rewrites a tag. Inspect
`git status`, `git log -1`, and `git tag --list 'v*'` before retrying.

- If validation or commit creation failed, fix the reported problem, then
  review the three release files and commit them manually, or intentionally
  restore them with `git restore --staged --worktree -- Cargo.toml Cargo.lock CHANGELOG.md`
  before rerunning. The command's clean-worktree gate prevents an automatic
  retry while partial edits remain.
- If the commit succeeded but tag creation failed, resolve the tag conflict
  and create the local annotated tag on the reviewed commit. Do not rerun the
  command against the new release commit.
- If the remote tag already exists, stop and reconcile it with the local tag;
  never force-push a release tag.
