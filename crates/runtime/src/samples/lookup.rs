use super::*;

/// Which file of a bank an onset plays, and how far it is transposed.
pub(super) fn pick_from_bank(bank: &Bank, n: f64, midi: f64) -> (Arc<str>, f64) {
    match bank {
        Bank::Array(urls) => {
            let url = urls[sound_index(n, urls.len())].clone();
            // Array banks: `transpose = midi - 36` (C3 anchors).
            (url, midi - 36.0)
        }
        Bank::Notes(notes) => {
            // Closest key wins; strict `<` keeps the FIRST of equals.
            let mut closest: Option<&(f64, Vec<Arc<str>>)> = None;
            for candidate in notes {
                let better = match closest {
                    None => true,
                    Some(current) => (candidate.0 - midi).abs() < (current.0 - midi).abs(),
                };
                if better {
                    closest = Some(candidate);
                }
            }
            let (key_midi, urls) = closest.expect("note banks are non-empty");
            let url = urls[sound_index(n, urls.len())].clone();
            // `transpose = -(noteToMidi(key) - midi)`.
            (url, midi - key_midi)
        }
    }
}

impl FoldedNames {
    fn find(&self, folded: &str) -> Option<&str> {
        self.0
            .iter()
            .find_map(|table| table.get(folded))
            .map(String::as_str)
    }
}

impl ReadyIds<'_> {
    /// The ids already decoded for any of `names`, every variant of each.
    pub fn of<'n>(&mut self, names: impl IntoIterator<Item = &'n str>) -> HashSet<SampleId> {
        self.of_variants(names.into_iter().map(|name| (name, &EVERY_VARIANT)))
    }

    /// The ids already decoded for the variants each name can play: of an
    /// ordinary bank, the files those `n` pick, wrapped and rounded as
    /// playback picks them; of a General MIDI name, those fonts, every
    /// zone of each. A bank the note picks from answers every file
    /// whatever the variants: which note a text plays is not in them.
    pub fn of_variants<'n>(
        &mut self,
        sounds: impl IntoIterator<Item = (&'n str, &'n crate::sounds::Variants)>,
    ) -> HashSet<SampleId> {
        let library = self.library;
        let mut ids = HashSet::new();
        for (name, variants) in sounds {
            if library.names_exactly(name) {
                ids.extend(library.peek_ready_variants(name, variants));
                continue;
            }
            let folded = self.folded.get_or_insert_with(|| library.fold_names());
            // The first table with a folded match is the one `look_up`
            // settles on, and its own spelling is an exact hit there: no
            // earlier table has it, or that table would have matched first.
            match folded.find(&name.to_lowercase()) {
                Some(spelling) => ids.extend(library.peek_ready_variants(spelling, variants)),
                None => {
                    if let Some(found) = local_names::lookup(&library.shared, name, |named| {
                        library.ready_ids_named(named, variants)
                    }) {
                        ids.extend(found);
                    }
                }
            }
        }
        ids
    }
}

/// The entries of `list` that the variants select: every entry for any,
/// else each `n` wrapped into the list with [`sound_index`], as playback
/// wraps it. Each selected entry appears once.
pub(super) fn picked<'a, T>(
    list: &'a [T],
    variants: &crate::sounds::Variants,
) -> impl Iterator<Item = &'a T> {
    let indexes: Option<std::collections::BTreeSet<usize>> = match variants {
        crate::sounds::Variants::All => None,
        crate::sounds::Variants::Only(_) if list.is_empty() => Some(Default::default()),
        crate::sounds::Variants::Only(values) => Some(
            values
                .iter()
                .map(|&n| sound_index(n as f64, list.len()))
                .collect(),
        ),
    };
    let every = if indexes.is_none() { list } else { &list[..0] };
    every
        .iter()
        .chain(indexes.into_iter().flatten().map(move |index| &list[index]))
}

