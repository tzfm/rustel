use std::sync::Mutex;

use rustel_core::extension_node::{ExtensionPatternNode, push_hap};
use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{DeclaredIn, Registry, add_in};
use rustel_core::settings::SettingsStateId;
use rustel_core::{Hap, OrderedMap, Pattern, State, TimeSpan};
use rustel_fraction::Fraction;

use super::{NativeExtensionOperand, ORIGIN};

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "fill",
    synonyms: &[],
    summary: "hold each event until the next one starts",
    description: "Stretches every event's whole so it ends where the next one begins, filling the gaps that mask, degrade and sometimesBy leave behind. A sustained sound then rings through the rest until something replaces it, rather than stopping on the grid.",
    params: &[],
    examples: &["s(\"bd*8\").sometimesBy(0.5, x => x.mask(rand.round())).fill()"],
    tags: &["switch angel", "time"],
    no_autocomplete: false,
    deprecated: false,
    origin: "switch angel",
};

/// The most answered queries one `fill` keeps.
///
/// `fill` reads a cycle either side of whatever it is asked for, so a
/// sixteenth-cycle question costs what two whole cycles cost. Under `struct`
/// that question is asked once per gate step, and a scheduler tick widened by
/// an outer `fill` covers some thirty steps - the same thirty spans, tick
/// after tick, and under `rib` the same spans every phrase. Keeping the
/// answers is what turns that from thirty two-cycle evaluations a tick into
/// one. Thirty-odd steps a phrase fit here with room for a four-cycle phrase
/// and the tick boundaries around it.
const FILL_CACHE_ENTRIES: usize = 256;
/// The most haps the cache holds in total, so a dense pattern cannot turn
/// the bound above into megabytes.
const FILL_CACHE_HAPS: usize = 16_384;

/// One answered query: the state it was asked with, and what it answered.
struct FillEntry {
    span: TimeSpan,
    controls: OrderedMap,
    settings: SettingsStateId,
    haps: Vec<Hap>,
}

#[derive(Default)]
struct FillCache {
    entries: Vec<FillEntry>,
    /// Haps across every entry, against `FILL_CACHE_HAPS`.
    haps: usize,
    /// Where the next replacement lands once full. Round-robin rather than
    /// least-recently-used: the spans a gate walks come round in a cycle,
    /// and LRU on a cycle one entry longer than the cache hits nothing.
    victim: usize,
}

impl FillCache {
    fn find(&self, state: &State, settings: &SettingsStateId) -> Option<&FillEntry> {
        self.entries.iter().find(|entry| {
            entry.span == state.span
                && entry.settings == *settings
                && entry.controls == state.controls
        })
    }

    fn insert(&mut self, state: &State, settings: SettingsStateId, haps: Vec<Hap>) {
        if haps.len() > FILL_CACHE_HAPS {
            return;
        }
        let entry = FillEntry {
            span: state.span,
            controls: state.controls.clone(),
            settings,
            haps,
        };
        while !self.entries.is_empty()
            && (self.entries.len() >= FILL_CACHE_ENTRIES
                || self.haps + entry.haps.len() > FILL_CACHE_HAPS)
        {
            let victim = self.victim % self.entries.len();
            self.victim = self.victim.wrapping_add(1);
            let evicted = self.entries.swap_remove(victim);
            self.haps -= evicted.haps.len();
        }
        self.haps += entry.haps.len();
        self.entries.push(entry);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.haps = 0;
    }
}

struct FillNode {
    pattern: Option<Pattern>,
    /// Whether the child is a function of the query alone - no JavaScript,
    /// nothing live, nothing remembered - so its answers may be kept.
    cacheable: bool,
    cache: Mutex<FillCache>,
}

impl FillNode {
    fn cache(&self) -> std::sync::MutexGuard<'_, FillCache> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The semantics: read a cycle either way, stretch every whole to the
    /// next onset, and keep what reaches into the asked-for span.
    fn evaluate(pattern: &Pattern, state: &State) -> Vec<Hap> {
        let original = state.span;
        let haps = pattern.query(&state.with_span(|span| {
            TimeSpan::new(span.begin.sub(Fraction::ONE), span.end.add(Fraction::ONE))
        }));
        if haps.iter().any(|hap| hap.whole.is_none()) {
            rustel_core::signal_query_error(|| {
                "Cannot read properties of undefined (reading 'begin')".into()
            });
            return Vec::new();
        }
        let mut onsets: Vec<Fraction> = haps
            .iter()
            .map(|hap| hap.whole.expect("discrete").begin)
            .collect();
        onsets.sort();
        onsets.dedup();
        let mut output = Vec::new();
        for mut hap in haps {
            if hap.part.begin >= original.end {
                continue;
            }
            let whole = hap.whole.expect("discrete");
            let Some(&next) = onsets.get(onsets.partition_point(|onset| *onset < whole.end)) else {
                continue;
            };
            if next <= original.begin {
                continue;
            }
            hap.whole = Some(TimeSpan::new(whole.begin, next));
            hap.part = TimeSpan::new(hap.part.begin.max(original.begin), next.min(original.end));
            output.push(hap);
        }
        output
    }
}

impl ExtensionPatternNode for FillNode {
    fn kind(&self) -> &'static str {
        "switch_angel.fill"
    }

    fn children(&self) -> &[Pattern] {
        self.pattern.as_slice()
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        let pattern = self.pattern.as_ref().expect("live extension node");
        if !self.cacheable {
            return Self::evaluate(pattern, state);
        }
        let settings = rustel_core::settings::current_state_id();
        let kept = self
            .cache()
            .find(state, &settings)
            .map(|entry| entry.haps.clone());
        if let Some(kept) = kept {
            // Replayed through the live budget, charged for what it hands
            // back. A cold evaluation also charged the two-cycle window it
            // read on the way; a replay reads no window, so it owes none.
            let mut output = Vec::with_capacity(kept.len());
            for hap in kept {
                if !push_hap(&mut output, hap) {
                    return Vec::new();
                }
            }
            return output;
        }
        let touches = rustel_core::uncacheable_touches();
        let output = Self::evaluate(pattern, state);
        // An interrupted query - an error, a refusal, a deadline, a
        // cancellation - answered less than it should have, and the boundary
        // is about to discard it. Only a complete answer is worth keeping.
        // And the child's static classification is not the last word: a
        // pattern-of-patterns inside it may have materialised something
        // volatile just now, which core counts as it is reached.
        if output.len() <= FILL_CACHE_HAPS
            && !rustel_core::query_interrupted()
            && rustel_core::uncacheable_touches() == touches
        {
            self.cache().insert(state, settings, output.clone());
        }
        output
    }

    /// Nothing of its own between queries: the cache only remembers what the
    /// child answered, and is keyed by everything that answer depends on.
    fn volatile(&self) -> bool {
        false
    }

    fn drain_children(&mut self, out: &mut Vec<Pattern>) {
        self.cache().clear();
        out.extend(self.pattern.take());
    }
}

/// Switch Angel's `fill` semantics and native graph node.
pub(crate) fn apply<P: NativeExtensionOperand>(pattern: &P) -> P {
    let pattern = pattern.pattern_handle();
    let cacheable = pattern.is_cacheable();
    P::from_extension_node(FillNode {
        pattern: Some(pattern),
        cacheable,
        cache: Mutex::new(FillCache::default()),
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["fill"],
        REFERENCE,
        1,
        false,
        rustel_core::native_combinator!(|_args, pattern| apply(&pattern)),
    );
}
