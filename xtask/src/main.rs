use std::collections::BTreeSet;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Version(u64, u64, u64);

impl Version {
    fn parse(text: &str) -> Result<Self> {
        let parts: Vec<_> = text.split('.').collect();
        if parts.len() != 3 {
            return Err(format!("invalid version '{text}': expected X.Y.Z"));
        }
        let mut numbers = [0; 3];
        for (index, part) in parts.into_iter().enumerate() {
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(format!(
                    "invalid version '{text}': use three nonnegative decimal numbers without leading zeroes"
                ));
            }
            numbers[index] = part
                .parse()
                .map_err(|_| format!("version component in '{text}' is too large"))?;
        }
        Ok(Self(numbers[0], numbers[1], numbers[2]))
    }

    fn requested(current: Self, request: &str) -> Result<Self> {
        match request {
            "patch" => Ok(Self(
                current.0,
                current.1,
                current.2.checked_add(1).ok_or("patch version overflow")?,
            )),
            "minor" => Ok(Self(
                current.0,
                current.1.checked_add(1).ok_or("minor version overflow")?,
                0,
            )),
            "major" => Ok(Self(
                current.0.checked_add(1).ok_or("major version overflow")?,
                0,
                0,
            )),
            _ => Self::parse(request),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug)]
struct Change {
    path: PathBuf,
    before: String,
    after: String,
}

#[derive(Debug)]
struct Plan {
    version: Version,
    previous_tag: Option<String>,
    changes: Vec<Change>,
}

fn main() {
    if let Err(error) = main_result() {
        eprintln!("release preparation failed: {error}");
        std::process::exit(1);
    }
}

fn main_result() -> Result<()> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("release") {
        return Err("usage: cargo xtask release <patch|minor|major|X.Y.Z> [--dry-run]".into());
    }
    let mut requested = None;
    let mut dry_run = false;
    for arg in args {
        if arg == "--dry-run" {
            dry_run = true;
        } else if requested.replace(arg).is_some() {
            return Err("provide exactly one release version".into());
        }
    }
    let requested = requested.ok_or("provide a release version: patch, minor, major, or X.Y.Z")?;
    let root = git(
        &env::current_dir().map_err(|e| e.to_string())?,
        &["rev-parse", "--show-toplevel"],
    )?;
    let root = Path::new(root.trim());
    let plan = build_plan(root, &requested)?;
    if dry_run {
        print_plan(&plan);
        println!("Dry run: no repository files, commit, or tag changed.");
    } else {
        apply_plan(root, &plan)?;
        println!(
            "Prepared v{} locally. Review the commit and tag, then push them explicitly.",
            plan.version
        );
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), detail.trim()));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("git output was not UTF-8: {e}"))
}

fn read(root: &Path, path: &str) -> Result<String> {
    fs::read_to_string(root.join(path)).map_err(|e| format!("cannot read {path}: {e}"))
}

