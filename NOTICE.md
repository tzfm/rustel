# Third-party licenses

The dependency tables are generated from the resolved dependency graph;
regenerate them whenever that graph changes. Source and asset attributions
are maintained by hand.

This project is AGPL-3.0-or-later (see `LICENSE`). It links the
556 packages below. This file is the attribution notice that must
travel with any binary built from this tree.

## Licenses that place obligations beyond attribution

Every package here is compatible with AGPL-3.0-or-later, but each carries
a condition a release must satisfy:

- **mp3lame-encoder 0.2.4** - LGPL-3.0 - https://github.com/DoumanAsh/mp3lame-encoder
- **mp3lame-sys 0.1.11** - LGPL-3.0 - https://github.com/DoumanAsh/mp3lame-sys
- **opuscule 0.2.1** - MPL-2.0 - https://codeberg.org/jojo-laplace/opuscule
- **serialport 4.9.0** - MPL-2.0 - https://github.com/serialport/serialport-rs
- **symphonia 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia
- **symphonia-bundle-mp3 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia
- **symphonia-codec-vorbis 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia
- **symphonia-core 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia
- **symphonia-format-ogg 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia
- **symphonia-metadata 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia
- **symphonia-utils-xiph 0.5.5** - MPL-2.0 - https://github.com/pdeljanov/Symphonia

- **LGPL-3.0** (statically linked): AGPL-3.0-or-later is a compatible
  outbound licence, but LGPL-3.0 §4 also requires that a recipient be able
  to relink the work against a modified version of the library. Shipping
  the Corresponding Source for the whole binary satisfies this; shipping
  only the notice does not.
- **MPL-2.0**: §3.3 permits distributing a Larger Work under a Secondary
  License, and §1.12 names AGPL-3.0 as one - provided the covered files do
  not carry the Exhibit B "Incompatible With Secondary Licenses" notice.
  Verified 2026-08-20 for `opuscule` and the `symphonia` crates and
  2026-08-26 for `serialport`: no source file carries it (the only matches
  are the boilerplate licence text itself). Their own file-level MPL terms continue to apply to those files.

## Scope of this file

This is the attribution notice. Distributing a binary also requires the
Corresponding Source for the statically linked LGPL components, per the
relink requirement above.

The generated tables below describe the resolved **all-features** graph. It
covers the complete default product, including Studio, Hydra, serial and MIDI,
plus opt-in instrumentation that adds no third-party dependency. A lean
`--no-default-features` build uses a subset of this table. The graphics and
camera bindings are Rust wrappers; operating-system graphics and capture
engines are not distributed here.

## Adapted source code