/// A name's first picked variant, or the others: see
/// [`SampleLibrary::load_variants_ahead`].
pub(super) fn split_first<T>(list: &[T], first: bool) -> &[T] {
    let at = list.len().min(1);
    if first { &list[..at] } else { &list[at..] }
}

/// `list` in the order to queue at `priority` so that the loader takes it in
/// list order. The loader takes bets newest first, so bets are queued in
/// reverse. The lowest variant, which a bare name plays, is then fetched
/// first.
pub(super) fn in_queue_order<T>(list: &[T], priority: LoadPriority) -> Vec<&T> {
    let mut ordered: Vec<&T> = list.iter().collect();
    if priority == LoadPriority::Bet {
        ordered.reverse();
    }
    ordered
}

impl SampleLibrary {
    /// Whether `s("name")` can sound as written: a bank this library knows
    /// by its exact name, a General MIDI font, or a native synth. A name a
    /// bank reaches through `.bank()` is not answered here - that spelling
    /// resolves only with the bank on the sound, and the voice refuses a
    /// bare name that a bank's key merely ends with.
    pub fn knows_sound(&self, name: &str) -> bool {
        self.knows(name) || rustel_voice::is_native_synth_sound(name)
    }

    /// The machines among `banks` that do not carry `s("name")`: each is
    /// judged by the banked spelling the voice builds (`{bank}_{name}`),
    /// which playback reads without regard to case. A native synth is
    /// carried by every machine - its dispatch reads the raw name the bank
    /// never touches. The plain spelling is not an answer here: the voice
    /// resolves the banked name and nothing else.
    ///
    /// A bank control is a pattern like any other: `.bank("A B")` alternates
    /// machines across the statement's haps, and `.bank("A,B")` stacks them.
    /// A machine that lacks the voice refuses its own haps while the others
    /// play on, so which machines go without is a question a score can act
    /// on, and only an empty answer says the sound cannot sound at all.
    pub fn banks_missing_sound(&self, name: &str, banks: &[String]) -> Vec<String> {
        if rustel_voice::is_native_synth_sound(name) {
            return Vec::new();
        }
        banks
            .iter()
            .filter(|bank| !self.knows(&format!("{bank}_{name}")))
            .cloned()
            .collect()
    }

    /// How many numbered variants `name:n` can pick from, when `name` is
    /// exactly one bank here - or, for a General MIDI sound, how many
    /// fonts `n` chooses between. A bank-qualified sound returns None: its
    /// count belongs to another name. A pitched bank, its files keyed by note,
    /// also returns None: the note picks a key and `n` wraps within that
    /// key's own files.
    pub fn variants_of(&self, name: &str) -> Option<usize> {
        self.look_up(name, |named| match named {
            Named::Bank(Bank::Array(urls)) => Some(urls.len()),
            Named::Bank(Bank::Notes(_)) => None,
            // A variant count is present only when a font is available.
            Named::Font(fonts) => (!fonts.is_empty()).then_some(fonts.len()),
        })
        .flatten()
    }

