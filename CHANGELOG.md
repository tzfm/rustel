# Changelog

## Unreleased

- Rustel announces new stable releases with a `rustelup` hint. Studio prints the notice after you quit. Disable checks with `rustel config set check_updates false`.
- Theme effects keep `//`, `=>` and other punctuation ligatures whole. The waves themes split them while a wave passed.
- Alt+click and Ctrl+click extend the selection, as Shift+click does. kitty keeps Shift+click and does not send it to Studio.
- Keyboard help says when the terminal does not send Shift+Home and Shift+End. Ghostty on Linux keeps both keys for its scrollback. See [Selecting text](docs/studio.md#selecting-text).
- Settings ▸ Keybinds rebinds the panel shortcuts: show file, rename, delete sample, trim sample, and focus the tape timeline.

## v0.1.0

First public release. The [README](https://github.com/tzfm/rustel#readme) shows what Rustel does and how to install.