Rustel began as a port of [Strudel](https://strudel.cc). Adapted pattern
operations, signals, musical helpers, source transforms and output mappings
retain Strudel attribution in their source files. Original Rustel code and
additions are Copyright (C) 2026 Rustel contributors. The combined project is
distributed under AGPL-3.0-or-later; the notices below also apply to the
identified portions.

"Rustel contributors" includes tzfm (also known as undefined_aeon) and other
authors of original Rustel contributions. Each contributor retains copyright
in their own work; this collective credit does not transfer ownership.

- **rquickjs 0.9.0 allocator** - MIT, Copyright (c) 2020 Mees Delzenne.
  The allocation layout and pointer handling in `crates/jsruntime/src/alloc.rs`
  are adapted from `core/src/allocator/rust.rs` in the 0.9.0 release. That
  package records revision `d4a6b2aa3c6bba0448ac54209dfec44f310e276f` with
  local changes. Heap budgeting and refusal tracking are Rustel additions.
  The original notice is preserved in
  [crates/jsruntime/LICENSE-rquickjs](crates/jsruntime/LICENSE-rquickjs).

- **Tonal** - MIT, Copyright (c) 2015 danigb.
  `crates/core/src/tonaljs.rs` ports the pitch algebra from @tonaljs 4.10.0;
  `crates/core/src/tonaljs_scales.rs` contains its generated scale dictionary.
  See [crates/core/LICENSE-tonal](crates/core/LICENSE-tonal).

- **fdlibm and V8** - Sun's permissive fdlibm notice and V8's BSD-3-Clause
  license. `crates/core/src/fdlibm.rs` adapts Sun's routines as implemented in
  V8's `src/base/ieee754.cc`. The original Sun and V8 copyright and permission
  notices are preserved in
  [crates/core/LICENSE-fdlibm-v8](crates/core/LICENSE-fdlibm-v8).

- **Tune.js** - MIT, by Andrew Bernstein and Ben Taylor; archive copyright
  2003-2010 Victor Cerullo. `crates/core/src/tune.rs` and the frequency tables
  in `crates/core/data/tuning_list.json.gz` come through Strudel's
  `packages/xen/tunejs.js` (Copyright (C) 2022 Strudel contributors, AGPL-3.0-or-later).
  Tune.js credits the Scala tuning archive and Cerullo's Microtuner files.
  See [crates/core/LICENSE-tunejs](crates/core/LICENSE-tunejs).

- **Voicing helpers and dictionaries** - `crates/core/src/voicings.rs`
  adapts Strudel's `packages/tonal/voicings.mjs` and `tonleiter.mjs` under
  AGPL-3.0-or-later, along with helpers from Felix Roos's `chord-voicings` 0.0.1,
  whose package metadata declares ISC. See
  [crates/core/LICENSE-chord-voicings](crates/core/LICENSE-chord-voicings).
  `crates/core/assets/voicing-dicts.json` and `voicing-registry.json` preserve
  Strudel's dictionaries and registration data. The `ireal` and `ireal-ext`
  tables originate in Strudel's `packages/tonal/ireal.mjs`, which links a
  [voicing explorer](https://codesandbox.io/s/voicing-explorer-ireal-47tkx5?file=/src/ireal.js:0-16036)
  and [MIDI scraper](https://codesandbox.io/s/ireal-midi-scraper-2-gjz2mr?file=/src/index.js).
  These tables come from Strudel's AGPL-3.0-or-later tonal package. The linked
  sources' generation details and any separate terms for their inputs have
  not been independently verified.

## Extension source permissions

`crates/ext/src/switch_angel/` is a native Rust adaptation of Switch Angel's
[prebake.strudel](https://github.com/switchangel/strudel-scripts/blob/main/prebake.strudel),
including chord data and synthesizer presets. The adapted source is Git blob
`e85abf952bf718d2ee8bb4e78a074bede21ea328` (the file blob, not a commit).
Existing credits stay, including Glossing's contributions, which that file
attributes to <https://codeberg.org/glossing/Strudel_Scripts>.

Use of these adaptations has been granted under the same licence as
[Strudel](https://strudel.cc): AGPL-3.0-or-later.

The separate `pad` asset license below does not cover these scripts.

`undefined_aeon` is tzfm's alias. The `inspire` recipe embedded in
`crates/ext/tests/undefined_aeon.rs` and its native implementation in
`crates/ext/src/undefined_aeon/inspire.rs` are original Rustel contributions,
covered by the project's AGPL-3.0-or-later license.

## Fetched assets

Not Cargo dependencies, so no generator will list them. Maintained by hand.

- **hydra-synth 1.4.0** - AGPL-3.0 - https://github.com/ojack/hydra-synth.
  The library itself is not used, fetched or shipped. This tree contains its
  GLSL: `crates/hydra/src/glsl/table.rs` holds the 52 shader function bodies
  from `src/glsl/glsl-functions.js`, character for character, generated from
  that file rather than retyped, and `crates/hydra/src/glsl/compose.rs` is a
  port of its `generate-glsl.js`. That makes this crate a derivative work of
  hydra-synth, which is why its licence sits in
  `crates/hydra/LICENSE-hydra-synth` and why the attribution stays whether or
  not any JavaScript does. AGPL-3.0 is this project's own outbound licence, so
  the combination raises no condition beyond the source-availability one that
  already applies.

- **switchangel/pad** - Unlicense (public domain) -
  https://github.com/switchangel/pad. Five pad recordings by Switch Angel,
  shipped as the `swpad` bank and fetched on demand rather than bundled. The
  manifest and the audio are both pinned to commit `4f1b7bbd` rather than to a
  branch: a manifest whose bytes change under its recorded hash stops the
  whole default library from loading, and a personal repository's default
  branch can change at any time. This asset license does not cover the
  script adaptations described under "Extension source permissions" above.

- **Adventure Kid Waveforms** - CC0 1.0 - by Kristoffer Ekstrand,
  https://github.com/KristofferKarlAxelEkstrand/AKWF-FREE. 4,221 single-cycle
  waveforms in 64 `wt_` banks, fetched on demand from
  https://github.com/tzfm/akwf-waveforms, which is built from AKWF-FREE
  commit `8de90bf9` and lowered by 6 dB. Manifest and audio are pinned to
  commit `4630e06f`, for the reason given for `switchangel/pad`. The banks,
  and the order within each, follow Dough-Waveforms
  (`samples('bubo:waveforms')` on strudel.cc), so `wt_dbass:12` is the same
  waveform in both. Dough-Waveforms itself carries no licence and is not
  shipped. Its `wt_vgame` is left out, because strudel.cc's uzu-wavetables
  already defines that name.

## License totals

| Count | License |
| ---: | :--- |
| 251 | MIT OR Apache-2.0 |
| 128 | MIT |
| 24 | MIT/Apache-2.0 |
| 18 | Apache-2.0 |
| 18 | Apache-2.0 OR MIT |
| 18 | Unicode-3.0 |
| 17 | Zlib OR Apache-2.0 OR MIT |
| 10 | Apache-2.0/MIT |
| 9 | MPL-2.0 |
| 6 | ISC |
| 6 | Unlicense OR MIT |
| 5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| 4 | MIT OR Apache-2.0 OR Zlib |
| 3 | BSD-3-Clause |
| 3 | Zlib |
| 2 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| 2 | BSD-2-Clause OR Apache-2.0 OR MIT |
| 2 | BSD-2-Clause OR MIT OR Apache-2.0 |
| 2 | BSD-3-Clause OR Apache-2.0 |
| 2 | BSD-3-Clause OR MIT OR Apache-2.0 |
| 2 | BSL-1.0 |
| 2 | CDLA-Permissive-2.0 |
| 2 | LGPL-3.0 |
| 2 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| 2 | Unlicense/MIT |
| 1 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| 1 | (MIT OR Apache-2.0) AND Unicode-DFS-2016 |
| 1 | 0BSD OR MIT OR Apache-2.0 |
| 1 | Apache-2.0 / MIT |
| 1 | Apache-2.0 AND ISC |
| 1 | Apache-2.0 OR BSL-1.0 |
| 1 | Apache-2.0 OR GPL-2.0-only |
| 1 | Apache-2.0 OR ISC OR MIT |
| 1 | Apache-2.0 WITH LLVM-exception OR BSL-1.0 |
| 1 | CC0-1.0 |
| 1 | MIT / Apache-2.0 |
| 1 | MIT AND Unicode-DFS-2016 |
| 1 | MIT OR GPL-3.0-only |
| 1 | MIT OR MPL-2.0 |
| 1 | MIT OR Zlib OR Apache-2.0 |
| 1 | WTFPL |

## Every dependency

| Crate | Version | License |
| :--- | :--- | :--- |
| `adler2` | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| `aho-corasick` | 1.1.5 | Unlicense OR MIT |
| `allocator-api2` | 0.2.21 | MIT OR Apache-2.0 |
| `alsa` | 0.11.0 | Apache-2.0/MIT |
| `alsa-sys` | 0.4.0 | MIT |
| `android-build` | 0.1.4 | MIT |
| `android_system_properties` | 0.1.6 | MIT OR Apache-2.0 |
| `anstream` | 1.0.0 | MIT OR Apache-2.0 |
| `anstyle` | 1.0.14 | MIT OR Apache-2.0 |
| `anstyle-parse` | 1.0.0 | MIT OR Apache-2.0 |
| `anstyle-query` | 1.1.5 | MIT OR Apache-2.0 |
| `anstyle-wincon` | 3.0.11 | MIT OR Apache-2.0 |
| `anyhow` | 1.0.104 | MIT OR Apache-2.0 |
| `approx` | 0.5.1 | Apache-2.0 |
| `arboard` | 3.6.1 | MIT OR Apache-2.0 |
| `arrayvec` | 0.7.8 | MIT OR Apache-2.0 |
| `ash` | 0.38.0+1.3.281 | MIT OR Apache-2.0 |
| `async-trait` | 0.1.92 | MIT OR Apache-2.0 |
| `atomic` | 0.6.1 | Apache-2.0/MIT |
| `autocfg` | 1.5.1 | Apache-2.0 OR MIT |
| `autotools` | 0.2.7 | MIT |
| `base64` | 0.22.1 | MIT OR Apache-2.0 |
| `base64-simd` | 0.8.0 | MIT |
| `bit-set` | 0.5.3 | MIT/Apache-2.0 |
| `bit-set` | 0.8.0 | Apache-2.0 OR MIT |
| `bit-vec` | 0.6.3 | MIT/Apache-2.0 |
| `bit-vec` | 0.8.0 | Apache-2.0 OR MIT |
| `bitflags` | 1.3.2 | MIT/Apache-2.0 |
| `bitflags` | 2.13.1 | MIT OR Apache-2.0 |
| `block` | 0.1.6 | MIT |
| `block-buffer` | 0.10.4 | MIT OR Apache-2.0 |
| `block-buffer` | 0.12.1 | MIT OR Apache-2.0 |
| `block2` | 0.6.2 | MIT |
| `bon` | 3.10.0 | MIT OR Apache-2.0 |
| `bon-macros` | 3.10.0 | MIT OR Apache-2.0 |
| `bumpalo` | 3.20.3 | MIT OR Apache-2.0 |
| `by_address` | 1.2.1 | MIT OR Apache-2.0 |
| `bytecount` | 0.6.9 | Apache-2.0/MIT |
| `bytemuck` | 1.25.2 | Zlib OR Apache-2.0 OR MIT |
| `bytemuck_derive` | 1.12.0 | Zlib OR Apache-2.0 OR MIT |
| `byteorder` | 1.5.0 | Unlicense OR MIT |
| `byteorder-lite` | 0.1.0 | Unlicense OR MIT |
| `bytes` | 1.12.1 | MIT |
| `castaway` | 0.2.4 | MIT |
| `cc` | 1.4.2 | MIT OR Apache-2.0 |
| `cesu8` | 1.1.0 | Apache-2.0/MIT |
| `cfg-if` | 1.0.4 | MIT OR Apache-2.0 |
| `cfg_aliases` | 0.2.2 | MIT |
| `clap` | 4.6.6 | MIT OR Apache-2.0 |
| `clap_builder` | 4.6.6 | MIT OR Apache-2.0 |
| `clap_complete` | 4.6.9 | MIT OR Apache-2.0 |
| `clap_derive` | 4.6.4 | MIT OR Apache-2.0 |
| `clap_lex` | 1.1.0 | MIT OR Apache-2.0 |
| `clipboard-win` | 5.4.1 | BSL-1.0 |
| `codespan-reporting` | 0.12.0 | Apache-2.0 |
| `color_quant` | 1.1.0 | MIT |
| `colorchoice` | 1.0.5 | MIT OR Apache-2.0 |
| `combine` | 4.6.7 | MIT |
| `compact_str` | 0.10.0 | MIT |
| `compact_str` | 0.9.1 | MIT |
| `const-oid` | 0.10.2 | Apache-2.0 OR MIT |
| `convert_case` | 0.10.0 | MIT |
| `convert_case` | 0.12.0 | MIT |
| `core-foundation` | 0.10.1 | MIT OR Apache-2.0 |
| `core-foundation-sys` | 0.8.7 | MIT OR Apache-2.0 |
| `core-graphics-types` | 0.2.0 | MIT OR Apache-2.0 |
| `coreaudio-rs` | 0.14.2 | MIT/Apache-2.0 |
| `coremidi` | 0.9.2 | MIT |
| `coremidi-sys` | 3.2.1 | MIT |
| `cow-utils` | 0.1.3 | MIT |
| `cpal` | 0.18.2 | Apache-2.0 |
| `cpufeatures` | 0.2.17 | MIT OR Apache-2.0 |
| `cpufeatures` | 0.3.0 | MIT OR Apache-2.0 |
| `crc32fast` | 1.5.0 | MIT OR Apache-2.0 |
| `crossbeam-channel` | 0.5.16 | MIT OR Apache-2.0 |
| `crossbeam-deque` | 0.8.7 | MIT OR Apache-2.0 |
| `crossbeam-epoch` | 0.9.20 | MIT OR Apache-2.0 |
| `crossbeam-utils` | 0.8.22 | MIT OR Apache-2.0 |
| `crossterm` | 0.29.0 | MIT |
| `crossterm_winapi` | 0.9.1 | MIT |
| `crunchy` | 0.2.4 | MIT |
| `crypto-common` | 0.1.7 | MIT OR Apache-2.0 |
| `crypto-common` | 0.2.2 | MIT OR Apache-2.0 |
| `csscolorparser` | 0.6.2 | MIT OR Apache-2.0 |
| `darling` | 0.24.1 | MIT |
| `darling_core` | 0.24.1 | MIT |
| `darling_macro` | 0.24.1 | MIT |
| `dasp_sample` | 0.11.0 | MIT OR Apache-2.0 |
| `deltae` | 0.3.2 | MIT |
| `deranged` | 0.5.8 | MIT OR Apache-2.0 |
| `derive_more` | 2.1.1 | MIT |
| `derive_more-impl` | 2.1.1 | MIT |
| `digest` | 0.10.7 | MIT OR Apache-2.0 |
| `digest` | 0.11.3 | MIT OR Apache-2.0 |
| `dispatch2` | 0.3.1 | Zlib OR Apache-2.0 OR MIT |
| `displaydoc` | 0.2.7 | MIT OR Apache-2.0 |
| `dlopen2` | 0.9.0 | MIT |
| `document-features` | 0.2.12 | MIT OR Apache-2.0 |
| `downcast-rs` | 1.2.1 | MIT/Apache-2.0 |
| `dragonbox_ecma` | 0.1.12 | Apache-2.0 WITH LLVM-exception OR BSL-1.0 |
| `either` | 1.17.0 | MIT OR Apache-2.0 |
| `encoding_rs` | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| `enum-primitive-derive` | 0.3.0 | MIT |
| `equivalent` | 1.0.2 | Apache-2.0 OR MIT |
| `errno` | 0.3.14 | MIT OR Apache-2.0 |
| `error-code` | 3.4.0 | BSL-1.0 |
| `euclid` | 0.22.14 | MIT OR Apache-2.0 |
| `fancy-regex` | 0.11.0 | MIT |
| `fastrand` | 2.5.0 | Apache-2.0 OR MIT |
| `fdeflate` | 0.3.7 | MIT OR Apache-2.0 |
| `filedescriptor` | 0.8.3 | MIT |
| `find-msvc-tools` | 0.1.10 | MIT OR Apache-2.0 |
| `finl_unicode` | 1.4.0 | (MIT OR Apache-2.0) AND Unicode-DFS-2016 |
| `fixedbitset` | 0.4.2 | MIT/Apache-2.0 |
| `flate2` | 1.1.9 | MIT OR Apache-2.0 |
| `fnv` | 1.0.7 | Apache-2.0 / MIT |
| `foldhash` | 0.1.5 | Zlib |
| `foldhash` | 0.2.0 | Zlib |
| `font8x8` | 0.3.1 | MIT |
| `foreign-types` | 0.5.0 | MIT/Apache-2.0 |
| `foreign-types-macros` | 0.2.4 | MIT/Apache-2.0 |
| `foreign-types-shared` | 0.3.1 | MIT/Apache-2.0 |
| `form_urlencoded` | 1.2.2 | MIT OR Apache-2.0 |
| `futures` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-channel` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-core` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-executor` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-io` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-macro` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-sink` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-task` | 0.3.34 | MIT OR Apache-2.0 |
| `futures-util` | 0.3.34 | MIT OR Apache-2.0 |
| `generic-array` | 0.14.7 | MIT |
| `gethostname` | 1.1.0 | Apache-2.0 |
| `getrandom` | 0.2.17 | MIT OR Apache-2.0 |
| `getrandom` | 0.3.4 | MIT OR Apache-2.0 |
| `getrandom` | 0.4.3 | MIT OR Apache-2.0 |
| `gif` | 0.14.2 | MIT OR Apache-2.0 |
| `gilrs` | 0.11.2 | Apache-2.0/MIT |
| `gilrs-core` | 0.6.8 | Apache-2.0/MIT |
| `gl_generator` | 0.14.0 | Apache-2.0 |
| `glob` | 0.3.4 | MIT OR Apache-2.0 |
| `glow` | 0.16.0 | MIT OR Apache-2.0 OR Zlib |
| `glutin_wgl_sys` | 0.6.1 | Apache-2.0 |
| `gpu-alloc` | 0.6.2 | MIT OR Apache-2.0 |
| `gpu-alloc-types` | 0.3.1 | MIT OR Apache-2.0 |
| `gpu-allocator` | 0.27.0 | MIT OR Apache-2.0 |
| `gpu-descriptor` | 0.3.2 | MIT OR Apache-2.0 |
| `gpu-descriptor-types` | 0.2.0 | MIT OR Apache-2.0 |
| `half` | 2.7.1 | MIT OR Apache-2.0 |
| `hashbrown` | 0.15.5 | MIT OR Apache-2.0 |
| `hashbrown` | 0.16.1 | MIT OR Apache-2.0 |
| `hashbrown` | 0.17.1 | MIT OR Apache-2.0 |
| `heck` | 0.5.0 | MIT OR Apache-2.0 |
| `hex` | 0.4.3 | MIT OR Apache-2.0 |
| `hexf-parse` | 0.2.1 | CC0-1.0 |
| `hybrid-array` | 0.4.14 | MIT OR Apache-2.0 |
| `icu_collections` | 2.3.0 | Unicode-3.0 |
| `icu_locale_core` | 2.3.0 | Unicode-3.0 |
| `icu_normalizer` | 2.3.0 | Unicode-3.0 |
| `icu_normalizer_data` | 2.3.0 | Unicode-3.0 |
| `icu_properties` | 2.3.0 | Unicode-3.0 |
| `icu_properties_data` | 2.3.0 | Unicode-3.0 |
| `icu_provider` | 2.3.0 | Unicode-3.0 |
| `ident_case` | 1.0.1 | MIT/Apache-2.0 |
| `idna` | 1.1.0 | MIT OR Apache-2.0 |
| `idna_adapter` | 1.2.2 | Apache-2.0 OR MIT |
| `image` | 0.25.10 | MIT OR Apache-2.0 |
| `image-webp` | 0.2.4 | MIT OR Apache-2.0 |
| `indexmap` | 2.14.0 | Apache-2.0 OR MIT |
| `indoc` | 2.0.7 | MIT OR Apache-2.0 |
| `inotify` | 0.11.5 | ISC |
| `inotify-sys` | 0.1.8 | ISC |
| `instability` | 0.3.13 | MIT |
| `io-kit-sys` | 0.4.1 | MIT / Apache-2.0 |
| `ioctl-rs` | 0.1.6 | MIT |
| `is_terminal_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `itertools` | 0.14.0 | MIT OR Apache-2.0 |
| `itertools` | 0.15.0 | MIT OR Apache-2.0 |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 |
| `java-locator` | 0.1.9 | MIT/Apache-2.0 |
| `jni` | 0.21.1 | MIT/Apache-2.0 |
| `jni` | 0.22.4 | MIT OR Apache-2.0 |
| `jni-macros` | 0.22.4 | MIT OR Apache-2.0 |
| `jni-min-helper` | 0.3.4 | MIT OR Apache-2.0 |
| `jni-sys` | 0.3.1 | MIT OR Apache-2.0 |
| `jni-sys` | 0.4.1 | MIT OR Apache-2.0 |
| `jni-sys-macros` | 0.4.1 | MIT OR Apache-2.0 |
| `js-sys` | 0.3.77 | MIT OR Apache-2.0 |
| `json-escape-simd` | 3.1.1 | MIT |
| `kasuari` | 0.4.12 | MIT OR Apache-2.0 |
| `khronos-egl` | 6.0.0 | MIT/Apache-2.0 |
| `khronos_api` | 3.1.0 | Apache-2.0 |
| `lab` | 0.11.0 | MIT |
| `lazy_static` | 1.5.0 | MIT OR Apache-2.0 |
| `libc` | 0.2.189 | MIT OR Apache-2.0 |
| `libloading` | 0.7.4 | ISC |
| `libloading` | 0.8.9 | ISC |
| `libm` | 0.2.16 | MIT |
| `libudev-sys` | 0.1.4 | MIT |
| `line-clipping` | 0.3.8 | MIT OR Apache-2.0 |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `litemap` | 0.8.3 | Unicode-3.0 |
| `litrs` | 1.0.0 | MIT OR Apache-2.0 |
| `lock_api` | 0.4.14 | MIT OR Apache-2.0 |
| `log` | 0.4.33 | MIT OR Apache-2.0 |
| `lru` | 0.18.2 | MIT |
| `mac_address` | 1.1.8 | MIT OR Apache-2.0 |
| `mach2` | 0.4.3 | BSD-2-Clause OR MIT OR Apache-2.0 |
| `mach2` | 0.6.0 | BSD-2-Clause OR MIT OR Apache-2.0 |
| `malloc_buf` | 0.0.6 | MIT |
| `md-5` | 0.11.0 | MIT OR Apache-2.0 |
| `memchr` | 2.8.3 | Unlicense OR MIT |
| `memmem` | 0.1.1 | MIT/Apache-2.0 |
| `memoffset` | 0.6.5 | MIT |
| `memoffset` | 0.9.1 | MIT |
| `metal` | 0.32.0 | MIT OR Apache-2.0 |
| `micromath` | 2.1.0 | Apache-2.0 OR MIT |
| `midir` | 0.11.0 | MIT |
| `minimal-lexical` | 0.2.1 | MIT/Apache-2.0 |
| `miniz_oxide` | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| `mio` | 1.2.2 | MIT |
| `moxcms` | 0.8.1 | BSD-3-Clause OR Apache-2.0 |
| `mp3lame-encoder` | 0.2.4 | LGPL-3.0 |
| `mp3lame-sys` | 0.1.11 | LGPL-3.0 |
| `naga` | 26.0.0 | MIT OR Apache-2.0 |
| `ndk` | 0.9.0 | MIT OR Apache-2.0 |
| `ndk-context` | 0.1.1 | MIT OR Apache-2.0 |
| `ndk-sys` | 0.6.0+11769913 | MIT OR Apache-2.0 |
| `nix` | 0.25.1 | MIT |
| `nix` | 0.26.4 | MIT |
| `nix` | 0.29.0 | MIT |
| `nix` | 0.31.3 | MIT |
| `nom` | 7.1.3 | MIT |
| `nonmax` | 0.5.5 | MIT OR Apache-2.0 |
| `num-bigint` | 0.5.1 | MIT OR Apache-2.0 |
| `num-complex` | 0.4.6 | MIT OR Apache-2.0 |
| `num-conv` | 0.2.2 | MIT OR Apache-2.0 |
| `num-derive` | 0.4.2 | MIT OR Apache-2.0 |
| `num-integer` | 0.1.47 | MIT OR Apache-2.0 |
| `num-traits` | 0.2.19 | MIT OR Apache-2.0 |
| `num_enum` | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 |
| `num_enum_derive` | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 |
| `num_threads` | 0.1.7 | MIT OR Apache-2.0 |
| `objc` | 0.2.7 | MIT |
| `objc2` | 0.6.4 | MIT |
| `objc2-app-kit` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-audio-toolbox` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-av-foundation` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-avf-audio` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-core-audio` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-core-audio-types` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-core-foundation` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-core-graphics` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-core-media` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-core-video` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-encode` | 4.1.0 | MIT |
| `objc2-foundation` | 0.3.2 | MIT |
| `objc2-game-controller` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-io-kit` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `objc2-io-surface` | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| `once_cell` | 1.21.4 | MIT OR Apache-2.0 |
| `once_cell_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `opuscule` | 0.2.1 | MPL-2.0 |
| `ordered-float` | 4.6.0 | MIT |
| `outref` | 0.5.2 | MIT |
| `owo-colors` | 4.3.0 | MIT |
| `oxc-miette` | 4.0.0 | Apache-2.0 |
| `oxc_allocator` | 0.144.0 | MIT |
| `oxc_ast` | 0.144.0 | MIT |
| `oxc_ast_macros` | 0.144.0 | MIT |
| `oxc_ast_visit` | 0.144.0 | MIT |
| `oxc_codegen` | 0.144.0 | MIT |
| `oxc_data_structures` | 0.144.0 | MIT |
| `oxc_diagnostics` | 0.144.0 | MIT |
| `oxc_ecmascript` | 0.144.0 | MIT |
| `oxc_estree` | 0.144.0 | MIT |
| `oxc_index` | 5.0.0 | MIT |
| `oxc_parser` | 0.144.0 | MIT |
| `oxc_regular_expression` | 0.144.0 | MIT |
| `oxc_semantic` | 0.144.0 | MIT |
| `oxc_sourcemap` | 8.1.2 | BSD-3-Clause |
| `oxc_span` | 0.144.0 | MIT |
| `oxc_str` | 0.144.0 | MIT |
| `oxc_syntax` | 0.144.0 | MIT |
| `oxiarc-core` | 0.4.1 | Apache-2.0 |
| `oxiarc-deflate` | 0.4.1 | Apache-2.0 |
| `oximedia-capture` | 0.2.1 | Apache-2.0 |
| `oximedia-codec` | 0.2.1 | Apache-2.0 |
| `oximedia-core` | 0.2.1 | Apache-2.0 |
| `oximedia-io` | 0.2.1 | Apache-2.0 |
| `oximedia-simd` | 0.2.1 | Apache-2.0 |
| `palette` | 0.7.7 | MIT OR Apache-2.0 |
| `palette_derive` | 0.7.7 | MIT OR Apache-2.0 |
| `palette_math` | 0.7.7 | MIT OR Apache-2.0 |
| `parking_lot` | 0.12.5 | MIT OR Apache-2.0 |
| `parking_lot_core` | 0.9.12 | MIT OR Apache-2.0 |
| `paste` | 1.0.15 | MIT OR Apache-2.0 |
| `percent-encoding` | 2.3.2 | MIT OR Apache-2.0 |
| `pest` | 2.9.0 | MIT OR Apache-2.0 |
| `pest_derive` | 2.9.0 | MIT OR Apache-2.0 |
| `pest_generator` | 2.9.0 | MIT OR Apache-2.0 |
| `pest_meta` | 2.9.0 | MIT OR Apache-2.0 |
| `phf` | 0.11.3 | MIT |
| `phf` | 0.14.0 | MIT |
| `phf_codegen` | 0.11.3 | MIT |
| `phf_generator` | 0.11.3 | MIT |
| `phf_generator` | 0.14.0 | MIT |
| `phf_macros` | 0.11.3 | MIT |
| `phf_macros` | 0.14.0 | MIT |
| `phf_shared` | 0.11.3 | MIT |
| `phf_shared` | 0.14.0 | MIT |
| `pin-project-lite` | 0.2.17 | Apache-2.0 OR MIT |
| `pin-utils` | 0.1.0 | MIT OR Apache-2.0 |
| `pkg-config` | 0.3.33 | MIT OR Apache-2.0 |
| `png` | 0.18.1 | MIT OR Apache-2.0 |
| `pollster` | 0.4.0 | Apache-2.0/MIT |
| `portable-atomic` | 1.15.0 | Apache-2.0 OR MIT |
| `portable-atomic-util` | 0.2.7 | Apache-2.0 OR MIT |
| `portable-pty` | 0.8.1 | MIT |
| `potential_utf` | 0.1.6 | Unicode-3.0 |
| `powerfmt` | 0.2.0 | MIT OR Apache-2.0 |
| `pp-rs` | 0.2.1 | BSD-3-Clause |
| `presser` | 0.3.1 | MIT OR Apache-2.0 |
| `prettyplease` | 0.3.0 | MIT OR Apache-2.0 |
| `primal-check` | 0.3.4 | MIT OR Apache-2.0 |
| `proc-macro-crate` | 3.5.0 | MIT OR Apache-2.0 |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 |
| `process_path` | 0.1.4 | MIT/Apache-2.0 |
| `profiling` | 1.0.18 | MIT OR Apache-2.0 |
| `pulseaudio` | 0.3.1 | MIT |
| `pxfm` | 0.1.30 | BSD-3-Clause OR Apache-2.0 |
| `quick-error` | 2.0.1 | MIT/Apache-2.0 |
| `quote` | 1.0.47 | MIT OR Apache-2.0 |
| `r-efi` | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `r-efi` | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `rand` | 0.8.8 | MIT OR Apache-2.0 |
| `rand_core` | 0.6.4 | MIT OR Apache-2.0 |
| `range-alloc` | 0.1.5 | MIT OR Apache-2.0 |
| `ratatui` | 0.30.2 | MIT |
| `ratatui-core` | 0.1.2 | MIT |
| `ratatui-crossterm` | 0.1.2 | MIT |
| `ratatui-termina` | 0.1.0 | MIT |
| `ratatui-termwiz` | 0.1.2 | MIT |
| `ratatui-widgets` | 0.3.2 | MIT |
| `raw-window-handle` | 0.6.2 | MIT OR Apache-2.0 OR Zlib |
| `rayon` | 1.12.0 | MIT OR Apache-2.0 |
| `rayon-core` | 1.13.0 | MIT OR Apache-2.0 |
| `redox_syscall` | 0.5.18 | MIT |
| `regex` | 1.13.1 | MIT OR Apache-2.0 |
| `regex-automata` | 0.4.18 | MIT OR Apache-2.0 |
| `regex-syntax` | 0.8.11 | MIT OR Apache-2.0 |
| `relative-path` | 2.0.1 | MIT OR Apache-2.0 |
| `renderdoc-sys` | 1.1.0 | MIT OR Apache-2.0 |
| `ring` | 0.17.14 | Apache-2.0 AND ISC |
| `ropey` | 1.6.1 | MIT |
| `rquickjs` | 0.14.0 | MIT |
| `rquickjs-core` | 0.14.0 | MIT |
| `rquickjs-macro` | 0.14.0 | MIT |
| `rquickjs-sys` | 0.14.0 | MIT |
| `rustc-hash` | 1.1.0 | Apache-2.0/MIT |
| `rustc-hash` | 2.1.3 | Apache-2.0 OR MIT |
| `rustc_version` | 0.4.1 | MIT OR Apache-2.0 |
| `rustfft` | 6.4.1 | MIT OR Apache-2.0 |
| `rustix` | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `rustls` | 0.23.43 | Apache-2.0 OR ISC OR MIT |
| `rustls-pki-types` | 1.15.1 | MIT OR Apache-2.0 |
| `rustls-webpki` | 0.103.14 | ISC |
| `rustversion` | 1.0.23 | MIT OR Apache-2.0 |
| `rusty-xinput` | 1.3.0 | Zlib OR Apache-2.0 OR MIT |
| `ryu` | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| `same-file` | 1.0.6 | Unlicense/MIT |
| `scopeguard` | 1.2.0 | MIT OR Apache-2.0 |
| `self_cell` | 1.3.0 | Apache-2.0 OR GPL-2.0-only |
| `semver` | 1.0.28 | MIT OR Apache-2.0 |
| `seq-macro` | 0.3.6 | MIT OR Apache-2.0 |
| `serde` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_core` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_derive` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_json` | 1.0.151 | MIT OR Apache-2.0 |
| `serial` | 0.4.0 | MIT |
| `serial-core` | 0.4.0 | MIT |
| `serial-unix` | 0.4.0 | MIT |
| `serial-windows` | 0.4.0 | MIT |
| `serialport` | 4.9.0 | MPL-2.0 |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 |
| `sha2` | 0.11.0 | MIT OR Apache-2.0 |
| `shared_library` | 0.1.9 | Apache-2.0/MIT |
| `shell-words` | 1.1.1 | MIT/Apache-2.0 |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 |
| `signal-hook` | 0.3.18 | Apache-2.0/MIT |
| `signal-hook-mio` | 0.2.5 | MIT OR Apache-2.0 |
| `signal-hook-registry` | 1.4.8 | MIT OR Apache-2.0 |
| `simd-adler32` | 0.3.10 | MIT |
| `simd_cesu8` | 1.2.0 | Apache-2.0 OR MIT |
| `simdutf8` | 0.1.5 | MIT OR Apache-2.0 |
| `similar` | 2.7.0 | Apache-2.0 |
| `siphasher` | 1.0.3 | MIT/Apache-2.0 |
| `slab` | 0.4.12 | MIT |
| `slotmap` | 1.1.1 | Zlib |
| `smallvec` | 1.15.2 | MIT OR Apache-2.0 |
| `smawk` | 0.3.3 | MIT |
| `socket2` | 0.6.5 | MIT OR Apache-2.0 |
| `spirv` | 0.3.0+sdk-1.3.268.0 | Apache-2.0 |
| `stable_deref_trait` | 1.2.1 | MIT OR Apache-2.0 |
| `static_assertions` | 1.1.0 | MIT OR Apache-2.0 |
| `str_indices` | 0.4.4 | MIT OR Apache-2.0 |
| `strength_reduce` | 0.2.4 | MIT OR Apache-2.0 |
| `strsim` | 0.11.1 | MIT |
| `strum` | 0.28.0 | MIT |
| `strum_macros` | 0.28.0 | MIT |
| `subtle` | 2.6.1 | BSD-3-Clause |
| `symphonia` | 0.5.5 | MPL-2.0 |
| `symphonia-bundle-mp3` | 0.5.5 | MPL-2.0 |
| `symphonia-codec-vorbis` | 0.5.5 | MPL-2.0 |
| `symphonia-core` | 0.5.5 | MPL-2.0 |
| `symphonia-format-ogg` | 0.5.5 | MPL-2.0 |
| `symphonia-metadata` | 0.5.5 | MPL-2.0 |
| `symphonia-utils-xiph` | 0.5.5 | MPL-2.0 |
| `syn` | 1.0.109 | MIT OR Apache-2.0 |
| `syn` | 2.0.119 | MIT OR Apache-2.0 |
| `syn` | 3.0.3 | MIT OR Apache-2.0 |
| `synstructure` | 0.13.2 | MIT |
| `tachyonfx` | 0.25.1 | MIT |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 |
| `termcolor` | 1.4.1 | Unlicense OR MIT |
| `termina` | 0.3.3 | MIT OR MPL-2.0 |
| `terminal_size` | 0.4.4 | MIT OR Apache-2.0 |
| `terminfo` | 0.9.0 | WTFPL |
| `termios` | 0.2.2 | MIT |
| `termios` | 0.3.3 | MIT |
| `termwiz` | 0.23.3 | MIT |
| `textwrap` | 0.16.2 | MIT |
| `thiserror` | 1.0.69 | MIT OR Apache-2.0 |
| `thiserror` | 2.0.20 | MIT OR Apache-2.0 |
| `thiserror-impl` | 1.0.69 | MIT OR Apache-2.0 |
| `thiserror-impl` | 2.0.20 | MIT OR Apache-2.0 |
| `time` | 0.3.55 | MIT OR Apache-2.0 |
| `time-core` | 0.1.9 | MIT OR Apache-2.0 |
| `tinystr` | 0.8.4 | Unicode-3.0 |
| `tokio` | 1.53.1 | MIT |
| `tokio-macros` | 2.7.2 | MIT |
| `toml_datetime` | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 |
| `toml_edit` | 0.25.13+spec-1.1.0 | MIT OR Apache-2.0 |
| `toml_parser` | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |
| `tracing` | 0.1.44 | MIT |
| `tracing-attributes` | 0.1.31 | MIT |
| `tracing-core` | 0.1.36 | MIT |
| `transpose` | 0.2.3 | MIT OR Apache-2.0 |
| `typenum` | 1.20.1 | MIT OR Apache-2.0 |
| `ucd-trie` | 0.1.7 | MIT OR Apache-2.0 |
| `unescaper` | 0.1.10 | MIT OR GPL-3.0-only |
| `unicode-id-start` | 1.4.0 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `unicode-ident` | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `unicode-linebreak` | 0.1.5 | Apache-2.0 |
| `unicode-segmentation` | 1.13.3 | MIT OR Apache-2.0 |
| `unicode-truncate` | 2.0.1 | MIT OR Apache-2.0 |
| `unicode-width` | 0.2.2 | MIT OR Apache-2.0 |
| `unicode-xid` | 0.2.6 | MIT OR Apache-2.0 |
| `untrusted` | 0.9.0 | ISC |
| `ureq` | 2.12.1 | MIT OR Apache-2.0 |
| `url` | 2.5.8 | MIT OR Apache-2.0 |
| `utf8_iter` | 1.0.4 | Apache-2.0 OR MIT |
| `utf8parse` | 0.2.2 | Apache-2.0 OR MIT |
| `uuid` | 1.25.0 | Apache-2.0 OR MIT |
| `vec_map` | 0.8.2 | MIT/Apache-2.0 |
| `version_check` | 0.9.5 | MIT/Apache-2.0 |
| `vsimd` | 0.8.0 | MIT |
| `vtparse` | 0.6.2 | MIT |
| `walkdir` | 2.5.0 | Unlicense/MIT |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `wasip2` | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `wasm-bindgen` | 0.2.100 | MIT OR Apache-2.0 |
| `wasm-bindgen-backend` | 0.2.100 | MIT OR Apache-2.0 |
| `wasm-bindgen-futures` | 0.4.50 | MIT OR Apache-2.0 |
| `wasm-bindgen-macro` | 0.2.100 | MIT OR Apache-2.0 |
| `wasm-bindgen-macro-support` | 0.2.100 | MIT OR Apache-2.0 |
| `wasm-bindgen-shared` | 0.2.100 | MIT OR Apache-2.0 |
| `web-sys` | 0.3.77 | MIT OR Apache-2.0 |
| `webpki-roots` | 0.26.11 | CDLA-Permissive-2.0 |
| `webpki-roots` | 1.0.9 | CDLA-Permissive-2.0 |
| `weezl` | 0.1.12 | MIT OR Apache-2.0 |
| `wezterm-bidi` | 0.2.3 | MIT AND Unicode-DFS-2016 |
| `wezterm-blob-leases` | 0.1.1 | MIT |
| `wezterm-color-types` | 0.3.0 | MIT |
| `wezterm-dynamic` | 0.2.1 | MIT |
| `wezterm-dynamic-derive` | 0.1.1 | MIT |
| `wezterm-input-types` | 0.1.0 | MIT |
| `wgpu` | 26.0.1 | MIT OR Apache-2.0 |
| `wgpu-core` | 26.0.1 | MIT OR Apache-2.0 |
| `wgpu-core-deps-apple` | 26.0.0 | MIT OR Apache-2.0 |
| `wgpu-core-deps-emscripten` | 26.0.0 | MIT OR Apache-2.0 |
| `wgpu-core-deps-windows-linux-android` | 26.0.0 | MIT OR Apache-2.0 |
| `wgpu-hal` | 26.0.6 | MIT OR Apache-2.0 |
| `wgpu-types` | 26.0.0 | MIT OR Apache-2.0 |
| `winapi` | 0.3.9 | MIT/Apache-2.0 |
| `winapi-i686-pc-windows-gnu` | 0.4.0 | MIT/Apache-2.0 |
| `winapi-util` | 0.1.11 | Unlicense OR MIT |
| `winapi-x86_64-pc-windows-gnu` | 0.4.0 | MIT/Apache-2.0 |
| `windows` | 0.58.0 | MIT OR Apache-2.0 |
| `windows` | 0.62.2 | MIT OR Apache-2.0 |
| `windows-collections` | 0.3.2 | MIT OR Apache-2.0 |
| `windows-core` | 0.58.0 | MIT OR Apache-2.0 |
| `windows-core` | 0.62.2 | MIT OR Apache-2.0 |
| `windows-future` | 0.3.2 | MIT OR Apache-2.0 |
| `windows-implement` | 0.58.0 | MIT OR Apache-2.0 |
| `windows-implement` | 0.60.2 | MIT OR Apache-2.0 |
| `windows-interface` | 0.58.0 | MIT OR Apache-2.0 |
| `windows-interface` | 0.59.3 | MIT OR Apache-2.0 |
| `windows-link` | 0.2.1 | MIT OR Apache-2.0 |
| `windows-numerics` | 0.3.1 | MIT OR Apache-2.0 |
| `windows-result` | 0.2.0 | MIT OR Apache-2.0 |
| `windows-result` | 0.4.1 | MIT OR Apache-2.0 |
| `windows-strings` | 0.1.0 | MIT OR Apache-2.0 |
| `windows-strings` | 0.5.1 | MIT OR Apache-2.0 |
| `windows-sys` | 0.45.0 | MIT OR Apache-2.0 |
| `windows-sys` | 0.52.0 | MIT OR Apache-2.0 |
| `windows-sys` | 0.61.2 | MIT OR Apache-2.0 |
| `windows-targets` | 0.42.2 | MIT OR Apache-2.0 |
| `windows-targets` | 0.52.6 | MIT OR Apache-2.0 |
| `windows-threading` | 0.2.1 | MIT OR Apache-2.0 |
| `windows_aarch64_gnullvm` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_aarch64_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_aarch64_msvc` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_aarch64_msvc` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_i686_gnu` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_i686_gnu` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_i686_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_i686_msvc` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_i686_msvc` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_x86_64_gnu` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_x86_64_gnu` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_x86_64_gnullvm` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_x86_64_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_x86_64_msvc` | 0.42.2 | MIT OR Apache-2.0 |
| `windows_x86_64_msvc` | 0.52.6 | MIT OR Apache-2.0 |
| `winnow` | 1.0.4 | MIT |
| `winreg` | 0.10.1 | MIT |
| `wit-bindgen` | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `writeable` | 0.6.4 | Unicode-3.0 |
| `x11rb` | 0.13.2 | MIT OR Apache-2.0 |
| `x11rb-protocol` | 0.13.2 | MIT OR Apache-2.0 |
| `xml-rs` | 0.8.29 | MIT |
| `yoke` | 0.8.3 | Unicode-3.0 |
| `yoke-derive` | 0.8.2 | Unicode-3.0 |
| `zerocopy` | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerocopy-derive` | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerofrom` | 0.1.8 | Unicode-3.0 |
| `zerofrom-derive` | 0.1.7 | Unicode-3.0 |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT |
| `zerotrie` | 0.2.5 | Unicode-3.0 |
| `zerovec` | 0.11.7 | Unicode-3.0 |
| `zerovec-derive` | 0.11.4 | Unicode-3.0 |
| `zmij` | 1.0.23 | MIT |
| `zune-core` | 0.5.3 | MIT OR Apache-2.0 OR Zlib |
| `zune-jpeg` | 0.5.15 | MIT OR Apache-2.0 OR Zlib |

## Strudel documentation text

The built-in reference includes descriptions, parameter notes and examples
adapted from the Strudel packages' JSDoc comments
(AGPL-3.0-or-later, https://strudel.cc), alongside documentation written for
Rustel. The adapted text lives in `crates/core/src/control_catalog.rs`,
`crates/core/src/register/`, `crates/jsruntime/src/install/surface.rs` and
`crates/jsruntime/src/install/compat/patterns.rs`.
`UPSTREAM_DOC_REVISION` in `crates/studio/src/reference.rs` records
its source revision. The text is redistributed under the same licence as
the rest of this project.