    pub(super) fn look_up<T>(&self, name: &str, take: impl FnOnce(Named<'_>) -> T) -> Option<T> {
        look_up_named(
            &self.custom,
            &self.global,
            &self.gm,
            &self.banks,
            &self.shared,
            name,
            take,
        )
    }

    /// Sample ids already decoded for `name`, without starting a fetch.
    ///
    /// Idle eviction uses this to keep what the sounding score can still
    /// hit and drop everything else. Asking `readiness` here would start
    /// loads, which is the opposite of reclaiming RAM.
    pub fn peek_ready_ids(&self, name: &str) -> Vec<SampleId> {
        self.peek_ready_variants(name, &EVERY_VARIANT)
    }

    /// [`Self::peek_ready_ids`] for the variants a text can play of `name`:
    /// see [`ReadyIds::of_variants`].
    fn peek_ready_variants(&self, name: &str, variants: &crate::sounds::Variants) -> Vec<SampleId> {
        self.look_up(name, |named| self.ready_ids_named(named, variants))
            .unwrap_or_default()
    }

    /// Every zone of each decoded font that holds one of `ids`.
    ///
    /// A font is one decode over one run of ids and is forgotten whole -
    /// see [`Self::forget_decoded`] - so a zone kept on its own is not
    /// kept: dropping a sibling takes it too. Whoever keeps a zone keeps
    /// its font.
    pub fn whole_fonts_of(&self, ids: &HashSet<SampleId>) -> Vec<SampleId> {
        if ids.is_empty() {
            return Vec::new();
        }
        let table = self.shared.fonts.read().expect("font table");
        let mut whole = Vec::new();
        for state in table.values() {
            if let FontState::Ready(zones) = state
                && zones.iter().any(|zone| ids.contains(&zone.id))
            {
                whole.extend(zones.iter().map(|zone| zone.id));
            }
        }
        whole
    }

    /// [`Self::peek_ready_ids`] for many names at once, resolved the same
    /// way.
    ///
    /// A name that misses its exact spelling is matched case-insensitively,
    /// which scans every table. The memory policy asks for every sound a
    /// set of tabs names under every bank those tabs name, so most of what
    /// it asks misses: one scan per miss cost it tens of milliseconds a
    /// pass on the thread that feeds the output. Each table is folded once
    /// per call here instead; [`Self::ready_ids`] keeps one folding for
    /// several calls.
    pub fn peek_ready_ids_of<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> HashSet<SampleId> {
        self.ready_ids().of(names)
    }

    /// Ask for the ready ids of several sets of names, folding the tables
    /// at most once between them.
    pub fn ready_ids(&self) -> ReadyIds<'_> {
        ReadyIds {
            library: self,
            folded: None,
        }
    }

    fn names_exactly(&self, name: &str) -> bool {
        self.custom.read().expect("custom banks").contains_key(name)
            || self.global.read().expect("global banks").contains_key(name)
            || self.gm.contains_key(name)
            || self.banks.read().expect("default banks").contains_key(name)
    }

