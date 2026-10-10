# rustelup

rustelup installs and updates rustel. Run it once to install rustel, and run
it again to update.

## Install

Linux and macOS:

```sh
curl -fsSL https://rustel.cc/install | bash
```

Windows, in PowerShell:

```powershell
irm https://rustel.cc/install.ps1 | iex
```

Open a new terminal, then:

```sh
rustelup
```

In PowerShell with the default execution policy (`Restricted`), run
`rustelup.cmd`. The installer prints this hint when it applies.

This downloads the newest release build for your platform into
`~/.rustel/bin` (`%USERPROFILE%\.rustel\bin` on Windows) and puts it on
your PATH. Linux x86_64, Linux aarch64 (Raspberry Pi), macOS on Apple
Silicon and Intel, and Windows x86_64 all ship prebuilt. The Windows
installer needs PowerShell 5 and the `tar` that ships with Windows 10 and
later. The Linux x86_64 build needs glibc 2.39 or later (Ubuntu 24.04,
Debian 13). The aarch64 build comes from the same Ubuntu release, so expect
the same limit. On an older system, use the [build guide](../docs/building.md).

The Linux builds need the ALSA library (`libasound`) and `libudev` to
start. If one is missing, the binary does not start and `rustelup` stops
with `downloaded rustel failed its version check`. Install both libraries
with your package manager, then run `rustelup` again. On Linux and macOS,
`rustelup` needs `curl`, `tar`, and `sha256sum` or `shasum`.

The bootstrap downloads `rustelup` (or `rustelup.ps1` on Windows) from the
latest release. Both it and `rustelup` verify the release asset against its
`.sha256` sidecar before replacing an installed file. A failed download or
verification leaves the previous installation in place. Releases without
checksums cannot be installed by this updater.

## Update

Run `rustelup` again. It always fetches the newest release.

## Pin a version

```sh
rustelup --version v0.1.0
rustelup --list
```

`--list` prints the last twenty release tags. `--version` installs the tag
you name. Use it to keep a performance setup on the build you rehearsed with.

## Build a branch or a commit

```sh
rustelup --branch main
rustelup --commit 26caf2b4e2a86ef908821688e461b49a9dcaa54c
```

`--branch` builds the newest commit of a branch. `--commit` builds one commit
and needs the full 40-character hash. Use them to test a change before its
release.

rustelup fetches the source into `~/.rustel/src`, builds `rustel` there with
the release profile, and installs the binary into `~/.rustel/bin`. The first
build takes about 4 minutes on one 24-thread x86-64 desktop. The checkout and
its `target` directory stay and use about 1.3 GB of disk. The next build
reuses the compiled dependencies and takes about 3 minutes on the same
desktop. A build of an unchanged commit takes 1 second. Delete
`~/.rustel/src` to free the space. rustelup stops when a tracked file in
`~/.rustel/src` has a local edit.

A source build needs `git`, [rustup](https://rustup.rs) and the system
packages in the [build guide](../docs/building.md). No checksum covers a
source build. `rustel doctor` prints the commit of the build. Run `rustelup`
with no option to return to the newest release.

## Where things live

| Path | Holds |
| --- | --- |
| `~/.rustel/bin/rustel` | the engine |
| `~/.rustel/bin/rustel.exe` | the engine on Windows |
| `~/.rustel/bin/rustelup` | the Unix updater |
| `~/.rustel/bin/rustelup.ps1` | the Windows updater |
| `~/.rustel/bin/rustelup.cmd` | Windows command shim |
| `~/.rustel/src` | the checkout and build cache of `--branch` and `--commit` |

Set `RUSTEL_DIR` before installing to move the whole tree somewhere else.

## Building from source instead

The [build guide](../docs/building.md) covers `cargo build`. rustelup and a
source build can coexist. They are separate binaries on your PATH.

## Uninstall

For the default install, remove the installed executables.

Linux and macOS:

```sh
rm -f ~/.rustel/bin/rustel ~/.rustel/bin/rustelup
```

Windows, in PowerShell:

```powershell
Remove-Item -ErrorAction SilentlyContinue @(
  "$HOME\.rustel\bin\rustel.exe",
  "$HOME\.rustel\bin\rustelup.ps1",
  "$HOME\.rustel\bin\rustelup.cmd"
)
```

After a `--branch` or `--commit` build, remove `~/.rustel/src` too.
If you installed with `RUSTEL_DIR`, use that directory's `bin` and `src` instead.
On Unix, remove the matching `export PATH="…/bin:$PATH"` line added by the
installer from your shell profile (`~/.bashrc`, `~/.zshenv`, or `~/.profile`).
For fish, remove only that bin directory from `fish_user_paths`. On Windows,
remove that bin directory from your user PATH. Remove any rustel completion
file you installed and its rustel-specific shell setup line; keep shared
completion setup used by other programs.

Keep `~/.rustel` itself: it can contain scores, sets, settings, session takes,
and downloaded samples. If you remove any of this data, back up what you want
to keep first. `RUSTEL_CONFIG_DIR`, `RUSTEL_SAMPLE_CACHE`, and
`RUSTEL_SESSION_DIR` can place that data elsewhere. A source/Cargo install is
separate: remove its binary from your chosen install prefix (or use
`cargo uninstall rustel` if installed through Cargo).
