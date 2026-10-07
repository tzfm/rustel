//! Sample banks, soundfonts, and score-selected sample sources.
//!
//! Default bank manifests are pinned in `assets/sample-banks.json` by URL,
//! caller base, and SHA-256. The manifests are cached on disk and verified
//! against their pins. Background loaders fetch and decode sample files on
//! demand, then deliver them through [`SampleLibrary::take_ready`].
//!
//! Name resolution rules:
//! - `n` selects a sound: rounded half-up (`js_round`), NaN as 0, wrapped
//!   Euclidean-mod into the bank's length;
//! - array banks transpose from the hap's midi against C3=36, note-keyed
//!   banks pick the closest key and repitch by the difference;
//! - a sound that is not ready at onset time is skipped and can play on a
//!   later trigger once loaded.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, Weak, mpsc};
use std::time::{Duration, Instant};

use rustel_audio::{
    BUNDLED_BD_SAMPLE_ID, BUNDLED_BD_SAMPLE_IDENTITY, DecodedSample, SAMPLE_BANK_CAPACITY, SampleId,
};
use rustel_voice::{SampleLookup, SampleResolution};
use sha2::{Digest, Sha256};
use url::Url;

use crate::product;

mod codecs;
mod folders;
mod font_queue;
mod local_names;
mod manifest_queue;
mod soundfonts;
use codecs::{codec_for, decode_guarded};
#[cfg(test)]
use codecs::{decode_mp3, decode_ogg, guard_vorbis_setup, vorbis_lookup1_values};
pub use folders::without_verbatim_prefix;
#[cfg(test)]
use folders::{
    PendingSampleDirectory, sample_scan_entry_bytes, sample_scan_path_bytes,
    sample_scan_stack_slots,
};
use folders::{folder_banks, local_file_url, scan_score_sample_folder, set_folder_banks};
pub use folders::{is_sample_audio, scan_sample_folder};
pub(crate) use folders::{sample_audio_kind, scan_sample_folder_with_limits};
use font_queue::FontQueue;
use manifest_queue::{ManifestJobs, ManifestQueue};
use soundfonts::load_font;
#[cfg(test)]
use soundfonts::{decode_font, fold_number_arithmetic};
#[cfg(all(test, unix))]
mod cache_read_tests {
    //! The plain-cache readers open entries through
    //! [`open_regular_cache_entry`], so a link at an entry name is refused.

    use std::os::unix::fs::symlink;

    use super::*;

    /// Pins that both readers refuse a symlink at a cache entry name without
    /// reading its target, and still read a regular entry.
    #[test]
    fn cache_reads_refuse_a_symlink_at_the_entry_name() {
        let dir = tempfile::tempdir().expect("cache root");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"secret").expect("victim");

        let sample = cache_path(dir.path(), "https://samples.example/kick.wav");
        symlink(&victim, &sample).expect("plant sample symlink");
        let error = read_sample_cache(&sample).expect_err("a sample symlink is refused");
        assert!(error.contains("not a regular file"), "{error}");

        let manifest = cache_path(dir.path(), "https://samples.example/kit.json");
        symlink(&victim, &manifest).expect("plant manifest symlink");
        let error = read_manifest_cache(&manifest).expect_err("a manifest symlink is refused");
        assert!(error.contains("not a regular file"), "{error}");
        assert_eq!(std::fs::read(&victim).expect("victim remains"), b"secret");

        let honest = cache_path(dir.path(), "https://samples.example/snare.wav");
        std::fs::write(&honest, b"sample").expect("regular sample entry");
        assert_eq!(read_sample_cache(&honest).expect("sample entry"), b"sample");
        std::fs::write(&honest, b"{}").expect("regular manifest entry");
        assert_eq!(read_manifest_cache(&honest).expect("manifest entry"), b"{}");
    }
}
#[cfg(test)]
mod empty_font_tests {
    //! Empty General MIDI font lists have no playable variants.

    use super::*;

    #[test]
    fn an_empty_font_list_counts_no_variants_and_resolves_to_nothing() {
        let (manifest_queue, _jobs) = manifest_queue::manifest_queue();
        let library = SampleLibrary {
            banks: Arc::new(RwLock::new(HashMap::new())),
            custom: Arc::new(RwLock::new(HashMap::new())),
            global: Arc::new(RwLock::new(HashMap::new())),
            gm: Arc::new(HashMap::from([(
                "gm_empty".to_owned(),
                Vec::<Arc<str>>::new(),
            )])),
            shared: Arc::new(super::codec_tests::sample_test_shared(1)),
            manifest_queue,
            manifest_order: Mutex::new(()),
        };
        assert_eq!(
            library.variants_of("gm_empty"),
            None,
            "an empty list has no variants to count"
        );
        assert!(
            matches!(
                library.resolve("gm_empty", 1.0, 60.0),
                SampleResolution::Failed
            ),
            "resolving against no fonts fails instead of panicking"
        );
        assert_eq!(sound_index(3.0, 0), 0, "the wrap itself is div-safe");
    }
}
#[cfg(test)]
mod host_cache_eviction_tests {
    //! A host-cache entry whose decode or parse fails is evicted, so the next
    //! ask fetches it again; see [`evict_host_cache_entry`].

    use std::io::Write;
    use std::net::TcpListener;

    use super::codec_tests::{sample_test_shared, two_zone_test_font};
    use super::manifest_worker_tests::{drain_request, tiny_wav};
    use super::*;

    const IDLE: Duration = Duration::from_secs(5);

    /// Answer one request per body, in order, each with a 200, at a url ending
    /// in `name` that no earlier process can have cached.
    fn serve(name: &str, bodies: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let url = format!(
            "http://{}/{}-{unique}/{name}",
            listener.local_addr().expect("listener address"),
            std::process::id(),
        );
        let server = std::thread::spawn(move || {
            for body in bodies {
                let (stream, _) = listener.accept().expect("a request");
                let mut stream = drain_request(stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("response head");
                stream.write_all(&body).expect("response body");
            }
        });
        (url, server)
    }

    /// Pins that the sample loader evicts a trusted body that does not decode,
    /// whether it was fetched now or read from disk, and that the retry after
    /// the rest fetches again and keeps a body that decodes.
    #[test]
    fn a_sample_that_does_not_decode_is_evicted_and_fetched_again() {
        let dir = tempfile::tempdir().expect("cache root");
        let (url, server) = serve(
            "bd.wav",
            vec![b"<html>not audio</html>".to_vec(), tiny_wav()],
        );
        let url: Arc<str> = Arc::from(url);
        let entry = cache_path(dir.path(), &url);
        let new_library = || {
            SampleLibrary::with_background_loaders_at(
                HashMap::new(),
                Vec::new(),
                HashMap::new(),
                String::new(),
                dir.path().to_path_buf(),
                Loading::InBackground,
            )
            .expect("library")
        };
        let ask = |library: &SampleLibrary| {
            ensure_loading_shared(
                &library.shared,
                &url,
                DecodeRate::Context,
                LoadPriority::Now,
            );
            library.wait_until_idle(IDLE);
            match library.shared.by_url.read().expect("url table").get(&url) {
                Some(UrlState::Ready { .. }) => Ok(()),
                Some(UrlState::Failed { .. }) => Err(()),
                _ => panic!("the load did not settle"),
            }
        };

        let library = new_library();
        assert!(ask(&library).is_err(), "an HTML page is not audio");
        assert!(
            !entry.exists(),
            "a fetched body that does not decode is evicted"
        );

        let rested = Instant::now() - FAILED_RETRY_AFTER - Duration::from_secs(1);
        library
            .shared
            .by_url
            .write()
            .expect("url table")
            .insert(url.clone(), UrlState::Failed { at: rested });
        assert!(
            ask(&library).is_ok(),
            "the retry after the rest reaches the server again"
        );
        server.join().expect("server");
        assert!(entry.exists(), "a body that decodes is kept");

        std::fs::write(&entry, b"corrupted").expect("corrupt the entry");
        assert!(
            ask(&new_library()).is_err(),
            "a later library does not serve a corrupt entry"
        );
        assert!(
            !entry.exists(),
            "a stored entry that does not decode is evicted"
        );
    }

    /// Pins that a soundfont body that does not decode is evicted, while a font
    /// refused only because the bank is full keeps its bytes.
    #[test]
    fn a_font_that_does_not_decode_is_evicted_but_one_the_bank_cannot_seat_is_kept() {
        let dir = tempfile::tempdir().expect("cache root");
        let url = "http://127.0.0.1:9/0000_Test.js";
        let entry = cache_path(dir.path(), url);
        let budget = sample_fetch::FetchBudget::for_one_fetch();
        let shared = sample_test_shared(1);

        std::fs::write(&entry, b"<html>not a font</html>").expect("a bad entry");
        assert!(
            load_font(dir.path(), url, &shared, &budget).is_err(),
            "an HTML page is not a font"
        );
        assert!(!entry.exists(), "bytes that do not decode are evicted");

        std::fs::write(&entry, two_zone_test_font()).expect("a good entry");
        let full = sample_test_shared((SAMPLE_BANK_CAPACITY - 1) as u32);
        let Err(error) = load_font(dir.path(), url, &full, &budget) else {
            panic!("two zones cannot fit in one remaining slot");
        };
        assert!(error.contains("sample bank capacity"), "{error}");
        assert!(
            entry.exists(),
            "a font the bank cannot seat keeps its bytes"
        );

        let zones = load_font(dir.path(), url, &shared, &budget).expect("the kept font installs");
        assert_eq!(zones.len(), 2);
    }

    /// Pins that a trusted `samples()` map whose body is not JSON leaves no
    /// host-cache entry, so registering it again fetches the map anew.
    #[test]
    fn a_manifest_that_is_not_json_is_evicted_and_fetched_again() {
        let (url, server) = serve(
            "strudel.json",
            vec![
                b"<html>not a map</html>".to_vec(),
                br#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_vec(),
            ],
        );
        let map = serde_json::to_string(&url).expect("a quoted url");
        let entry = cache_path(&cache_dir(), &url);
        let library = SampleLibrary::empty();

        let error = library
            .register_trusted_custom(&map, None)
            .expect_err("an HTML page is not a map");
        assert!(error.contains("not JSON"), "{error}");
        assert!(!entry.exists(), "a body that is not JSON is evicted");

        library
            .register_trusted_custom(&map, None)
            .expect("the next registration fetches the map again");
        server.join().expect("server");
        library.wait_until_idle(IDLE);
        assert!(library.knows("tone"), "the map fetched again registers");
        assert!(entry.exists(), "a body that is JSON is kept");
        let _ = std::fs::remove_file(entry);
    }
}
#[cfg(test)]
mod live_warm_tests {
    use super::*;

    /// A library with its queues but no workers; the test holds the manifest
    /// worker's end of the line.
    pub(super) fn library() -> (SampleLibrary, ManifestJobs) {
        let (manifest_queue, jobs) = manifest_queue::manifest_queue();
        let library = SampleLibrary {
            banks: Arc::new(RwLock::new(HashMap::new())),
            custom: Arc::new(RwLock::new(HashMap::new())),
            global: Arc::new(RwLock::new(HashMap::new())),
            gm: Arc::new(HashMap::from([(
                "gm_test".to_owned(),
                ["font0", "font1", "font2", "font3"]
                    .map(Arc::<str>::from)
                    .to_vec(),
            )])),
            shared: Arc::new(super::codec_tests::sample_test_shared(1)),
            manifest_queue,
            manifest_order: Mutex::new(()),
        };
        (library, jobs)
    }

    #[test]
    fn live_warm_requests_one_gm_variant_but_explicit_preload_keeps_all() {
        let (library, _receiver) = library();
        assert_eq!(
            library
                .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                .unwrap(),
            PrefetchStatus::Requested(1)
        );
        assert_eq!(library.shared.fonts.read().unwrap().len(), 1);
        assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font0");

        // Asking explicitly still covers all variants, including an already
        // queued/decoded one in its public count, exactly like ordinary prefetch.
        assert_eq!(library.prefetch("gm_test"), 4);
        assert_eq!(library.shared.fonts.read().unwrap().len(), 4);
        let remaining: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
            .map(|font| font.to_string())
            .collect();
        assert_eq!(remaining, ["font1", "font2", "font3"]);
    }

    #[test]
    fn gm_preload_and_live_warm_use_numeric_sound_indices() {
        for (index, expected) in [
            ("2", "font2"),
            ("-1", "font3"),
            ("1.5", "font2"),
            ("-0.5", "font0"),
            ("6", "font2"),
        ] {
            for intent in [PrefetchIntent::Explicit, PrefetchIntent::LiveWarm] {
                let (library, _receiver) = library();
                let spec = format!("gm_test:{index}");
                let result = match intent {
                    PrefetchIntent::Explicit => PrefetchStatus::Requested(library.prefetch(&spec)),
                    PrefetchIntent::LiveWarm => library
                        .warm_score_sounds_async(&[spec], &ScoreSampleAccess::denied())
                        .unwrap(),
                    PrefetchIntent::CacheDisk => unreachable!("not in this loop"),
                };
                assert_eq!(result, PrefetchStatus::Requested(1));
                assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), expected);
                assert!(library.shared.font_jobs.try_pop().is_none());
            }
        }
    }

    #[test]
    fn actual_gm_resolution_promotes_a_warmed_variant_without_duplicate_jobs() {
        let (library, _receiver) = library();
        library
            .warm_score_sounds_async(
                &["gm_test:0".into(), "gm_test:1".into(), "gm_test:3".into()],
                &ScoreSampleAccess::denied(),
            )
            .unwrap();
        assert!(matches!(
            SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
            SampleResolution::Loading
        ));
        for _ in 0..2 {
            assert!(matches!(
                SampleLookup::resolve(&library, "gm_test", 0.0, 60.0),
                SampleResolution::Loading
            ));
        }
        let order: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
            .map(|font| font.to_string())
            .collect();
        assert_eq!(order, ["font2", "font0", "font3", "font1"]);
    }

    #[test]
    fn selected_gm_retry_keeps_cooldown_and_precedes_speculative_fonts() {
        let (library, _receiver) = library();
        let failed: Arc<str> = Arc::from("font2");
        library
            .shared
            .fonts
            .write()
            .unwrap()
            .insert(failed.clone(), FontState::Failed { at: Instant::now() });
        assert_eq!(library.prefetch("gm_test:1.5"), 1);
        assert!(
            library.shared.font_jobs.try_pop().is_none(),
            "fresh failure rests"
        );
        assert!(matches!(
            SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
            SampleResolution::Failed
        ));

        library.shared.fonts.write().unwrap().insert(
            failed.clone(),
            FontState::Failed {
                at: Instant::now() - FAILED_RETRY_AFTER,
            },
        );
        library
            .warm_score_sounds_async(
                &["gm_test:2".into(), "gm_test:3".into()],
                &ScoreSampleAccess::denied(),
            )
            .unwrap();
        assert!(
            matches!(
                library.shared.fonts.read().unwrap().get(&failed),
                Some(FontState::Failed { .. })
            ),
            "live speculation does not retry even a rested failure"
        );

        assert_eq!(library.prefetch("gm_test:1.5"), 1);
        assert!(matches!(
            SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
            SampleResolution::Loading
        ));
        let order: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
            .map(|font| font.to_string())
            .collect();
        assert_eq!(
            order,
            ["font2", "font3"],
            "retry is unique and precedes bets"
        );
        assert_eq!(
            library.shared.fonts.read().unwrap().len(),
            2,
            "selected retry did not load other variants"
        );
    }

    #[test]
    fn queued_gm_retry_reuses_only_forgotten_and_released_font_slots() {
        let (library, _receiver) = library();
        let font = br#"var fixture={zones:[{originalPitch:6000,keyRangeLow:0,keyRangeHigh:63,sampleRate:48000,sample:'AAAAAA=='},{originalPitch:6000,keyRangeLow:64,keyRangeHigh:127,sampleRate:48000,sample:'AAAAAA=='}]};"#;
        let old_zones = decode_font(font, &library.shared).unwrap();
        let old_ids: Vec<_> = old_zones.iter().map(|zone| zone.id).collect();
        library
            .shared
            .fonts
            .write()
            .unwrap()
            .insert(Arc::from("font0"), FontState::Ready(Arc::new(old_zones)));
        let _ = library.take_ready();
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);

        // A published font still owns these ids. Releasing before forgetting it
        // must not make the new font decode over a potentially sounding voice.
        library.release_ids(old_ids.iter().copied());
        assert_eq!(library.free_id_count(), 0);
        assert!(
            decode_font(font, &library.shared)
                .err()
                .unwrap()
                .contains("sample bank capacity")
        );
        library.shared.fonts.write().unwrap().insert(
            Arc::from("font2"),
            FontState::Failed {
                at: Instant::now() - FAILED_RETRY_AFTER,
            },
        );

        let mut forgotten = library.forget_decoded(&HashSet::from([old_ids[0]]));
        forgotten.sort_by_key(|id| id.0);
        assert_eq!(
            forgotten, old_ids,
            "one retired zone retires its whole font"
        );
        assert_eq!(
            library.free_id_count(),
            0,
            "forgetting alone does not permit reuse"
        );
        library.release_ids(forgotten.iter().copied());
        library.release_ids(forgotten);
        assert_eq!(library.free_id_count(), 2, "duplicate release is inert");

        library
            .warm_score_sounds_async(&["gm_test:3".into()], &ScoreSampleAccess::denied())
            .unwrap();
        assert!(matches!(
            SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
            SampleResolution::Loading
        ));
        let retry = library.shared.font_jobs.try_pop().unwrap();
        assert_eq!(
            &*retry, "font2",
            "rested retry precedes the speculative variant"
        );
        let zones = decode_font(font, &library.shared).unwrap();
        assert_eq!(
            zones.iter().map(|zone| zone.id).collect::<Vec<_>>(),
            old_ids
        );
        library
            .shared
            .fonts
            .write()
            .unwrap()
            .insert(retry, FontState::Ready(Arc::new(zones)));
        assert!(matches!(
            SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
            SampleResolution::Found { id, .. } if id == old_ids[0]
        ));
        assert_eq!(
            library.shared.next_id.load(Ordering::Relaxed),
            SAMPLE_BANK_CAPACITY as u32
        );
        assert_eq!(library.free_id_count(), 0);
        assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font3");
        assert!(library.shared.font_jobs.try_pop().is_none());
    }

    #[test]
    fn live_warm_of_a_bare_name_loads_the_first_file_of_an_overriding_bank() {
        let (library, _receiver) = library();
        library.global.write().unwrap().insert(
            "gm_test".into(),
            Bank::Array(vec![
                Arc::from("https://example.com/a.wav"),
                Arc::from("https://example.com/b.wav"),
            ]),
        );
        assert_eq!(
            library
                .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                .unwrap(),
            PrefetchStatus::Requested(1)
        );
        assert!(library.shared.fonts.read().unwrap().is_empty());
        let state = library.shared.jobs.state.lock().unwrap();
        assert!(state.now.is_empty());
        assert_eq!(state.bets.len(), 1);
        assert_eq!(&*state.bets[0].url, "https://example.com/a.wav");
    }

    #[test]
    fn live_warm_waits_for_an_overriding_manifest_before_selecting_gm_or_bank() {
        let (library, receiver) = library();
        let context = library.manifest_context();
        let worker = std::thread::spawn(move || run_manifest_worker(context, receiver));
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        library
            .enqueue_manifest_work_async(ManifestWork::Custom {
                effects: vec![(
                    r#"{"gm_test":["https://example.com/new-a.wav","https://example.com/new-b.wav"]}"#
                        .into(),
                    None,
                )],
                preloads: Vec::new(),
                intent: PrefetchIntent::Explicit,
                access: ManifestAccess::Trusted,
                continue_on_error: false,
                layer: BankLayer::Score,
            })
            .unwrap();
        reached.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            library
                .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                .unwrap(),
            PrefetchStatus::Deferred
        );
        assert!(library.shared.fonts.read().unwrap().is_empty());
        assert!(library.shared.by_url.read().unwrap().is_empty());
        release.send(()).unwrap();
        // FIFO completion proves the live warm has finished without waiting for
        // the deliberately worker-less audio queues to decode anything.
        library
            .enqueue_manifest_work_blocking(ManifestWork::Custom {
                effects: Vec::new(),
                preloads: Vec::new(),
                intent: PrefetchIntent::Explicit,
                access: ManifestAccess::Trusted,
                continue_on_error: false,
                layer: BankLayer::Score,
            })
            .unwrap();
        assert!(library.shared.fonts.read().unwrap().is_empty());
        let urls = library.shared.by_url.read().unwrap();
        assert_eq!(urls.len(), 1);
        assert!(urls.contains_key("https://example.com/new-a.wav"));
        drop(urls);
        let state = library.shared.jobs.state.lock().unwrap();
        assert!(state.now.is_empty(), "deferred warm lost its Bet intent");
        assert_eq!(state.bets.len(), 1);
        drop(state);
        drop(library);
        worker.join().unwrap();
    }

    #[test]
    fn dropping_a_library_closes_its_idle_font_queue() {
        let (library, _receiver) = library();
        let jobs = Arc::clone(&library.shared.font_jobs);
        drop(library);
        assert!(jobs.pop().is_none());
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn live_session_warms_evaluated_gm_variants_before_source_only_bets() {
        for (source, from_cycle, expected) in [
            ("n(2).s('gm_test')", 0.0, vec!["font2", "font0"]),
            ("s('gm_test:3')", 0.0, vec!["font3"]),
            (
                r#"n("<0 1 2 3>").s("gm_test").slow(4)"#,
                8.0,
                vec!["font2", "font0"],
            ),
        ] {
            let (library, _receiver) = library();
            let library = Arc::new(library);
            let mut session = crate::Session::new().unwrap();
            session.set_direct_diagnostic_logging(false);
            session.set_sample_library_for_test(Arc::clone(&library));
            session.evaluate(source).unwrap();
            session
                .warm_live_samples_checked(
                    &crate::sounds::in_score(source),
                    from_cycle,
                    Duration::from_millis(50),
                )
                .1
                .unwrap();
            let jobs: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                .map(|font| font.to_string())
                .collect();
            assert_eq!(jobs, expected, "{source} at cycle {from_cycle}");
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn live_session_queries_dynamic_sound_names_even_without_literal_bets() {
        let (library, _receiver) = library();
        let library = Arc::new(library);
        let mut session = crate::Session::new().unwrap();
        session.set_direct_diagnostic_logging(false);
        session.set_sample_library_for_test(Arc::clone(&library));
        session
            .evaluate("const name = 'gm_' + 'test'; n(2).s(name)")
            .unwrap();
        let (window, warmed) =
            session.warm_live_samples_checked(&[], 0.0, Duration::from_millis(50));
        warmed.unwrap();
        assert_eq!(
            window,
            [(
                "gm_test".to_owned(),
                crate::sounds::Variants::Only([2].into())
            )],
            "the window answers the name the score built"
        );
        assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font2");
        assert!(library.shared.font_jobs.try_pop().is_none());
    }

    /// Speculative warming pairs each bank name with each sound name in the
    /// source. Many pairs do not exist, so these misses produce no warning.
    /// An explicit `preload()` reports missing names because the user requested them.
    #[test]
    fn a_warms_miss_is_quiet_and_an_explicit_preloads_miss_is_not() {
        let (library, _receiver) = library();

        assert_eq!(
            library
                .warm_score_sounds_async(
                    &["korgminipops_sawtooth".into(), "AkaiLinn_kik:1".into()],
                    &ScoreSampleAccess::denied(),
                )
                .unwrap(),
            PrefetchStatus::Requested(0),
            "neither pairing exists"
        );
        assert!(
            library.take_failures().is_empty(),
            "a bet that missed is how the bet works, not a fault in it"
        );

        // The same library, the same absent name, asked for on purpose.
        library
            .register_trusted_batch_async_for_test(Vec::new(), &["korgminipops_sawtooth".into()])
            .expect("the batch is admitted");
        let said = library.take_failures();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].contains("korgminipops_sawtooth"),
            "and it names what was not found: {said:?}"
        );

        // What the warm CAN find it still loads, quietly.
        assert_eq!(
            library
                .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                .unwrap(),
            PrefetchStatus::Requested(1)
        );
        assert!(library.take_failures().is_empty());
    }
}
#[cfg(test)]
mod manifest_congestion_tests {
    //! A busy manifest loader keeps a job waiting in line rather than turning it
    //! away; only the line's bound refuses, and a refusal is a failure. See
    //! [`ManifestQueue`].

    use super::manifest_queue::MANIFEST_QUEUE_JOBS;
    use super::*;

    const SPEC: &str = "github:me/pack";

    /// A score grant that lets `github:` maps be fetched.
    pub(super) fn access() -> ScoreSampleAccess {
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://raw.githubusercontent.com")
            .expect("origin");
        access
    }

    fn inline(name: &str) -> String {
        format!(r#"{{"{name}":"http://127.0.0.1:9/{name}.wav"}}"#)
    }

    /// Put `count` inline registrations in a line nothing drains.
    fn park(library: &SampleLibrary, count: usize) {
        for slot in 0..count {
            library
                .register_trusted_batch_async_for_test(
                    vec![(inline(&format!("slot{slot}")), None)],
                    &[],
                )
                .expect("a busy loader parks the job");
        }
    }

    fn quoted(spec: &str) -> String {
        serde_json::to_string(spec).expect("a string")
    }

    /// What a job asks for, in a word, for reading the line's order.
    fn describe(job: &ManifestJob) -> String {
        match &job.work {
            ManifestWork::Defaults { .. } => "defaults".to_owned(),
            ManifestWork::Folders { specs, .. } => format!("folders {}", specs.join(" ")),
            ManifestWork::Custom {
                effects, preloads, ..
            } => effects
                .iter()
                .map(|(map, _)| map.clone())
                .chain(preloads.iter().map(|spec| format!("preload {spec}")))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// A look-up behind a busy loader waits in line: its spec reads loading,
    /// the loader counts it as pending, and the checker has nothing to refuse.
    #[test]
    fn a_look_up_behind_a_busy_loader_waits_and_reads_loading() {
        let (library, jobs) = super::live_warm_tests::library();
        park(&library, 2);

        library
            .look_up_samples_source(SPEC, &access())
            .expect("a busy loader parks the look-up");
        assert_eq!(
            library.samples_source_state(SPEC),
            Some(SourceState::Loading)
        );
        assert_eq!(library.manifests_pending(), 3);
        let source = format!("samples('{SPEC}')\n$: s(\"bd\")");
        let diagnostics = crate::lint::lint(&source, false, Some(&library));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let last = jobs.try_iter().last().expect("jobs in line");
        assert_eq!(describe(&last), quoted(SPEC));
    }

    /// Jobs leave the line in the order they were put in, whichever producer
    /// put them there, so the last registration wins and a preload rides behind
    /// the registration it waits for.
    #[test]
    fn jobs_leave_the_line_in_the_order_every_producer_put_them_in() {
        let (library, jobs) = super::live_warm_tests::library();
        let walked = tempfile::tempdir().expect("a folder to walk");
        let folder = walked.path().to_string_lossy().into_owned();

        library
            .register_trusted_batch_async_for_test(vec![(inline("first"), None)], &[])
            .expect("an evaluation's registration");
        library
            .warm_score_sounds_async(&["first".to_owned()], &access())
            .expect("a warm-up behind it");
        library
            .look_up_samples_source(SPEC, &access())
            .expect("the checker's look-up");
        library.adopt_global_sources(&[GlobalSource {
            spec: folder.clone(),
            enabled: true,
        }]);
        library
            .register_trusted_batch_async_for_test(vec![(inline("last"), None)], &[])
            .expect("a later registration");

        assert_eq!(
            jobs.try_iter()
                .map(|job| describe(&job))
                .collect::<Vec<_>>(),
            [
                inline("first"),
                "preload first".to_owned(),
                quoted(SPEC),
                format!("folders {folder}"),
                inline("last"),
            ]
        );
    }

    /// A Settings source asked for while the loader is busy waits in line: its
    /// row reads fetching, not failed.
    #[test]
    fn a_settings_source_behind_a_busy_loader_waits_rather_than_failing() {
        let (library, _jobs) = super::live_warm_tests::library();
        park(&library, 2);
        let folder = tempfile::tempdir().expect("a folder to walk");

        let reports = library.adopt_global_sources(&[
            GlobalSource {
                spec: folder.path().to_string_lossy().into_owned(),
                enabled: true,
            },
            GlobalSource {
                spec: "https://example.com/pack/strudel.json".to_owned(),
                enabled: true,
            },
        ]);

        assert_eq!(reports.len(), 2);
        assert!(
            reports
                .iter()
                .all(|report| report.state == GlobalSourceState::Loading),
            "{reports:?}"
        );
    }

    /// Past its bound of jobs the line refuses, and the refusal is the spec's
    /// failure: the loader is overwhelmed, not merely busy.
    #[test]
    fn the_line_refuses_past_its_bound_and_the_spec_says_why() {
        let (library, _jobs) = super::live_warm_tests::library();
        park(&library, MANIFEST_QUEUE_JOBS);

        let error = library
            .look_up_samples_source(SPEC, &access())
            .expect_err("the line is full");
        assert!(error.contains("queue is full"), "{error}");
        assert_eq!(
            library.samples_source_state(SPEC),
            Some(SourceState::Failed(error))
        );
        assert_eq!(library.manifests_pending(), MANIFEST_QUEUE_JOBS);
    }

    /// The text the waiting jobs carry is bounded too: a few of the largest
    /// registrations fill the line long before its count of jobs does.
    #[test]
    fn the_text_waiting_in_line_is_bounded() {
        let (library, _jobs) = super::live_warm_tests::library();
        let large = |name: &str| {
            let file = "x".repeat(MAX_MANIFEST_EFFECT_BYTES - 64);
            vec![(
                format!(r#"{{"{name}":"http://127.0.0.1:9/{file}.wav"}}"#),
                None,
            )]
        };
        for name in ["one", "two"] {
            library
                .register_trusted_batch_async_for_test(large(name), &[])
                .expect("room for two of the largest");
        }
        let error = library
            .register_trusted_batch_async_for_test(large("three"), &[])
            .expect_err("the line's text is full");
        assert!(error.contains("queue is full"), "{error}");
        assert_eq!(library.manifests_pending(), 2);
    }

    /// A stopped loader refuses every job, and each map it names says why.
    #[test]
    fn a_stopped_loader_marks_every_map_failed() {
        let (library, jobs) = super::live_warm_tests::library();
        let arrived = "github:me/arrived";
        library.note_samples_source_for_tests(arrived);
        drop(jobs);

        let error = library
            .register_score_batch_async_for_session(
                vec![(quoted(arrived), None), (quoted(SPEC), None)],
                &[],
                &access(),
            )
            .expect_err("the loader has stopped");
        assert!(error.contains("stopped"), "{error}");
        for spec in [arrived, SPEC] {
            assert_eq!(
                library.samples_source_state(spec),
                Some(SourceState::Failed(error.clone()))
            );
        }
    }
}
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
mod soundfont_controls_tests {
    //! A General MIDI soundfont zone plays at its note's pitch, from its start:
    //! the sampler's own controls do not reach it.

    use super::*;

    const RATE: u32 = 48_000;

    /// A two-second 440 Hz tone recorded as a4 (`baseDetune` 6900), as the one
    /// zone of `gm_test`'s first font, over the whole key range, with no loop.
    fn library_with_a_tone_zone() -> SampleLibrary {
        let (library, _jobs) = super::variant_tests::library();
        library.shared.fonts.write().unwrap().insert(
            Arc::from("font0"),
            FontState::Ready(Arc::new(vec![FontZone {
                key_lo: 0.0,
                key_hi: 127.0,
                base_detune: 6900.0,
                id: SampleId(40),
                duration_secs: 2.0,
                loop_secs: None,
            }])),
        );
        library
    }

    /// One second of the left channel of `gm_test` playing a half-second e5 at
    /// each of `onsets` (seconds), with `controls` added to every hap.
    fn render(library: &SampleLibrary, onsets: &[f64], controls: &serde_json::Value) -> Vec<f32> {
        let mut value = serde_json::json!({ "s": "gm_test", "note": "e5" });
        let object = value.as_object_mut().expect("object");
        for (key, control) in controls.as_object().expect("controls") {
            object.insert(key.clone(), control.clone());
        }
        let events: Vec<_> = onsets
            .iter()
            .zip(1..)
            .map(|(&onset, id)| {
                rustel_voice::resolve_voice_with_samples(&value, id, 0.5, onset, RATE, 0.5, library)
                    .unwrap_or_else(|error| panic!("{value}: {error}"))
            })
            .collect();
        let tone = (0..2 * RATE)
            .map(|frame| 0.5 * (std::f32::consts::TAU * 440.0 * frame as f32 / RATE as f32).sin())
            .collect();
        let mut backend = rustel_audio::ScalarBackend::new();
        assert!(
            backend
                .install_sample(
                    SampleId(40),
                    Box::new(DecodedSample::from_parts(RATE, 1, tone).expect("tone"))
                )
                .is_ok()
        );
        let pcm =
            rustel_audio::render_pcm(&mut backend, RATE, RATE as usize, &events).expect("pcm");
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[0])
            .collect()
    }

    /// Frequency in Hz from the rising zero crossings of `left[from..to]`.
    fn pitch(left: &[f32], from: usize, to: usize) -> f64 {
        let crossings: Vec<f64> = (from..to - 1)
            .filter(|&frame| left[frame] <= 0.0 && left[frame + 1] > 0.0)
            .map(|frame| {
                let (a, b) = (f64::from(left[frame]), f64::from(left[frame + 1]));
                frame as f64 + a / (a - b)
            })
            .collect();
        assert!(crossings.len() > 2, "no tone between {from} and {to}");
        let span = crossings[crossings.len() - 1] - crossings[0];
        (crossings.len() - 1) as f64 * f64::from(RATE) / span
    }

    #[test]
    fn a_soundfont_note_plays_at_its_pitch_whatever_the_sampler_controls_say() {
        let library = library_with_a_tone_zone();
        let e5 = 440.0 * 2f64.powf(7.0 / 12.0);
        let plain = render(&library, &[0.0], &serde_json::json!({}));
        let heard = pitch(&plain, 2_400, 19_200);
        assert!(
            (heard / e5 - 1.0).abs() < 0.002,
            "e5 sounds at {heard:.2} Hz, not {e5:.2} Hz"
        );
        for controls in [
            serde_json::json!({ "speed": 0.3 }),
            serde_json::json!({ "speed": -1 }),
            serde_json::json!({ "speed": 0 }),
            serde_json::json!({ "unit": "c", "speed": 2 }),
            serde_json::json!({ "begin": 0.4, "end": 0.8 }),
            serde_json::json!({ "nudge": 0.01 }),
        ] {
            let played = render(&library, &[0.0], &controls);
            let heard = pitch(&played, 2_400, 19_200);
            assert!(
                (heard / e5 - 1.0).abs() < 0.002,
                "{controls}: e5 sounds at {heard:.2} Hz, not {e5:.2} Hz"
            );
            assert!(
                played == plain,
                "{controls} changed the soundfont note, which plays like the plain one"
            );
        }
    }

    /// A later note in the same `cut` group leaves an earlier soundfont note
    /// sounding.
    #[test]
    fn a_soundfont_note_is_not_cut_by_the_next() {
        let library = library_with_a_tone_zone();
        let onsets = [0.0, 0.25];
        let plain = render(&library, &onsets, &serde_json::json!({}));
        let cut = render(&library, &onsets, &serde_json::json!({ "cut": 1 }));
        assert!(cut == plain, "cut(1) cut the first soundfont note short");
    }
}
#[cfg(test)]
mod source_retention_tests {
    //! Repeated score registration retains a fixed source-history working set.
    //!
    //! Owned string storage reaches a plateau and container capacities remain
    //! bounded. This does not measure allocator overhead, process RSS, or the
    //! memory used by decoded audio and other tables.

    use super::*;

    const MAP_BYTES: usize = 1024 * 1024;
    const REGISTRATIONS: usize = 96;

    fn location(index: usize) -> &'static str {
        if index.is_multiple_of(2) {
            "https://samples.example/first.wav"
        } else {
            "https://samples.example/second.wav"
        }
    }

    /// Different valid JSON text, with one bank and only two file identities.
    /// Whitespace fills the history budget without accumulating banks or URLs.
    fn source_map(index: usize) -> String {
        let mut map = String::with_capacity(MAP_BYTES);
        map.push_str(&format!(r#"{{"active":"{}"}}"#, location(index)));
        map.extend(std::iter::repeat_n('\n', index));
        map.extend(std::iter::repeat_n(' ', MAP_BYTES - map.len()));
        map
    }

    #[derive(Debug, PartialEq, Eq)]
    struct HistoryStorage {
        unique_text_bytes: usize,
        entry_counts: [usize; 3],
        string_capacity: usize,
    }

    fn history_storage(library: &SampleLibrary) -> HistoryStorage {
        let tables = library
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        assert_eq!(
            tables.bytes,
            tables.order.iter().map(String::len).sum::<usize>()
        );
        // Hash-map capacity can vary as removal leaves tombstones and insertion
        // reclaims them. Bound that slack instead of requiring identical capacity
        // at every checkpoint; source text, entries and string buffers are exact.
        for capacity in [
            tables.sources.capacity(),
            tables.states.capacity(),
            tables.order.capacity(),
        ] {
            assert!(capacity <= 4 * (MAX_SOURCE_TABLE_BYTES / MAP_BYTES));
        }
        HistoryStorage {
            unique_text_bytes: tables.bytes,
            entry_counts: [
                tables.sources.len(),
                tables.states.len(),
                tables.order.len(),
            ],
            string_capacity: tables
                .sources
                .iter()
                .chain(tables.states.keys())
                .chain(tables.order.iter())
                .map(String::capacity)
                .sum(),
        }
    }

    #[test]
    fn repeated_score_registrations_bound_history_and_keep_current_lookup() {
        // Keep this fixture at 96 MiB of streamed input if the product's budget
        // changes: resizing the soak should be an explicit test decision.
        assert_eq!(MAX_SOURCE_TABLE_BYTES, 32 * MAP_BYTES);
        let retained_maps = MAX_SOURCE_TABLE_BYTES / MAP_BYTES;
        let library = SampleLibrary::empty();
        library.set_direct_diagnostic_logging(false);
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://samples.example")
            .expect("fixture origin");
        let first = source_map(0);
        let mut plateau = None;

        for index in 0..REGISTRATIONS {
            let map = source_map(index);
            library
                .register_score_custom(&map, None, &access)
                .expect("inline score map publishes without a fetch");
            assert_eq!(library.samples_source_state(&map), Some(SourceState::Ready));
            assert!(library.knows_samples_source(&map));
            assert_eq!(
                library.file_location("active", None).as_deref(),
                Some(location(index)),
                "the newest registration supplies the active bank"
            );

            let completed = index + 1;
            if completed == retained_maps + 1 || completed.is_multiple_of(retained_maps) {
                let storage = history_storage(&library);
                assert_eq!(storage.unique_text_bytes, MAX_SOURCE_TABLE_BYTES);
                assert_eq!(storage.entry_counts, [retained_maps; 3]);
                assert_eq!(storage.string_capacity, 3 * MAX_SOURCE_TABLE_BYTES);
                if completed > retained_maps {
                    assert!(!library.knows_samples_source(&first));
                    assert_eq!(library.samples_source_state(&first), None);
                    match &plateau {
                        Some(previous) => assert_eq!(&storage, previous),
                        None => plateau = Some(storage),
                    }
                }
            }
        }

        assert!(plateau.is_some(), "the registrations crossed the bound");
        assert_eq!(library.manifests_pending(), 0);
        assert_eq!(library.pending_loads(), 0, "lookup never fetches audio");
        assert!(library.take_failures().is_empty());
    }
}
#[cfg(test)]
mod source_retry_tests {
    //! Failed manifest imports retry after a cooldown. Repeated failures increase
    //! the background retry delay. Each new failure reason is reported once.
    //! Updates remain blocked until the import succeeds.

    use super::manifest_congestion_tests::access;
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// A server answering each request with the next status and body of
    /// `script`, at an address no earlier run can have cached. `{origin}` in a
    /// body is its own origin. Returns the origin, the manifest's address and
    /// the server thread.
    fn scripted_server(
        script: Vec<(u16, &'static str)>,
    ) -> (String, String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let spec = format!("{origin}/retry-{}-{unique}.json", std::process::id());
        let own = origin.clone();
        let server = std::thread::spawn(move || {
            for (status, body) in script {
                let (stream, _) = listener.accept().expect("accept");
                let mut stream = super::manifest_worker_tests::drain_request(stream);
                let body = body.replace("{origin}", &own);
                write!(
                    stream,
                    "HTTP/1.1 {status} Scripted\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("write response");
            }
        });
        (origin, spec, server)
    }

    /// A failed manifest blocks updates until a retry succeeds. Repeated failures
    /// report each distinct reason once. Recovery does not require a restart.
    #[test]
    fn a_failed_import_recovers_once_a_later_ask_succeeds() {
        let (origin, spec, server) = scripted_server(vec![
            (404, ""),
            (404, ""),
            (500, ""),
            (200, r#"{"_base":"{origin}/","retried":"retried.wav"}"#),
        ]);
        let mut access = ScoreSampleAccess::denied();
        access.permit_origin(&origin).expect("origin");
        let library = SampleLibrary::empty();
        let score = format!("samples('{spec}')\n$: s(\"retried\")");
        let refusal = || crate::lint::rejection(&score, false, Some(&library));
        let ask = || {
            library
                .look_up_samples_source(&spec, &access)
                .expect("asked");
            library.wait_until_idle(Duration::from_secs(30));
            library.samples_source_state(&spec)
        };

        let Some(SourceState::Failed(missing)) = ask() else {
            panic!("a missing map is a failure");
        };
        assert_eq!(
            library.take_failures_with_maps(),
            [SampleFailure {
                message: missing.clone(),
                maps: vec![serde_json::to_string(&spec).expect("a string")],
            }],
            "said about its own map"
        );
        assert!(
            refusal().is_some_and(|reason| reason.contains(&missing)),
            "the failure refuses the update"
        );

        // Fresh, it rests: nobody asks the server.
        assert_eq!(ask(), Some(SourceState::Failed(missing.clone())));

        library.rest_samples_source_for_tests(&spec);
        assert_eq!(ask(), Some(SourceState::Failed(missing.clone())));
        assert!(
            library.take_failures().is_empty(),
            "the same failure is said once"
        );
        assert!(refusal().is_some(), "and still refuses the update");

        library.rest_samples_source_for_tests(&spec);
        let Some(SourceState::Failed(broken)) = ask() else {
            panic!("a server error is a failure");
        };
        assert_ne!(broken, missing);
        assert_eq!(library.take_failures(), [broken]);

        library.rest_samples_source_for_tests(&spec);
        assert_eq!(ask(), Some(SourceState::Ready));
        assert!(library.knows("retried"));
        assert_eq!(refusal(), None, "the update goes through");
        server.join().expect("server");
    }

    /// A retry requires an expired cooldown and an idle loader. The source keeps
    /// its failure reason while the retry is pending.
    #[test]
    fn a_failure_is_asked_again_only_once_rested_with_the_loader_idle() {
        let spec = "github:me/gone";
        let failed = SourceState::Failed("offline".to_owned());

        let (library, _jobs) = super::live_warm_tests::library();
        library.note_samples_source_state_for_tests(spec, failed.clone());
        library
            .look_up_samples_source(spec, &access())
            .expect("a fresh failure rests");
        assert_eq!(library.manifests_pending(), 0, "a fresh failure rests");
        library.rest_samples_source_for_tests(spec);
        library
            .register_trusted_batch_async_for_test(
                vec![(r#"{"busy":"http://127.0.0.1:9/busy.wav"}"#.to_owned(), None)],
                &[],
            )
            .expect("the loader has something in hand");
        library
            .look_up_samples_source(spec, &access())
            .expect("a busy loader is left alone");
        assert_eq!(
            library.manifests_pending(),
            1,
            "a busy loader is left alone"
        );

        let (library, _jobs) = super::live_warm_tests::library();
        library.note_samples_source_state_for_tests(spec, failed.clone());
        library.rest_samples_source_for_tests(spec);
        library
            .look_up_samples_source(spec, &access())
            .expect("asked again");
        assert_eq!(library.manifests_pending(), 1);
        assert_eq!(
            library.samples_source_state(spec),
            Some(failed),
            "the failure stands while it is asked again"
        );
    }

    /// Consecutive failures double the background retry delay. Explicit requests
    /// keep the shorter cooldown. A successful response resets the failure count.
    #[test]
    fn a_failure_in_a_row_rests_longer_before_the_background_asks() {
        let (origin, spec, server) = scripted_server(vec![(404, ""), (404, "")]);
        let mut access = ScoreSampleAccess::denied();
        access.permit_origin(&origin).expect("origin");
        let library = SampleLibrary::empty();
        let ask = || {
            library
                .look_up_samples_source(&spec, &access)
                .expect("asked");
            library.wait_until_idle(Duration::from_secs(30));
        };

        ask();
        library.rest_samples_source_for_tests(&spec);
        assert!(
            library.samples_source_rested(&spec, true),
            "a first failure"
        );

        ask();
        library.rest_samples_source_for_tests(&spec);
        assert!(library.samples_source_rested(&spec, false), "an ask");
        assert!(
            !library.samples_source_rested(&spec, true),
            "the second failure rests twice as long"
        );
        library.rest_samples_source_for_tests(&spec);
        assert!(library.samples_source_rested(&spec, true));
        server.join().expect("server");

        library.note_samples_source_state_for_tests(&spec, SourceState::Ready);
        library.note_samples_source_state_for_tests(&spec, SourceState::Failed("offline".into()));
        library.rest_samples_source_for_tests(&spec);
        assert!(
            library.samples_source_rested(&spec, true),
            "an answer starts the count over"
        );
    }

    /// A refused lookup counts as one failure, whether access is denied before
    /// submission or the queue refuses the job.
    #[test]
    fn a_refused_look_up_counts_as_one_failure() {
        let (library, jobs) = super::live_warm_tests::library();
        drop(jobs);
        for (spec, access) in [
            ("github:me/refused", access()),
            ("github:me/denied", ScoreSampleAccess::denied()),
        ] {
            let ask = || {
                library
                    .look_up_samples_source(spec, &access)
                    .expect_err("refused");
                library.rest_samples_source_for_tests(spec);
            };
            ask();
            assert!(library.samples_source_rested(spec, true), "{spec}: one");
            ask();
            assert!(!library.samples_source_rested(spec, true), "{spec}: two");
            library.rest_samples_source_for_tests(spec);
            assert!(library.samples_source_rested(spec, true), "{spec}: rested");
        }
    }

    /// The retry delay cannot exceed [`FAILED_RETRY_LONGEST`].
    #[test]
    fn the_rest_stops_growing() {
        let standing = |failures| Standing {
            failures,
            ..Standing::now(SourceState::Failed("offline".into()))
        };
        assert_eq!(standing(1).rest(), FAILED_RETRY_AFTER);
        assert_eq!(standing(2).rest(), FAILED_RETRY_AFTER * 2);
        assert_eq!(standing(u32::MAX).rest(), FAILED_RETRY_LONGEST);
    }
}
#[cfg(test)]
mod source_text_tests {
    //! What the text of a `samples()` source, base or bank entry may hold, and
    //! what a refusal of it says.

    use super::*;

    const IDLE: Duration = Duration::from_secs(5);

    /// A library that has registered what `register` asks of it and settled,
    /// with nothing refused.
    fn settled(register: impl FnOnce(&SampleLibrary) -> Result<(), String>) -> SampleLibrary {
        let library = SampleLibrary::empty();
        register(&library).expect("the map is queued");
        library.wait_until_idle(IDLE);
        assert_eq!(library.take_failures(), Vec::<String>::new());
        library
    }

    /// `map` registered on the trusted route, as the studio's own sources are.
    fn trusted(map: &str, base: Option<&str>) -> SampleLibrary {
        settled(|library| library.register_trusted_custom(map, base))
    }

    /// `map` registered by a score granted `origin`, as `--allow-sample-origin`
    /// grants it.
    fn scored(map: &str, base: Option<&str>, origin: &str) -> SampleLibrary {
        let mut access = ScoreSampleAccess::denied();
        access.permit_origin(origin).expect("a well-formed origin");
        settled(|library| library.register_score_custom(map, base, &access))
    }

    /// A refused bank entry reports the fetch boundary's own reason, such as
    /// the scheme or a missing host. The other entries still register.
    #[test]
    fn a_refused_bank_entry_says_what_was_wrong() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{
                "ftp": "ftp://example.com/kit/bd.wav",
                    "hostless": "https:///kit/bd.wav",
                    "fine": "https://example.com/kit/bd.wav"
                }"#,
                None,
            )
            .expect("the map is queued");
        library.wait_until_idle(IDLE);
        let failures = library.take_failures();

        let refusal = |name: &str| {
            failures
                .iter()
                .find(|failure| {
                    failure.starts_with(&format!("samples() entry {name:?} was refused: "))
                })
                .unwrap_or_else(|| panic!("{name} was not refused: {failures:?}"))
        };
        assert!(refusal("ftp").contains("is a ftp: address"), "{failures:?}");
        assert!(
            refusal("hostless").contains("names no host"),
            "{failures:?}"
        );
        assert!(
            failures
                .iter()
                .all(|failure| !failure.contains("bank URLs must be http(s)")),
            "{failures:?}"
        );
        assert_eq!(
            library.file_location("fine", None).as_deref(),
            Some("https://example.com/kit/bd.wav")
        );
    }

    /// Drum libraries name cymbals by size in inches: `Crash 18".wav`. A bank
    /// holding one registers on both routes, as it does in a browser, which
    /// percent-encodes the quote; a sample server decodes it back. The trusted
    /// route keeps the entry as written and encodes it when it fetches; the
    /// score route registers the parsed address.
    #[test]
    fn a_file_name_holding_a_quote_still_registers() {
        let map = r#"{"crash": "https://example.com/kit/Crash 18\".wav"}"#;
        assert_eq!(
            trusted(map, None).file_location("crash", None).as_deref(),
            Some("https://example.com/kit/Crash 18\".wav")
        );
        assert_eq!(
            scored(map, None, "https://example.com")
                .file_location("crash", None)
                .as_deref(),
            Some("https://example.com/kit/Crash%2018%22.wav")
        );
    }

    /// A `samples()` source written in a multi-line template literal ends in
    /// a newline. The URL parser always dropped it, and it is still dropped:
    /// on the score's own address, and on the address a `github:` shorthand
    /// expands to, where it would otherwise land inside the path.
    #[test]
    fn a_samples_source_with_a_trailing_newline_still_registers() {
        let mut access = ScoreSampleAccess::denied();
        access.permit_public_cors_origins();

        let source = "https://samples.example/kit.json\n";
        access
            .preapprove_effect(&serde_json::to_string(source).unwrap())
            .expect("the score's samples() call is accepted");
        let (approved, _) = access.approve_remote(source).expect("approved");
        assert_eq!(approved.as_ref(), "https://samples.example/kit.json");
        // Leading whitespace too, the same on this route as on the trusted one.
        let (approved, _) = access
            .approve_remote("\n  https://samples.example/kit.json")
            .expect("approved with leading whitespace");
        assert_eq!(approved.as_ref(), "https://samples.example/kit.json");

        match read_source("github:tidalcycles/dirt-samples\n", GITHUB_SAMPLE_MANIFEST) {
            SampleSource::Url(url) => assert_eq!(
                url,
                "https://raw.githubusercontent.com/tidalcycles/dirt-samples/main/strudel.json"
            ),
            SampleSource::LocalFolder(folder) => panic!("read as a folder: {folder}"),
        }
        access
            .preapprove_effect(&serde_json::to_string("github:tidalcycles/dirt-samples\n").unwrap())
            .expect("the shorthand is accepted too");
    }

    /// A base written in a template literal ends in a newline too, and a base is
    /// joined to every file name: `https://samples.example/kit/\n` + `bd.wav`.
    /// The newline is trimmed off the base before the join, on both routes, so
    /// each file registers at the address it names.
    #[test]
    fn a_base_with_a_trailing_newline_still_joins_its_files() {
        let map = r#"{"bd": "bd.wav", "sd": {"_base": "https://samples.example/snares/\n", "c3": "sd.wav"}}"#;
        let base = Some("https://samples.example/kit/\n");
        for library in [
            trusted(map, base),
            scored(map, base, "https://samples.example"),
        ] {
            assert_eq!(
                library.file_location("bd", None).as_deref(),
                Some("https://samples.example/kit/bd.wav")
            );
            assert_eq!(
                library.file_location("sd", None).as_deref(),
                Some("https://samples.example/snares/sd.wav")
            );
        }
    }

    #[test]
    fn loading_and_published_sources_share_the_retention_bound() {
        let library = SampleLibrary::empty();
        let pad = "x".repeat(1024 * 1024);
        let first = format!("{{\"m-first\": \"{pad}\"}}");
        let latest = format!("{{\"m-latest\": \"{}\"}}", "y".repeat(1024 * 1024));
        library.mark_source_loading(&first);
        library
            .shared
            .note_source_standing(&first, SourceState::Ready);
        // Enough distinct megabyte maps to cross the retention bound.
        for index in 0..(MAX_SOURCE_TABLE_BYTES / (1024 * 1024) + 2) {
            let map = format!("{{\"m{index}\": \"{pad}\"}}");
            library.mark_source_loading(&map);
            library
                .shared
                .note_source_standing(&map, SourceState::Ready);
        }
        let tables = library
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        assert!(
            !tables.states.contains_key(&first) && !tables.sources.contains(&first),
            "the oldest registration survived eviction at the retention bound"
        );
        assert!(
            tables.bytes <= MAX_SOURCE_TABLE_BYTES,
            "the retained bytes ran past the bound"
        );
        assert_eq!(
            tables.bytes,
            tables.order.iter().map(String::len).sum::<usize>()
        );
        drop(tables);
        // The newest registration is never the one evicted.
        library
            .shared
            .note_source_standing(&latest, SourceState::Ready);
        assert!(
            library
                .shared
                .source_tables
                .lock()
                .expect("samples source tables")
                .states
                .contains_key(&latest)
        );
    }

    #[test]
    fn refreshed_source_status_does_not_duplicate_retention_accounting() {
        let mut tables = SourceTables::default();
        for state in [SourceState::Loading, SourceState::Ready] {
            tables.note("map", state);
            tables.forget_state("map");
            tables.note("map", SourceState::Loading);
            assert_eq!(tables.bytes, "map".len());
            assert_eq!(tables.order.len(), 1);
        }
        tables.note("map", SourceState::Ready);
        assert!(tables.sources.contains("map"));
    }

    #[test]
    fn oversized_lookup_failures_do_not_enter_source_history() {
        let mut tables = SourceTables::default();
        tables.note("recent", SourceState::Ready);
        let oversized = "x".repeat(MAX_SCORE_SAMPLE_MAP_BYTES + 1);
        tables.note(&oversized, SourceState::Failed("map exceeds limit".into()));
        assert_eq!(tables.states.len(), 1);
        assert!(tables.sources.contains("recent"));
        assert_eq!(tables.bytes, "recent".len());
    }
}
#[cfg(test)]
mod variant_tests {
    //! Which of a sound's files are kept and loaded ahead for the variants a
    //! text can play: see [`crate::sounds::Variants`].

    use super::*;
    use crate::sounds::Variants;

    /// A library with no workers: an ordinary bank `takes` of four files, a
    /// bank `keys` the note picks from, a General MIDI name `gm_test` of four
    /// fonts and `gm_other` of two.
    pub(super) fn library() -> (SampleLibrary, ManifestJobs) {
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
            shared: Arc::new(super::codec_tests::sample_test_shared(1)),
            manifest_queue,
            manifest_order: Mutex::new(()),
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
#[cfg(test)]
mod vorbis_guard_tests {
    //! The Vorbis setup-header guard, [`guard_vorbis_setup`]: its codebook
    //! arithmetic checked directly on the buffer symphonia's demuxer hands the
    //! codec, and the production decode paths driven end to end with hand-built
    //! and real Ogg files.

    use super::*;

    /// A least-significant-bit-first bit writer, the packing direction of
    /// Vorbis header fields (spec section 3.1), for building header packets
    /// bit by bit in the fixtures below.
    struct TestBits {
        bytes: Vec<u8>,
        bit: u32,
    }

    impl TestBits {
        fn new() -> Self {
            TestBits {
                bytes: Vec::new(),
                bit: 0,
            }
        }

        fn push(&mut self, value: u32, bits: u32) {
            for index in 0..bits {
                if self.bit == 0 {
                    self.bytes.push(0);
                }
                if (value >> index) & 1 == 1 {
                    *self.bytes.last_mut().expect("a byte to set") |= 1 << self.bit;
                }
                self.bit = (self.bit + 1) % 8;
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            self.bytes
        }
    }

    /// The Ogg CRC-32 (polynomial 0x04c11db7, init 0, no reflection, no
    /// final xor) of a page whose checksum field is zeroed, written back into
    /// it. symphonia's page reader refuses a page whose CRC does not match,
    /// so a fixture meant to reach the decoder has to carry a real one.
    fn ogg_crc_page(page: &mut [u8]) {
        page[22..26].copy_from_slice(&[0u8; 4]);
        let mut crc = 0u32;
        for &byte in page.iter() {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        page[22..26].copy_from_slice(&crc.to_le_bytes());
    }

    /// The Ogg lacing values for one packet that terminates on its page:
    /// runs of 255 for each full segment, then a final value under 255 (a
    /// trailing 0 when the length is an exact multiple of 255).
    fn lacing_for(len: usize) -> Vec<u8> {
        let mut segments = vec![255u8; len / 255];
        segments.push((len % 255) as u8);
        segments
    }

    /// Build one Ogg page from an explicit segment table and payload, with a
    /// correct CRC. This is the low-level builder the fixtures below layer on:
    /// a segment table lets a page end mid-packet (a trailing 255) so a packet
    /// can be split across pages the way a real muxer splits a large one.
    fn ogg_page_raw(
        payload: &[u8],
        segments: &[u8],
        header_type: u8,
        sequence: u32,
        serial: u32,
        absgp: u64,
    ) -> Vec<u8> {
        assert!(segments.len() <= 255, "a page holds at most 255 segments");
        let mut page = Vec::with_capacity(27 + segments.len() + payload.len());
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(header_type);
        page.extend_from_slice(&absgp.to_le_bytes());
        page.extend_from_slice(&serial.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(segments.len() as u8);
        page.extend_from_slice(segments);
        page.extend_from_slice(payload);
        ogg_crc_page(&mut page);
        page
    }

    /// One Ogg page carrying whole packets, each terminated with its own
    /// lacing - the ordinary case where no packet spans a page boundary.
    fn ogg_page_packets(
        packets: &[&[u8]],
        header_type: u8,
        sequence: u32,
        serial: u32,
        absgp: u64,
    ) -> Vec<u8> {
        let mut segments = Vec::new();
        let mut payload = Vec::new();
        for packet in packets {
            segments.extend(lacing_for(packet.len()));
            payload.extend_from_slice(packet);
        }
        ogg_page_raw(&payload, &segments, header_type, sequence, serial, absgp)
    }

    /// Split one packet across two pages: the first page ends on a 255 lacing
    /// (the packet continues), the second is flagged a continuation (0x01)
    /// and carries the rest. This is the layout a real muxer produces when a
    /// setup header does not fit on one page, and the case the guard used to
    /// mis-handle by splicing the second page's header into the packet.
    fn split_packet_pages(packet: &[u8], first_sequence: u32, serial: u32) -> Vec<u8> {
        assert!(
            packet.len() > 255,
            "a split needs a packet over one segment"
        );
        let mut pages = ogg_page_raw(&packet[..255], &[255], 0x00, first_sequence, serial, 0);
        let rest = &packet[255..];
        pages.extend(ogg_page_raw(
            rest,
            &lacing_for(rest.len()),
            0x01,
            first_sequence + 1,
            serial,
            0,
        ));
        pages
    }

    /// A well-formed 30-byte Vorbis identification header: mono 44.1 kHz.
    fn vorbis_id_packet() -> Vec<u8> {
        let mut id = vec![0x01];
        id.extend_from_slice(b"vorbis");
        id.extend_from_slice(&0u32.to_le_bytes());
        id.push(1);
        id.extend_from_slice(&44_100u32.to_le_bytes());
        id.extend_from_slice(&0u32.to_le_bytes());
        id.extend_from_slice(&0u32.to_le_bytes());
        id.extend_from_slice(&0u32.to_le_bytes());
        id.push((11 << 4) | 11);
        id.push(1);
        assert_eq!(id.len(), 30, "the mapper requires exactly 30 bytes");
        id
    }

    /// An empty but well-formed Vorbis comment header.
    fn vorbis_comment_packet() -> Vec<u8> {
        let mut comment = vec![0x03];
        comment.extend_from_slice(b"vorbis");
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.push(1);
        comment
    }

    /// Wrap a setup header in the pages of one logical stream, closed by an
    /// end-of-stream page carrying one audio packet. The closing page matters
    /// for a hostile fixture reaching the decoder: a stream that ends right
    /// after its setup header is refused by the demuxer's probe before the
    /// codec parses it, so the attack carries one audio packet to get the
    /// probe one packet further. `split` puts the setup across two pages,
    /// the layout a real muxer uses for a header too big for one page.
    fn vorbis_header_ogg_inner(setup: &[u8], split: bool) -> Vec<u8> {
        let serial = 0x564F_u32;
        let mut ogg = ogg_page_packets(&[&vorbis_id_packet()], 0x02, 0, serial, 0);
        ogg.extend(ogg_page_packets(
            &[&vorbis_comment_packet()],
            0x00,
            1,
            serial,
            0,
        ));
        if split {
            ogg.extend(split_packet_pages(setup, 2, serial));
            ogg.extend(ogg_page_packets(&[&[0x00]], 0x04, 4, serial, 0));
        } else {
            ogg.extend(ogg_page_packets(&[setup], 0x00, 2, serial, 0));
            ogg.extend(ogg_page_packets(&[&[0x00]], 0x04, 3, serial, 0));
        }
        ogg
    }

    fn vorbis_header_ogg(setup: &[u8]) -> Vec<u8> {
        vorbis_header_ogg_inner(setup, false)
    }

    /// The buffer symphonia's Ogg Vorbis mapper hands the codec: the 30-byte
    /// identification header, then the setup packet whole. This is exactly
    /// what [`guard_vorbis_setup`] receives, so the direct-guard tests below
    /// can feed a setup packet without paginating anything.
    fn extra_data(setup: &[u8]) -> Vec<u8> {
        let mut buf = vorbis_id_packet();
        buf.extend_from_slice(setup);
        buf
    }

    /// A setup header packet (`0x05 'vorbis'` then the raw codebook bits).
    fn setup_packet(bits: TestBits) -> Vec<u8> {
        let mut setup = vec![0x05];
        setup.extend_from_slice(b"vorbis");
        setup.extend_from_slice(&bits.into_bytes());
        setup
    }

    /// One codebook whose ordered codeword lengths cover every entry in a
    /// single run - the cheapest way to write a book with a huge entry count,
    /// and what a size-lie fixture uses. `lookup` is the VQ lookup type; when
    /// it is 1 or 2 the table header follows but no multiplicands, so the book
    /// is only ever valid to *allocate* toward.
    fn oversized_codebook(bits: &mut TestBits, dimensions: u32, entries: u32, lookup: u32) {
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(dimensions, 16);
        bits.push(entries, 24);
        bits.push(1, 1); // codeword lengths are ordered
        bits.push(0, 5); // initial length minus one
        let run_bits = 32 - entries.leading_zeros();
        bits.push(entries, run_bits); // one run covers every entry
        bits.push(lookup, 4);
        if lookup != 0 {
            bits.push(0, 32); // minimum value
            bits.push(0, 32); // delta value
            bits.push(0, 4); // value bits minus one
            bits.push(0, 1); // sequence_p
        }
    }

    /// A minimal second codebook to follow the book under test: one dimension,
    /// two entries of length 1, no lookup. If the walk over the book before it
    /// skipped a single bit too many or too few, this book's sync word would not
    /// be where the guard looks for it.
    fn small_codebook(bits: &mut TestBits) {
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(1, 16); // dimensions
        bits.push(2, 24); // entries
        bits.push(0, 1); // unordered
        bits.push(0, 1); // not sparse
        bits.push(0, 5); // length 1
        bits.push(0, 5); // length 1
        bits.push(0, 4); // lookup type 0
    }

    /// Regression: 2^23 entries * 2^15 dimensions wraps u32 in symphonia 0.5.5,
    /// and the decoder then aborts on a huge allocation. The guard refuses the
    /// u64 product. The test calls it directly: the test profile panics first.
    #[test]
    fn the_wrapping_size_lie_is_refused_by_the_setup_guard() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        oversized_codebook(&mut bits, 32_768, 8_388_608, 2); // 2^15 x 2^23
        let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect_err("the wrapping size lie must be refused");
        assert!(error.contains("cap"), "names the value cap: {error}");
    }

    /// A type-2 lookup that promises entries × dimensions multiplicands the
    /// packet does not carry. The decoder reads exactly that many before it
    /// can build the book, so a header promising more table than the packet
    /// holds is a stream no decoder finishes, only allocates toward.
    #[test]
    fn a_type2_multiplicand_promise_the_packet_cannot_back_is_refused() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(2, 16); // dimensions
        bits.push(4, 24); // entries: eight multiplicands promised
        bits.push(0, 1); // codeword lengths unordered
        bits.push(0, 1); // ...and not sparse
        for _ in 0..4 {
            bits.push(0, 5); // one codeword length per entry
        }
        bits.push(2, 4); // lookup type 2
        bits.push(0, 32); // minimum value
        bits.push(0, 32); // delta value
        bits.push(0, 4); // one bit per multiplicand
        bits.push(0, 1); // sequence_p
        // The packet ends here: not one promised multiplicand is carried.
        let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect_err("a table its packet cannot back must be refused");
        assert!(
            error.contains("multiplicand"),
            "names the missing multiplicands: {error}"
        );
    }

    /// The value cap also covers lookup type 1. The packet carries only
    /// `floor(entries^(1/dims))` multiplicands, but the decoder allocates the
    /// full 2^26-value table.
    #[test]
    fn a_type1_lookup_table_over_the_cap_is_refused() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        oversized_codebook(&mut bits, 8, 8_388_608, 1); // 8 x 2^23 = 2^26 table
        let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect_err("a type-1 table over the cap must be refused");
        assert!(error.contains("cap"), "names the value cap: {error}");
    }

    /// An honest type-1 book whose `floor(entries^(1/dims))` multiplicands are
    /// all present passes: the guard walks the type-1 path, computes the count
    /// by integer root, and lets a real book through.
    #[test]
    fn a_type1_lookup_codebook_with_its_multiplicands_passes() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(3, 16); // dimensions
        bits.push(8, 24); // entries: floor(8^(1/3)) = 2 multiplicands
        bits.push(0, 1); // unordered
        bits.push(0, 1); // not sparse
        for _ in 0..8 {
            bits.push(0, 5); // one codeword length per entry
        }
        bits.push(1, 4); // lookup type 1
        bits.push(0, 32); // minimum value
        bits.push(0, 32); // delta value
        bits.push(0, 4); // one bit per multiplicand
        bits.push(0, 1); // sequence_p
        bits.push(0, 1); // the two promised multiplicands...
        bits.push(1, 1);
        guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect("an honest type-1 book is not the guard's business");
    }

    /// Books with zero dimensions and 2^24-1 entries each. Their lookup tables
    /// are empty, but symphonia builds entry-sized state for such a book. The
    /// guard charges the entry count against the header budget and refuses the
    /// fifth book. The test checks the guard's charge, not symphonia's.
    #[test]
    fn a_dimension_zero_codebook_flood_is_refused() {
        let mut bits = TestBits::new();
        bits.push(4, 8); // five codebooks
        for _ in 0..5 {
            oversized_codebook(&mut bits, 0, 16_777_215, 0); // dims 0, entries 2^24-1
        }
        let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect_err("a dimension-zero flood must be refused");
        assert!(error.contains("budget"), "names the stream budget: {error}");
    }

    /// A small, honestly-declared type-2 book passes. The guard refuses
    /// impossible tables, not real ones: a two-dimension, four-entry book
    /// whose eight one-bit multiplicands are all present is what a real
    /// encoder writes, and the same walk must let it through.
    #[test]
    fn an_honest_codebook_passes_the_setup_guard() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(2, 16); // dimensions
        bits.push(4, 24); // entries
        bits.push(0, 1); // unordered
        bits.push(0, 1); // not sparse
        for _ in 0..4 {
            bits.push(0, 5); // one codeword length per entry
        }
        bits.push(2, 4); // lookup type 2: eight multiplicands
        bits.push(0, 32); // minimum value
        bits.push(0, 32); // delta value
        bits.push(0, 4); // one bit per multiplicand
        bits.push(0, 1); // sequence_p
        for value in 0..8 {
            bits.push(u32::from(value % 2 == 0), 1); // the eight promised values
        }
        guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect("an honest codebook is not the guard's business");
    }

    /// A sparsely packed codeword list passes. Most real books mark unused
    /// entries with a single 0 bit, so the guard must walk the sparse branch
    /// (one flag bit per entry, five length bits only when used) to land the
    /// next field on its true boundary.
    #[test]
    fn a_sparse_codeword_list_is_walked() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(1, 16); // dimensions
        bits.push(4, 24); // entries
        bits.push(0, 1); // unordered
        bits.push(1, 1); // sparse
        for entry in 0..4 {
            let used = entry % 2 == 0;
            bits.push(u32::from(used), 1); // used flag
            if used {
                bits.push(0, 5); // length only for used entries
            }
        }
        bits.push(0, 4); // lookup type 0: no table
        guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect("a sparse book is not the guard's business");
    }

    /// A complete type-2 book followed by a second book. The guard must skip
    /// exactly entries * dimensions (4 * 2) three-bit multiplicands to find
    /// the sync word of the next book.
    #[test]
    fn a_type2_book_is_stepped_over_to_the_next_codebook() {
        let mut bits = TestBits::new();
        bits.push(1, 8); // two codebooks
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(2, 16); // dimensions
        bits.push(4, 24); // entries
        bits.push(0, 1); // unordered
        bits.push(0, 1); // not sparse
        for _ in 0..4 {
            bits.push(1, 5); // four codewords of length 2: a complete code
        }
        bits.push(2, 4); // lookup type 2
        bits.push(0, 32); // minimum value
        bits.push(0, 32); // delta value
        bits.push(2, 4); // three bits per multiplicand
        bits.push(0, 1); // sequence_p
        for value in 0..8 {
            bits.push(value, 3); // all eight multiplicands
        }
        small_codebook(&mut bits);
        guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect("a type-2 book and the book after it are not the guard's business");
    }

    /// A length-ordered book with several runs, followed by a second book.
    /// Each run count uses `ilog(entries left)` bits (3, 2, 2 here), so the
    /// guard must size each run by the entries that remain.
    #[test]
    fn a_multi_run_ordered_book_is_stepped_over_to_the_next_codebook() {
        let mut bits = TestBits::new();
        bits.push(1, 8); // two codebooks
        bits.push(0x564342, 24); // codebook sync "BCV"
        bits.push(1, 16); // dimensions
        bits.push(4, 24); // entries
        bits.push(1, 1); // codeword lengths are ordered
        bits.push(0, 5); // lengths start at 1
        bits.push(1, 3); // one entry of length 1, in ilog(4) = 3 bits
        bits.push(1, 2); // one of length 2, in ilog(3) = 2 bits
        bits.push(2, 2); // two of length 3, in ilog(2) = 2 bits: 1, 2, 3, 3 is complete
        bits.push(0, 4); // lookup type 0
        small_codebook(&mut bits);
        guard_vorbis_setup(&extra_data(&setup_packet(bits)))
            .expect("an ordered book and the book after it are not the guard's business");
    }

    /// A type-1 book that symphonia would take to the table allocation: 2^22
    /// entries and 8 dimensions give a 2^25-value (128 MiB) f32 table, with
    /// all six multiplicands present. The guard refuses it first. The sizes
    /// keep symphonia's f32 `lookup1_values` exact, so the probe passes it on.
    #[test]
    fn a_type1_table_symphonia_would_allocate_is_refused_before_the_decoder() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        oversized_codebook(&mut bits, 8, 4_194_304, 1); // 8 x 2^22 = a 2^25-value table
        for value in 0..6 {
            bits.push(value % 2, 1); // the six one-bit multiplicands, all present
        }
        let ogg = vorbis_header_ogg(&setup_packet(bits));
        let error = decode_guarded("https://example.test/table.ogg", &ogg, Some(48_000))
            .expect_err("a table symphonia would allocate must be refused first");
        assert!(error.contains("cap"), "names the value cap: {error}");
    }

    /// The type-1 multiplicand count, `floor(entries^(1/dims))`, computed by
    /// integer binary search. Exact-power and off-by-one edges are where an
    /// f32 root would drift, and dimension/entry zero are the saturating
    /// corners symphonia's release build hits.
    #[test]
    fn vorbis_lookup1_values_matches_the_spec() {
        assert_eq!(vorbis_lookup1_values(8, 3), 2); // 2^3 = 8
        assert_eq!(vorbis_lookup1_values(7, 3), 1); // 2^3 > 7
        assert_eq!(vorbis_lookup1_values(27, 3), 3); // 3^3 = 27
        assert_eq!(vorbis_lookup1_values(26, 3), 2); // 3^3 > 26
        assert_eq!(vorbis_lookup1_values(1, 1), 1);
        assert_eq!(vorbis_lookup1_values(16_777_215, 1), 16_777_215); // v^1 <= entries
        assert_eq!(vorbis_lookup1_values(0, 4), 0);
        assert_eq!(vorbis_lookup1_values(100, 0), u64::MAX); // saturating corner
    }

    /// The rename bypass, closed: a hostile Vorbis stream served under any
    /// extension reaches the same decoder, so the guard runs at the decoder
    /// boundary, not by file name. A book declaring a 2^26-value table (which
    /// fits u32, so symphonia's probe demuxes it without panicking) is refused
    /// whether the URL says `.ogg` or `.mp3`, and whether it arrives as a
    /// bank sample or a soundfont zone (which decodes through `decode_mp3`).
    /// symphonia would stop this particular book itself, short of the table,
    /// for want of its multiplicands, so this test pins *where* the guard runs;
    /// the type-1 test below pins what it prevents.
    #[test]
    fn a_hostile_vorbis_setup_is_refused_at_every_entry_point() {
        let mut bits = TestBits::new();
        bits.push(0, 8); // one codebook
        oversized_codebook(&mut bits, 8, 8_388_608, 2); // 2^26-value table, fits u32
        let ogg = vorbis_header_ogg(&setup_packet(bits));

        for url in [
            "https://example.test/attack.ogg",
            "https://example.test/attack.mp3",
        ] {
            let error = decode_guarded(url, &ogg, Some(48_000))
                .expect_err("the hostile setup must be refused");
            assert!(error.contains("cap"), "{url} names the value cap: {error}");
            // A refusal does not depend on gapless trimming, so it is reported
            // once rather than again from a pointless non-gapless retry.
            assert!(
                !error.contains("without gapless trimming"),
                "{url} reports the refusal once: {error}"
            );
        }
        // The soundfont zone path decodes every non-RIFF zone through
        // `decode_mp3`; the guard fires there too.
        let error = decode_mp3(&ogg).expect_err("the zone path must refuse it");
        assert!(
            error.contains("cap"),
            "the zone path names the cap: {error}"
        );
    }

    /// A setup header split across two pages, hostile. symphonia's demuxer
    /// reassembles the packet from both pages before the codec sees it, so the
    /// guard inspects the whole header: a valid padding book fills the first
    /// page and the oversized book begins only on the continuation page. The
    /// old page-splicing pre-scan spliced the second page's 27-byte header into
    /// the packet and lost the walk; guarding the demuxed header cannot.
    #[test]
    fn a_hostile_setup_split_across_two_pages_is_refused() {
        let mut bits = TestBits::new();
        bits.push(1, 8); // two codebooks
        // A valid padding book: 512 entries of length 9 are a complete code
        // (512 x 2^-9 = 1), and their 320 bytes of dense lengths push the second
        // book past the first page's 255 bytes onto the continuation page.
        bits.push(0x564342, 24);
        bits.push(1, 16); // dimensions
        bits.push(512, 24); // entries
        bits.push(0, 1); // unordered
        bits.push(0, 1); // not sparse
        for _ in 0..512 {
            bits.push(8, 5); // length 9
        }
        bits.push(0, 4); // lookup type 0
        // The oversized book, now past the page boundary.
        oversized_codebook(&mut bits, 8, 8_388_608, 2);

        let setup = setup_packet(bits);
        assert!(setup.len() > 300, "the setup must span two pages");
        let ogg = vorbis_header_ogg_inner(&setup, true);
        let error = decode_guarded("https://example.test/split.ogg", &ogg, Some(48_000))
            .expect_err("the split hostile setup must be refused");
        assert!(error.contains("cap"), "names the value cap: {error}");
    }

    /// The committed real file: 30 ms of a 330 Hz tone, 8 kHz mono, encoded
    /// off-line by libvorbis into 2.7 KB.
    const TINY_VORBIS: &[u8] = include_bytes!("samples/testdata/tiny_vorbis.ogg");

    /// A decode that ran but produced silence passes every other check and is
    /// wrong only at the speaker, so a real file is held to audible output at
    /// its own rate (decoded with no context rate, so nothing resamples it).
    fn assert_audible(decoded: &DecodedSample) {
        assert!(decoded.frames() > 0, "decoded to no frames");
        assert_eq!(decoded.sample_rate(), 8_000, "the fixture's own rate");
        let peak = decoded
            .pcm()
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.05, "decoded to near silence: peak {peak}");
    }

    /// A real libvorbis file decodes to audible audio through the production
    /// entry point: the guard walks its 19 codebooks and refuses none. It has
    /// no length-ordered books and no type-2 lookups; other tests cover those.
    #[test]
    fn a_real_vorbis_file_decodes_through_the_guarded_path() {
        let decoded =
            decode_guarded("kick.ogg", TINY_VORBIS, None).expect("a real vorbis file must decode");
        assert_audible(&decoded);
    }

    /// The same file with its setup header split across two pages still
    /// decodes: the guard reads the reassembled header, not the raw page bytes.
    #[test]
    fn a_real_vorbis_setup_spanning_pages_still_decodes() {
        let repaginated = repaginate_setup_across_pages(TINY_VORBIS);
        let decoded = decode_guarded("kick.ogg", &repaginated, None)
            .expect("a real file with a page-spanning setup must decode");
        assert_audible(&decoded);
    }

    /// Reassemble the packets of a single-stream Ogg file in order, returning
    /// the serial, the final page's granule, and the packet payloads.
    fn ogg_packets(ogg: &[u8]) -> (u32, u64, Vec<Vec<u8>>) {
        let mut packets = Vec::new();
        let mut current = Vec::new();
        let (mut serial, mut final_gp) = (0u32, 0u64);
        let mut cursor = 0usize;
        while cursor + 27 <= ogg.len() && &ogg[cursor..cursor + 4] == b"OggS" {
            serial = u32::from_le_bytes(ogg[cursor + 14..cursor + 18].try_into().unwrap());
            let absgp = u64::from_le_bytes(ogg[cursor + 6..cursor + 14].try_into().unwrap());
            if absgp != u64::MAX {
                final_gp = final_gp.max(absgp);
            }
            let segments = usize::from(ogg[cursor + 26]);
            let table = &ogg[cursor + 27..cursor + 27 + segments];
            let mut offset = cursor + 27 + segments;
            for &lacing in table {
                current.extend_from_slice(&ogg[offset..offset + usize::from(lacing)]);
                offset += usize::from(lacing);
                if lacing < 255 {
                    packets.push(std::mem::take(&mut current));
                }
            }
            cursor = offset;
        }
        (serial, final_gp, packets)
    }

    /// Re-lay a single-stream Ogg file so the setup packet spans two pages,
    /// keeping the identification, comment, and audio packets intact.
    fn repaginate_setup_across_pages(ogg: &[u8]) -> Vec<u8> {
        let (serial, final_gp, packets) = ogg_packets(ogg);
        assert!(packets.len() >= 4, "need ident, comment, setup, and audio");
        let mut out = ogg_page_packets(&[&packets[0]], 0x02, 0, serial, 0); // ident (BOS)
        out.extend(ogg_page_packets(&[&packets[1]], 0x00, 1, serial, 0)); // comment
        out.extend(split_packet_pages(&packets[2], 2, serial)); // setup (seq 2, 3)
        let audio: Vec<&[u8]> = packets[3..].iter().map(Vec::as_slice).collect();
        out.extend(ogg_page_packets(&audio, 0x04, 4, serial, final_gp)); // audio (EOS)
        out
    }
}
#[cfg(test)]
pub(crate) mod wait_tests {
    //! Offline sample waits stop without making the loader finish its work.

    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use super::SampleLibrary;

    thread_local! {
        static WAIT_ENTERED: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
    }

    /// Observe a pending sample wait on this thread after its cancellation guard.
    pub(crate) fn watch_next_wait() -> Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        WAIT_ENTERED.with(|slot| {
            assert!(slot.replace(Some(sender)).is_none(), "wait already watched");
        });
        receiver
    }

    pub(super) fn entering_wait() {
        WAIT_ENTERED.with(|slot| {
            if let Some(sender) = slot.borrow_mut().take() {
                let _ = sender.send(());
            }
        });
    }

    fn assert_empty_render_cancels(cancel_during_wait: bool) {
        let library = SampleLibrary::empty();
        let hold = library.hold_manifest_worker_for_test();
        let cancellation = AtomicBool::new(!cancel_during_wait);
        let folder = tempfile::tempdir().expect("render directory");
        let path = folder.path().join("cancelled.wav");
        let (finished, completion) = mpsc::channel();
        let (watching, watch) = mpsc::channel();

        let (result, entered, pending) = std::thread::scope(|scope| {
            scope.spawn(|| {
                if cancel_during_wait {
                    watching.send(watch_next_wait()).expect("wait observer");
                }
                let rendered = crate::render::write_scalar_wav_controlled_with_dispatch(
                    &path,
                    48_000,
                    0.1,
                    &[],
                    0.5,
                    Some(&library),
                    false,
                    rustel_audio::RenderControl {
                        cancelled: Some(&cancellation),
                        ..Default::default()
                    },
                    rustel_audio::WavSampleFormat::Pcm16,
                    rustel_audio::DspDispatch::portable(),
                    128,
                );
                let _ = finished.send(rendered);
            });
            let entered = if cancel_during_wait {
                Some(
                    watch
                        .recv_timeout(Duration::from_secs(2))
                        .and_then(|entered| entered.recv_timeout(Duration::from_secs(2))),
                )
            } else {
                None
            };
            cancellation.store(true, Ordering::Relaxed);
            let result = completion.recv_timeout(Duration::from_secs(2));
            let pending = library.manifests_pending();
            // Release before the scoped join even if the regression timed out:
            // the held job is an inline map, with no network work behind it.
            drop(hold);
            (result, entered, pending)
        });

        if let Some(entered) = entered {
            entered.expect("cancellation arrived during the sample wait");
        }
        let error = result
            .expect("cancelled render returned while the manifest worker was held")
            .expect_err("a stopped render is not a completed file");
        assert_eq!(error.to_string(), rustel_audio::RENDER_CANCELLED);
        assert!(pending > 0, "the render waited for the held manifest");
        assert!(!path.exists(), "sample cancellation reached audio writing");
    }

    #[test]
    fn stopped_empty_render_does_not_wait_for_sample_manifests() {
        assert_empty_render_cancels(false);
    }

    #[test]
    fn empty_render_cancels_during_the_sample_manifest_wait() {
        assert_empty_render_cancels(true);
    }

    #[test]
    fn ordinary_sample_wait_still_waits_for_the_manifest_to_finish() {
        let library = SampleLibrary::empty();
        let hold = library.hold_manifest_worker_for_test();
        let (watching, watch) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let (entered, before_release, after_release) = std::thread::scope(|scope| {
            scope.spawn(|| {
                watching.send(watch_next_wait()).expect("wait observer");
                library.wait_until_idle(Duration::from_secs(5));
                let _ = finished.send(());
            });
            let entered = watch
                .recv_timeout(Duration::from_secs(2))
                .and_then(|entered| entered.recv_timeout(Duration::from_secs(2)));
            let before_release = completion.try_recv();
            drop(hold);
            let after_release = completion.recv_timeout(Duration::from_secs(2));
            (entered, before_release, after_release)
        });

        entered.expect("the ordinary wait reached the pending manifest");
        assert!(matches!(before_release, Err(mpsc::TryRecvError::Empty)));
        after_release.expect("the ordinary wait finished after the worker was released");
        assert_eq!(library.manifests_pending(), 0);
    }
}

/// How many sample-loader threads fetch at once. Enough to saturate a
/// typical link on bulk cache jobs without hammering the CDN; a live note
/// still jumps the queue and starts on the next free worker.
const SAMPLE_LOAD_WORKERS: usize = 6;

const MAX_SCORE_SAMPLE_MAP_BYTES: usize = 4 * 1024 * 1024;
/// Unique map-text bytes retained for source lookup and status history.
const MAX_SOURCE_TABLE_BYTES: usize = 32 * 1024 * 1024;
/// The most files a score's `samples()` maps may name, one map and the whole
/// session alike.
///
/// Naming a file costs one URL in the grant table. A slot in the decoded
/// bank is taken only when a sound plays, and the bank refuses there when it
/// is full. The limit is therefore not the decoded bank's capacity: a
/// library can name more files than the bank has slots, for example
/// `github:bubobubobubobubo/dough-waveforms` with 4,358 wavetables in 65
/// banks. The limit is the same as for the built-in manifests.
const MAX_SCORE_SAMPLE_FILES: usize = MAX_SAMPLE_MANIFEST_ENTRIES;
const MAX_SCORE_SAMPLE_SCAN_ENTRIES: usize = 65_536;

/// Maximum committed response bytes in the score-selected cache.
pub const SCORE_SAMPLE_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum number of score-selected responses kept on disk.
pub const SCORE_SAMPLE_CACHE_MAX_ENTRIES: usize = 4_096;
/// Maximum bytes one [`SampleLibrary`] may add during its lifetime.
pub const SCORE_SAMPLE_CACHE_SESSION_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum entries one [`SampleLibrary`] may add during its lifetime.
pub const SCORE_SAMPLE_CACHE_SESSION_MAX_ENTRIES: usize = 2_048;

const SCORE_CACHE_NAMESPACE: &str = "score";
const SCORE_CACHE_NO_LEGACY_MARKER: &str = ".score-cache-no-legacy";
// Persistent schema identity: product renames must not invalidate existing
// offline entries.
const SCORE_CACHE_KEY_DOMAIN_V1: &[u8] = b"strudel-score-cache-v1\0";

/// Host-granted resources that score-level `samples(...)` may use, and origin
/// grants for Hydra image sources.
///
/// An empty policy is deliberately inert: evaluating score text must not read
/// the working directory or choose a network destination. Hosts opt in with
/// exact HTTP(S) origins, one canonical local sample root, and/or the
/// strudel.cc parity default of [`Self::permit_public_cors_origins`].
/// Hydra images also require public HTTPS destinations and CORS consent,
/// including for explicitly granted origins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoreSampleAccess {
    local_root: Option<Arc<crate::sample_server::ServedRoot>>,
    allowed_origins: Vec<String>,
    open_public: bool,
}

/// The `Origin` this host presents when it requires CORS consent, and the
/// value a server's `Access-Control-Allow-Origin` answer is compared against.
/// strudel.cc is the origin whose reachable set this policy reproduces: a
/// server that would refuse the browser refuses us.
const CORS_REQUEST_ORIGIN: &str = "https://strudel.cc";

/// Whether an `Access-Control-Allow-Origin` answer consents to the read.
fn cors_consents(header: Option<&str>) -> bool {
    header
        .map(str::trim)
        .is_some_and(|value| value == "*" || value.eq_ignore_ascii_case(CORS_REQUEST_ORIGIN))
}

impl ScoreSampleAccess {
    pub fn denied() -> Self {
        Self::default()
    }

    /// Permit score-selected URLs on one exact HTTP(S) origin.
    ///
    /// The origin must not contain credentials, a path, query, or fragment.
    /// Redirects are confined to the same origin.
    pub fn permit_origin(&mut self, origin: &str) -> Result<(), String> {
        let parsed =
            Url::parse(origin).map_err(|error| format!("invalid sample origin: {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("sample origin must be an http or https origin".to_owned());
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("sample origin must not contain credentials".to_owned());
        }
        if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err("sample origin must not contain a path, query, or fragment".to_owned());
        }
        let canonical = parsed.origin().ascii_serialization();
        if canonical == "null" {
            return Err("sample origin is not a network origin".to_owned());
        }
        if !self.allowed_origins.contains(&canonical) {
            self.allowed_origins.push(canonical);
        }
        Ok(())
    }

    /// Permit `samples('local:...')` beneath one host-selected directory.
    pub fn permit_local_root(&mut self, root: impl AsRef<Path>) -> Result<(), String> {
        let root = root.as_ref();
        self.local_root = Some(Arc::new(crate::sample_server::ServedRoot::open(root)?));
        Ok(())
    }

    /// Permit any public **https** origin whose server consents to
    /// cross-origin reads - the strudel.cc parity policy.
    ///
    /// Consent is checked at the wire, per response: every hop of every
    /// manifest and audio fetch must answer with
    /// `Access-Control-Allow-Origin: *` (or `https://strudel.cc`), exactly the
    /// header without which the same URL fails on strudel.cc. The reachable
    /// set is therefore the browser's, not "everything": a host that never
    /// opted into scripted fetching stays unreachable. The address boundary
    /// is unchanged - only public addresses, and URLs that *name* loopback or
    /// use plain http still require an exact [`Self::permit_origin`] grant.
    pub fn permit_public_cors_origins(&mut self) {
        self.open_public = true;
    }

    pub fn is_denied(&self) -> bool {
        self.local_root.is_none() && self.allowed_origins.is_empty() && !self.open_public
    }

    fn approve_remote(&self, raw: &str) -> Result<(Arc<str>, ScoreFetchAccess), String> {
        let parsed = sample_fetch::wire_url(raw)
            .map_err(|error| format!("samples() URL is invalid: {error}"))?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("samples() URL contains credentials".to_owned());
        }
        let origin = parsed.origin().ascii_serialization();
        let access = if self.allowed_origins.contains(&origin) {
            // An exact operator grant carries its own trust: no consent
            // header is demanded, which is what lets a personal server
            // without CORS configuration be granted at all.
            ScoreFetchAccess::Remote {
                origin,
                cors_required: false,
            }
        } else if self.open_public
            && parsed.scheme() == "https"
            && !sample_fetch::url_names_loopback(&parsed)
        {
            ScoreFetchAccess::Remote {
                origin,
                cors_required: true,
            }
        } else if self.open_public {
            return Err(
                "samples() URL is outside the permitted sample origins (the default policy \
                 fetches public https origins that consent via CORS; grant this one \
                 explicitly with --allow-sample-origin)"
                    .to_owned(),
            );
        } else {
            return Err("samples() URL is outside the permitted sample origins".to_owned());
        };
        let normalized: Arc<str> = Arc::from(parsed.as_str());
        Ok((normalized, access))
    }

    /// Apply the same host-granted origin scope to a score's Hydra image.
    /// The image loader independently keeps its stricter HTTPS and CORS rules.
    #[cfg(feature = "hydra")]
    pub(crate) fn approve_hydra_image_url(&self, raw: &str) -> Result<(), String> {
        self.approve_remote(raw)
            .map(|_| ())
            .map_err(|_| "Hydra image URL is outside the permitted sample origins".to_owned())
    }

    /// The textual half of approval, run at enqueue time so a refused source
    /// string answers the evaluation that named it instead of racing process
    /// exit on the manifest worker. The worker re-runs the full approval and
    /// stays the authority; shapes that need fetched context (bank maps,
    /// nested manifests) are left to it - this must never touch the network.
    fn preapprove_effect(&self, map_json: &str) -> Result<(), String> {
        let Ok(serde_json::Value::String(source)) = serde_json::from_str(map_json) else {
            return Ok(());
        };
        match read_source(&source, GITHUB_SAMPLE_MANIFEST) {
            SampleSource::LocalFolder(_) => {
                if self.local_root.is_none() {
                    return Err(
                        "samples('local:') requires a host-selected local sample root".to_owned(),
                    );
                }
                Ok(())
            }
            SampleSource::Url(url) => self.approve_remote(&url).map(|_| ()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScoreFetchAccess {
    Remote {
        origin: String,
        /// Approved by the default policy rather than an exact grant: every
        /// response must consent via `Access-Control-Allow-Origin`.
        cors_required: bool,
    },
    Local {
        root: Arc<crate::sample_server::ServedRoot>,
    },
}

use crate::sample_fetch;

/// Compiled-in pin of the default bank manifests.
const PINNED_BANKS: &str = include_str!("../assets/sample-banks.json");

#[derive(Debug, serde::Deserialize)]
struct PinnedFonts {
    base: String,
    fonts: std::collections::BTreeMap<String, Vec<String>>,
}

/// Compiled-in pin of the General MIDI soundfont map.
const PINNED_GM_FONTS: &str = include_str!("../assets/gm-fonts.json");

/// A pack's base as a prefix its files start with: with the slash, so
/// `…/piano` does not claim `…/piano-two/`.
fn source_prefix(base: &str) -> std::borrow::Cow<'_, str> {
    if base.ends_with('/') {
        std::borrow::Cow::Borrowed(base)
    } else {
        std::borrow::Cow::Owned(format!("{base}/"))
    }
}

/// Where the pinned packs live, by base prefix: the category each pack's
/// pin declares, lowercased with `source_prefix`'s trailing slash, longest
/// prefix first so a pack nested inside another's files wins. Read once
/// from the compiled-in pin; the pin is the only place a shipped pack's
/// shelf is named.
type DefaultCategories = Vec<(String, SoundCategory)>;

fn default_categories() -> &'static DefaultCategories {
    static CATEGORIES: std::sync::OnceLock<DefaultCategories> = std::sync::OnceLock::new();
    CATEGORIES.get_or_init(|| {
        let Ok(pinned) = serde_json::from_str::<PinnedFile>(PINNED_BANKS) else {
            return Vec::new();
        };
        let mut declared: Vec<(String, SoundCategory)> = pinned
            .sources
            .iter()
            .filter_map(|source| {
                let base = source.base.as_deref()?;
                let category = source.category?;
                Some((source_prefix(base).to_lowercase(), category))
            })
            .chain(
                pinned
                    .inline
                    .category
                    .map(|category| (source_prefix(&pinned.inline.base).to_lowercase(), category)),
            )
            .collect();
        declared.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| b.0.cmp(&a.0)));
        declared
    })
}

#[derive(Debug, serde::Deserialize)]
struct PinnedFile {
    sources: Vec<PinnedSource>,
    inline: PinnedInline,
}

#[derive(Debug, serde::Deserialize)]
struct PinnedSource {
    name: String,
    url: String,
    base: Option<String>,
    sha256: String,
    /// The browser shelf the pack's banks file under, as the pin says.
    /// None for a pin that only maps aliases onto another pack's banks:
    /// it brings no files, so it has nothing to file.
    category: Option<SoundCategory>,
}

#[derive(Debug, serde::Deserialize)]
struct PinnedInline {
    base: String,
    /// The shelf the inline banks file under, as the pin says.
    category: Option<SoundCategory>,
    banks: HashMap<String, Vec<String>>,
}

/// One of the packs the studio ships with, as the Sources page lists it
/// under the packs the player imported: a pinned manifest, the inline
/// banks, or the General MIDI soundfonts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultSource {
    /// The pack's short name, as the pin file names it.
    pub name: String,
    /// Where its list lives - the address its row shows.
    pub url: String,
    /// Where its files live: every url under this is the pack's, which is
    /// how its banks are told apart from the others' once they are all
    /// merged into one map.
    pub base: String,
}

/// What caching a shipped pack onto disk asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheRequest {
    /// Sounds the library knows under the pack right now.
    pub sounds: usize,
    /// Distinct files those sounds name.
    pub files: usize,
    /// Files put in the loader's line. The rest are on disk already, or
    /// were on their way for a note.
    pub queued: usize,
}

/// The shipped packs a pin file and a font map describe. Only the packs
/// that bring files: an alias-only pin has no row's worth of anything.
fn default_sources_from(pinned_banks: &str, pinned_fonts: &str) -> Vec<DefaultSource> {
    let mut sources = Vec::new();
    if let Ok(pinned) = serde_json::from_str::<PinnedFile>(pinned_banks) {
        for source in pinned.sources {
            if let Some(base) = source.base {
                sources.push(DefaultSource {
                    name: source.name,
                    url: source.url,
                    base,
                });
            }
        }
        if !pinned.inline.banks.is_empty() {
            let base = pinned.inline.base;
            sources.push(DefaultSource {
                name: base
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("built-in")
                    .to_owned(),
                url: base.clone(),
                base,
            });
        }
    }
    if let Ok(fonts) = serde_json::from_str::<PinnedFonts>(pinned_fonts)
        && !fonts.fonts.is_empty()
    {
        sources.push(DefaultSource {
            name: "gm soundfonts".to_owned(),
            url: fonts.base.clone(),
            base: fonts.base,
        });
    }
    sources
}

/// Banks baked into the pin file - Dirt-Samples and friends - not fetched
/// from a remote manifest.
fn inline_banks_from(pinned: &PinnedFile) -> HashMap<String, Bank> {
    pinned
        .inline
        .banks
        .iter()
        .map(|(name, files)| {
            let urls = files
                .iter()
                .map(|file| Arc::from(join_url(&pinned.inline.base, file)))
                .collect();
            (name.clone(), Bank::Array(urls))
        })
        .collect()
}

/// What the pin file says an inline pack holds, when its banks are not
/// in the live map yet - after a refresh cleared them, or before the
/// first poll.
fn pinned_inline_files(source: &DefaultSource) -> Option<(usize, HashSet<Arc<str>>)> {
    let pinned: PinnedFile = serde_json::from_str(PINNED_BANKS).ok()?;
    if pinned.inline.banks.is_empty()
        || source_prefix(&source.base) != source_prefix(&pinned.inline.base)
    {
        return None;
    }
    let mut files = HashSet::new();
    for file_list in pinned.inline.banks.values() {
        for file in file_list {
            files.insert(Arc::from(join_url(&pinned.inline.base, file)));
        }
    }
    Some((pinned.inline.banks.len(), files))
}

#[derive(Clone)]
enum Bank {
    /// Urls in file order; `n` indexes with round + euclidean wrap.
    Array(Vec<Arc<str>>),
    /// (midi, urls) per note key, in JSON key order for the closest-key
    /// reduce tie-break (`<` strictly-closer keeps the FIRST of equals).
    Notes(Vec<(f64, Vec<Arc<str>>)>),
}

/// Which rate a decoded file lands at.
///
/// An ordinary sample arrives at the context's rate. A wavetable decodes at
/// the FILE's rate so its frames stay aligned to the file: slicing
/// 2048-sample frames out of a resampled buffer would cut each one at the
/// wrong stride and play a different waveform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeRate {
    Context,
    Native,
}

impl DecodeRate {
    /// `registerSampleSource` sends every `wt_`-named bank to the wavetable
    /// loader; everything else goes through the sampler.
    fn for_sound(name: &str) -> Self {
        if name.starts_with("wt_") {
            Self::Native
        } else {
            Self::Context
        }
    }
}

enum UrlState {
    Loading,
    Ready {
        id: SampleId,
        duration_secs: f64,
    },
    /// When it failed, so a later ask knows whether it has rested.
    Failed {
        at: Instant,
    },
}

/// Where one sound of a score stands before it is asked to play.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundReadiness {
    /// Decoded and ready to sound on its first onset.
    Ready,
    /// Known, and on its way.
    Loading,
    /// Known, and its download or decode failed.
    Failed,
    /// No bank of that name.
    Unknown,
}

/// Where a browsable sound came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundOrigin {
    /// The pinned default manifests.
    Default,
    /// A `samples(…)` call in a score.
    Score,
    /// A General MIDI soundfont.
    Font,
    /// A sound the voice engine makes by itself: an oscillator, a noise,
    /// `supersaw`, `sbd`.
    Synth,
    /// The live audio input, `in`: one channel a voice, `in:1` the next.
    Input,
    /// Audio sitting in the open set's own folder, taken up without the
    /// score asking for it.
    Set,
    /// A folder or pack the player imported in Settings, which every set
    /// sees.
    Global,
}

/// What kind of sound a browser files an entry under: the pinned banks
/// by the shelf their pin declares, the synths, the soundfonts, and the
/// score's own `samples(…)`. A browser filters by these; twenty of them
/// would still be a list to pick from, not twenty keys to remember.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SoundCategory {
    Synth,
    Drums,
    Percussion,
    Piano,
    Orchestra,
    Wavetable,
    Dirt,
    #[serde(rename = "gm")]
    Font,
    Score,
    Set,
    /// A source the player imported in Settings.
    Mine,
    Other,
}

impl SoundCategory {
    /// Every category, in the order a chooser lists them.
    pub const ALL: [Self; 12] = [
        Self::Synth,
        Self::Drums,
        Self::Percussion,
        Self::Piano,
        Self::Orchestra,
        Self::Wavetable,
        Self::Dirt,
        Self::Font,
        Self::Score,
        Self::Set,
        Self::Mine,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Synth => "synth",
            Self::Drums => "drums",
            Self::Percussion => "percussion",
            Self::Piano => "piano",
            Self::Orchestra => "orchestra",
            Self::Wavetable => "wavetable",
            Self::Dirt => "dirt",
            Self::Font => "gm",
            Self::Score => "score",
            Self::Set => "set",
            Self::Mine => "mine",
            Self::Other => "other",
        }
    }

    /// The category of a pinned bank, read off the pin: each pack in
    /// `sample-banks.json` declares the browser shelf its banks belong
    /// on. A bank whose first url is under no declared pack's base files
    /// as Other.
    fn of_location(location: Option<&str>) -> Self {
        Self::of_location_with(location, default_categories())
    }

    /// [`Self::of_location`] against a pin's declared categories.
    fn of_location_with(location: Option<&str>, declared: &DefaultCategories) -> Self {
        let Some(url) = location else {
            return Self::Other;
        };
        let url = url.to_ascii_lowercase();
        declared
            .iter()
            .find(|(prefix, _)| url.starts_with(prefix.as_str()))
            .map(|(_, category)| *category)
            .unwrap_or(Self::Other)
    }
}

/// The upstream repository address of a file the pinned library holds on
/// the strudel CDN.
///
/// The CDN serves the files but refuses directory listings: a bank's address
/// there answers `403 Forbidden`. Each pinned manifest in
/// `assets/sample-banks.json` names the repository of its files as its
/// `_base`, and the table below repeats those repositories. The rewritten
/// address opens a page that renders. A URL outside the pinned CDN
/// collections is not rewritten.
pub fn upstream_file_url(url: &str) -> Option<String> {
    const CDN: &str = "https://strudel.b-cdn.net/";
    const RAW: &str = "https://raw.githubusercontent.com/";
    // The CDN folder is dropped, and what stands under it is the path
    // within the repository itself - root and branch, as each manifest's
    // `_base` spells them. Matched case-insensitively (the VCSL base is
    // uppercase), spelled back the way GitHub itself spells them.
    let path = url.strip_prefix(CDN)?;
    let (root, rest) = path.split_once('/')?;
    if rest.is_empty() {
        return None;
    }
    let repo = match root.to_ascii_lowercase().as_str() {
        "piano" => "felixroos/dough-samples/main/piano/",
        "vcsl" => "sgossner/VCSL/master/",
        "tidal-drum-machines" => "ritchse/tidal-drum-machines/main/",
        "uzu-drumkit" => "tidalcycles/uzu-drumkit/main/",
        "uzu-wavetables" => "tidalcycles/uzu-wavetables/main/",
        "mrid" => "yaxu/mrid/main/",
        "dirt-samples" => "tidalcycles/Dirt-Samples/master/",
        _ => return None,
    };
    Some(format!("{RAW}{repo}{rest}"))
}

/// One sound the library can play, for a browser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundEntry {
    pub name: String,
    /// How many numbered variants `name:n` can pick from.
    pub variants: usize,
    /// File stems for local variants, in the same order as `name:n`.
    /// Remote packs leave this empty: their implementation filenames are
    /// rarely useful, while a recording's filename is the name its owner
    /// chose on disk.
    pub variant_names: Vec<String>,
    pub origin: SoundOrigin,
    pub category: SoundCategory,
    /// The address of its first file - where the bank came from.
    pub location: Option<String>,
    /// The `samples(…)` import that brought a score's bank, as written -
    /// `github:user/repo` - for a browser to file it under. None for a
    /// bank from an inline map, and for everything not the score's.
    pub import: Option<String>,
}

impl SoundEntry {
    pub fn variant_label(&self, variant: usize) -> String {
        let indexed = format!("{}:{variant}", self.name);
        self.variant_names
            .get(variant)
            .filter(|name| !name.is_empty())
            .map_or(indexed.clone(), |name| format!("{indexed} ({name})"))
    }
}

fn local_sample_file_stem(location: &str) -> Option<String> {
    let path = location.strip_prefix("file://")?;
    std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
}

fn local_variant_names(bank: &Bank) -> Vec<String> {
    let names: Vec<String> = match bank {
        Bank::Array(urls) => urls
            .iter()
            .map(|url| local_sample_file_stem(url).unwrap_or_default())
            .collect(),
        Bank::Notes(notes) => {
            let variants = notes.iter().map(|(_, urls)| urls.len()).max().unwrap_or(0);
            (0..variants)
                .map(|index| {
                    notes
                        .iter()
                        .find_map(|(_, urls)| urls.get(index))
                        .and_then(|url| local_sample_file_stem(url))
                        .unwrap_or_default()
                })
                .collect()
        }
    };
    if names.iter().any(|name| !name.is_empty()) {
        names
    } else {
        Vec::new()
    }
}

/// A source the player added in Settings: a folder on disk, or a pack at a
/// URL. A pack may use the shorthands a score can write, such as
/// `github:tidalcycles/dirt-samples`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalSource {
    pub spec: String,
    /// Off keeps the row without its sounds: a pack you want back next
    /// week should not have to be typed again.
    pub enabled: bool,
}

/// How one of those sources stands, for the browser and the settings sheet
/// to show. A source is never fatal, so every failure is a state here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalSourceState {
    /// Its names are in.
    Ready { banks: usize },
    /// A pack whose manifest is still on its way.
    Loading,
    /// The folder or file is not there - an unplugged drive, a renamed
    /// path.
    Missing(String),
    /// It is there and would not read.
    Failed(String),
    /// Turned off in Settings.
    Off,
}

/// One row of [`SampleLibrary::adopt_global_sources`]'s answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalSourceReport {
    pub spec: String,
    pub state: GlobalSourceState,
}

/// Where a registration's names land.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BankLayer {
    /// A score's own `samples(…)`, and the open set's folder.
    Score,
    /// A pack the player added in Settings, under the adoption that asked
    /// for it: a list that lands after the next adoption is thrown away.
    Global { generation: u64 },
}

/// One row of Settings ▸ Samples, as the library holds it.
struct GlobalSlot {
    row: usize,
    spec: String,
    kind: GlobalKind,
    /// What the row brought. A pack keeps its last list while a new one is
    /// being fetched, so a refetch does not silence it in the meantime.
    banks: HashMap<String, Bank>,
    state: GlobalSourceState,
    /// A file was deleted while this folder was being walked, so the walk
    /// under way may still list it: its result is dropped and the folder
    /// is walked again.
    rewalk: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GlobalKind {
    Folder,
    Pack,
}

/// Whether a Settings source is a folder already on this machine, so
/// there is nothing to download: a path, or a `local:` folder.
pub fn source_is_local(spec: &str) -> bool {
    folder_of_spec(spec).is_some()
}

/// Keep the least-specific enabled local folders. An enabled parent owns all
/// banks below it, so retaining a child would only scan and list the same
/// files twice. Remote sources and disabled roots that no enabled parent
/// owns are preserved in their order.
pub fn deduplicated_global_sources(sources: &[GlobalSource]) -> Vec<GlobalSource> {
    deduplicated_global_sources_with_owners(sources).0
}

/// Deduplicate sources and report each removed source's surviving owner.
/// Preferences use the ownership pairs to move source-scoped bank aliases
/// before the redundant child row disappears.
pub fn deduplicated_global_sources_with_owners(
    sources: &[GlobalSource],
) -> (Vec<GlobalSource>, Vec<(String, String)>) {
    let local_paths: Vec<(usize, String, bool)> = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let folder = folder_of_spec(&source.spec)?;
            let path = Path::new(&folder).canonicalize().ok()?;
            let key = path
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_ascii_lowercase();
            Some((index, key, source.enabled))
        })
        .collect();
    let mut enabled: Vec<(usize, String)> = local_paths
        .iter()
        .filter(|(_, _, enabled)| *enabled)
        .map(|(index, path, _)| (*index, path.clone()))
        .collect();
    enabled.sort_by_key(|(_, path)| (path.len(), path.clone()));
    let mut accepted: Vec<(usize, String)> = Vec::new();
    for (index, path) in enabled {
        let is_covered = accepted.iter().any(|(_, parent)| {
            path == *parent
                || path
                    .strip_prefix(parent)
                    .is_some_and(|rest| rest.starts_with(['/', '\\']))
        });
        if !is_covered {
            accepted.push((index, path));
        }
    }
    let owners: HashMap<usize, usize> = local_paths
        .iter()
        .filter_map(|(index, path, _)| {
            accepted
                .iter()
                .find(|(parent_index, parent)| {
                    index != parent_index
                        && (path == parent
                            || path
                                .strip_prefix(parent)
                                .is_some_and(|rest| rest.starts_with(['/', '\\'])))
                })
                .map(|(parent_index, _)| (*index, *parent_index))
        })
        .collect();
    let kept = sources
        .iter()
        .enumerate()
        .filter(|(index, _)| !owners.contains_key(index))
        .map(|(_, source)| source.clone())
        .collect();
    let mut ownership_indices: Vec<(usize, usize)> = owners.into_iter().collect();
    ownership_indices.sort_by_key(|(child, _)| *child);
    let ownership = ownership_indices
        .into_iter()
        .filter_map(|(child, parent)| {
            let child = sources.get(child)?.spec.clone();
            let parent = sources.get(parent)?.spec.clone();
            (child != parent).then_some((child, parent))
        })
        .collect();
    (kept, ownership)
}

/// The distinct file URLs a bank names.
fn bank_file_urls(bank: &Bank) -> Vec<&Arc<str>> {
    match bank {
        Bank::Array(urls) => urls.iter().collect(),
        Bank::Notes(notes) => notes.iter().flat_map(|(_, urls)| urls.iter()).collect(),
    }
}

fn source_sort_key(slot: &GlobalSlot) -> String {
    folder_of_spec(&slot.spec)
        .and_then(|folder| Path::new(&folder).canonicalize().ok())
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| slot.spec.to_ascii_lowercase())
}

/// Rebuild the flat imported layer from the rows. Banks with the same name
/// get stable source-scoped aliases rather than silently replacing one another.
fn rebuild_global(shared: &Shared, global: &RwLock<HashMap<String, Bank>>) {
    let slots = shared.global_slots.lock().expect("global source slots");
    let renames = shared.renames.read().expect("bank renames");
    let source_renames = shared.source_renames.read().expect("source bank renames");
    let mut auto_aliases = shared.auto_aliases.write().expect("automatic bank aliases");
    let mut flat: HashMap<String, Bank> = HashMap::new();
    let mut source_of: HashMap<String, Arc<str>> = HashMap::new();
    let mut rows: Vec<&GlobalSlot> = slots.iter().collect();
    rows.sort_by_key(|slot| (source_sort_key(slot), slot.row));
    for slot in rows {
        // Off and unreadable rows bring nothing; a pack still fetching keeps
        // what it brought last time.
        if matches!(
            slot.state,
            GlobalSourceState::Off | GlobalSourceState::Missing(_) | GlobalSourceState::Failed(_)
        ) && slot.banks.is_empty()
        {
            continue;
        }
        if matches!(slot.state, GlobalSourceState::Off) {
            continue;
        }
        let mut banks: Vec<_> = slot.banks.iter().collect();
        banks.sort_by_key(|(name, _)| *name);
        for (original, bank) in banks {
            let identity = (slot.spec.clone(), original.clone());
            let manual = source_renames
                .get(&identity)
                .or_else(|| renames.get(original));
            let requested = manual.cloned().unwrap_or_else(|| original.clone());
            let remembered = manual
                .is_none()
                .then(|| auto_aliases.get(&identity).cloned())
                .flatten();
            let name = if remembered
                .as_ref()
                .is_some_and(|name| !flat.contains_key(name))
            {
                remembered.expect("checked above")
            } else if !flat.contains_key(&requested) {
                requested.clone()
            } else {
                let mut suffix = 1usize;
                loop {
                    let candidate = format!("{requested}_{suffix}");
                    if !flat.contains_key(&candidate)
                        && !auto_aliases.values().any(|alias| alias == &candidate)
                    {
                        break candidate;
                    }
                    suffix += 1;
                }
            };
            if name != requested {
                auto_aliases.insert(identity, name.clone());
            }
            source_of.insert(name.clone(), Arc::from(slot.spec.as_str()));
            flat.insert(name, bank.clone());
        }
    }
    drop(auto_aliases);
    drop(source_renames);
    drop(renames);
    drop(slots);
    // One write each, so the browser never sees a moment with the old
    // sources gone and the new ones not yet in.
    let mut global = global.write().expect("global banks");
    *shared.global_file_names.write().expect("global filenames") =
        local_names::index(flat.values());
    *global = flat;
    drop(global);
    *shared.global_source_of.write().expect("global sources") = source_of;
}

/// A pack's list has landed: fill its row, unless the row has been
/// replaced since the fetch was asked for.
fn fill_global_slot(
    shared: &Shared,
    spec: Option<&str>,
    generation: u64,
    banks: Vec<(String, Bank)>,
) -> bool {
    if shared.global_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let Some(spec) = spec else {
        return false;
    };
    let mut slots = shared.global_slots.lock().expect("global source slots");
    let Some(slot) = slots
        .iter_mut()
        .find(|slot| slot.kind == GlobalKind::Pack && slot.spec == spec)
    else {
        return false;
    };
    slot.state = if banks.is_empty() {
        GlobalSourceState::Failed("brought no sounds".to_owned())
    } else {
        GlobalSourceState::Ready { banks: banks.len() }
    };
    slot.banks = banks.into_iter().collect();
    true
}

/// What became of a finished folder walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FolderFill {
    /// The row holds what the walk found.
    Filled,
    /// The row was replaced since the walk was asked for.
    Superseded,
    /// A file was deleted from the row during the walk: walk it again.
    Stale,
}

/// A folder's walk has finished: fill its row with what it holds and how
/// it stands, unless the row has been replaced since the walk was asked
/// for or a file was deleted from it meanwhile.
fn fill_global_folder_slot(
    shared: &Shared,
    spec: &str,
    generation: u64,
    banks: Vec<(String, Bank)>,
    state: GlobalSourceState,
) -> FolderFill {
    if shared.global_generation.load(Ordering::Acquire) != generation {
        return FolderFill::Superseded;
    }
    let mut slots = shared.global_slots.lock().expect("global source slots");
    let Some(slot) = slots
        .iter_mut()
        .find(|slot| slot.kind == GlobalKind::Folder && slot.spec == spec)
    else {
        return FolderFill::Superseded;
    };
    if std::mem::take(&mut slot.rewalk) {
        return FolderFill::Stale;
    }
    slot.state = state;
    slot.banks = banks.into_iter().collect();
    FolderFill::Filled
}

/// What one imported folder holds, and how its row should read: missing
/// when the folder is not there, failed when it is there and will not
/// read.
fn walk_global_folder(folder: &str) -> (Vec<(String, Bank)>, GlobalSourceState) {
    let root = Path::new(folder.trim());
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            let why = format!("cannot read {}: {error}", root.display());
            let state = if error.kind() == std::io::ErrorKind::NotFound {
                GlobalSourceState::Missing(why)
            } else {
                GlobalSourceState::Failed(why)
            };
            return (Vec::new(), state);
        }
    };
    match folder_banks(&root, &[]) {
        Ok(banks) => {
            let count = banks.len();
            (banks, GlobalSourceState::Ready { banks: count })
        }
        Err(error) => (Vec::new(), GlobalSourceState::Failed(error)),
    }
}

/// Whether a source spelling is a folder on this machine: a bare absolute
/// path, or the `local:` spelling a score would use. A drop and the
/// picker hand over bare paths; nothing tells anyone to type `local:`.
pub fn folder_of_spec(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if Path::new(spec).is_absolute() {
        return Some(spec.to_owned());
    }
    match read_source(spec, "") {
        SampleSource::LocalFolder(folder) => Some(folder),
        SampleSource::Url(_) => None,
    }
}

/// A loader failure, said once, with the `samples(…)` maps it leaves
/// failed, spelled as the library keeps them: none unless it is an
/// import's.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SampleFailure {
    pub message: String,
    pub maps: Vec<String>,
}

impl From<String> for SampleFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            maps: Vec::new(),
        }
    }
}

/// Where a `samples(…)` the score wrote stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceState {
    /// Asked for; its map is on its way.
    Loading,
    /// Its map is in, and the names it brought are the library's.
    Ready,
    /// Its map could not be read, and this is why.
    Failed(String),
}

/// Where a map stands, and since when: a failure rests from `since`
/// before it is asked for again.
struct Standing {
    state: SourceState,
    since: Instant,
    /// How many failures in a row this one ends; 0 for any other state.
    failures: u32,
}

impl Standing {
    fn now(state: SourceState) -> Self {
        Self {
            state,
            since: Instant::now(),
            failures: 0,
        }
    }

    fn failed(&self) -> bool {
        matches!(self.state, SourceState::Failed(_))
    }

    /// How long a failure rests before the background asks again:
    /// [`FAILED_RETRY_AFTER`], doubled for each failure in a row before it,
    /// up to [`FAILED_RETRY_LONGEST`].
    fn rest(&self) -> Duration {
        FAILED_RETRY_AFTER
            .saturating_mul(2u32.saturating_pow(self.failures.saturating_sub(1)))
            .min(FAILED_RETRY_LONGEST)
    }
}

/// One decoded webaudiofontdata zone: a pitched, possibly looping sample
/// keyed to a midi range.
#[derive(Clone)]
struct FontZone {
    key_lo: f64,
    key_hi: f64,
    /// `originalPitch - 100·coarseTune - fineTune` (cents).
    base_detune: f64,
    id: SampleId,
    duration_secs: f64,
    loop_secs: Option<(f64, f64)>,
}

enum FontState {
    Loading,
    Ready(Arc<Vec<FontZone>>),
    /// When it failed, so a later real ask knows whether it has rested.
    Failed {
        at: Instant,
    },
}

/// Explicit preloads cover a bank; live source scans are only guesses about
/// the next sound. Keep that distinction when a manifest delays the request.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum PrefetchIntent {
    #[default]
    Explicit,
    LiveWarm,
    /// Fetch onto disk so a later play does not wait on the network.
    /// Does not reserve a live-bank id or decode PCM.
    CacheDisk,
}

impl PrefetchIntent {
    /// Whether to report a name that this intent did not find.
    ///
    /// Only an explicit preload reports it. `preload("kik:1")` names a sound
    /// that does not exist, and the player can correct that.
    ///
    /// A live warm is a bet. It pairs each sound name with each `.bank()`
    /// name in the text, because the text does not show which bank holds
    /// which sound. Four banks and six sounds give twenty-four names, and
    /// most of them do not exist. `s("sawtooth")` is an oscillator, so no
    /// bank holds it. These misses are expected and are not reported.
    fn reports_unknown(self) -> bool {
        matches!(self, Self::Explicit)
    }

    fn priority(self) -> LoadPriority {
        match self {
            Self::Explicit => LoadPriority::Now,
            Self::LiveWarm | Self::CacheDisk => LoadPriority::Bet,
        }
    }
}

/// Why a sound was asked for, which is where it stands in the loader's line.
///
/// Several threads fetch at once, so order still matters: a bet placed on a
/// name as it was typed must not delay the sound a score is about to play.
/// What plays now goes first; among bets the newest first, because the name
/// just typed is the one meant and the one before it was probably on the way
/// there. Idle workers take the next job; a live note therefore starts as
/// soon as any one of them is free, while cache bets keep filling the rest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadPriority {
    /// About to play, or asked for by an update or a preload.
    Now,
    /// Typed, and probably meant.
    Bet,
}

enum LoadKind {
    /// Fetch, decode, and seat in the live sample bank.
    Install {
        id: SampleId,
        decode_rate: DecodeRate,
    },
    /// Fetch onto disk only. The live bank is left alone: caching the
    /// whole library is thousands of files, and seating them would
    /// exhaust the 2048-slot bank before an evening of browsing could
    /// hand any id back.
    Cache,
}

struct LoadJob {
    url: Arc<str>,
    kind: LoadKind,
}

/// The loader's line, in two parts, with the thread waiting on it.
struct LoadQueue {
    state: Mutex<LoadQueueState>,
    wake: Condvar,
}

#[derive(Default)]
struct LoadQueueState {
    now: VecDeque<LoadJob>,
    bets: VecDeque<LoadJob>,
    closed: bool,
}

impl LoadQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(LoadQueueState::default()),
            wake: Condvar::new(),
        })
    }

    /// Queue a job. Returns `false` when the line is closed - the caller
    /// must not leave a claim that will never be worked.
    fn push(&self, job: LoadJob, priority: LoadPriority) -> bool {
        let mut state = self.state.lock().expect("sample load queue");
        if state.closed {
            return false;
        }
        match priority {
            LoadPriority::Now => state.now.push_back(job),
            LoadPriority::Bet => state.bets.push_back(job),
        }
        self.wake.notify_one();
        true
    }

    /// A bet that turned out to be needed now moves up the line.
    fn promote(&self, url: &str) {
        let mut state = self.state.lock().expect("sample load queue");
        if let Some(at) = state.bets.iter().position(|job| &*job.url == url)
            && let Some(job) = state.bets.remove(at)
        {
            state.now.push_back(job);
            self.wake.notify_one();
        }
    }

    fn take(state: &mut LoadQueueState) -> Option<LoadJob> {
        state.now.pop_front().or_else(|| state.bets.pop_back())
    }

    /// The next job, waiting for one; `None` once the line is closed and
    /// empty, which is the thread's cue to go.
    fn pop(&self) -> Option<LoadJob> {
        let mut state = self.state.lock().expect("sample load queue");
        loop {
            if let Some(job) = Self::take(&mut state) {
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = self.wake.wait(state).expect("sample load queue");
        }
    }

    #[cfg(any(test, all(feature = "test-support", feature = "device-audio")))]
    fn try_pop(&self) -> Option<LoadJob> {
        Self::take(&mut self.state.lock().expect("sample load queue"))
    }

    /// Nothing more will be queued.
    fn close(&self) {
        let mut state = self.state.lock().expect("sample load queue");
        state.closed = true;
        self.wake.notify_all();
    }
}

#[derive(Clone, Copy)]
enum DecodedIdentity {
    Unknown,
    Current(u64),
    Forgotten,
}

/// Buckets in a remembered sample shape. Wider than any preview row a
/// terminal can offer, so the drawing samples down rather than up.
pub const SHAPE_BUCKETS: usize = 512;

/// Shapes kept at once. A browsing session decodes a few hundred sounds;
/// past this the oldest go, because a shape is only ever wanted for the
/// sound sounding right now. Half a megabyte at worst.
const MAX_REMEMBERED_SHAPES: usize = 4_096;

/// What each decoded sample looked like, oldest first.
#[derive(Default)]
struct SampleShapes {
    by_url: HashMap<Arc<str>, Arc<[u8]>>,
    order: VecDeque<Arc<str>>,
}

impl SampleShapes {
    fn remember(&mut self, url: Arc<str>, shape: Arc<[u8]>) {
        if self.by_url.insert(Arc::clone(&url), shape).is_none() {
            self.order.push_back(url);
        }
        while self.order.len() > MAX_REMEMBERED_SHAPES {
            if let Some(oldest) = self.order.pop_front() {
                self.by_url.remove(&oldest);
            }
        }
    }
}

/// A decoded sample as [`SHAPE_BUCKETS`] peaks, 0 through 255.
///
/// The peak of each bucket rather than its average: a drum hit averaged
/// over its own length is a flat line, and the transient is the part a
/// reader recognises.
fn shape_of(decoded: &DecodedSample) -> Arc<[u8]> {
    let frames = decoded.frames();
    let channels = usize::from(decoded.channels().max(1));
    let pcm = decoded.pcm();
    let mut shape = vec![0u8; SHAPE_BUCKETS];
    if frames == 0 {
        return shape.into();
    }
    for (bucket, slot) in shape.iter_mut().enumerate() {
        let from = bucket * frames / SHAPE_BUCKETS;
        let to = ((bucket + 1) * frames / SHAPE_BUCKETS)
            .max(from + 1)
            .min(frames);
        let peak = pcm[from * channels..to * channels]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        *slot = (peak.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    shape.into()
}

/// The install queue and its intended payload identities share one publication
/// lock. Taking the PCM does not erase intent: the device may refuse the first
/// install before the producer has even attempted later entries in that batch.
struct ReadySamples {
    samples: Vec<(SampleId, DecodedSample)>,
    identities: [DecodedIdentity; SAMPLE_BANK_CAPACITY],
}

impl Default for ReadySamples {
    fn default() -> Self {
        let mut identities = [DecodedIdentity::Unknown; SAMPLE_BANK_CAPACITY];
        identities[BUNDLED_BD_SAMPLE_ID.0 as usize] =
            DecodedIdentity::Current(BUNDLED_BD_SAMPLE_IDENTITY);
        Self {
            samples: Vec::new(),
            identities,
        }
    }
}

impl ReadySamples {
    fn identity(&self, id: SampleId) -> Option<u64> {
        match self.identities.get(id.0 as usize)? {
            DecodedIdentity::Current(identity) => Some(*identity),
            DecodedIdentity::Unknown | DecodedIdentity::Forgotten => None,
        }
    }

    fn accept_identity(&mut self, id: SampleId, identity: u64) -> bool {
        let Some(slot) = self.identities.get_mut(id.0 as usize) else {
            return false;
        };
        match *slot {
            DecodedIdentity::Forgotten => false,
            DecodedIdentity::Current(current) if current > identity => false,
            _ => {
                *slot = DecodedIdentity::Current(identity);
                true
            }
        }
    }

    fn push(&mut self, (id, decoded): (SampleId, DecodedSample)) {
        // Decode identities increase monotonically; clones and recovery keep
        // theirs. An old retry must not replace a newer decode even after the
        // newer queue entry has already been taken for installation.
        if self.accept_identity(id, decoded.identity()) {
            self.samples.push((id, decoded));
        }
    }
}

#[derive(Default)]
struct SourceTables {
    sources: HashSet<String>,
    states: HashMap<String, Standing>,
    order: VecDeque<String>,
    bytes: usize,
}

impl SourceTables {
    fn note(&mut self, map: &str, state: SourceState) -> Option<Standing> {
        // Failed lookups may reach history after map validation refuses them.
        if map.len() > MAX_SCORE_SAMPLE_MAP_BYTES {
            return None;
        }
        let mut standing = Standing::now(state);
        if standing.failed() {
            standing.failures = self
                .states
                .get(map)
                .map_or(0, |previous| previous.failures)
                .saturating_add(1);
        }
        let known = self.states.contains_key(map) || self.sources.contains(map);
        if !known {
            self.order.push_back(map.to_owned());
            self.bytes = self.bytes.saturating_add(map.len());
        }
        if standing.state == SourceState::Ready {
            self.sources.insert(map.to_owned());
        }
        let previous = self.states.insert(map.to_owned(), standing);
        while self.bytes > MAX_SOURCE_TABLE_BYTES && self.order.len() > 1 {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(oldest.len());
            self.states.remove(&oldest);
            self.sources.remove(&oldest);
        }
        previous
    }

    fn forget_state(&mut self, map: &str) {
        self.states.remove(map);
        if !self.sources.contains(map) {
            self.order.retain(|known| known != map);
            self.bytes = self.order.iter().map(String::len).sum();
        }
    }
}

struct Shared {
    by_url: RwLock<HashMap<Arc<str>, UrlState>>,
    /// URLs introduced by score text and the exact host grant attached to
    /// each one. Loader threads consult this again at I/O time so a later
    /// refactor cannot validate registration and then fetch with broader
    /// authority.
    score_sources: RwLock<HashMap<Arc<str>, ScoreFetchAccess>>,
    ready: Mutex<ReadySamples>,
    /// A peak envelope per decoded url: what a sample looks like, for a
    /// browser to draw a preview against.
    ///
    /// Kept here because this is the only place that ever sees the PCM.
    /// The decoded body goes straight to the device and the library does
    /// not retain it, so anything asking afterwards has nothing to
    /// measure. One byte a bucket, a fixed [`SHAPE_BUCKETS`] of them, and
    /// the oldest fall out past `MAX_REMEMBERED_SHAPES`.
    shapes: Mutex<SampleShapes>,
    /// Published source maps and registration status share one retention
    /// budget. Publication and eviction are atomic for loader and UI readers.
    source_tables: Mutex<SourceTables>,
    /// The `samples(...)` import each of the score's banks came from, by
    /// bank name, as the argument was written.
    bank_imports: RwLock<HashMap<String, Arc<str>>>,
    /// The banks the open set's own folder brought, by name, each with the
    /// bank it displaced - so the next set can take them back out and put
    /// back what was there first. A set's samples belong to that set, and
    /// leaving them behind would let one set's `kicks` play in another;
    /// dropping them by name alone would take a score's `kicks` with them,
    /// which the set never supplied.
    set_banks: RwLock<BTreeMap<String, Option<Bank>>>,
    /// Hidden filename addresses; kept out of the browser's bank rows.
    custom_file_names: RwLock<local_names::Names>,
    global_file_names: RwLock<local_names::Names>,
    next_id: AtomicU32,
    /// Ids the studio handed back - forgotten by every table, cleared from
    /// the bank, past the last frame anything queued could name them at -
    /// for the next reservation to take before the counter moves. Oldest
    /// first, so a reissued id is as far as it can be from its last owner.
    free_ids: Mutex<VecDeque<SampleId>>,
    /// The loader's line. Closed when this goes, which is what ends the
    /// loader threads.
    jobs: Arc<LoadQueue>,
    /// Files the loader workers have in hand - fetching or decoding - for
    /// a progress line to name. Empty between jobs.
    loading: Mutex<HashSet<Arc<str>>>,
    /// URLs a disk-cache job has claimed, from queueing until the fetch
    /// settles, so a second cache pass does not enqueue the same file twice.
    caching: Mutex<HashSet<Arc<str>>>,
    /// Host-trusted sample cache directory this library's loader writes.
    host_cache: PathBuf,
    /// `{base}/{font}.js` for General MIDI files, as the font worker fetches.
    font_base: String,
    /// gm soundfont FILES (webaudiofontdata), keyed by font file name.
    fonts: RwLock<HashMap<Arc<str>, FontState>>,
    /// What the player renamed an imported bank to, by the name the scan
    /// or the pack gave it. An overlay rather than a rewrite: the files are
    /// untouched and the old name is what comes back if the rename is
    /// removed.
    renames: RwLock<HashMap<String, String>>,
    /// Aliases written by current versions, keyed by source and original name.
    source_renames: RwLock<HashMap<(String, String), String>>,
    /// Stable collision aliases keyed by source and original bank name.
    auto_aliases: RwLock<HashMap<(String, String), String>>,
    /// Settings ▸ Samples, row by row, as the library holds them: what each
    /// row was, what it brought, and how it stands. The flat `global` map is
    /// rebuilt from these in row order whenever one of them changes, which
    /// is what makes "a later row wins a shared name" true for a folder
    /// against a pack and not only for two of a kind.
    global_slots: Mutex<Vec<GlobalSlot>>,
    /// Bumped by every adoption. A pack's manifest lands on the loader's
    /// own schedule, so one enqueued before a re-adoption must not fill a
    /// slot the re-adoption has since replaced - or removed.
    global_generation: std::sync::atomic::AtomicU64,
    /// Which imported source each name in `global` came of, for the
    /// browser to file it under.
    global_source_of: RwLock<HashMap<String, Arc<str>>>,
    /// The fetch policy a score gets, handed in by the host so an imported
    /// pack is granted no less than a score would be: the exact origin the
    /// player typed on top of it.
    import_policy: Mutex<Option<ScoreSampleAccess>>,
    font_jobs: Arc<FontQueue>,
    /// One loud log line per failed url, never per onset.
    failures: Mutex<Vec<SampleFailure>>,
    /// Full-screen clients own the terminal and suppress informational
    /// loader writes; failures still travel through `failures` above.
    direct_diagnostic_logging: AtomicBool,
    /// Persistent score-selected bytes have their own namespace and budget.
    /// One library belongs to one Session, so this also owns that Session's
    /// share of the process-independent cache allowance.
    score_cache: ScoreCache,
    /// Manifest batches queued or active. Included in `wait_until_idle` so an
    /// offline render cannot mistake "the bank map has not arrived" for an
    /// unknown sound.
    manifest_pending: AtomicUsize,
    /// The last cancellation check and every externally visible manifest
    /// publication share this gate. Once `SampleLibrary::drop` returns, a
    /// worker can finish staging bytes but cannot publish them, a bank map, or
    /// a deferred preload.
    publication: Arc<PublicationGate>,
    /// The rate decoded PCM is converted to, as an AudioContext converts every
    /// file it decodes to its own. Owners set it from the render or device rate
    /// before a score can ask for a sound; see [`SampleLibrary::set_render_rate`].
    render_rate: AtomicU32,
    /// Moves whenever something a still-loading sound can be waiting on
    /// settles: a sample or a soundfont leaves Loading (ready or failed), or
    /// a manifest batch finishes. Bumped AFTER the table it describes has
    /// changed, so a reader that samples it before asking for a sound and
    /// sees it unchanged afterwards knows the answer it got still stands.
    settled_epoch: AtomicU64,
}

impl Shared {
    fn note_settled(&self) {
        self.settled_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Record where `map` stands and return where it stood. A failure
    /// counts one more in a row after a failure.
    fn note_source_standing(&self, map: &str, state: SourceState) -> Option<Standing> {
        self.source_tables
            .lock()
            .expect("samples source tables")
            .note(map, state)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.jobs.close();
        self.font_jobs.close();
    }
}

/// How long a failed download rests before a real ask tries it again. A
/// connection that dropped for a moment should not cost a sound for the
/// rest of the set; a server that is down should not be knocked on every
/// onset either.
const FAILED_RETRY_AFTER: Duration = Duration::from_secs(5);

/// The longest a `samples("…")` import that keeps failing rests before the
/// background asks again: a few [`MANIFEST_BATCH_TIMEOUT`]s, so one whose
/// server never answers leaves the loader free most of the time.
const FAILED_RETRY_LONGEST: Duration = MANIFEST_BATCH_TIMEOUT.saturating_mul(5);

/// How long a manifest job may take, counted from when the worker takes it.
const MANIFEST_BATCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MANIFEST_EFFECTS: usize = 64;
const MAX_MANIFEST_EFFECT_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUED_PRELOADS: usize = SAMPLE_BANK_CAPACITY;
const MAX_QUEUED_PRELOAD_BYTES: usize = 256 * 1024;
const MANIFEST_QUEUE_WAIT_SLICE: Duration = Duration::from_millis(10);

/// What a producer is told when the manifest line does not take its job.
fn manifest_refusal(refused: &mpsc::TrySendError<Box<ManifestJob>>) -> String {
    match refused {
        mpsc::TrySendError::Full(_) => "sample manifest queue is full; registration refused",
        mpsc::TrySendError::Disconnected(_) => "sample manifest loader stopped",
    }
    .to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationKind {
    Cache,
    Defaults,
    Custom,
    Local,
    Preload,
}

struct PublicationGate {
    lock: Mutex<()>,
    cancelled: Arc<AtomicBool>,
    #[cfg(any(test, feature = "test-support"))]
    barrier: Mutex<Option<TestPublicationBarrier>>,
}

impl PublicationGate {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-support"))]
            barrier: Mutex::new(None),
        }
    }

    fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    fn enter<'a>(
        &'a self,
        kind: PublicationKind,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<MutexGuard<'a, ()>, String> {
        #[cfg(any(test, feature = "test-support"))]
        self.wait_at_test_barrier(kind);
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = kind;
        let guard = self.lock.lock().expect("sample publication gate");
        budget.check()?;
        Ok(guard)
    }

    fn cancel(&self) {
        let _guard = self.lock.lock().expect("sample publication gate");
        self.cancelled.store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-support"))]
    fn install_test_barrier(
        &self,
        kind: PublicationKind,
    ) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (reached, reached_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let previous = self
            .barrier
            .lock()
            .expect("test publication barrier")
            .replace(TestPublicationBarrier {
                kind,
                reached,
                release: release_rx,
            });
        assert!(
            previous.is_none(),
            "only one publication barrier may be armed"
        );
        (reached_rx, release)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn wait_at_test_barrier(&self, kind: PublicationKind) {
        let barrier = {
            let mut slot = self.barrier.lock().expect("test publication barrier");
            if slot.as_ref().is_some_and(|barrier| barrier.kind == kind) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(barrier) = barrier {
            barrier.reached.send(()).expect("announce publication");
            barrier.release.recv().expect("release publication");
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
struct TestPublicationBarrier {
    kind: PublicationKind,
    reached: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
}

fn validate_manifest_effects<'a>(
    effects: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Result<(), String> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (map, base) in effects {
        count = count
            .checked_add(1)
            .ok_or_else(|| "samples() effect count overflowed".to_owned())?;
        if count > MAX_MANIFEST_EFFECTS {
            return Err(format!(
                "samples() registrations exceed the per-evaluation limit of {MAX_MANIFEST_EFFECTS}"
            ));
        }
        bytes = bytes
            .checked_add(map.len())
            .and_then(|bytes| bytes.checked_add(base.map_or(0, str::len)))
            .ok_or_else(|| "samples() effect byte count overflowed".to_owned())?;
        if bytes > MAX_MANIFEST_EFFECT_BYTES {
            return Err(format!(
                "samples() registrations exceed the per-evaluation byte limit of {MAX_MANIFEST_EFFECT_BYTES}"
            ));
        }
    }
    Ok(())
}

fn copy_manifest_text(text: &str) -> Result<String, String> {
    let mut copy = String::new();
    copy.try_reserve_exact(text.len())
        .map_err(|_| "samples() registration exceeds host memory".to_owned())?;
    copy.push_str(text);
    Ok(copy)
}

fn copy_preload_specs(names: &[String]) -> Result<Vec<String>, String> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for spec in names.iter().flat_map(|names| names.split_whitespace()) {
        count = count
            .checked_add(1)
            .ok_or_else(|| "preload effect count overflowed".to_owned())?;
        bytes = bytes
            .checked_add(spec.len())
            .ok_or_else(|| "preload effect byte count overflowed".to_owned())?;
        if count > MAX_QUEUED_PRELOADS || bytes > MAX_QUEUED_PRELOAD_BYTES {
            return Err(format!(
                "preloads queued behind sample manifests exceed the session limit of \
                 {MAX_QUEUED_PRELOADS} entries or {MAX_QUEUED_PRELOAD_BYTES} bytes"
            ));
        }
    }

    let mut copied = Vec::new();
    copied
        .try_reserve_exact(count)
        .map_err(|_| "preload effects exceed host memory".to_owned())?;
    for spec in names.iter().flat_map(|names| names.split_whitespace()) {
        let mut owned = String::new();
        owned
            .try_reserve_exact(spec.len())
            .map_err(|_| "preload effects exceed host memory".to_owned())?;
        owned.push_str(spec);
        copied.push(owned);
    }
    Ok(copied)
}

/// What a name resolves to, and the order of the search.
///
/// The order is: a score's own `samples(...)` and the open set's folder,
/// then the sources the player imported in Settings, then a General MIDI
/// font, then the pinned banks. This is the order playback uses. The library
/// and the manifest worker both resolve names through this function, so a
/// warm and a play cannot resolve one name to different banks. To add a
/// layer, add a step here.
fn look_up_named<T>(
    custom: &RwLock<HashMap<String, Bank>>,
    global: &RwLock<HashMap<String, Bank>>,
    gm: &HashMap<String, Vec<Arc<str>>>,
    banks: &RwLock<HashMap<String, Bank>>,
    shared: &Shared,
    name: &str,
    take: impl FnOnce(Named<'_>) -> T,
) -> Option<T> {
    // Exact spelling wins first, in layer order. If it misses, repeat that
    // same layer order case-insensitively. Imported and score banks are
    // published under their catalogue spelling only, whereas pinned maps
    // also keep lowercase aliases for their historical fast path.
    {
        let custom = custom.read().expect("custom banks");
        if let Some(bank) = custom.get(name) {
            return Some(take(Named::Bank(bank)));
        }
    }
    {
        let global = global.read().expect("global banks");
        if let Some(bank) = global.get(name) {
            return Some(take(Named::Bank(bank)));
        }
    }
    if let Some(fonts) = gm.get(name) {
        return Some(take(Named::Font(fonts)));
    }
    {
        let banks = banks.read().expect("default banks");
        if let Some(bank) = banks.get(name) {
            return Some(take(Named::Bank(bank)));
        }
    }

    let folded = name.to_lowercase();
    let folded_entry = |candidate: &str| candidate.to_lowercase() == folded;
    {
        let custom = custom.read().expect("custom banks");
        if let Some(bank) = custom.get(&folded).or_else(|| {
            custom
                .iter()
                .find_map(|(name, bank)| folded_entry(name).then_some(bank))
        }) {
            return Some(take(Named::Bank(bank)));
        }
    }
    {
        let global = global.read().expect("global banks");
        if let Some(bank) = global.get(&folded).or_else(|| {
            global
                .iter()
                .find_map(|(name, bank)| folded_entry(name).then_some(bank))
        }) {
            return Some(take(Named::Bank(bank)));
        }
    }
    if let Some(fonts) = gm.get(&folded).or_else(|| {
        gm.iter()
            .find_map(|(name, fonts)| folded_entry(name).then_some(fonts))
    }) {
        return Some(take(Named::Font(fonts)));
    }
    {
        let banks = banks.read().expect("default banks");
        if let Some(bank) = banks.get(&folded).or_else(|| {
            banks
                .iter()
                .find_map(|(name, bank)| folded_entry(name).then_some(bank))
        }) {
            return Some(take(Named::Bank(bank)));
        }
    }
    local_names::lookup(shared, name, take)
}

/// Which file of a bank an onset plays, and how far it is transposed.
fn pick_from_bank(bank: &Bank, n: f64, midi: f64) -> (Arc<str>, f64) {
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

/// What warming a name turned out to mean: files to fetch, or font files
/// for the soundfont loader. Carried out of the lock rather than acted on
/// inside it.
enum Warm {
    Urls(Vec<Arc<str>>),
    /// A bank's files that the note picks between rather than `n`.
    Pitched(Vec<Arc<str>>),
    Fonts(Vec<Arc<str>>),
}

/// The four name tables [`look_up_named`] searches, each folded to
/// lowercase once, in its order.
struct FoldedNames(Vec<HashMap<String, String>>);

impl FoldedNames {
    fn find(&self, folded: &str) -> Option<&str> {
        self.0
            .iter()
            .find_map(|table| table.get(folded))
            .map(String::as_str)
    }
}

/// [`SampleLibrary::peek_ready_ids_of`], over one folding of the name
/// tables: see [`SampleLibrary::ready_ids`].
pub struct ReadyIds<'a> {
    library: &'a SampleLibrary,
    folded: Option<FoldedNames>,
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

/// Every variant, for the callers that ask about a name as a whole.
static EVERY_VARIANT: crate::sounds::Variants = crate::sounds::Variants::All;

/// The entries of `list` that the variants select: every entry for any,
/// else each `n` wrapped into the list with [`sound_index`], as playback
/// wraps it. Each selected entry appears once.
fn picked<'a, T>(list: &'a [T], variants: &crate::sounds::Variants) -> impl Iterator<Item = &'a T> {
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
fn split_first<T>(list: &[T], first: bool) -> &[T] {
    let at = list.len().min(1);
    if first { &list[..at] } else { &list[at..] }
}

/// `list` in the order to queue at `priority` so that the loader takes it in
/// list order. The loader takes bets newest first, so bets are queued in
/// reverse. The lowest variant, which a bare name plays, is then fetched
/// first.
fn in_queue_order<T>(list: &[T], priority: LoadPriority) -> Vec<&T> {
    let mut ordered: Vec<&T> = list.iter().collect();
    if priority == LoadPriority::Bet {
        ordered.reverse();
    }
    ordered
}

enum Named<'a> {
    Bank(&'a Bank),
    /// The ordered font-file variants of a General MIDI name; `n` picks.
    Font(&'a [Arc<str>]),
}

#[derive(Clone)]
struct ManifestContext {
    banks: Arc<RwLock<HashMap<String, Bank>>>,
    custom: Arc<RwLock<HashMap<String, Bank>>>,
    global: Arc<RwLock<HashMap<String, Bank>>>,
    gm: Arc<HashMap<String, Vec<Arc<str>>>>,
    shared: Arc<Shared>,
}

enum ManifestWork {
    Defaults {
        sources: Vec<PinnedSource>,
        cache_dir: PathBuf,
    },
    /// Walk imported folders and fill their rows.
    ///
    /// A folder used to be walked inline, on whatever thread asked. That
    /// is fine for a dozen kits beside a score and ruinous for a real
    /// library: fifty thousand files on network-backed storage is minutes
    /// of a studio that answers nothing, on every drop, every Enter and
    /// every start. Packs have always landed on this worker; folders do
    /// now too, and their rows read `fetching…` in the meantime.
    Folders { specs: Vec<String>, generation: u64 },
    Custom {
        effects: Vec<(String, Option<String>)>,
        preloads: Vec<String>,
        intent: PrefetchIntent,
        access: ManifestAccess,
        continue_on_error: bool,
        /// Which map the names land in. A score's `samples(…)` and the set
        /// folder share one; the player's own Settings sources have their
        /// own, a step lower in the chain.
        layer: BankLayer,
    },
}

#[derive(Clone)]
enum ManifestAccess {
    Trusted,
    Score(ScoreSampleAccess),
}

struct ManifestJob {
    work: ManifestWork,
    /// How long the worker may spend on it, counted from when it takes it.
    timeout: Duration,
    completion: Option<mpsc::SyncSender<Result<(), String>>>,
}

pub struct SampleLibrary {
    banks: Arc<RwLock<HashMap<String, Bank>>>,
    /// Score-level `samples(...)` registrations, and the open set's own
    /// folder. Checked BEFORE everything else: registration overwrites, so
    /// the last user registration wins over anything prebaked.
    custom: Arc<RwLock<HashMap<String, Bank>>>,
    /// Sources the player added in Settings - folders on disk and packs at
    /// a URL - which every set sees. Under the open set's own folder, so a
    /// set can still keep its own `bd`, and over the pinned banks and the
    /// fonts, because a pack you went and imported is one you meant.
    global: Arc<RwLock<HashMap<String, Bank>>>,
    /// gm_* soundfont name → ordered font-file variants (pinned map; `n`
    /// selects the variant with the same round+euclid index as samples).
    gm: Arc<HashMap<String, Vec<Arc<str>>>>,
    shared: Arc<Shared>,
    manifest_queue: Arc<ManifestQueue>,
    /// Held across a producer's marks and its push, and across a preload's
    /// look at whether a manifest is on its way, so no other producer's job
    /// lands in between. Never held while waiting.
    manifest_order: Mutex<()>,
}

/// Keeps the manifest worker on a job until it drops; see
/// [`SampleLibrary::hold_manifest_worker_for_test`].
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct ManifestWorkerHold {
    release: mpsc::SyncSender<()>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ManifestWorkerHold {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

impl Drop for SampleLibrary {
    fn drop(&mut self) {
        // Publication is cancelled immediately and dropping the owner never
        // joins a network thread. A blocking OS connect/read cannot be
        // interrupted by ureq; it remains bounded by the batch deadline, and
        // the checks surrounding it prevent either the active response or
        // queued work from committing after this point.
        self.shared.publication.cancel();
        self.manifest_queue.close();
    }
}

/// Where downloaded samples are kept: `RUSTEL_SAMPLE_CACHE` when it is set,
/// else `cache/samples` beside the configuration
/// (`~/.rustel/cache/samples`), so everything rustel keeps is in one folder.
fn cache_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os(product::SAMPLE_CACHE_ENV) {
        return PathBuf::from(explicit);
    }
    crate::config_dir::canonical()
        .unwrap_or_else(|| std::env::temp_dir().join(product::CACHE_DIRECTORY_NAME))
        .join(CACHE_DIRECTORY_BESIDE_CONFIG)
        .join(product::SAMPLES_DIRECTORY_NAME)
}

/// What the downloaded samples take on disk, in bytes.
///
/// The function walks the cache on each call. Loader threads and other
/// processes write to the same folder, so a running total would not be
/// accurate. The walk is bounded like the other scans here: a cache past
/// the limit reports what the walk counted.
pub fn sample_cache_usage() -> u64 {
    sample_cache_usage_at(&cache_dir())
}

fn sample_cache_usage_at(base: &Path) -> u64 {
    const MAX_ENTRIES: usize = 200_000;
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut folders = vec![base.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return total;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file()
                && let Ok(metadata) = entry.metadata()
            {
                total = total.saturating_add(metadata.len());
            }
            // A symlink is neither: following one would let a link planted
            // in the cache fold somebody else's disk into this number.
        }
    }
    total
}

/// Empty the sample cache - everything downloaded, both the pinned banks'
/// files and the score-selected namespace beside them.
///
/// Nothing decoded is touched: what is already in RAM goes on playing, and
/// the next thing that needs a file fetches it again.
pub fn clear_sample_cache() -> Result<(), String> {
    clear_sample_cache_at(&cache_dir())
}

/// Whether a name is one the cache writes: the hex digest an entry is
/// filed under, with the extension its url had. Nothing else in the folder
/// is deleted: not the cache's own lock and marker, and not a file that
/// somebody keeps beside the cache.
///
/// The writer uses the first sixteen bytes of the digest
/// ([`cache_path_in_domain`]), which is thirty-two hex characters. A
/// sixty-four character name is a full digest, the other form this cache
/// has used, and also matches.
fn is_cache_entry_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let (stem, extension) = match name.split_once('.') {
        Some((stem, extension)) => (stem, Some(extension)),
        None => (name, None),
    };
    (stem.len() == 32 || stem.len() == 64)
        && stem
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && extension.is_none_or(|ext| {
            !ext.is_empty() && ext.len() <= 5 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn clear_sample_cache_at(base: &Path) -> Result<(), String> {
    // The score namespace has a lock and a marker protocol of its own, so
    // it is cleared through its own door rather than deleted from under it.
    clear_score_sample_cache_at(base)?;
    let Ok(entries) = std::fs::read_dir(base) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if !is_cache_entry_name(&entry.file_name()) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let path = entry.path();
        std::fs::remove_file(&path)
            .map_err(|error| format!("clear {}: {error}", path.display()))?;
    }
    Ok(())
}

/// The folder the caches share beside the configuration.
const CACHE_DIRECTORY_BESIDE_CONFIG: &str = "cache";

/// Where the pinned and host-trusted sample files land: the same folder
/// [`cache_dir`] picks, reported so the CLI can say where a cache run wrote
/// to without each caller re-deriving the location.
pub fn sample_cache_dir() -> PathBuf {
    cache_dir()
}

/// Directory containing only persistently cached score-selected responses.
///
/// [`clear_score_sample_cache`] removes this directory without touching the
/// pinned and host-trusted sample cache beside it. The product's sample-cache
/// environment override, when set, selects the parent directory.
pub fn score_sample_cache_dir() -> PathBuf {
    cache_dir().join(SCORE_CACHE_NAMESPACE)
}

/// Remove the dedicated score cache and disable reads from its legacy shared
/// location.
///
/// Active Sessions may continue using bytes they already decoded. Their
/// per-Session admission counters deliberately do not reset: clearing storage
/// is not a way for one Session to exceed its write allowance.
pub fn clear_score_sample_cache() -> Result<(), String> {
    clear_score_sample_cache_at(&cache_dir())
}

#[derive(Clone, Copy, Debug)]
struct ScoreCacheLimits {
    global_bytes: u64,
    global_entries: usize,
    session_bytes: u64,
    session_entries: usize,
}

impl Default for ScoreCacheLimits {
    fn default() -> Self {
        Self {
            global_bytes: SCORE_SAMPLE_CACHE_MAX_BYTES,
            global_entries: SCORE_SAMPLE_CACHE_MAX_ENTRIES,
            session_bytes: SCORE_SAMPLE_CACHE_SESSION_MAX_BYTES,
            session_entries: SCORE_SAMPLE_CACHE_SESSION_MAX_ENTRIES,
        }
    }
}

#[derive(Debug, Default)]
struct ScoreCacheUsage {
    bytes: u64,
    entries: usize,
}

#[derive(Debug)]
struct ScoreCache {
    base: PathBuf,
    limits: ScoreCacheLimits,
    session: Mutex<ScoreCacheUsage>,
}

/// A response's interpretation is part of its persistent identity.
///
/// One URL can legally be named as either a sample map or an audio file. A
/// validator for one kind must never reject and evict bytes cached for the
/// other kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScoreCacheKind {
    Manifest,
    Audio,
}

impl ScoreCacheKind {
    fn key_domain(self) -> &'static [u8] {
        match self {
            Self::Manifest => b"manifest",
            Self::Audio => b"audio",
        }
    }
}

/// Whether the cache entry was admitted under CORS consent or an exact grant.
///
/// Trust mode is part of the persistent identity so a grant-only fetch (no
/// `Access-Control-Allow-Origin`) cannot later satisfy the public-https/CORS
/// policy for the same URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScoreCacheTrust {
    Cors,
    Grant,
}

impl ScoreCacheTrust {
    fn key_domain(self) -> &'static [u8] {
        match self {
            Self::Cors => b"cors",
            Self::Grant => b"grant",
        }
    }

    fn from_cors_required(cors_required: bool) -> Self {
        if cors_required {
            Self::Cors
        } else {
            Self::Grant
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheAdmission {
    Stored,
    AlreadyPresent,
    Refused,
}

impl ScoreCache {
    fn new(base: PathBuf) -> Self {
        Self {
            base,
            limits: ScoreCacheLimits::default(),
            session: Mutex::new(ScoreCacheUsage::default()),
        }
    }

    #[cfg(test)]
    fn with_limits(base: PathBuf, limits: ScoreCacheLimits) -> Self {
        Self {
            base,
            limits,
            session: Mutex::new(ScoreCacheUsage::default()),
        }
    }

    fn dir(&self) -> PathBuf {
        self.base.join(SCORE_CACHE_NAMESPACE)
    }

    fn path(&self, url: &str, kind: ScoreCacheKind, trust: ScoreCacheTrust) -> PathBuf {
        score_cache_path(&self.dir(), url, kind, trust)
    }

    fn open(&self, path: &Path) -> Result<Option<std::fs::File>, String> {
        let dir = self.dir();
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_dir() => open_regular_cache_entry(path),
            Ok(_) => Err(format!(
                "score sample cache {} is not a directory",
                dir.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "inspect score sample cache {}: {error}",
                dir.display()
            )),
        }
    }

    /// Admit one already-validated body without evicting another score's
    /// cache entry.
    ///
    /// Refusal is intentionally an optimisation failure, not a sample
    /// failure: the caller still has validated bytes for the current run. This
    /// keeps the persistent bound hard without allowing an untrusted score to
    /// evict an offline reference corpus. Both budgets are charged while the
    /// process-wide cache lock is held and BEFORE a staging file is opened.
    #[cfg(test)]
    fn admit(&self, path: &Path, bytes: &[u8]) -> Result<CacheAdmission, String> {
        let publication = PublicationGate::new();
        let budget = sample_fetch::FetchBudget::for_one_fetch();
        self.admit_with_publication(path, bytes, &publication, &budget)
    }

    fn admit_with_publication(
        &self,
        path: &Path,
        bytes: &[u8],
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<CacheAdmission, String> {
        budget.check()?;
        let mut session = self.session.lock().expect("score cache session budget");
        with_score_cache_lock(&self.base, || {
            self.admit_locked(&mut session, path, bytes, publication, budget)
        })
    }

    fn admit_locked(
        &self,
        session: &mut ScoreCacheUsage,
        path: &Path,
        bytes: &[u8],
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<CacheAdmission, String> {
        budget.check()?;
        let incoming = u64::try_from(bytes.len())
            .map_err(|_| "score sample cache entry is too large to account".to_owned())?;
        let dir = self.dir();
        ensure_score_cache_dir(&dir)?;
        cleanup_score_cache_staging(&dir)?;

        // Another Session may have populated the same key while this one
        // fetched. Keeping the complete entry costs this Session nothing, even
        // when its own admission allowance is already spent.
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                return Ok(CacheAdmission::AlreadyPresent);
            }
            Ok(_) => {
                return Err(format!(
                    "score sample cache entry {} is not a regular file",
                    path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect score sample cache entry {}: {error}",
                    path.display()
                ));
            }
        }

        if session.entries >= self.limits.session_entries
            || session
                .bytes
                .checked_add(incoming)
                .is_none_or(|total| total > self.limits.session_bytes)
        {
            return Ok(CacheAdmission::Refused);
        }

        let (entries, occupied) = score_cache_occupancy(&dir)?;
        if entries >= self.limits.global_entries
            || occupied
                .checked_add(incoming)
                .is_none_or(|total| total > self.limits.global_bytes)
        {
            return Ok(CacheAdmission::Refused);
        }

        // Resource limits fire before allocation at the allocation site.
        // Charge before `commit_cache_entry` opens its staging file; roll the
        // charge back if the filesystem refuses the commit.
        session.entries += 1;
        session.bytes += incoming;
        if let Err(error) = commit_cache_entry(path, bytes, Some((publication, budget))) {
            // A failed parent-directory sync happens after rename published
            // the complete file. Keep that visible entry charged even though
            // its crash durability could not be confirmed.
            if !error.published {
                session.entries -= 1;
                session.bytes -= incoming;
            }
            return Err(format!(
                "write score sample cache entry {}: {error}",
                path.display()
            ));
        }
        Ok(CacheAdmission::Stored)
    }

    fn remove(&self, path: &Path) -> Result<(), String> {
        with_score_cache_lock(&self.base, || match std::fs::remove_file(path) {
            Ok(()) => sync_parent_directory(path)
                .map_err(|error| format!("sync score sample cache removal: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove score sample cache entry {}: {error}",
                path.display()
            )),
        })
    }

    fn read_legacy(&self, legacy: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;

        with_score_cache_lock(&self.base, || {
            if !legacy_reads_enabled_locked(&self.base)? {
                return Ok(None);
            }
            let Some(file) = open_regular_cache_entry(legacy)? else {
                return Ok(None);
            };
            let mut bytes = Vec::new();
            if !file
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .is_ok_and(|_| bytes.len() <= limit)
            {
                return Ok(None);
            }
            Ok(Some(bytes))
        })
    }

    #[cfg(test)]
    fn copy_legacy(&self, promoted: &Path, bytes: &[u8]) {
        let publication = PublicationGate::new();
        let budget = sample_fetch::FetchBudget::for_one_fetch();
        self.copy_legacy_with_publication(promoted, bytes, &publication, &budget);
    }

    fn copy_legacy_with_publication(
        &self,
        promoted: &Path,
        bytes: &[u8],
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) {
        let mut session = self.session.lock().expect("score cache session budget");
        let _ = with_score_cache_lock(&self.base, || {
            budget.check()?;
            // An explicit clear may land after the legacy read and validation.
            // Re-check under the SAME lock as admission so an old root entry
            // cannot repopulate the namespace after clear returned.
            if !legacy_reads_enabled_locked(&self.base)? {
                return Ok(());
            }
            // The old namespace is shared with pinned and host-trusted data,
            // so its provenance cannot be recovered from the hash alone. Copy
            // an authorized, validated hit into the bounded namespace, but
            // never retire the only copy a trusted loader may know about.
            let _ = self.admit_locked(&mut session, promoted, bytes, publication, budget)?;
            Ok(())
        });
    }
}

fn legacy_reads_enabled_locked(base: &Path) -> Result<bool, String> {
    let marker = base.join(SCORE_CACHE_NO_LEGACY_MARKER);
    match std::fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(false),
        Ok(_) => Err(format!(
            "score sample cache marker {} is not a regular file",
            marker.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!(
            "inspect score sample cache marker {}: {error}",
            marker.display()
        )),
    }
}

fn score_cache_lock_path(base: &Path) -> PathBuf {
    base.join(".score-cache.lock")
}

fn with_score_cache_lock<T>(
    base: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    ensure_private_dir(base).map_err(|error| format!("create sample cache {error}"))?;
    let lock_path = score_cache_lock_path(base);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err("score sample cache lock is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect score sample cache lock: {error}")),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    // A cache directory may be shared between local accounts. Refuse a
    // pre-placed symlink rather than opening and locking its target.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options
        .open(&lock_path)
        .map_err(|error| format!("open score sample cache lock: {error}"))?;
    if !std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return Err("score sample cache lock is not a regular file".to_owned());
    }
    lock.lock()
        .map_err(|error| format!("lock score sample cache: {error}"))?;
    operation()
}

fn ensure_score_cache_dir(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(format!(
            "score sample cache {} is not a directory",
            dir.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_dir(dir)
                .map_err(|error| format!("create score sample cache {}: {error}", dir.display()))?;
            sync_parent_directory(dir).map_err(|error| {
                format!(
                    "sync score sample cache directory {}: {error}",
                    dir.display()
                )
            })
        }
        Err(error) => Err(format!(
            "inspect score sample cache {}: {error}",
            dir.display()
        )),
    }
}

fn is_score_cache_staging(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.get(..32).is_some_and(|hash| {
        hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && name
                .get(32..)
                .is_some_and(|suffix| suffix.starts_with(".partial"))
    })
}

fn cleanup_score_cache_staging(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir)
        .map_err(|error| format!("read score sample cache {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read score sample cache entry: {error}"))?;
        if is_score_cache_staging(&entry.path()) {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "remove incomplete score sample cache entry: {error}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn score_cache_occupancy(dir: &Path) -> Result<(usize, u64), String> {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(dir)
        .map_err(|error| format!("read score sample cache {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read score sample cache entry: {error}"))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("inspect score sample cache entry: {error}"))?;
        if is_score_cache_staging(&entry.path()) {
            continue;
        }
        if !metadata.file_type().is_file() {
            return Err(format!(
                "score sample cache entry {} is not a regular file",
                entry.path().display()
            ));
        }
        entries = entries
            .checked_add(1)
            .ok_or_else(|| "score sample cache entry count overflow".to_owned())?;
        bytes = bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "score sample cache byte count overflow".to_owned())?;
    }
    Ok((entries, bytes))
}

fn clear_score_sample_cache_at(base: &Path) -> Result<(), String> {
    with_score_cache_lock(base, || {
        // Disable compatibility reads FIRST. If removal below fails, clear
        // reports that failure, but old mixed-namespace entries cannot quietly
        // make score-selected data reappear.
        let marker = base.join(SCORE_CACHE_NO_LEGACY_MARKER);
        cleanup_no_legacy_marker_staging(base)?;
        match std::fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(format!(
                    "score sample cache marker {} is not a regular file",
                    marker.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                commit_cache_entry(&marker, b"score-cache-v1\n", None)
                    .map_err(|error| format!("create score sample cache clear marker: {error}"))?;
            }
            Err(error) => {
                return Err(format!(
                    "inspect score sample cache marker {}: {error}",
                    marker.display()
                ));
            }
        }
        // An earlier attempt may have published the marker but failed while
        // syncing this directory. Do not report a later clear as durable until
        // the marker's directory entry has reached storage too.
        sync_directory(base)
            .map_err(|error| format!("sync score sample cache clear marker: {error}"))?;

        let dir = base.join(SCORE_CACHE_NAMESPACE);
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                std::fs::remove_file(&dir).map_err(|error| {
                    format!("remove score sample cache {}: {error}", dir.display())
                })?
            }
            Ok(_) => std::fs::remove_dir_all(&dir)
                .map_err(|error| format!("remove score sample cache {}: {error}", dir.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect score sample cache {}: {error}",
                    dir.display()
                ));
            }
        }
        // Persist the namespace removal before returning success. Syncing the
        // parent makes the root unlink durable even though the removed tree no
        // longer has a directory handle to flush.
        sync_directory(base)
            .map_err(|error| format!("sync cleared score sample cache: {error}"))?;
        Ok(())
    })
}

fn cleanup_no_legacy_marker_staging(base: &Path) -> Result<(), String> {
    let prefix = format!("{SCORE_CACHE_NO_LEGACY_MARKER}.partial");
    for entry in std::fs::read_dir(base)
        .map_err(|error| format!("read sample cache {}: {error}", base.display()))?
    {
        let entry = entry.map_err(|error| format!("read sample cache entry: {error}"))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "remove incomplete score sample cache marker: {error}"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The identity of a request, for cache purposes: the URL as it goes on the
/// wire, plus the extension a human would expect the file to have.
///
/// The URL is parsed, not trimmed as text. `Url` serialises an empty path as
/// `/`, so `https://host` and `https://host/` share one identity. `/a` and
/// `/a/` stay distinct, and the query stays part of the identity: a slash
/// inside a query does not merge two resources.
///
/// `#` is percent-encoded before parsing and is not a fragment. Bank
/// filenames contain it, and `sample_fetch::fetch` puts `%23` on the wire,
/// so the key matches the request.
fn cache_identity(url: &str) -> (String, Option<String>) {
    let wire = url.replace('#', "%23");
    let Ok(parsed) = sample_fetch::wire_url(url) else {
        // Not a parseable URL; key on the text and take no extension
        // rather than guess one from an unparsed tail.
        return (wire, None);
    };
    // Take the extension from the path only. A dotted host must not supply
    // one: `https://example.com` and `https://example.com/` share an entry.
    let extension = parsed
        .path()
        .rsplit('/')
        .next()
        .and_then(|segment| segment.rsplit_once('.'))
        .map(|(_, ext)| ext)
        .filter(|ext| {
            ext.len() <= 5 && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .map(str::to_owned);
    (parsed.as_str().to_owned(), extension)
}

#[derive(Debug)]
struct CacheCommitError {
    error: std::io::Error,
    /// The final name is already visible, although its parent directory could
    /// not be synced. Quota accounting must not treat it as absent.
    published: bool,
}

impl std::fmt::Display for CacheCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    path.parent().map_or(Ok(()), sync_directory)
}

/// Persist a directory-entry change where the platform exposes directory
/// syncing through ordinary file handles.
#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// Atomic visibility remains portable; Rust does not expose a portable
/// directory-sync operation on other targets.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Put bytes at `path` so a reader sees all of them or none.
///
/// The staging name is unique per writer, not per target. Two Sessions in
/// one process share a cache directory, and each has a loader thread. With
/// a name keyed on the process id alone they would write to the same
/// staging inode, and a rename could publish mixed bytes.
///
/// The staging file is created with `create_new`, so a symlink that an
/// attacker placed in a shared cache directory causes a refusal, not a
/// write through the link.
fn commit_cache_entry(
    path: &Path,
    bytes: &[u8],
    publication: Option<(&PublicationGate, &sample_fetch::FetchBudget)>,
) -> Result<(), CacheCommitError> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static WRITER: AtomicU64 = AtomicU64::new(0);
    let staging = path.with_extension(format!(
        "partial{}.{}",
        std::process::id(),
        WRITER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| CacheCommitError {
            error,
            published: false,
        })?;
    let written = file.write_all(bytes).and_then(|()| {
        // Durable before the rename publishes it, so a crash cannot leave a
        // named entry whose contents never reached the disk.
        file.sync_all()
    });
    if let Err(error) = written {
        drop(file);
        let _ = std::fs::remove_file(&staging);
        return Err(CacheCommitError {
            error,
            published: false,
        });
    }
    drop(file);
    let _publication = match publication {
        Some((publication, budget)) => match publication.enter(PublicationKind::Cache, budget) {
            Ok(guard) => Some(guard),
            Err(error) => {
                let _ = std::fs::remove_file(&staging);
                return Err(CacheCommitError {
                    error: std::io::Error::new(std::io::ErrorKind::Interrupted, error),
                    published: false,
                });
            }
        },
        None => None,
    };
    match std::fs::rename(&staging, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&staging);
            Err(CacheCommitError {
                error,
                published: false,
            })
        }
    }?;
    // File sync makes the bytes durable; syncing the containing directory
    // makes the rename durable. Without both, a successful clear marker could
    // disappear after a power loss and silently re-enable legacy imports.
    sync_parent_directory(path).map_err(|error| CacheCommitError {
        error,
        published: true,
    })
}

fn cache_path(dir: &Path, url: &str) -> PathBuf {
    cache_path_in_domain(dir, url, None)
}

fn score_cache_path(
    dir: &Path,
    url: &str,
    kind: ScoreCacheKind,
    trust: ScoreCacheTrust,
) -> PathBuf {
    cache_path_in_domain(dir, url, Some(&[kind.key_domain(), trust.key_domain()]))
}

fn cache_path_in_domain(dir: &Path, url: &str, domains: Option<&[&[u8]]>) -> PathBuf {
    let (identity, extension) = cache_identity(url);
    let mut hasher = Sha256::new();
    if let Some(domains) = domains {
        // Domain separation keeps a manifest and an audio response at the same
        // URL from sharing validation or eviction state, and keeps CORS-
        // consented bytes apart from grant-only ones. The legacy/trusted
        // layout deliberately retains its original URL-only identity above.
        hasher.update(SCORE_CACHE_KEY_DOMAIN_V1);
        for domain in domains {
            hasher.update(domain);
            hasher.update(b"\0");
        }
    }
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut name = String::with_capacity(40);
    for byte in digest.iter().take(16) {
        name.push_str(&format!("{byte:02x}"));
    }
    // Kept only so a human poking at the cache can tell what a file is.
    if let Some(extension) = extension {
        name.push('.');
        name.push_str(&extension);
    }
    dir.join(name)
}

/// Open a cache entry for reading without following a link: `Ok(None)` when
/// the name is absent, and an error when it holds anything but a regular file.
fn open_regular_cache_entry(path: &Path) -> Result<Option<std::fs::File>, String> {
    // One open, no follow: `symlink_metadata` then `File::open` races a
    // swapped symlink on a shared temp cache directory.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        // A reparse point opens as itself, so the check below refuses it.
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    match options.open(path) {
        Ok(file) => {
            let metadata = file.metadata().map_err(|error| {
                format!("inspect sample cache entry {}: {error}", path.display())
            })?;
            if !metadata.is_file() || cache_entry_is_reparse_point(&metadata) {
                return Err(format!(
                    "sample cache entry {} is not a regular file",
                    path.display()
                ));
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            if cache_open_followed_symlink(&error) {
                return Err(format!(
                    "sample cache entry {} is not a regular file",
                    path.display()
                ));
            }
            Err(format!(
                "open sample cache entry {}: {error}",
                path.display()
            ))
        }
    }
}

#[cfg(unix)]
fn cache_open_followed_symlink(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn cache_open_followed_symlink(_error: &std::io::Error) -> bool {
    false
}

/// Whether a Windows entry opened as itself is a reparse point: a link, or a
/// file whose data a filter serves and a raw read would not see.
#[cfg(windows)]
fn cache_entry_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn cache_entry_is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path)
    }
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(format!("{} is not a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                match std::fs::symlink_metadata(parent) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        ensure_private_dir(parent)?;
                    }
                    Err(error) => {
                        return Err(format!("inspect {}: {error}", parent.display()));
                    }
                }
            }
            match create_private_dir(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure_private_dir(path)
                }
                Err(error) => Err(format!("create {}: {error}", path.display())),
            }
        }
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

/// Read one already-located sample.
///
/// A `file://` URL reaches this function from two registrants only, and
/// each is a folder the host chose: `register_local_folder`, whose root an
/// explicit `--allow-local-samples` named, and `adopt_set_folder`, for the
/// studio's open set. Both write the URL as `file://` followed by the raw
/// filesystem path. Neither goes through `register_custom_value`, because
/// no part of the URL is score-chosen. This function does not decode the
/// path: a percent-encoded path is opened under its literal name.
///
/// A score's own sources never arrive here as `file://`. Every URL a score
/// writes goes through `register_custom_value`. A `samples('local:...')`
/// folder is published into `score_sources` before its banks, so the loader
/// routes it to that grant's reader, which parses and decodes the URL.
/// [`sample_fetch::fetch`] checks the URL again before it connects.
pub(crate) fn fetch_located(url: &str) -> Result<Vec<u8>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        use std::io::Read;
        let ceiling = rustel_audio::sample_pcm_ceiling();
        let file = std::fs::File::open(path).map_err(|error| format!("read {path}: {error}"))?;
        // Ask the filesystem for the length first. One stat refuses a file
        // that is too big, and the error can state its size. Without the
        // stat, the read runs to the ceiling and the buffer doubles behind it.
        let length = file.metadata().ok().map(|meta| meta.len());
        if let Some(length) = length
            && length > ceiling as u64
        {
            return Err(format!(
                "{path} is {}, past the {} one sound can hold",
                rustel_audio::format_sample_bytes(length as usize),
                rustel_audio::format_sample_bytes(ceiling)
            ));
        }
        // The read stays bounded, because the reported length is not
        // reliable: a pipe or a device reports none (`/dev/zero` reports
        // zero and never ends), and a file can grow between the stat and
        // the read. `fs::read` reads the whole file before a caller can
        // compare its length, so the limit must apply during the read.
        let mut bytes = Vec::new();
        if let Some(length) = length.filter(|length| *length <= ceiling as u64) {
            // Known and inside the ceiling: take it in one allocation
            // rather than growing to twice the file on the way.
            bytes.reserve_exact(length as usize + 1);
        }
        file.take(ceiling as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {path}: {error}"))?;
        if bytes.len() > ceiling {
            // The reader stopped at the ceiling, so this one cannot say the
            // size: nothing measured it, which is the point.
            return Err(format!(
                "{path} is over {}, the most one sound can hold",
                rustel_audio::format_sample_bytes(ceiling)
            ));
        }
        return Ok(bytes);
    }
    sample_fetch::fetch(url)
}

fn fetch_manifest_located_with_budget(
    url: &str,
    budget: &sample_fetch::FetchBudget,
) -> Result<Vec<u8>, String> {
    budget.check()?;
    if url.starts_with("file://") {
        let path = url.trim_start_matches("file://");
        let file = std::fs::File::open(path).map_err(|error| format!("read {path}: {error}"))?;
        let mut bytes = Vec::new();
        file.take(sample_fetch::MAX_REMOTE_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {path}: {error}"))?;
        if bytes.len() > sample_fetch::MAX_REMOTE_MANIFEST_BYTES {
            return Err(format!("{path} exceeds the sample manifest size limit"));
        }
        budget.check()?;
        return Ok(bytes);
    }
    sample_fetch::fetch_manifest_with_budget(url, budget)
}

/// Read a sample cache entry through [`open_regular_cache_entry`], so a link
/// at its name is refused; callers treat any `Err` as a miss.
fn read_sample_cache(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_regular_cache_entry(path)?
        .ok_or_else(|| format!("read sample cache {}: not found", path.display()))?;
    let mut reader = file.take(rustel_audio::sample_pcm_ceiling() as u64 + 1);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| format!("read sample cache {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        let next = bytes
            .len()
            .checked_add(read)
            .ok_or_else(|| format!("{} exceeds the sample size limit", path.display()))?;
        if next > rustel_audio::sample_pcm_ceiling() {
            return Err(format!("{} exceeds the sample size limit", path.display()));
        }
        bytes
            .try_reserve_exact(read)
            .map_err(|_| "sample cache exceeds host memory".to_owned())?;
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn fetch_cached_with_budget(
    dir: &Path,
    url: &str,
    budget: &sample_fetch::FetchBudget,
    publication: &PublicationGate,
) -> Result<Vec<u8>, String> {
    budget.check()?;
    // A file on disk is already its own cache, and copying it would mean an
    // edited sample keeps playing the stale copy.
    if url.starts_with("file://") {
        let bytes = fetch_located(url)?;
        budget.check()?;
        return Ok(bytes);
    }
    let path = cache_path(dir, url);
    if let Ok(bytes) = read_sample_cache(&path) {
        budget.check()?;
        return Ok(bytes);
    }
    let bytes = sample_fetch::fetch_audio_with_budget(url, budget)?;
    budget.check()?;
    if StagedCacheFile::create(&path, &bytes)
        .and_then(|staged| staged.publish(&path, publication, budget))
        .is_err()
    {
        // Caching is optional, but cancellation is not: a library dropped
        // while bytes were staged must not proceed as if the fetch completed.
        budget.check()?;
    }
    budget.check()?;
    Ok(bytes)
}

/// Fetch `url` through the host cache and decode it, evicting the entry when
/// `decode` refuses the bytes, whether they were fetched now or read from
/// disk. Mirrors the eviction half of [`fetch_score_source_cached_with_budget`].
fn fetch_cached_decoded_with_budget<T>(
    dir: &Path,
    url: &str,
    budget: &sample_fetch::FetchBudget,
    publication: &PublicationGate,
    decode: impl FnOnce(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    let bytes = fetch_cached_with_budget(dir, url, budget, publication)?;
    decode(&bytes).inspect_err(|_| evict_host_cache_entry(dir, url))
}

/// Remove the host-cache entry for `url`, so the next ask fetches it again.
/// A `file://` URL is its own cache and is never removed.
fn evict_host_cache_entry(dir: &Path, url: &str) {
    if !url.starts_with("file://") {
        let _ = std::fs::remove_file(cache_path(dir, url));
    }
}

static NEXT_CACHE_STAGE: AtomicUsize = AtomicUsize::new(0);

struct StagedCacheFile {
    path: PathBuf,
    published: bool,
}

impl StagedCacheFile {
    fn create(path: &Path, bytes: &[u8]) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("cache path {} has no parent", path.display()))?;
        ensure_private_dir(parent).map_err(|error| format!("create sample cache {error}"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sample");
        let (staged, mut file) = loop {
            let sequence = NEXT_CACHE_STAGE.fetch_add(1, Ordering::Relaxed);
            let staged = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)
            {
                Ok(file) => break (staged, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("stage sample cache {}: {error}", staged.display()));
                }
            }
        };
        let staged = Self {
            path: staged,
            published: false,
        };
        if let Err(error) = file.write_all(bytes) {
            drop(file);
            return Err(format!(
                "write sample cache {}: {error}",
                staged.path.display()
            ));
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            return Err(format!(
                "sync sample cache {}: {error}",
                staged.path.display()
            ));
        }
        drop(file);
        Ok(staged)
    }

    fn publish(
        mut self,
        destination: &Path,
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        let _gate = publication.enter(PublicationKind::Cache, budget)?;
        std::fs::rename(&self.path, destination)
            .map_err(|error| format!("publish sample cache {}: {error}", destination.display()))?;
        self.published = true;
        Ok(())
    }
}

impl Drop for StagedCacheFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Mirrors [`read_sample_cache`] for manifests, under the manifest size limit.
fn read_manifest_cache(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_regular_cache_entry(path)?
        .ok_or_else(|| format!("read sample cache {}: not found", path.display()))?;
    let mut bytes = Vec::new();
    file.take(sample_fetch::MAX_REMOTE_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read sample cache {}: {error}", path.display()))?;
    if bytes.len() > sample_fetch::MAX_REMOTE_MANIFEST_BYTES {
        return Err(format!(
            "{} exceeds the sample manifest size limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn publish_manifest_cache(
    path: &Path,
    bytes: &[u8],
    publication: &PublicationGate,
    budget: &sample_fetch::FetchBudget,
) -> Result<(), String> {
    budget.check()?;
    StagedCacheFile::create(path, bytes)?.publish(path, publication, budget)
}

fn fetch_manifest_cached_with_budget(
    dir: &Path,
    url: &str,
    budget: &sample_fetch::FetchBudget,
    publication: &PublicationGate,
) -> Result<Vec<u8>, String> {
    budget.check()?;
    if url.starts_with("file://") {
        return fetch_manifest_located_with_budget(url, budget);
    }
    let path = cache_path(dir, url);
    if let Ok(bytes) = read_manifest_cache(&path) {
        budget.check()?;
        return Ok(bytes);
    }
    let bytes = fetch_manifest_located_with_budget(url, budget)?;
    budget.check()?;
    if publish_manifest_cache(&path, &bytes, publication, budget).is_err() {
        // Caches are an optimization, so an unwritable cache cannot make a
        // reachable manifest unusable. Cancellation and expiry are different:
        // neither may be hidden by falling through to map publication.
        budget.check()?;
    }
    budget.check()?;
    Ok(bytes)
}

/// A score-selected fetch that may use the on-disk cache.
///
/// The grant is checked before the cache is read. A score with no grant, or
/// with a grant for a different origin, must not read bytes that a granted
/// score left in the cache.
///
/// An approved remote URL may persist under the score cache. CORS-consented
/// bytes and exact-grant bytes keep separate identities, so a grant-only
/// admission cannot later satisfy the public-https/CORS policy for the same
/// URL. A hit serves the bytes whether or not the server is reachable, so
/// an offline render of a score that fetches its samples still works.
///
/// Entries are keyed by the full URL, kind, and trust mode. Reads are
/// size-bounded like a network body.
#[cfg(test)]
fn fetch_score_source_cached<T>(
    cache: &ScoreCache,
    url: &str,
    access: &ScoreFetchAccess,
    kind: ScoreCacheKind,
    limit: usize,
    validate: impl Fn(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    let publication = PublicationGate::new();
    let budget = sample_fetch::FetchBudget::for_one_fetch();
    fetch_score_source_cached_with_budget(
        cache,
        url,
        access,
        kind,
        limit,
        CacheFetchScope {
            budget: &budget,
            publication: &publication,
        },
        validate,
    )
}

#[derive(Clone, Copy)]
struct CacheFetchScope<'a> {
    budget: &'a sample_fetch::FetchBudget,
    publication: &'a PublicationGate,
}

fn fetch_score_source_cached_with_budget<T>(
    cache: &ScoreCache,
    url: &str,
    access: &ScoreFetchAccess,
    kind: ScoreCacheKind,
    limit: usize,
    scope: CacheFetchScope<'_>,
    validate: impl Fn(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    use std::io::Read;

    let CacheFetchScope {
        budget,
        publication,
    } = scope;
    budget.check()?;
    // A local grant reads the file where it lies; a copy would only go stale.
    let ScoreFetchAccess::Remote {
        origin,
        cors_required,
    } = access
    else {
        let value = validate(&fetch_score_source_with_budget(
            url,
            access,
            limit,
            budget,
            || {},
        )?)?;
        budget.check()?;
        return Ok(value);
    };

    let parsed = sample_fetch::wire_url(url)?;
    if parsed.origin().ascii_serialization() != *origin
        || !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(format!(
            "sample URL {url:?} is outside its permitted origin"
        ));
    }

    let trust = ScoreCacheTrust::from_cors_required(*cors_required);
    let path = cache.path(url, kind, trust);
    if let Some(file) = cache.open(&path)? {
        budget.check()?;
        let mut bytes = Vec::new();
        if file
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok_and(|_| bytes.len() <= limit)
        {
            budget.check()?;
            match validate(&bytes) {
                Ok(value) => {
                    budget.check()?;
                    return Ok(value);
                }
                // A stored entry that no longer parses or decodes is worse
                // than no entry: it would answer every future request with the
                // same bad bytes. Drop it and go to the network ONCE.
                Err(_) => {
                    cache.remove(&path)?;
                }
            }
        } else {
            cache.remove(&path)?;
        }
    }

    // Before score-selected bytes had a dedicated namespace they shared the
    // host-trusted cache root. Keep offline renders working across the
    // transition for exact grants only: the old namespace holds pinned and
    // host-trusted data with no CORS provenance, so a cors_required fetch
    // must not read it. A valid legacy hit is copied under both quotas. The
    // old namespace is never removed during migration. If admission is full
    // it still serves this run from its original location.
    if !*cors_required {
        let legacy = cache_path(&cache.base, url);
        if let Some(bytes) = cache.read_legacy(&legacy, limit)?
            && let Ok(value) = validate(&bytes)
        {
            budget.check()?;
            cache.copy_legacy_with_publication(&path, &bytes, publication, budget);
            budget.check()?;
            return Ok(value);
        }
    }

    let bytes = fetch_score_source_with_budget(url, access, limit, budget, || {})?;
    // Validate BEFORE committing, so an error page served with status 200, or
    // a truncated body, cannot become the permanent answer for this URL.
    let value = validate(&bytes)?;
    // Written through a temporary in the same directory and renamed, so a
    // reader never sees a half-written body and a crash leaves no torn entry.
    budget.check()?;
    let _ = cache.admit_with_publication(&path, &bytes, publication, budget);
    budget.check()?;
    Ok(value)
}

#[cfg(test)]
fn fetch_score_source(
    url: &str,
    access: &ScoreFetchAccess,
    limit: usize,
) -> Result<Vec<u8>, String> {
    fetch_score_source_with_budget(
        url,
        access,
        limit,
        &sample_fetch::FetchBudget::for_one_fetch(),
        || {},
    )
}

fn fetch_score_source_with_budget(
    url: &str,
    access: &ScoreFetchAccess,
    limit: usize,
    budget: &sample_fetch::FetchBudget,
    before_local_open: impl FnOnce(),
) -> Result<Vec<u8>, String> {
    use std::io::Read;

    budget.check()?;
    match access {
        ScoreFetchAccess::Local { root } => {
            let parsed = Url::parse(url)
                .map_err(|error| format!("invalid local sample URL {url:?}: {error}"))?;
            if parsed.scheme() != "file" {
                return Err(format!("local sample URL {url:?} is not a file URL"));
            }
            let path = parsed
                .to_file_path()
                .map_err(|_| format!("local sample URL {url:?} is not a valid file path"))?;
            let (file, len) = root.open_score_sample_with(&path, before_local_open)?;
            if len > limit as u64 {
                return Err(format!("{} exceeds the sample size limit", path.display()));
            }
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            if bytes.len() > limit {
                return Err(format!("{} exceeds the sample size limit", path.display()));
            }
            budget.check()?;
            Ok(bytes)
        }
        ScoreFetchAccess::Remote {
            origin,
            cors_required,
        } => {
            let mut current = sample_fetch::wire_url(url)?;
            for redirects in 0..=5 {
                budget.check()?;
                if current.origin().ascii_serialization() != *origin
                    || !matches!(current.scheme(), "http" | "https")
                    || !current.username().is_empty()
                    || current.password().is_some()
                {
                    return Err(format!(
                        "sample URL {current:?} is outside its permitted origin"
                    ));
                }
                // The guarded agent, not a bare one: the origin check above is
                // text, and says nothing about the address the name resolves
                // to. A granted origin can still point at a private target,
                // and can point somewhere different on the next lookup.
                let response = sample_fetch::guarded_single_hop_get(
                    &current,
                    budget,
                    cors_required.then_some(CORS_REQUEST_ORIGIN),
                )?;
                // Consent is per RESPONSE, redirects included, exactly as a
                // browser applies its access check to every hop: a consenting
                // server must not become a pivot through a non-consenting one.
                if *cors_required && !cors_consents(response.header("Access-Control-Allow-Origin"))
                {
                    return Err(format!(
                        "GET {current}: the server does not consent to cross-origin reads \
                         (no `Access-Control-Allow-Origin: *`), so this URL would fail on \
                         strudel.cc as well; pass --allow-sample-origin {origin} to trust \
                         it anyway"
                    ));
                }
                if (300..400).contains(&response.status()) {
                    if redirects == 5 {
                        return Err(format!("GET {url}: too many redirects"));
                    }
                    let location = response
                        .header("Location")
                        .ok_or_else(|| format!("GET {current}: redirect has no Location header"))?;
                    let next = current.join(location).map_err(|error| {
                        format!("GET {current}: invalid redirect target: {error}")
                    })?;
                    if next.origin().ascii_serialization() != *origin
                        || !next.username().is_empty()
                        || next.password().is_some()
                    {
                        return Err(format!(
                            "GET {current}: redirect target is outside the permitted sample origin"
                        ));
                    }
                    current = next;
                    continue;
                }
                let mut bytes = Vec::new();
                response
                    .into_reader()
                    .take(limit as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("read {current}: {error}"))?;
                budget.check()?;
                if bytes.len() > limit {
                    return Err(format!("{current} exceeds the sample size limit"));
                }
                return Ok(bytes);
            }
            unreachable!("redirect loop returns on success or refusal")
        }
    }
}

/// Which decoder a sample URL asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Wav,
    Mp3,
    Ogg,
}

/// Take `count` slots of the sample bank: ids handed back through
/// [`SampleLibrary::release_ids`] first, oldest first, then fresh ones off
/// the counter. The ids need not run consecutively - a font keeps each
/// zone's own - and nothing is taken when the bank cannot seat the whole
/// request.
fn reserve_sample_ids(shared: &Shared, count: usize) -> Result<Vec<SampleId>, String> {
    let capacity = u32::try_from(SAMPLE_BANK_CAPACITY).expect("sample capacity fits u32");
    let mut free = shared.free_ids.lock().expect("free sample ids");
    let reused = free.len().min(count);
    let fresh =
        u32::try_from(count - reused).map_err(|_| "sample bank capacity exceeded".to_owned())?;
    let mut next = shared.next_id.load(Ordering::Relaxed);
    let first_fresh = loop {
        let Some(end) = next.checked_add(fresh) else {
            return Err("sample bank capacity exceeded".to_owned());
        };
        if end > capacity {
            return Err(format!(
                "sample bank capacity exceeded (maximum {} decoded assets)",
                SAMPLE_BANK_CAPACITY - 1
            ));
        }
        match shared
            .next_id
            .compare_exchange_weak(next, end, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break next,
            Err(observed) => next = observed,
        }
    };
    let mut ids: Vec<SampleId> = free.drain(..reused).collect();
    ids.extend((first_fresh..first_fresh + fresh).map(SampleId));
    Ok(ids)
}

/// Hand back an id whose decode never reached anyone: reserved for a url
/// that then failed to fetch or decode. Its table said Loading throughout,
/// so no resolution, event, or bank ever saw it, and its identity slot is
/// still unknown; the retry after the rest reserves afresh.
fn release_unpublished_id(shared: &Shared, id: SampleId) {
    shared
        .free_ids
        .lock()
        .expect("free sample ids")
        .push_back(id);
}

/// A source-string shorthand. [`SHORTHANDS`] defines the supported forms;
/// [`read_source`] resolves them for both manifests and base URLs.
struct Shorthand {
    /// What a score writes, up to and including whatever ends it: `local:`
    /// and friends carry their colon, and `shabda/speech` is spelled with a
    /// slash and continues with a grammar of its own.
    prefix: &'static str,
    kind: ShorthandKind,
}

impl Shorthand {
    /// What follows this shorthand in `source`, or `None` when the row is
    /// not what was written.
    ///
    /// Prefixes without a trailing separator need a boundary check so
    /// `shabda/speechify` is not mistaken for `shabda/speech`.
    fn read<'a>(&self, source: &'a str) -> Option<&'a str> {
        let rest = source.strip_prefix(self.prefix)?;
        if self.prefix.ends_with(':') || rest.is_empty() {
            return Some(rest);
        }
        rest.starts_with([':', '/']).then_some(rest)
    }
}

enum ShorthandKind {
    /// A folder beneath the root the host granted.
    LocalFolder,
    /// `user[/repo[/branch[/dir]]]` under raw.githubusercontent.
    GitHub,
    /// Another way of writing one of the above, with `{}` taking whatever
    /// followed the colon.
    Spelling(&'static str),
    /// `[/<language>/<gender>]:<words>` - the one shorthand with a grammar
    /// rather than a substitution. The address and both defaults still live
    /// on the row, so what changes when the service does is still a row.
    Speech {
        pattern: &'static str,
        language: &'static str,
        gender: &'static str,
    },
}

/// Every shorthand there is. A row, not a branch.
///
/// The longest matching prefix wins, so `shabda/speech` is read as itself
/// rather than as `shabda` with a strange tail, and the order rows are
/// written in carries no meaning.
static SHORTHANDS: &[Shorthand] = &[
    Shorthand {
        prefix: "local:",
        kind: ShorthandKind::LocalFolder,
    },
    Shorthand {
        prefix: "github:",
        kind: ShorthandKind::GitHub,
    },
    Shorthand {
        prefix: "bubo:",
        kind: ShorthandKind::Spelling("github:Bubobubobubobubo/dough-{}"),
    },
    // `samples('shabda:bass,kick')` searches Freesound by word and returns
    // a sample map from the Shabda service.
    Shorthand {
        prefix: "shabda:",
        kind: ShorthandKind::Spelling("https://shabda.ndre.gr/{}.json?strudel=1"),
    },
    // `samples('shabda/speech/en-US/m:music,vocode')` requests speech
    // samples, with optional language and gender settings before the colon.
    Shorthand {
        prefix: "shabda/speech",
        kind: ShorthandKind::Speech {
            pattern: "https://shabda.ndre.gr/speech/{words}.json\
                      ?gender={gender}&language={language}&strudel=1",
            language: "en-GB",
            gender: "f",
        },
    },
];

/// What a source string turned out to name.
enum SampleSource {
    /// A folder to scan, as written after the prefix and not yet trimmed
    /// or checked against the granted root.
    LocalFolder(String),
    /// A manifest or base URL to fetch.
    Url(String),
}

/// Read a source string the way every caller needs it: spellings resolved
/// first, then either a folder or a URL. `subpath` is what a `github:`
/// shorthand asks for - the manifest name when a map is being fetched, and
/// nothing when the answer is a base for other paths to hang off.
fn read_source(source: &str, subpath: &str) -> SampleSource {
    let mut source = source.to_owned();
    // A spelling can expand into another spelling - `bubo:` expands into
    // `github:` - so this walks. Bounded by the table's own length: a row
    // added in a cycle must not hang the score that named it.
    for _ in 0..SHORTHANDS.len() {
        // Longest match, so a row cannot be shadowed by a shorter one that
        // happens to be written above it.
        let Some((found, rest)) = SHORTHANDS
            .iter()
            .filter_map(|shorthand| shorthand.read(&source).map(|rest| (shorthand, rest)))
            .max_by_key(|(shorthand, _)| shorthand.prefix.len())
        else {
            break;
        };
        // A folder's text is its callers' to trim. An address is trimmed
        // here, what a shorthand expands and the plain URL below alike: the
        // newline that ends a multi-line template literal would otherwise
        // land inside every address built from it - a shorthand's path, or
        // each file a base is joined to.
        let trimmed_rest = rest.trim_ascii();
        match found.kind {
            ShorthandKind::LocalFolder => return SampleSource::LocalFolder(rest.to_owned()),
            ShorthandKind::GitHub => return SampleSource::Url(github_path(trimmed_rest, subpath)),
            ShorthandKind::Speech {
                pattern,
                language,
                gender,
            } => return SampleSource::Url(speech_path(trimmed_rest, pattern, language, gender)),
            ShorthandKind::Spelling(pattern) => source = pattern.replace("{}", trimmed_rest),
        }
    }
    SampleSource::Url(source.trim_ascii().to_owned())
}

/// `[/<language>/<gender>]:<words>` - what shabda's speech form says after
/// its prefix.
///
/// Language and gender are optional and positional, language first. A colon
/// separates settings from words; without one, the entire input is words.
fn speech_path(rest: &str, pattern: &str, language: &str, gender: &str) -> String {
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    // Only the first colon separates settings from words, preserving word
    // counts such as `hello:2` on the right.
    let (params, words) = match rest.split_once(':') {
        Some((params, words)) => (params, words),
        None => ("", rest),
    };
    let mut parts = params.split('/').filter(|part| !part.is_empty());
    let language = parts.next().unwrap_or(language);
    // A language without a gender still uses the shorthand's default voice.
    let gender = parts.next().unwrap_or(gender);
    pattern
        .replace("{words}", words)
        .replace("{language}", language)
        .replace("{gender}", gender)
}

/// `githubPath`: `user[/repo[/branch[/dir…]]]` with repo
/// defaulting to "samples" and branch to "main".
const GITHUB_SAMPLE_MANIFEST: &str = "strudel.json";

fn github_path(path: &str, subpath: &str) -> String {
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut components = path.split('/');
    let user = components.next().unwrap_or("");
    let repo = components.next().unwrap_or("samples");
    let branch = components.next().unwrap_or("main");
    let mut other: Vec<&str> = components.collect();
    other.push(subpath);
    format!(
        "https://raw.githubusercontent.com/{user}/{repo}/{branch}/{}",
        other.join("/")
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleAudioKind {
    Wav,
    Mp3,
    Ogg,
}

pub(crate) const MAX_SAMPLE_MANIFEST_ENTRIES: usize = 16_384;
pub(crate) const MAX_SAMPLE_SCAN_ENTRIES: usize = 65_536;
pub(crate) const MAX_SAMPLE_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SAMPLE_SCAN_WORK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SampleScanLimits {
    pub(crate) examined_entries: usize,
    pub(crate) manifest_entries: usize,
    pub(crate) manifest_bytes: usize,
    pub(crate) working_bytes: usize,
}

impl SampleScanLimits {
    pub(crate) const DEFAULT: Self = Self {
        examined_entries: MAX_SAMPLE_SCAN_ENTRIES,
        manifest_entries: MAX_SAMPLE_MANIFEST_ENTRIES,
        manifest_bytes: MAX_SAMPLE_MANIFEST_BYTES,
        working_bytes: MAX_SAMPLE_SCAN_WORK_BYTES,
    };
}

#[derive(Debug)]
pub(crate) enum SampleFolderScanError {
    Io(String),
    Empty(String),
    Limit {
        resource: &'static str,
        limit: usize,
    },
}

impl SampleFolderScanError {
    pub(crate) fn is_limit(&self) -> bool {
        matches!(self, Self::Limit { .. })
    }
}

impl std::fmt::Display for SampleFolderScanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) | Self::Empty(message) => formatter.write_str(message),
            // What a player can do about it, not only what the scanner
            // counted. A library of fifty thousand files is an ordinary
            // thing to own, and the row used to say only that some number
            // had been passed.
            Self::Limit { resource, limit } => {
                write!(
                    formatter,
                    "local samples: {resource} passed its limit of {limit} - import the \
                     subfolders separately, or a folder further in"
                )
            }
        }
    }
}

/// The directory a relative sample path is resolved against.
///
/// The last slash is the cut only when the URL has a path. A bare origin
/// such as `http://localhost:5432`, which the sample server gives, has its
/// last slash inside `http://`, and a cut there gives the base `http:/`.
fn base_url(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    match after_scheme.find('/') {
        // No path at all: the origin itself is the directory.
        None => format!("{url}/"),
        Some(_) => match url.rfind('/') {
            Some(cut) => url[..=cut].to_owned(),
            None => format!("{url}/"),
        },
    }
}

/// Whether a url points at a sample server on this machine.
///
/// `@strudel/sampler` defaults to port 5432, but the port is configurable and
/// the host is what actually identifies it: nothing on localhost is a CDN, and
/// a failed fetch there means the server is not running rather than the
/// network being down.
fn is_local_sampler(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    rest.starts_with("localhost")
        || rest.starts_with("127.0.0.1")
        || rest.starts_with("[::1]")
        || rest.starts_with("0.0.0.0")
}

/// A base may itself be a shorthand (processSampleMap).
fn expand_base(base: &str) -> String {
    match read_source(base, "") {
        SampleSource::Url(url) => url,
        // A base is a prefix the entries hang off, not a folder to scan, so
        // a `local:` base is handed back as it was written.
        SampleSource::LocalFolder(rest) => format!("local:{rest}"),
    }
}

fn join_url(base: &str, entry: &str) -> String {
    if entry.starts_with("http://") || entry.starts_with("https://") {
        return entry.to_owned();
    }
    let entry = repair_vcsl_relative(base, entry);
    // `@strudel/sampler` writes its paths with a leading slash and its callers
    // hand over a base with a trailing one, so a naive concatenation doubles
    // the separator. Harmless over HTTP, wrong for a `file://` path.
    if base.ends_with('/') && entry.starts_with('/') {
        return format!("{base}{}", entry.trim_start_matches('/'));
    }
    format!("{base}{entry}")
}

/// The pinned VCSL map omits the `Membranophones/` parent on a handful of
/// Tom 1 Mallet paths, so those URLs 404 forever and a pack's row can never
/// reach a full cache. Put the parent back when the base is VCSL's.
fn repair_vcsl_relative<'a>(base: &str, entry: &'a str) -> std::borrow::Cow<'a, str> {
    let base_l = base.to_ascii_lowercase();
    if !base_l.contains("/vcsl/") && !base_l.ends_with("/vcsl") {
        return std::borrow::Cow::Borrowed(entry);
    }
    let broken = entry.starts_with("Struck%20Membranophones/")
        || entry.starts_with("Struck Membranophones/");
    if broken {
        std::borrow::Cow::Owned(format!("Membranophones/{entry}"))
    } else {
        std::borrow::Cow::Borrowed(entry)
    }
}

/// What a prefetch started: the files it requested now, a request queued
/// behind manifests still loading, or nothing, for a name no bank knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchStatus {
    Requested(usize),
    Deferred,
    Unknown,
}

impl ManifestContext {
    fn failure(&self, failure: impl Into<SampleFailure>) {
        self.shared
            .failures
            .lock()
            .expect("sample failures")
            .push(failure.into());
    }

    fn register_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        budget.check()?;
        let value: serde_json::Value = serde_json::from_str(map_json)
            .map_err(|error| format!("samples() map does not parse: {error}"))?;
        budget.check()?;
        self.register_custom_value(&value, base, None, 0, budget)
    }

    fn register_score_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
        access: &ScoreSampleAccess,
        budget: &sample_fetch::FetchBudget,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        if access.is_denied() {
            return Err(
                "score-level samples() cannot access files or the network without a host grant"
                    .to_owned(),
            );
        }
        if map_json.len() > MAX_SCORE_SAMPLE_MAP_BYTES {
            return Err(format!(
                "samples() map exceeds the {MAX_SCORE_SAMPLE_MAP_BYTES} byte limit"
            ));
        }
        let value: serde_json::Value = serde_json::from_str(map_json)
            .map_err(|error| format!("samples() map does not parse: {error}"))?;
        budget.check()?;
        self.register_score_custom_value(&value, base, None, access, 0, budget, None, layer)
    }

    /// `import` is the string the score wrote - `github:user/repo` - when
    /// this map came of one, for the banks it brings to be filed under.
    #[allow(clippy::too_many_arguments)]
    fn register_score_custom_value(
        &self,
        value: &serde_json::Value,
        base: Option<&str>,
        fallback_base: Option<&str>,
        access: &ScoreSampleAccess,
        depth: usize,
        budget: &sample_fetch::FetchBudget,
        import: Option<&str>,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 2 {
            return Err("samples() map recursion is too deep".to_owned());
        }
        match value {
            serde_json::Value::String(source) => {
                let written = source.as_str();
                let url = match read_source(source, GITHUB_SAMPLE_MANIFEST) {
                    SampleSource::LocalFolder(rest) => {
                        return self.register_score_local_folder(
                            rest.trim(),
                            access,
                            budget,
                            Some(written),
                            layer,
                        );
                    }
                    SampleSource::Url(url) => url,
                };
                let (url, fetch_access) = access.approve_remote(&url)?;
                // Parsing IS the validation: an error page served with status
                // 200 must not become this URL's permanent manifest.
                let fetched: serde_json::Value = fetch_score_source_cached_with_budget(
                    &self.shared.score_cache,
                    &url,
                    &fetch_access,
                    ScoreCacheKind::Manifest,
                    MAX_SCORE_SAMPLE_MAP_BYTES,
                    CacheFetchScope {
                        budget,
                        publication: &self.shared.publication,
                    },
                    |bytes| {
                        serde_json::from_str(
                            std::str::from_utf8(bytes)
                                .map_err(|error| format!("{url}: not UTF-8: {error}"))?,
                        )
                        .map_err(|error| format!("{url}: not JSON: {error}"))
                    },
                )?;
                budget.check()?;
                // The directory the manifest came from is only a fallback base.
                // A fetched map can name its own `_base`: the shabda maps are
                // relative to one, and it is not their own directory. The base
                // the score wrote outranks both. The explicit base and the
                // fallback are separate parameters, because one `Option` cannot
                // tell an explicit base from a derived one.
                //
                // This does not widen access. `join_url` returns an entry that
                // is a whole URL unchanged, so a map can already point a sound
                // at any host. Each address still goes through `approve_remote`
                // under the score's policy.
                self.register_score_custom_value(
                    &fetched,
                    base,
                    Some(&base_url(&url)),
                    access,
                    depth + 1,
                    budget,
                    Some(import.unwrap_or(written)),
                    layer,
                )
            }
            serde_json::Value::Object(map) => {
                if map.len() > SAMPLE_BANK_CAPACITY - 1 {
                    return Err(format!(
                        "samples() map exceeds the {}-entry limit",
                        SAMPLE_BANK_CAPACITY - 1
                    ));
                }
                // What the score wrote, then what the map says of itself,
                // then where the map was fetched from.
                let map_base = map.get("_base").and_then(serde_json::Value::as_str);
                let effective_base = base
                    .filter(|base| !base.is_empty())
                    .or(map_base.filter(|base| !base.is_empty()))
                    .or(fallback_base)
                    .unwrap_or("");
                let effective_base = expand_base(effective_base);
                let mut staged = Vec::new();
                staged
                    .try_reserve(map.len())
                    .map_err(|_| "not enough host memory for sample banks".to_owned())?;
                let mut sources = Vec::new();
                let mut unsupported = Vec::new();
                unsupported
                    .try_reserve(map.len())
                    .map_err(|_| "not enough host memory for sample diagnostics".to_owned())?;
                for (name, entry) in map {
                    budget.check()?;
                    if name == "_base" {
                        continue;
                    }
                    let entry_base = entry
                        .get("_base")
                        .and_then(serde_json::Value::as_str)
                        .map(expand_base);
                    let base = entry_base.as_deref().unwrap_or(&effective_base);
                    match parse_score_bank(entry, base, access, &mut sources)? {
                        Some(bank) => staged.push((name.clone(), bank)),
                        None => unsupported
                            .push(format!("samples() entry {name:?} has an unsupported shape")),
                    }
                }
                // A source that answered with a well-formed map and no
                // sounds in it. Shabda does exactly this when a word finds
                // nothing: `{"_base": "…"}` on its own, HTTP 200. Registering
                // that as a success and saying nothing leaves you looking for
                // a sound the browser never had.
                if staged.is_empty()
                    && unsupported.is_empty()
                    && let Some(import) = import
                {
                    unsupported.push(format!("samples({import:?}) brought no sounds"));
                }
                self.publish_score_custom(sources, staged, unsupported, budget, import, layer)
            }
            other => Err(format!(
                "samples() expects a map or source string, got {other}"
            )),
        }
    }

    fn publish_score_custom(
        &self,
        sources: Vec<(Arc<str>, ScoreFetchAccess)>,
        staged: Vec<(String, Bank)>,
        unsupported: Vec<String>,
        budget: &sample_fetch::FetchBudget,
        import: Option<&str>,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        let limit = MAX_SCORE_SAMPLE_FILES;
        let _publication = self
            .shared
            .publication
            .enter(PublicationKind::Custom, budget)?;
        let mut source_table = self
            .shared
            .score_sources
            .write()
            .expect("score sample sources");
        // The grant table above is shared: a URL is reachable or it is not,
        // whoever asked. Only the names differ by layer.
        let additional_sources = sources
            .iter()
            .enumerate()
            .filter(|(index, (url, _))| {
                !source_table.contains_key(url.as_ref())
                    && !sources[..*index].iter().any(|(earlier, _)| earlier == url)
            })
            .count();
        if source_table.len().saturating_add(additional_sources) > limit {
            return Err(format!(
                "score sample registrations exceed the {limit}-source session limit"
            ));
        }
        if let BankLayer::Global { generation } = layer {
            // Publish URL restrictions before banks, as below.
            source_table.extend(sources);
            drop(source_table);
            // A pack fills its own row and the imported layer is read again
            // from every row, so a later row still wins a name whichever
            // kind it is and however long its list took to arrive. None of
            // the score-layer bookkeeping below applies: an imported name
            // is never the set folder's and never a score import's.
            if fill_global_slot(&self.shared, import, generation, staged) {
                rebuild_global(&self.shared, &self.global);
            }
            self.shared
                .failures
                .lock()
                .expect("sample failures")
                .extend(unsupported.into_iter().map(SampleFailure::from));
            return Ok(());
        }
        let mut custom = self.custom.write().expect("custom banks");
        let additional_banks = staged
            .iter()
            .enumerate()
            .filter(|(index, (name, _))| {
                !custom.contains_key(name.as_str())
                    && !staged[..*index].iter().any(|(earlier, _)| earlier == name)
            })
            .count();
        if custom.len().saturating_add(additional_banks) > limit {
            return Err(format!(
                "score sample registrations exceed the {limit}-bank session limit"
            ));
        }

        // Publish URL restrictions before banks. A concurrent resolver can
        // therefore never observe a score URL without the grant attached to
        // it. The publication gate makes both updates precede cancellation.
        source_table.extend(sources);
        drop(source_table);
        {
            // Which import each bank came of, for the browser; a bank an
            // inline map redefines is the map's now, not the import's.
            let mut imports = self.shared.bank_imports.write().expect("bank imports");
            for (name, _) in &staged {
                match import {
                    Some(import) => {
                        imports.insert(name.clone(), Arc::from(import));
                    }
                    None => {
                        imports.remove(name);
                    }
                }
            }
        }
        {
            // The same for a name the open set's folder had taken up: the
            // score has redefined it, so it is the score's now - the browser
            // says whose it is, and the next set leaves it alone rather than
            // putting the set's old bank back over it.
            let mut of_set = self.shared.set_banks.write().expect("set banks");
            for (name, _) in &staged {
                of_set.remove(name);
            }
        }
        custom.extend(staged);
        *self
            .shared
            .custom_file_names
            .write()
            .expect("custom filenames") = local_names::index(custom.values());
        drop(custom);
        self.shared
            .failures
            .lock()
            .expect("sample failures")
            .extend(unsupported.into_iter().map(SampleFailure::from));
        Ok(())
    }

    fn register_score_local_folder(
        &self,
        requested: &str,
        access: &ScoreSampleAccess,
        budget: &sample_fetch::FetchBudget,
        import: Option<&str>,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        let allowed_root = access.local_root.as_ref().ok_or_else(|| {
            "samples('local:') requires a host-selected local sample root".to_owned()
        })?;
        let allowed_path = allowed_root.path();
        let requested = Path::new(requested);
        if requested.is_absolute()
            || requested.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err("samples('local:') path must stay beneath the permitted root".to_owned());
        }
        let folder = allowed_path
            .join(requested)
            .canonicalize()
            .map_err(|error| {
                format!(
                    "local samples: cannot read {}: {error}",
                    allowed_path.join(requested).display()
                )
            })?;
        if !folder.starts_with(allowed_path) || !folder.is_dir() {
            return Err(format!(
                "local samples folder {} is outside the permitted root {}",
                folder.display(),
                allowed_path.display()
            ));
        }

        let scanned = scan_score_sample_folder(&folder)?;
        budget.check()?;
        let files = scanned.values().map(Vec::len).sum::<usize>();
        if files >= SAMPLE_BANK_CAPACITY {
            return Err(format!(
                "local sample folder has {files} files; limit is {}",
                SAMPLE_BANK_CAPACITY - 1
            ));
        }
        let mut sources = Vec::new();
        sources
            .try_reserve(files)
            .map_err(|_| "not enough host memory for local sample sources".to_owned())?;
        let mut staged = Vec::new();
        staged
            .try_reserve(scanned.len())
            .map_err(|_| "not enough host memory for local sample banks".to_owned())?;
        for (bank, paths) in &scanned {
            let mut urls = Vec::new();
            urls.try_reserve(paths.len())
                .map_err(|_| "not enough host memory for local sample URLs".to_owned())?;
            for relative in paths {
                budget.check()?;
                let path = folder
                    .join(relative)
                    .canonicalize()
                    .map_err(|error| format!("local samples: cannot read {relative}: {error}"))?;
                if !path.starts_with(allowed_path) || !path.is_file() {
                    return Err(format!(
                        "local sample {} is outside the permitted root {}",
                        path.display(),
                        allowed_path.display()
                    ));
                }
                let url = Url::from_file_path(&path)
                    .map_err(|_| format!("cannot represent {} as a file URL", path.display()))?;
                let url: Arc<str> = Arc::from(url.as_str());
                sources.push((
                    url.clone(),
                    ScoreFetchAccess::Local {
                        root: allowed_root.clone(),
                    },
                ));
                urls.push(url);
            }
            staged.push((bank.clone(), Bank::Array(urls)));
        }

        self.publish_score_custom(sources, staged, Vec::new(), budget, import, layer)?;
        let names = self.custom.read().expect("custom banks").len();
        if self
            .shared
            .direct_diagnostic_logging
            .load(Ordering::Acquire)
        {
            eprintln!(
                "{}",
                serde_json::json!({
                    "local_samples": {
                        "folder": folder.display().to_string(),
                        "banks": names,
                        "files": files,
                    }
                })
            );
        }
        Ok(())
    }

    fn register_custom_value(
        &self,
        value: &serde_json::Value,
        base: Option<&str>,
        fallback_base: Option<&str>,
        depth: usize,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 2 {
            return Err("samples() map recursion is too deep".to_owned());
        }
        match value {
            serde_json::Value::String(source) => {
                let url = match read_source(source, GITHUB_SAMPLE_MANIFEST) {
                    SampleSource::LocalFolder(rest) => {
                        return self.register_local_folder(rest.trim(), base, budget);
                    }
                    SampleSource::Url(url) => url,
                };
                sample_fetch::remote_url_allowed(&url)?;
                let dir = cache_dir();
                let bytes = match fetch_manifest_cached_with_budget(
                    &dir,
                    &url,
                    budget,
                    &self.shared.publication,
                ) {
                    Ok(bytes) => bytes,
                    Err(error) if is_local_sampler(&url) && budget.check().is_ok() => {
                        if self
                            .shared
                            .direct_diagnostic_logging
                            .load(Ordering::Acquire)
                        {
                            eprintln!(
                                "{}",
                                serde_json::json!({
                                    "local_samples": {
                                        "instead_of": url,
                                        "reason": error,
                                        "message": "no sample server answering; reading the folder directly",
                                    }
                                })
                            );
                        }
                        return self.register_local_folder("", base, budget);
                    }
                    Err(error) => return Err(error),
                };
                budget.check()?;
                // A body that is not JSON is evicted, so the next
                // registration fetches it again.
                let fetched: serde_json::Value = std::str::from_utf8(&bytes)
                    .map_err(|error| format!("{url}: not UTF-8: {error}"))
                    .and_then(|text| {
                        serde_json::from_str(text)
                            .map_err(|error| format!("{url}: not JSON: {error}"))
                    })
                    .inspect_err(|_| evict_host_cache_entry(&dir, &url))?;
                budget.check()?;
                // The same three-way order as the score path. The pinned
                // banks do not come through here - `load_default_manifests`
                // parses those itself, against the base each source names in
                // `sample-banks.json` - so this reaches trusted `samples()`
                // registrations only.
                self.register_custom_value(&fetched, base, Some(&base_url(&url)), depth + 1, budget)
            }
            serde_json::Value::Object(map) => {
                let map_base = map.get("_base").and_then(serde_json::Value::as_str);
                let effective_base = base
                    .filter(|base| !base.is_empty())
                    .or(map_base.filter(|base| !base.is_empty()))
                    .or(fallback_base)
                    .unwrap_or("");
                let effective_base = expand_base(effective_base);
                let mut parsed = Vec::new();
                parsed
                    .try_reserve(map.len())
                    .map_err(|_| "samples() map exceeds host memory".to_owned())?;
                for (name, entry) in map {
                    budget.check()?;
                    if name == "_base" {
                        continue;
                    }
                    let entry_base = entry
                        .get("_base")
                        .and_then(serde_json::Value::as_str)
                        .map(expand_base);
                    let base = entry_base.as_deref().unwrap_or(&effective_base);
                    match parse_bank(entry, base) {
                        Some(bank) => match check_bank_urls(&bank) {
                            Ok(()) => parsed.push((name.clone(), bank)),
                            Err(why) => {
                                self.failure(format!("samples() entry {name:?} was refused: {why}"))
                            }
                        },
                        None => self
                            .failure(format!("samples() entry {name:?} has an unsupported shape")),
                    }
                }
                let _publication = self
                    .shared
                    .publication
                    .enter(PublicationKind::Custom, budget)?;
                // Same reasoning as the score path: a well-formed map with
                // nothing in it is not a success worth staying quiet about.
                if parsed.is_empty() && !map.is_empty() {
                    self.failure("samples() source brought no sounds".to_owned());
                }
                let mut registered = self.custom.write().expect("custom banks");
                for (name, bank) in parsed {
                    registered.insert(name, bank);
                }
                *self
                    .shared
                    .custom_file_names
                    .write()
                    .expect("custom filenames") = local_names::index(registered.values());
                Ok(())
            }
            other => Err(format!(
                "samples() expects a map or source string, got {other}"
            )),
        }
    }

    fn register_local_folder(
        &self,
        requested: &str,
        base: Option<&str>,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        budget.check()?;
        let root = if !requested.is_empty() {
            PathBuf::from(requested)
        } else if let Some(dir) = std::env::var_os(product::LOCAL_SAMPLES_ENV) {
            PathBuf::from(dir)
        } else if let Some(dir) = base.filter(|base| !base.is_empty()) {
            PathBuf::from(dir)
        } else {
            std::env::current_dir().map_err(|error| format!("no working directory: {error}"))?
        };
        let root = root
            .canonicalize()
            .map_err(|error| format!("local samples: cannot read {}: {error}", root.display()))?;
        let scanned = scan_sample_folder(&root)?;
        budget.check()?;
        let files = scanned.values().map(Vec::len).sum::<usize>();
        let mut parsed = Vec::new();
        parsed
            .try_reserve_exact(scanned.len())
            .map_err(|_| "local sample map exceeds host memory".to_owned())?;
        for (bank, paths) in &scanned {
            let mut urls = Vec::new();
            urls.try_reserve_exact(paths.len())
                .map_err(|_| "local sample map exceeds host memory".to_owned())?;
            for path in paths {
                budget.check()?;
                urls.push(Arc::from(local_file_url(&root.join(path)).as_str()));
            }
            parsed.push((bank.clone(), Bank::Array(urls)));
        }
        let names = {
            let _publication = self
                .shared
                .publication
                .enter(PublicationKind::Local, budget)?;
            let mut custom = self.custom.write().expect("custom banks");
            for (name, bank) in parsed {
                custom.insert(name, bank);
            }
            *self
                .shared
                .custom_file_names
                .write()
                .expect("custom filenames") = local_names::index(custom.values());
            custom.len()
        };
        if self
            .shared
            .direct_diagnostic_logging
            .load(Ordering::Acquire)
        {
            eprintln!(
                "{}",
                serde_json::json!({
                    "local_samples": {
                        "folder": root.display().to_string(),
                        "banks": names,
                        "files": files,
                    }
                })
            );
        }
        Ok(())
    }

    fn look_up<T>(&self, name: &str, take: impl FnOnce(Named<'_>) -> T) -> Option<T> {
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

    /// Whether some bank holds this name, i.e. whether `{bank}_{name}` exists.
    fn name_belongs_to_a_bank(&self, name: &str) -> bool {
        let suffix = format!("_{name}");
        let custom = self.custom.read().expect("custom banks");
        let global = self.global.read().expect("global banks");
        let banks = self.banks.read().expect("default banks");
        custom
            .keys()
            .chain(global.keys())
            .chain(banks.keys())
            .any(|key| key.ends_with(&suffix))
    }

    fn prefetch_known(&self, spec: &str, priority: LoadPriority) -> PrefetchStatus {
        self.prefetch_known_with_intent(spec, priority, PrefetchIntent::Explicit)
    }

    fn prefetch_known_with_intent(
        &self,
        spec: &str,
        priority: LoadPriority,
        intent: PrefetchIntent,
    ) -> PrefetchStatus {
        let spec = spec.trim();
        if spec.is_empty() {
            return PrefetchStatus::Unknown;
        }
        // A synthesised source has nothing to fetch, so there is nothing to
        // preload -- but calling it an "unknown sound" reads as a missing
        // sample and sends people looking for a bank that was never involved.
        // `s("sbd")` and `s("supersaw")` both reported that while playing
        // perfectly well.
        let name = spec.split_once(':').map_or(spec, |(name, _)| name);
        if rustel_voice::is_native_synth_sound(name) {
            return PrefetchStatus::Requested(0);
        }
        let (name, index) = match spec.split_once(':') {
            Some((name, index)) => (name, index.trim().parse::<usize>().ok()),
            None => (spec, None),
        };
        // GM variants use the same numeric index as an evaluated `n`, including
        // fractional rounding and negative wrap. An ordinary bank preload keeps
        // its own parsing. A live warm reads a bank's index like a GM index
        // and accepts a list, `name:0,3`, so a text with many variants of a
        // name makes one request per name.
        let chosen = spec.split_once(':').and_then(|(_, index)| {
            let number = |index: &str| index.trim().parse::<f64>().ok().filter(|n| n.is_finite());
            let numbers: Option<Vec<f64>> = if intent == PrefetchIntent::LiveWarm {
                index.split(',').map(number).collect()
            } else {
                number(index).map(|n| vec![n])
            };
            numbers.map(|numbers| {
                crate::sounds::Variants::Only(
                    numbers
                        .into_iter()
                        .map(crate::sounds::Variants::index)
                        .collect(),
                )
            })
        });
        // A soundfont variant is not a sample URL. It is a
        // `{font_base}/{font}.js` file that the font loader fetches. The sample
        // fetcher cannot load the bare font name, which has no scheme.
        let found = self.look_up(name, |named| match named {
            Named::Bank(Bank::Array(bank_urls)) => Warm::Urls(bank_urls.clone()),
            Named::Bank(Bank::Notes(notes)) => Warm::Pitched(
                notes
                    .iter()
                    .flat_map(|(_, note_urls)| note_urls.iter().cloned())
                    .collect(),
            ),
            Named::Font(fonts) => Warm::Fonts(fonts.to_vec()),
        });
        let mut urls = match found {
            Some(Warm::Urls(bank_urls) | Warm::Pitched(bank_urls)) => bank_urls,
            Some(Warm::Fonts(mut fonts)) => {
                if fonts.is_empty() {
                    return PrefetchStatus::Unknown;
                }
                if let Some(chosen) = chosen.or_else(|| {
                    (intent == PrefetchIntent::LiveWarm).then(crate::sounds::Variants::first)
                }) {
                    fonts = picked(&fonts, &chosen).cloned().collect();
                }
                for font in in_queue_order(&fonts, priority) {
                    if intent == PrefetchIntent::CacheDisk {
                        queue_font_cache(&self.shared, font);
                    } else {
                        queue_font(&self.shared, font, priority);
                    }
                }
                return PrefetchStatus::Requested(fonts.len());
            }
            None => Vec::new(),
        };
        if urls.is_empty() {
            // Native generators are valid sounds but have no bytes to warm.
            // Treat them as a resolved zero-file preload so the presentation
            // layer does not report a false "unknown sound" warning.
            if rustel_voice::is_native_synth_sound(name) {
                return PrefetchStatus::Requested(0);
            }
            // A name the score reaches through `.bank(…)` is not a sound on
            // its own: `s("basique").bank("wt_digital")` plays
            // `wt_digital_basique`, and the bank can come from a variable the
            // text scan cannot read. The query-driven warm resolves those
            // correctly; what is left here is a bare name that belongs to
            // SOME bank, and calling it unknown printed an error over a score
            // that plays perfectly. A real typo still belongs to no bank and
            // still reports.
            if self.name_belongs_to_a_bank(name) {
                return PrefetchStatus::Requested(0);
            }
            return PrefetchStatus::Unknown;
        }
        // A live warm reads `name:k,j` as the files `n = k` and `n = j`. A bare
        // name selects `n = 0`, also for a note-keyed bank: `s("piano")` is
        // warmed as `piano:0` and fetches one file, not every key. The note
        // about to play still resolves its own file. A player's
        // `preload("name:k")` reads one index and no list. A bare
        // `preload("name")` also selects `n = 0`. A disk-cache request is the
        // exception: a bare name selects every file, and no file is decoded.
        let chosen = match intent {
            PrefetchIntent::LiveWarm => Some(chosen.unwrap_or_else(crate::sounds::Variants::first)),
            PrefetchIntent::Explicit if spec.split_once(':').is_none() => {
                Some(crate::sounds::Variants::first())
            }
            PrefetchIntent::Explicit | PrefetchIntent::CacheDisk => index.map(|index| {
                crate::sounds::Variants::Only(std::collections::BTreeSet::from([index as i64]))
            }),
        };
        if let Some(chosen) = chosen {
            urls = picked(&urls, &chosen).cloned().collect();
        }
        let decode_rate = DecodeRate::for_sound(name);
        for url in in_queue_order(&urls, priority) {
            if intent == PrefetchIntent::CacheDisk {
                ensure_cached_shared(&self.shared, url);
            } else {
                let _ = ensure_loading_shared(&self.shared, url, decode_rate, priority);
            }
        }
        PrefetchStatus::Requested(urls.len())
    }

    fn prefetch_owned(
        &self,
        preloads: Vec<String>,
        intent: PrefetchIntent,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        for spec in preloads {
            // Starting a loader is itself publication. Holding this gate over
            // the table insertion and queue send makes Drop the precise line:
            // either the request started before cancellation, or it never
            // starts at all.
            let _publication = self
                .shared
                .publication
                .enter(PublicationKind::Preload, budget)?;
            if self.prefetch_known_with_intent(&spec, intent.priority(), intent)
                == PrefetchStatus::Unknown
                && intent.reports_unknown()
            {
                self.failure(format!("preload skipped unknown sound {spec:?}"));
            }
        }
        Ok(())
    }

    /// A map is in: a checker may now hold the score to the names it brought.
    fn note_source(&self, map: String) {
        self.shared.note_source_standing(&map, SourceState::Ready);
    }

    /// A map could not be read: a checker may say so where it was asked for.
    /// True when this is news, false when the map already stood failed for
    /// this reason.
    fn note_source_failed(&self, map: &str, error: &str) -> bool {
        let previous = self
            .shared
            .note_source_standing(map, SourceState::Failed(error.to_owned()));
        let news = !previous
            .is_some_and(|standing| standing.state == SourceState::Failed(error.to_owned()));
        // A Settings pack's map is its spec, quoted. Its row says so; what
        // it brought last time stays, which is kinder than silence.
        if let Ok(spec) = serde_json::from_str::<String>(map) {
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            if let Some(slot) = slots
                .iter_mut()
                .find(|slot| slot.kind == GlobalKind::Pack && slot.spec == spec)
            {
                slot.state = GlobalSourceState::Failed(error.to_owned());
            }
        }
        news
    }

    fn finish_manifest(&self) {
        // Every load owned by the job starts before this release publication.
        // `wait_until_idle` reads the counter first, then the loader tables, so
        // it cannot observe zero against an older table snapshot.
        self.shared.manifest_pending.fetch_sub(1, Ordering::AcqRel);
        self.shared.note_settled();
    }

    /// A folder walk never reached this row: say why on it, unless the row
    /// has been replaced since the walk was asked for. The folder family's
    /// counterpart to [`Self::note_source_failed`], for the rows a batch
    /// stops in front of rather than the packs it fails one by one.
    fn note_folder_standing(&self, error: &str, spec: &str, generation: u64) {
        let mut slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        if self.shared.global_generation.load(Ordering::Acquire) != generation {
            return;
        }
        if let Some(slot) = slots
            .iter_mut()
            .find(|slot| slot.kind == GlobalKind::Folder && slot.spec == spec)
            && slot.state == GlobalSourceState::Loading
        {
            slot.state = GlobalSourceState::Failed(error.to_owned());
        }
    }
}

fn load_default_manifests(
    context: &ManifestContext,
    sources: &[PinnedSource],
    dir: &Path,
    budget: &sample_fetch::FetchBudget,
) -> Result<(), String> {
    let mut banks = HashMap::new();
    let mut aliases = Vec::new();
    let hash_of = |bytes: &[u8]| {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };

    for source in sources {
        budget.check()?;
        let path = cache_path(dir, &source.url);
        let cached = read_manifest_cache(&path).ok();
        budget.check()?;
        let verified = match cached {
            Some(bytes) if hash_of(&bytes) == source.sha256 => bytes,
            _ => {
                let bytes = fetch_manifest_located_with_budget(&source.url, budget)?;
                if hash_of(&bytes) != source.sha256 {
                    return Err(format!(
                        "bank \"{}\" ({}) does not match the installed manifest pin",
                        source.name, source.url
                    ));
                }
                budget.check()?;
                if publish_manifest_cache(&path, &bytes, &context.shared.publication, budget)
                    .is_err()
                {
                    budget.check()?;
                }
                bytes
            }
        };
        let json: serde_json::Value = serde_json::from_slice(&verified)
            .map_err(|error| format!("{}: {error}", source.url))?;
        let object = json
            .as_object()
            .ok_or_else(|| format!("{} is not a JSON object", source.url))?;
        match &source.base {
            None => {
                for (canonical, value) in object {
                    match value {
                        serde_json::Value::String(alias) => {
                            aliases.push((canonical.clone(), alias.clone()));
                        }
                        serde_json::Value::Array(list) => {
                            for alias in list {
                                if let Some(alias) = alias.as_str() {
                                    aliases.push((canonical.clone(), alias.to_owned()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(base) => {
                for (name, entry) in object {
                    budget.check()?;
                    if !name.starts_with('_')
                        && let Some(bank) = parse_bank(entry, base)
                    {
                        banks.insert(name.clone(), bank);
                    }
                }
            }
        }
    }
    budget.check()?;
    expand_bank_aliases(&mut banks, &aliases);
    expand_case_insensitive(&mut banks);
    let _publication = context
        .shared
        .publication
        .enter(PublicationKind::Defaults, budget)?;
    context.banks.write().expect("default banks").extend(banks);
    Ok(())
}

/// Seed the default bank map from already verified manifest-cache files.
///
/// The asynchronous worker still refreshes the full pinned set. This small
/// synchronous disk pass means a returning Studio session can draw the whole
/// sample catalogue in its first frame instead of waiting for the manifest
/// worker and the UI's next poll.
fn seed_cached_default_manifests(
    banks: &mut HashMap<String, Bank>,
    sources: &[PinnedSource],
    dir: &Path,
) {
    let mut aliases = Vec::new();
    for source in sources {
        let path = cache_path(dir, &source.url);
        let Ok(bytes) = read_manifest_cache(&path) else {
            continue;
        };
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hash = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if hash != source.sha256 {
            continue;
        }
        let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(&bytes) else {
            continue;
        };
        match &source.base {
            None => {
                for (canonical, value) in object {
                    match value {
                        serde_json::Value::String(alias) => aliases.push((canonical, alias)),
                        serde_json::Value::Array(list) => {
                            for alias in list
                                .into_iter()
                                .filter_map(|value| value.as_str().map(str::to_owned))
                            {
                                aliases.push((canonical.clone(), alias));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(base) => {
                for (name, entry) in object {
                    if !name.starts_with('_')
                        && let Some(bank) = parse_bank(&entry, base)
                    {
                        banks.insert(name, bank);
                    }
                }
            }
        }
    }
    expand_bank_aliases(banks, &aliases);
    expand_case_insensitive(banks);
}

/// Walk a folder batch, one folder at a time.
///
/// The deadline is checked between folders. A walk in progress is not
/// aborted. When the budget is spent, each row still on `Loading` gets a
/// message that names the walk, so no row stays on `Loading` for the rest
/// of the session. The function then returns the budget error, the only
/// failure the caller receives.
fn run_folders_work(
    context: &ManifestContext,
    specs: &[String],
    generation: u64,
    budget: &sample_fetch::FetchBudget,
) -> Result<(), String> {
    let mut changed = false;
    let mut spent = None;
    'folders: for spec in specs {
        loop {
            // Each folder lands on its own, so a small kit beside a huge
            // library is browsable while the library is still being walked.
            if let Err(error) = budget.check() {
                spent = Some(error);
                break 'folders;
            }
            let Some(folder) = folder_of_spec(spec) else {
                continue 'folders;
            };
            let (banks, state) = walk_global_folder(&folder);
            match fill_global_folder_slot(&context.shared, spec, generation, banks, state) {
                FolderFill::Filled => {
                    changed = true;
                    rebuild_global(&context.shared, &context.global);
                    break;
                }
                FolderFill::Superseded => break,
                FolderFill::Stale => {}
            }
        }
    }
    // A batch whose budget was spent when it arrived walked nothing at all;
    // one spent between folders stopped in front of the rest. Either way,
    // every row still standing is told why.
    if let Some(error) = spent {
        for spec in specs {
            context.note_folder_standing(&error, spec, generation);
        }
        return Err(error);
    }
    let _ = changed;
    Ok(())
}

fn run_manifest_worker(context: ManifestContext, jobs: ManifestJobs) {
    while let Some(job) = jobs.recv() {
        let budget = sample_fetch::FetchBudget::until(
            Instant::now() + job.timeout,
            context.shared.publication.cancellation(),
        );
        // A job that stops on a map's failure it has said before says
        // nothing about the stop either: one line per reason, not per retry.
        let mut said_before = false;
        // The maps a stop leaves failed, said with it.
        let mut stopped = Vec::new();
        let result = match job.work {
            ManifestWork::Defaults { sources, cache_dir } => budget
                .check()
                .and_then(|()| load_default_manifests(&context, &sources, &cache_dir, &budget)),
            // The folder arm checks its own budget, so a batch that arrives
            // spent still reaches it: it names the walk on every row it never
            // got to, where a pre-check here in the lobby would leave those
            // rows on `Loading` for the rest of the session.
            ManifestWork::Folders { specs, generation } => {
                run_folders_work(&context, &specs, generation, &budget)
            }
            ManifestWork::Custom {
                effects,
                preloads,
                intent,
                access,
                continue_on_error,
                layer,
            } => {
                let mut unreached = effects.into_iter();
                let result = budget.check().and_then(|()| {
                    for (map, base) in unreached.by_ref() {
                        let registered = budget.check().and_then(|()| match &access {
                            ManifestAccess::Trusted => {
                                context.register_custom(&map, base.as_deref(), &budget)
                            }
                            ManifestAccess::Score(access) => context.register_score_custom(
                                &map,
                                base.as_deref(),
                                access,
                                &budget,
                                layer,
                            ),
                        });
                        match registered {
                            Ok(()) => context.note_source(map),
                            Err(error) => {
                                let news = context.note_source_failed(&map, &error);
                                // Deadline and cancellation are structural
                                // outcomes; a later effect or preload may
                                // never launder them.
                                if let Err(spent) = budget.check() {
                                    said_before = !news;
                                    stopped.push(map);
                                    return Err(spent);
                                }
                                if !continue_on_error {
                                    stopped.push(map);
                                    return Err(error);
                                }
                                if news {
                                    context.failure(SampleFailure {
                                        message: error,
                                        maps: vec![map],
                                    });
                                }
                            }
                        }
                    }
                    context.prefetch_owned(preloads, intent, &budget)
                });
                // A spent budget stops the batch in front of its later
                // maps: each reads failed, as the folder arm's rows do,
                // never loading with no job behind it.
                if let Err(spent) = budget.check() {
                    for (map, _) in unreached {
                        context.note_source_failed(&map, &spent);
                        stopped.push(map);
                    }
                }
                result
            }
        };
        let completion = job.completion;
        if completion.is_none()
            && !said_before
            && let Err(error) = &result
        {
            context.failure(SampleFailure {
                message: error.clone(),
                maps: stopped,
            });
        }
        context.finish_manifest();
        if let Some(completion) = completion {
            let _ = completion.send(result);
        }
    }
}

/// Whether a library starts the thread that fetches and decodes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Loading {
    /// The product, and every test whose subject is registration reaching a
    /// real loader.
    InBackground,
    /// Only for a test that reads the job queue itself.
    #[cfg(any(test, feature = "test-support"))]
    NotStarted,
}

impl SampleLibrary {
    /// Load and verify every pinned default manifest before returning.
    ///
    /// A successful return means the default bank map is ready. Network,
    /// deadline, pin, and parse failures are returned to the caller. Session's
    /// live producer uses a separate crate-private asynchronous constructor so
    /// it never waits for manifest I/O.
    pub fn load_default() -> Result<Self, String> {
        Self::load_default_from(PINNED_BANKS, PINNED_GM_FONTS, false, cache_dir())
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn load_default_with_manifest_cache_for_tests(
        manifest_cache_dir: PathBuf,
    ) -> Result<Self, String> {
        Self::load_default_from(PINNED_BANKS, PINNED_GM_FONTS, false, manifest_cache_dir)
    }

    /// Build the inline default library immediately and load its pinned
    /// manifests on the bounded worker used by Session.
    pub(crate) fn load_default_async_for_session() -> Result<Self, String> {
        Self::load_default_from(PINNED_BANKS, PINNED_GM_FONTS, true, cache_dir())
    }

    fn load_default_from(
        pinned_banks: &str,
        pinned_fonts: &str,
        asynchronous: bool,
        manifest_cache_dir: PathBuf,
    ) -> Result<Self, String> {
        let pinned: PinnedFile = serde_json::from_str(pinned_banks)
            .map_err(|error| format!("pinned sample-banks.json: {error}"))?;
        let mut banks = inline_banks_from(&pinned);
        if asynchronous {
            seed_cached_default_manifests(&mut banks, &pinned.sources, &manifest_cache_dir);
        }

        let pinned_fonts: PinnedFonts = serde_json::from_str(pinned_fonts)
            .map_err(|error| format!("pinned gm-fonts.json: {error}"))?;
        let gm: HashMap<String, Vec<Arc<str>>> = pinned_fonts
            .fonts
            .into_iter()
            .map(|(name, fonts)| {
                (
                    name,
                    fonts
                        .into_iter()
                        .map(|font| Arc::from(font.as_str()))
                        .collect(),
                )
            })
            .collect();
        let font_base = pinned_fonts.base;

        let library = Self::with_background_loaders(banks, Vec::new(), gm, font_base)?;
        let work = ManifestWork::Defaults {
            sources: pinned.sources,
            cache_dir: manifest_cache_dir,
        };
        if asynchronous {
            library.enqueue_manifest_work_async(work)?;
        } else {
            library.enqueue_manifest_work_blocking(work)?;
        }
        Ok(library)
    }

    /// Assemble a library around the background loader threads. Split out of
    /// [`load_default`] so tests can build an EMPTY library (no pinned data,
    /// no network) and still exercise registration through the real loaders.
    fn with_background_loaders(
        banks: HashMap<String, Bank>,
        aliases: Vec<(String, String)>,
        gm: HashMap<String, Vec<Arc<str>>>,
        font_base: String,
    ) -> Result<Self, String> {
        Self::with_background_loaders_at(
            banks,
            aliases,
            gm,
            font_base,
            cache_dir(),
            Loading::InBackground,
        )
    }

    fn with_background_loaders_at(
        mut banks: HashMap<String, Bank>,
        aliases: Vec<(String, String)>,
        gm: HashMap<String, Vec<Arc<str>>>,
        font_base: String,
        dir: PathBuf,
        loading: Loading,
    ) -> Result<Self, String> {
        let jobs = LoadQueue::new();
        let font_jobs = FontQueue::new();
        let (manifest_queue, manifest_jobs) = manifest_queue::manifest_queue();
        let publication = Arc::new(PublicationGate::new());
        let shared = Arc::new(Shared {
            by_url: RwLock::new(HashMap::new()),
            score_sources: RwLock::new(HashMap::new()),
            ready: Mutex::new(ReadySamples::default()),
            shapes: Mutex::new(SampleShapes::default()),
            source_tables: Mutex::new(SourceTables::default()),
            bank_imports: RwLock::new(HashMap::new()),
            set_banks: RwLock::new(BTreeMap::new()),
            custom_file_names: RwLock::new(HashMap::new()),
            global_file_names: RwLock::new(HashMap::new()),
            // Id 0 is the bundled bd; the library allocates from 1.
            next_id: AtomicU32::new(1),
            free_ids: Mutex::new(VecDeque::new()),
            jobs: Arc::clone(&jobs),
            loading: Mutex::new(HashSet::new()),
            caching: Mutex::new(HashSet::new()),
            host_cache: dir.clone(),
            font_base: font_base.clone(),
            fonts: RwLock::new(HashMap::new()),
            renames: RwLock::new(HashMap::new()),
            source_renames: RwLock::new(HashMap::new()),
            auto_aliases: RwLock::new(HashMap::new()),
            global_slots: Mutex::new(Vec::new()),
            global_generation: std::sync::atomic::AtomicU64::new(0),
            global_source_of: RwLock::new(HashMap::new()),
            import_policy: Mutex::new(None),
            font_jobs: Arc::clone(&font_jobs),
            failures: Mutex::new(Vec::new()),
            direct_diagnostic_logging: AtomicBool::new(
                rustel_voice::default_direct_diagnostic_logging(),
            ),
            score_cache: ScoreCache::new(dir.clone()),
            manifest_pending: AtomicUsize::new(0),
            publication,
            render_rate: AtomicU32::new(crate::session::DEFAULT_SAMPLE_RATE),
            settled_epoch: AtomicU64::new(0),
        });
        // A worker must not keep `Shared` alive: worker -> Shared -> queue
        // would otherwise form a cycle and leak one thread per discarded
        // library. It holds the queue alone, upgrades only while handling a
        // job, and goes when dropping `Shared` closes the line.
        // A test that reads the job queue is racing these threads for the
        // same job: a loader pops one to work on it, so whether the
        // assertion sees it depends on which thread got there first.
        if loading == Loading::InBackground {
            for index in 0..SAMPLE_LOAD_WORKERS {
                let worker_shared = Arc::downgrade(&shared);
                let jobs = Arc::clone(&jobs);
                let dir = dir.clone();
                std::thread::Builder::new()
                    .name(format!("sample-loader-{index}"))
                    .spawn(move || run_sample_loader(worker_shared, jobs, dir))
                    .map_err(|error| format!("spawn sample loader: {error}"))?;
            }
        }

        let font_shared = Arc::downgrade(&shared);
        let font_dir = cache_dir();
        std::thread::Builder::new()
            .name("soundfont-loader".into())
            .spawn(move || run_font_loader(font_shared, font_jobs, font_dir, font_base))
            .map_err(|error| format!("spawn soundfont loader: {error}"))?;

        expand_bank_aliases(&mut banks, &aliases);
        expand_case_insensitive(&mut banks);

        let banks = Arc::new(RwLock::new(banks));
        let custom = Arc::new(RwLock::new(HashMap::new()));
        let global = Arc::new(RwLock::new(HashMap::new()));
        let gm = Arc::new(gm);
        let manifest_context = ManifestContext {
            banks: Arc::clone(&banks),
            custom: Arc::clone(&custom),
            global: Arc::clone(&global),
            gm: Arc::clone(&gm),
            shared: Arc::clone(&shared),
        };
        std::thread::Builder::new()
            .name("sample-manifest-loader".into())
            .spawn(move || run_manifest_worker(manifest_context, manifest_jobs))
            .map_err(|error| format!("spawn sample manifest loader: {error}"))?;

        Ok(Self {
            banks,
            custom,
            global,
            gm,
            shared,
            manifest_queue,
            manifest_order: Mutex::new(()),
        })
    }

    /// Register score data with exactly one origin granted.
    ///
    /// The grant has to be real: with no origin the entry would be refused for
    /// want of permission, and a test asserting the ADDRESS guard would pass
    /// without ever reaching it.
    #[cfg(test)]
    fn register_custom_for_test(&self, origin: &str, map_json: &str) -> Result<(), String> {
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin(origin)
            .expect("test origin is well formed");
        self.register_score_custom(map_json, None, &access)
    }

    /// Record `url` as decoded under `id`, the way a finished load does.
    /// For tests outside this module that need the state a host sees after
    /// a sample has been published and taken.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn remember_ready_for_test(&self, url: &str, id: SampleId) {
        let url: Arc<str> = Arc::from(url);
        self.banks
            .write()
            .expect("default banks")
            .insert(url.to_string(), Bank::Array(vec![url.clone()]));
        self.shared
            .by_url
            .write()
            .expect("sample url table")
            .insert(
                url,
                UrlState::Ready {
                    id,
                    duration_secs: 1.0,
                },
            );
    }

    /// Publish `decoded` for `url` under `id` the way a finished load does:
    /// Ready in the tables and waiting in the ready queue for a device to
    /// install it.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn publish_ready_for_test(&self, url: &str, id: SampleId, decoded: DecodedSample) {
        self.remember_ready_for_test(url, id);
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .push((id, decoded));
    }

    /// Leave the tombstone `forget_decoded` leaves, on an id the library
    /// never tabled - a retained sample installed straight into a device.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn forget_id_for_test(&self, id: SampleId) {
        self.shared.ready.lock().expect("ready samples").identities[id.0 as usize] =
            DecodedIdentity::Forgotten;
    }

    /// Whether anything has asked for `url` yet: loading, in, or failed.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn asked_for_test(&self, url: &str) -> bool {
        self.shared
            .by_url
            .read()
            .expect("sample url table")
            .contains_key(&Arc::from(url))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn knows_ready_for_test(&self, url: &str) -> bool {
        matches!(
            self.shared
                .by_url
                .read()
                .expect("sample url table")
                .get(&Arc::from(url)),
            Some(UrlState::Ready { .. })
        )
    }

    /// An empty library with live loaders, for tests that exercise
    /// registration without touching the pinned banks or the network.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn empty() -> Self {
        Self::with_background_loaders(HashMap::new(), Vec::new(), HashMap::new(), String::new())
            .expect("empty library")
    }

    /// An empty library whose loader thread was never started.
    ///
    /// For a test that asserts the content of the job queue. `empty()` runs
    /// the real loader, which pops jobs, so a test that reads the queue would
    /// race the loader for the same job.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn empty_without_loading() -> Self {
        Self::with_background_loaders_at(
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            String::new(),
            cache_dir(),
            Loading::NotStarted,
        )
        .expect("empty library")
    }

    /// A real lookup that stays Loading without submitting any loader work.
    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn with_loading_sample_for_test(name: &str) -> Self {
        let library = Self::empty();
        let url: Arc<str> = Arc::from("held-sample-for-test.wav");
        library
            .shared
            .by_url
            .write()
            .expect("sample url table")
            .insert(Arc::clone(&url), UrlState::Loading);
        library
            .custom
            .write()
            .expect("custom banks")
            .insert(name.to_owned(), Bank::Array(vec![url]));
        assert!(
            library.shared.jobs.try_pop().is_none(),
            "no sample job was submitted"
        );
        assert_eq!(library.manifests_pending(), 0);
        library
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn fail_loading_sample_for_test(&self) {
        let url: Arc<str> = Arc::from("held-sample-for-test.wav");
        let mut by_url = self.shared.by_url.write().expect("sample url table");
        assert!(matches!(by_url.get(&url), Some(UrlState::Loading)));
        by_url.insert(url, UrlState::Failed { at: Instant::now() });
        drop(by_url);
        self.shared.note_settled();
        assert!(self.shared.jobs.try_pop().is_none());
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn finish_loading_sample_for_test(&self) -> SampleId {
        self.finish_loading_sample_frames_for_test(480)
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn finish_loading_sample_frames_for_test(&self, frames: usize) -> SampleId {
        self.finish_loading_url_for_test(Arc::from("held-sample-for-test.wav"), frames)
    }

    /// A bank keyed by note, each key's file Loading without any loader
    /// work until [`Self::finish_note_key_for_test`].
    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn with_loading_note_bank_for_test(name: &str, keys: &[f64]) -> Self {
        let library = Self::empty();
        let notes = keys
            .iter()
            .map(|key| {
                let url: Arc<str> = Arc::from(format!("note-key-{key}.wav"));
                library
                    .shared
                    .by_url
                    .write()
                    .expect("sample url table")
                    .insert(Arc::clone(&url), UrlState::Loading);
                (*key, vec![url])
            })
            .collect();
        library
            .custom
            .write()
            .expect("custom banks")
            .insert(name.to_owned(), Bank::Notes(notes));
        library
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn finish_note_key_for_test(&self, key: f64) -> SampleId {
        self.finish_loading_url_for_test(Arc::from(format!("note-key-{key}.wav")), 480)
    }

    /// Banks of one file each that nobody has asked for yet, on a library
    /// whose loader never starts, so what an ask queues stays readable
    /// through [`Self::queued_loads_for_test`].
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_unasked_banks_for_test(names: &[&str]) -> Self {
        let library = Self::empty_without_loading();
        for name in names {
            library.custom.write().expect("custom banks").insert(
                (*name).to_owned(),
                Bank::Array(vec![Arc::from(format!("unasked-{name}.wav"))]),
            );
        }
        library
    }

    /// The loads queued to play now, and those queued as bets.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn queued_loads_for_test(&self) -> (usize, usize) {
        let state = self.shared.jobs.state.lock().expect("sample load queue");
        (state.now.len(), state.bets.len())
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    fn finish_loading_url_for_test(&self, url: Arc<str>, frames: usize) -> SampleId {
        assert!(matches!(
            self.shared
                .by_url
                .read()
                .expect("sample url table")
                .get(&url),
            Some(UrlState::Loading)
        ));
        let id = reserve_sample_ids(&self.shared, 1).expect("sample slot")[0];
        let decoded =
            DecodedSample::from_parts(48_000, 1, vec![0.25; frames]).expect("decoded sample");
        let duration_secs = decoded.frames() as f64 / f64::from(decoded.sample_rate());
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .push((id, decoded));
        self.shared
            .by_url
            .write()
            .expect("sample url table")
            .insert(url, UrlState::Ready { id, duration_secs });
        self.shared.note_settled();
        assert!(self.shared.jobs.try_pop().is_none());
        id
    }

    /// Keep the manifest worker on a job of its own until the hold drops:
    /// a busy loader, for tests that must not reach the network.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn hold_manifest_worker_for_test(&self) -> ManifestWorkerHold {
        let (reached, release) = self
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        self.enqueue_manifest_work_async(ManifestWork::Custom {
            effects: vec![(r#"{"held":"http://127.0.0.1:9/held.wav"}"#.to_owned(), None)],
            preloads: Vec::new(),
            intent: PrefetchIntent::Explicit,
            access: ManifestAccess::Trusted,
            continue_on_error: true,
            layer: BankLayer::Score,
        })
        .expect("the held job");
        reached
            .recv_timeout(Duration::from_secs(30))
            .expect("the worker took the held job");
        ManifestWorkerHold { release }
    }

    /// Queue `specs` as one evaluation's `samples("…")` batch, each map
    /// marked loading as such a batch's are, on `budget` in place of the
    /// minute a batch gets.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn register_samples_batch_for_test(&self, specs: &[&str], budget: Duration) {
        let effects = specs
            .iter()
            .map(|spec| (serde_json::to_string(spec).expect("a string"), None))
            .collect::<Vec<_>>();
        for (map, _) in &effects {
            self.mark_source_loading(map);
        }
        self.enqueue_manifest_work_for(
            ManifestWork::Custom {
                effects,
                preloads: Vec::new(),
                intent: PrefetchIntent::Explicit,
                access: ManifestAccess::Trusted,
                continue_on_error: true,
                layer: BankLayer::Score,
            },
            budget,
        )
        .expect("the batch");
    }

    /// Register a host-trusted map of banks whose files load lazily per
    /// trigger. This route may read local paths and arbitrary URLs; evaluated
    /// score data must use [`Self::register_score_custom`] instead.
    ///
    /// `map_json` is a JSON value. A string form fetches the map:
    /// `github:user/repo[/branch[/dir]]` resolves to raw.githubusercontent's
    /// `strudel.json`, `bubo:x` to `github:Bubobubobubobubo/dough-x`, and a
    /// plain URL is fetched with its own directory as the default base.
    /// The call blocks. On success the complete map is published and ready
    /// for lookup. Fetch, deadline, and parse errors are returned directly.
    ///
    /// A host-trusted `local:` folder is walked here, not on the manifest
    /// worker. Opening a Session queues the pinned default maps first, and
    /// the worker is busy with those fetches until they finish or their
    /// budget expires. A disk walk fetches nothing, so it does not wait
    /// behind them. The hermetic studio's `.samples` folder is such a case.
    pub fn register_trusted_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
    ) -> Result<(), String> {
        validate_manifest_effects(std::iter::once((map_json, base)))?;
        if let Some(result) = self.register_trusted_local_folder_now(map_json, base) {
            return result;
        }
        let mut effects = Vec::new();
        effects
            .try_reserve_exact(1)
            .map_err(|_| "samples() registration exceeds host memory".to_owned())?;
        effects.push((
            copy_manifest_text(map_json)?,
            base.map(copy_manifest_text).transpose()?,
        ));
        self.enqueue_manifest_work_blocking(ManifestWork::Custom {
            effects,
            preloads: Vec::new(),
            intent: PrefetchIntent::Explicit,
            access: ManifestAccess::Trusted,
            continue_on_error: false,
            layer: BankLayer::Score,
        })
    }

    /// `Some` when `map_json` is a `local:` folder, already published or
    /// failed. `None` means this map still belongs on the worker.
    fn register_trusted_local_folder_now(
        &self,
        map_json: &str,
        base: Option<&str>,
    ) -> Option<Result<(), String>> {
        let value: serde_json::Value = serde_json::from_str(map_json).ok()?;
        let serde_json::Value::String(source) = value else {
            return None;
        };
        let SampleSource::LocalFolder(rest) = read_source(&source, GITHUB_SAMPLE_MANIFEST) else {
            return None;
        };
        let budget = self.manifest_budget(MANIFEST_BATCH_TIMEOUT);
        let context = self.manifest_context();
        Some(
            match context.register_local_folder(rest.trim(), base, &budget) {
                Ok(()) => {
                    context.note_source(map_json.to_owned());
                    Ok(())
                }
                Err(error) => {
                    context.note_source_failed(map_json, &error);
                    Err(error)
                }
            },
        )
    }

    /// Register a map requested by evaluated score text under an explicit
    /// host policy. Every source URL in the complete map is validated before
    /// any bank becomes visible, including URLs inside fetched maps.
    ///
    /// This public API remains blocking: success means the complete map is
    /// ready for lookup. Session uses the asynchronous batch API below.
    pub fn register_score_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
        access: &ScoreSampleAccess,
    ) -> Result<(), String> {
        if access.is_denied() {
            return Err(
                "score-level samples() cannot access files or the network without a host grant"
                    .to_owned(),
            );
        }
        if map_json.len() > MAX_SCORE_SAMPLE_MAP_BYTES {
            return Err(format!(
                "samples() map exceeds the {MAX_SCORE_SAMPLE_MAP_BYTES} byte limit"
            ));
        }
        validate_manifest_effects(std::iter::once((map_json, base)))?;
        let mut effects = Vec::new();
        effects
            .try_reserve_exact(1)
            .map_err(|_| "samples() registration exceeds host memory".to_owned())?;
        effects.push((
            copy_manifest_text(map_json)?,
            base.map(copy_manifest_text).transpose()?,
        ));
        self.enqueue_manifest_work_blocking(ManifestWork::Custom {
            effects,
            preloads: Vec::new(),
            intent: PrefetchIntent::Explicit,
            access: ManifestAccess::Score(access.clone()),
            continue_on_error: false,
            layer: BankLayer::Score,
        })
    }

    /// Enqueue one accepted evaluation's score-authorized `samples(...)` and
    /// `preload(...)` effects as one ordered job and one aggregate deadline.
    ///
    /// Carrying the preloads with their registrations is load-bearing: a
    /// default map or earlier score may define the same sound name, but this
    /// job cannot warm it until its own last-writer-wins overrides publish.
    pub(crate) fn register_score_batch_async_for_session(
        &self,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: &ScoreSampleAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async(
            effects,
            preload_names,
            ManifestAccess::Score(access.clone()),
        )
    }

    /// Speculate about sounds named by a live score without downloading every
    /// GM instrument variant. Resolve after earlier registrations publish, so
    /// a score or global bank that overrides a GM name keeps its own semantics.
    pub fn warm_score_sounds_async(
        &self,
        names: &[String],
        access: &ScoreSampleAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async_into_with_intent(
            BankLayer::Score,
            Vec::new(),
            names,
            ManifestAccess::Score(access.clone()),
            PrefetchIntent::LiveWarm,
        )
    }

    #[cfg(test)]
    fn register_trusted_batch_async_for_test(
        &self,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async(effects, preload_names, ManifestAccess::Trusted)
    }

    fn register_batch_async(
        &self,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: ManifestAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async_into(BankLayer::Score, effects, preload_names, access)
    }

    fn register_batch_async_into(
        &self,
        layer: BankLayer,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: ManifestAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async_into_with_intent(
            layer,
            effects,
            preload_names,
            access,
            PrefetchIntent::Explicit,
        )
    }

    fn register_batch_async_into_with_intent(
        &self,
        layer: BankLayer,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: ManifestAccess,
        intent: PrefetchIntent,
    ) -> Result<PrefetchStatus, String> {
        validate_manifest_effects(
            effects
                .iter()
                .map(|(map, base)| (map.as_str(), base.as_deref())),
        )?;
        if let ManifestAccess::Score(policy) = &access {
            for (map, _) in &effects {
                policy.preapprove_effect(map)?;
            }
        }
        let preloads = copy_preload_specs(preload_names)?;
        let order = self
            .manifest_order
            .lock()
            .map_err(|_| "sample manifest enqueue lock is unavailable".to_owned())?;

        // With no registration ahead of us, preload can start now and report
        // an accurate public-facing count. Otherwise it must be carried in the
        // FIFO: consulting today's map would preload an about-to-be-stale URL.
        if effects.is_empty() && self.shared.manifest_pending.load(Ordering::Acquire) == 0 {
            let context = self.manifest_context();
            let mut requested = 0usize;
            for spec in preloads {
                match context.prefetch_known_with_intent(&spec, intent.priority(), intent) {
                    PrefetchStatus::Requested(files) => requested = requested.saturating_add(files),
                    PrefetchStatus::Unknown if intent.reports_unknown() => {
                        context.failure(format!("preload skipped unknown sound {spec:?}"));
                    }
                    PrefetchStatus::Unknown => {}
                    PrefetchStatus::Deferred => {
                        unreachable!("prefetch_known returns only requested or unknown")
                    }
                }
            }
            drop(order);
            return Ok(PrefetchStatus::Requested(requested));
        }

        // Marked before the push, not after: the worker may have the map
        // in before this thread runs again, and a mark placed then would
        // overwrite its verdict.
        let maps = effects
            .iter()
            .map(|(map, _)| {
                self.mark_source_loading(map);
                map.clone()
            })
            .collect::<Vec<_>>();
        if let Err(error) = self.enqueue_manifest_work_async_locked(
            ManifestWork::Custom {
                effects,
                preloads,
                intent,
                access,
                continue_on_error: true,
                layer,
            },
            MANIFEST_BATCH_TIMEOUT,
        ) {
            // Nothing will answer for these maps: the refusal is their
            // failure.
            for map in maps {
                self.shared
                    .note_source_standing(&map, SourceState::Failed(error.clone()));
            }
            return Err(error);
        }
        drop(order);
        Ok(PrefetchStatus::Deferred)
    }

    fn enqueue_manifest_work_async(&self, work: ManifestWork) -> Result<(), String> {
        self.enqueue_manifest_work_for(work, MANIFEST_BATCH_TIMEOUT)
    }

    fn enqueue_manifest_work_for(
        &self,
        work: ManifestWork,
        timeout: Duration,
    ) -> Result<(), String> {
        let _order = self
            .manifest_order
            .lock()
            .expect("sample manifest enqueue order");
        self.enqueue_manifest_work_async_locked(work, timeout)
    }

    fn validate_manifest_work(work: &ManifestWork) -> Result<(), String> {
        if let ManifestWork::Custom { effects, .. } = work {
            validate_manifest_effects(
                effects
                    .iter()
                    .map(|(map, base)| (map.as_str(), base.as_deref())),
            )?;
        }
        Ok(())
    }

    fn manifest_budget(&self, timeout: Duration) -> sample_fetch::FetchBudget {
        sample_fetch::FetchBudget::until(
            Instant::now() + timeout,
            self.shared.publication.cancellation(),
        )
    }

    fn enqueue_manifest_work_async_locked(
        &self,
        work: ManifestWork,
        timeout: Duration,
    ) -> Result<(), String> {
        Self::validate_manifest_work(&work)?;
        self.push_manifest_job(ManifestJob {
            work,
            timeout,
            completion: None,
        })
        .map_err(|refused| manifest_refusal(&refused))
    }

    /// Put `job` in the loader's line, counted as pending from before it is
    /// in: the worker may finish it before this returns.
    fn push_manifest_job(
        &self,
        job: ManifestJob,
    ) -> Result<(), mpsc::TrySendError<Box<ManifestJob>>> {
        self.shared.manifest_pending.fetch_add(1, Ordering::AcqRel);
        self.manifest_queue.push(Box::new(job)).inspect_err(|_| {
            self.shared.manifest_pending.fetch_sub(1, Ordering::AcqRel);
        })
    }

    fn enqueue_manifest_work_blocking(&self, work: ManifestWork) -> Result<(), String> {
        Self::validate_manifest_work(&work)?;
        let (completion, completed) = mpsc::sync_channel(1);
        let mut job = ManifestJob {
            work,
            timeout: MANIFEST_BATCH_TIMEOUT,
            completion: Some(completion),
        };
        // A full line is waited out, for as long as the job may take once
        // the worker has it.
        let room_by = Instant::now() + MANIFEST_BATCH_TIMEOUT;
        loop {
            let pushed = {
                let _order = self
                    .manifest_order
                    .lock()
                    .expect("sample manifest enqueue order");
                self.push_manifest_job(job)
            };
            match pushed {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) if Instant::now() < room_by => {
                    job = *returned;
                    std::thread::sleep(MANIFEST_QUEUE_WAIT_SLICE);
                }
                Err(refused) => return Err(manifest_refusal(&refused)),
            }
        }
        // The worker answers every job it takes, within the job's timeout.
        completed
            .recv()
            .unwrap_or_else(|_| Err("sample manifest loader stopped".to_owned()))
    }

    /// Font-zone lookup for gm_* names: variant by `n`, zone by midi range,
    /// exactly `registerSoundfonts`/`findZone`.
    fn resolve_soundfont(
        &self,
        fonts: &[Arc<str>],
        n: f64,
        midi: f64,
        priority: LoadPriority,
    ) -> SampleResolution {
        // An empty font list cannot resolve a sound.
        let Some(font) = fonts.get(sound_index(n, fonts.len())) else {
            return SampleResolution::Failed;
        };
        // A failure rests, then a real ask tries again; a bet does not. A
        // font that failed because the bank was full is the case that
        // matters: by the time the rest is over the studio may have handed
        // ids back, and a font marked failed for good would never find out.
        let rested = |state: Option<&FontState>| match state {
            None => true,
            Some(FontState::Failed { at }) => {
                priority == LoadPriority::Now && at.elapsed() >= FAILED_RETRY_AFTER
            }
            Some(FontState::Loading | FontState::Ready(_)) => false,
        };
        {
            let table = self.shared.fonts.read().expect("font table");
            match table.get(font) {
                Some(FontState::Ready(zones)) => {
                    // `zone.keyRangeLow <= pitch && zone.keyRangeHigh + 1 >= pitch`
                    let Some(zone) = zones
                        .iter()
                        .find(|zone| zone.key_lo <= midi && zone.key_hi + 1.0 >= midi)
                    else {
                        return SampleResolution::Failed;
                    };
                    return SampleResolution::Found {
                        id: zone.id,
                        // playbackRate = 2^((100·midi − baseDetune)/1200); the
                        // caller applies 2^(transpose/12).
                        transpose: (100.0 * midi - zone.base_detune) / 100.0,
                        duration_secs: zone.duration_secs,
                        loop_secs: zone.loop_secs,
                        // getParamADSR(..., 0, 0.3, …) - the soundfont peak.
                        envelope_peak: 0.3,
                        soundfont: true,
                    };
                }
                Some(FontState::Loading) => {
                    if priority == LoadPriority::Now {
                        self.shared.font_jobs.promote(font);
                    }
                    return SampleResolution::Loading;
                }
                state @ Some(FontState::Failed { .. }) if !rested(state) => {
                    return SampleResolution::Failed;
                }
                _ => {}
            }
        }
        queue_font(&self.shared, font, priority);
        SampleResolution::Loading
    }

    /// Decoded samples awaiting installation into a playing backend. The
    /// session drains this into the live device (between producer steps) or
    /// into an offline backend before rendering.
    pub fn take_ready(&self) -> Vec<(SampleId, DecodedSample)> {
        std::mem::take(&mut self.shared.ready.lock().expect("ready samples").samples)
    }

    /// Decoded samples waiting to be installed that `known` does not already
    /// account for, cloned and left where they are.
    ///
    /// An owner with nowhere to install them still owes their memory to its
    /// policy: the queue keeps whatever the loaders finish until a device
    /// drains it.
    pub fn peek_ready_unless(
        &self,
        known: impl Fn(SampleId) -> bool,
    ) -> Vec<(SampleId, DecodedSample)> {
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .samples
            .iter()
            .filter(|(id, _)| !known(*id))
            .cloned()
            .collect()
    }

    /// The current decoded payload for a slot still awaiting installation,
    /// cloned without draining it. A payload already taken for installation
    /// is available from the caller's retained copy instead.
    pub fn peek_ready_sample(&self, id: SampleId) -> Option<DecodedSample> {
        let ready = self.shared.ready.lock().expect("ready samples");
        let identity = ready.identity(id)?;
        ready
            .samples
            .iter()
            .rev()
            .find(|(queued_id, decoded)| *queued_id == id && decoded.identity() == identity)
            .map(|(_, decoded)| decoded.clone())
    }

    /// Exact immutable payload currently intended for this sample slot.
    /// Published with decoded PCM, before its URL/font becomes Ready, and kept
    /// after [`Self::take_ready`] even if device installation is deferred. This
    /// is neither an installation acknowledgement nor a playback receipt.
    pub fn decoded_identity(&self, id: SampleId) -> Option<u64> {
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .identity(id)
    }

    /// Hand a decoded sample back after a failed install so a later drain
    /// retries it (the device's install ring was momentarily full). Forgotten
    /// slots and retries superseded by a newer decoded body are discarded.
    pub fn requeue_ready(&self, id: SampleId, decoded: DecodedSample) {
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .push((id, decoded));
    }

    /// Atomically put older retry/recovery PCM before publications completed
    /// since the caller last drained the queue, dropping an older item when a
    /// newer same-ID payload is already known, even if it has left the queue.
    /// Loader threads append under the same mutex, so stale recovery PCM cannot
    /// overwrite a publication that won the race to this queue.
    pub fn requeue_ready_batch_before_newer(&self, older: Vec<(SampleId, DecodedSample)>) {
        if older.is_empty() {
            return;
        }
        let mut ready = self.shared.ready.lock().expect("ready samples");
        let mut seen = [false; SAMPLE_BANK_CAPACITY];
        for (id, _) in ready.samples.iter() {
            if let Some(seen) = seen.get_mut(id.0 as usize) {
                *seen = true;
            }
        }
        // Walk backwards so only the newest item per id in the older batch is
        // kept. Anything already in `ready` was published after that batch and
        // supersedes it completely. Besides preserving last-writer-wins, this
        // prevents repeated recycle attempts from growing duplicate PCM.
        let mut prepend = Vec::with_capacity(older.len().min(SAMPLE_BANK_CAPACITY));
        for (id, decoded) in older.into_iter().rev() {
            let Some(seen) = seen.get_mut(id.0 as usize) else {
                continue;
            };
            if !*seen && ready.accept_identity(id, decoded.identity()) {
                *seen = true;
                prepend.push((id, decoded));
            }
        }
        prepend.reverse();
        prepend.append(&mut ready.samples);
        ready.samples = prepend;
    }

    /// One line per NEW failure since the last call (loud-once logging).
    pub fn take_failures(&self) -> Vec<String> {
        self.take_failures_with_maps()
            .into_iter()
            .map(|failure| failure.message)
            .collect()
    }

    /// [`Self::take_failures`], each with the maps it leaves failed.
    pub fn take_failures_with_maps(&self) -> Vec<SampleFailure> {
        std::mem::take(&mut *self.shared.failures.lock().expect("sample failures"))
    }

    /// Suppress loader-side informational stderr writes for full-screen
    /// clients. Error delivery remains available through [`Self::take_failures`].
    pub fn set_direct_diagnostic_logging(&self, enabled: bool) {
        self.shared
            .direct_diagnostic_logging
            .store(enabled, Ordering::Release);
    }

    /// Take the open set's own folder up as sample banks.
    ///
    /// A set is a folder of scores, and audio a player drops beside them is
    /// theirs: it belongs in the browser and in `s(...)` with nothing
    /// written and no flag passed. `samples('local:...')` asks a host grant
    /// to do exactly this, which is why the grant is the studio's to give
    /// rather than the score's to ask for.
    ///
    /// A file in a subfolder is a variant of that folder's bank, so
    /// `kicks/1.wav` and `kicks/2.wav` are `s("kicks")` and `s("kicks:1")`.
    /// Files directly inside the set folder form one bank named after that
    /// folder (see `folder_banks`). A local file also has a filename
    /// address when `local_names::index` accepts its stem, so
    /// `deep_bass.wav` is `s("deep_bass")`.
    ///
    /// `sessions/` and `exports/` are skipped: those are the set's takes,
    /// its tapes and its bounces, and last night's recording is not an
    /// instrument.
    ///
    /// A bank takes the name it is given, so a set holding a `bd` folder plays
    /// its own `bd` - the precedence a score's `samples()` already has over
    /// the pinned banks. The browser files it under `set`, so which one is
    /// sounding is on screen rather than a surprise. Answering again
    /// replaces the last set's banks: they belong to that set, and leaving
    /// them behind would let one set's `kicks` play in another. What each of
    /// them displaced goes back on the way out, so a `bd` this set took over
    /// from a score's import is the score's again at the next one.
    pub fn adopt_set_folder(&self, folder: &Path) -> Result<usize, String> {
        self.adopt_set_folder_inner(folder, false)
    }

    /// Re-read the open set's folder, so the set holds what is on disk now.
    /// Unlike switching sets, a refresh that fails leaves the current banks
    /// in place, and a name a score or prebake redefined stays theirs.
    pub fn refresh_set_folder(&self, folder: &Path) -> Result<usize, String> {
        self.adopt_set_folder_inner(folder, true)
    }

    fn adopt_set_folder_inner(&self, folder: &Path, refresh: bool) -> Result<usize, String> {
        // Read before either lock is taken: the folder is walked on the
        // thread that opened the set, and the browser reads these two at
        // the other end of them.
        let staged: Result<Vec<(String, Bank)>, String> = folder
            .canonicalize()
            .map_err(|error| format!("set samples: cannot read {}: {error}", folder.display()))
            .and_then(|root| set_folder_banks(&root))
            .map(|banks| {
                banks
                    .into_iter()
                    .map(|(name, bank)| (self.renamed(name), bank))
                    .collect()
            });
        if refresh && let Err(error) = &staged {
            return Err(error.clone());
        }
        let mut custom = self.custom.write().expect("custom banks");
        let mut adopted = self.shared.set_banks.write().expect("set banks");
        // A refresh leaves alone a name the session holds outside this set:
        // a score or prebake registered it, and only opening a set takes a
        // name over (see `publish_score_custom`).
        let staged = staged.map(|banks| {
            banks
                .into_iter()
                .filter(|(name, _)| {
                    !refresh || adopted.contains_key(name) || !custom.contains_key(name)
                })
                .collect::<Vec<_>>()
        });
        let limit = SAMPLE_BANK_CAPACITY - 1;
        // The session's size once this set's banks are put down and the
        // folder's are taken up, counted the way `publish_score_custom`
        // counts it: a name already held is replaced rather than added, and
        // the session's total is what the limit is on - not anything the
        // set's folder is responsible for. It is counted before either map
        // changes, so a refresh that does not fit changes nothing.
        let full = staged
            .as_ref()
            .is_ok_and(|banks| {
                let held = |name: &str| {
                    custom.contains_key(name) && !matches!(adopted.get(name), Some(None))
                };
                let kept = custom.keys().filter(|name| held(name)).count();
                let additional = banks.iter().filter(|(name, _)| !held(name)).count();
                kept.saturating_add(additional) > limit
            })
            .then(|| format!("the session holds more than the {limit}-bank limit"));
        if refresh && let Some(error) = full {
            return Err(error);
        }
        // Switching sets closes the previous one even when the new folder
        // does not read or does not fit: one set's `kicks` never plays in
        // the next.
        for (name, displaced) in std::mem::take(&mut *adopted) {
            match displaced {
                Some(bank) => {
                    custom.insert(name, bank);
                }
                None => {
                    custom.remove(&name);
                }
            }
        }
        *self
            .shared
            .custom_file_names
            .write()
            .expect("custom filenames") = local_names::index(custom.values());
        let staged = staged?;
        if let Some(error) = full {
            return Err(error);
        }
        for (name, bank) in staged {
            adopted.insert(name.clone(), custom.insert(name, bank));
        }
        *self
            .shared
            .custom_file_names
            .write()
            .expect("custom filenames") = local_names::index(custom.values());
        Ok(adopted.len())
    }

    /// Rename imported banks, by the name they arrived under.
    ///
    /// Only the imported layers answer to this - the set's own folder and
    /// the sources in Settings. The pinned banks are shared vocabulary: a
    /// score that says `bd` means the same thing on anybody's machine, and
    /// a local rename of one would quietly stop being true the moment the
    /// set was handed over.
    pub fn set_bank_renames(&self, renames: HashMap<String, String>) {
        *self.shared.renames.write().expect("bank renames") = renames;
        // The imported layer answers to the overlay too, and its rows are
        // still here to be read through it again.
        rebuild_global(&self.shared, &self.global);
    }

    /// Apply aliases using the source plus original name as the identity.
    pub fn set_source_bank_renames(&self, renames: HashMap<(String, String), String>) {
        *self
            .shared
            .source_renames
            .write()
            .expect("source bank renames") = renames;
        rebuild_global(&self.shared, &self.global);
    }

    pub fn alias_for_import(&self, spec: &str, original: &str) -> String {
        self.shared
            .source_renames
            .read()
            .expect("source bank renames")
            .get(&(spec.to_owned(), original.to_owned()))
            .cloned()
            .or_else(|| {
                self.shared
                    .auto_aliases
                    .read()
                    .expect("automatic bank aliases")
                    .get(&(spec.to_owned(), original.to_owned()))
                    .cloned()
            })
            .or_else(|| {
                self.shared
                    .renames
                    .read()
                    .expect("bank renames")
                    .get(original)
                    .cloned()
            })
            .unwrap_or_else(|| original.to_owned())
    }

    pub fn original_for_import_alias(&self, spec: &str, alias: &str) -> Option<String> {
        self.banks_for_import(spec)
            .into_iter()
            .find(|original| self.alias_for_import(spec, original) == alias)
    }

    /// The name an imported bank goes by, after the player's renames.
    fn renamed(&self, name: String) -> String {
        self.shared
            .renames
            .read()
            .expect("bank renames")
            .get(&name)
            .cloned()
            .unwrap_or(name)
    }

    /// Register the sources the player added in Settings, replacing the
    /// ones registered before.
    ///
    /// A folder is walked on the manifest worker, and a URL pack is a
    /// manifest the loader fetches. Both arrive later: the state is
    /// `Loading` until then, and the source keeps the banks of its previous
    /// adoption in the meantime. A folder that does not exist is reported
    /// as `Missing` at once. All names go in the `global` layer, below the
    /// open set's folder and above the pinned banks.
    ///
    /// A source that cannot be read is reported and skipped. It is never
    /// fatal: a folder on an unplugged drive must not stop the studio from
    /// opening, and the sound browser shows the source as missing.
    pub fn adopt_global_sources(&self, sources: &[GlobalSource]) -> Vec<GlobalSourceReport> {
        let sources = deduplicated_global_sources(sources);
        let generation = self
            .shared
            .global_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let previous = std::mem::take(
            &mut *self
                .shared
                .global_slots
                .lock()
                .expect("global source slots"),
        );
        let mut slots: Vec<GlobalSlot> = Vec::with_capacity(sources.len());
        let mut packs: Vec<(usize, String)> = Vec::new();
        let mut folders: Vec<String> = Vec::new();
        for (row, source) in sources.iter().enumerate() {
            let spec = source.spec.trim();
            if spec.is_empty() {
                continue;
            }
            if !source.enabled {
                slots.push(GlobalSlot {
                    row,
                    spec: spec.to_owned(),
                    kind: if folder_of_spec(spec).is_some() {
                        GlobalKind::Folder
                    } else {
                        GlobalKind::Pack
                    },
                    banks: HashMap::new(),
                    state: GlobalSourceState::Off,
                    rewalk: false,
                });
                continue;
            }
            if let Some(folder) = folder_of_spec(spec) {
                // A folder that is not there has nothing to walk: its row
                // says so at once, and nothing waits in line for it.
                if matches!(Path::new(folder.trim()).try_exists(), Ok(false)) {
                    slots.push(GlobalSlot {
                        row,
                        spec: spec.to_owned(),
                        kind: GlobalKind::Folder,
                        banks: HashMap::new(),
                        state: GlobalSourceState::Missing("no such folder".to_owned()),
                        rewalk: false,
                    });
                    continue;
                }
                // The walk happens on the manifest worker, exactly as a
                // pack's fetch does. A folder keeps what it brought last
                // time until the new walk lands, so re-adopting - which
                // every add, remove and reorder does - never silences a
                // library for the length of a directory scan.
                let carried = previous
                    .iter()
                    .find(|slot| slot.kind == GlobalKind::Folder && slot.spec == spec)
                    .map(|slot| slot.banks.clone())
                    .unwrap_or_default();
                slots.push(GlobalSlot {
                    row,
                    spec: spec.to_owned(),
                    kind: GlobalKind::Folder,
                    banks: carried,
                    state: GlobalSourceState::Loading,
                    rewalk: false,
                });
                folders.push(spec.to_owned());
                continue;
            }
            // A pack keeps what it brought last time until its new list
            // lands: a refetch must not silence it in the meantime.
            let carried = previous
                .iter()
                .find(|slot| slot.kind == GlobalKind::Pack && slot.spec == spec)
                .map(|slot| slot.banks.clone())
                .unwrap_or_default();
            slots.push(GlobalSlot {
                row,
                spec: spec.to_owned(),
                kind: GlobalKind::Pack,
                banks: carried,
                state: GlobalSourceState::Loading,
                rewalk: false,
            });
            packs.push((row, spec.to_owned()));
        }
        *self
            .shared
            .global_slots
            .lock()
            .expect("global source slots") = slots;
        rebuild_global(&self.shared, &self.global);
        if !folders.is_empty()
            && let Err(error) = self.enqueue_manifest_work_async_locked(
                ManifestWork::Folders {
                    specs: folders.clone(),
                    generation,
                },
                MANIFEST_BATCH_TIMEOUT,
            )
        {
            // The queue refused the walk, so nothing will ever fill these
            // rows: say so on them rather than leaving `fetching…` up for
            // the rest of the session.
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            for slot in slots.iter_mut() {
                if slot.kind == GlobalKind::Folder && folders.contains(&slot.spec) {
                    slot.state = GlobalSourceState::Failed(error.clone());
                }
            }
        }
        if !packs.is_empty()
            && let Err(error) = self.register_global_packs(&packs, generation)
        {
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            for slot in slots.iter_mut() {
                if slot.kind == GlobalKind::Pack && slot.state == GlobalSourceState::Loading {
                    slot.state = GlobalSourceState::Failed(error.clone());
                }
            }
        }
        self.global_source_reports()
    }

    /// Adopt, and wait for every folder walk to land.
    ///
    /// For tests that want the settled reading rather than the one on the
    /// way to it. A studio never waits: its rows say `fetching…` and the
    /// half-second poll picks them up.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn adopt_global_sources_settled(
        &self,
        sources: &[GlobalSource],
    ) -> Vec<GlobalSourceReport> {
        let _ = self.adopt_global_sources(sources);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.shared.manifest_pending.load(Ordering::Acquire) != 0 && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.global_source_reports()
    }

    /// How every imported source stands right now, in row order - for the
    /// sources page, which asks again as packs land.
    pub fn global_source_reports(&self) -> Vec<GlobalSourceReport> {
        let slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        let mut rows: Vec<&GlobalSlot> = slots.iter().collect();
        rows.sort_by_key(|slot| slot.row);
        rows.iter()
            .map(|slot| GlobalSourceReport {
                spec: slot.spec.clone(),
                state: slot.state.clone(),
            })
            .collect()
    }

    /// The fetch policy the host gives a score, so an imported pack is
    /// granted no less: a pack on one host whose samples live on another is
    /// as ordinary as a score that does the same.
    pub fn set_import_policy(&self, access: ScoreSampleAccess) {
        *self.shared.import_policy.lock().expect("import policy") = Some(access);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn import_policy_for_test(&self) -> Option<ScoreSampleAccess> {
        self.shared
            .import_policy
            .lock()
            .expect("import policy")
            .clone()
    }

    /// Ask the loader for the packs the player named in Settings, in one
    /// job.
    ///
    /// A URL that the player types in Settings is a grant, like
    /// `--allow-sample-origin` for a score from another author. Each pack's
    /// origin is permitted exactly, in addition to the policy a score gets.
    /// A pack's `_base` can then point at a CDN, as a score's imports can.
    fn register_global_packs(
        &self,
        packs: &[(usize, String)],
        generation: u64,
    ) -> Result<(), String> {
        let mut access = self
            .shared
            .import_policy
            .lock()
            .expect("import policy")
            .clone()
            .unwrap_or_else(|| {
                let mut access = ScoreSampleAccess::denied();
                access.permit_public_cors_origins();
                access
            });
        let mut effects = Vec::with_capacity(packs.len());
        for (_, spec) in packs {
            let SampleSource::Url(url) = read_source(spec, "") else {
                return Err(format!("{spec}: not a URL source"));
            };
            let origin = Url::parse(&url)
                .map_err(|error| format!("{spec}: invalid sample source: {error}"))?
                .origin()
                .ascii_serialization();
            access.permit_origin(&origin)?;
            effects.push((
                serde_json::to_string(spec).map_err(|error| error.to_string())?,
                None,
            ));
        }
        self.register_batch_async_into(
            BankLayer::Global { generation },
            effects,
            &[],
            ManifestAccess::Score(access),
        )
        .map(|_| ())
    }

    /// Forget a pack's cached list, so the next adoption asks for it again
    /// rather than answering out of the cache: Enter on the row means "go
    /// and get it", and a `shabda:` source means a fresh draw.
    pub fn forget_manifest(&self, spec: &str) {
        let SampleSource::Url(url) = read_source(spec.trim(), "") else {
            return;
        };
        self.forget_manifest_url(&url);
        let map = serde_json::to_string(spec.trim()).unwrap_or_default();
        self.shared
            .source_tables
            .lock()
            .expect("samples source tables")
            .forget_state(&map);
    }

    /// Drop a pinned default pack's cached manifest so the next load asks
    /// the network again. Sample files already on disk are kept.
    pub fn forget_default_manifest(&self, url: &str) {
        self.forget_manifest_url(url);
    }

    /// Refetch every shipped pack's list from the network and merge what
    /// changed. Banks under those packs are cleared first so a refresh
    /// does not stack duplicate names.
    pub fn refresh_default_sources(&self) -> Result<(), String> {
        let pinned: PinnedFile = serde_json::from_str(PINNED_BANKS)
            .map_err(|error| format!("pinned sample-banks.json: {error}"))?;
        for source in &pinned.sources {
            self.forget_default_manifest(&source.url);
        }
        let bases: Vec<String> = Self::default_sources()
            .iter()
            .filter(|source| !self.is_font_source(source))
            .map(|source| source.base.clone())
            .collect();
        {
            let mut banks = self.banks.write().expect("default banks");
            banks.retain(|_, bank| {
                bank_file_urls(bank).first().is_none_or(|url| {
                    !bases
                        .iter()
                        .any(|base| url.starts_with(&*source_prefix(base)))
                })
            });
            banks.extend(inline_banks_from(&pinned));
        }
        self.enqueue_manifest_work_async(ManifestWork::Defaults {
            sources: pinned.sources,
            cache_dir: self.shared.host_cache.clone(),
        })
    }

    /// How many of these urls are on disk, and the bytes they take.
    pub fn count_cached_urls<'a>(
        &self,
        urls: impl IntoIterator<Item = &'a Arc<str>>,
    ) -> (usize, u64) {
        let mut held = 0usize;
        let mut bytes = 0u64;
        for url in urls {
            if let Some(len) = cache_entry_bytes(&self.shared.host_cache, url) {
                held += 1;
                bytes = bytes.saturating_add(len);
            }
        }
        (held, bytes)
    }

    fn forget_manifest_url(&self, url: &str) {
        let path = cache_path(&self.shared.host_cache, url);
        let _ = std::fs::remove_file(&path);
        let cache = &self.shared.score_cache;
        for trust in [ScoreCacheTrust::Grant, ScoreCacheTrust::Cors] {
            let path = cache.path(url, ScoreCacheKind::Manifest, trust);
            let _ = with_score_cache_lock(&cache.base, || match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!("forget {}: {error}", path.display())),
            });
        }
    }

    /// Whether the set's own folder already answers `name`.
    ///
    /// Consolidating must not write over audio the player put in the set: a
    /// `bd` folder beside the scores is the set's `bd`, and the pinned bank
    /// must not replace it.
    pub fn set_folder_holds(&self, root: &Path, name: &str) -> bool {
        // The prefix is spelled the way `folder_banks` writes its URLs: the
        // raw path, not a percent-encoded URL, so a set path with a space
        // still matches. The separator is this platform's, because
        // `root.join(...)` builds those URLs and joins with a backslash on
        // Windows.
        let spelled =
            |root: &Path| format!("{}{}", local_file_url(root), std::path::MAIN_SEPARATOR);
        // Both spellings are tried, because the URLs carry whatever root the
        // caller handed `folder_banks` and this is handed one that may not be
        // the same text. Canonicalising is what reconciles them on Unix, where
        // the usual difference is a symlinked temporary directory. On Windows
        // `canonicalize` answers in the `\\?\` verbatim form, which
        // `local_file_url` strips from both sides, so the two spellings meet
        // in the middle. Accepting either is what makes "does this folder
        // hold that bank" mean the same thing on both.
        let mut prefixes = vec![spelled(root)];
        if let Ok(canonical) = root.canonicalize() {
            let canonical = spelled(&canonical);
            if !prefixes.contains(&canonical) {
                prefixes.push(canonical);
            }
        }
        let under = |url: &str| prefixes.iter().any(|prefix| url.starts_with(prefix));
        let holds = |bank: &Bank| match bank {
            Bank::Array(urls) => urls.iter().any(|url| under(url)),
            Bank::Notes(notes) => notes
                .iter()
                .any(|(_, urls)| urls.iter().any(|url| under(url))),
        };
        // Only the set's own layer is asked, not the whole chain: a name
        // the score's `samples()` took over is not the folder's.
        self.custom
            .read()
            .expect("custom banks")
            .get(name)
            .is_some_and(holds)
    }

    /// Copy every file behind `name` into `root`, under a folder of its
    /// own, and report how many landed.
    ///
    /// The naming is the scan's own rule read backwards: files inside a
    /// folder become a bank called after the folder, so `name/0.wav`,
    /// `name/1.wav` is what makes the copied bank answer to `name` - and
    /// `n` still picks between them, in the order the bank had.
    ///
    /// Only what is already downloaded is copied. A sound whose files have
    /// never been fetched is reported as nothing copied rather than
    /// silently reaching the network in the middle of a file operation.
    pub fn copy_bank_into(&self, name: &str, root: &Path) -> Result<usize, String> {
        enum Copyable {
            Files(Vec<Arc<str>>),
            Keyed,
            Font,
        }
        let Some(found) = self.look_up(name, |named| match named {
            Named::Bank(Bank::Array(urls)) => Copyable::Files(urls.to_vec()),
            // A keyed bank picks its file by the note nearest the key it is
            // filed under; laid out as a row the scan would read it back
            // by `n`, transposed from C3 - the same name, a different
            // instrument. It is left where it is rather than copied wrong.
            Named::Bank(Bank::Notes(_)) => Copyable::Keyed,
            // A soundfont is a font file and a decode of it, not a set of
            // samples with names: there is nothing to lay in a folder that
            // would play the same way.
            Named::Font(_) => Copyable::Font,
        }) else {
            return Ok(0);
        };
        let urls = match found {
            Copyable::Files(urls) => urls,
            Copyable::Keyed => {
                return Err(format!(
                    "{name} is a keyed bank and cannot be laid out as a row; it stays where it is"
                ));
            }
            Copyable::Font => return Ok(0),
        };
        if urls.is_empty() {
            return Ok(0);
        }
        // All or nothing: a bank copied with gaps is re-read by the scan
        // with the gaps closed, and `s("bd:7")` plays what used to be
        // `bd:9`. Better to say what is missing than to change the score's
        // meaning in the act of making it portable.
        let mut files = Vec::with_capacity(urls.len());
        let mut missing = 0usize;
        for url in &urls {
            match self.cached_bytes_for(url) {
                Some(bytes) => files.push((url.clone(), bytes)),
                None => missing += 1,
            }
        }
        if missing > 0 {
            return Err(format!(
                "{missing} of {} files are not downloaded yet - play it once, or turn on fetch imports",
                urls.len()
            ));
        }
        let folder = root.join(name);
        std::fs::create_dir_all(&folder)
            .map_err(|error| format!("create {}: {error}", folder.display()))?;
        // Zero-padded, because the scan orders a bank's files by name and
        // `10.wav` sorts before `2.wav`.
        let width = urls.len().to_string().len();
        let mut copied = 0usize;
        for (index, (url, bytes)) in files.iter().enumerate() {
            // The extension the scan will accept, read the way the decoder
            // reads it: from the path, not from a query or a fragment.
            let extension = match codec_for(url) {
                Codec::Mp3 => "mp3",
                Codec::Ogg => "ogg",
                _ => "wav",
            };
            let into = folder.join(format!("{index:0width$}.{extension}"));
            std::fs::write(&into, bytes)
                .map_err(|error| format!("write {}: {error}", into.display()))?;
            copied += 1;
        }
        Ok(copied)
    }

    /// The bytes behind a url, if this machine already has them: a local
    /// file read straight, a downloaded one out of the cache.
    ///
    /// Never a fetch. Consolidating is a file operation the player asked
    /// for, and one that quietly went to the network in the middle would
    /// be a very different thing from the one they asked for.
    fn cached_bytes_for(&self, url: &str) -> Option<Vec<u8>> {
        if url.starts_with("file://") {
            return fetch_located(url).ok();
        }
        // A url a score or a Settings pack named was fetched under a grant
        // and lives in the score cache, keyed by the trust its grant
        // carried. The pinned banks live in the plain cache beside it.
        let granted = self
            .shared
            .score_sources
            .read()
            .expect("score sample sources")
            .contains_key(url);
        if granted {
            let cache = &self.shared.score_cache;
            return [ScoreCacheTrust::Grant, ScoreCacheTrust::Cors]
                .into_iter()
                .find_map(|trust| {
                    read_sample_cache(&cache.path(url, ScoreCacheKind::Audio, trust)).ok()
                });
        }
        read_sample_cache(&cache_path(&cache_dir(), url)).ok()
    }

    /// Every sound a score can name right now, for a browser: banks from
    /// the pinned manifests, banks a score registered, and the General MIDI
    /// fonts, each with how many numbered variants `n` can pick from.
    /// Banks a Settings import brought, under the names they play as.
    pub fn banks_for_import(&self, spec: &str) -> Vec<String> {
        // Read original names from the source row. An alias must not erase
        // the identity used to edit that same alias again.
        let slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        let mut names: Vec<String> = slots
            .iter()
            .filter(|slot| slot.spec == spec)
            .flat_map(|slot| slot.banks.keys().cloned())
            .collect();
        drop(slots);
        // Score-shaped imports may exist outside a Settings source slot.
        let imports = self.shared.bank_imports.read().expect("bank imports");
        names.extend(
            imports
                .iter()
                .filter(|(_, import)| import.as_ref() == spec)
                .map(|(name, _)| name.clone()),
        );
        drop(imports);
        names.sort();
        names.dedup();
        names
    }

    /// Automatic collision aliases currently assigned to one Settings source.
    pub fn automatic_aliases_for_import(&self, spec: &str) -> Vec<(String, String)> {
        let aliases = self
            .shared
            .auto_aliases
            .read()
            .expect("automatic bank aliases");
        let mut found: Vec<_> = aliases
            .iter()
            .filter(|((source, _), _)| source == spec)
            .map(|((_, original), alias)| (original.clone(), alias.clone()))
            .collect();
        found.sort();
        found
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_catalogue_lock_for_tests<T>(&self, f: impl FnOnce() -> T) -> T {
        let _banks = self.custom.write().expect("custom banks");
        f()
    }

    pub fn catalogue(&self) -> Vec<SoundEntry> {
        let mut entries = Vec::new();
        let variants = |bank: &Bank| match bank {
            Bank::Array(urls) => urls.len(),
            Bank::Notes(notes) => notes.iter().map(|(_, urls)| urls.len()).max().unwrap_or(1),
        };
        let location = |bank: &Bank| match bank {
            Bank::Array(urls) => urls.first().map(|url| url.to_string()),
            Bank::Notes(notes) => notes
                .iter()
                .find_map(|(_, urls)| urls.first())
                .map(|url| url.to_string()),
        };
        // One order for the three, and the same one `publish_score_custom`
        // and `adopt_set_folder` take them in: `custom`, then the imports,
        // then the set's. The browser reads them on the studio's thread
        // while the loader writes them on the worker's, and a pair taken in
        // both orders is a pair that can wait on itself.
        let custom = self.custom.read().expect("custom banks");
        let imports = self.shared.bank_imports.read().expect("bank imports");
        let of_set = self.shared.set_banks.read().expect("set banks");
        for (name, bank) in custom.iter() {
            // The set's own folder brought this one, so say so: a `bd` that
            // is the set's rather than the pinned one is the difference
            // between a surprise and a choice.
            let set = of_set.contains_key(name);
            entries.push(SoundEntry {
                name: name.clone(),
                variants: variants(bank),
                variant_names: local_variant_names(bank),
                origin: if set {
                    SoundOrigin::Set
                } else {
                    SoundOrigin::Score
                },
                category: if set {
                    SoundCategory::Set
                } else {
                    SoundCategory::Score
                },
                location: location(bank),
                import: imports.get(name).map(|import| import.to_string()),
            });
        }
        drop(of_set);
        drop(imports);
        drop(custom);
        let mut names: HashSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        let source_of = self.shared.global_source_of.read().expect("global sources");
        for (name, bank) in self.global.read().expect("global banks").iter() {
            if !names.insert(name.clone()) {
                continue;
            }
            entries.push(SoundEntry {
                name: name.clone(),
                variants: variants(bank),
                variant_names: local_variant_names(bank),
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: location(bank),
                import: source_of.get(name).map(|spec| spec.to_string()),
            });
        }
        drop(source_of);
        for (name, bank) in self.banks.read().expect("default banks").iter() {
            if !names.insert(name.clone()) {
                continue;
            }
            let location = location(bank);
            entries.push(SoundEntry {
                name: name.clone(),
                variants: variants(bank),
                variant_names: local_variant_names(bank),
                origin: SoundOrigin::Default,
                category: SoundCategory::of_location(location.as_deref()),
                location,
                import: None,
            });
        }
        for (name, fonts) in self.gm.iter() {
            if !names.insert(name.clone()) {
                continue;
            }
            entries.push(SoundEntry {
                name: name.clone(),
                variants: fonts.len(),
                variant_names: Vec::new(),
                origin: SoundOrigin::Font,
                category: SoundCategory::Font,
                location: None,
                import: None,
            });
        }
        // The synths, unless a bank has taken the name - a bank wins at
        // play time, so the browser says the same.
        for name in rustel_voice::NATIVE_SYNTH_SOUNDS {
            if !names.insert((*name).to_owned()) {
                continue;
            }
            entries.push(SoundEntry {
                name: (*name).to_owned(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Synth,
                category: SoundCategory::Synth,
                location: None,
                import: None,
            });
        }
        // The audio input stands with the synths: what `s("…")` plays
        // without a sample. Its channels are its variants, and how many
        // there are is the device's to say - a browser with a device sets
        // the count; here it is one, the channel `in` alone plays.
        if names.insert("in".to_owned()) {
            entries.push(SoundEntry {
                name: "in".to_owned(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Input,
                category: SoundCategory::Synth,
                location: None,
                import: None,
            });
        }
        // Case-insensitive lookup registers a lower-case key beside a
        // canonical mixed-case bank. They are two spellings of the same
        // sound, not two browser rows. Keep the canonical spelling while
        // leaving genuinely distinct banks that happen to differ by case.
        let mut visible: Vec<SoundEntry> = Vec::with_capacity(entries.len());
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for entry in entries {
            let candidates = by_name.entry(entry.name.to_ascii_lowercase()).or_default();
            let same = candidates.iter().copied().find(|&index| {
                let shown = &visible[index];
                shown.variants == entry.variants
                    && shown.origin == entry.origin
                    && shown.category == entry.category
                    && shown.location == entry.location
                    && shown.import == entry.import
            });
            if let Some(index) = same {
                let shown = &mut visible[index];
                let shown_has_case = shown.name.chars().any(char::is_uppercase);
                let entry_has_case = entry.name.chars().any(char::is_uppercase);
                if entry_has_case && !shown_has_case {
                    *shown = entry;
                }
            } else {
                candidates.push(visible.len());
                visible.push(entry);
            }
        }
        let mut entries = visible;
        // Synths first, then the banks by name: the browser groups the
        // synths under one heading at the top.
        entries.sort_by_key(|entry| {
            (
                !matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input),
                entry.name.to_lowercase(),
            )
        });
        entries
    }

    /// Where one sound stands, and - for a name that is known but not yet
    /// fetched - the request that fetches it. Asking is what starts the
    /// download, so a score can be warmed as its names are typed. Asked as
    /// a bet: it queues behind what plays, and does not retry a failure.
    pub fn readiness(&self, name: &str, n: f64) -> SoundReadiness {
        self.readiness_at(name, n, 36.0)
    }

    /// [`Self::readiness`] for the file a note picks: in a bank keyed by
    /// note, `midi` chooses the key.
    pub fn readiness_at(&self, name: &str, n: f64, midi: f64) -> SoundReadiness {
        if rustel_voice::is_native_synth_sound(name) {
            return SoundReadiness::Ready;
        }
        match self.resolve_with_priority(name, n, midi, LoadPriority::Bet) {
            SampleResolution::Found { .. } => SoundReadiness::Ready,
            SampleResolution::Loading => SoundReadiness::Loading,
            SampleResolution::Failed => SoundReadiness::Failed,
            SampleResolution::Unknown => SoundReadiness::Unknown,
        }
    }

    /// The keys of a bank whose files are picked by note, as MIDI numbers;
    /// `None` for any other kind of sound, or a name not known yet.
    pub fn note_keys(&self, name: &str) -> Option<Vec<f64>> {
        self.look_up(name, |named| match named {
            Named::Bank(Bank::Notes(notes)) => Some(notes.iter().map(|(key, _)| *key).collect()),
            _ => None,
        })
        .flatten()
    }

    /// The shape of the sample `s("name")` would play, and how long it
    /// lasts.
    ///
    /// `name` is spelled the way a browser spells it: `bd`, or `bd:3` for
    /// a numbered variant. `None` until the sample has been decoded once,
    /// because the shape is measured at decode - the only moment the
    /// library holds the audio. A soundfont has no one shape and answers
    /// nothing.
    pub fn sound_shape(&self, name: &str) -> Option<(Arc<[u8]>, f64)> {
        let (bank, n) = match name.rsplit_once(':') {
            Some((head, tail)) => match tail.parse::<f64>() {
                Ok(n) if !head.is_empty() => (head, n),
                _ => (name, 0.0),
            },
            None => (name, 0.0),
        };
        let url = self.look_up(bank, |named| match named {
            // A browser preview carries no `note`, so the sampler uses its
            // ordinary MIDI 36 default. This must choose the same key as the
            // voice: MIDI 60 can be another file in a note-keyed bank, whose
            // shape was never decoded by the preview we just heard.
            Named::Bank(bank) => Some(pick_from_bank(bank, n, 36.0).0),
            Named::Font(_) => None,
        })??;
        let shape = self
            .shared
            .shapes
            .lock()
            .expect("sample shapes")
            .by_url
            .get(&url)
            .cloned()?;
        let duration = match self
            .shared
            .by_url
            .read()
            .expect("sample url table")
            .get(&url)
        {
            Some(UrlState::Ready { duration_secs, .. }) => *duration_secs,
            _ => return None,
        };
        Some((shape, duration))
    }

    /// Whether a `samples(...)` the score wrote - its string argument, as
    /// written - has been registered and published. A string argument
    /// reaches the library as the JSON text it stringifies to, so both
    /// spellings are tried.
    pub fn knows_samples_source(&self, spec: &str) -> bool {
        let tables = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        let sources = &tables.sources;
        sources.contains(spec)
            || serde_json::to_string(spec).is_ok_and(|quoted| sources.contains(&quoted))
    }

    /// Source status for the Studio catalogue worker's next snapshot.
    pub fn browser_source_states(&self) -> HashMap<String, SourceState> {
        self.shared
            .source_tables
            .lock()
            .expect("samples source tables")
            .states
            .iter()
            .map(|(map, standing)| (map.clone(), standing.state.clone()))
            .collect()
    }

    /// Where a `samples("…")` the score wrote stands - its string argument,
    /// as written - or None for one nobody has asked the library for.
    pub fn samples_source_state(&self, spec: &str) -> Option<SourceState> {
        let tables = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        let states = &tables.states;
        states
            .get(spec)
            .or_else(|| {
                serde_json::to_string(spec)
                    .ok()
                    .and_then(|quoted| states.get(&quoted))
            })
            .map(|standing| standing.state.clone())
    }

    /// Whether a `samples("…")` whose map failed may be asked for again:
    /// the failure has rested, and the loader has nothing else in hand, so
    /// a retry never waits in line behind another. An ask rests
    /// [`FAILED_RETRY_AFTER`]; a `background` one also waits out a rest that
    /// doubles with each failure in a row, up to [`FAILED_RETRY_LONGEST`].
    pub fn samples_source_rested(&self, spec: &str, background: bool) -> bool {
        let Ok(map) = serde_json::to_string(spec) else {
            return false;
        };
        self.manifests_pending() == 0
            && self
                .shared
                .source_tables
                .lock()
                .expect("samples source tables")
                .states
                .get(&map)
                .is_some_and(|standing| {
                    let rest = if background {
                        standing.rest()
                    } else {
                        FAILED_RETRY_AFTER
                    };
                    standing.failed() && standing.since.elapsed() >= rest
                })
    }

    /// Mark `map` as on its way. A failure keeps its reason until the job's
    /// verdict replaces it, so asking again does not lift the refusal the
    /// failure explains.
    fn mark_source_loading(&self, map: &str) {
        let mut tables = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        if !tables.states.get(map).is_some_and(Standing::failed) {
            tables.note(map, SourceState::Loading);
        }
    }

    /// Ask for a `samples("…")` a score names before the score is
    /// evaluated, as the checker sees it: its map is fetched under the same
    /// grant an evaluation would use, so its banks are in the browser to
    /// look at, and its names judged, while the score is still being
    /// typed. A spec nobody has asked for is asked, and one whose failure
    /// has rested is asked again ([`Self::samples_source_rested`]); one on
    /// its way or in is left as it stands. A refusal is recorded as the
    /// spec's failure, so a checker can show it.
    pub fn look_up_samples_source(
        &self,
        spec: &str,
        access: &ScoreSampleAccess,
    ) -> Result<(), String> {
        let map = serde_json::to_string(spec).map_err(|error| error.to_string())?;
        let failures = || {
            self.shared
                .source_tables
                .lock()
                .expect("samples source tables")
                .states
                .get(&map)
                .map(|standing| standing.failures)
        };
        let asked = failures();
        if asked.is_some() && !self.samples_source_rested(spec, false) {
            return Ok(());
        }
        self.register_batch_async_into_with_intent(
            BankLayer::Score,
            vec![(map.clone(), None)],
            &[],
            ManifestAccess::Score(access.clone()),
            PrefetchIntent::Explicit,
        )
        .map(|_| ())
        .inspect_err(|error| {
            // A job the line refused has already counted as the map's
            // failure; an ask turned away before that counts here.
            if failures() == asked {
                self.shared
                    .note_source_standing(&map, SourceState::Failed(error.clone()));
            }
        })
    }

    /// What the manifest worker notes for a score's `samples("…")` once its
    /// map is in - the argument as JSON.stringify writes it - for tests
    /// that cannot fetch.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn note_samples_source_for_tests(&self, spec: &str) {
        let map = serde_json::to_string(spec).expect("a string");
        self.shared.note_source_standing(&map, SourceState::Ready);
    }

    /// Where a `samples("…")` stands, for tests that cannot fetch.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn note_samples_source_state_for_tests(&self, spec: &str, state: SourceState) {
        let map = serde_json::to_string(spec).expect("a string");
        if state == SourceState::Ready {
            self.note_samples_source_for_tests(spec);
        } else {
            self.shared.note_source_standing(&map, state);
        }
    }

    /// Age a `samples("…")`'s standing by [`FAILED_RETRY_AFTER`], as if
    /// that long had passed.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn rest_samples_source_for_tests(&self, spec: &str) {
        let map = serde_json::to_string(spec).expect("a string");
        if let Some(standing) = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables")
            .states
            .get_mut(&map)
        {
            standing.since -= FAILED_RETRY_AFTER;
        }
    }

    /// Whether any bank has been registered yet. A library with none knows
    /// too little to say a sound does not exist.
    pub fn has_banks(&self) -> bool {
        !self.banks.read().expect("default banks").is_empty()
            || !self.custom.read().expect("custom banks").is_empty()
            || !self.global.read().expect("global banks").is_empty()
    }

    /// Files the loader still has in its line, both the ones a score is
    /// waiting on and the bets placed ahead of it.
    ///
    /// What a progress bar reads. It is a queue depth rather than a
    /// fraction: nothing here knows how many files a pre-cache will turn
    /// out to be until the manifests are in, and a bar that jumps backwards
    /// as it learns is worse than one that simply counts down.
    pub fn pending_loads(&self) -> usize {
        let queued = {
            let state = self.shared.jobs.state.lock().expect("sample load queue");
            state.now.len().saturating_add(state.bets.len())
        };
        let caching = self.shared.caching.lock().expect("disk-cache claims").len();
        queued.max(caching)
    }

    /// One of the files a loader has in hand right now - fetching or
    /// decoding - named as a person would: the last segment of its url,
    /// with the percent-escapes undone. `None` when every worker is idle.
    pub fn loading_now(&self) -> Option<String> {
        let loading = self.shared.loading.lock().expect("loading sample");
        loading.iter().next().map(|url| file_name_of_url(url))
    }

    /// Manifests still being fetched. While this is above zero a name that
    /// is not known yet may simply not have arrived.
    pub fn manifests_pending(&self) -> usize {
        self.shared.manifest_pending.load(Ordering::Acquire)
    }

    /// A counter that moves whenever a sound that was loading may now answer
    /// differently: a sample or soundfont finished (or failed), or a
    /// manifest arrived. Read it BEFORE asking for sounds; if it has not
    /// moved since, every "still loading" answer then is still the answer.
    pub fn settled_epoch(&self) -> u64 {
        self.shared.settled_epoch.load(Ordering::Acquire)
    }

    /// Whether `s("name")` can sound as written: a bank this library knows
    /// by its exact name, a General MIDI font, or a native synth. A name a
    /// bank reaches through `.bank()` is not answered here - that spelling
    /// resolves only with the bank on the sound, and the voice refuses a
    /// bare name that a bank's key merely ends with.
    pub fn knows_sound(&self, name: &str) -> bool {
        self.knows(name) || rustel_voice::is_native_synth_sound(name)
    }

    /// Whether `s("name")` can sound on a statement that plays through the
    /// given `.bank()` names: the banked spelling the voice builds
    /// (`{bank}_{name}`), or a native synth - whose dispatch reads the raw
    /// name the bank never touches. The plain spelling is not an answer
    /// here: the voice resolves the banked name and nothing else.
    ///
    /// A bank control is a pattern like any other: `.bank("A B")` alternates
    /// banks across the statement's haps. Every name in it must hold the
    /// sound. `ht` under `.bank("BossDR110 AkaiXR10")` fails on the BossDR110
    /// haps when only AkaiXR10 holds `ht`.
    pub fn knows_sound_under_banks(&self, name: &str, banks: &[String]) -> bool {
        rustel_voice::is_native_synth_sound(name)
            || banks
                .iter()
                .all(|bank| self.knows(&format!("{bank}_{name}")))
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

    fn look_up<T>(&self, name: &str, take: impl FnOnce(Named<'_>) -> T) -> Option<T> {
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

    /// Start loading, as bets, the files that the variants of each name
    /// select. For an ordinary bank these are the files that those `n` select.
    /// For a General MIDI name they are the fonts. For a note-keyed bank they
    /// are the same indices in its flattened file list. A name that can play
    /// any variant, because the text names no index, loads its first file
    /// only. The note about to play still resolves its own file. Returns the
    /// number of files requested, including files already loaded or in
    /// progress.
    ///
    /// A tab behind a pad is loaded this way before it plays, so its first
    /// press finds the variants that its text names.
    ///
    /// The loader takes bets newest first, so the first variant of each name,
    /// which is the one a bare name plays, is queued after the other variants
    /// of every name.
    pub fn load_variants_ahead<'a>(
        &self,
        sounds: impl IntoIterator<Item = (&'a str, &'a crate::sounds::Variants)>,
    ) -> usize {
        let found: Vec<(DecodeRate, Warm)> = sounds
            .into_iter()
            .filter(|(name, _)| !rustel_voice::is_native_synth_sound(name))
            .filter_map(|(name, variants)| {
                // Any index is not a request for every file. A bare name
                // plays the first, and that is the one loaded ahead.
                let variants = match variants {
                    crate::sounds::Variants::All => crate::sounds::Variants::first(),
                    only => only.clone(),
                };
                let found = self.look_up(name, |named| match named {
                    Named::Bank(Bank::Array(urls)) => {
                        Warm::Urls(picked(urls, &variants).cloned().collect())
                    }
                    Named::Bank(Bank::Notes(notes)) => {
                        let urls: Vec<Arc<str>> = notes
                            .iter()
                            .flat_map(|(_, urls)| urls.iter().cloned())
                            .collect();
                        Warm::Urls(picked(&urls, &variants).cloned().collect())
                    }
                    Named::Font(fonts) => Warm::Fonts(picked(fonts, &variants).cloned().collect()),
                })?;
                Some((DecodeRate::for_sound(name), found))
            })
            .collect();
        let mut asked = 0usize;
        // Every name's others first, then every name's first.
        for firsts in [false, true] {
            for (decode_rate, found) in &found {
                let (files, fonts) = match found {
                    Warm::Urls(urls) => (split_first(urls, firsts), &[][..]),
                    Warm::Pitched(urls) => (if firsts { &urls[..] } else { &[][..] }, &[][..]),
                    Warm::Fonts(fonts) => (&[][..], split_first(fonts, firsts)),
                };
                for url in in_queue_order(files, LoadPriority::Bet) {
                    let _ =
                        ensure_loading_shared(&self.shared, url, *decode_rate, LoadPriority::Bet);
                }
                for font in in_queue_order(fonts, LoadPriority::Bet) {
                    queue_font(&self.shared, font, LoadPriority::Bet);
                }
                asked += files.len() + fonts.len();
            }
        }
        asked
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

    /// Forget the decode of `ids` so a later ask fetches and decodes again.
    ///
    /// Nothing here holds PCM: the loader publishes each decode once,
    /// through [`Self::take_ready`], and afterwards these tables keep only
    /// the id and payload identity. A host reclaiming that PCM must say so, or
    /// `Ready` goes on promising a sample nobody can play - no loader ever
    /// publishes it a second time, so the sound previews as "did not load
    /// in time" and a score that names it plays silence until the process
    /// is restarted.
    ///
    /// A font is forgotten whole: its zones are one decode over one run of
    /// ids, and half of one plays nothing anybody asked for. Every id
    /// invalidated is returned, the untouched zones of a part-dropped font
    /// included, so the caller can retire those with the rest.
    ///
    /// The decode is what is forgotten, not the bytes: a re-ask reads the
    /// sample cache on disk and only reaches the network if it has been
    /// cleared.
    pub fn forget_decoded(&self, ids: &HashSet<SampleId>) -> Vec<SampleId> {
        if ids.is_empty() {
            return Vec::new();
        }
        let mut forgotten: HashSet<SampleId> = HashSet::new();
        {
            let mut fonts = self.shared.fonts.write().expect("font table");
            fonts.retain(|_, state| {
                let FontState::Ready(zones) = state else {
                    return true;
                };
                if !zones.iter().any(|zone| ids.contains(&zone.id)) {
                    return true;
                }
                forgotten.extend(zones.iter().map(|zone| zone.id));
                false
            });
        }
        {
            let mut by_url = self.shared.by_url.write().expect("sample url table");
            by_url.retain(|_, state| {
                let UrlState::Ready { id, .. } = state else {
                    return true;
                };
                if !ids.contains(id) {
                    return true;
                }
                forgotten.insert(*id);
                false
            });
        }
        {
            let mut ready = self.shared.ready.lock().expect("ready samples");
            for id in &forgotten {
                if let Some(slot) = ready.identities.get_mut(id.0 as usize) {
                    // A retained retry cannot resurrect this one. The id stays
                    // out of circulation until the studio hands it back through
                    // `release_ids`, once nothing can read it.
                    *slot = DecodedIdentity::Forgotten;
                }
            }
            ready.samples.retain(|(id, _)| !forgotten.contains(id));
        }
        forgotten.into_iter().collect()
    }

    /// Take back ids the caller can vouch nothing reads any more: forgotten
    /// here through [`Self::forget_decoded`], cleared from every bank they
    /// were installed in, and past the last frame any queued event or
    /// sounding voice could name them at. The next reservation takes them
    /// before it moves the counter, so a browsing session stops draining
    /// the bank one preview at a time. The tombstone `forget_decoded` left
    /// goes with them: a decode at a reissued id starts from an unknown
    /// identity, like a fresh slot. An id never forgotten - still promised
    /// by a table, or the bundled bd - is refused; a second release is a
    /// no-op.
    pub fn release_ids(&self, ids: impl IntoIterator<Item = SampleId>) {
        let mut ready = self.shared.ready.lock().expect("ready samples");
        let mut free = self.shared.free_ids.lock().expect("free sample ids");
        for id in ids {
            let Some(slot) = ready.identities.get_mut(id.0 as usize) else {
                continue;
            };
            if !matches!(*slot, DecodedIdentity::Forgotten) {
                continue;
            }
            *slot = DecodedIdentity::Unknown;
            free.push_back(id);
        }
    }

    /// How many ids are waiting to be reissued, for a test to watch.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn free_id_count(&self) -> usize {
        self.shared.free_ids.lock().expect("free sample ids").len()
    }

    /// Whether `.bank("name")` names a family of banks: something is called
    /// `name_…`.
    pub fn knows_bank(&self, name: &str) -> bool {
        let prefix = format!("{name}_");
        let starts_with =
            |names: &HashMap<String, Bank>| names.keys().any(|known| known.starts_with(&prefix));
        starts_with(&self.banks.read().expect("default banks"))
            || starts_with(&self.custom.read().expect("custom banks"))
            || starts_with(&self.global.read().expect("global banks"))
    }

    /// Convert decoded PCM to `rate` from here on.
    ///
    /// `decodeAudioData` resamples every file it decodes to the context's rate,
    /// so playback then only interpolates for `speed`. Owners call this with
    /// the render or device rate before a score can ask for a sound. Samples
    /// already decoded keep the rate they were converted to; playback derives
    /// its read increment from each buffer's own rate, so they stay in tune
    /// either way, and only the conversion filter is the older one.
    pub fn set_render_rate(&self, rate: u32) {
        if rate > 0 {
            self.shared.render_rate.store(rate, Ordering::Release);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn render_rate_for_test(&self) -> u32 {
        self.shared.render_rate.load(Ordering::Acquire)
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

    fn manifest_context(&self) -> ManifestContext {
        ManifestContext {
            banks: Arc::clone(&self.banks),
            custom: Arc::clone(&self.custom),
            global: Arc::clone(&self.global),
            gm: Arc::clone(&self.gm),
            shared: Arc::clone(&self.shared),
        }
    }

    /// Walk a folder batch on this library with a budget the test chose -
    /// the same call `run_manifest_worker` makes for `ManifestWork::Folders`,
    /// minus the queue. A test that needs the batch to arrive already spent
    /// cannot get there through the real one, which budgets its own.
    #[cfg(test)]
    fn run_folders_work_for_test(
        &self,
        specs: &[String],
        generation: u64,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        run_folders_work(&self.manifest_context(), specs, generation, budget)
    }

    /// Start loading the files a sound name asks for, before anything asks
    /// to play it.
    ///
    /// `"bd"` warms the first file, `"bd:3"` only that variant, and a bare
    /// `gm_*` name still warms every font it holds. A live set cannot wait for
    /// the network at the moment of the first hit - the artist hears the
    /// gap as a dropped beat, which is exactly what `preload` exists to
    /// prevent.
    ///
    /// This method consults the maps published when it is called and returns
    /// the exact number of files it started (already-loaded ones included,
    /// since asking again is free). It does not retain an unknown name behind
    /// an asynchronous Session registration; Session's internal path carries
    /// those preloads in the owning manifest job instead.
    pub fn prefetch(&self, spec: &str) -> usize {
        self.prefetch_with(spec, LoadPriority::Now)
    }

    /// Warm a sound at a chosen place in the loader's line. A pre-cache is
    /// a bet: it goes behind whatever a score is about to play.
    pub fn prefetch_with(&self, spec: &str, priority: LoadPriority) -> usize {
        match self.manifest_context().prefetch_known(spec, priority) {
            PrefetchStatus::Requested(files) => files,
            PrefetchStatus::Deferred | PrefetchStatus::Unknown => 0,
        }
    }

    /// Fetch every file a sound name can produce onto disk, without seating
    /// it in the live sample bank.
    ///
    /// `Advanced ▸ cache whole library` is this: thousands of files, a
    /// gigabyte, and none of them need to occupy a decoded slot until a
    /// note actually asks. Prefetching them the ordinary way filled the
    /// 2048-slot bank and then refused the rest as capacity exceeded.
    pub fn cache_to_disk(&self, spec: &str) -> usize {
        match self.manifest_context().prefetch_known_with_intent(
            spec,
            LoadPriority::Bet,
            PrefetchIntent::CacheDisk,
        ) {
            PrefetchStatus::Requested(files) => files,
            PrefetchStatus::Deferred | PrefetchStatus::Unknown => 0,
        }
    }

    /// The packs the studio ships with, in the order the pin file lists
    /// them: each pinned manifest that brings files, the inline banks, and
    /// the General MIDI soundfonts last.
    ///
    /// A pin that only maps aliases onto another pack's banks brings no
    /// files of its own, so it is not a row: there would be nothing to
    /// count and nothing to cache under it.
    pub fn default_sources() -> &'static [DefaultSource] {
        static SOURCES: std::sync::OnceLock<Vec<DefaultSource>> = std::sync::OnceLock::new();
        SOURCES.get_or_init(|| default_sources_from(PINNED_BANKS, PINNED_GM_FONTS))
    }

    /// What a shipped pack holds right now: how many sounds this library
    /// knows under it, and how many distinct files they name. Zero of
    /// both until its manifest has landed.
    pub fn default_source_holds(&self, source: &DefaultSource) -> (usize, usize) {
        let (sounds, files) = self.default_source_files(source);
        (sounds, files.len())
    }

    /// Fetch every file of one shipped pack onto disk, skipping what is
    /// there already. `Enter` on its row, and one pass of `c` for the
    /// whole library: what the row then counts down is
    /// [`Self::pending_cache_under`] its base.
    pub fn cache_default_source(&self, source: &DefaultSource) -> CacheRequest {
        if self.is_font_source(source) {
            let fonts = self.default_font_files();
            let queued = fonts
                .iter()
                .filter(|font| queue_font_cache(&self.shared, font))
                .count();
            return CacheRequest {
                sounds: self.gm.len(),
                files: fonts.len(),
                queued,
            };
        }
        let (sounds, files) = self.default_source_files(source);
        let queued = files
            .iter()
            .filter(|url| ensure_cached_shared(&self.shared, url))
            .count();
        CacheRequest {
            sounds,
            files: files.len(),
            queued,
        }
    }

    /// Disk-cache fetches still in the line for files under `base`: the
    /// number a pack's row counts down while the library is being cached.
    ///
    /// Only claims that still have a job, or a file in a loader's hand: a
    /// claim left behind with no job would pin the last file of a pack on
    /// the row forever.
    pub fn pending_cache_under(&self, base: &str) -> usize {
        let prefix = source_prefix(base);
        let mut caching = self.shared.caching.lock().expect("disk-cache claims");
        let state = self.shared.jobs.state.lock().expect("sample load queue");
        let loading = self.shared.loading.lock().expect("loading sample");
        let live = |url: &str| {
            state.now.iter().any(|job| &*job.url == url)
                || state.bets.iter().any(|job| &*job.url == url)
                || loading.iter().any(|hand| hand.as_ref() == url)
        };
        // Drop orphan claims so a later ask can try again, and so the row
        // reaches "done" instead of counting ghosts.
        caching.retain(|url| !url.starts_with(&*prefix) || live(url));
        caching
            .iter()
            .filter(|url| url.starts_with(&*prefix))
            .count()
    }

    /// How much of a shipped pack is already on disk: files held, files
    /// named, and the bytes those held files take in the host cache.
    pub fn default_source_cached(&self, source: &DefaultSource) -> (usize, usize, u64) {
        if self.is_font_source(source) {
            let fonts = self.default_font_files();
            let total = fonts.len();
            let mut held = 0usize;
            let mut bytes = 0u64;
            for font in fonts {
                let url = font_file_url(&self.shared.font_base, &font);
                if let Some(len) = cache_entry_bytes(&self.shared.host_cache, &url) {
                    held += 1;
                    bytes = bytes.saturating_add(len);
                }
            }
            return (held, total, bytes);
        }
        let (_, files) = self.default_source_files(source);
        let total = files.len();
        let mut held = 0usize;
        let mut bytes = 0u64;
        for url in files {
            if let Some(len) = cache_entry_bytes(&self.shared.host_cache, &url) {
                held += 1;
                bytes = bytes.saturating_add(len);
            }
        }
        (held, total, bytes)
    }

    /// Whether a file in a loader's hand belongs to the pack at `base`.
    pub fn loading_under(&self, base: &str) -> Option<String> {
        let prefix = source_prefix(base);
        let loading = self.shared.loading.lock().expect("loading sample");
        loading
            .iter()
            .find(|url| url.starts_with(&*prefix))
            .map(|url| file_name_of_url(url))
    }

    fn is_font_source(&self, source: &DefaultSource) -> bool {
        !self.shared.font_base.is_empty()
            && source_prefix(&source.base) == source_prefix(&self.shared.font_base)
    }

    /// The sounds under a shipped pack and the distinct files they name.
    /// Distinct, because the case-insensitive and alias copies of a bank
    /// point at the same files, and a count that took each copy as its own
    /// download would have a bar reach the end at half way.
    pub fn default_source_files(&self, source: &DefaultSource) -> (usize, HashSet<Arc<str>>) {
        if self.is_font_source(source) {
            let files = self
                .default_font_files()
                .into_iter()
                .map(|font| Arc::from(font_file_url(&self.shared.font_base, &font)))
                .collect();
            return (self.gm.len(), files);
        }
        let prefix = source_prefix(&source.base);
        let mut files = HashSet::new();
        let mut sounds = 0;
        let banks = self.banks.read().expect("default banks");
        for (name, bank) in banks.iter() {
            let urls = bank_file_urls(bank);
            if !urls.first().is_some_and(|url| url.starts_with(&*prefix)) {
                continue;
            }
            let lowercase_lookup_copy = name == &name.to_lowercase()
                && banks.iter().any(|(other_name, other_bank)| {
                    other_name != name
                        && other_name.to_lowercase() == *name
                        && bank_file_urls(other_bank) == urls
                });
            if !lowercase_lookup_copy {
                sounds += 1;
            }
            files.extend(urls.into_iter().cloned());
        }
        if sounds == 0
            && let Some((pinned_sounds, pinned_files)) = pinned_inline_files(source)
        {
            return (pinned_sounds, pinned_files);
        }
        (sounds, files)
    }

    /// What a user import holds right now: sounds this library knows under
    /// it, and how many distinct files they name. Zero of both until its
    /// list has landed.
    pub fn import_source_holds(&self, spec: &str) -> (usize, usize) {
        let (sounds, files) = self.import_source_files(spec);
        (sounds, files.len())
    }

    /// Fetch every remote file of one user import onto disk, skipping what
    /// is there already. Local folders queue nothing: they are the cache.
    pub fn cache_import_source(&self, spec: &str) -> CacheRequest {
        if source_is_local(spec) {
            let (sounds, files) = self.import_source_files(spec);
            return CacheRequest {
                sounds,
                files: files.len(),
                queued: 0,
            };
        }
        let (sounds, files) = self.import_source_files(spec);
        let queued = files
            .iter()
            .filter(|url| ensure_cached_shared(&self.shared, url))
            .count();
        CacheRequest {
            sounds,
            files: files.len(),
            queued,
        }
    }

    /// How much of a user import is already on disk: files held, files
    /// named, and the bytes those held files take in the host cache.
    /// Local folders report every file as held, sized from the folder.
    pub fn import_source_cached(&self, spec: &str) -> (usize, usize, u64) {
        let (_, files) = self.import_source_files(spec);
        let total = files.len();
        let mut held = 0usize;
        let mut bytes = 0u64;
        for url in files {
            if let Some(len) = cache_entry_bytes(&self.shared.host_cache, &url) {
                held += 1;
                bytes = bytes.saturating_add(len);
            }
        }
        (held, total, bytes)
    }

    /// Disk-cache fetches still in the line for this exact set of files.
    /// User packs do not share a CDN prefix the way shipped packs do, so
    /// the row counts the URLs it named rather than a base string.
    pub fn pending_cache_of(&self, files: &HashSet<Arc<str>>) -> usize {
        let mut caching = self.shared.caching.lock().expect("disk-cache claims");
        let state = self.shared.jobs.state.lock().expect("sample load queue");
        let loading = self.shared.loading.lock().expect("loading sample");
        let live = |url: &str| {
            state.now.iter().any(|job| &*job.url == url)
                || state.bets.iter().any(|job| &*job.url == url)
                || loading.iter().any(|hand| hand.as_ref() == url)
        };
        caching.retain(|url| !files.contains(url) || live(url));
        caching.iter().filter(|url| files.contains(*url)).count()
    }

    /// Disk-cache fetches still in the line for a user import, counted
    /// from the files that pack currently names.
    pub fn pending_cache_for_import(&self, spec: &str) -> usize {
        let (_, files) = self.import_source_files(spec);
        self.pending_cache_of(&files)
    }

    /// Whether a file in a loader's hand belongs to this set of URLs.
    pub fn loading_among(&self, files: &HashSet<Arc<str>>) -> Option<String> {
        let loading = self.shared.loading.lock().expect("loading sample");
        loading
            .iter()
            .find(|url| files.contains(*url))
            .map(|url| file_name_of_url(url))
    }

    /// The file in hand for a user import, when the loader has one.
    pub fn loading_for_import(&self, spec: &str) -> Option<String> {
        let (_, files) = self.import_source_files(spec);
        self.loading_among(&files)
    }

    /// True when `name` is a bank a score or Settings import brought -
    /// not a shipped default, a font, or a synth. Fetch imports caches
    /// these when the score names them; `c` on a pack caches the rest.
    pub fn is_imported_sound(&self, name: &str) -> bool {
        self.custom.read().expect("custom banks").contains_key(name)
            || self.global.read().expect("global banks").contains_key(name)
    }

    /// The sounds under a user import and the distinct files they name.
    pub fn import_source_files(&self, spec: &str) -> (usize, HashSet<Arc<str>>) {
        let spec = spec.trim();
        let slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        let Some(slot) = slots.iter().find(|slot| slot.spec == spec) else {
            return (0, HashSet::new());
        };
        let mut files = HashSet::new();
        let mut sounds = 0;
        for bank in slot.banks.values() {
            let urls = bank_file_urls(bank);
            if urls.is_empty() {
                continue;
            }
            sounds += 1;
            files.extend(urls.into_iter().cloned());
        }
        (sounds, files)
    }

    /// Every soundfont file the General MIDI names reach, once each: the
    /// fonts share files across instruments.
    fn default_font_files(&self) -> HashSet<Arc<str>> {
        self.gm.values().flatten().cloned().collect()
    }

    /// Block until every url this library has started loading is settled or
    /// the deadline passes. Prefetch/offline convenience; the live path
    /// never waits.
    pub fn wait_until_idle(&self, deadline: std::time::Duration) {
        self.wait_until_idle_cancellable(deadline, None);
    }

    /// The offline wait with an optional host stop flag. Stopping this wait
    /// leaves the library and its background loaders available to the host.
    pub(crate) fn wait_until_idle_cancellable(
        &self,
        deadline: std::time::Duration,
        cancelled: Option<&AtomicBool>,
    ) {
        let started = std::time::Instant::now();
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return;
            }
            // A manifest job starts its owned preloads before release-publishing
            // zero. Reading that counter first means an observed zero happens
            // before these table reads; a load cannot appear between an old
            // table snapshot and a new zero and be mistaken for idle.
            let pending = self.shared.manifest_pending.load(Ordering::Acquire) != 0
                || {
                    let by_url = self.shared.by_url.read().expect("sample url table");
                    by_url
                        .values()
                        .any(|state| matches!(state, UrlState::Loading))
                }
                || {
                    let fonts = self.shared.fonts.read().expect("font table");
                    fonts
                        .values()
                        .any(|state| matches!(state, FontState::Loading))
                }
                || {
                    !self
                        .shared
                        .caching
                        .lock()
                        .expect("disk-cache claims")
                        .is_empty()
                };
            if !pending || started.elapsed() >= deadline {
                return;
            }
            #[cfg(test)]
            wait_tests::entering_wait();
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

/// The last segment of a url, as a person would name the file: what comes
/// after the final `/`, before any `?` or `#`, with percent-escapes undone.
fn file_name_of_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut out = Vec::with_capacity(name.len());
    let mut rest = name.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%'
            && let [hi, lo, ..] = tail
            && let (Some(hi), Some(lo)) =
                (char::from(*hi).to_digit(16), char::from(*lo).to_digit(16))
        {
            out.push((hi * 16 + lo) as u8);
            rest = &tail[2..];
        } else {
            out.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| name.to_owned())
}

#[cfg(test)]
mod loading_name_tests {
    use super::file_name_of_url;

    #[test]
    fn the_file_in_hand_is_named_by_its_last_segment() {
        assert_eq!(
            file_name_of_url("https://example.org/packs/drums/kick%20one.wav?raw=1"),
            "kick one.wav"
        );
        assert_eq!(
            file_name_of_url("file:///tmp/my%20drums/snare.wav"),
            "snare.wav"
        );
        assert_eq!(file_name_of_url("snare.wav"), "snare.wav");
        assert_eq!(file_name_of_url("bad%zz.wav"), "bad%zz.wav");
        assert_eq!(file_name_of_url("dir/"), "");
    }
}

/// Queue an unloaded font once, or retry a rested failure for a real ask.
/// A speculative request that is now needed for playback moves up the line;
/// bets never spend retries and fresh failures retain their cooldown.
fn queue_font(shared: &Shared, font: &Arc<str>, priority: LoadPriority) {
    let mut table = shared.fonts.write().expect("font table");
    let ask = match table.get(font) {
        None => true,
        Some(FontState::Failed { at }) => {
            priority == LoadPriority::Now && at.elapsed() >= FAILED_RETRY_AFTER
        }
        Some(FontState::Loading | FontState::Ready(_)) => false,
    };
    if ask {
        table.insert(font.clone(), FontState::Loading);
        shared.font_jobs.push(font.clone(), priority);
    } else if priority == LoadPriority::Now && matches!(table.get(font), Some(FontState::Loading)) {
        shared.font_jobs.promote(font);
    }
}

/// Start `url` loading unless it is already on its way, in, or freshly
/// failed. `priority` says where it joins the line, and whether a failure
/// is worth another try: a real ask after the rest is; a bet is not.
fn ensure_loading_shared(
    shared: &Shared,
    url: &Arc<str>,
    decode_rate: DecodeRate,
    priority: LoadPriority,
) -> SampleResolution {
    let settled = |state: Option<&UrlState>| match state {
        Some(UrlState::Ready { id, duration_secs }) => Some(SampleResolution::Found {
            id: *id,
            transpose: 0.0,
            duration_secs: *duration_secs,
            loop_secs: None,
            envelope_peak: 1.0,
            soundfont: false,
        }),
        Some(UrlState::Loading) => Some(SampleResolution::Loading),
        Some(UrlState::Failed { at })
            if priority == LoadPriority::Bet || at.elapsed() < FAILED_RETRY_AFTER =>
        {
            Some(SampleResolution::Failed)
        }
        Some(UrlState::Failed { .. }) | None => None,
    };
    {
        let by_url = shared.by_url.read().expect("sample url table");
        if let Some(resolution) = settled(by_url.get(url)) {
            // Bet on earlier, needed now: it moves up the line.
            if priority == LoadPriority::Now && matches!(resolution, SampleResolution::Loading) {
                shared.jobs.promote(url);
            }
            return resolution;
        }
    }
    let mut by_url = shared.by_url.write().expect("sample url table");
    if let Some(resolution) = settled(by_url.get(url)) {
        return resolution;
    }
    let ids = match reserve_sample_ids(shared, 1) {
        Ok(ids) => ids,
        Err(error) => {
            // A full bank is a failure like any other: it is reported once,
            // then rests, and the next real ask after the rest tries again.
            // By then the studio may have released a preview's id.
            by_url.insert(url.clone(), UrlState::Failed { at: Instant::now() });
            drop(by_url);
            shared
                .failures
                .lock()
                .expect("sample failures")
                .push(format!("{url}: {error}").into());
            return SampleResolution::Failed;
        }
    };
    let id = ids[0];
    by_url.insert(url.clone(), UrlState::Loading);
    drop(by_url);
    shared.jobs.push(
        LoadJob {
            url: url.clone(),
            kind: LoadKind::Install { id, decode_rate },
        },
        priority,
    );
    SampleResolution::Loading
}

/// Whether this URL already has a host-cache file, or is a local path that
/// is its own cache.
///
/// An empty file does not count: a failed or interrupted publish can leave
/// a zero-byte entry that would otherwise poison every later ask into
/// "already here", and a pack's row would never finish the last file.
fn host_cache_holds(dir: &Path, url: &str) -> bool {
    cache_entry_bytes(dir, url).is_some_and(|bytes| bytes > 0)
}

/// How many bytes a host-cache entry holds for `url`, when there is a
/// non-empty regular file. Local `file://` paths are already on disk and
/// report their own length.
fn cache_entry_bytes(dir: &Path, url: &str) -> Option<u64> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::metadata(path).ok().map(|meta| meta.len());
    }
    let file = open_regular_cache_entry(&cache_path(dir, url))
        .ok()
        .flatten()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        // A zero-byte publish is not a sample: drop it so the next ask
        // fetches for real rather than treating emptiness as success.
        evict_host_cache_entry(dir, url);
        return None;
    }
    Some(len)
}

/// One soundfont-loader worker: a refused font does not stop later jobs.
fn run_font_loader(
    font_shared: Weak<Shared>,
    font_jobs: Arc<FontQueue>,
    font_dir: PathBuf,
    font_base: String,
) {
    while let Some(font) = font_jobs.pop() {
        let Some(font_shared) = font_shared.upgrade() else {
            return;
        };
        let url = format!("{font_base}/{font}.js");
        let budget = sample_fetch::FetchBudget::for_one_fetch_with_cancellation(
            font_shared.publication.cancellation(),
        );
        let outcome = load_font(&font_dir, &url, &font_shared, &budget);
        let mut fonts = font_shared.fonts.write().expect("font table");
        match outcome {
            Ok(zones) => {
                fonts.insert(font.clone(), FontState::Ready(Arc::new(zones)));
                drop(fonts);
                font_shared.note_settled();
            }
            Err(error) => {
                fonts.insert(font.clone(), FontState::Failed { at: Instant::now() });
                drop(fonts);
                font_shared.note_settled();
                font_shared
                    .failures
                    .lock()
                    .expect("sample failures")
                    .push(format!("{url}: {error}").into());
            }
        }
    }
}

/// One sample-loader worker: pops jobs until the library closes the line.
fn run_sample_loader(shared: Weak<Shared>, jobs: Arc<LoadQueue>, dir: PathBuf) {
    while let Some(LoadJob { url, kind }) = jobs.pop() {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        shared
            .loading
            .lock()
            .expect("loading sample")
            .insert(Arc::clone(&url));
        let budget = sample_fetch::FetchBudget::for_one_fetch_with_cancellation(
            shared.publication.cancellation(),
        );
        let LoadKind::Install { id, decode_rate } = kind else {
            let outcome = fetch_cached_with_budget(&dir, &url, &budget, &shared.publication);
            shared
                .caching
                .lock()
                .expect("disk-cache claims")
                .remove(&url);
            if let Err(error) = outcome {
                shared
                    .failures
                    .lock()
                    .expect("sample failures")
                    .push(format!("{url}: {error}").into());
            }
            shared.loading.lock().expect("loading sample").remove(&url);
            continue;
        };
        let score_access = shared
            .score_sources
            .read()
            .expect("score sample sources")
            .get(&url)
            .cloned();
        let context_rate = match decode_rate {
            DecodeRate::Context => Some(shared.render_rate.load(Ordering::Acquire)),
            DecodeRate::Native => None,
        };
        let outcome = match score_access {
            // Decoding is the validation: the score cache commits only bytes
            // that decode, and the host cache evicts bytes that do not, so
            // neither serves an undecodable entry again.
            Some(access) => fetch_score_source_cached_with_budget(
                &shared.score_cache,
                &url,
                &access,
                ScoreCacheKind::Audio,
                rustel_audio::sample_pcm_ceiling(),
                CacheFetchScope {
                    budget: &budget,
                    publication: &shared.publication,
                },
                |bytes| decode_guarded(&url, bytes, context_rate),
            ),
            None => fetch_cached_decoded_with_budget(
                &dir,
                &url,
                &budget,
                &shared.publication,
                |bytes| decode_guarded(&url, bytes, context_rate),
            ),
        };
        match outcome {
            Ok(decoded) => {
                let duration = decoded.frames() as f64 / f64::from(decoded.sample_rate());
                // The only moment the PCM is in hand: from here the body
                // goes to the device and is not kept.
                shared
                    .shapes
                    .lock()
                    .expect("sample shapes")
                    .remember(url.clone(), shape_of(&decoded));
                // Queue the PCM first: `wait_until_idle` treats a Ready url
                // as settled, so publish Ready only after the decoded
                // sample reaches the install queue.
                shared
                    .ready
                    .lock()
                    .expect("ready samples")
                    .push((id, decoded));
                shared.by_url.write().expect("sample url table").insert(
                    url.clone(),
                    UrlState::Ready {
                        id,
                        duration_secs: duration,
                    },
                );
                shared.note_settled();
            }
            Err(error) => {
                let mut by_url = shared.by_url.write().expect("sample url table");
                by_url.insert(url.clone(), UrlState::Failed { at: Instant::now() });
                drop(by_url);
                shared.note_settled();
                // Nothing ever saw this id: the table said Loading
                // throughout. The retry after the rest reserves afresh.
                release_unpublished_id(&shared, id);
                shared
                    .failures
                    .lock()
                    .expect("sample failures")
                    .push(format!("{url}: {error}").into());
            }
        }
        shared.loading.lock().expect("loading sample").remove(&url);
    }
}

/// Queue a fetch that writes the host cache and then forgets the bytes.
/// No live-bank id, no decode, no Ready row: a later play loads from disk.
///
/// `true` when a fetch was put in the line; `false` when the file is on
/// disk already, being loaded for a note, or claimed by an earlier pass -
/// which is what lets a pack's row say how much of it was already here.
fn ensure_cached_shared(shared: &Shared, url: &Arc<str>) -> bool {
    if host_cache_holds(&shared.host_cache, url) {
        return false;
    }
    {
        let by_url = shared.by_url.read().expect("sample url table");
        if matches!(
            by_url.get(url),
            Some(UrlState::Loading | UrlState::Ready { .. })
        ) {
            return false;
        }
    }
    {
        let mut caching = shared.caching.lock().expect("disk-cache claims");
        if !caching.insert(Arc::clone(url)) {
            return false;
        }
    }
    // Claim first, then push. If the queue is closed because the library is
    // shutting down, drop the claim, so a pack's row does not count a fetch
    // that never runs.
    if !shared.jobs.push(
        LoadJob {
            url: Arc::clone(url),
            kind: LoadKind::Cache,
        },
        LoadPriority::Bet,
    ) {
        shared
            .caching
            .lock()
            .expect("disk-cache claims")
            .remove(url);
        return false;
    }
    true
}

/// Cache a soundfont file the way [`ensure_cached_shared`] caches a bank
/// sample: the `.js` lands on disk, and none of its zones take a bank slot.
fn queue_font_cache(shared: &Shared, font: &Arc<str>) -> bool {
    if shared.font_base.is_empty() {
        return false;
    }
    {
        let table = shared.fonts.read().expect("font table");
        if matches!(
            table.get(font),
            Some(FontState::Loading | FontState::Ready(_))
        ) {
            return false;
        }
    }
    let url: Arc<str> = Arc::from(font_file_url(&shared.font_base, font));
    ensure_cached_shared(shared, &url)
}

/// The file a General MIDI font is fetched from: `{base}/{font}.js`.
fn font_file_url(font_base: &str, font: &str) -> String {
    format!("{font_base}/{font}.js")
}

/// Refuse the first URL of a bank that the fetch boundary,
/// [`sample_fetch::remote_url_allowed`], refuses, with the boundary's own
/// reason word for word, so the refusal says what was wrong.
/// Locally-scanned folders never pass through here; they are registered
/// directly with `file://` URLs under the operator's own choice of root.
fn check_bank_urls(bank: &Bank) -> Result<(), String> {
    match bank {
        Bank::Array(urls) => urls
            .iter()
            .try_for_each(|url| sample_fetch::remote_url_allowed(url)),
        Bank::Notes(notes) => notes
            .iter()
            .flat_map(|(_, urls)| urls)
            .try_for_each(|url| sample_fetch::remote_url_allowed(url)),
    }
}

fn parse_bank(entry: &serde_json::Value, base: &str) -> Option<Bank> {
    match entry {
        serde_json::Value::String(file) => Some(Bank::Array(vec![Arc::from(join_url(base, file))])),
        serde_json::Value::Array(files) => {
            let urls: Vec<Arc<str>> = files
                .iter()
                .filter_map(|file| file.as_str())
                .map(|file| Arc::from(join_url(base, file)))
                .collect();
            (!urls.is_empty()).then_some(Bank::Array(urls))
        }
        serde_json::Value::Object(keys) => {
            let mut notes = Vec::new();
            for (key, files) in keys {
                if key.starts_with('_') {
                    continue;
                }
                let midi = rustel_core::util::note_to_midi(key, 3).ok()?;
                let urls: Vec<Arc<str>> = match files {
                    serde_json::Value::String(file) => vec![Arc::from(join_url(base, file))],
                    serde_json::Value::Array(files) => files
                        .iter()
                        .filter_map(|file| file.as_str())
                        .map(|file| Arc::from(join_url(base, file)))
                        .collect(),
                    _ => return None,
                };
                if midi.is_nan() || urls.is_empty() {
                    return None;
                }
                notes.push((midi, urls));
            }
            (!notes.is_empty()).then_some(Bank::Notes(notes))
        }
        _ => None,
    }
}

fn approve_score_url(
    base: &str,
    file: &str,
    access: &ScoreSampleAccess,
    sources: &mut Vec<(Arc<str>, ScoreFetchAccess)>,
) -> Result<Arc<str>, String> {
    if sources.len() >= MAX_SCORE_SAMPLE_FILES {
        return Err(format!(
            "samples() map exceeds the {MAX_SCORE_SAMPLE_FILES}-file limit"
        ));
    }
    sources
        .try_reserve(1)
        .map_err(|_| "not enough host memory for sample sources".to_owned())?;
    let (url, source_access) = access.approve_remote(&join_url(base, file))?;
    sources.push((url.clone(), source_access));
    Ok(url)
}

fn parse_score_bank(
    entry: &serde_json::Value,
    base: &str,
    access: &ScoreSampleAccess,
    sources: &mut Vec<(Arc<str>, ScoreFetchAccess)>,
) -> Result<Option<Bank>, String> {
    match entry {
        serde_json::Value::String(file) => Ok(Some(Bank::Array(vec![approve_score_url(
            base, file, access, sources,
        )?]))),
        serde_json::Value::Array(files) => {
            let count = files.iter().filter(|file| file.is_string()).count();
            if sources.len().saturating_add(count) > MAX_SCORE_SAMPLE_FILES {
                return Err(format!(
                    "samples() map exceeds the {MAX_SCORE_SAMPLE_FILES}-file limit"
                ));
            }
            let mut urls = Vec::new();
            urls.try_reserve(count)
                .map_err(|_| "not enough host memory for sample URLs".to_owned())?;
            for file in files.iter().filter_map(serde_json::Value::as_str) {
                urls.push(approve_score_url(base, file, access, sources)?);
            }
            Ok((!urls.is_empty()).then_some(Bank::Array(urls)))
        }
        serde_json::Value::Object(keys) => {
            if keys.len() > SAMPLE_BANK_CAPACITY - 1 {
                return Err(format!(
                    "samples() note map exceeds the {}-entry limit",
                    SAMPLE_BANK_CAPACITY - 1
                ));
            }
            let mut notes = Vec::new();
            notes
                .try_reserve(keys.len())
                .map_err(|_| "not enough host memory for sample notes".to_owned())?;
            for (key, files) in keys {
                if key.starts_with('_') {
                    continue;
                }
                let Some(midi) = rustel_core::util::note_to_midi(key, 3).ok() else {
                    return Ok(None);
                };
                let mut urls = Vec::new();
                match files {
                    serde_json::Value::String(file) => {
                        urls.push(approve_score_url(base, file, access, sources)?);
                    }
                    serde_json::Value::Array(files) => {
                        let count = files.iter().filter(|file| file.is_string()).count();
                        if sources.len().saturating_add(count) > MAX_SCORE_SAMPLE_FILES {
                            return Err(format!(
                                "samples() map exceeds the {MAX_SCORE_SAMPLE_FILES}-file limit"
                            ));
                        }
                        urls.try_reserve(count)
                            .map_err(|_| "not enough host memory for sample URLs".to_owned())?;
                        for file in files.iter().filter_map(serde_json::Value::as_str) {
                            urls.push(approve_score_url(base, file, access, sources)?);
                        }
                    }
                    _ => return Ok(None),
                }
                if midi.is_nan() || urls.is_empty() {
                    return Ok(None);
                }
                notes.push((midi, urls));
            }
            Ok((!notes.is_empty()).then_some(Bank::Notes(notes)))
        }
        _ => Ok(None),
    }
}

/// `getSoundIndex(n, numSounds)` - round, NaN falls back to 0, euclid wrap.
/// An empty list wraps to 0 rather than dividing by zero; a caller that
/// indexes with the result has to check for emptiness itself.
pub(crate) fn sound_index(n: f64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = if n.is_nan() { 0.0 } else { n };
    let rounded = crate::samples::js_round(n) as i64;
    rounded.rem_euclid(len as i64) as usize
}

/// `Math.round`: floor(x + 0.5), including negative halves.
pub(crate) fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Every registered name also answers to itself lowercased.
///
/// A bank name in the catalogue has mixed case, such as `RolandMC303`, and a
/// score can write it in another case: `.bank('Rolandmc303')` names the same
/// bank. Aliases are registered lowercased and match in any case. This
/// function gives the canonical names the same behaviour.
///
/// The lowercase key is registered, so the exact lookup stays one hash of
/// the given name. Existing keys are never overwritten, so of two banks
/// that differ only by case, the first registered keeps the key.
fn expand_case_insensitive(banks: &mut HashMap<String, Bank>) {
    let lowered: Vec<(String, Bank)> = banks
        .iter()
        .filter(|(key, _)| key.chars().any(char::is_uppercase))
        .map(|(key, bank)| (key.to_lowercase(), Bank::clone(bank)))
        .collect();
    for (key, bank) in lowered {
        banks.entry(key).or_insert(bank);
    }
}

/// For every registered `Bank_suffix` sound whose bank part has an alias,
/// register the lowercased `alias_suffix` for the same bank. Alias
/// canonicals match case-insensitively; existing keys are never overwritten.
fn expand_bank_aliases(banks: &mut HashMap<String, Bank>, aliases: &[(String, String)]) {
    let mut alias_lookup: HashMap<String, Vec<&str>> = HashMap::new();
    for (canonical, alias) in aliases {
        alias_lookup
            .entry(canonical.to_lowercase())
            .or_default()
            .push(alias.as_str());
    }
    let expanded: Vec<(String, Bank)> = banks
        .iter()
        .filter_map(|(key, bank)| {
            let (prefix, suffix) = key.split_once('_')?;
            let alias_names = alias_lookup.get(&prefix.to_lowercase())?;
            Some(
                alias_names
                    .iter()
                    .map(|alias| {
                        (
                            format!("{alias}_{suffix}").to_lowercase(),
                            Bank::clone(bank),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    for (key, bank) in expanded {
        banks.entry(key).or_insert(bank);
    }
}

#[cfg(test)]
mod score_source_boundary_tests {
    use super::*;

    #[test]
    fn score_and_trusted_routes_share_one_wire_identity() {
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://example.com")
            .expect("grant origin");
        let raw = "HTTPS://EXAMPLE.com:443/kit/take#2.wav?q=//";
        let (approved, approved_access) = access.approve_remote(raw).expect("approve score URL");

        assert_eq!(
            approved.as_ref(),
            sample_fetch::wire_url(raw).unwrap().as_str()
        );
        assert_eq!(
            approved_access,
            ScoreFetchAccess::Remote {
                origin: "https://example.com".to_owned(),
                cors_required: false,
            }
        );
        assert_eq!(
            cache_path(Path::new("/cache"), approved.as_ref()),
            cache_path(Path::new("/cache"), raw),
            "authorization and cache identity diverged from the request"
        );
    }

    /// The default policy matches strudel.cc: public https with the server's
    /// CORS consent. Loopback names and plain http need an exact grant, and
    /// an exact grant removes the consent requirement.
    #[test]
    fn the_default_policy_approves_public_https_and_nothing_nearer() {
        let denied = ScoreSampleAccess::denied();
        assert!(denied.is_denied());
        assert!(
            denied
                .approve_remote("https://samples.example/kit.json")
                .is_err()
        );

        let mut access = ScoreSampleAccess::denied();
        access.permit_public_cors_origins();
        assert!(!access.is_denied(), "the parity default is not a denial");
        let (_, open) = access
            .approve_remote("https://samples.example/kit.json")
            .expect("public https is approved by default");
        assert_eq!(
            open,
            ScoreFetchAccess::Remote {
                origin: "https://samples.example".to_owned(),
                cors_required: true,
            }
        );
        for nearer in [
            "http://samples.example/kit.json",
            "https://localhost:9000/kit.json",
            "https://kit.localhost/kit.json",
            "https://127.0.0.1:9000/kit.json",
            "https://[::1]:9000/kit.json",
        ] {
            let refusal = access.approve_remote(nearer).expect_err(nearer);
            assert!(
                refusal.contains("--allow-sample-origin"),
                "{nearer}: the refusal must name the grant that fixes it: {refusal}"
            );
        }

        access
            .permit_origin("http://localhost:9000")
            .expect("grant loopback origin");
        let (_, granted) = access
            .approve_remote("http://localhost:9000/kit.json")
            .expect("an exact grant reaches what the default cannot");
        assert_eq!(
            granted,
            ScoreFetchAccess::Remote {
                origin: "http://localhost:9000".to_owned(),
                cors_required: false,
            }
        );
    }

    /// Consent is enforced on the wire, per response: a server that answers
    /// without `Access-Control-Allow-Origin` is refused even though its
    /// address and origin both passed, and the request carries the `Origin`
    /// header a dynamically-configured server needs to answer at all.
    #[test]
    fn wire_fetches_under_the_default_policy_require_cors_consent() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/kit.wav");
        let saw_origin_header = Arc::new(AtomicBool::new(false));
        let server_seen = Arc::clone(&saw_origin_header);
        let server = std::thread::spawn(move || {
            for consent in [None, Some("*"), None] {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::BufRead::read_line(&mut reader, &mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => {
                            if line
                                .to_ascii_lowercase()
                                .starts_with(&format!("origin: {CORS_REQUEST_ORIGIN}"))
                            {
                                server_seen.store(true, Ordering::Release);
                            }
                        }
                        Err(_) => break,
                    }
                }
                let consent_header = consent
                    .map(|value| format!("Access-Control-Allow-Origin: {value}\r\n"))
                    .unwrap_or_default();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\n{consent_header}Content-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                let _ = stream.flush();
            }
        });

        let consent_required = ScoreFetchAccess::Remote {
            origin: origin.clone(),
            cors_required: true,
        };
        let refusal = fetch_score_source(&url, &consent_required, 4096)
            .expect_err("a response without consent must be refused");
        assert!(
            refusal.contains("consent") && refusal.contains("--allow-sample-origin"),
            "the refusal names the header and the remedy: {refusal}"
        );
        assert_eq!(
            fetch_score_source(&url, &consent_required, 4096).expect("a consenting response"),
            b"ok".to_vec()
        );
        assert!(
            saw_origin_header.load(Ordering::Acquire),
            "consent-requiring fetches must send the Origin header"
        );

        // An exact grant asks the same server with no consent demanded.
        let granted = ScoreFetchAccess::Remote {
            origin,
            cors_required: false,
        };
        assert_eq!(
            fetch_score_source(&url, &granted, 4096).expect("granted fetch"),
            b"ok".to_vec()
        );
        server.join().expect("server thread");
    }

    fn settle(library: &SampleLibrary) {
        library.wait_until_idle(Duration::from_secs(2));
    }

    /// An origin grant gives network access only, never the filesystem.
    /// With no grant, the call fails instead of dropping the bad entry.
    #[test]
    fn an_origin_grant_never_unlocks_the_filesystem() {
        let library = SampleLibrary::empty();

        assert!(
            library
                .register_score_custom(
                    r#"{"bd": "file:///etc/passwd"}"#,
                    None,
                    &ScoreSampleAccess::denied(),
                )
                .is_err(),
            "an ungranted score must fail closed"
        );
        assert!(!library.knows("bd"));

        // Grant an ordinary web origin, then try the same local read.
        let _ = library
            .register_custom_for_test("https://example.com", r#"{"bd": "file:///etc/passwd"}"#);
        assert!(
            !library.knows("bd"),
            "an origin grant is not a filesystem grant"
        );

        // A `_base` is as much a local read as an entry is.
        let _ = library.register_custom_for_test(
            "https://example.com",
            r#"{"sd": {"_base": "file:///etc", "c3": "passwd"}}"#,
        );
        assert!(!library.knows("sd"));
    }

    /// The documented web paths keep working: an absolute https entry
    /// registers without any network activity (loads happen per trigger).
    #[test]
    fn ordinary_web_sources_still_register() {
        let library = SampleLibrary::empty();
        library
            .register_custom_for_test(
                "https://example.com",
                r#"{"bd": "https://example.com/kits/808/kick.wav"}"#,
            )
            .expect("https entry");
        settle(&library);
        assert!(library.knows("bd"));
    }

    /// Cache names stay flat even when a URL's tail tries to carry a path.
    #[test]
    fn cache_names_take_only_a_plain_extension() {
        let dir = std::env::temp_dir();
        for (url, expected) in [
            ("https://example.com/kick.wav", Some(".wav")),
            ("https://example.com/kick.WAV", Some(".WAV")),
            ("https://example.com/kick.wav?rev=2", Some(".wav")),
            ("https://example.com/a.b/c", None),
            ("https://example.com/x/y..//z", None),
        ] {
            let path = cache_path(&dir, url);
            let parent_gone = path.parent().map(|p| p == dir).unwrap_or(true);
            assert!(parent_gone, "{url} escaped the cache directory: {path:?}");
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(
                name.rfind('.').map(|i| &name[i..]),
                expected,
                "{url}: wrong extension handling ({name})"
            );
        }
    }
}

#[cfg(test)]
mod manifest_worker_tests {
    /// How long a liveness wait allows for a request to arrive.
    ///
    /// The value is generous on purpose. These waits check that the request
    /// reached the server, not how fast. The separate `elapsed()` bounds make
    /// the timing claims. A short timeout fails under parallel load.
    const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(30);

    use super::*;
    use std::io::{BufRead, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicUsize;

    fn async_register(
        library: &SampleLibrary,
        effects: Vec<(String, Option<String>)>,
        preloads: &[String],
    ) -> Result<PrefetchStatus, String> {
        library.register_trusted_batch_async_for_test(effects, preloads)
    }

    fn inline_bank(name: &str, url: &str) -> String {
        format!(r#"{{"{name}":"{url}"}}"#)
    }

    fn digest(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// A budget whose deadline has already passed: every `check` fails at
    /// once, deterministic where waiting out a real deadline is not.
    fn rustel_sample_fetch_deadline_in_past() -> sample_fetch::FetchBudget {
        sample_fetch::FetchBudget::until(
            Instant::now() - Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn test_dir(label: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "rustel-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("test directory");
        path
    }

    fn wait_for_manifest_jobs(shared: &Shared) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.manifest_pending.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shared.manifest_pending.load(Ordering::Acquire), 0);
    }

    pub(super) fn drain_request(stream: TcpStream) -> TcpStream {
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone stream"));
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
        }
        stream
    }

    #[test]
    fn dropping_a_library_releases_its_background_workers() {
        let library = SampleLibrary::empty();
        let shared = Arc::downgrade(&library.shared);
        drop(library);

        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.strong_count() != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            shared.strong_count(),
            0,
            "sample/font/manifest worker retained Shared after its library dropped"
        );
    }

    fn respond(mut stream: TcpStream, body: &str) {
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
    }

    #[test]
    fn retry_batch_stays_before_publications_on_both_sides_of_its_lock() {
        let library = SampleLibrary::empty();
        let replaced_id = SampleId(9);
        let retry_id = SampleId(10);
        let decoded =
            |value| DecodedSample::from_parts(48_000, 1, vec![value]).expect("decoded fixture");
        let stale_retry = decoded(0.25);
        let superseded_retry = decoded(0.1);
        let latest_retry = decoded(0.2);
        let already_published = decoded(0.5);
        let published_after = decoded(0.75);

        // This publication represents the race window after a producer took
        // its old batch but before it reacquired the ready mutex to retry.
        library.requeue_ready(replaced_id, already_published.clone());
        library.requeue_ready_batch_before_newer(vec![
            (retry_id, superseded_retry),
            (replaced_id, stale_retry),
            (retry_id, latest_retry.clone()),
        ]);
        library.requeue_ready(replaced_id, published_after.clone());

        assert_eq!(
            library.decoded_identity(retry_id),
            Some(latest_retry.identity())
        );
        assert_eq!(
            library.decoded_identity(replaced_id),
            Some(published_after.identity())
        );

        assert_eq!(
            library.take_ready(),
            vec![
                (retry_id, latest_retry),
                (replaced_id, already_published),
                (replaced_id, published_after)
            ]
        );
    }

    /// A one-frame WAV, enough for the scan to call a file audio.
    pub(super) fn tiny_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&40u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&96_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[0, 0, 0, 0]);
        wav
    }

    /// A folder imported in Settings is every set's, and it sits under the
    /// set you are in: a set that has its own `bd` keeps playing its own.
    #[test]
    fn an_imported_folder_is_under_the_open_sets_own_folder() {
        let root = test_dir("imported-folder-precedence");
        std::fs::create_dir_all(root.join("kicks")).expect("fixture");
        std::fs::write(root.join("kicks/1.wav"), tiny_wav()).expect("fixture");
        std::fs::create_dir_all(root.join("bd")).expect("fixture");
        std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let reports = library.adopt_global_sources_settled(&[GlobalSource {
            spec: format!("local:{}", root.display()),
            enabled: true,
        }]);
        assert_eq!(
            reports,
            vec![GlobalSourceReport {
                spec: format!("local:{}", root.display()),
                state: GlobalSourceState::Ready { banks: 2 },
            }],
            "each child folder is one bank"
        );
        assert!(library.knows("bd"), "the imported bd is nameable");
        assert!(library.knows("kicks"));
        assert_eq!(library.variants_of("kicks"), Some(1));

        // The set's own folder outranks it, which is the whole point of the
        // layer sitting under `custom`.
        library.custom.write().expect("custom banks").insert(
            "bd".to_owned(),
            Bank::Array(vec![Arc::from("file:///set/bd.wav")]),
        );
        let chosen = library
            .look_up("bd", |named| match named {
                Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                _ => "not an array".to_owned(),
            })
            .expect("bd resolves");
        assert_eq!(chosen, "file:///set/bd.wav", "the set's own bd wins");

        // And it comes back when the set stops claiming the name.
        library.custom.write().expect("custom banks").remove("bd");
        let chosen = library
            .look_up("bd", |named| match named {
                Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                _ => "not an array".to_owned(),
            })
            .expect("bd resolves");
        let chosen = chosen.replace('\\', "/");
        assert!(chosen.ends_with("bd/0.wav") && chosen.contains("imported-folder-precedence"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// What a drop and the picker hand over is a bare path. It has to be a
    /// folder, or nothing anybody imports from the studio ever imports.
    #[test]
    fn a_bare_folder_path_is_a_folder_source() {
        let root = test_dir("bare-path-source");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("snap")).expect("fixture");
        std::fs::write(root.join("snap/0.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let reports = library.adopt_global_sources_settled(&[GlobalSource {
            spec: root.display().to_string(),
            enabled: true,
        }]);
        assert_eq!(
            reports[0].state,
            GlobalSourceState::Ready { banks: 1 },
            "a bare absolute path is a folder, not a URL: {:?}",
            reports[0].state
        );
        assert!(library.knows("snap"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A new folder can be added before any audio has been copied into it. It
    /// stays imported and a later scan picks up the files when they arrive.
    #[test]
    fn an_empty_folder_is_a_ready_source_and_can_fill_later() {
        let root = test_dir("empty-source-can-fill-later");
        let source = GlobalSource {
            spec: root.display().to_string(),
            enabled: true,
        };
        let library = SampleLibrary::empty();

        let reports = library.adopt_global_sources_settled(std::slice::from_ref(&source));
        assert_eq!(reports[0].state, GlobalSourceState::Ready { banks: 0 });

        std::fs::write(root.join("kick.wav"), tiny_wav()).expect("sample arrives");
        let reports = library.adopt_global_sources_settled(&[source]);
        assert_eq!(reports[0].state, GlobalSourceState::Ready { banks: 1 });
        assert!(library.knows("kick"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A pack lands in the imported layer, filed under its source, and a
    /// re-adoption that no longer lists it takes it away.
    #[test]
    fn a_pack_lands_in_the_imported_layer_and_a_later_adoption_can_drop_it() {
        // A relative entry: it resolves against the pack's own origin,
        // which is what the player's grant covers. An absolute entry on
        // another plain-http host is refused, exactly as a score's would be.
        let body = r#"{"tone":"tone.wav"}"#.to_owned();
        let (url, arrival, server) = serve_once("/pack.json", body);

        let library = SampleLibrary::empty();
        // Not the settled reading: this one is about the row that says
        // `fetching…` while the pack is still on its way.
        let reports = library.adopt_global_sources(&[GlobalSource {
            spec: url.clone(),
            enabled: true,
        }]);
        assert_eq!(reports[0].state, GlobalSourceState::Loading);
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the pack was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("pack server");

        assert!(library.knows("tone"), "the pack's sound is nameable");
        assert!(
            library
                .global
                .read()
                .expect("global banks")
                .contains_key("tone"),
            "it lives in the imported layer"
        );
        assert!(
            !library
                .custom
                .read()
                .expect("custom banks")
                .contains_key("tone"),
            "and not in the score's"
        );
        assert_eq!(
            library.global_source_reports()[0].state,
            GlobalSourceState::Ready { banks: 1 },
            "the row says what it brought"
        );
        let entry = library
            .catalogue()
            .into_iter()
            .find(|entry| entry.name == "tone")
            .expect("in the browser");
        assert_eq!(entry.origin, SoundOrigin::Global);
        assert_eq!(
            entry.import.as_deref(),
            Some(url.as_str()),
            "filed under its source"
        );

        // Gone from Settings, gone from the library.
        library.adopt_global_sources(&[]);
        assert!(!library.knows("tone"));
    }

    /// A pack's list that arrives after the adoption that asked for it has
    /// been replaced is thrown away, not resurrected.
    #[test]
    fn a_stale_pack_list_does_not_resurrect_a_removed_source() {
        let body = r#"{"late":"late.wav"}"#.to_owned();
        let (url, entered, release, server) = stalled_server(body);

        let library = SampleLibrary::empty();
        library.adopt_global_sources(&[GlobalSource {
            spec: url.clone(),
            enabled: true,
        }]);
        entered
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the pack was asked for");
        // Removed before its list has landed.
        library.adopt_global_sources(&[]);
        release.send(()).expect("release the list");
        server.join().expect("pack server");
        library.wait_until_idle(Duration::from_secs(2));

        assert!(
            !library.knows("late"),
            "a list from a superseded adoption must not fill a row that is gone"
        );
        assert!(library.global_source_reports().is_empty());
    }

    /// A pack that shares a name with the open set's folder leaves the
    /// set's bookkeeping alone: the set's bank stays the set's, and the
    /// next set can still put it down.
    #[test]
    fn a_pack_does_not_untrack_the_sets_own_bank() {
        let set_root = test_dir("pack-vs-set-folder");
        std::fs::create_dir_all(&set_root).expect("fixture");
        std::fs::create_dir_all(set_root.join("bd")).expect("fixture");
        std::fs::write(set_root.join("bd/0.wav"), tiny_wav()).expect("fixture");
        let body = r#"{"bd":"bd.wav"}"#.to_owned();
        let (url, arrival, server) = serve_once("/pack.json", body);

        let library = SampleLibrary::empty();
        library.adopt_set_folder(&set_root).expect("adopt the set");
        assert!(library.set_folder_holds(&set_root, "bd"));
        library.adopt_global_sources(&[GlobalSource {
            spec: url,
            enabled: true,
        }]);
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the pack was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("pack server");

        assert!(
            library.set_folder_holds(&set_root, "bd"),
            "the set's own bd is still the set's"
        );
        assert!(
            library
                .shared
                .set_banks
                .read()
                .expect("set banks")
                .contains_key("bd"),
            "the set's displacement record survived the pack"
        );
        let bd = library
            .catalogue()
            .into_iter()
            .find(|entry| entry.name == "bd")
            .expect("bd in the browser");
        assert_eq!(
            bd.origin,
            SoundOrigin::Set,
            "the browser still says whose it is"
        );
        let _ = std::fs::remove_dir_all(set_root);
    }

    /// Colliding sources keep a stable canonical name and preserve the other
    /// source under an automatic alias, independent of arrival order.
    #[test]
    fn a_later_folder_row_outranks_an_earlier_pack() {
        let root = test_dir("folder-below-pack");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("bd")).expect("fixture");
        std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");
        let body = r#"{"bd":"pack-bd.wav"}"#.to_owned();
        let (url, arrival, server) = serve_once("/pack.json", body);

        let library = SampleLibrary::empty();
        library.adopt_global_sources(&[
            GlobalSource {
                spec: url,
                enabled: true,
            },
            GlobalSource {
                spec: root.display().to_string(),
                enabled: true,
            },
        ]);
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the pack was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("pack server");

        let chosen = library
            .look_up("bd", |named| match named {
                Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                _ => "not an array".to_owned(),
            })
            .expect("bd resolves");
        assert!(chosen.starts_with("file://"), "stable canonical: {chosen}");
        assert!(library.knows("bd_1"), "the pack remains addressable");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The imported layer sits over the fonts and the pinned banks, not
    /// only under the set's own folder.
    #[test]
    fn an_import_outranks_the_fonts_and_the_pinned_banks() {
        let root = test_dir("import-over-pinned");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("bd")).expect("fixture");
        std::fs::create_dir_all(root.join("gm_piano")).expect("fixture");
        std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");
        std::fs::write(root.join("gm_piano/0.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::with_background_loaders(
            HashMap::from([(
                "bd".to_owned(),
                Bank::Array(vec![Arc::from("http://127.0.0.1:9/pinned-bd.wav")]),
            )]),
            Vec::new(),
            HashMap::from([(
                "gm_piano".to_owned(),
                vec![Arc::from("0000_Piano_sf2_file")],
            )]),
            String::new(),
        )
        .expect("library");
        library.adopt_global_sources_settled(&[GlobalSource {
            spec: root.display().to_string(),
            enabled: true,
        }]);

        let first = |name: &str| {
            library
                .look_up(name, |named| match named {
                    Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                    Named::Bank(Bank::Notes(_)) => "keyed".to_owned(),
                    Named::Font(_) => "font".to_owned(),
                })
                .expect("resolves")
        };
        assert!(
            first("bd").starts_with("file://"),
            "over the pinned bank: {}",
            first("bd")
        );
        assert!(
            first("gm_piano").starts_with("file://"),
            "over the font: {}",
            first("gm_piano")
        );
        assert_eq!(
            library.variants_of("gm_piano"),
            Some(1),
            "and every asker agrees"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn colliding_imports_get_stable_source_scoped_aliases() {
        let root = test_dir("collision aliases");
        let alpha = root.join("alpha");
        let beta = root.join("beta");
        std::fs::create_dir_all(&alpha).expect("alpha");
        std::fs::create_dir_all(&beta).expect("beta");
        std::fs::create_dir_all(alpha.join("kick")).expect("alpha bank");
        std::fs::create_dir_all(beta.join("kick")).expect("beta bank");
        std::fs::write(alpha.join("kick/0.wav"), tiny_wav()).expect("alpha kick");
        std::fs::write(beta.join("kick/0.wav"), tiny_wav()).expect("beta kick");
        let library = SampleLibrary::empty();
        let source = |path: &Path| GlobalSource {
            spec: path.display().to_string(),
            enabled: true,
        };
        library.adopt_global_sources_settled(&[source(&beta), source(&alpha)]);
        assert!(library.knows("kick"));
        assert!(library.knows("kick_1"));
        let entries = library.catalogue();
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.name == "kick")
                .and_then(|entry| entry.import.as_deref()),
            Some(alpha.to_string_lossy().as_ref()),
            "the canonical first source owns the unsuffixed name"
        );
        library.set_source_bank_renames(HashMap::from([(
            (beta.display().to_string(), "kick".to_owned()),
            "thump".to_owned(),
        )]));
        assert!(library.knows("thump"), "the manual source alias wins");
        assert!(library.knows("kick"), "the other source keeps its own name");
        library.set_source_bank_renames(HashMap::new());
        library.adopt_global_sources_settled(&[source(&beta)]);
        assert!(
            library.knows("kick_1"),
            "removal does not rename a live score"
        );
        assert!(!library.knows("kick"), "the vacated base remains a gap");
        assert_eq!(
            library.banks_for_import(&beta.display().to_string()),
            ["kick"]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_parent_silently_owns_and_replaces_its_child_source() {
        let root = test_dir("covered child");
        let child = root.join("drums");
        std::fs::create_dir_all(&child).expect("child");
        std::fs::write(child.join("kick.wav"), tiny_wav()).expect("kick");
        let reports = SampleLibrary::empty().adopt_global_sources_settled(&[
            GlobalSource {
                spec: child.display().to_string(),
                enabled: true,
            },
            GlobalSource {
                spec: root.display().to_string(),
                enabled: true,
            },
        ]);
        assert_eq!(reports.len(), 1, "the child has no duplicate row");
        assert_eq!(reports[0].spec, root.display().to_string());
        assert_eq!(reports[0].state, GlobalSourceState::Ready { banks: 1 });
        let _ = std::fs::remove_dir_all(root);
    }

    /// The file behind a browser row is that row's own: a numbered sample
    /// is its own file, not the bank's first, and a name no file plays has
    /// no location.
    #[test]
    fn a_sounds_file_location_is_the_variants_own_file() {
        let root = test_dir("file location");
        let kicks = root.join("kicks");
        std::fs::create_dir_all(&kicks).expect("fixture");
        for name in ["a.wav", "b.wav", "c.wav"] {
            std::fs::write(kicks.join(name), tiny_wav()).expect("fixture");
        }
        let library = SampleLibrary::empty();
        library.adopt_set_folder(&root).expect("adopt");

        let first = library.file_location("kicks", None).expect("the bank");
        assert!(first.ends_with("a.wav"), "{first}");
        let third = library
            .file_location("kicks", Some(2))
            .expect("the third sample");
        assert!(third.ends_with("c.wav"), "{third}");
        assert_eq!(
            library.file_location("kicks", Some(9)),
            Some(first),
            "a number past the end falls back to the first file"
        );
        assert_eq!(library.file_location("no-such-sound", Some(0)), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The set's own folder answers to the renames too.
    #[test]
    fn a_renamed_set_bank_answers_to_its_new_name() {
        let root = test_dir("rename-set-folder");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("hats")).expect("fixture");
        std::fs::write(root.join("hats/0.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        library.set_bank_renames(HashMap::from([("hats".to_owned(), "ch".to_owned())]));
        library.adopt_set_folder(&root).expect("adopt");
        assert!(library.set_folder_holds(&root, "ch"));
        assert!(!library.knows("hats"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A rename made after a source landed reaches it without re-adopting:
    /// the rows are read through the overlay again.
    #[test]
    fn a_rename_reaches_an_imported_folder_without_re_adopting() {
        let root = test_dir("rename-after-adopt");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("hats")).expect("fixture");
        std::fs::write(root.join("hats/0.wav"), tiny_wav()).expect("fixture");
        let library = SampleLibrary::empty();
        library.adopt_global_sources_settled(&[GlobalSource {
            spec: root.display().to_string(),
            enabled: true,
        }]);
        assert!(library.knows("hats"));
        library.set_bank_renames(HashMap::from([("hats".to_owned(), "ch".to_owned())]));
        assert!(library.knows("ch") && !library.knows("hats"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The set's own folder is found by the spelling the scan gives its
    /// urls - a raw path - so a set with a space in its path still holds
    /// what it holds.
    #[test]
    fn set_folder_holds_survives_a_space_in_the_path() {
        let root = test_dir("set folder with space");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("bd")).expect("fixture");
        std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");
        let library = SampleLibrary::empty();
        library.adopt_set_folder(&root).expect("adopt");
        assert!(
            library.set_folder_holds(&root, "bd"),
            "a space in the set's path must not hide its own sounds"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Eleven files come back in the order they went: the copies are
    /// zero-padded so the scan's name order is the bank's order.
    #[test]
    fn a_consolidated_bank_keeps_its_order_past_ten_files() {
        let source = test_dir("consolidate-order-source");
        let set = test_dir("consolidate-order-set");
        std::fs::create_dir_all(source.join("kit")).expect("fixture");
        std::fs::create_dir_all(&set).expect("fixture");
        for index in 0..11 {
            // Each file is told apart by its length, so order is checkable
            // after the copy.
            let mut wav = tiny_wav();
            wav.extend(std::iter::repeat_n(0u8, index * 2));
            std::fs::write(source.join(format!("kit/{index}.wav")), wav).expect("fixture");
        }
        let library = SampleLibrary::empty();
        library.adopt_global_sources_settled(&[GlobalSource {
            spec: source.display().to_string(),
            enabled: true,
        }]);
        let lengths = |library: &SampleLibrary| -> Vec<usize> {
            library
                .look_up("kit", |named| match named {
                    Named::Bank(Bank::Array(urls)) => urls
                        .iter()
                        .map(|url| fetch_located(url).expect("read").len())
                        .collect(),
                    _ => Vec::new(),
                })
                .expect("kit")
        };
        let before = lengths(&library);
        assert_eq!(library.copy_bank_into("kit", &set), Ok(11));
        library.adopt_set_folder(&set).expect("adopt the set");
        assert_eq!(
            lengths(&library),
            before,
            "`n` picks the same file before and after"
        );
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(set);
    }

    /// A keyed bank is refused rather than laid out wrong, and a bank with
    /// a file still to download is refused rather than copied with a gap.
    #[test]
    fn consolidating_refuses_what_cannot_round_trip() {
        let set = test_dir("consolidate-refusals");
        std::fs::create_dir_all(&set).expect("fixture");
        let library = SampleLibrary::empty();
        library.banks.write().expect("banks").insert(
            "piano".to_owned(),
            Bank::Notes(vec![(60.0, vec![Arc::from("http://127.0.0.1:9/c4.wav")])]),
        );
        library.banks.write().expect("banks").insert(
            "remote".to_owned(),
            Bank::Array(vec![Arc::from("http://127.0.0.1:9/never.wav")]),
        );
        let keyed = library
            .copy_bank_into("piano", &set)
            .expect_err("a keyed bank is refused");
        assert!(keyed.contains("keyed"), "{keyed}");
        let gap = library
            .copy_bank_into("remote", &set)
            .expect_err("a missing file is refused");
        assert!(gap.contains("1 of 1"), "{gap}");
        assert!(!set.join("piano").exists() && !set.join("remote").exists());
        let _ = std::fs::remove_dir_all(set);
    }

    /// Consolidating never fetches - proven by a server that would have to
    /// see a connection, and does not.
    #[test]
    fn consolidating_never_fetches() {
        let set = test_dir("consolidate-no-fetch");
        std::fs::create_dir_all(&set).expect("fixture");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}/never.wav", listener.local_addr().unwrap());
        let library = SampleLibrary::empty();
        library.banks.write().expect("banks").insert(
            "remote".to_owned(),
            Bank::Array(vec![Arc::from(url.as_str())]),
        );
        let _ = library.copy_bank_into("remote", &set);
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "consolidating connected to the network"
        );
        let _ = std::fs::remove_dir_all(set);
    }

    /// A pack's file is copied out of the score cache, where a granted url
    /// is kept.
    #[test]
    fn consolidating_reads_a_granted_url_out_of_the_score_cache() {
        let set = test_dir("consolidate-score-cache");
        std::fs::create_dir_all(&set).expect("fixture");
        let library = SampleLibrary::empty();
        let url = "http://127.0.0.1:9/pack/clap.wav";
        library
            .shared
            .score_sources
            .write()
            .expect("grants")
            .insert(
                Arc::from(url),
                ScoreFetchAccess::Remote {
                    origin: "http://127.0.0.1:9".to_owned(),
                    cors_required: false,
                },
            );
        let path =
            library
                .shared
                .score_cache
                .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
        library
            .shared
            .score_cache
            .admit(&path, &tiny_wav())
            .expect("seed the score cache");
        library
            .banks
            .write()
            .expect("banks")
            .insert("clap".to_owned(), Bank::Array(vec![Arc::from(url)]));
        assert_eq!(library.copy_bank_into("clap", &set), Ok(1));
        assert!(set.join("clap/0.wav").is_file());
        let _ = std::fs::remove_dir_all(set);
    }

    /// The extension a copy gets is the one the scan will read back, taken
    /// from the path rather than from a query or a fragment.
    #[test]
    fn a_copy_is_named_by_what_the_scan_can_read() {
        assert!(is_sample_audio(Path::new("0.mp3")));
        assert!(!is_sample_audio(Path::new("0.mp3#v")));
        assert!(matches!(codec_for("http://x/kick.mp3?v=2"), Codec::Mp3));
        assert!(matches!(codec_for("http://x/kick.wav#frag"), Codec::Wav));
    }

    /// Emptying the cache removes the cache's own entries and nothing else:
    /// not its lock, not its marker, not a file somebody kept beside it.
    #[test]
    fn clearing_the_cache_removes_only_what_the_cache_wrote() {
        let base = test_dir("clear-cache-scope");
        std::fs::create_dir_all(&base).expect("fixture");
        // The cache writer names the entries, so the guard is tested against
        // the names the cache really writes.
        let entry = cache_path(&base, "https://example.test/kick.wav");
        let bare = cache_path(&base, "https://example.test/manifest");
        let foreign = base.join("my-notes.txt");
        let dir = base.join("keep");
        std::fs::create_dir_all(&dir).expect("fixture");
        for path in [&entry, &bare, &foreign] {
            std::fs::write(path, b"x").expect("fixture");
        }
        assert_ne!(entry, bare, "two urls, two entries");
        assert!(sample_cache_usage_at(&base) >= 3);
        clear_sample_cache_at(&base).expect("clear");
        assert!(
            !entry.exists() && !bare.exists(),
            "the cache's own entries go"
        );
        assert!(foreign.exists(), "a foreign file stays");
        assert!(dir.exists(), "a folder stays");
        assert!(
            base.join(SCORE_CACHE_NO_LEGACY_MARKER).exists(),
            "the score cache's marker is left standing"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    /// A source that will not read is a row that says so, never a studio
    /// that will not open: an unplugged drive must cost you its sounds and
    /// nothing else.
    #[test]
    fn a_missing_source_is_reported_and_the_others_still_import() {
        let root = test_dir("missing-source-is-not-fatal");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::write(root.join("clap.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let reports = library.adopt_global_sources_settled(&[
            GlobalSource {
                spec: "local:/no/such/folder/anywhere".to_owned(),
                enabled: true,
            },
            GlobalSource {
                spec: format!("local:{}", root.display()),
                enabled: true,
            },
            GlobalSource {
                spec: "local:/turned/off".to_owned(),
                enabled: false,
            },
        ]);
        assert!(matches!(reports[0].state, GlobalSourceState::Missing(_)));
        assert_eq!(reports[1].state, GlobalSourceState::Ready { banks: 1 });
        assert_eq!(reports[2].state, GlobalSourceState::Off);
        assert!(library.knows("clap"), "the readable source still imported");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A folder batch whose budget is spent must not leave an unwalked row
    /// on `Loading`. Each such row reports the walk's name instead. The test
    /// calls the folder walk directly with a spent budget, which is
    /// deterministic.
    #[test]
    fn a_spent_budget_says_so_on_every_folder_it_never_walked() {
        let first = test_dir("spent-budget-folder-a");
        let second = test_dir("spent-budget-folder-b");
        std::fs::create_dir_all(&first).expect("fixture");
        std::fs::create_dir_all(&second).expect("fixture");
        std::fs::write(first.join("kick.wav"), tiny_wav()).expect("fixture");
        std::fs::write(second.join("snap.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let specs = vec![
            format!("local:{}", first.display()),
            format!("local:{}", second.display()),
        ];
        // The rows are staged straight on, not adopted: an adoption would
        // enqueue a real walk on a fine budget, and the always-running worker
        // could land it either side of the spent call below - a race, not a
        // result.
        *library
            .shared
            .global_slots
            .lock()
            .expect("global source slots") = specs
            .iter()
            .enumerate()
            .map(|(row, spec)| GlobalSlot {
                row,
                spec: spec.clone(),
                kind: GlobalKind::Folder,
                banks: HashMap::new(),
                state: GlobalSourceState::Loading,
                rewalk: false,
            })
            .collect::<Vec<_>>();
        let generation = library
            .shared
            .global_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;

        // A spent budget: the check between folders is the walk's first act.
        let budget = rustel_sample_fetch_deadline_in_past();
        let _ = library.run_folders_work_for_test(&specs, generation, &budget);

        let reports = library.global_source_reports();
        for report in reports.iter().filter(|report| specs.contains(&report.spec)) {
            assert!(
                matches!(report.state, GlobalSourceState::Failed(_)),
                "{} was left on {:?} with nothing coming to fill it",
                report.spec,
                report.state
            );
        }
        assert!(
            !library.knows("kick") && !library.knows("snap"),
            "a walk that never happened brings no sounds"
        );
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }

    /// The same spent budget through the whole worker, not just the folder
    /// arm: the deadline error surfaces as the job's result, and no later
    /// folder steals a walk the batch had already lost.
    #[test]
    fn a_folder_batch_that_runs_out_of_budget_aborts_between_folders_not_mid_row() {
        let first = test_dir("abort-between-folders-a");
        let second = test_dir("abort-between-folders-b");
        std::fs::create_dir_all(&first).expect("fixture");
        std::fs::create_dir_all(&second).expect("fixture");
        std::fs::write(first.join("kick.wav"), tiny_wav()).expect("fixture");
        std::fs::write(second.join("snap.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let specs = vec![
            format!("local:{}", first.display()),
            format!("local:{}", second.display()),
        ];
        // The rows are staged straight on, not adopted: an adoption would
        // enqueue a real walk on a fine budget, and the always-running worker
        // could land it either side of the spent call below - a race, not a
        // result.
        *library
            .shared
            .global_slots
            .lock()
            .expect("global source slots") = specs
            .iter()
            .enumerate()
            .map(|(row, spec)| GlobalSlot {
                row,
                spec: spec.clone(),
                kind: GlobalKind::Folder,
                banks: HashMap::new(),
                state: GlobalSourceState::Loading,
                rewalk: false,
            })
            .collect::<Vec<_>>();
        let generation = library
            .shared
            .global_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        // Through the whole worker, not just the folder arm: a real job on a
        // real queue, its verdict read from the completion the caller owns.
        // A job with no time at all is spent the moment the worker takes it.
        let context = library.manifest_context();
        let (queue, jobs) = manifest_queue::manifest_queue();
        let worker = std::thread::spawn(move || run_manifest_worker(context, jobs));
        // enqueue marks pending before it sends and the worker unpends when
        // the job is finished; the count is kept honest the same way here.
        library
            .shared
            .manifest_pending
            .fetch_add(1, Ordering::AcqRel);
        let (completion, verdict) = mpsc::sync_channel(1);
        queue
            .push(Box::new(ManifestJob {
                work: ManifestWork::Folders {
                    specs: specs.clone(),
                    generation,
                },
                timeout: Duration::ZERO,
                completion: Some(completion),
            }))
            .expect("the empty queue takes the batch");
        queue.close();
        let result = verdict
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers a queued batch");
        worker
            .join()
            .expect("the worker ends when its queue closes");
        assert_eq!(
            result.expect_err("a spent budget refuses the batch"),
            "sample manifest deadline exceeded"
        );
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }

    /// Re-adopting replaces rather than accumulates: a source removed in
    /// Settings takes its sounds with it.
    #[test]
    fn re_adopting_drops_the_sources_that_are_gone() {
        let root = test_dir("re-adopt-drops-old");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::write(root.join("rim.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let source = GlobalSource {
            spec: format!("local:{}", root.display()),
            enabled: true,
        };
        library.adopt_global_sources_settled(std::slice::from_ref(&source));
        assert!(library.knows("rim"));
        library.adopt_global_sources_settled(&[]);
        assert!(!library.knows("rim"), "the removed source took its sounds");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Consolidating copies what the scores name into the set's folder,
    /// where the set's own scan then answers for it - no score is
    /// rewritten, because the folder already wins by name.
    #[test]
    fn copying_a_bank_into_the_set_lays_it_out_the_way_the_scan_reads_it() {
        let library_root = test_dir("consolidate-source");
        let set_root = test_dir("consolidate-set");
        std::fs::create_dir_all(&library_root).expect("fixture");
        std::fs::create_dir_all(&set_root).expect("fixture");
        std::fs::write(library_root.join("clap.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        library.adopt_global_sources_settled(&[GlobalSource {
            spec: format!("local:{}", library_root.display()),
            enabled: true,
        }]);
        assert!(library.knows("clap"));
        assert!(
            !library.set_folder_holds(&set_root, "clap"),
            "the set does not carry it yet"
        );

        assert_eq!(library.copy_bank_into("clap", &set_root), Ok(1));
        // The scan's rule read backwards: a folder named for the bank, with
        // the bank's files inside in the order it had them.
        assert!(set_root.join("clap/0.wav").is_file());

        // And the set now answers for it out of its own folder, which is
        // what makes the copied set play on another machine.
        library.adopt_set_folder(&set_root).expect("adopt");
        assert!(library.set_folder_holds(&set_root, "clap"));

        let _ = std::fs::remove_dir_all(library_root);
        let _ = std::fs::remove_dir_all(set_root);
    }

    /// A rename is an overlay on what a source brings: the files keep their
    /// own names, and taking the rename away brings the old name back.
    #[test]
    fn a_renamed_import_answers_to_the_new_name_and_gives_it_back() {
        let root = test_dir("rename-overlay");
        std::fs::create_dir_all(&root).expect("fixture");
        std::fs::create_dir_all(root.join("hats")).expect("fixture");
        std::fs::write(root.join("hats/0.wav"), tiny_wav()).expect("fixture");

        let library = SampleLibrary::empty();
        let source = GlobalSource {
            spec: format!("local:{}", root.display()),
            enabled: true,
        };
        library.set_bank_renames(HashMap::from([("hats".to_owned(), "ch".to_owned())]));
        library.adopt_global_sources_settled(std::slice::from_ref(&source));
        assert!(library.knows("ch"), "it plays under the new name");
        assert!(!library.knows("hats"), "and not under the old one");

        library.set_bank_renames(HashMap::new());
        library.adopt_global_sources_settled(std::slice::from_ref(&source));
        assert!(library.knows("hats"), "removing the rename gives it back");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn peeking_ready_sample_keeps_the_current_payload_alive_without_draining_it() {
        let library = SampleLibrary::empty();
        let id = SampleId(9);
        let older = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("older decode");
        let newer = DecodedSample::from_parts(48_000, 1, vec![0.5]).expect("newer decode");
        assert!(library.peek_ready_sample(id).is_none());

        library.requeue_ready(id, older);
        library.requeue_ready(id, newer.clone());
        let retained = library.peek_ready_sample(id).expect("queued sample");
        assert_eq!(retained.identity(), newer.identity());
        let installing = library.take_ready();
        assert_eq!(installing.len(), 2, "peeking must leave the queue intact");
        assert!(library.peek_ready_sample(id).is_none());
        drop(installing);
        assert_eq!(retained, newer, "the in-flight clone keeps its PCM alive");

        library.requeue_ready(id, retained);
        assert_eq!(
            library
                .peek_ready_sample(id)
                .expect("deferred install")
                .identity(),
            newer.identity()
        );
    }

    #[test]
    fn older_retry_cannot_replace_identity_after_newer_pcm_has_left_the_queue() {
        let library = SampleLibrary::empty();
        let id = SampleId(9);
        let older = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("older decode");
        let newer = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("newer decode");
        assert_eq!(older, newer, "content equality is independent of identity");
        assert!(newer.identity() > older.identity());

        library.requeue_ready(id, older.clone());
        let retry = library.take_ready();
        library.requeue_ready(id, newer.clone());
        assert_eq!(library.decoded_identity(id), Some(newer.identity()));
        let installing = library.take_ready();
        assert_eq!(installing[0].1.identity(), newer.identity());

        library.requeue_ready_batch_before_newer(retry);
        library.requeue_ready(id, older);
        assert_eq!(library.decoded_identity(id), Some(newer.identity()));
        assert!(library.take_ready().is_empty(), "stale PCM was requeued");
    }

    #[test]
    fn deferred_install_keeps_identity_for_every_unattempted_sample() {
        let library = SampleLibrary::empty();
        let first = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("first decode");
        let later = DecodedSample::from_parts(48_000, 1, vec![0.5]).expect("later decode");
        library.requeue_ready(SampleId(8), first.clone());
        library.requeue_ready(SampleId(9), later.clone());

        // The whole batch leaves the library before any install is attempted.
        // Refusing its first entry must not hide the later intended payload.
        let deferred = library.take_ready();
        assert_eq!(
            library.decoded_identity(SampleId(8)),
            Some(first.identity())
        );
        assert_eq!(
            library.decoded_identity(SampleId(9)),
            Some(later.identity())
        );
        library.requeue_ready_batch_before_newer(deferred);
        assert_eq!(
            library.decoded_identity(SampleId(9)),
            Some(later.identity())
        );
        assert_eq!(library.take_ready().len(), 2);
    }

    #[test]
    fn recovery_requeue_preserves_cloned_payload_identity() {
        let library = SampleLibrary::empty();
        let id = SampleId(8);
        let retained = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("decoded sample");
        library.requeue_ready(id, retained.clone());
        let initial = library.take_ready();
        assert_eq!(initial[0].1.identity(), retained.identity());
        drop(initial);

        library.requeue_ready_batch_before_newer(vec![(id, retained.clone())]);
        let reopened = library.take_ready();
        assert_eq!(library.decoded_identity(id), Some(retained.identity()));
        assert_eq!(reopened[0].1.identity(), retained.identity());
        assert_eq!(
            library.decoded_identity(BUNDLED_BD_SAMPLE_ID),
            Some(BUNDLED_BD_SAMPLE_IDENTITY)
        );
        assert_eq!(library.decoded_identity(SampleId(7)), None);
        assert_eq!(
            library.decoded_identity(SampleId(SAMPLE_BANK_CAPACITY as u32)),
            None
        );
    }

    /// Released ids are the next ones reserved, oldest first, before the
    /// counter moves - and a request the bank cannot seat whole takes
    /// nothing, so a font is never half-reserved.
    #[test]
    fn reserve_takes_released_ids_oldest_first_before_advancing_the_counter() {
        let library = SampleLibrary::empty();
        let counter_before = library.shared.next_id.load(Ordering::Relaxed);
        for id in [SampleId(5), SampleId(9)] {
            library.shared.ready.lock().expect("ready").identities[id.0 as usize] =
                DecodedIdentity::Forgotten;
        }
        library.release_ids([SampleId(5), SampleId(9)]);
        assert_eq!(library.free_id_count(), 2);

        assert_eq!(
            reserve_sample_ids(&library.shared, 1),
            Ok(vec![SampleId(5)])
        );
        assert_eq!(
            library.shared.next_id.load(Ordering::Relaxed),
            counter_before,
            "a reissued id does not move the counter"
        );
        let three = reserve_sample_ids(&library.shared, 3).expect("three");
        assert_eq!(three[0], SampleId(9), "the older released id goes first");
        assert_eq!(three[1], SampleId(counter_before));
        assert_eq!(three[2], SampleId(counter_before + 1));

        // Nothing is taken when the whole request cannot be seated.
        library.shared.ready.lock().expect("ready").identities[11] = DecodedIdentity::Forgotten;
        library.release_ids([SampleId(11)]);
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32 - 1, Ordering::Relaxed);
        assert!(reserve_sample_ids(&library.shared, 3).is_err());
        assert_eq!(library.free_id_count(), 1, "the released id is still there");
        assert_eq!(
            library.shared.next_id.load(Ordering::Relaxed),
            SAMPLE_BANK_CAPACITY as u32 - 1,
            "and the counter did not move"
        );
    }

    /// Only an id the library has forgotten can be handed back: one a table
    /// still promises, the bundled bd, or one never known are refused, and a
    /// second release is a no-op.
    #[test]
    fn release_refuses_ids_still_promised_and_the_bundled_bd() {
        let library = SampleLibrary::empty();
        let promised = SampleId(4);
        library.requeue_ready(
            promised,
            DecodedSample::from_parts(48_000, 1, vec![0.0; 4]).expect("pcm"),
        );
        library.release_ids([promised, BUNDLED_BD_SAMPLE_ID, SampleId(77)]);
        assert_eq!(
            library.free_id_count(),
            0,
            "nothing forgotten, nothing freed"
        );

        library.shared.ready.lock().expect("ready").identities[6] = DecodedIdentity::Forgotten;
        library.release_ids([SampleId(6)]);
        library.release_ids([SampleId(6)]);
        assert_eq!(library.free_id_count(), 1, "a second release is a no-op");
    }

    /// A released id accepts the next decode like a fresh slot: the
    /// tombstone that refused a stale retry is gone with the release.
    #[test]
    fn a_released_id_accepts_the_next_decode() {
        let library = SampleLibrary::empty();
        let id = SampleId(8);
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 4]).expect("pcm");
        library.shared.ready.lock().expect("ready").identities[id.0 as usize] =
            DecodedIdentity::Forgotten;
        library.requeue_ready(id, pcm());
        assert!(
            library.take_ready().is_empty(),
            "a forgotten id refuses PCM"
        );
        library.release_ids([id]);
        let fresh = pcm();
        let identity = fresh.identity();
        library.requeue_ready(id, fresh);
        let taken = library.take_ready();
        assert_eq!(taken.len(), 1, "a released id takes the next decode");
        assert_eq!(taken[0].0, id);
        assert_eq!(library.decoded_identity(id), Some(identity));
    }

    /// A fetch that fails hands its id straight back: nothing ever saw it.
    #[test]
    fn a_failed_fetch_hands_its_id_back() {
        let url: Arc<str> = Arc::from("http://127.0.0.1:9/never.wav");
        let library = SampleLibrary::with_background_loaders(
            HashMap::from([("never".to_owned(), Bank::Array(vec![url.clone()]))]),
            Vec::new(),
            HashMap::new(),
            String::new(),
        )
        .expect("library");
        assert_eq!(library.readiness("never", 0.0), SoundReadiness::Loading);
        library.wait_until_idle(Duration::from_secs(5));
        assert!(matches!(
            library.shared.by_url.read().expect("url table").get(&url),
            Some(UrlState::Failed { .. })
        ));
        assert_eq!(library.free_id_count(), 1, "the id came back");
        let reissued = reserve_sample_ids(&library.shared, 1).expect("one");
        assert_eq!(reissued, vec![SampleId(1)], "and is the next one reserved");
    }

    /// A font that failed rests, then a real ask tries again; a bet does
    /// not, and a fresh failure is left to rest.
    #[test]
    fn a_rested_failed_font_is_asked_again_by_a_real_ask() {
        let font: Arc<str> = Arc::from("0000_Test_sf2_file");
        let library = SampleLibrary::with_background_loaders(
            HashMap::new(),
            Vec::new(),
            HashMap::from([("gm_test".to_owned(), vec![font.clone()])]),
            String::new(),
        )
        .expect("library");
        let rested = Instant::now() - FAILED_RETRY_AFTER - Duration::from_secs(1);
        library
            .shared
            .fonts
            .write()
            .expect("fonts")
            .insert(font.clone(), FontState::Failed { at: rested });

        assert!(matches!(
            library.resolve_with_priority("gm_test", 0.0, 60.0, LoadPriority::Bet),
            SampleResolution::Failed
        ));
        assert!(
            matches!(
                library.shared.fonts.read().expect("fonts").get(&font),
                Some(FontState::Failed { .. })
            ),
            "a bet does not spend the retry"
        );
        assert!(matches!(
            library.resolve_with_priority("gm_test", 0.0, 60.0, LoadPriority::Now),
            SampleResolution::Loading
        ));
        assert!(
            matches!(
                library.shared.fonts.read().expect("fonts").get(&font),
                Some(FontState::Loading)
            ),
            "a real ask after the rest asks again"
        );

        library
            .shared
            .fonts
            .write()
            .expect("fonts")
            .insert(font.clone(), FontState::Failed { at: Instant::now() });
        assert!(
            matches!(
                library.resolve_with_priority("gm_test", 0.0, 60.0, LoadPriority::Now),
                SampleResolution::Failed
            ),
            "a fresh failure is left to rest"
        );
    }

    /// The batch lookup the memory policy uses finds what asking name by
    /// name finds: exact spellings first in table order, then any case.
    #[test]
    fn many_names_at_once_resolve_as_one_at_a_time_does() {
        let library = SampleLibrary::empty();
        let url = |name: &str| -> Arc<str> { Arc::from(format!("http://127.0.0.1:9/{name}.wav")) };
        let bank = |name: &str| Bank::Array(vec![url(name)]);
        library
            .custom
            .write()
            .expect("custom")
            .insert("Kick".to_owned(), bank("custom-kick"));
        {
            let mut global = library.global.write().expect("global");
            global.insert("kick".to_owned(), bank("global-kick"));
            global.insert("Snare".to_owned(), bank("global-snare"));
        }
        {
            let mut banks = library.banks.write().expect("banks");
            banks.insert("snare".to_owned(), bank("default-snare"));
            banks.insert("HH".to_owned(), bank("default-hh"));
        }
        {
            let mut by_url = library.shared.by_url.write().expect("url table");
            for (id, name) in [
                "custom-kick",
                "global-kick",
                "global-snare",
                "default-snare",
                "default-hh",
            ]
            .into_iter()
            .enumerate()
            {
                by_url.insert(
                    url(name),
                    UrlState::Ready {
                        id: SampleId(id as u32 + 1),
                        duration_secs: 1.0,
                    },
                );
            }
        }
        let names = ["Kick", "kick", "KICK", "snare", "SNARE", "hh", "Hh", "sine"];
        for name in names {
            assert_eq!(
                library.peek_ready_ids_of([name]),
                library.peek_ready_ids(name).into_iter().collect(),
                "{name}"
            );
        }
        assert_eq!(
            library.peek_ready_ids_of(names),
            names
                .iter()
                .flat_map(|name| library.peek_ready_ids(name))
                .collect()
        );
        assert_eq!(
            library.peek_ready_ids_of(["KICK"]),
            HashSet::from([SampleId(1)]),
            "the first table with any spelling of a name answers for it"
        );
    }

    /// A host that drops decoded PCM has to be able to hand the name back:
    /// nothing here keeps the samples, so a `Ready` url nobody holds is a
    /// sound that can never be played again.
    #[test]
    fn a_forgotten_url_is_asked_for_again_rather_than_reported_ready() {
        let library = SampleLibrary::empty();
        let url: Arc<str> = Arc::from("http://127.0.0.1:9/tone.wav");
        let other: Arc<str> = Arc::from("http://127.0.0.1:9/kept.wav");
        library.banks.write().expect("banks").insert(
            "tone".to_owned(),
            Bank::Array(vec![url.clone(), other.clone()]),
        );
        {
            let mut by_url = library.shared.by_url.write().expect("url table");
            by_url.insert(
                url.clone(),
                UrlState::Ready {
                    id: SampleId(7),
                    duration_secs: 1.0,
                },
            );
            by_url.insert(
                other.clone(),
                UrlState::Ready {
                    id: SampleId(8),
                    duration_secs: 1.0,
                },
            );
        }
        assert_eq!(
            library.peek_ready_ids("tone"),
            vec![SampleId(7), SampleId(8)]
        );
        let retired = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("retired decode");
        let kept = DecodedSample::from_parts(48_000, 1, vec![0.5]).expect("kept decode");
        library.requeue_ready(SampleId(7), retired.clone());
        library.requeue_ready(SampleId(8), kept.clone());

        let forgotten = library.forget_decoded(&HashSet::from([SampleId(7)]));
        assert_eq!(forgotten, vec![SampleId(7)]);
        assert_eq!(library.decoded_identity(SampleId(7)), None);
        assert_eq!(library.decoded_identity(SampleId(8)), Some(kept.identity()));
        library.requeue_ready_batch_before_newer(vec![(SampleId(7), retired.clone())]);
        library.requeue_ready(SampleId(7), retired);
        assert_eq!(library.decoded_identity(SampleId(7)), None);
        assert_eq!(library.take_ready(), vec![(SampleId(8), kept)]);
        assert_eq!(
            library.peek_ready_ids("tone"),
            vec![SampleId(8)],
            "only the dropped url is forgotten"
        );
        assert!(
            !library
                .shared
                .by_url
                .read()
                .expect("url table")
                .contains_key(&url),
            "a forgotten url must be unknown again, so the next ask loads it"
        );
    }

    /// A released id is the next one reserved. A full bank reports the
    /// failure once, rests, and the next real ask after the rest tries again.
    #[test]
    fn evicting_previews_drains_sample_ids_until_new_sounds_fail_silently() {
        // This test reads the job queue directly, so it must not share it
        // with a loader that pops jobs in order to work on them.
        let library = SampleLibrary::empty_without_loading();
        let old_url: Arc<str> = Arc::from("http://127.0.0.1:9/old.wav");
        library
            .banks
            .write()
            .expect("banks")
            .insert("old".to_owned(), Bank::Array(vec![old_url.clone()]));
        let old_id = SampleId(5);
        library.shared.by_url.write().expect("url table").insert(
            old_url.clone(),
            UrlState::Ready {
                id: old_id,
                duration_secs: 1.0,
            },
        );
        library.requeue_ready(
            old_id,
            DecodedSample::from_parts(48_000, 1, vec![0.0; 8]).expect("pcm"),
        );
        let _ = library.take_ready();
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32 - 2, Ordering::Relaxed);

        // Evicted and handed back: the next ask reuses the id rather than
        // taking a fresh one.
        assert_eq!(
            library.forget_decoded(&HashSet::from([old_id])),
            vec![old_id]
        );
        library.release_ids([old_id]);
        assert_eq!(library.readiness("old", 0.0), SoundReadiness::Loading);
        let job = library.shared.jobs.try_pop().expect("a load was queued");
        match job.kind {
            LoadKind::Install { id, .. } => {
                assert_eq!(id, old_id, "the released id is reissued first")
            }
            LoadKind::Cache => panic!("eviction retry must seat PCM, not only cache bytes"),
        }
        assert_eq!(
            library.shared.next_id.load(Ordering::Relaxed),
            SAMPLE_BANK_CAPACITY as u32 - 2,
            "the counter did not move for a reissued id"
        );

        // A full bank fails out loud, once, and rests.
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);
        let fresh_url: Arc<str> = Arc::from("http://127.0.0.1:9/fresh.wav");
        library
            .banks
            .write()
            .expect("banks")
            .insert("fresh".to_owned(), Bank::Array(vec![fresh_url.clone()]));
        assert_eq!(library.readiness("fresh", 0.0), SoundReadiness::Failed);
        let failures = library.take_failures();
        assert_eq!(
            failures
                .iter()
                .filter(|line| line.contains("fresh.wav"))
                .count(),
            1,
            "said once: {failures:?}"
        );
        assert_eq!(library.readiness("fresh", 0.0), SoundReadiness::Failed);
        assert!(library.take_failures().is_empty(), "and not again per ask");
        assert!(
            matches!(
                library
                    .shared
                    .by_url
                    .read()
                    .expect("url table")
                    .get(&fresh_url),
                Some(UrlState::Failed { .. })
            ),
            "the table remembers the failure, so the next onset does not re-fail it"
        );

        // A released id after the rest is what the retry takes.
        library.shared.by_url.write().expect("url table").insert(
            fresh_url.clone(),
            UrlState::Failed {
                at: Instant::now() - FAILED_RETRY_AFTER,
            },
        );
        let spare = SampleId(7);
        library.shared.ready.lock().expect("ready").identities[spare.0 as usize] =
            DecodedIdentity::Forgotten;
        library.release_ids([spare]);
        assert!(
            matches!(
                library.resolve_with_priority("fresh", 0.0, 60.0, LoadPriority::Now),
                SampleResolution::Loading
            ),
            "a real ask after the rest tries again"
        );
        let job = library.shared.jobs.try_pop().expect("a load was queued");
        match job.kind {
            LoadKind::Install { id, .. } => assert_eq!(id, spare),
            LoadKind::Cache => panic!("a real ask after rest must seat PCM"),
        }
    }

    /// A soundfont is one decode over one run of ids. Dropping a zone of it
    /// under the preview ceiling retires the whole font: what is left plays
    /// only the keys those zones cover, and the caller is told which ids it
    /// is now holding for nothing.
    #[test]
    fn forgetting_one_zone_forgets_its_whole_font() {
        let library = SampleLibrary::empty();
        let zone = |id| FontZone {
            key_lo: 0.0,
            key_hi: 127.0,
            base_detune: 0.0,
            id,
            duration_secs: 1.0,
            loop_secs: None,
        };
        let kept: Arc<str> = Arc::from("other.sf.js");
        library.shared.fonts.write().expect("fonts").insert(
            Arc::from("piano.sf.js"),
            FontState::Ready(Arc::new(vec![zone(SampleId(3)), zone(SampleId(4))])),
        );
        library.shared.fonts.write().expect("fonts").insert(
            kept.clone(),
            FontState::Ready(Arc::new(vec![zone(SampleId(5))])),
        );
        for id in [SampleId(3), SampleId(4), SampleId(5)] {
            library.requeue_ready(
                id,
                DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("decoded zone"),
            );
        }

        let mut forgotten = library.forget_decoded(&HashSet::from([SampleId(4)]));
        forgotten.sort_by_key(|id| id.0);
        assert_eq!(
            forgotten,
            vec![SampleId(3), SampleId(4)],
            "the untouched zone comes back so the caller can drop it too"
        );
        assert_eq!(library.decoded_identity(SampleId(3)), None);
        assert_eq!(library.decoded_identity(SampleId(4)), None);
        assert!(library.decoded_identity(SampleId(5)).is_some());
        let ready = library.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, SampleId(5));
        let fonts = library.shared.fonts.read().expect("fonts");
        assert!(!fonts.contains_key(&Arc::from("piano.sf.js")));
        assert!(
            fonts.contains_key(&kept),
            "a font with no dropped zone is left alone"
        );
    }

    /// Serve one body once, at a url no earlier process can have cached.
    ///
    /// The word over the channel is what a test waits on, with a deadline:
    /// a server thread blocked in `accept()` that nobody ever connects to
    /// would otherwise hang the whole binary on `join()`.
    fn serve_once(
        path: &str,
        body: String,
    ) -> (String, mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
        let unique_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let url = format!(
            "http://{}/{}-{}-{unique_time}{path}",
            listener.local_addr().unwrap(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        );
        let (arrived, arrival) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let stream = drain_request(stream);
            let _ = arrived.send(());
            respond(stream, &body);
        });
        (url, arrival, thread)
    }

    fn stalled_server(
        body: String,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::SyncSender<()>,
        std::thread::JoinHandle<()>,
    ) {
        static NEXT_URL: AtomicUsize = AtomicUsize::new(0);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
        // The trusted manifest cache outlives this test process. A fixed
        // path can hit bytes left by an earlier process when the OS reuses its
        // ephemeral port. Include wall time as well as the process-local
        // counter: a self-hosted runner may reuse both PIDs and ports across
        // jobs, while the fixture must always exercise its server.
        let unique_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let url = format!(
            "http://{}/strudel-{}-{unique_time}-{}.json",
            listener.local_addr().unwrap(),
            std::process::id(),
            NEXT_URL.fetch_add(1, Ordering::Relaxed)
        );
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let stream = drain_request(stream);
            entered.send(()).expect("announce request");
            release_rx.recv().expect("release response");
            respond(stream, &body);
        });
        (url, entered_rx, release, thread)
    }

    #[test]
    fn session_manifest_io_is_background_work_and_counts_as_pending() {
        let body = r#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_owned();
        let (url, entered, release, server) = stalled_server(body);
        let library = SampleLibrary::empty();
        let started = Instant::now();
        async_register(
            &library,
            vec![(serde_json::to_string(&url).unwrap(), None)],
            &[],
        )
        .expect("enqueue manifest");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "registration waited for the server: {:?}",
            started.elapsed()
        );
        entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");

        let waited = Instant::now();
        library.wait_until_idle(Duration::from_millis(40));
        assert!(
            waited.elapsed() >= Duration::from_millis(30),
            "manifest work was omitted from idle accounting"
        );
        assert_ne!(library.shared.manifest_pending.load(Ordering::Acquire), 0);

        release.send(()).unwrap();
        server.join().unwrap();
        library.wait_until_idle(Duration::from_secs(2));
        assert!(library.knows("tone"));
        assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn public_register_custom_blocks_and_returns_fetch_errors() {
        let body = r#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_owned();
        let (url, entered, release, server) = stalled_server(body);
        let library = Arc::new(SampleLibrary::empty());
        let worker_library = Arc::clone(&library);
        let (returned, returned_rx) = mpsc::sync_channel(1);
        let registration = std::thread::spawn(move || {
            let result = worker_library
                .register_trusted_custom(&serde_json::to_string(&url).expect("source JSON"), None);
            returned.send(result).expect("return result");
        });
        entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");
        assert!(
            returned_rx.recv_timeout(Duration::from_millis(40)).is_err(),
            "public registration returned before its map was ready"
        );
        release.send(()).expect("release response");
        server.join().expect("server");
        returned_rx
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("registration returned")
            .expect("registration succeeded");
        registration.join().expect("registration thread");
        assert!(library.knows("tone"), "success returned before publication");

        let error = library
            .register_trusted_custom("{", None)
            .expect_err("public parse failure must be returned");
        assert!(error.contains("does not parse"), "{error}");
    }

    #[test]
    fn public_prefetch_reports_only_the_current_published_map() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"tone":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav"]}"#,
                None,
            )
            .expect("published bank");
        assert_eq!(library.prefetch("tone"), 1);
        assert_eq!(library.prefetch("tone:1"), 1);
        assert_eq!(library.prefetch("not-yet-defined"), 0);
    }

    #[test]
    fn the_catalogue_lists_every_bank_with_its_variants_and_origin() {
        let library = SampleLibrary::empty();
        // With no banks the catalogue holds only the synths - and the
        // audio input, which stands with them.
        assert!(
            library
                .catalogue()
                .iter()
                .all(|entry| matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input))
        );
        assert!(!library.has_banks());
        library
                .register_trusted_custom(
                    r#"{"kick":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav"],"snare":["http://127.0.0.1:9/c.wav"]}"#,
                    None,
                )
                .expect("banks");
        let catalogue = library.catalogue();
        let names = catalogue
            .iter()
            .filter(|entry| !matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input))
            .map(|entry| (entry.name.as_str(), entry.variants, entry.origin))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("kick", 2, SoundOrigin::Score),
                ("snare", 1, SoundOrigin::Score)
            ]
        );
        assert!(library.has_banks());
        assert!(library.knows_sound("kick") && !library.knows_sound("kickk"));
    }

    /// The browser's catalogue lists the synths a score can play beside the
    /// banks, first, and never twice when a bank has taken a synth's name.
    #[test]
    fn the_catalogue_lists_the_native_synths_first_unless_a_bank_claims_the_name() {
        let library = SampleLibrary::empty();
        let entries = library.catalogue();
        assert!(!entries.is_empty());
        let supersaw = entries
            .iter()
            .find(|entry| entry.name == "supersaw")
            .expect("supersaw is listed");
        assert_eq!(supersaw.origin, SoundOrigin::Synth);
        assert_eq!(supersaw.variants, 1);
        assert!(
            entries.iter().any(|entry| entry.name == "sbd")
                && entries.iter().any(|entry| entry.name == "sawtooth")
        );
        let native =
            |entry: &&SoundEntry| matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input);
        assert!(
            entries.iter().take_while(native).count() == entries.iter().filter(native).count(),
            "the synths come first"
        );
        // The audio input stands with them, under its own origin: `s("i…")`
        // finds it the way it finds a synth.
        let input = entries
            .iter()
            .find(|entry| entry.name == "in")
            .expect("the audio input is listed");
        assert_eq!(input.origin, SoundOrigin::Input);
        assert_eq!(input.category, SoundCategory::Synth);
        assert_eq!(input.variants, 1, "one channel until a device says more");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.name == "supersaw")
                .count(),
            1
        );
    }

    /// A pinned bank takes the category that its pin declares, whatever its
    /// address. The pin is the only place that names a pack's category.
    #[test]
    fn a_bank_is_filed_by_what_its_pin_declares() {
        let of = |url: &str| SoundCategory::of_location(Some(url));
        assert_eq!(
            of("https://strudel.b-cdn.net/tidal-drum-machines/machines/RolandTR909/bd/x.wav"),
            SoundCategory::Drums
        );
        assert_eq!(
            of("https://strudel.b-cdn.net/uzu-drumkit/kick.wav"),
            SoundCategory::Drums
        );
        assert_eq!(
            of("https://strudel.b-cdn.net/mrid/dhin.wav"),
            SoundCategory::Percussion
        );
        assert_eq!(
            of("https://strudel.b-cdn.net/piano/A0v8.mp3"),
            SoundCategory::Piano
        );
        assert_eq!(
            of("https://strudel.b-cdn.net/VCSL/Strings/x.wav"),
            SoundCategory::Orchestra
        );
        assert_eq!(
            of("https://strudel.b-cdn.net/uzu-wavetables/wt_x.wav"),
            SoundCategory::Wavetable
        );
        assert_eq!(
            of("https://strudel.b-cdn.net/Dirt-Samples/casio/high.wav"),
            SoundCategory::Dirt
        );
        // A pack pinned off the CDN, on its author's GitHub: filed by what
        // its pin says it is, not left in Other for the accident of its
        // address.
        assert_eq!(
            of(
                "https://raw.githubusercontent.com/switchangel/pad/4f1b7bbddc72556a4246cc24e5ef282812f44d80/10_switch_angel_pad.wav"
            ),
            SoundCategory::Synth
        );
        assert_eq!(
            of(
                "https://raw.githubusercontent.com/tzfm/akwf-waveforms/4630e06f96d342fee64faf662764ede07be4475e/wt_dbass/AKWF_dbass_0001.wav"
            ),
            SoundCategory::Wavetable
        );
        // Case-insensitive against the declared prefix, the way VCSL is
        // spelled in caps on the CDN.
        assert_eq!(
            of("https://STRUDEL.B-CDN.NET/vcsl/Strings/x.wav"),
            SoundCategory::Orchestra
        );
        assert_eq!(of("http://127.0.0.1:9/a.wav"), SoundCategory::Other);
        assert_eq!(SoundCategory::of_location(None), SoundCategory::Other);

        // Every source with files declares a category, and every declared
        // category is a real shelf label.
        let pinned: PinnedFile = serde_json::from_str(PINNED_BANKS).expect("pin parses");
        for source in &pinned.sources {
            if source.base.is_some() {
                assert!(
                    source.category.is_some(),
                    "pin source {} declares no category",
                    source.name
                );
            }
        }
        assert_eq!(pinned.inline.category, Some(SoundCategory::Dirt));

        let library = SampleLibrary::empty();
        assert!(
            library
                .catalogue()
                .iter()
                .all(|entry| entry.category == SoundCategory::Synth)
        );
    }

    #[test]
    fn native_synth_preloads_need_no_files_and_emit_no_failure() {
        let library = SampleLibrary::empty();
        let preloads = [
            "triangle", "supersaw", "pulse", "sbd", "white", "pink", "sawtooth",
        ]
        .map(str::to_owned);

        assert_eq!(
            async_register(&library, Vec::new(), &preloads).expect("native synth preloads"),
            PrefetchStatus::Requested(0)
        );
        assert!(library.take_failures().is_empty());
    }

    /// A fetched map's own `_base` is the base for its entries, unless the
    /// source that named the map gave one. The folder the map came from is
    /// only the fallback. Shabda's maps depend on this.
    #[test]
    fn a_fetched_maps_own_base_outranks_the_folder_it_came_from() {
        // Served from `/deep/`, but declaring samples at the root - the
        // shape every shabda answer has.
        let declared = "http://127.0.0.1:9/declared/";
        let body = format!(r#"{{"_base":"{declared}","tone":"tone.wav"}}"#);
        let (url, arrival, server) = serve_once("/deep/map.json", body);

        let library = SampleLibrary::empty();
        async_register(
            &library,
            vec![(serde_json::to_string(&url).unwrap(), None)],
            &[],
        )
        .expect("manifest job");
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the manifest was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("manifest server");

        let banks = library.custom.read().expect("custom banks");
        let Some(Bank::Array(urls)) = banks.get("tone") else {
            panic!("the manifest's bank was not registered: {:?}", banks.keys());
        };
        assert_eq!(
            urls[0].as_ref(),
            format!("{declared}tone.wav"),
            "the map's own _base is what its relative entries hang off"
        );
        assert!(
            !urls[0].contains("/deep/"),
            "the folder the manifest was fetched from is only the fallback"
        );
    }

    /// A source that answers with an empty map says so.
    ///
    /// Shabda returns `{"_base": "…"}` and HTTP 200 when a word finds
    /// nothing, so "it worked" and "there is nothing here" arrive looking
    /// identical. Registering the second as the first leaves you hunting a
    /// sound the browser never had.
    #[test]
    fn a_source_that_brings_no_sounds_is_reported() {
        let body = r#"{"_base":"https://example.com/"}"#.to_owned();
        let (url, arrival, server) = serve_once("/nothing.json", body);
        let origin = Url::parse(&url).unwrap().origin().ascii_serialization();

        let library = SampleLibrary::empty();
        library
            .register_custom_for_test(&origin, &serde_json::to_string(&url).unwrap())
            .expect("score registration");
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the manifest was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("manifest server");

        let failures = library.take_failures();
        assert!(
            failures.iter().any(|failure| failure.contains("no sounds")),
            "an empty map registered silently: {failures:?}"
        );
    }

    /// A base the score wrote outranks the map's own `_base`. The order is:
    /// the score's base, the map's `_base`, the folder the map came from.
    /// Upstream reads `baseUrl || json._base || base`, the same order.
    #[test]
    fn a_base_the_score_wrote_outranks_the_maps_own() {
        let body = r#"{"_base":"http://127.0.0.1:9/declared/","tone":"tone.wav"}"#.to_owned();
        let (url, arrival, server) = serve_once("/deep/map.json", body);

        let library = SampleLibrary::empty();
        let asked = "http://127.0.0.1:9/asked/";
        async_register(
            &library,
            vec![(serde_json::to_string(&url).unwrap(), Some(asked.to_owned()))],
            &[],
        )
        .expect("manifest job");
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the manifest was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("manifest server");

        let banks = library.custom.read().expect("custom banks");
        let Some(Bank::Array(urls)) = banks.get("tone") else {
            panic!("the manifest's bank was not registered: {:?}", banks.keys());
        };
        assert_eq!(
            urls[0].as_ref(),
            format!("{asked}tone.wav"),
            "the base the score named is the one its samples hang off"
        );
    }

    /// The same three-way order on the score path - the one `samples('…')`
    /// in a score, and every Settings pack, actually take.
    #[test]
    fn the_score_path_honours_a_fetched_maps_base_too() {
        let declared = "http://127.0.0.1:9/declared/";
        let body = format!(r#"{{"_base":"{declared}","tone":"tone.wav"}}"#);
        let (url, arrival, server) = serve_once("/deep/map.json", body);
        let origin = Url::parse(&url).unwrap().origin().ascii_serialization();
        let library = SampleLibrary::empty();
        let mut access = ScoreSampleAccess::denied();
        access.permit_origin(&origin).expect("origin");
        access
            .permit_origin("http://127.0.0.1:9")
            .expect("declared host");
        library
            .register_score_custom(&serde_json::to_string(&url).unwrap(), None, &access)
            .expect("score registration");
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the manifest was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("manifest server");
        let banks = library.custom.read().expect("custom banks");
        let Some(Bank::Array(urls)) = banks.get("tone") else {
            panic!("not registered: {:?}", banks.keys());
        };
        assert_eq!(urls[0].as_ref(), format!("{declared}tone.wav"));
    }

    /// A map with no `_base` uses the folder it was fetched from as its
    /// base.
    #[test]
    fn a_fetched_map_without_a_base_still_uses_its_own_folder() {
        let body = r#"{"tone":"tone.wav"}"#.to_owned();
        let (url, arrival, server) = serve_once("/deep/map.json", body);

        let library = SampleLibrary::empty();
        async_register(
            &library,
            vec![(serde_json::to_string(&url).unwrap(), None)],
            &[],
        )
        .expect("manifest job");
        arrival
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the manifest was asked for");
        library.wait_until_idle(Duration::from_secs(2));
        server.join().expect("manifest server");

        let banks = library.custom.read().expect("custom banks");
        let Some(Bank::Array(urls)) = banks.get("tone") else {
            panic!("the manifest's bank was not registered: {:?}", banks.keys());
        };
        let folder = url.rsplit_once('/').map(|(folder, _)| folder).unwrap();
        assert_eq!(urls[0].as_ref(), format!("{folder}/tone.wav"));
    }

    #[test]
    fn preload_waits_for_default_then_uses_its_own_override() {
        let library = SampleLibrary::empty();
        let root = test_dir("default-preload-order");
        let manifest_path = root.join("default.json");
        let old = "http://127.0.0.1:9/default.wav";
        let body = inline_bank("tone", old);
        std::fs::write(&manifest_path, &body).expect("default fixture");
        let source = PinnedSource {
            name: "fixture".to_owned(),
            url: format!("file://{}", manifest_path.display()),
            base: Some(String::new()),
            sha256: digest(body.as_bytes()),
            category: None,
        };
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Defaults);
        library
            .enqueue_manifest_work_async(ManifestWork::Defaults {
                sources: vec![source],
                cache_dir: root.join("cache"),
            })
            .expect("default job");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("default ready to publish");

        let new = "http://127.0.0.1:9/new.wav";
        assert_eq!(
            async_register(
                &library,
                vec![(inline_bank("tone", new), None)],
                &["tone".to_owned()],
            )
            .expect("override and preload"),
            PrefetchStatus::Deferred
        );
        assert!(
            library.shared.by_url.read().unwrap().is_empty(),
            "preload ran before its owning override"
        );

        release.send(()).unwrap();
        library.wait_until_idle(Duration::from_secs(2));
        assert!(library.shared.by_url.read().unwrap().contains_key(new));
        assert!(
            !library.shared.by_url.read().unwrap().contains_key(old),
            "preload warmed the default URL that its owning job replaced"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn preload_owned_by_the_second_override_never_uses_the_first() {
        let library = SampleLibrary::empty();
        let first = "http://127.0.0.1:9/first.wav";
        let second = "http://127.0.0.1:9/second.wav";
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        async_register(&library, vec![(inline_bank("tone", first), None)], &[])
            .expect("first override");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("first override ready to publish");
        async_register(
            &library,
            vec![(inline_bank("tone", second), None)],
            &["tone".to_owned()],
        )
        .expect("second override and preload");
        release.send(()).expect("release first override");
        library.wait_until_idle(Duration::from_secs(2));
        let urls = library.shared.by_url.read().unwrap();
        assert!(urls.contains_key(second));
        assert!(
            !urls.contains_key(first),
            "the later evaluation's preload warmed an earlier override"
        );
    }

    #[test]
    fn idle_wait_includes_a_sample_load_started_by_manifest_completion() {
        let sample_listener = TcpListener::bind("127.0.0.1:0").expect("bind sample server");
        // The trusted audio cache persists across self-hosted CI jobs. A bare
        // ephemeral-port URL can therefore consume stale invalid bytes and
        // skip this fixture server entirely when the OS reuses the port.
        let unique_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sample_url = format!(
            "http://{}/tone-{}-{unique_time}.wav",
            sample_listener.local_addr().unwrap(),
            std::process::id()
        );
        let (sample_entered, sample_entered_rx) = mpsc::sync_channel(1);
        let (sample_release, sample_release_rx) = mpsc::sync_channel(1);
        let sample_server = std::thread::spawn(move || {
            let (stream, _) = sample_listener.accept().expect("sample request");
            let stream = drain_request(stream);
            sample_entered.send(()).expect("announce sample request");
            sample_release_rx.recv().expect("release sample response");
            // Empty PCM fails decoding after the network work settles; either
            // Ready or Failed is idle, while the stalled request is not.
            respond(stream, "");
        });

        let (manifest_url, manifest_entered, manifest_release, manifest_server) =
            stalled_server(format!(r#"{{"tone":"{sample_url}"}}"#));
        let library = SampleLibrary::empty();
        async_register(
            &library,
            vec![(serde_json::to_string(&manifest_url).unwrap(), None)],
            &["tone".to_owned()],
        )
        .expect("manifest job");
        manifest_entered
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("manifest request");
        manifest_release.send(()).unwrap();
        manifest_server.join().unwrap();
        sample_entered_rx
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("deferred preload starts after manifest publication");

        let started = Instant::now();
        library.wait_until_idle(Duration::from_millis(70));
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "idle returned while the deferred sample response was stalled"
        );

        sample_release.send(()).unwrap();
        sample_server.join().unwrap();
        library.wait_until_idle(Duration::from_secs(1));
        assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
        assert!(matches!(
            library
                .shared
                .by_url
                .read()
                .unwrap()
                .get(sample_url.as_str()),
            Some(UrlState::Failed { .. })
        ));
    }

    #[test]
    fn the_loader_takes_what_plays_now_first_and_the_newest_bet_before_older_ones() {
        let queue = LoadQueue::new();
        let job = |name: &str| LoadJob {
            url: Arc::from(name),
            kind: LoadKind::Install {
                id: SampleId(0),
                decode_rate: DecodeRate::Native,
            },
        };
        queue.push(job("bet-a"), LoadPriority::Bet);
        queue.push(job("bet-b"), LoadPriority::Bet);
        queue.push(job("now-c"), LoadPriority::Now);
        queue.push(job("bet-d"), LoadPriority::Bet);
        // The first bet turns out to be needed: it joins the front line.
        queue.promote("bet-a");
        let order = std::iter::from_fn(|| queue.try_pop())
            .map(|job| job.url.to_string())
            .collect::<Vec<_>>();
        assert_eq!(order, ["now-c", "bet-a", "bet-d", "bet-b"]);
        queue.close();
        assert!(
            queue.pop().is_none(),
            "a closed, empty line ends the thread"
        );
        queue.push(job("late"), LoadPriority::Now);
        assert!(queue.try_pop().is_none(), "nothing joins a closed line");
    }

    #[test]
    fn a_failed_download_rests_then_a_real_ask_tries_again_but_a_bet_does_not() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(r#"{"bd":["http://127.0.0.1:9/a.wav"]}"#, None)
            .expect("bank");
        assert_eq!(library.readiness("bd", 0.0), SoundReadiness::Loading);
        library.wait_until_idle(Duration::from_secs(5));
        assert_eq!(library.readiness("bd", 0.0), SoundReadiness::Failed);
        // Needed now, but the failure is fresh: it is not knocked on again.
        assert!(matches!(
            SampleLookup::resolve(&library, "bd", 0.0, 36.0),
            SampleResolution::Failed
        ));
        // Once it has rested, a real ask tries again; a bet still does not.
        let url: Arc<str> = Arc::from("http://127.0.0.1:9/a.wav");
        library.shared.by_url.write().expect("table").insert(
            url,
            UrlState::Failed {
                at: Instant::now() - FAILED_RETRY_AFTER,
            },
        );
        assert_eq!(
            library.readiness("bd", 0.0),
            SoundReadiness::Failed,
            "a bet does not spend a retry"
        );
        assert!(
            matches!(
                SampleLookup::resolve(&library, "bd", 0.0, 36.0),
                SampleResolution::Loading
            ),
            "a real ask does"
        );
        library.wait_until_idle(Duration::from_secs(5));
        assert_eq!(library.readiness("bd", 0.0), SoundReadiness::Failed);
    }

    #[test]
    fn a_registered_map_is_a_source_the_score_can_be_held_to() {
        let library = SampleLibrary::empty();
        let map = r#"{"bd":["http://127.0.0.1:9/a.wav"]}"#;
        assert!(!library.knows_samples_source(map));
        library.register_trusted_custom(map, None).expect("map");
        assert!(
            library.knows_samples_source(map),
            "the map text, as registered"
        );
        // A score's string argument reaches the library stringified.
        library
            .shared
            .note_source_standing("\"github:me/mine\"", SourceState::Ready);
        assert!(library.knows_samples_source("github:me/mine"));
        assert!(!library.knows_samples_source("github:me/other"));
    }

    #[test]
    fn queued_preloads_are_count_and_byte_bounded_before_copying() {
        let exact = vec!["x".repeat(MAX_QUEUED_PRELOAD_BYTES)];
        let copied = copy_preload_specs(&exact).expect("exact byte boundary");
        assert_eq!(copied.len(), 1);
        assert_eq!(copied[0].len(), MAX_QUEUED_PRELOAD_BYTES);

        let over_bytes = vec![format!("{} y", exact[0])];
        let error = copy_preload_specs(&over_bytes).expect_err("byte cap");
        assert!(error.contains("session limit"), "{error}");

        let over_count = vec![vec!["x"; MAX_QUEUED_PRELOADS + 1].join(" ")];
        let error = copy_preload_specs(&over_count).expect_err("entry cap");
        assert!(error.contains("session limit"), "{error}");
    }

    #[test]
    fn manifest_payload_limits_include_effect_count_and_bases() {
        validate_manifest_effects((0..MAX_MANIFEST_EFFECTS).map(|_| ("{}", Option::<&str>::None)))
            .expect("exact effect boundary");
        let error = validate_manifest_effects(
            (0..=MAX_MANIFEST_EFFECTS).map(|_| ("{}", Option::<&str>::None)),
        )
        .expect_err("one effect above the boundary");
        assert!(error.contains("per-evaluation limit"), "{error}");

        let map = "x".repeat(MAX_MANIFEST_EFFECT_BYTES - 1);
        validate_manifest_effects(std::iter::once((map.as_str(), Some("x"))))
            .expect("map plus base at exact byte boundary");
        let error = validate_manifest_effects(std::iter::once((map.as_str(), Some("xx"))))
            .expect_err("base bytes count toward the boundary");
        assert!(error.contains("byte limit"), "{error}");
    }

    #[test]
    fn manifest_fifo_rejects_oversized_owned_batches_before_pending_charge() {
        let library = SampleLibrary::empty();
        let too_many = (0..=MAX_MANIFEST_EFFECTS)
            .map(|_| ("{}".to_owned(), None))
            .collect();
        let error = library
            .register_trusted_batch_async_for_test(too_many, &[])
            .expect_err("effect count bound");
        assert!(error.contains("per-evaluation limit"), "{error}");
        assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);

        let too_large = "x".repeat(MAX_MANIFEST_EFFECT_BYTES + 1);
        let error = library
            .register_trusted_batch_async_for_test(vec![(too_large, None)], &[])
            .expect_err("effect byte bound");
        assert!(error.contains("byte limit"), "{error}");
        assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn async_manifest_json_is_parsed_only_after_enqueue() {
        let library = SampleLibrary::empty();
        async_register(&library, vec![("{".to_owned(), None)], &[])
            .expect("producer performs only bounded shape checks");
        library.wait_until_idle(Duration::from_secs(1));
        assert!(
            library
                .take_failures()
                .iter()
                .any(|failure| failure.contains("does not parse"))
        );
    }

    /// Registrations behind a fetch in flight wait in line without holding
    /// up the producer, and land in the order they were made: the last
    /// registration of a name wins.
    #[test]
    fn a_busy_manifest_loader_parks_without_blocking_the_producer() {
        let (url, entered, release, server) = stalled_server("{}".to_owned());
        let library = SampleLibrary::empty();
        async_register(
            &library,
            vec![(serde_json::to_string(&url).unwrap(), None)],
            &[],
        )
        .expect("active job");
        entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");
        let started = Instant::now();
        for (name, file) in [
            ("queued_one", "one"),
            ("queued_two", "first"),
            ("queued_three", "three"),
            ("queued_two", "last"),
        ] {
            async_register(
                &library,
                vec![(
                    inline_bank(name, &format!("http://127.0.0.1:9/{file}.wav")),
                    None,
                )],
                &[],
            )
            .expect("a busy loader parks the job");
        }
        assert!(started.elapsed() < Duration::from_millis(500));

        release.send(()).unwrap();
        server.join().unwrap();
        library.wait_until_idle(Duration::from_secs(2));
        for name in ["queued_one", "queued_two", "queued_three"] {
            assert!(library.knows(name), "{name}");
        }
        let custom = library.custom.read().unwrap();
        assert_eq!(
            bank_file_urls(&custom["queued_two"])
                .iter()
                .map(|url| &***url)
                .collect::<Vec<_>>(),
            ["http://127.0.0.1:9/last.wav"]
        );
    }

    #[test]
    fn one_deadline_covers_recursion_and_every_effect_in_the_batch() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopping) = mpsc::sync_channel(1);
        let nested = serde_json::to_string(&format!("{origin}/second.json")).unwrap();
        let server = std::thread::spawn(move || -> Result<usize, String> {
            let mut requests = 0usize;
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        requests += 1;
                        let stream = drain_request(stream);
                        match requests {
                            1 => respond(stream, &nested),
                            2 => {
                                // Consume the complete batch deadline without
                                // answering. Keeping the listener alive means
                                // a later effect can still connect and be
                                // counted if it receives a fresh budget.
                                std::thread::sleep(Duration::from_millis(1_200));
                                drop(stream);
                            }
                            _ => respond(stream, "{}"),
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopping.try_recv().is_ok() {
                            return Ok(requests);
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        });

        let library = SampleLibrary::empty();
        library
            .enqueue_manifest_work_for(
                ManifestWork::Custom {
                    effects: vec![
                        (
                            serde_json::to_string(&format!("{origin}/first.json")).unwrap(),
                            None,
                        ),
                        (
                            serde_json::to_string(&format!("{origin}/third.json")).unwrap(),
                            None,
                        ),
                    ],
                    preloads: Vec::new(),
                    intent: PrefetchIntent::Explicit,
                    access: ManifestAccess::Trusted,
                    continue_on_error: true,
                    layer: BankLayer::Score,
                },
                // Long enough that two loopback connections are certain even
                // on a machine running the rest of the suite beside this one,
                // and short enough that the stall above outlasts it: what the
                // test reads is which effects got to ask, so the budget has to
                // be generous for the ones that should and spent for the one
                // that should not. At 70 ms a loaded machine spent it before
                // the first connection and the server saw nothing at all.
                // 500 ms was still spent before the RECURSION connected once
                // this module grew two more tests beside it, which read as one
                // request instead of two. The ceiling is the 1_200 ms stall:
                // the budget has to expire inside it, or the later effect gets
                // to ask and the point of the test is lost.
                Duration::from_millis(900),
            )
            .expect("enqueue batch");
        library.wait_until_idle(Duration::from_secs(5));
        let failures = library.take_failures();
        let _ = stop.send(());
        let requests = server
            .join()
            .expect("manifest server thread")
            .expect("manifest server");
        assert_eq!(requests, 2, "the later effect received a fresh timeout");
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("deadline exceeded")),
            "the aggregate deadline was not surfaced"
        );
    }

    #[test]
    fn dropping_a_library_is_nonblocking_and_prevents_active_or_queued_commits() {
        let active = r#"{"active":"http://127.0.0.1:9/active.wav"}"#.to_owned();
        let (url, entered, release, server) = stalled_server(active);
        let library = SampleLibrary::empty();
        async_register(
            &library,
            vec![(serde_json::to_string(&url).unwrap(), None)],
            &[],
        )
        .expect("active job");
        entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");
        async_register(
            &library,
            vec![(r#"{"late":"http://127.0.0.1:9/late.wav"}"#.to_owned(), None)],
            &[],
        )
        .expect("queued job");
        let custom = Arc::clone(&library.custom);
        let shared = Arc::clone(&library.shared);
        let dropped = Instant::now();
        drop(library);
        assert!(
            dropped.elapsed() < Duration::from_millis(500),
            "Drop waited for an active HTTP response"
        );

        // Ureq cannot interrupt the active read. Releasing it lets the worker
        // observe cancellation after I/O; neither those bytes nor the queued
        // job may publish anything.
        release.send(()).unwrap();
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.manifest_pending.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shared.manifest_pending.load(Ordering::Acquire), 0);
        let custom = custom.read().unwrap();
        assert!(!custom.contains_key("active"));
        assert!(!custom.contains_key("late"));
        drop(custom);
        assert!(
            shared
                .failures
                .lock()
                .unwrap()
                .iter()
                .any(|failure| failure.message.contains("cancelled"))
        );
    }

    /// A job's time starts when the worker takes it: a wait in line longer
    /// than the job's whole budget costs it nothing.
    #[test]
    fn a_queued_job_budget_starts_when_the_worker_takes_it() {
        let library = SampleLibrary::empty();
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        library
            .enqueue_manifest_work_for(
                ManifestWork::Custom {
                    effects: vec![(inline_bank("first", "http://127.0.0.1:9/first.wav"), None)],
                    preloads: Vec::new(),
                    intent: PrefetchIntent::Explicit,
                    access: ManifestAccess::Trusted,
                    continue_on_error: true,
                    layer: BankLayer::Score,
                },
                Duration::from_secs(30),
            )
            .expect("first job");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("first job reached publication");
        library
            .enqueue_manifest_work_for(
                ManifestWork::Custom {
                    effects: vec![(inline_bank("waited", "http://127.0.0.1:9/waited.wav"), None)],
                    preloads: Vec::new(),
                    intent: PrefetchIntent::Explicit,
                    access: ManifestAccess::Trusted,
                    continue_on_error: true,
                    layer: BankLayer::Score,
                },
                Duration::from_secs(1),
            )
            .expect("queued job");
        std::thread::sleep(Duration::from_millis(1_200));
        release.send(()).expect("release first job");
        library.wait_until_idle(Duration::from_secs(5));
        assert!(library.knows("first"));
        assert!(library.knows("waited"), "the wait in line spent its budget");
        assert!(
            library
                .take_failures()
                .iter()
                .all(|failure| !failure.contains("deadline exceeded")),
        );
    }

    /// A map whose load spends the batch's budget leaves no later map of the
    /// batch reading loading with no job behind it: each reads failed, as a
    /// retry and anything waiting on it can see.
    #[test]
    fn a_spent_batch_fails_every_map_it_never_reached() {
        let library = SampleLibrary::empty();
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        let first = inline_bank("first", "http://127.0.0.1:9/first.wav");
        let later = inline_bank("later", "http://127.0.0.1:9/later.wav");
        for map in [&first, &later] {
            library.mark_source_loading(map);
        }
        library
            .enqueue_manifest_work_for(
                ManifestWork::Custom {
                    effects: vec![(first.clone(), None), (later.clone(), None)],
                    preloads: Vec::new(),
                    intent: PrefetchIntent::Explicit,
                    access: ManifestAccess::Trusted,
                    continue_on_error: true,
                    layer: BankLayer::Score,
                },
                Duration::from_secs(1),
            )
            .expect("the batch");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("the first map reached publication");
        std::thread::sleep(Duration::from_millis(1_200));
        release.send(()).expect("release the first map");
        library.wait_until_idle(Duration::from_secs(5));
        for map in [&first, &later] {
            let state = library.samples_source_state(map);
            assert!(
                matches!(&state, Some(SourceState::Failed(reason)) if reason.contains("deadline exceeded")),
                "{map}: {state:?}"
            );
        }
        assert!(!library.knows("later"));
    }

    #[test]
    fn drop_wins_at_custom_map_publication() {
        let library = SampleLibrary::empty();
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        async_register(
            &library,
            vec![(inline_bank("late", "http://127.0.0.1:9/late.wav"), None)],
            &[],
        )
        .expect("job");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("map ready to publish");
        let custom = Arc::clone(&library.custom);
        let shared = Arc::clone(&library.shared);
        drop(library);
        release.send(()).expect("release publication");
        wait_for_manifest_jobs(&shared);
        assert!(!custom.read().unwrap().contains_key("late"));
    }

    #[test]
    fn drop_wins_at_default_map_publication() {
        let root = test_dir("default-publication");
        let manifest = root.join("default.json");
        let body = inline_bank("late_default", "http://127.0.0.1:9/default.wav");
        std::fs::write(&manifest, &body).expect("manifest fixture");
        let library = SampleLibrary::empty();
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Defaults);
        library
            .enqueue_manifest_work_async(ManifestWork::Defaults {
                sources: vec![PinnedSource {
                    name: "fixture".to_owned(),
                    url: format!("file://{}", manifest.display()),
                    base: Some(String::new()),
                    sha256: digest(body.as_bytes()),
                    category: None,
                }],
                cache_dir: root.join("cache"),
            })
            .expect("default job");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("defaults ready to publish");
        let banks = Arc::clone(&library.banks);
        let shared = Arc::clone(&library.shared);
        drop(library);
        release.send(()).expect("release publication");
        wait_for_manifest_jobs(&shared);
        assert!(!banks.read().unwrap().contains_key("late_default"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_wins_at_local_map_publication() {
        let root = test_dir("local-publication");
        std::fs::create_dir_all(root.join("kit")).expect("sample folder");
        std::fs::write(root.join("kit/hit.wav"), b"RIFF").expect("sample fixture");
        let library = SampleLibrary::empty();
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Local);
        async_register(
            &library,
            vec![(
                serde_json::to_string(&format!("local:{}", root.display())).unwrap(),
                None,
            )],
            &[],
        )
        .expect("local job");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("local map ready to publish");
        let custom = Arc::clone(&library.custom);
        let shared = Arc::clone(&library.shared);
        drop(library);
        release.send(()).expect("release publication");
        wait_for_manifest_jobs(&shared);
        assert!(!custom.read().unwrap().contains_key("kit"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A host-trusted local folder must publish even while the pinned
    /// default maps still own the manifest worker. Queuing it behind that
    /// fetch is what made studio e2e die with "sample manifest deadline
    /// exceeded" on a slow first GitHub round-trip.
    #[test]
    fn trusted_local_folder_does_not_wait_behind_default_manifests() {
        let root = test_dir("local-ahead-of-defaults");
        let samples = root.join("samples");
        std::fs::create_dir_all(samples.join("bd")).expect("bank");
        std::fs::write(samples.join("bd/hit.wav"), b"RIFF").expect("sample");
        let manifest = root.join("default.json");
        let body = inline_bank("late_default", "http://127.0.0.1:9/default.wav");
        std::fs::write(&manifest, &body).expect("manifest fixture");

        let library = SampleLibrary::empty();
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Defaults);
        library
            .enqueue_manifest_work_async(ManifestWork::Defaults {
                sources: vec![PinnedSource {
                    name: "fixture".to_owned(),
                    url: format!("file://{}", manifest.display()),
                    base: Some(String::new()),
                    sha256: digest(body.as_bytes()),
                    category: None,
                }],
                cache_dir: root.join("cache"),
            })
            .expect("default job");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("defaults at publication");

        let map = serde_json::to_string(&format!("local:{}", samples.display())).unwrap();
        let (done, done_rx) = mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = done.send(library.register_trusted_custom(&map, None));
            });
            let registered = done_rx.recv_timeout(Duration::from_secs(2));
            release.send(()).expect("release defaults");
            registered
                .expect("local folder waited behind the default manifest job")
                .expect("local folder");
        });
        assert!(
            library.knows("bd"),
            "the local bank must be published while defaults are still in flight"
        );
        library.wait_until_idle(Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_wins_after_cache_staging_before_final_rename() {
        let (url, _entered, release_response, server) =
            stalled_server(r#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_owned());
        let root = test_dir("cache-publication");
        let library = SampleLibrary::empty();
        let (reached, release_publication) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Cache);
        let publication = Arc::clone(&library.shared.publication);
        let budget = library.manifest_budget(Duration::from_secs(2));
        let worker_root = root.clone();
        let worker_url = url.clone();
        let fetch = std::thread::spawn(move || {
            fetch_manifest_cached_with_budget(&worker_root, &worker_url, &budget, &publication)
        });
        release_response.send(()).expect("release response");
        server.join().expect("server");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("cache staged and synced");
        let destination = cache_path(&root, &url);
        drop(library);
        release_publication.send(()).expect("release publication");
        let error = fetch
            .join()
            .expect("cache worker")
            .expect_err("cancelled cache cannot publish");
        assert!(error.contains("cancelled"), "{error}");
        assert!(!destination.exists(), "final cache appeared after Drop");
        assert_eq!(
            std::fs::read_dir(&root).expect("cache directory").count(),
            0,
            "staged cache file was not cleaned up"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_wins_after_sample_cache_staging_before_final_rename() {
        let (url, _entered, release_response, server) = stalled_server("sample".to_owned());
        let root = test_dir("sample-cache-publication");
        let library = SampleLibrary::empty();
        let (reached, release_publication) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Cache);
        let publication = Arc::clone(&library.shared.publication);
        let budget =
            sample_fetch::FetchBudget::for_one_fetch_with_cancellation(publication.cancellation());
        let worker_root = root.clone();
        let worker_url = url.clone();
        let fetch = std::thread::spawn(move || {
            fetch_cached_with_budget(&worker_root, &worker_url, &budget, &publication)
        });
        release_response.send(()).expect("release response");
        server.join().expect("server");
        reached
            .recv_timeout(Duration::from_secs(5))
            .expect("sample cache staged and synced");
        let destination = cache_path(&root, &url);
        drop(library);
        release_publication.send(()).expect("release publication");
        let error = fetch
            .join()
            .expect("sample cache worker")
            .expect_err("cancelled sample cache cannot publish");
        assert!(error.contains("cancelled"), "{error}");
        assert!(
            !destination.exists(),
            "final sample cache appeared after Drop"
        );
        assert_eq!(
            std::fs::read_dir(&root).expect("cache directory").count(),
            0,
            "staged sample cache file was not cleaned up"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_wins_after_score_cache_staging_before_final_rename() {
        let root = test_dir("score-cache-publication");
        let library = SampleLibrary::empty();
        let (reached, release_publication) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Cache);
        let publication = Arc::clone(&library.shared.publication);
        let budget =
            sample_fetch::FetchBudget::for_one_fetch_with_cancellation(publication.cancellation());
        let cache = Arc::new(ScoreCache::new(root.clone()));
        let destination = cache.path(
            "https://samples.example/late.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        let worker_cache = Arc::clone(&cache);
        let worker_destination = destination.clone();
        let writer = std::thread::spawn(move || {
            worker_cache.admit_with_publication(
                &worker_destination,
                b"complete sample bytes",
                &publication,
                &budget,
            )
        });

        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("score cache staged and synced");
        drop(library);
        release_publication.send(()).expect("release publication");
        let error = writer
            .join()
            .expect("score cache writer")
            .expect_err("cancelled score cache cannot publish");
        assert!(error.contains("cancelled"), "{error}");
        assert!(
            !destination.exists(),
            "final score cache appeared after Drop"
        );
        assert!(
            std::fs::read_dir(cache.dir())
                .expect("score cache directory")
                .all(|entry| !is_score_cache_staging(&entry.expect("entry").path())),
            "staged score cache file was not cleaned up"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_before_deferred_preload_prevents_a_loader_job() {
        let library = SampleLibrary::empty();
        let sample = "http://127.0.0.1:9/never.wav";
        let (reached, release) = library
            .shared
            .publication
            .install_test_barrier(PublicationKind::Preload);
        async_register(
            &library,
            vec![(inline_bank("tone", sample), None)],
            &["tone".to_owned()],
        )
        .expect("manifest and preload");
        reached
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("preload ready to start");
        let shared = Arc::clone(&library.shared);
        assert!(shared.by_url.read().unwrap().is_empty());
        drop(library);
        release.send(()).expect("release preload");
        wait_for_manifest_jobs(&shared);
        assert!(
            shared.by_url.read().unwrap().is_empty(),
            "deferred preload started after cancellation"
        );
    }

    #[test]
    fn public_and_session_default_constructors_keep_distinct_readiness_contracts() {
        let body = inline_bank("tone", "http://127.0.0.1:9/tone.wav");
        let (url, entered, release, server) = stalled_server(body.clone());
        let pinned = serde_json::json!({
            "sources": [{
                "name": "fixture",
                "url": url,
                "base": "",
                "sha256": digest(body.as_bytes()),
            }],
            "inline": { "base": "", "banks": {} },
        })
        .to_string();
        let fonts = r#"{"base":"","fonts":{}}"#.to_owned();
        let root = test_dir("blocking-default");
        let (returned, returned_rx) = mpsc::sync_channel(1);
        let blocking_root = root.clone();
        let load = std::thread::spawn(move || {
            returned
                .send(SampleLibrary::load_default_from(
                    &pinned,
                    &fonts,
                    false,
                    blocking_root,
                ))
                .expect("return load result");
        });
        entered
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("default request");
        assert!(
            returned_rx.recv_timeout(Duration::from_millis(40)).is_err(),
            "public load_default returned before defaults were ready"
        );
        release.send(()).expect("release defaults");
        server.join().expect("server");
        let library = returned_rx
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("load returned")
            .expect("load succeeded");
        load.join().expect("load thread");
        assert!(library.knows("tone"));
        assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
        drop(library);
        let _ = std::fs::remove_dir_all(root);
        let async_body = inline_bank("later", "http://127.0.0.1:9/later.wav");
        let (url, entered, release, server) = stalled_server(async_body.clone());
        let pinned = serde_json::json!({
            "sources": [{
                "name": "fixture",
                "url": url,
                "base": "",
                "sha256": digest(async_body.as_bytes()),
            }],
            "inline": { "base": "", "banks": {} },
        })
        .to_string();
        let root = test_dir("async-default");
        let started = Instant::now();
        let library = SampleLibrary::load_default_from(
            &pinned,
            r#"{"base":"","fonts":{}}"#,
            true,
            root.clone(),
        )
        .expect("asynchronous default library");
        assert!(started.elapsed() < Duration::from_millis(500));
        entered
            .recv_timeout(ARRIVAL_TIMEOUT)
            .expect("async default request");
        assert!(!library.knows("later"));
        assert_ne!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
        release.send(()).expect("release async defaults");
        server.join().expect("server");
        library.wait_until_idle(Duration::from_secs(1));
        assert!(library.knows("later"));
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod base_url_tests {
    use super::{
        GITHUB_SAMPLE_MANIFEST, SHORTHANDS, SampleSource, base_url, expand_base, github_path,
        read_source,
    };

    /// Every shorthand goes through the one table, and a spelling that
    /// expands into another shorthand is followed.
    #[test]
    fn the_table_is_the_only_place_a_shorthand_is_known() {
        let url = |source: &str| match read_source(source, GITHUB_SAMPLE_MANIFEST) {
            SampleSource::Url(url) => url,
            SampleSource::LocalFolder(rest) => panic!("{source} read as the folder {rest:?}"),
        };
        assert_eq!(
            url("github:user/repo"),
            "https://raw.githubusercontent.com/user/repo/main/strudel.json"
        );
        // bubo: is a spelling of github:, and the walk follows it.
        assert_eq!(url("bubo:drum"), url("github:Bubobubobubobubo/dough-drum"));
        // A URL is not a shorthand, and neither is an unknown prefix: both
        // are handed back to be fetched, not rewritten.
        assert_eq!(
            url("https://example.com/map.json"),
            "https://example.com/map.json"
        );
        assert_eq!(url("nosuch:thing"), "nosuch:thing");
        assert!(matches!(
            read_source("local: kit ", GITHUB_SAMPLE_MANIFEST),
            SampleSource::LocalFolder(rest) if rest == " kit "
        ));
        // A base wants the folder, not the manifest, and a local base is
        // a prefix rather than a folder to scan.
        assert_eq!(
            expand_base("github:user/repo"),
            "https://raw.githubusercontent.com/user/repo/main/"
        );
        assert_eq!(expand_base("local:kit"), "local:kit");

        // shabda finds sounds by word. The plain form substitutes; nothing
        // is escaped, because the comma and the colon in `bass:2,kick` are
        // the service's own grammar.
        assert_eq!(
            url("shabda:bass,kick"),
            "https://shabda.ndre.gr/bass,kick.json?strudel=1"
        );

        // The speech form has a grammar instead: `[/language/gender]:words`,
        // both optional, language first.
        assert_eq!(
            url("shabda/speech/en-US/m:music,vocode"),
            "https://shabda.ndre.gr/speech/music,vocode.json\
             ?gender=m&language=en-US&strudel=1"
        );
        assert_eq!(
            url("shabda/speech:hello"),
            "https://shabda.ndre.gr/speech/hello.json?gender=f&language=en-GB&strudel=1",
            "no parameters is the row's own default voice"
        );
        assert_eq!(
            url("shabda/speech/de-DE:wort"),
            "https://shabda.ndre.gr/speech/wort.json?gender=f&language=de-DE&strudel=1",
            "a language with no gender beside it keeps the default gender; \
             upstream sends the word `undefined` here and gets another file"
        );
        // Only the first colon divides settings from words, so a count
        // stays with the word it belongs to.
        assert_eq!(
            url("shabda/speech:hello:2"),
            "https://shabda.ndre.gr/speech/hello:2.json?gender=f&language=en-GB&strudel=1"
        );
        // The boundary is checked, so a longer word beginning with the
        // prefix is not read as the prefix and a stray language.
        assert_eq!(url("shabda/speechify:x"), "shabda/speechify:x");

        assert_eq!(SHORTHANDS.len(), 5, "a new shorthand is a row, and a test");
    }

    #[test]
    fn github_shorthand_keeps_the_standard_manifest_name() {
        assert_eq!(GITHUB_SAMPLE_MANIFEST, "strudel.json");
        assert_eq!(
            github_path("user/repo", GITHUB_SAMPLE_MANIFEST),
            "https://raw.githubusercontent.com/user/repo/main/strudel.json"
        );
    }

    #[test]
    fn a_bare_origin_is_its_own_directory() {
        // What `samples('http://localhost:5432')` and the sample server give.
        assert_eq!(base_url("http://localhost:5432"), "http://localhost:5432/");
        assert_eq!(base_url("http://127.0.0.1:5432"), "http://127.0.0.1:5432/");
    }

    #[test]
    fn a_map_url_resolves_against_its_own_folder() {
        assert_eq!(
            base_url("https://example.com/kit/strudel.json"),
            "https://example.com/kit/"
        );
        assert_eq!(
            base_url("https://example.com/strudel.json"),
            "https://example.com/"
        );
        assert_eq!(
            base_url("https://example.com/kit/"),
            "https://example.com/kit/"
        );
    }
}

#[cfg(test)]
mod score_access_tests {
    use super::*;

    fn test_cache_base(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "rustel-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn empty_library() -> SampleLibrary {
        SampleLibrary::with_background_loaders_at(
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            String::new(),
            test_cache_base("score-test-cache"),
            Loading::InBackground,
        )
        .expect("empty score-access library")
    }

    fn set_folder(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp set folder");
        for file in files {
            let path = dir.path().join(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("sample folder");
            }
            // The scan reads names, not audio: an extension is the whole
            // of what makes a file a sample to it.
            std::fs::write(&path, b"").expect("sample file");
        }
        dir
    }

    /// The set's own folder is a sample source with nothing written and no
    /// grant passed, and it names what it finds the way a player would say
    /// it: a folder is a bank of variants, loose root files share one compact
    /// bank named after their folder while their filenames remain playable,
    /// and what the studio itself wrote into the set is not an instrument.
    #[test]
    fn a_set_folder_is_adopted_under_the_names_a_player_would_type() {
        let dir = set_folder(&[
            "deep bass.wav",
            "kicks/1.wav",
            "kicks/2.wav",
            "sessions/take 1.wav",
            "exports/drums-2026-08-25T18-04-11.wav",
            "song.strudel",
        ]);
        let library = empty_library();
        assert_eq!(library.adopt_set_folder(dir.path()), Ok(2));

        let catalogue = library.catalogue();
        let named = |name: &str| catalogue.iter().find(|entry| entry.name == name);
        let root_name = dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temporary folder name");
        let loose = named(root_name).expect("loose files share the root bank");
        assert_eq!(loose.variants, 1);
        assert_eq!(loose.variant_names, ["deep bass"]);
        assert_eq!(loose.origin, SoundOrigin::Set);
        assert_eq!(loose.category, SoundCategory::Set);
        // Being named is half of it. The URL has to be the one the loader
        // opens, and a name the filesystem spells with a space in it is
        // exactly where a percent-encoding would quietly lose the file.
        let location = loose.location.clone().expect("a location");
        assert!(
            fetch_located(&location).is_ok(),
            "the set's own sample reads back through the loader: {location}"
        );
        assert_eq!(
            named("kicks").map(|entry| entry.variants),
            Some(2),
            "a folder is one bank of variants"
        );
        assert!(
            named("take 1").is_none() && named("sessions").is_none(),
            "the set's own takes are not instruments"
        );
        assert!(
            named("drums-2026-08-25T18-04-11").is_none() && named("exports").is_none(),
            "and neither are its bounces"
        );
        assert!(library.knows("kicks"), "and the score can name it");
    }

    /// A set's samples belong to that set: opening another one takes them
    /// back out, or one set's `kicks` would play in the next. A folder that
    /// will not read is still another set, so it takes them out too.
    #[test]
    fn the_set_that_was_open_takes_its_samples_with_it() {
        let library = empty_library();
        let first = set_folder(&["kicks/1.wav"]);
        let second = set_folder(&["claps/1.wav"]);
        assert_eq!(library.adopt_set_folder(first.path()), Ok(1));
        assert!(library.knows("kicks"));
        assert_eq!(library.adopt_set_folder(second.path()), Ok(1));
        assert!(library.knows("claps"));
        assert!(!library.knows("kicks"), "the first set's bank went with it");

        // Leaving the set that was open does not depend on the next one
        // reading: a folder that will not open still closed the last.
        assert!(
            library
                .adopt_set_folder(&second.path().join("nope"))
                .is_err()
        );
        assert!(
            !library.knows("claps"),
            "a set that would not read still took the last one's banks with it"
        );
    }

    /// A failed rescan of the same set must not remove sounds already on stage.
    #[test]
    fn a_failed_set_refresh_keeps_the_current_samples() {
        let library = empty_library();
        let set = set_folder(&["kicks/1.wav"]);
        assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
        assert!(
            library
                .refresh_set_folder(&set.path().join("missing"))
                .is_err()
        );
        assert!(
            library.knows("kicks"),
            "the last good bank remains playable"
        );
    }

    /// A new bank that would exceed the session cap cannot evict the set's old
    /// bank or replace its variant list during a refresh.
    #[test]
    fn a_full_session_keeps_the_old_set_bank_when_refresh_exceeds_capacity() {
        let library = empty_library();
        let set = set_folder(&["kicks/1.wav"]);
        assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
        let limit = SAMPLE_BANK_CAPACITY - 1;
        {
            let mut custom = library.custom.write().expect("custom banks");
            for index in 0..limit - 1 {
                custom.insert(
                    format!("bank{index}"),
                    Bank::Array(vec![Arc::from("https://samples.example/a.wav")]),
                );
            }
        }
        std::fs::write(set.path().join("kicks/2.wav"), b"").expect("second variant");
        std::fs::create_dir(set.path().join("extra")).expect("new bank folder");
        std::fs::write(set.path().join("extra/1.wav"), b"").expect("new bank");
        let error = library
            .refresh_set_folder(set.path())
            .expect_err("the refresh would exceed capacity");
        assert!(error.contains("limit"), "{error}");
        assert!(!library.knows("extra"));
        assert_eq!(
            library
                .catalogue()
                .iter()
                .find(|entry| entry.name == "kicks")
                .map(|entry| entry.variants),
            Some(1),
            "the original set bank remains playable and unchanged"
        );
    }

    /// Re-reading the open set replaces its banks and their variant lists.
    #[test]
    fn a_set_refresh_picks_up_added_audio() {
        let library = empty_library();
        let set = set_folder(&["kicks/1.wav"]);
        assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
        std::fs::write(set.path().join("kicks/2.wav"), b"").expect("second variant");
        std::fs::create_dir(set.path().join("fresh")).expect("new bank folder");
        std::fs::write(set.path().join("fresh/1.wav"), b"").expect("new bank");
        assert_eq!(library.refresh_set_folder(set.path()), Ok(2));
        assert!(library.knows("fresh"));
        let catalogue = library.catalogue();
        assert_eq!(
            catalogue
                .iter()
                .find(|entry| entry.name == "kicks")
                .map(|entry| entry.variants),
            Some(2)
        );
    }

    /// A refresh leaves a name a score redefined with the score, so an update
    /// cannot change which `bd` plays.
    #[test]
    fn a_set_refresh_leaves_a_name_the_score_redefined_with_the_score() {
        let library = empty_library();
        let set = set_folder(&["bd/0.wav"]);
        assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://samples.example")
            .expect("origin");
        library
            .register_score_custom(r#"{"bd":"https://samples.example/bd.wav"}"#, None, &access)
            .expect("a granted origin");

        assert_eq!(library.refresh_set_folder(set.path()), Ok(0));
        let bd = library
            .catalogue()
            .into_iter()
            .find(|entry| entry.name == "bd")
            .expect("a bd bank");
        assert_eq!(
            bd.location.as_deref(),
            Some("https://samples.example/bd.wav")
        );
        assert_eq!(bd.origin, SoundOrigin::Score);
    }

    /// Most sets hold no audio at all, which is not a failure.
    #[test]
    fn a_set_folder_with_no_audio_in_it_adopts_nothing() {
        let dir = set_folder(&["song.strudel"]);
        let library = empty_library();
        assert_eq!(library.adopt_set_folder(dir.path()), Ok(0));
    }

    /// The `kicks` bank holds the same files in the same order whatever the
    /// set's folder is called. A loose `kicks.wav` belongs to the bank named
    /// after the folder and does not join `kicks`.
    #[test]
    fn a_merged_bank_orders_its_variants_the_same_way_in_every_set() {
        let variants = |set: &str| {
            let parent = tempfile::tempdir().expect("temp parent");
            let dir = parent.path().join(set);
            for file in ["kicks.wav", "kicks/1.wav", "kicks/2.wav"] {
                let path = dir.join(file);
                if let Some(folder) = path.parent() {
                    std::fs::create_dir_all(folder).expect("bank folder");
                }
                std::fs::write(&path, b"").expect("sample file");
            }
            let library = empty_library();
            library.adopt_set_folder(&dir).expect("adopted");
            let root = dir.canonicalize().expect("canonical set folder");
            // Use this platform's separator: `root.join(...)` builds the
            // URLs, and on Windows a prefix that ends in `/` matches nothing.
            let prefix = format!("{}{}", local_file_url(&root), std::path::MAIN_SEPARATOR);
            let custom = library.custom.read().expect("custom banks");
            let Bank::Array(urls) = custom.get("kicks").expect("a kicks bank") else {
                panic!("a folder of audio is an array bank");
            };
            // Relative to the set, so two sets named differently compare.
            urls.iter()
                .map(|url| url.strip_prefix(prefix.as_str()).unwrap_or(url).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            variants("aaa techno"),
            variants("zzz techno"),
            "the same audio in the same shape is the same bank whatever the set is called"
        );
        assert_eq!(
            variants("zzz techno").first().map(String::as_str),
            Some(format!("kicks{}1.wav", std::path::MAIN_SEPARATOR).as_str()),
            "the explicit folder bank keeps its own numbered files"
        );
    }

    /// A bank the set's folder takes a name from is the set's while that set
    /// is open - and the score's again at the next one, because the set only
    /// ever owned the folder it was handed, not the name.
    #[test]
    fn a_name_the_set_took_over_goes_back_to_the_score_that_held_it() {
        let library = empty_library();
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://samples.example")
            .expect("origin");
        library
            .register_score_custom(r#"{"bd":"https://samples.example/bd.wav"}"#, None, &access)
            .expect("a granted origin");
        let score_url = "https://samples.example/bd.wav";
        let location = |library: &SampleLibrary| {
            library
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == "bd")
                .expect("a bd bank")
        };
        assert_eq!(location(&library).location.as_deref(), Some(score_url));

        let set = set_folder(&["bd/0.wav"]);
        assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
        let taken = location(&library);
        assert!(
            taken.location.is_some_and(|url| url.starts_with("file://")),
            "the set's own bd is the one sounding while the set is open"
        );
        assert_eq!(taken.origin, SoundOrigin::Set, "and the browser says so");

        let next = set_folder(&["song.strudel"]);
        assert_eq!(library.adopt_set_folder(next.path()), Ok(0));
        let given_back = location(&library);
        assert_eq!(
            given_back.location.as_deref(),
            Some(score_url),
            "the score's bd came back with the set that took it"
        );
        assert_eq!(
            given_back.origin,
            SoundOrigin::Score,
            "and reads as the score's"
        );
    }

    /// And the other way round. A score that redefines a name the set's
    /// folder brought owns it from then on: the browser says whose it is,
    /// and the next set leaves it alone rather than putting the set's old
    /// bank back over the score's.
    #[test]
    fn a_score_that_redefines_a_set_bank_owns_the_name_from_then_on() {
        let library = empty_library();
        let set = set_folder(&["bd/0.wav"]);
        assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
        let bd = |library: &SampleLibrary| {
            library
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == "bd")
                .expect("a bd bank")
        };
        assert_eq!(bd(&library).origin, SoundOrigin::Set, "the set brought it");

        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://samples.example")
            .expect("origin");
        library
            .register_score_custom(r#"{"bd":"https://samples.example/bd.wav"}"#, None, &access)
            .expect("a granted origin");
        assert_eq!(
            bd(&library).origin,
            SoundOrigin::Score,
            "a score's redefinition is the score's, not the set's"
        );

        let next = set_folder(&["song.strudel"]);
        assert_eq!(library.adopt_set_folder(next.path()), Ok(0));
        assert_eq!(
            bd(&library).location.as_deref(),
            Some("https://samples.example/bd.wav"),
            "and the set that closed did not put its own bd back over it"
        );
    }

    /// The bank limit is the session's, not the set folder's: the message
    /// says which, and a name the set shares with a bank already registered
    /// replaces that bank rather than adding to the count.
    #[test]
    fn the_bank_limit_counts_what_the_session_holds() {
        let library = empty_library();
        let limit = SAMPLE_BANK_CAPACITY - 1;
        {
            let mut custom = library.custom.write().expect("custom banks");
            for index in 0..limit {
                custom.insert(
                    format!("bank{index}"),
                    Bank::Array(vec![Arc::from("https://samples.example/a.wav")]),
                );
            }
        }
        // At the limit, and the set's folder shares a name with a bank that
        // is already there. Replacing it adds nothing, so there is room.
        let shared = set_folder(&["bank0/0.wav"]);
        assert_eq!(library.adopt_set_folder(shared.path()), Ok(1));
        assert!(library.knows("bank0"), "the name is still a bank");

        // One past it, and the refusal names the session that is full rather
        // than the folder that holds one file.
        let error = library
            .adopt_set_folder(set_folder(&["extra/0.wav"]).path())
            .expect_err("a session at its limit has no room for another bank");
        assert!(
            error.contains("session") && !error.contains("folder"),
            "the limit is the session's, not the set folder's: {error}"
        );
    }

    #[test]
    fn score_sample_access_is_denied_until_the_host_grants_it() {
        let library = empty_library();
        let error = library
            .register_score_custom(
                r#"{"bank":"https://samples.example/kick.wav"}"#,
                None,
                &ScoreSampleAccess::denied(),
            )
            .expect_err("the default policy must not register a URL");
        assert!(error.contains("without a host grant"), "{error}");
        assert!(library.custom.read().expect("custom banks").is_empty());
        assert!(
            library
                .shared
                .score_sources
                .read()
                .expect("score sources")
                .is_empty()
        );
    }

    /// A score map is bounded by the files it may NAME, not by the decoded
    /// bank's slots: naming costs a grant, decoding happens per played sound.
    /// `github:bubobubobubobubo/dough-waveforms` names 4,358 wavetables
    /// across 65 banks; under the old 2,047 bound the whole map was refused
    /// and every `s("wt_…")` reached the voice as unknown.
    #[test]
    fn a_wavetable_library_larger_than_the_decoded_bank_registers() {
        fn library_map(banks: usize, files_per_bank: usize) -> String {
            let banks = (0..banks)
                .map(|bank| {
                    let files = (0..files_per_bank)
                        .map(|file| format!("\"wt_{bank:02}/AKWF_{file:04}.wav\""))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("\"wt_{bank:02}\":[{files}]")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{banks}}}")
        }
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://samples.example")
            .expect("origin");

        let library = empty_library();
        library
            .register_score_custom(
                &library_map(65, 68),
                Some("https://samples.example/dough/"),
                &access,
            )
            .expect("4,420 named files fit: none of them is decoded yet");
        assert_eq!(
            library
                .shared
                .score_sources
                .read()
                .expect("score sources")
                .len(),
            65 * 68
        );
        assert!(library.knows("wt_64"), "the last bank is there to play");

        let error = library
            .register_score_custom(
                &library_map(1, MAX_SCORE_SAMPLE_FILES + 1),
                Some("https://samples.example/huge/"),
                &access,
            )
            .expect_err("a map past the naming bound is still refused");
        assert!(
            error.contains(&format!("{MAX_SCORE_SAMPLE_FILES}-file limit")),
            "{error}"
        );
    }

    #[test]
    fn an_origin_grant_is_exact_and_applies_to_every_map_entry() {
        let library = empty_library();
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin("https://samples.example")
            .expect("origin");
        library
            .register_score_custom(
                r#"{"safe":["kick.wav","https://samples.example/snare.wav?take=2"]}"#,
                Some("https://samples.example/kit/"),
                &access,
            )
            .expect("same-origin entries");
        let sources = library.shared.score_sources.read().expect("score sources");
        assert!(sources.contains_key("https://samples.example/kit/kick.wav"));
        assert!(sources.contains_key("https://samples.example/snare.wav?take=2"));
        drop(sources);

        for (map, expected) in [
            (
                r#"{"blocked":"https://other.example/kick.wav"}"#,
                "outside the permitted sample origins",
            ),
            (
                r#"{"blocked":"file:///tmp/not-a-score-sample.wav"}"#,
                "is a file: address; only http and https are allowed",
            ),
        ] {
            let error = library
                .register_score_custom(map, None, &access)
                .expect_err("an entry outside the grant must fail the whole map");
            assert!(error.contains(expected), "{error}");
        }
        assert!(
            !library
                .custom
                .read()
                .expect("custom banks")
                .contains_key("blocked"),
            "a rejected map was partially published"
        );

        let credentialed = format!(
            r#"{{"blocked":"https://name:secret{}samples.example/kick.wav"}}"#,
            char::from(64)
        );
        let error = library
            .register_score_custom(&credentialed, None, &access)
            .expect_err("credentials must not be accepted in a score-selected URL");
        assert!(error.contains("contains credentials"), "{error}");
    }

    /// Read a request head off `stream` before answering it.
    ///
    /// Writing a response and closing while the client is still sending
    /// resets the connection, which surfaces as a network error instead of
    /// the response - intermittently, depending on scheduling. Every test
    /// server here consumes the request first for that reason.
    #[cfg(test)]
    fn drain_request_head(stream: &std::net::TcpStream) {
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut reader = std::io::BufReader::new(clone);
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::BufRead::read_line(&mut reader, &mut line) {
                Ok(0) => break,
                Ok(_) if line == "\r\n" || line == "\n" => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    /// Bytes reach the cache only once something has agreed they are what
    /// they claim to be, and an entry that stops validating is dropped rather
    /// than served forever.
    ///
    /// The case that motivates it: an origin returns an HTML error page with
    /// status 200. Committing first would make that page the permanent
    /// manifest for the URL, surviving restarts, with no way to retry.
    #[test]
    fn only_validated_bytes_are_committed_and_bad_entries_are_evicted() {
        use std::io::Write;
        use std::net::TcpListener;

        let dir = std::env::temp_dir().join(format!("rustel-cache-valid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/kit.json");
        let server = std::thread::spawn(move || {
            for body in ["<html>not json</html>", "{\"ok\":true}"] {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::BufRead::read_line(&mut reader, &mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.flush();
            }
        });

        let access = ScoreFetchAccess::Remote {
            origin: origin.clone(),
            cors_required: false,
        };
        let as_json = |bytes: &[u8]| {
            serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| error.to_string())
        };

        // A 200 that is not JSON is refused AND leaves nothing behind.
        assert!(
            fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Manifest,
                4096,
                as_json,
            )
            .is_err(),
            "an unparseable body must not be accepted"
        );
        assert!(
            !cache
                .path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant)
                .exists(),
            "an unvalidated body must not be committed"
        );

        // The next attempt reaches the server again and succeeds, and THAT is
        // what gets stored.
        let value = fetch_score_source_cached(
            &cache,
            &url,
            &access,
            ScoreCacheKind::Manifest,
            4096,
            as_json,
        )
        .expect("the valid body is accepted");
        assert_eq!(value["ok"], serde_json::json!(true));
        assert!(
            cache
                .path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant)
                .exists(),
            "valid bytes are committed"
        );
        server.join().expect("server");

        // A stored entry that stops validating is evicted, not served. The
        // listener is closed now, so the retry has nowhere to go and errors -
        // which is exactly how eviction shows.
        std::fs::write(
            cache.path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant),
            b"corrupted",
        )
        .expect("corrupt the entry");
        assert!(
            fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Manifest,
                4096,
                as_json,
            )
            .is_err(),
            "a corrupt entry must not be served"
        );
        assert!(
            !cache
                .path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant)
                .exists(),
            "a corrupt entry must be evicted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_content_kind_cannot_evict_another_kinds_entry() {
        use std::io::Write;
        use std::net::TcpListener;

        let dir =
            std::env::temp_dir().join(format!("rustel-cache-content-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/shared-resource");
        let access = ScoreFetchAccess::Remote {
            origin: origin.clone(),
            cors_required: false,
        };

        let manifest_path = cache.path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant);
        cache
            .admit(&manifest_path, br#"{"bank":[]}"#)
            .expect("seed manifest cache");
        let audio_path = cache.path(&url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
        assert_ne!(manifest_path, audio_path, "content kinds shared one key");

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            drain_request_head(&stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot audio"
            )
            .expect("response");
        });
        let error =
            fetch_score_source_cached(&cache, &url, &access, ScoreCacheKind::Audio, 1024, |_| {
                Err::<Vec<u8>, _>("audio validation failed".to_owned())
            })
            .expect_err("the audio form must fail validation");
        assert!(error.contains("audio validation failed"), "{error}");
        server.join().expect("server");

        assert!(
            manifest_path.exists(),
            "an audio validator evicted the manifest cache entry"
        );
        let manifest: serde_json::Value = fetch_score_source_cached(
            &cache,
            &url,
            &access,
            ScoreCacheKind::Manifest,
            1024,
            |bytes| serde_json::from_slice(bytes).map_err(|error| error.to_string()),
        )
        .expect("the manifest remains available offline");
        assert_eq!(manifest["bank"], serde_json::json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn quota_limits(
        global_bytes: u64,
        global_entries: usize,
        session_bytes: u64,
        session_entries: usize,
    ) -> ScoreCacheLimits {
        ScoreCacheLimits {
            global_bytes,
            global_entries,
            session_bytes,
            session_entries,
        }
    }

    #[test]
    fn score_cache_refuses_before_staging_without_evicting_existing_entries() {
        let dir =
            std::env::temp_dir().join(format!("rustel-cache-global-quota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = ScoreCache::with_limits(dir.clone(), quota_limits(6, 2, 100, 10));
        let first = cache.path(
            "https://samples.example/first.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        let second = cache.path(
            "https://samples.example/second.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        let third = cache.path(
            "https://samples.example/third.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );

        assert_eq!(
            cache.admit(&first, b"1234").expect("first admission"),
            CacheAdmission::Stored
        );
        assert_eq!(
            cache.admit(&second, b"567").expect("byte refusal"),
            CacheAdmission::Refused,
            "a write crossing the byte ceiling must be refused"
        );
        assert_eq!(std::fs::read(&first).expect("first remains"), b"1234");
        assert!(!second.exists(), "a refused target was created");
        assert!(
            std::fs::read_dir(cache.dir())
                .expect("cache entries")
                .all(|entry| !is_score_cache_staging(&entry.expect("entry").path())),
            "a refused admission opened a staging file"
        );

        // The exact byte boundary is admitted. A second ScoreCache models a
        // second Session: its independent allowance cannot bypass the shared
        // on-disk entry ceiling.
        let other = ScoreCache::with_limits(dir.clone(), quota_limits(6, 2, 100, 10));
        assert_eq!(
            other.admit(&second, b"56").expect("boundary admission"),
            CacheAdmission::Stored
        );
        assert_eq!(
            other.admit(&third, b"").expect("entry refusal"),
            CacheAdmission::Refused
        );
        assert!(!third.exists(), "entry-limit refusal created a target");
        assert_eq!(
            score_cache_occupancy(&cache.dir()).expect("occupancy"),
            (2, 6)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_session_has_an_independent_cache_write_allowance() {
        let dir =
            std::env::temp_dir().join(format!("rustel-cache-session-quota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let limits = quota_limits(100, 10, 4, 1);
        let first_session = ScoreCache::with_limits(dir.clone(), limits);
        let second_session = ScoreCache::with_limits(dir.clone(), limits);
        let first = first_session.path(
            "https://samples.example/first.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        let second = first_session.path(
            "https://samples.example/second.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );

        assert_eq!(
            first_session
                .admit(&first, b"1234")
                .expect("first session admission"),
            CacheAdmission::Stored,
            "the exact byte boundary must be admitted"
        );
        assert_eq!(
            first_session
                .admit(&second, b"x")
                .expect("first session refusal"),
            CacheAdmission::Refused
        );
        assert_eq!(
            second_session
                .admit(&second, b"x")
                .expect("second session admission"),
            CacheAdmission::Stored,
            "one Session's allowance leaked into another"
        );
        assert_eq!(
            first_session
                .admit(&first, b"ignored")
                .expect("existing entry"),
            CacheAdmission::AlreadyPresent,
            "hits must not consume or require Session admission allowance"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_sessions_cannot_race_past_the_global_cache_bound() {
        let dir = std::env::temp_dir().join(format!(
            "rustel-cache-concurrent-quota-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let limits = quota_limits(4, 1, 100, 10);
        let first = Arc::new(ScoreCache::with_limits(dir.clone(), limits));
        let second = Arc::new(ScoreCache::with_limits(dir.clone(), limits));
        let barrier = Arc::new(std::sync::Barrier::new(3));

        let spawn =
            |cache: Arc<ScoreCache>, url: &'static str, barrier: Arc<std::sync::Barrier>| {
                std::thread::spawn(move || {
                    let path = cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
                    barrier.wait();
                    cache.admit(&path, b"1234").expect("admission")
                })
            };
        let a = spawn(
            Arc::clone(&first),
            "https://samples.example/a.wav",
            Arc::clone(&barrier),
        );
        let b = spawn(
            Arc::clone(&second),
            "https://samples.example/b.wav",
            Arc::clone(&barrier),
        );
        barrier.wait();
        let outcomes = [a.join().expect("first"), b.join().expect("second")];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == CacheAdmission::Stored)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == CacheAdmission::Refused)
                .count(),
            1
        );
        assert_eq!(
            score_cache_occupancy(&first.dir()).expect("occupancy"),
            (1, 4)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_entries_copy_only_when_admitted_and_clear_disables_reimport() {
        let dir = std::env::temp_dir().join(format!(
            "rustel-cache-legacy-migration-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("cache root");
        let url = "http://127.0.0.1:9/kick.wav";
        let access = ScoreFetchAccess::Remote {
            origin: "http://127.0.0.1:9".to_owned(),
            cors_required: false,
        };
        let legacy = cache_path(&dir, url);
        std::fs::write(&legacy, b"legacy").expect("legacy entry");

        let refused = ScoreCache::with_limits(dir.clone(), quota_limits(0, 0, 0, 0));
        assert_eq!(
            fetch_score_source_cached(&refused, url, &access, ScoreCacheKind::Audio, 64, |bytes| {
                Ok(bytes.to_vec())
            },)
            .expect("legacy hit remains usable"),
            b"legacy"
        );
        assert!(
            legacy.exists(),
            "refused migration destroyed the offline copy"
        );
        assert!(
            !refused
                .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant)
                .exists()
        );

        let admitted = ScoreCache::with_limits(dir.clone(), quota_limits(64, 4, 64, 4));
        assert_eq!(
            fetch_score_source_cached(
                &admitted,
                url,
                &access,
                ScoreCacheKind::Audio,
                64,
                |bytes| Ok(bytes.to_vec()),
            )
            .expect("legacy migration"),
            b"legacy"
        );
        assert!(
            admitted
                .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant)
                .exists(),
            "legacy copy was not committed"
        );
        assert!(
            legacy.exists(),
            "migration removed a possibly host-trusted cache entry"
        );

        // Clearing leaves unrelated host-trusted cache files alone, but a
        // durable marker prevents one of those mixed-namespace files from
        // becoming score-selected cache data again.
        std::fs::write(&legacy, b"trusted cache").expect("trusted cache entry");
        let in_flight = admitted
            .read_legacy(&legacy, 64)
            .expect("legacy read before clear")
            .expect("legacy bytes before clear");
        clear_score_sample_cache_at(&dir).expect("clear score cache");
        // This models the exact clear/migration race: a fetch read and
        // validated legacy bytes, then clear completed before its admission.
        admitted.copy_legacy(
            &admitted.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant),
            &in_flight,
        );
        assert!(!admitted.dir().exists(), "score namespace survived clear");
        assert!(legacy.exists(), "clear removed a host-trusted cache entry");
        assert!(
            dir.join(SCORE_CACHE_NO_LEGACY_MARKER).is_file(),
            "clear did not disable legacy migration"
        );
        assert!(
            fetch_score_source_cached(
                &admitted,
                url,
                &access,
                ScoreCacheKind::Audio,
                64,
                |bytes| Ok(bytes.to_vec()),
            )
            .is_err(),
            "clear silently reimported a legacy entry"
        );
        assert!(
            !admitted
                .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant)
                .exists()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn score_cache_control_paths_refuse_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "rustel-cache-control-symlinks-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("cache root");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"untouched").expect("victim");

        let lock = score_cache_lock_path(&dir);
        symlink(&victim, &lock).expect("lock symlink");
        let cache = ScoreCache::with_limits(dir.clone(), quota_limits(64, 4, 64, 4));
        let target = cache.path(
            "https://samples.example/kick.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        let error = cache
            .admit(&target, b"sample")
            .expect_err("a symlink lock must be refused");
        assert!(error.contains("lock is not a regular file"), "{error}");
        assert_eq!(
            std::fs::read(&victim).expect("victim remains"),
            b"untouched"
        );
        std::fs::remove_file(&lock).expect("remove lock symlink");

        std::fs::create_dir_all(cache.dir()).expect("score namespace");
        std::fs::write(cache.dir().join("existing"), b"cache").expect("score entry");
        let marker = dir.join(SCORE_CACHE_NO_LEGACY_MARKER);
        symlink(&victim, &marker).expect("marker symlink");
        let error = clear_score_sample_cache_at(&dir)
            .expect_err("a symlink marker must be refused before clearing");
        assert!(error.contains("marker") && error.contains("not a regular file"));
        assert!(cache.dir().is_dir(), "clear ran after refusing its marker");
        assert_eq!(
            std::fs::read(&victim).expect("victim remains"),
            b"untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn score_cache_open_refuses_a_symlink_swap() {
        use std::os::unix::fs::symlink;

        let dir = test_cache_base("cache-entry-symlink-swap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("cache root");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"secret").expect("victim");
        let cache = ScoreCache::with_limits(dir.clone(), quota_limits(64, 4, 64, 4));
        let path = cache.path(
            "https://samples.example/kick.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        cache.admit(&path, b"sample").expect("admit");
        std::fs::remove_file(&path).expect("remove cache file");
        symlink(&victim, &path).expect("plant symlink");
        let error = cache
            .open(&path)
            .expect_err("a swapped symlink must not be followed");
        assert!(error.contains("not a regular file"), "{error}");
        assert_eq!(std::fs::read(&victim).expect("victim remains"), b"secret");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The identity a cache entry is keyed by, pinned as a matrix.
    ///
    /// Each row here is a defect that existed: the first two shared a file
    /// when they are different resources, and the next two failed to share
    /// one when they are the same resource.
    #[test]
    fn score_cache_identity_survives_product_renames() {
        let path = score_cache_path(
            Path::new("/cache"),
            "https://samples.example/kick.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Grant,
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("f51176c6893fc2b1d0ea1ea37a6b171b.wav")
        );
        let cors = score_cache_path(
            Path::new("/cache"),
            "https://samples.example/kick.wav",
            ScoreCacheKind::Audio,
            ScoreCacheTrust::Cors,
        );
        assert_ne!(
            path, cors,
            "CORS-consented and grant-only bytes must not share a file"
        );
    }

    #[test]
    fn cors_required_fetches_do_not_reuse_grant_only_cache_bytes() {
        let dir =
            std::env::temp_dir().join(format!("rustel-cache-cors-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());
        let url = "https://samples.example/kick.wav";
        let grant_path = cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
        std::fs::create_dir_all(grant_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&grant_path, b"grant-only bytes").expect("seed grant entry");

        let cors_access = ScoreFetchAccess::Remote {
            origin: "https://samples.example".into(),
            cors_required: true,
        };
        // No server: a cors_required hit must not return the grant-only file.
        // The fetch fails on the wire instead of serving unconsented bytes.
        let error = fetch_score_source_cached(
            &cache,
            url,
            &cors_access,
            ScoreCacheKind::Audio,
            1024,
            |bytes| Ok(bytes.to_vec()),
        )
        .expect_err("cors policy must not read grant-only cache");
        assert!(
            !error.is_empty(),
            "refusal should name the failed fetch, got {error:?}"
        );
        assert!(
            !cache
                .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Cors)
                .exists(),
            "a failed cors fetch must not invent a cors entry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_identity_follows_the_url_not_the_text() {
        let dir = Path::new("/cache");
        let same = |a: &str, b: &str| cache_path(dir, a) == cache_path(dir, b);

        // Distinct resources must not collide. A slash inside a query used to
        // satisfy the old trailing-slash test, which searched the whole URL.
        assert!(!same(
            "https://a.example/kick.wav?q=///",
            "https://a.example/kick.wav?q=//"
        ));
        assert!(!same("https://a.example/a", "https://a.example/a/"));
        assert!(!same(
            "https://a.example/kick.wav",
            "https://a.example/kick.wav?v=2"
        ));
        assert!(!same(
            "https://a.example/kick#2.wav",
            "https://a.example/kick.wav"
        ));

        // The same resource must share one entry, dotted host or not. The
        // extension used to come from the raw string, so a dotted HOST
        // supplied one for the bare form and not the slashed form.
        assert!(same("https://example.com", "https://example.com/"));
        assert!(same("http://localhost:5432", "http://localhost:5432/"));
        assert!(same(
            "https://a.example/kick#2.wav",
            "https://a.example/kick%232.wav"
        ));

        // The extension is for humans reading the cache directory; it comes
        // from the path's last segment and nowhere else.
        let named = cache_path(dir, "https://a.example/kits/kick.wav");
        assert_eq!(named.extension().and_then(|ext| ext.to_str()), Some("wav"));
        assert_eq!(
            cache_path(dir, "https://example.com/").extension(),
            None,
            "a dotted host must not masquerade as a file extension"
        );
        // A query does not change what the file is.
        assert_eq!(
            cache_path(dir, "https://a.example/kick.wav?v=2")
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("wav")
        );
    }

    /// The grant decides before the cache does.
    ///
    /// Checking the cache first and the policy second would turn the cache
    /// into a way around the policy: a score with no grant, or a grant for
    /// somewhere else, could read whatever a granted score had left behind.
    #[test]
    fn a_denied_score_cannot_read_a_seeded_cache_entry() {
        let dir = std::env::temp_dir().join(format!("rustel-cache-deny-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());
        let url = "http://granted.example/kick.wav";
        std::fs::create_dir_all(cache.dir()).expect("score cache");
        std::fs::write(
            cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant),
            b"seeded bytes",
        )
        .expect("seed the cache");

        // A grant for a DIFFERENT origin must not reach this entry, and must
        // not fall through to the network either.
        let elsewhere = ScoreFetchAccess::Remote {
            origin: "http://other.example".to_owned(),
            cors_required: false,
        };
        let error =
            fetch_score_source_cached(&cache, url, &elsewhere, ScoreCacheKind::Audio, 1024, |b| {
                Ok(b.to_vec())
            })
            .expect_err("a grant elsewhere must not read this entry");
        assert!(error.contains("outside its permitted origin"), "{error}");

        // The matching grant does read it, which is what proves the entry was
        // reachable all along and policy is what withheld it.
        let granted = ScoreFetchAccess::Remote {
            origin: "http://granted.example".to_owned(),
            cors_required: false,
        };
        assert_eq!(
            fetch_score_source_cached(&cache, url, &granted, ScoreCacheKind::Audio, 1024, |b| Ok(
                b.to_vec()
            ),)
            .expect("granted read"),
            b"seeded bytes",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hit serves the bytes whether or not the server is reachable. This is
    /// the property offline rendering depends on: the corpus fetches its
    /// samples once and every later comparison runs with nothing listening.
    #[test]
    fn a_granted_url_is_served_from_cache_with_no_server_running() {
        let dir = std::env::temp_dir().join(format!("rustel-cache-hit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());
        // Port 9 is the discard port: nothing accepts there, so a fetch that
        // reached the network at all would fail rather than pass quietly.
        let url = "http://127.0.0.1:9/kick.wav";
        std::fs::create_dir_all(cache.dir()).expect("score cache");
        std::fs::write(
            cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant),
            b"cached audio",
        )
        .expect("seed");
        let access = ScoreFetchAccess::Remote {
            origin: "http://127.0.0.1:9".to_owned(),
            cors_required: false,
        };
        assert_eq!(
            fetch_score_source_cached(&cache, url, &access, ScoreCacheKind::Audio, 1024, |b| Ok(
                b.to_vec()
            ),)
            .expect("served from cache"),
            b"cached audio",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A miss fetches once and writes, so the NEXT run needs no server. Both
    /// halves are asserted here: the server is taken down between them.
    #[test]
    fn a_cache_miss_populates_for_the_next_offline_run() {
        use std::io::Write;
        use std::net::TcpListener;

        let dir = std::env::temp_dir().join(format!("rustel-cache-fill-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/kick.wav");
        let body = "fetched once";
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            // Consume the request head before answering. Writing and closing
            // while the client is still sending resets the connection, which
            // surfaces as a network error rather than the response.
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
            let _ = stream.flush();
        });

        let access = ScoreFetchAccess::Remote {
            origin: origin.clone(),
            cors_required: false,
        };
        let first =
            fetch_score_source_cached(&cache, &url, &access, ScoreCacheKind::Audio, 1024, |b| {
                Ok(b.to_vec())
            })
            .expect("first fetch");
        server.join().expect("server");
        assert_eq!(first, b"fetched once");

        // The listener is closed now. A second read can only come from disk.
        let second =
            fetch_score_source_cached(&cache, &url, &access, ScoreCacheKind::Audio, 1024, |b| {
                Ok(b.to_vec())
            })
            .expect("second read must not need the server");
        assert_eq!(second, b"fetched once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An origin grant is not an address grant. A granted name that resolves
    /// somewhere private is still refused, at the resolver, on every hop.
    #[test]
    fn a_granted_origin_pointing_somewhere_private_is_still_refused() {
        let dir = std::env::temp_dir().join(format!("rustel-cache-priv-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = ScoreCache::new(dir.clone());
        for target in [
            "http://10.255.0.7/kick.wav",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let parsed = Url::parse(target).expect("test url");
            let access = ScoreFetchAccess::Remote {
                origin: parsed.origin().ascii_serialization(),
                cors_required: false,
            };
            let error = fetch_score_source_cached(
                &cache,
                target,
                &access,
                ScoreCacheKind::Audio,
                1024,
                |b| Ok(b.to_vec()),
            )
            .expect_err("a private target must be refused even when its origin is granted");
            assert!(
                error.contains("not a public address"),
                "{target}: refusal must name the address guard: {error}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetched_maps_cannot_expand_their_origin_grant() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("map listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let map_url = format!("{origin}/strudel.json");
        let body = r#"{"escape":"http://127.0.0.1:9/private.wav"}"#;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("map request");
            drain_request_head(&stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("map response");
        });

        let mut access = ScoreSampleAccess::denied();
        access.permit_origin(&origin).expect("map origin");
        let library = empty_library();
        let source = serde_json::to_string(&map_url).expect("source string");
        let error = library
            .register_score_custom(&source, None, &access)
            .expect_err("a fetched map must not introduce another origin");
        server.join().expect("map server");
        assert!(
            error.contains("outside the permitted sample origins"),
            "{error}"
        );
        assert!(library.custom.read().expect("custom banks").is_empty());
    }

    #[test]
    fn score_selected_http_redirects_cannot_leave_the_granted_origin() {
        use std::io::Write;
        use std::net::TcpListener;

        let destination = TcpListener::bind("127.0.0.1:0").expect("destination listener");
        destination
            .set_nonblocking(true)
            .expect("nonblocking destination");
        let destination_url = format!("http://{}/payload", destination.local_addr().unwrap());
        let redirect = TcpListener::bind("127.0.0.1:0").expect("redirect listener");
        let redirect_url = format!("http://{}/map", redirect.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = redirect.accept().expect("redirect request");
            drain_request_head(&stream);
            write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {destination_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .expect("redirect response");
        });
        let parsed = Url::parse(&redirect_url).expect("redirect URL");
        let access = ScoreFetchAccess::Remote {
            origin: parsed.origin().ascii_serialization(),
            cors_required: false,
        };
        let error = fetch_score_source(&redirect_url, &access, 1024)
            .expect_err("score fetch must stop at a cross-origin redirect");
        server.join().expect("redirect server");
        assert!(
            error.contains("outside the permitted sample origin"),
            "{error}"
        );
        assert!(
            matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "the redirect target received a request"
        );
    }

    #[test]
    fn same_origin_sample_redirects_remain_compatible() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("sample listener");
        let url = format!("http://{}/redirect", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("redirect request");
            drain_request_head(&first);
            write!(
                    first,
                    "HTTP/1.1 302 Found\r\nLocation: /payload\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .expect("redirect response");
            let (mut second, _) = listener.accept().expect("payload request");
            drain_request_head(&second);
            write!(
                second,
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nRIFF"
            )
            .expect("payload response");
        });
        let parsed = Url::parse(&url).expect("sample URL");
        let access = ScoreFetchAccess::Remote {
            origin: parsed.origin().ascii_serialization(),
            cors_required: false,
        };
        let bytes = fetch_score_source(&url, &access, 1024).expect("same-origin redirect");
        server.join().expect("sample server");
        assert_eq!(bytes, b"RIFF");
    }

    #[cfg(unix)]
    #[test]
    fn local_score_samples_are_confined_to_the_granted_root() {
        let root =
            std::env::temp_dir().join(format!("rustel-score-samples-{}", std::process::id()));
        let outside = root.with_extension("outside.wav");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
        std::fs::create_dir_all(root.join("kit")).expect("sample folder");
        std::fs::write(root.join("kit/kick.wav"), b"RIFF").expect("sample");
        std::fs::write(&outside, b"RIFF private").expect("outside file");
        std::os::unix::fs::symlink(&outside, root.join("kit/escape.wav")).expect("symlink");

        let mut access = ScoreSampleAccess::denied();
        access.permit_local_root(&root).expect("local root");
        let library = empty_library();
        library
            .register_score_custom(r#""local:kit""#, None, &access)
            .expect("local kit");
        let custom = library.custom.read().expect("custom banks");
        let Bank::Array(urls) = custom.get("kit").expect("kit bank") else {
            panic!("kit was not an array bank");
        };
        assert_eq!(urls.len(), 1, "a symlink escaped the granted root");
        assert!(urls[0].ends_with("/kit/kick.wav"));
        drop(custom);

        for source in [
            r#""local:..""#,
            r#""local:kit/../../password.txt""#,
            r#""local:/tmp""#,
        ] {
            assert!(
                library
                    .register_score_custom(source, None, &access)
                    .is_err(),
                "{source} escaped the granted root"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn score_local_read_refuses_a_sample_swapped_to_an_escape() {
        use std::os::unix::fs::symlink;

        let root = test_cache_base("score-local-symlink-swap");
        let outside = root.with_extension("outside.wav");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
        std::fs::create_dir_all(root.join("kit")).expect("sample folder");
        let target = root.join("kit/kick.wav");
        let original = root.join("kit/original.wav");
        std::fs::write(&target, b"RIFF inside").expect("inside sample");
        std::fs::write(&outside, b"RIFF outside private").expect("outside sample");

        let mut grant = ScoreSampleAccess::denied();
        grant.permit_local_root(&root).expect("local root");
        let access = ScoreFetchAccess::Local {
            root: Arc::clone(grant.local_root.as_ref().expect("granted root")),
        };
        let url = Url::from_file_path(&target)
            .expect("sample file URL")
            .to_string();
        let error = fetch_score_source_with_budget(
            &url,
            &access,
            1024,
            &sample_fetch::FetchBudget::for_one_fetch(),
            || {
                std::fs::rename(&target, &original).expect("move original sample");
                symlink(&outside, &target).expect("install escaping symlink");
            },
        )
        .expect_err("an escaping replacement must be refused");
        assert!(
            // Which guard fires is the platform's business (macOS's upfront
            // containment check speaks first; Linux's open does) - the
            // contract is the refusal, always in the root's name.
            error.contains("outside the permitted root")
                || error.contains("unavailable beneath the permitted root"),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn score_local_read_refuses_a_fifo_swap_without_blocking() {
        use std::os::unix::ffi::OsStrExt;

        let root = test_cache_base("score-local-fifo-swap");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("kit")).expect("sample folder");
        let target = root.join("kit/kick.wav");
        let original = root.join("kit/original.wav");
        std::fs::write(&target, b"RIFF inside").expect("inside sample");

        let mut grant = ScoreSampleAccess::denied();
        grant.permit_local_root(&root).expect("local root");
        let access = ScoreFetchAccess::Local {
            root: Arc::clone(grant.local_root.as_ref().expect("granted root")),
        };
        let url = Url::from_file_path(&target)
            .expect("sample file URL")
            .to_string();
        let started = std::time::Instant::now();
        let error = fetch_score_source_with_budget(
            &url,
            &access,
            1024,
            &sample_fetch::FetchBudget::for_one_fetch(),
            || {
                std::fs::rename(&target, &original).expect("move original sample");
                let path = std::ffi::CString::new(target.as_os_str().as_bytes())
                    .expect("fixture path without NUL");
                // SAFETY: `path` is NUL-terminated and names a new fixture FIFO.
                let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
                assert_eq!(
                    result,
                    0,
                    "create replacement FIFO: {}",
                    std::io::Error::last_os_error()
                );
            },
        )
        .expect_err("a replacement FIFO must be refused");
        assert!(
            // Which guard fires is the platform's business (macOS's upfront
            // containment check speaks first; Linux's open does) - the
            // contract is the refusal, always in the root's name.
            error.contains("outside the permitted root")
                || error.contains("unavailable beneath the permitted root"),
            "{error}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "opening the replacement FIFO blocked"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn local_score_samples_are_confined_to_the_granted_root_on_windows() {
        let root = std::env::temp_dir().join(format!(
            "rustel-score-samples-windows-{}",
            std::process::id()
        ));
        let outside = root.with_extension("outside.wav");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
        std::fs::create_dir_all(root.join("kit")).expect("sample folder");
        std::fs::write(root.join("kit/kick.wav"), b"RIFF").expect("sample");
        std::fs::write(&outside, b"RIFF private").expect("outside file");

        let mut access = ScoreSampleAccess::denied();
        access.permit_local_root(&root).expect("local root");
        let library = empty_library();
        library
            .register_score_custom(r#""local:kit""#, None, &access)
            .expect("local kit");
        let custom = library.custom.read().expect("custom banks");
        let Bank::Array(urls) = custom.get("kit").expect("kit bank") else {
            panic!("kit was not an array bank");
        };
        assert_eq!(urls.len(), 1, "the kit registered files it does not hold");
        assert!(urls[0].ends_with("/kit/kick.wav"), "{}", urls[0]);
        let url = urls[0].to_string();
        drop(custom);

        for source in [
            r#""local:..""#,
            r#""local:kit/../../outside.wav""#,
            r#""local:/Windows""#,
            r#""local:C:/Windows""#,
        ] {
            assert!(
                library
                    .register_score_custom(source, None, &access)
                    .is_err(),
                "{source} escaped the granted root"
            );
        }

        // The Windows regression proper: registration spelled the canonical
        // verbatim path into the URL and playback gets a non-verbatim path
        // back from `to_file_path`. The registered URL must still load.
        let bytes = fetch_score_source(
            &url,
            &ScoreFetchAccess::Local {
                root: Arc::clone(access.local_root.as_ref().expect("granted root")),
            },
            1024,
        )
        .expect("a registered local sample must load on Windows");
        assert_eq!(bytes, b"RIFF");

        // A URL naming the file beside the root must not.
        let outside_url = Url::from_file_path(outside.canonicalize().expect("canonical outside"))
            .expect("outside URL")
            .to_string();
        let error = fetch_score_source(
            &outside_url,
            &ScoreFetchAccess::Local {
                root: Arc::clone(access.local_root.as_ref().expect("granted root")),
            },
            1024,
        )
        .expect_err("a file beside the granted root must be refused");
        assert!(error.contains("outside the permitted root"), "{error}");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    /// The size limit applies during the read, not after it. `/dev/zero`
    /// never ends, so a read without a bound would never return.
    #[cfg(unix)]
    #[test]
    fn an_endless_local_sample_is_refused_rather_than_read() {
        // `/dev/zero` reports a length of zero and never ends, so the stat
        // says nothing and the bounded read is what refuses it. That is the
        // case the bound exists for.
        let error = fetch_located("file:///dev/zero").expect_err("an endless file must be refused");
        assert!(
            error.contains("the most one sound can hold"),
            "the refusal did not name the size limit: {error}"
        );
        // And it says the ceiling in the units a reader thinks in, so a
        // sample that is genuinely too big can be measured against it.
        assert!(
            error.contains(&rustel_audio::format_sample_bytes(
                rustel_audio::sample_pcm_ceiling()
            )),
            "{error}"
        );

        // A real file that is plainly too big is refused off its own
        // length, before anything is read: it can say its own size, which
        // the endless one above cannot.
        let directory = tempfile::tempdir().expect("temp dir");
        let big = directory.path().join("too big.wav");
        let ceiling = rustel_audio::sample_pcm_ceiling();
        rustel_audio::set_sample_pcm_ceiling(rustel_audio::MIN_SAMPLE_PCM_BYTES);
        std::fs::write(&big, vec![0u8; rustel_audio::MIN_SAMPLE_PCM_BYTES + 1]).expect("fixture");
        let error = fetch_located(&format!("file://{}", big.display()))
            .expect_err("a file past the ceiling must be refused");
        assert!(error.contains("past the"), "{error}");
        assert!(
            error.contains(&rustel_audio::format_sample_bytes(
                rustel_audio::MIN_SAMPLE_PCM_BYTES + 1
            )),
            "the refusal names the file's own size: {error}"
        );
        // And one inside the ceiling still arrives whole.
        let small = directory.path().join("fine.wav");
        std::fs::write(&small, vec![7u8; 4096]).expect("fixture");
        let bytes = fetch_located(&format!("file://{}", small.display())).expect("read");
        assert_eq!(bytes.len(), 4096);
        rustel_audio::set_sample_pcm_ceiling(ceiling);
    }

    /// The bound must not break ordinary local samples.
    #[test]
    fn an_ordinary_local_sample_still_reads_whole() {
        let path = std::env::temp_dir().join(format!("rustel-sample-{}.wav", std::process::id()));
        let contents: Vec<u8> = (0..4096u32).map(|byte| byte as u8).collect();
        std::fs::write(&path, &contents).expect("write the fixture");
        let read = fetch_located(&format!("file://{}", path.display())).expect("read the fixture");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read, contents, "the local sample did not come back whole");
    }

    /// A folder that links back to itself must not be walked twice.
    ///
    /// `path.is_dir()` follows a symlink, so a cycle produced the same sample
    /// over and over: 41 entries from one file, which silently shifts every
    /// `n()` index in the bank. The scan reads the entry's own file type and
    /// never follows one.
    #[cfg(unix)]
    #[test]
    fn a_directory_symlink_cycle_does_not_duplicate_a_bank() {
        let root = std::env::temp_dir().join(format!("rustel-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("kick")).expect("dirs");
        std::fs::write(root.join("kick/1.wav"), b"RIFF").expect("sample");
        let root = root.canonicalize().expect("canonical");
        std::os::unix::fs::symlink(&root, root.join("kick/loop")).expect("symlink");

        let banks = scan_sample_folder(&root).expect("scan");
        let files: usize = banks.values().map(Vec::len).sum();
        assert_eq!(files, 1, "the cycle was walked: {banks:?}");
        assert_eq!(banks["kick"], vec!["kick/1.wav".to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_scan_working_set_is_bounded_before_retention() {
        let root = std::env::temp_dir().join(format!("rustel-scan-work-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("aaa/bbb/ccc")).expect("fixture folders");
        std::fs::write(root.join("aaa/bbb/ccc/tone.wav"), b"RIFF").expect("fixture sample");
        let root = root.canonicalize().expect("canonical root");
        let first = root.join("aaa");
        let second = first.join("bbb");
        let third = second.join("ccc");
        let relative = third
            .join("tone.wav")
            .strip_prefix(&root)
            .expect("relative sample")
            .to_string_lossy()
            .into_owned();
        let stack_bytes = sample_scan_stack_slots(4)
            .and_then(|slots| slots.checked_mul(std::mem::size_of::<PendingSampleDirectory>()))
            .expect("stack accounting");
        let path_pair = |left: &Path, right: &Path| {
            sample_scan_path_bytes(left)
                .and_then(|bytes| bytes.checked_add(sample_scan_path_bytes(right)?))
                .expect("path accounting")
        };
        let peak_paths = path_pair(&root, &first)
            .max(path_pair(&first, &second))
            .max(path_pair(&second, &third));
        let peak_entry = sample_scan_path_bytes(&third)
            .and_then(|bytes| bytes.checked_add(sample_scan_entry_bytes("ccc", &relative)?))
            .expect("entry accounting");
        let required = stack_bytes
            .checked_add(peak_paths.max(peak_entry))
            .expect("working-set accounting");
        let exact = SampleScanLimits {
            examined_entries: 4,
            manifest_entries: 1,
            manifest_bytes: 1_024,
            working_bytes: required,
        };

        let banks = scan_sample_folder_with_limits(&root, exact)
            .expect("the exact scanner working-set limit must succeed");
        assert_eq!(banks.values().map(Vec::len).sum::<usize>(), 1);

        let under = SampleScanLimits {
            working_bytes: required - 1,
            ..exact
        };
        let error = scan_sample_folder_with_limits(&root, under)
            .expect_err("one byte below the scanner working set must refuse");
        assert!(matches!(
            error,
            SampleFolderScanError::Limit {
                resource: "scanner working-set bytes",
                limit
            } if limit == required - 1
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A 16-bit mono WAV of `frames` samples at `rate`, for the decode tests.
    pub(super) fn wav_bytes(rate: u32, frames: usize) -> Vec<u8> {
        let mut pcm = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let phase = std::f64::consts::TAU * 1000.0 * frame as f64 / f64::from(rate);
            pcm.extend_from_slice(&((phase.sin() * 16000.0) as i16).to_le_bytes());
        }
        let mut wav = Vec::with_capacity(44 + pcm.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + pcm.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        wav
    }

    /// A sample arrives at the context's rate, the way `decodeAudioData` hands
    /// one back, so playback interpolates for `speed` and nothing else.
    #[test]
    fn a_sample_decodes_to_the_context_rate() {
        let bytes = wav_bytes(44_100, 4410);
        let decoded = decode_guarded("kit/hit.wav", &bytes, Some(48_000)).expect("decode");
        assert_eq!(decoded.sample_rate(), 48_000);
        assert_eq!(decoded.frames(), 4800);
        // Same rate in and out is a copy, not a trip through the kernel.
        let same =
            decode_guarded("kit/hit.wav", &wav_bytes(48_000, 4800), Some(48_000)).expect("decode");
        assert_eq!(same.sample_rate(), 48_000);
        assert_eq!(same.frames(), 4800);
    }

    /// A wavetable keeps the file's rate: the worklet slices the buffer into
    /// frames, so converting first would cut every frame at the wrong stride
    /// and play a waveform the table does not contain.
    #[test]
    fn a_wavetable_keeps_the_files_own_rate() {
        let bytes = wav_bytes(88_200, 8192);
        let decoded = decode_guarded("wt/table.wav", &bytes, None).expect("decode");
        assert_eq!(decoded.sample_rate(), 88_200);
        assert_eq!(decoded.frames(), 8192);
    }

    #[test]
    fn only_wt_named_banks_decode_at_the_files_rate() {
        assert_eq!(DecodeRate::for_sound("wt_digital"), DecodeRate::Native);
        assert_eq!(DecodeRate::for_sound("wt_vgame"), DecodeRate::Native);
        assert_eq!(DecodeRate::for_sound("bd"), DecodeRate::Context);
        assert_eq!(DecodeRate::for_sound("piano"), DecodeRate::Context);
        // Not a prefix match on "wt" alone.
        assert_eq!(DecodeRate::for_sound("wtf"), DecodeRate::Context);
    }

    #[test]
    fn the_file_extension_picks_the_decoder() {
        assert_eq!(codec_for("file:///kit/kick.wav"), Codec::Wav);
        assert_eq!(codec_for("https://example.com/piano/C4.mp3"), Codec::Mp3);
        // A folder of phone recordings is the reason ogg matters at all.
        assert_eq!(
            codec_for("file:///s/artist/audio_2025-10-08.ogg"),
            Codec::Ogg
        );
        assert_eq!(codec_for("file:///s/artist/voice.oga"), Codec::Ogg);
        assert_eq!(codec_for("file:///s/ARTIST/SHOUT.OGG"), Codec::Ogg);
    }

    #[test]
    fn a_query_string_is_not_part_of_the_filename() {
        // A cache-busted bank URL still names its codec in the path.
        assert_eq!(codec_for("https://example.com/hit.ogg?v=2"), Codec::Ogg);
        assert_eq!(codec_for("https://example.com/hit.mp3#frag"), Codec::Mp3);
        // Without the split this reads the extension as "com/hit" and falls
        // back to wav, which decodes to "not a RIFF file".
        assert_eq!(codec_for("https://example.com/hit?name=a.wav"), Codec::Wav);
    }

    /// A local file URL has no Windows verbatim prefix (`\\?\C:\...`). The
    /// `?` of that prefix would read as a query string and hide the file
    /// extension from `codec_for`.
    #[test]
    fn a_local_file_url_never_carries_the_windows_verbatim_prefix() {
        assert_eq!(
            without_verbatim_prefix(r"\\?\C:\Users\me\Downloads\a_sample.mp3"),
            r"C:\Users\me\Downloads\a_sample.mp3"
        );
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\server\share\kit\kick.ogg"),
            r"\\server\share\kit\kick.ogg"
        );
        assert_eq!(
            without_verbatim_prefix("/home/me/kit/kick.wav"),
            "/home/me/kit/kick.wav"
        );
        assert_eq!(
            without_verbatim_prefix(r"C:\plain\already.wav"),
            r"C:\plain\already.wav"
        );
        let url = local_file_url(Path::new(r"\\?\C:\Users\me\Downloads\a_sample.mp3"));
        assert_eq!(url, r"file://C:\Users\me\Downloads\a_sample.mp3");
        assert_eq!(codec_for(&url), Codec::Mp3);
        assert_eq!(
            codec_for(&local_file_url(Path::new(r"\\?\C:\s\voice.oga"))),
            Codec::Ogg
        );
    }

    /// The same, through the walk itself: a folder registered by its
    /// canonical path yields URLs whose extension the decoder can read, and
    /// that `fetch_located` still opens.
    #[test]
    fn a_walked_folder_spells_urls_the_decoder_and_the_reader_both_take() {
        let directory = tempfile::tempdir().expect("temp dir");
        let kit = directory.path().join("kit");
        std::fs::create_dir_all(&kit).expect("bank folder");
        std::fs::write(kit.join("hit.mp3"), b"ID3mp3bytes").expect("sample");
        let root = directory.path().canonicalize().expect("canonical root");
        let banks = folder_banks(&root, &[]).expect("walk");
        let (_, Bank::Array(urls)) = banks.iter().find(|(name, _)| name == "kit").expect("kit")
        else {
            panic!("a folder of audio is an array bank");
        };
        assert_eq!(urls.len(), 1);
        let url = urls[0].as_ref();
        assert!(!url.contains(r"\\?\"), "{url}");
        assert_eq!(codec_for(url), Codec::Mp3, "{url}");
        assert_eq!(fetch_located(url).expect("readable"), b"ID3mp3bytes");
    }

    /// Direct files become variants of the selected folder, while child
    /// folders remain their own banks.
    #[test]
    fn folder_banks_groups_direct_files_under_the_selected_folder() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("take 1.mp3"), b"one").expect("sample");
        std::fs::write(directory.path().join("take 2.mp3"), b"two").expect("sample");
        let kit = directory.path().join("kit");
        std::fs::create_dir_all(&kit).expect("bank folder");
        std::fs::write(kit.join("snare.mp3"), b"snarebytes").expect("sample");
        let root = directory.path().canonicalize().expect("canonical root");
        let banks = folder_banks(&root, &[]).expect("walk");
        let root_name = root.file_name().unwrap().to_string_lossy();
        let (_, direct_bank @ Bank::Array(direct)) = banks
            .iter()
            .find(|(name, _)| name == root_name.as_ref())
            .expect("selected folder bank")
        else {
            panic!("direct files are an array bank");
        };
        assert_eq!(direct.len(), 2, "both takes are variants of one bank");
        assert_eq!(
            local_variant_names(direct_bank),
            ["take 1", "take 2"],
            "the browser keeps the filesystem stems in variant order"
        );
        assert!(banks.iter().any(|(name, _)| name == "kit"));
    }

    #[test]
    fn a_note_keyed_banks_waveform_uses_the_same_default_note_as_its_preview() {
        let library = SampleLibrary::empty_without_loading();
        let low: Arc<str> = Arc::from("https://samples.example/piano-c2.wav");
        let middle: Arc<str> = Arc::from("https://samples.example/piano-c4.wav");
        library.banks.write().expect("banks").insert(
            "piano".to_owned(),
            Bank::Notes(vec![
                (36.0, vec![low.clone()]),
                (60.0, vec![middle.clone()]),
            ]),
        );
        library
            .shared
            .by_url
            .write()
            .expect("sample table")
            .extend([
                (
                    low.clone(),
                    UrlState::Ready {
                        id: SampleId(7),
                        duration_secs: 1.25,
                    },
                ),
                (
                    middle.clone(),
                    UrlState::Ready {
                        id: SampleId(8),
                        duration_secs: 2.5,
                    },
                ),
            ]);
        library
            .shared
            .shapes
            .lock()
            .expect("sample shapes")
            .remember(low, Arc::from(&[36u8, 18][..]));
        library
            .shared
            .shapes
            .lock()
            .expect("sample shapes")
            .remember(middle, Arc::from(&[60u8, 30][..]));

        let SampleResolution::Found { id, .. } =
            SampleLookup::resolve(&library, "piano", 0.0, 36.0)
        else {
            panic!("preview sample resolves");
        };
        assert_eq!(id, SampleId(7));
        let (shape, duration) = library.sound_shape("piano:0").expect("preview shape");
        assert_eq!(&*shape, &[36, 18], "the picture belongs to the sound heard");
        assert_eq!(duration, 1.25);
    }

    #[test]
    fn a_local_variant_shows_its_index_and_filename_without_the_extension() {
        assert_eq!(
            local_sample_file_stem("file:///music/sessions/vox1.wav").as_deref(),
            Some("vox1")
        );
        let entry = SoundEntry {
            name: "sessions".into(),
            variants: 2,
            variant_names: vec!["take-001".into(), "vox1".into()],
            origin: SoundOrigin::Global,
            category: SoundCategory::Mine,
            location: None,
            import: Some("/music/sessions".into()),
        };
        assert_eq!(entry.variant_label(1), "sessions:1 (vox1)");
        assert!(!entry.variant_label(1).contains(".wav"));
    }

    /// Decode a real Ogg file, when one is pointed at. Works for either codec.
    ///
    /// Only one tiny Vorbis file is committed (`samples/testdata`), and the
    /// Vorbis guard tests decode it on every run; this test stays opt-in for
    /// qualifying real banks:
    /// `RUSTEL_TEST_OGG=/path/to/file.ogg cargo test -p rustel-runtime`.
    /// Point it at a folder's worth in a loop to qualify a whole sample bank.
    #[test]
    fn a_real_ogg_decodes_to_audio() {
        let Ok(path) = std::env::var("RUSTEL_TEST_OGG") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read the ogg named by RUSTEL_TEST_OGG");
        // The loader never calls a decoder directly, so check the guarded
        // entry point first: reaching the next line at all is the assertion,
        // because an unguarded panic would end the test here the way it would
        // end the loader thread.
        let guarded = decode_guarded(&path, &bytes, Some(48_000));
        assert!(
            guarded.is_ok(),
            "the loader would refuse this file: {}",
            guarded.err().unwrap_or_default()
        );
        let decoded = decode_ogg(&bytes).expect("decode ogg");
        assert!(decoded.frames() > 0, "decoded to no frames");
        assert!(
            (8_000..=192_000).contains(&decoded.sample_rate()),
            "implausible sample rate {}",
            decoded.sample_rate()
        );
        // A file that decodes to digital silence is a decoder that ran without
        // producing audio, which is the failure this catches: it looks like
        // success everywhere else and is inaudible only at the speaker.
        let peak = decoded
            .pcm()
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.001, "decoded to digital silence: peak {peak}");
    }

    pub(super) fn sample_test_shared(next_id: u32) -> Shared {
        Shared {
            by_url: RwLock::new(HashMap::new()),
            score_sources: RwLock::new(HashMap::new()),
            ready: Mutex::new(ReadySamples::default()),
            shapes: Mutex::new(SampleShapes::default()),
            next_id: AtomicU32::new(next_id),
            free_ids: Mutex::new(VecDeque::new()),
            jobs: LoadQueue::new(),
            loading: Mutex::new(HashSet::new()),
            caching: Mutex::new(HashSet::new()),
            host_cache: cache_dir(),
            font_base: String::new(),
            source_tables: Mutex::new(SourceTables::default()),
            bank_imports: RwLock::new(HashMap::new()),
            set_banks: RwLock::new(BTreeMap::new()),
            custom_file_names: RwLock::new(HashMap::new()),
            global_file_names: RwLock::new(HashMap::new()),
            fonts: RwLock::new(HashMap::new()),
            renames: RwLock::new(HashMap::new()),
            source_renames: RwLock::new(HashMap::new()),
            auto_aliases: RwLock::new(HashMap::new()),
            global_slots: Mutex::new(Vec::new()),
            global_generation: std::sync::atomic::AtomicU64::new(0),
            global_source_of: RwLock::new(HashMap::new()),
            import_policy: Mutex::new(None),
            font_jobs: FontQueue::new(),
            failures: Mutex::new(Vec::new()),
            direct_diagnostic_logging: AtomicBool::new(true),
            score_cache: ScoreCache::new(cache_dir()),
            manifest_pending: AtomicUsize::new(0),
            publication: Arc::new(PublicationGate::new()),
            render_rate: AtomicU32::new(crate::session::DEFAULT_SAMPLE_RATE),
            settled_epoch: AtomicU64::new(0),
        }
    }

    #[test]
    fn sample_id_reservation_stops_at_the_audio_bank_capacity_without_wrapping() {
        let shared = sample_test_shared((SAMPLE_BANK_CAPACITY - 1) as u32);
        assert_eq!(
            reserve_sample_ids(&shared, 1),
            Ok(vec![SampleId((SAMPLE_BANK_CAPACITY - 1) as u32)])
        );
        assert!(reserve_sample_ids(&shared, 1).is_err());
        assert_eq!(
            shared.next_id.load(Ordering::Relaxed),
            SAMPLE_BANK_CAPACITY as u32
        );
    }

    #[test]
    fn cache_to_disk_does_not_reserve_live_bank_slots() {
        let library = SampleLibrary::empty_without_loading();
        let url: Arc<str> = Arc::from("https://example.test/tone.wav");
        library
            .banks
            .write()
            .expect("banks")
            .insert("tone".to_owned(), Bank::Array(vec![url.clone()]));
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);

        assert_eq!(library.cache_to_disk("tone"), 1);
        assert_eq!(
            library.shared.next_id.load(Ordering::Relaxed),
            SAMPLE_BANK_CAPACITY as u32,
            "disk cache must not take a live-bank id"
        );
        assert!(
            library
                .shared
                .by_url
                .read()
                .expect("url table")
                .get(&url)
                .is_none(),
            "disk cache must not publish Loading or Ready"
        );
        let job = library.shared.jobs.try_pop().expect("cache job queued");
        assert_eq!(&*job.url, &*url);
        assert!(
            matches!(job.kind, LoadKind::Cache),
            "cache job must not ask the decoder to seat PCM"
        );
        assert!(
            library
                .shared
                .caching
                .lock()
                .expect("disk-cache claims")
                .contains(&url)
        );
        assert_eq!(library.cache_to_disk("tone"), 1);
        assert!(
            library.shared.jobs.try_pop().is_none(),
            "a second pass must not enqueue the same url twice"
        );
    }

    #[test]
    fn cache_to_disk_queues_font_files_instead_of_decoding_zones() {
        let library = SampleLibrary::with_background_loaders_at(
            HashMap::new(),
            Vec::new(),
            HashMap::from([(
                "gm_test".to_owned(),
                vec![Arc::from("font0"), Arc::from("font1")],
            )]),
            "https://example.test/sound".to_owned(),
            cache_dir(),
            Loading::NotStarted,
        )
        .expect("library");
        library
            .shared
            .next_id
            .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);

        assert_eq!(library.cache_to_disk("gm_test"), 2);
        assert_eq!(
            library.shared.next_id.load(Ordering::Relaxed),
            SAMPLE_BANK_CAPACITY as u32
        );
        assert!(
            library.shared.font_jobs.try_pop().is_none(),
            "font decode must not run during a disk cache"
        );
        let mut queued: Vec<_> = std::iter::from_fn(|| library.shared.jobs.try_pop())
            .map(|job| {
                assert!(matches!(job.kind, LoadKind::Cache));
                job.url.to_string()
            })
            .collect();
        queued.sort();
        assert_eq!(
            queued,
            [
                "https://example.test/sound/font0.js".to_owned(),
                "https://example.test/sound/font1.js".to_owned()
            ]
        );
        assert!(library.shared.fonts.read().expect("fonts").is_empty());
    }

    /// `join_url` puts the missing `Membranophones/` parent back on VCSL
    /// entries that start with `Struck Membranophones/` (encoded or not), and
    /// rewrites no other entry.
    #[test]
    fn vcsl_tom_mallet_paths_get_their_membranophones_parent() {
        let base = "https://strudel.b-cdn.net/VCSL/";
        assert_eq!(
            join_url(
                base,
                "Struck%20Membranophones/Tom%201/Mallet/TomH_HitM_v2_rr1_Mid.wav"
            ),
            "https://strudel.b-cdn.net/VCSL/Membranophones/Struck%20Membranophones/Tom%201/Mallet/TomH_HitM_v2_rr1_Mid.wav"
        );
        assert_eq!(
            join_url(
                base,
                "Membranophones/Struck%20Membranophones/Tom%202/Stick/x.wav"
            ),
            "https://strudel.b-cdn.net/VCSL/Membranophones/Struck%20Membranophones/Tom%202/Stick/x.wav",
            "paths that already have the parent stay put"
        );
        assert_eq!(
            join_url(
                "https://strudel.b-cdn.net/piano/",
                "Struck%20Membranophones/x.wav"
            ),
            "https://strudel.b-cdn.net/piano/Struck%20Membranophones/x.wav",
            "only VCSL is rewritten"
        );
    }

    /// The shipped packs as the Sources page lists them: every pin that
    /// brings files, the inline banks under their folder's name, the fonts
    /// last, and no alias-only pin, which has nothing to count or cache.
    #[test]
    fn the_shipped_packs_are_the_pins_that_bring_files() {
        let sources = SampleLibrary::default_sources();
        let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
        assert!(names.contains(&"piano"), "{names:?}");
        assert!(names.contains(&"Dirt-Samples"), "{names:?}");
        assert_eq!(names.last(), Some(&"gm soundfonts"));
        assert!(
            !names.contains(&"tidal-drum-machines-alias"),
            "an alias map brings no files: {names:?}"
        );
        for source in sources {
            assert!(
                source.url.starts_with("https://"),
                "a shipped pack lives on the network: {}",
                source.url
            );
            assert!(!source.base.is_empty());
        }
        assert_eq!(
            default_sources_from("not json", "{}"),
            Vec::<DefaultSource>::new()
        );
    }

    #[test]
    fn a_verified_manifest_cache_seeds_the_catalogue_synchronously() {
        let cache = tempfile::tempdir().expect("manifest cache");
        let url = "https://example.test/instant.json";
        let bytes = br#"{"Instant":["kick.wav","snare.wav"]}"#;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let sha256 = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::fs::write(cache_path(cache.path(), url), bytes).expect("cached manifest");
        let source = PinnedSource {
            name: "instant".into(),
            url: url.into(),
            base: Some("https://example.test/audio/".into()),
            sha256,
            category: Some(SoundCategory::Other),
        };
        let mut banks = HashMap::new();

        seed_cached_default_manifests(&mut banks, &[source], cache.path());

        assert!(banks.contains_key("Instant"));
        assert!(
            banks.contains_key("instant"),
            "case-insensitive lookup is ready too"
        );
    }

    /// Dirt-Samples is inline in the pin file, not a fetched manifest.
    /// Its row must still read how many sounds and files it holds, and
    /// survive a refresh-all that clears remote pack banks first.
    #[test]
    fn inline_dirt_samples_are_counted_and_restored_on_refresh() {
        let dirt = SampleLibrary::default_sources()
            .iter()
            .find(|source| source.name == "Dirt-Samples")
            .cloned()
            .expect("Dirt-Samples is a shipped pack");
        let library = SampleLibrary::empty_without_loading();
        let (sounds, files) = library.default_source_holds(&dirt);
        assert!(sounds > 0, "the pin file lists inline banks");
        assert!(files > 0, "each bank names wav files");
        {
            let mut banks = library.banks.write().expect("banks");
            banks.clear();
        }
        let (sounds, files) = library.default_source_holds(&dirt);
        assert!(sounds > 0, "counts fall back to the pin file");
        assert!(files > 0);
        library
            .refresh_default_sources()
            .expect("refresh enqueues remote manifests");
        let (sounds, files) = library.default_source_holds(&dirt);
        assert!(sounds > 0, "inline banks are put back after refresh");
        assert!(files > 0);
    }

    /// A pack's files are counted once each however many names reach
    /// them, only the ones under its base are its own, and a second
    /// caching pass queues nothing - the row shows the rest as here.
    #[test]
    fn a_shipped_pack_counts_its_distinct_files_and_queues_them_once() {
        let library = SampleLibrary::empty_without_loading();
        let kick: Arc<str> = Arc::from("https://example.test/drums/kick.wav");
        let snare: Arc<str> = Arc::from("https://example.test/drums/snare.wav");
        {
            let mut banks = library.banks.write().expect("banks");
            banks.insert(
                "Drums_bd".to_owned(),
                Bank::Array(vec![kick.clone(), snare.clone()]),
            );
            // The case-insensitive copy: the same files, another name.
            banks.insert(
                "drums_bd".to_owned(),
                Bank::Array(vec![kick.clone(), snare.clone()]),
            );
            banks.insert(
                "elsewhere".to_owned(),
                Bank::Array(vec![Arc::from("https://other.test/x.wav")]),
            );
        }
        let pack = DefaultSource {
            name: "drums".to_owned(),
            url: "https://example.test/drums.json".to_owned(),
            base: "https://example.test/drums".to_owned(),
        };
        assert_eq!(library.default_source_holds(&pack), (1, 2));
        assert_eq!(library.pending_cache_under(&pack.base), 0);

        let asked = library.cache_default_source(&pack);
        assert_eq!(
            asked,
            CacheRequest {
                sounds: 1,
                files: 2,
                queued: 2
            }
        );
        assert_eq!(library.pending_cache_under(&pack.base), 2);
        assert_eq!(
            library.pending_cache_under("https://other.test/"),
            0,
            "another pack's files are not this one's count"
        );
        let again = library.cache_default_source(&pack);
        assert_eq!(again.queued, 0, "claimed files are not queued twice");
        assert_eq!(again.files, 2, "but they are still the pack's files");
        let queued = std::iter::from_fn(|| library.shared.jobs.try_pop()).count();
        assert_eq!(queued, 2);
    }

    /// A user URL pack counts and queues the way a shipped pack does, and
    /// a local folder queues nothing - it is already on this machine.
    #[test]
    fn a_user_url_pack_counts_its_files_and_a_local_folder_does_not_queue() {
        let library = SampleLibrary::empty_without_loading();
        let kick: Arc<str> = Arc::from("https://github.test/kit/kick.wav");
        let snare: Arc<str> = Arc::from("https://github.test/kit/snare.wav");
        {
            let mut slots = library.shared.global_slots.lock().expect("slots");
            slots.push(GlobalSlot {
                row: 0,
                spec: "github:me/kit".to_owned(),
                kind: GlobalKind::Pack,
                banks: HashMap::from([(
                    "kick".to_owned(),
                    Bank::Array(vec![kick.clone(), snare.clone()]),
                )]),
                state: GlobalSourceState::Ready { banks: 1 },
                rewalk: false,
            });
        }
        library.global.write().expect("global").insert(
            "kick".to_owned(),
            Bank::Array(vec![kick.clone(), snare.clone()]),
        );

        assert!(!source_is_local("github:me/kit"));
        assert!(!source_is_local("https://example.test/pack.json"));
        assert!(library.is_imported_sound("kick"));
        assert!(!library.is_imported_sound("missing"));

        assert_eq!(library.import_source_holds("github:me/kit"), (1, 2));
        let asked = library.cache_import_source("github:me/kit");
        assert_eq!(
            asked,
            CacheRequest {
                sounds: 1,
                files: 2,
                queued: 2
            }
        );
        assert_eq!(library.pending_cache_for_import("github:me/kit"), 2);
        let again = library.cache_import_source("github:me/kit");
        assert_eq!(again.queued, 0, "claimed files are not queued twice");

        let local = if cfg!(windows) {
            "C:\\Users\\me\\drums"
        } else {
            "/tmp/drums"
        };
        assert!(source_is_local(local));
        let local_asked = library.cache_import_source(local);
        assert_eq!(local_asked.queued, 0, "a folder on disk is not downloaded");
    }

    /// The soundfonts are one pack: the files are the `.js` under the
    /// font base, once each across every instrument that shares them.
    #[test]
    fn the_soundfonts_are_one_shipped_pack() {
        let library = SampleLibrary::with_background_loaders_at(
            HashMap::new(),
            Vec::new(),
            HashMap::from([
                (
                    "gm_piano".to_owned(),
                    vec![Arc::from("font0"), Arc::from("font1")],
                ),
                ("gm_organ".to_owned(), vec![Arc::from("font1")]),
            ]),
            "https://example.test/sound".to_owned(),
            cache_dir(),
            Loading::NotStarted,
        )
        .expect("library");
        let fonts = DefaultSource {
            name: "gm soundfonts".to_owned(),
            url: "https://example.test/sound".to_owned(),
            base: "https://example.test/sound".to_owned(),
        };
        assert_eq!(library.default_source_holds(&fonts), (2, 2));
        let asked = library.cache_default_source(&fonts);
        assert_eq!(
            asked,
            CacheRequest {
                sounds: 2,
                files: 2,
                queued: 2
            }
        );
        assert_eq!(library.pending_cache_under(&fonts.base), 2);
        assert!(library.loading_under(&fonts.base).is_none());
        library
            .shared
            .loading
            .lock()
            .expect("loading")
            .insert(Arc::from("https://example.test/sound/font1.js"));
        assert_eq!(
            library.loading_under(&fonts.base).as_deref(),
            Some("font1.js")
        );
        assert!(
            library
                .loading_under("https://example.test/drums/")
                .is_none()
        );
        // Several workers can hold claimed files at once; each still keeps
        // its claim live after the queue is drained.
        while library.shared.jobs.try_pop().is_some() {}
        {
            let mut loading = library.shared.loading.lock().expect("loading");
            loading.clear();
            loading.insert(Arc::from("https://example.test/sound/font0.js"));
            loading.insert(Arc::from("https://example.test/sound/font1.js"));
        }
        assert_eq!(library.pending_cache_under(&fonts.base), 2);
        assert!(library.loading_now().is_some());
    }

    pub(super) fn two_zone_test_font() -> String {
        // Base64 of 4 s16le frames (raw `sample` zone payload).
        let raw = base64_encode(&[0u8, 0, 255, 127, 0, 128, 0, 0]);
        format!(
            "console.log('load _tone_test');\nvar _tone_test={{\n\tzones:[\n\t\t{{\n\t\t\tmidi:33\n\t\t\t,originalPitch:3100\n\t\t\t//_tone.comment\n\t\t\t,keyRangeLow:0\n\t\t\t,keyRangeHigh:28\n\t\t\t,loopStart:2\n\t\t\t,loopEnd:3\n\t\t\t,coarseTune:1\n\t\t\t,fineTune:-25\n\t\t\t,sampleRate:22050\n\t\t\t,ahdsr:true\n\t\t\t,sample:'{raw}'\n\t\t}}\n\t\t,{{\n\t\t\tmidi:45\n\t\t\t,originalPitch:4500\n\t\t\t,keyRangeLow:29\n\t\t\t,keyRangeHigh:127\n\t\t\t,loopStart:0\n\t\t\t,loopEnd:0\n\t\t\t,sampleRate:22050\n\t\t\t,sample:'{raw}'\n\t\t}}]\n}};\n"
        )
    }

    #[test]
    fn a_font_that_cannot_fit_publishes_no_orphaned_pcm_prefix() {
        let shared = sample_test_shared((SAMPLE_BANK_CAPACITY - 1) as u32);
        let error = match decode_font(two_zone_test_font().as_bytes(), &shared) {
            Err(error) => error,
            Ok(_) => panic!("two zones cannot fit in one remaining slot"),
        };
        assert!(error.contains("sample bank capacity"), "{error}");
        assert!(shared.ready.lock().expect("ready").samples.is_empty());
        assert_eq!(
            shared.next_id.load(Ordering::Relaxed),
            (SAMPLE_BANK_CAPACITY - 1) as u32
        );
    }

    /// A font's zones each keep an id of their own, so a font decoded after
    /// a release takes what is free before it takes anything fresh.
    #[test]
    fn decode_font_gives_each_zone_its_own_id_from_the_free_list() {
        let shared = sample_test_shared(100);
        shared
            .free_ids
            .lock()
            .expect("free ids")
            .extend([SampleId(3), SampleId(7)]);
        let zones = decode_font(two_zone_test_font().as_bytes(), &shared).expect("font");
        let ids: Vec<SampleId> = zones.iter().map(|zone| zone.id).collect();
        assert_eq!(ids, vec![SampleId(3), SampleId(7)], "released ids first");
        assert_eq!(
            shared.next_id.load(Ordering::Relaxed),
            100,
            "the counter did not move for reissued ids"
        );
        let published: Vec<SampleId> = shared
            .ready
            .lock()
            .expect("ready")
            .samples
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            published, ids,
            "every zone's PCM is published under its own id"
        );
    }

    /// 0253_Acoustic_Guitar writes `originalPitch:4200-140`: JavaScript,
    /// which the browser evaluates and JSON cannot. Every gm_acoustic_guitar
    /// preview failed to load on it.
    #[test]
    fn a_font_zone_written_as_arithmetic_still_decodes() {
        assert_eq!(
            fold_number_arithmetic("{\"originalPitch\":4200-140}"),
            "{\"originalPitch\":4060}"
        );
        assert_eq!(
            fold_number_arithmetic("{\"a\":6000+50-25,\"b\":-20}"),
            "{\"a\":6025,\"b\":-20}"
        );
        assert_eq!(
            fold_number_arithmetic("{\"file\":\"ab12+3-4cd\",\"x\":1}"),
            "{\"file\":\"ab12+3-4cd\",\"x\":1}",
            "a base64 payload is not arithmetic"
        );
        let font = two_zone_test_font().replace("originalPitch:3100", "originalPitch:3240-140");
        let shared = sample_test_shared(1);
        let zones = decode_font(font.as_bytes(), &shared).expect("decode");
        assert_eq!(zones[0].base_detune, 3100.0 - 100.0 - (-25.0));
    }

    /// Decode real webaudiofontdata files from disk: a developer's check
    /// against the fonts the CDN actually serves. `RUSTEL_FONT_PROBE` is a
    /// colon-separated list of `.js` paths.
    #[test]
    #[ignore]
    fn probe_real_fonts_from_disk() {
        let Ok(paths) = std::env::var("RUSTEL_FONT_PROBE") else {
            return;
        };
        for path in paths.split(':').filter(|path| !path.is_empty()) {
            let bytes = std::fs::read(path).expect("font file");
            let shared = sample_test_shared(1);
            match decode_font(&bytes, &shared) {
                Ok(zones) => eprintln!("{path}: {} zones", zones.len()),
                Err(error) => panic!("{path}: {error}"),
            }
        }
    }

    #[test]
    fn decode_font_parses_webaudiofontdata_zones_and_loop_rule() {
        let font = two_zone_test_font();
        let shared = sample_test_shared(1);
        let zones = decode_font(font.as_bytes(), &shared).expect("decode");
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].id, SampleId(1));
        assert_eq!(zones[1].id, SampleId(2));
        assert_eq!(shared.next_id.load(Ordering::Relaxed), 3);
        // baseDetune = originalPitch − 100·coarseTune − fineTune.
        assert_eq!(zones[0].base_detune, 3100.0 - 100.0 - (-25.0));
        // `loop = loopStart > 1 && loopStart < loopEnd`, seconds of the
        // zone's own timeline.
        assert_eq!(zones[0].loop_secs, Some((2.0 / 22_050.0, 3.0 / 22_050.0)));
        assert_eq!(zones[1].loop_secs, None, "loopStart 0 does not loop");
        assert_eq!(zones[0].key_lo, 0.0);
        assert_eq!(zones[0].key_hi, 28.0);
        // WebAudioFontPlayer divides s16 by 65536, not 32768.
        let ready = shared.ready.lock().expect("ready");
        assert_eq!(ready.samples.len(), 2);
        for (id, decoded) in &ready.samples {
            assert_eq!(ready.identity(*id), Some(decoded.identity()));
        }
        let pcm = ready.samples[0].1.pcm();
        assert_eq!(pcm[0], 0.0);
        assert!((pcm[1] - 32767.0 / 65536.0).abs() < 1e-6);
        assert!((pcm[2] + 0.5).abs() < 1e-6);
    }

    pub(super) fn base64_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    /// A bank answers to its own name in any case, so `.bank('Rolandmc303')`
    /// finds `RolandMC303`.
    #[test]
    fn a_bank_answers_to_its_name_in_any_case() {
        let mut banks = HashMap::new();
        banks.insert(
            "RolandMC303_bd".to_string(),
            Bank::Array(vec![std::sync::Arc::from("mc303/bd.wav")]),
        );
        banks.insert(
            "lowercase_already".to_string(),
            Bank::Array(vec![std::sync::Arc::from("plain/a.wav")]),
        );
        let before = banks.len();
        expand_case_insensitive(&mut banks);

        assert!(
            banks.contains_key("rolandmc303_bd"),
            "the name lowercased answers too"
        );
        assert!(
            banks.contains_key("RolandMC303_bd"),
            "and the catalogue's own spelling is kept"
        );
        assert_eq!(
            banks.len(),
            before + 1,
            "a name already lowercase gains no duplicate"
        );

        // A bank that really does differ only by case keeps whichever it
        // registered first rather than being quietly replaced.
        let mut collide = HashMap::new();
        collide.insert(
            "abc".to_string(),
            Bank::Array(vec![std::sync::Arc::from("first.wav")]),
        );
        collide.insert(
            "ABC".to_string(),
            Bank::Array(vec![std::sync::Arc::from("second.wav")]),
        );
        expand_case_insensitive(&mut collide);
        assert!(matches!(
            collide.get("abc"),
            Some(Bank::Array(urls)) if urls[0].as_ref() == "first.wav"
        ));
    }

    #[test]
    fn imported_and_score_banks_answer_case_insensitively_without_catalogue_duplicates() {
        let library = SampleLibrary::empty_without_loading();
        library.global.write().expect("global banks").insert(
            "MyImportedKit".into(),
            Bank::Array(vec![Arc::from("file:///imported.wav")]),
        );
        library.custom.write().expect("custom banks").insert(
            "MyScoreKit".into(),
            Bank::Array(vec![Arc::from("https://example.test/score.wav")]),
        );

        assert!(library.knows("myimportedkit"));
        assert!(library.knows("MYIMPORTEDKIT"));
        assert!(library.knows("myscorekit"));
        assert!(library.knows("MYSCOREKIT"));
        assert_eq!(
            library
                .catalogue()
                .iter()
                .filter(|entry| entry.name.eq_ignore_ascii_case("MyImportedKit"))
                .count(),
            1,
            "lookup tolerance does not add a second browser row"
        );
    }

    #[test]
    fn the_catalogue_hides_a_lowercase_lookup_alias() {
        let library = SampleLibrary::empty_without_loading();
        let bank = Bank::Array(vec![Arc::from("https://example.test/kit.wav")]);
        {
            let mut banks = library.banks.write().expect("banks");
            banks.insert("MyKit".into(), bank.clone());
            banks.insert("mykit".into(), bank);
        }

        let shown: Vec<_> = library
            .catalogue()
            .into_iter()
            .filter(|entry| entry.name.eq_ignore_ascii_case("MyKit"))
            .collect();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, "MyKit");
        assert!(library.knows("mykit"), "the hidden spelling still resolves");
    }

    #[test]
    fn alias_bank_expansion_matches_the_alias_bank_map() {
        let mut banks = HashMap::new();
        banks.insert(
            "RolandTR909_bd".to_string(),
            Bank::Array(vec![std::sync::Arc::from("tr909/bd.wav")]),
        );
        banks.insert(
            "plain".to_string(),
            Bank::Array(vec![std::sync::Arc::from("plain/a.wav")]),
        );
        // The alias JSON maps CANONICAL -> alias ("RolandTR909": "TR909");
        // the registered alias key is lowercased.
        expand_bank_aliases(
            &mut banks,
            &[("RolandTR909".to_string(), "TR909".to_string())],
        );
        assert!(banks.contains_key("tr909_bd"), "lowercased alias key");
        assert!(banks.contains_key("RolandTR909_bd"), "original key kept");
        assert!(
            !banks.keys().any(|k| k != "plain" && k.starts_with("plain")),
            "suffix-less sounds are not aliased"
        );
    }
}

impl SampleLibrary {
    /// The lookup behind [`SampleLookup::resolve`], with a say in where a
    /// fetch it starts joins the loader's line.
    fn resolve_with_priority(
        &self,
        s: &str,
        n: f64,
        midi: f64,
        priority: LoadPriority,
    ) -> SampleResolution {
        enum Chosen {
            Font(SampleResolution),
            Sample(Arc<str>, f64),
        }
        let chosen = self.look_up(s, |named| {
            let bank = match named {
                Named::Font(fonts) => {
                    return Chosen::Font(self.resolve_soundfont(fonts, n, midi, priority));
                }
                Named::Bank(bank) => bank,
            };
            let (url, transpose) = pick_from_bank(bank, n, midi);
            Chosen::Sample(url, transpose)
        });
        let (url, transpose) = match chosen {
            Some(Chosen::Font(resolution)) => return resolution,
            Some(Chosen::Sample(url, transpose)) => (url, transpose),
            None => {
                return if self.shared.manifest_pending.load(Ordering::Acquire) != 0 {
                    SampleResolution::Loading
                } else {
                    SampleResolution::Unknown
                };
            }
        };
        match ensure_loading_shared(&self.shared, &url, DecodeRate::for_sound(s), priority) {
            SampleResolution::Found {
                id, duration_secs, ..
            } => SampleResolution::Found {
                id,
                transpose,
                duration_secs,
                loop_secs: None,
                envelope_peak: 1.0,
                soundfont: false,
            },
            other => other,
        }
    }
}

impl SampleLookup for SampleLibrary {
    fn resolve(&self, s: &str, n: f64, midi: f64) -> SampleResolution {
        // A sound the engine asks for is one about to play.
        self.resolve_with_priority(s, n, midi, LoadPriority::Now)
    }
}