fn build_plan(root: &Path, request: &str) -> Result<Plan> {
    // On a detached HEAD, `symbolic-ref --quiet` fails and prints nothing.
    git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|error| {
        if error.ends_with("failed: ") {
            "check out a branch before preparing a release".to_string()
        } else {
            error
        }
    })?;
    if !git(root, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err(
            "worktree is dirty; commit, stash, or remove local changes before preparing a release"
                .into(),
        );
    }

    let manifest = read(root, "Cargo.toml")?;
    let current = workspace_version(&manifest)?;
    let target = Version::requested(current, request)?;
    let target_tag = format!("v{target}");
    let tags = release_tags(root)?;
    if tags.iter().any(|(_, name)| name == &target_tag) {
        return Err(format!(
            "tag {target_tag} already exists; choose another version, or inspect the existing release"
        ));
    }
    let latest = tags.iter().max_by_key(|(version, _)| version);
    if let Some((version, name)) = latest {
        if *version > current {
            return Err(format!(
                "workspace version {current} is behind release tag {name}; reconcile release state first"
            ));
        }
        if target <= *version {
            return Err(format!(
                "target v{target} must be newer than existing release tag {name}"
            ));
        }
    }
    if target < current || (target == current && !tags.is_empty()) {
        return Err(format!(
            "target v{target} must be newer than workspace version {current}; only the first untagged release may use the current version"
        ));
    }

    let previous_tag = tags
        .iter()
        .filter(|(version, _)| *version < target)
        .max_by_key(|(version, _)| version)
        .map(|(_, name)| name.clone());
    if let Some(tag) = &previous_tag {
        git(root, &["merge-base", "--is-ancestor", tag, "HEAD"])
            .map_err(|_| format!("previous release tag {tag} is not an ancestor of HEAD; reconcile branch history before releasing"))?;
    }

    let members = workspace_packages(root, &manifest)?;
    let lock = read(root, "Cargo.lock")?;
    let changelog = read(root, "CHANGELOG.md")?;
    let commits = match &previous_tag {
        Some(tag) => git(
            root,
            &[
                "log",
                "--first-parent",
                "--format=%s",
                &format!("{tag}..HEAD"),
            ],
        )?,
        None => git(root, &["log", "--first-parent", "--format=%s", "HEAD"])?,
    };
    let after_manifest = change_workspace_version(&manifest, current, target)?;
    let after_lock = change_lockfile(&lock, &members, current, target)?;
    let after_changelog =
        draft_changelog(&changelog, &target_tag, previous_tag.as_deref(), &commits)?;
    Ok(Plan {
        version: target,
        previous_tag,
        changes: [
            ("Cargo.toml", manifest, after_manifest),
            ("Cargo.lock", lock, after_lock),
            ("CHANGELOG.md", changelog, after_changelog),
        ]
        .into_iter()
        .filter(|(_, before, after)| before != after)
        .map(|(path, before, after)| Change {
            path: path.into(),
            before,
            after,
        })
        .collect(),
    })
}

fn release_tags(root: &Path) -> Result<Vec<(Version, String)>> {
    let mut tags = Vec::new();
    for name in git(root, &["tag", "--list", "v*"])?.lines() {
        if let Some(version) = name
            .strip_prefix('v')
            .and_then(|text| Version::parse(text).ok())
        {
            tags.push((version, name.to_owned()));
        }
    }
    Ok(tags)
}

fn workspace_package_section(manifest: &str) -> Result<std::ops::Range<usize>> {
    let mut start = None;
    let mut offset = 0;
    for line in manifest.split_inclusive('\n') {
        let header = line.trim_end_matches(['\r', '\n']);
        if let Some(start) = start {
            if header.starts_with('[') {
                return Ok(start..offset);
            }
        } else if header == "[workspace.package]" {
            start = Some(offset + line.len());
        }
        offset += line.len();
    }
    start
        .map(|start| start..manifest.len())
        .ok_or_else(|| "Cargo.toml lacks [workspace.package]".into())
}

fn workspace_version(manifest: &str) -> Result<Version> {
    let section = &manifest[workspace_package_section(manifest)?];
    let versions: Vec<_> = section
        .lines()
        .filter_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
        })
        .collect();
    if versions.len() != 1 {
        return Err(
            "[workspace.package] must contain exactly one plain version = \"X.Y.Z\" line".into(),
        );
    }
    Version::parse(versions[0])
}

fn change_workspace_version(manifest: &str, current: Version, target: Version) -> Result<String> {
    let std::ops::Range { start, end } = workspace_package_section(manifest)?;
    let section = &manifest[start..end];
    let from = format!("version = \"{current}\"");
    if section.matches(&from).count() != 1 {
        return Err("workspace version line changed or is ambiguous; inspect Cargo.toml".into());
    }
    Ok(format!(
        "{}{}{}",
        &manifest[..start],
        section.replacen(&from, &format!("version = \"{target}\""), 1),
        &manifest[end..]
    ))
}