    /// Every table's names, lowercased, in `look_up`'s order. Where two
    /// spellings fold together, the lowercase one wins, as `look_up` tries
    /// it before scanning.
    fn fold_names(&self) -> FoldedNames {
        fn fold<'a>(names: impl Iterator<Item = &'a String>) -> HashMap<String, String> {
            let mut folded = HashMap::new();
            for name in names {
                let lower = name.to_lowercase();
                if lower == *name {
                    folded.insert(lower, name.clone());
                } else {
                    folded.entry(lower).or_insert_with(|| name.clone());
                }
            }
            folded
        }
        FoldedNames(vec![
            fold(self.custom.read().expect("custom banks").keys()),
            fold(self.global.read().expect("global banks").keys()),
            fold(self.gm.keys()),
            fold(self.banks.read().expect("default banks").keys()),
        ])
    }

    fn ready_ids_named(
        &self,
        named: Named<'_>,
        variants: &crate::sounds::Variants,
    ) -> Vec<SampleId> {
        let mut ids = Vec::new();
        match named {
            Named::Font(fonts) => {
                let table = self.shared.fonts.read().expect("font table");
                for font in picked(fonts, variants) {
                    if let Some(FontState::Ready(zones)) = table.get(font) {
                        ids.extend(zones.iter().map(|zone| zone.id));
                    }
                }
            }
            Named::Bank(bank) => {
                let by_url = self.shared.by_url.read().expect("sample url table");
                let ready = |url: &Arc<str>| match by_url.get(url) {
                    Some(UrlState::Ready { id, .. }) => Some(*id),
                    _ => None,
                };
                match bank {
                    Bank::Array(urls) => ids.extend(picked(urls, variants).filter_map(ready)),
                    // The note picks the file here, and no text scan knows
                    // which notes a score will play.
                    Bank::Notes(notes) => ids.extend(
                        notes
                            .iter()
                            .flat_map(|(_, urls)| urls.iter())
                            .filter_map(ready),
                    ),
                }
            }
        }
        ids
    }

    /// Whether `.bank("name")` names a family of banks: something is called
    /// `name_…`. The General MIDI names count: `gm_pad_halo` makes `gm_pad`
    /// a bank, the way the voice's `{bank}_{s}` rule reads it. The name is
    /// read without regard to case, the way playback reads the whole
    /// spelling: `.bank("TR909")` names the machine `tr909_…` holds, and a
    /// check that said otherwise would flag a score that plays.
    pub fn knows_bank(&self, name: &str) -> bool {
        let prefix = format!("{name}_");
        let folded = prefix.to_lowercase();
        let family =
            |known: &str| known.starts_with(&prefix) || known.to_lowercase().starts_with(&folded);
        let starts_with = |names: &HashMap<String, Bank>| names.keys().any(|known| family(known));
        starts_with(&self.banks.read().expect("default banks"))
            || starts_with(&self.custom.read().expect("custom banks"))
            || starts_with(&self.global.read().expect("global banks"))
            || self.gm.keys().any(|known| family(known))
    }

    /// Where one of the browser's sounds lives, as the address of a file.
    ///
    /// `name` resolves the way [`Self::catalogue`] lists it - a score's or
    /// the set's bank first, then a Settings source, then the pinned banks -
    /// and within that bank `variant` picks the numbered sample, with the
    /// first file for none. `bd:3` is the fourth file of `bd`, which is what
    /// a player asking to see that sample expects to find selected, not the
    /// bank's first. A pitched bank answers with the first key holding that
    /// many files; a number past the end answers with the first file. A
    /// font or a synth has no file and answers `None`.
    pub fn file_location(&self, name: &str, variant: Option<usize>) -> Option<String> {
        let index = variant.unwrap_or(0);
        let pick = |bank: &Bank| {
            let url = match bank {
                Bank::Array(urls) => urls.get(index).or_else(|| urls.first()),
                Bank::Notes(notes) => notes
                    .iter()
                    .find_map(|(_, urls)| urls.get(index))
                    .or_else(|| notes.iter().find_map(|(_, urls)| urls.first())),
            };
            url.map(|url| url.to_string())
        };
        self.look_up(name, |named| match named {
            Named::Bank(bank) => pick(bank),
            Named::Font(_) => None,
        })
        .flatten()
    }

    /// True when the name resolves to a bank this library knows.
    pub fn knows(&self, s: &str) -> bool {
        self.look_up(s, |_| ()).is_some()
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    #[cfg(test)]
    mod note_bank_tests {
        //! Which key of a note-keyed bank an equidistant note plays.

        use super::*;

        /// Two keys a whole tone apart, declared in the opposite of their sorted order.
        const TIE_BANK: &str = r#"{"G2": "g2.wav", "A2": "a2.wav"}"#;
        const BASE: &str = "https://samples.example/strumstick/";

        /// A manifest bank gives a tie to the key it declares first, as strudel's
        /// `getCommonSampleInfo` does.
        #[test]
        fn a_manifest_bank_breaks_a_tie_in_declaration_order() {
            let entry: serde_json::Value = serde_json::from_str(TIE_BANK).expect("bank JSON");
            let bank = parse_bank(&entry, BASE).expect("note bank");
            assert_the_tie_plays_g2(&bank);
        }

        /// A score's `samples({...})` bank breaks the same tie the same way.
        #[test]
        fn a_score_bank_breaks_a_tie_in_declaration_order() {
            let entry: serde_json::Value = serde_json::from_str(TIE_BANK).expect("bank JSON");
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_origin("https://samples.example")
                .expect("sample origin");
            let bank = parse_score_bank(&entry, BASE, &access, &mut Vec::new())
                .expect("approved bank")
                .expect("note bank");
            assert_the_tie_plays_g2(&bank);
        }

        /// Midi 44 is a semitone from both G2 (43) and A2 (45).
        fn assert_the_tie_plays_g2(bank: &Bank) {
            let (url, transpose) = pick_from_bank(bank, 0.0, 44.0);
            assert_eq!(&*url, "https://samples.example/strumstick/g2.wav");
            assert_eq!(transpose, 1.0);
        }
    }

    #[cfg(test)]
    pub(in crate::samples) mod variant_tests {
        //! Which of a sound's files are kept and loaded ahead for the variants a
        //! text can play: see [`crate::sounds::Variants`].

        use super::*;
        use crate::sounds::Variants;

        /// A library with no workers: an ordinary bank `takes` of four files, a
        /// bank `keys` the note picks from, a General MIDI name `gm_test` of four
        /// fonts and `gm_other` of two.
        pub(in crate::samples) fn library() -> (SampleLibrary, ManifestJobs) {
            let (manifest_queue, jobs) = manifest_queue::manifest_queue();
            let library = SampleLibrary {
                banks: Arc::new(RwLock::new(HashMap::new())),
                custom: Arc::new(RwLock::new(HashMap::from([
                    (
                        "takes".to_owned(),
                        Bank::Array((0..4).map(|take| take_url(take).into()).collect()),
                    ),
                    (
                        "keys".to_owned(),
                        Bank::Notes(vec![
                            (
                                36.0,
                                vec![Arc::from("keys/c2.wav"), Arc::from("keys/c2b.wav")],
                            ),
                            (48.0, vec![Arc::from("keys/c3.wav")]),
                        ]),
                    ),
                ]))),
                global: Arc::new(RwLock::new(HashMap::new())),
                gm: Arc::new(HashMap::from([
                    (
                        "gm_test".to_owned(),
                        ["font0", "font1", "font2", "font3"]
                            .map(Arc::<str>::from)
                            .to_vec(),
                    ),
                    (
                        "gm_other".to_owned(),
                        ["other0", "other1"].map(Arc::<str>::from).to_vec(),
                    ),
                ])),
                shared: Arc::new(crate::samples::codec_tests::sample_test_shared(1)),
                manifest_queue,
                manifest_order: Mutex::new(()),
                bus_names: rustel_voice::BusNames::default(),
            };
            (library, jobs)
        }

        fn take_url(take: usize) -> String {
            format!("takes/{take}.wav")
        }

        fn only(values: &[i64]) -> Variants {
            Variants::Only(values.iter().copied().collect())
        }

        /// Every file of both banks decoded, `takes` at ids 10-13 and `keys` at
        /// 20-22; `font1` and `font2` decoded at two zones each, 31-32 and 33-34.
        fn all_decoded(library: &SampleLibrary) {
            let mut by_url = library.shared.by_url.write().unwrap();
            let files = (0..4)
                .map(take_url)
                .chain(["keys/c2.wav", "keys/c2b.wav", "keys/c3.wav"].map(str::to_owned))
                .zip([10, 11, 12, 13, 20, 21, 22]);
            for (url, id) in files {
                by_url.insert(
                    Arc::from(url),
                    UrlState::Ready {
                        id: SampleId(id),
                        duration_secs: 1.0,
                    },
                );
            }
            let zone = |id: u32, key_lo: f64| FontZone {
                key_lo,
                key_hi: key_lo + 63.0,
                base_detune: 6000.0,
                id: SampleId(id),
                duration_secs: 1.0,
                loop_secs: None,
            };
            let mut fonts = library.shared.fonts.write().unwrap();
            fonts.insert(
                Arc::from("font1"),
                FontState::Ready(Arc::new(vec![zone(31, 0.0), zone(32, 64.0)])),
            );
            fonts.insert(
                Arc::from("font2"),
                FontState::Ready(Arc::new(vec![zone(33, 0.0), zone(34, 64.0)])),
            );
        }

        fn ids(values: &[u32]) -> HashSet<SampleId> {
            values.iter().copied().map(SampleId).collect()
        }

        /// A variant of an ordinary bank keeps the one file that `n` selects. The
        /// index wraps at both ends as in playback, and the name matches in any
        /// case.
        #[test]
        fn an_ordinary_bank_keeps_the_files_its_variants_pick() {
            let (library, _receiver) = library();
            all_decoded(&library);
            let mut ready = library.ready_ids();
            let kept = |ready: &mut ReadyIds<'_>, name: &str, variants: &Variants| {
                ready.of_variants([(name, variants)])
            };
            assert_eq!(kept(&mut ready, "takes", &only(&[0])), ids(&[10]));
            assert_eq!(kept(&mut ready, "takes", &only(&[0, 2])), ids(&[10, 12]));
            assert_eq!(
                kept(&mut ready, "takes", &only(&[5])),
                ids(&[11]),
                "wrapped"
            );
            assert_eq!(
                kept(&mut ready, "takes", &only(&[-1])),
                ids(&[13]),
                "the last"
            );
            assert_eq!(
                kept(&mut ready, "takes", &only(&[Variants::index(1.5)])),
                ids(&[12]),
                "rounded first"
            );
            assert_eq!(kept(&mut ready, "TAKES", &only(&[3])), ids(&[13]));
            assert_eq!(
                kept(&mut ready, "takes", &Variants::All),
                ids(&[10, 11, 12, 13])
            );
            assert_eq!(
                ready.of(["takes"]),
                ids(&[10, 11, 12, 13]),
                "a name asked for whole is every file"
            );
        }

        /// The note picks among a pitched bank's files, and no variant says which
        /// notes a text plays: every file is kept.
        #[test]
        fn a_bank_the_note_picks_from_keeps_every_file() {
            let (library, _receiver) = library();
            all_decoded(&library);
            assert_eq!(
                library.ready_ids().of_variants([("keys", &only(&[0]))]),
                ids(&[20, 21, 22])
            );
        }

        /// Of a General MIDI name, a variant keeps its font, every zone of it,
        /// and no other font.
        #[test]
        fn a_font_variant_keeps_that_fonts_zones() {
            let (library, _receiver) = library();
            all_decoded(&library);
            let mut ready = library.ready_ids();
            assert_eq!(
                ready.of_variants([("gm_test", &only(&[1]))]),
                ids(&[31, 32])
            );
            assert_eq!(
                ready.of_variants([("gm_test", &only(&[0]))]),
                HashSet::new(),
                "font0 is not decoded"
            );
            assert_eq!(
                ready.of_variants([("gm_test", &Variants::All)]),
                ids(&[31, 32, 33, 34])
            );
            assert_eq!(
                library
                    .whole_fonts_of(&ids(&[33]))
                    .into_iter()
                    .collect::<HashSet<_>>(),
                ids(&[33, 34]),
                "a zone kept keeps its font"
            );
        }

        /// A text with no `n` warms the first file, for an ordinary bank and for a
        /// bank that the note selects from. A text that names takes warms those
        /// takes. A text that can play any take warms only the first file, which a
        /// bare name plays. Other takes load when they are about to sound.
        #[test]
        fn a_warm_loads_the_first_file_unless_the_text_names_takes() {
            let requested = |source: &str| {
                let (library, _receiver) = library();
                let status = library
                    .warm_score_sounds_async(
                        &crate::sounds::to_warm(source, false),
                        &ScoreSampleAccess::denied(),
                    )
                    .unwrap();
                let mut asked: Vec<String> = library
                    .shared
                    .by_url
                    .read()
                    .unwrap()
                    .keys()
                    .map(|url| url.to_string())
                    .collect();
                asked.sort();
                (status, asked)
            };
            assert_eq!(
                requested(r#"s("takes")"#),
                (PrefetchStatus::Requested(1), vec![take_url(0)])
            );
            assert_eq!(
                requested(r#"s("takes:-1").n("<0 2>")"#),
                (
                    PrefetchStatus::Requested(3),
                    vec![take_url(0), take_url(2), take_url(3)]
                )
            );
            assert_eq!(
                requested(r#"s("takes").n(irand(4))"#),
                (PrefetchStatus::Requested(1), vec![take_url(0)])
            );
            assert_eq!(
                requested(r#"s("keys")"#),
                (PrefetchStatus::Requested(1), vec!["keys/c2.wav".to_owned()])
            );

            // A player's own `preload("keys:0")` keeps its old reading.
            let (library, _receiver) = library();
            assert_eq!(library.prefetch("keys:0"), 1);
        }

        /// A kept tab is loaded ahead at the takes its variants pick. A bank the
        /// note picks from is the same list, not every key, and a name that can
        /// play any loads its first file.
        #[test]
        fn loading_ahead_asks_for_the_takes_a_text_names() {
            let (library, _receiver) = library();
            assert_eq!(library.load_variants_ahead([("takes", &only(&[1, 5]))]), 1);
            assert_eq!(library.load_variants_ahead([("keys", &only(&[0]))]), 1);
            assert_eq!(library.load_variants_ahead([("gm_test", &only(&[2]))]), 1);
            assert_eq!(
                library.load_variants_ahead([("nothing_here", &Variants::All)]),
                0
            );
            let asked: HashSet<String> = library
                .shared
                .by_url
                .read()
                .unwrap()
                .keys()
                .map(|url| url.to_string())
                .collect();
            assert_eq!(
                asked,
                [take_url(1), "keys/c2.wav".into()].into_iter().collect()
            );
            assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font2");
            assert!(library.shared.font_jobs.try_pop().is_none());

            assert_eq!(
                library.load_variants_ahead([("gm_test", &Variants::All)]),
                1
            );
            assert_eq!(
                &*library.shared.font_jobs.try_pop().unwrap(),
                "font0",
                "a name that can play any loads its first font"
            );
            assert!(library.shared.font_jobs.try_pop().is_none());
        }

        /// Fonts are fetched one at a time and bets come off the line newest
        /// first, so what loads ahead is queued for the first variant of every
        /// name to come off before any other: a tab that can play every font of
        /// one name does not hold back the first font of another, and its own
        /// first comes before its others.
        #[test]
        fn loading_ahead_fetches_every_names_first_variant_before_the_others() {
            let (library, _receiver) = library();
            assert_eq!(
                library.load_variants_ahead([
                    ("gm_test", &only(&[0, 1, 2, 3])),
                    ("gm_other", &only(&[0, 1])),
                ]),
                6
            );
            let order: Vec<String> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                .map(|font| font.to_string())
                .collect();
            assert_eq!(
                order,
                ["other0", "font0", "other1", "font1", "font2", "font3"]
            );
        }

        /// A live warm asks for every variant of a name in one ask, and comes off
        /// the line lowest first: the first font is the one a bare name, and a
        /// scale's melody, plays.
        #[test]
        fn a_warm_asks_for_many_variants_of_a_name_at_once_lowest_first() {
            let (library, _receiver) = library();
            assert_eq!(
                library
                    .warm_score_sounds_async(
                        &["gm_test:2,0,-1".into(), "takes:1,3".into()],
                        &ScoreSampleAccess::denied()
                    )
                    .unwrap(),
                PrefetchStatus::Requested(5)
            );
            let order: Vec<String> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                .map(|font| font.to_string())
                .collect();
            assert_eq!(order, ["font0", "font2", "font3"]);
            let asked: HashSet<String> = library
                .shared
                .by_url
                .read()
                .unwrap()
                .keys()
                .map(|url| url.to_string())
                .collect();
            assert_eq!(asked, [take_url(1), take_url(3)].into_iter().collect());

            // A player's own `preload` does not read a list: the whole bank.
            let (preloading, _receiver) = self::library();
            assert_eq!(preloading.prefetch("takes:1,3"), 4);
        }
    }
}
