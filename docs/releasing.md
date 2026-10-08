# Releasing

A release is one pull request, then one tag. The tag starts
[the release workflow](../.github/workflows/release.yml). The workflow builds
the platform packages and publishes the GitHub release.

The examples use version 0.1.2. Put your version in its place.

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
workflow does not publish these two files.

```sh
curl -fsSL https://rustel.cc/install | cmp - rustelup/install
curl -fsSL https://rustel.cc/install.ps1 | cmp - rustelup/install.ps1
```

Each command prints nothing when the files are equal. When one differs, fix
the repository file in the release pull request, or fix the site.

## 3. Make the release commit

```sh
git switch -c prepare-v0.1.2
cargo xtask release 0.1.2 --dry-run
cargo xtask release 0.1.2
git tag -d v0.1.2
```

`--dry-run` prints the changes and writes nothing. The command accepts
`patch`, `minor`, `major` or `X.Y.Z`. The command sets the workspace version,
moves the `Unreleased` notes under `## v0.1.2`, and makes one local commit and
one local tag. Delete the tag. The commit to tag exists only after the merge.

## 4. Edit the release notes

The `## v0.1.2` section of `CHANGELOG.md` is the text of the GitHub release.

- Delete the `Commit draft` list, or rewrite its lines for readers.
- Put changes to the sound under `### Sound changes`.
- Write each link as a full URL. A relative link breaks on the release page.

```sh
bash .github/scripts/release-notes.sh v0.1.2
git commit --all --amend --no-edit
```

The first command prints the release text.

## 5. Open the pull request

```sh
git log --format=fuller -1
git push -u origin prepare-v0.1.2
gh pr create --title "Prepare the v0.1.2 release" --body "Version 0.1.2 and its changelog section."
```

The name and the e-mail address in the first output become public. `main`
accepts changes only through a pull request, so a push to `main` fails. Merge
the pull request when the checks pass.

## 6. Tag the merged commit

```sh
git switch main
git pull --prune
git log --oneline -1
git ls-remote --tags origin refs/tags/v0.1.2
git tag -a v0.1.2 -m 'Release v0.1.2'
git push origin v0.1.2
```

`git log` shows the release pull request as the last commit. `git ls-remote`
prints nothing. Never move or delete a tag after the push. To correct a
release, make a new patch release.

## 7. Check the release

```sh
gh run list --workflow release.yml --limit 1
gh release view v0.1.2
```

The run shows `success`. The release holds one archive for each platform and
one `.sha256` file for each download. Each archive holds `rustel` (`rustel.exe`
on Windows), `LICENSE`, `NOTICE.md` and the `crates/*/LICENSE-*` notices.
`rustelup` installs the binary from these archives.

## When a step fails

- `cargo xtask release` stops with an error. The command keeps its files, its
  commit and its tag. Read `git status`, `git log -1` and `git tag --list 'v*'`.
  To start again, restore the three files:
  `git restore --staged --worktree -- Cargo.toml Cargo.lock CHANGELOG.md`
- The tag is on `origin` already. Stop. Do not force the push.
- The tag has no matching changelog section. GitHub then writes the release
  notes from the repository history.

## One-time setup

The Security tab of the repository offers "Report a vulnerability". When the
button is missing, enable private vulnerability reporting with
[GitHub's guide](https://docs.github.com/en/code-security/security-advisories/working-with-repository-security-advisories/configuring-private-vulnerability-reporting-for-a-repository)
before a release.