fn workspace_packages(root: &Path, manifest: &str) -> Result<BTreeSet<String>> {
    let section = manifest
        .split("members = [")
        .nth(1)
        .ok_or("Cargo.toml lacks workspace members")?;
    let section = section
        .split(']')
        .next()
        .ok_or("workspace members list is not closed")?;
    let mut names = BTreeSet::new();
    for line in section.lines() {
        let member = line.trim().trim_end_matches(',').trim_matches('"');
        if member.is_empty() {
            continue;
        }
        if !member.starts_with("crates/") && member != "xtask" {
            return Err(format!(
                "unsupported workspace member {member}; update xtask's workspace parser"
            ));
        }
        let path = root.join(member).join("Cargo.toml");
        let package = fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if member == "xtask" {
            continue;
        }
        let name = package
            .lines()
            .find_map(|line| {
                line.strip_prefix("name = \"")
                    .and_then(|value| value.strip_suffix('"'))
            })
            .ok_or_else(|| format!("{} lacks package name", path.display()))?;
        if !package
            .lines()
            .any(|line| line == "version.workspace = true")
        {
            return Err(format!(
                "{} does not inherit the workspace version",
                path.display()
            ));
        }
        if !names.insert(name.to_owned()) {
            return Err(format!("duplicate workspace package name {name}"));
        }
    }
    if names.is_empty() {
        return Err("workspace member list is empty".into());
    }
    Ok(names)
}

fn change_lockfile(
    lock: &str,
    members: &BTreeSet<String>,
    current: Version,
    target: Version,
) -> Result<String> {
    let mut lines: Vec<_> = lock.lines().map(str::to_owned).collect();
    let mut seen = BTreeSet::new();
    let mut start = 0;
    while start < lines.len() {
        if lines[start] != "[[package]]" {
            start += 1;
            continue;
        }
        let end = (start + 1..lines.len())
            .find(|&i| lines[i] == "[[package]]")
            .unwrap_or(lines.len());
        let name = lines[start + 1..end].iter().find_map(|line| {
            line.strip_prefix("name = \"")
                .and_then(|value| value.strip_suffix('"'))
        });
        if let Some(name) = name.filter(|name| members.contains(*name)) {
            let name = name.to_owned();
            if !seen.insert(name.clone()) {
                return Err(format!(
                    "Cargo.lock contains duplicate {name} package entries"
                ));
            }
            if lines[start + 1..end]
                .iter()
                .any(|line| line.starts_with("source = "))
            {
                return Err(format!(
                    "Cargo.lock package {name} is not local; inspect the lockfile"
                ));
            }
            let version = format!("version = \"{current}\"");
            let index = (start + 1..end)
                .find(|&i| lines[i] == version)
                .ok_or_else(|| {
                    format!("Cargo.lock package {name} does not match workspace version {current}")
                })?;
            lines[index] = format!("version = \"{target}\"");
        }
        start = end;
    }
    if &seen != members {
        return Err(format!(
            "Cargo.lock is missing workspace packages: {:?}; regenerate it before releasing",
            members.difference(&seen).collect::<Vec<_>>()
        ));
    }
    let ending = line_ending(lock);
    let mut result = lines.join(ending);
    if lock.ends_with('\n') {
        result.push_str(ending);
    }
    Ok(result)
}

fn line_ending(text: &str) -> &'static str {
    if text
        .split_once('\n')
        .is_some_and(|(line, _)| line.ends_with('\r'))
    {
        "\r\n"
    } else {
        "\n"
    }
}

fn draft_changelog(
    changelog: &str,
    target_tag: &str,
    previous_tag: Option<&str>,
    commits: &str,
) -> Result<String> {
    let ending = line_ending(changelog);
    let heading = format!("# Changelog{ending}{ending}## Unreleased{ending}");
    // An empty Unreleased section can end the file without a blank line.
    let rest = match changelog.strip_prefix(&heading) {
        Some("") => "",
        Some(rest) if rest.starts_with(ending) => &rest[ending.len()..],
        _ => {
            return Err(
                "CHANGELOG.md must start with '# Changelog' and an '## Unreleased' section".into(),
            );
        }
    };
    let prefix = format!("{heading}{ending}");
    if changelog
        .lines()
        .any(|line| line == format!("## {target_tag}"))
    {
        return Err(format!(
            "CHANGELOG.md already contains {target_tag}; reconcile the release draft"
        ));
    }
    let section_end = if rest.starts_with("## ") {
        0
    } else {
        rest.find(&format!("{ending}## "))
            .map(|i| i + ending.len())
            .unwrap_or(rest.len())
    };
    let unreleased = rest[..section_end].trim_end();
    let older = &rest[section_end..];
    let mut result = format!("{prefix}## {target_tag}{ending}{ending}");
    if !unreleased.is_empty() {
        result.push_str(unreleased);
        result.push_str(ending);
        result.push_str(ending);
    }
    let baseline = previous_tag.unwrap_or("repository start");
    result.push_str(&format!(
        "### Commit draft since {baseline} (review before publishing){ending}{ending}"
    ));
    let mut count = 0;
    for subject in commits.lines().filter(|line| !line.trim().is_empty()) {
        result.push_str(&format!("- {}{ending}", subject.trim()));
        count += 1;
    }
    if count == 0 {
        result.push_str(&format!(
            "- No commits since the previous release tag.{ending}"
        ));
    }
    if !older.is_empty() {
        result.push_str(ending);
        result.push_str(older);
    }
    Ok(result)
}

fn print_plan(plan: &Plan) {
    println!(
        "Release v{} (previous tag: {})",
        plan.version,
        plan.previous_tag.as_deref().unwrap_or("none")
    );
    for change in &plan.changes {
        println!(
            "--- a/{}\n+++ b/{}",
            change.path.display(),
            change.path.display()
        );
        let before: Vec<_> = change.before.lines().collect();
        let after: Vec<_> = change.after.lines().collect();
        if before.len() == after.len() {
            for (index, (old, new)) in before.iter().zip(&after).enumerate() {
                if old != new {
                    println!("@@ line {} @@\n-{old}\n+{new}", index + 1);
                }
            }
        } else {
            let prefix = before
                .iter()
                .zip(&after)
                .take_while(|(old, new)| old == new)
                .count();
            let suffix = before[prefix..]
                .iter()
                .rev()
                .zip(after[prefix..].iter().rev())
                .take_while(|(old, new)| old == new)
                .count();
            println!("@@ after line {prefix} @@");
            for line in &before[prefix..before.len() - suffix] {
                println!("-{line}");
            }
            for line in &after[prefix..after.len() - suffix] {
                println!("+{line}");
            }
        }
    }
}

fn apply_plan(root: &Path, plan: &Plan) -> Result<()> {
    for change in &plan.changes {
        fs::write(root.join(&change.path), &change.after).map_err(|e| {
            format!(
                "cannot write {}: {e}; inspect the worktree and rerun after recovery",
                change.path.display()
            )
        })?;
    }
    let outcome = (|| {
        let output = Command::new("cargo")
            .args([
                "metadata",
                "--offline",
                "--locked",
                "--no-deps",
                "--format-version",
                "1",
            ])
            .current_dir(root)
            .output()
            .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata rejected the prepared manifest or lockfile: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        git(
            root,
            &["add", "--", "Cargo.toml", "Cargo.lock", "CHANGELOG.md"],
        )?;
        git(
            root,
            &[
                "commit",
                "-m",
                &format!("chore(release): prepare v{}", plan.version),
            ],
        )?;
        git(
            root,
            &[
                "tag",
                "-a",
                &format!("v{}", plan.version),
                "-m",
                &format!("Release v{}", plan.version),
            ],
        )?;
        Ok(())
    })();
    outcome.map_err(|error: String| format!("{error}. Local edits or the release commit may remain; inspect 'git status' and 'git log -1', then finish or revert them deliberately. No push occurred"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

    struct TempRepo(PathBuf);

    impl TempRepo {
        fn new() -> Self {
            let id = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("rustel-release-test-{}-{id}", std::process::id()));
            if root.exists() {
                fs::remove_dir_all(&root).unwrap();
            }
            fs::create_dir_all(root.join("crates/demo/src")).unwrap();
            fs::write(root.join("Cargo.toml"), "[workspace]\nresolver = \"3\"\nmembers = [\n    \"crates/demo\",\n]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n").unwrap();
            fs::write(
                root.join("crates/demo/Cargo.toml"),
                "[package]\nname = \"demo\"\nversion.workspace = true\nedition.workspace = true\n",
            )
            .unwrap();
            fs::write(root.join("crates/demo/src/lib.rs"), "pub fn demo() {}\n").unwrap();
            fs::write(root.join("Cargo.lock"), "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n").unwrap();
            fs::write(root.join("CHANGELOG.md"), "# Changelog\n\n## Unreleased\n\nHand-written note stays intact.\n\n## v0.0.9\n\nEarlier notes.\n").unwrap();
            git(&root, &["init", "-q", "-b", "main"]).unwrap();
            git(&root, &["config", "user.name", "Release Test"]).unwrap();
            git(&root, &["config", "user.email", "release@example.invalid"]).unwrap();
            git(&root, &["config", "core.autocrlf", "false"]).unwrap();
            git(&root, &["add", "."]).unwrap();
            git(&root, &["commit", "-qm", "Initial release candidate"]).unwrap();
            Self(root)
        }

        fn root(&self) -> &Path {
            &self.0
        }

        fn write_and_commit(&self, message: &str) {
            fs::write(self.0.join("note.txt"), message).unwrap();
            git(self.root(), &["add", "."]).unwrap();
            git(self.root(), &["commit", "-qm", message]).unwrap();
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn accepts_each_version_form() {
        let repo = TempRepo::new();
        for (request, expected) in [
            ("patch", "0.1.1"),
            ("minor", "0.2.0"),
            ("major", "1.0.0"),
            ("2.3.4", "2.3.4"),
            ("0.1.0", "0.1.0"),
        ] {
            assert_eq!(
                build_plan(repo.root(), request)
                    .unwrap()
                    .version
                    .to_string(),
                expected
            );
        }
        for request in ["0.1", "01.2.3", "0.1.0-rc1", "0.0.9", "v0.2.0"] {
            assert!(build_plan(repo.root(), request).is_err(), "{request}");
        }
    }

    #[test]
    fn dry_run_plan_does_not_change_files_or_git() {
        let repo = TempRepo::new();
        let head = git(repo.root(), &["rev-parse", "HEAD"]).unwrap();
        let before = ["Cargo.toml", "Cargo.lock", "CHANGELOG.md"]
            .map(|path| read(repo.root(), path).unwrap());
        let plan = build_plan(repo.root(), "patch").unwrap();
        assert_eq!(plan.changes.len(), 3);
        assert_eq!(
            plan.changes[0].after.matches("version = \"0.1.1\"").count(),
            1
        );
        assert_eq!(
            plan.changes[1].after.matches("version = \"0.1.1\"").count(),
            1
        );
        assert!(
            plan.changes[2]
                .after
                .contains("Hand-written note stays intact.")
        );
        assert!(
            plan.changes[2]
                .after
                .contains("## v0.0.9\n\nEarlier notes.")
        );
        assert!(plan.changes[2].after.contains("Initial release candidate"));
        assert_eq!(
            before,
            ["Cargo.toml", "Cargo.lock", "CHANGELOG.md"]
                .map(|path| read(repo.root(), path).unwrap())
        );
        assert_eq!(git(repo.root(), &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(
            git(repo.root(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        assert!(git(repo.root(), &["tag", "--list"]).unwrap().is_empty());
    }

    #[test]
    fn uses_previous_release_tag_for_draft() {
        let repo = TempRepo::new();
        git(repo.root(), &["tag", "v0.1.0"]).unwrap();
        repo.write_and_commit("Add feature after tag");
        let plan = build_plan(repo.root(), "minor").unwrap();
        assert_eq!(plan.previous_tag.as_deref(), Some("v0.1.0"));
        let changelog = &plan.changes[2].after;
        assert!(changelog.contains("### Commit draft since v0.1.0"));
        assert!(changelog.contains("Add feature after tag"));
        assert!(!changelog.contains("Initial release candidate"));
    }

    #[test]
    fn ignores_tags_with_repeated_v_prefixes() {
        let repo = TempRepo::new();
        for tag in ["v0.1.0", "vv9.0.0", "vvv9.0.0"] {
            git(repo.root(), &["tag", tag]).unwrap();
        }
        repo.write_and_commit("Fix after the release");
        let plan = build_plan(repo.root(), "patch").unwrap();
        assert_eq!(plan.version, Version(0, 1, 1));
        assert_eq!(plan.previous_tag.as_deref(), Some("v0.1.0"));
        let changelog = &plan.changes[2].after;
        assert!(changelog.contains("Fix after the release"));
        assert!(!changelog.contains("Initial release candidate"));
    }

    #[test]
    fn changes_only_the_workspace_version_with_each_line_ending() {
        for ending in ["\n", "\r\n"] {
            let manifest = "[package]\nversion = \"0.1.0\"\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\ndemo = { version = \"0.1.0\" }\n"
                .replace('\n', ending);
            let expected = manifest.replace(
                &format!("[workspace.package]{ending}version = \"0.1.0\""),
                &format!("[workspace.package]{ending}version = \"0.2.0\""),
            );
            assert_eq!(workspace_version(&manifest).unwrap(), Version(0, 1, 0));
            assert_eq!(
                change_workspace_version(&manifest, Version(0, 1, 0), Version(0, 2, 0)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn empty_unreleased_section_keeps_previous_release_notes_separate() {
        for ending in ["\n", "\r\n"] {
            let changelog = "# Changelog\n\n## Unreleased\n\n## v0.1.0\n\nPrevious notes.\n"
                .replace('\n', ending);
            let draft =
                draft_changelog(&changelog, "v0.1.1", Some("v0.1.0"), "Fix a bug\n").unwrap();
            let expected = "# Changelog\n\n## Unreleased\n\n## v0.1.1\n\n### Commit draft since v0.1.0 (review before publishing)\n\n- Fix a bug\n\n## v0.1.0\n\nPrevious notes.\n"
                .replace('\n', ending);
            assert_eq!(draft, expected);
        }
    }

    #[test]
    fn unreleased_heading_at_end_of_file_is_accepted() {
        for ending in ["\n", "\r\n"] {
            let changelog = "# Changelog\n\n## Unreleased\n".replace('\n', ending);
            let draft = draft_changelog(&changelog, "v0.1.0", None, "First release\n").unwrap();
            let expected = "# Changelog\n\n## Unreleased\n\n## v0.1.0\n\n### Commit draft since repository start (review before publishing)\n\n- First release\n"
                .replace('\n', ending);
            assert_eq!(draft, expected);
        }
    }

    #[test]
    fn release_preserves_crlf_checkout_line_endings() {
        let repo = TempRepo::new();
        git(repo.root(), &["config", "core.autocrlf", "true"]).unwrap();
        for path in git(repo.root(), &["ls-files"]).unwrap().lines() {
            let text = read(repo.root(), path).unwrap();
            fs::write(repo.root().join(path), text.replace('\n', "\r\n")).unwrap();
        }
        // Refresh the index after changing Git's line-ending mode.
        git(repo.root(), &["add", "--renormalize", "."]).unwrap();
        let paths = ["Cargo.toml", "Cargo.lock", "CHANGELOG.md"];
        let before = paths.map(|path| read(repo.root(), path).unwrap());
        let plan = build_plan(repo.root(), "patch").unwrap();
        assert_eq!(before, paths.map(|path| read(repo.root(), path).unwrap()));
        assert_eq!(plan.changes.len(), 3);
        for change in &plan.changes {
            assert!(change.after.contains("\r\n"));
            assert!(!change.after.replace("\r\n", "").contains('\n'));
        }
        assert!(
            plan.changes[2]
                .after
                .contains("## v0.0.9\r\n\r\nEarlier notes.\r\n")
        );
        apply_plan(repo.root(), &plan).unwrap();
        for change in &plan.changes {
            assert_eq!(
                fs::read_to_string(repo.root().join(&change.path)).unwrap(),
                change.after
            );
        }
        assert_eq!(
            git(repo.root(), &["tag", "--list", "v0.1.1"])
                .unwrap()
                .trim(),
            "v0.1.1"
        );
        assert!(
            git(repo.root(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn refuses_a_detached_head_with_a_clear_message() {
        let repo = TempRepo::new();
        git(repo.root(), &["checkout", "-q", "--detach"]).unwrap();
        let error = build_plan(repo.root(), "patch").unwrap_err();
        assert_eq!(error, "check out a branch before preparing a release");
    }

    #[test]
    fn refuses_dirty_tree_and_duplicate_tag() {
        let repo = TempRepo::new();
        fs::write(repo.root().join("untracked.txt"), "unsaved").unwrap();
        assert!(
            build_plan(repo.root(), "patch")
                .unwrap_err()
                .contains("dirty")
        );
        fs::remove_file(repo.root().join("untracked.txt")).unwrap();
        git(repo.root(), &["tag", "v0.1.1"]).unwrap();
        assert!(
            build_plan(repo.root(), "patch")
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[test]
    fn commit_failure_leaves_recoverable_edits_and_no_tag() {
        let repo = TempRepo::new();
        let plan = build_plan(repo.root(), "patch").unwrap();
        let hooks = repo.root().join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(repo.root(), &["config", "core.hooksPath", "hooks"]).unwrap();
        let head = git(repo.root(), &["rev-parse", "HEAD"]).unwrap();
        let error = apply_plan(repo.root(), &plan).unwrap_err();
        assert!(error.contains("No push occurred"));
        assert!(error.contains("git status"));
        assert_eq!(git(repo.root(), &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(
            !git(repo.root(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        assert!(
            git(repo.root(), &["tag", "--list", "v0.1.1"])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn tag_failure_keeps_release_commit_and_does_not_rewrite_tag() {
        let repo = TempRepo::new();
        let plan = build_plan(repo.root(), "patch").unwrap();
        let original = git(repo.root(), &["rev-parse", "HEAD"]).unwrap();
        git(repo.root(), &["tag", "v0.1.1"]).unwrap();
        let error = apply_plan(repo.root(), &plan).unwrap_err();
        assert!(error.contains("No push occurred"));
        assert_ne!(git(repo.root(), &["rev-parse", "HEAD"]).unwrap(), original);
        assert_eq!(
            git(repo.root(), &["rev-parse", "v0.1.1"]).unwrap(),
            original
        );
        assert!(
            git(repo.root(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn refuses_version_behind_existing_release() {
        let repo = TempRepo::new();
        git(repo.root(), &["tag", "v1.0.0"]).unwrap();
        let error = build_plan(repo.root(), "1.0.1").unwrap_err();
        assert!(error.contains("behind release tag"));
    }

    #[test]
    fn successful_release_creates_only_local_commit_and_tag() {
        let repo = TempRepo::new();
        let plan = build_plan(repo.root(), "patch").unwrap();
        apply_plan(repo.root(), &plan).unwrap();
        assert_eq!(
            git(repo.root(), &["tag", "--list", "v0.1.1"])
                .unwrap()
                .trim(),
            "v0.1.1"
        );
        assert!(
            git(repo.root(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        assert!(
            git(repo.root(), &["log", "-1", "--format=%s"])
                .unwrap()
                .contains("prepare v0.1.1")
        );
    }
}
