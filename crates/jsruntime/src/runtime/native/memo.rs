//! What a native build of a score keeps for the build after it.
//!
//! A score is evaluated again after each edit, and most of its text is then
//! the text of the evaluation before. A call builds the same graph when its
//! text is the same, at the same place in the score, under the same
//! reading, and each name of the score that it reads means what it meant.
//! The build then takes the graph that the call built before, and builds
//! only what the edit changed.
//!
//! A part is a call that a build may keep: one that the score writes as a
//! statement, as the value of a binding or of a property, or as an argument
//! of a call of a name. An element of an array and the result of a function
//! are no parts: a pick writes the patterns of its array where they are,
//! and so does a control of the result of a call, which a handle here would
//! make a copy first. A part is kept when it made a pattern, ran no effect of the
//! score, and read no name of a function call or an arrow around it: its
//! pattern is then a function of its text and of the names of the score
//! that it read.
//!
//! A name of the score has a version. The version stays while the text of
//! the statement of the name is the same and each name that the statement
//! read keeps its version. A part holds the version of each name that it
//! read, through each function that it called, and is taken again only
//! while each is the version in force.
//!
//! A kept pattern has a handle here and a handle in the graph in play, so
//! a constructor that writes a node where no other handle holds it writes
//! a copy. A list that folds its arguments into one node leaves each
//! argument held here alone, which costs the memory of its node and saves
//! the build of it at the next edit of the list.

use rustel_core::{Pattern, PatternHandleMeta, PatternNodeRef, PatternSharedExtras};
use rustel_transpiler::{ScoreProgram, ScoreStatement, Written};
use std::mem;
use std::sync::Arc;

/// Sentinel version for an unread or out-of-range name.
const UNREAD_VERSION: u16 = u16::MAX;
/// Compact sentinel for [`Written::slots`] when a span holds no widgets.
const NO_SLOTS: u16 = u16::MAX;
/// Bit in packed `u16` length fields carrying [`Written::mini`].
const MINI_FLAG: u16 = 0x8000;
const LEN_MASK: u16 = 0x7fff;

/// A slice `(start, len)` into a flat pool of `(name_number, version)` pairs.
#[derive(Clone, Copy, Default)]
struct ReadSpan {
    start: u16,
    len: u16,
}

impl ReadSpan {
    #[inline]
    fn get(self, pool: &[(u16, u16)]) -> &[(u16, u16)] {
        let start = usize::from(self.start);
        let end = start + usize::from(self.len);
        &pool[start..end]
    }
}

#[inline]
fn pack_slots(slots: u32) -> Option<u16> {
    if slots == u32::MAX {
        Some(NO_SLOTS)
    } else {
        u16::try_from(slots).ok().filter(|&v| v != NO_SLOTS)
    }
}

#[inline]
fn unpack_slots(slots: u16) -> u32 {
    if slots == NO_SLOTS {
        u32::MAX
    } else {
        u32::from(slots)
    }
}

/// One kept part: where the score writes it, its compact 8-byte graph node
/// handle, inline small `_steps` + node flags, and what it read. Packs into
/// 24 bytes for scores up to 64 KiB; rare wide `_steps` or `PatternExtras`
/// spill to sparse [`Memo::part_meta`], and widget slots to [`Memo::part_slots`].
struct Part {
    node: PatternNodeRef,
    start: u16,
    len: u16,
    read_start: u16,
    read_len_flags: u16,
    step_numer: i16,
    step_denom: u16,
    flags: u8,
    meta_kind: u8,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Part>() == 24);

struct MadePart {
    part: Part,
    meta: Option<PatternHandleMeta>,
    slot: u16,
}

impl Part {
    #[inline]
    fn from_written(
        written: Written,
        pattern: Pattern,
        read: ReadSpan,
        shared_extras: &mut Option<PatternSharedExtras>,
    ) -> Option<MadePart> {
        let start = u16::try_from(written.start).ok()?;
        let len = u16::try_from(written.len).ok()?;
        let slot = pack_slots(written.slots)?;
        if read.len > LEN_MASK {
            return None;
        }
        let read_len_flags = read.len | if written.mini { MINI_FLAG } else { 0 };
        let (node, flags, step_numer, step_denom, meta_kind, meta) =
            pattern.split_compact(shared_extras);
        Some(MadePart {
            part: Self {
                node,
                start,
                len,
                read_start: read.start,
                read_len_flags,
                step_numer,
                step_denom,
                flags,
                meta_kind,
            },
            meta,
            slot,
        })
    }

    #[inline]
    fn read(&self) -> ReadSpan {
        ReadSpan {
            start: self.read_start,
            len: self.read_len_flags & LEN_MASK,
        }
    }

    #[inline]
    fn set_read(&mut self, read: ReadSpan) {
        self.read_start = read.start;
        self.read_len_flags = (self.read_len_flags & MINI_FLAG) | (read.len & LEN_MASK);
    }

    #[inline]
    fn written(&self, slots: u16) -> Written {
        Written {
            start: u32::from(self.start),
            len: u32::from(self.len),
            slots: unpack_slots(slots),
            mini: (self.read_len_flags & MINI_FLAG) != 0,
        }
    }

    /// The place of the part among the parts: where its text starts, then
    /// how long the text is.
    fn key(&self) -> (u16, u16) {
        (self.start, self.len)
    }
}

/// One name of the score: where the score writes its statement, its
/// version, its byte slice inside [`Memo::source`], and what the statement read.
struct Name {
    start: u16,
    len: u16,
    text_start: u16,
    version: u16,
    slots: u16,
    read: ReadSpan,
    text_len_flags: u16,
}

const _: () = assert!(std::mem::size_of::<Name>() <= 16);

impl Name {
    #[inline]
    fn written(&self) -> Written {
        Written {
            start: u32::from(self.start),
            len: u32::from(self.len),
            slots: unpack_slots(self.slots),
            mini: (self.text_len_flags & MINI_FLAG) != 0,
        }
    }
}

/// Trailing single-number method kind eligible for incremental reuse when only
/// its literal argument changes in a single-statement binding-free score.
#[derive(Clone, Copy)]
pub(in crate::runtime) enum TailNumericKind {
    Fast,
    Slow,
    Early,
    Late,
}

/// Compact descriptor for an edit that only changes the numeric literal inside
/// a trailing `.fast(n)` / `.slow(n)` / `.early(n)` / `.late(n)` call whose
/// receiver is retained in [`Memo::parts`].
#[derive(Clone, Copy)]
pub(in crate::runtime) struct TailNumericEdit {
    pub(in crate::runtime) kind: TailNumericKind,
    pub(in crate::runtime) part_idx: u8,
    pub(in crate::runtime) arg_start: u16,
    pub(in crate::runtime) suffix_len: u16,
}

/// What the last native build of a score kept.
#[derive(Default)]
pub(in crate::runtime) struct Memo {
    /// The text of the score of that build, one handle of it: the session
    /// that keeps the text for a replay shares this handle.
    /// None before the first build.
    pub(in crate::runtime) source: Option<Arc<str>>,
    /// The kept parts, in the order of their places.
    parts: Box<[Part]>,
    /// Sparse `(part_index, meta)` for parts whose handles carry wide `_steps`
    /// or non-shared `PatternExtras`; empty for ordinary native scores.
    part_meta: Box<[(u16, PatternHandleMeta)]>,
    /// Shared plain `runtime_settings` handle reused across parts that carry
    /// the session's own runtime settings without per-part `part_meta` entries.
    shared_extras: Option<PatternSharedExtras>,
    /// Parallel widget slot indices for [`Self::parts`]; empty when no part
    /// carries a widget slot.
    part_slots: Box<[u16]>,
    /// The names of the score, each at its number: the place of its
    /// statement among the statements with a name.
    names: Box<[Name]>,
    /// Flat pool of `(name_number, version)` pairs read by parts and names.
    reads: Box<[(u16, u16)]>,
    /// The numbers of the names, in the order of the texts of the names.
    by_text: Box<[u16]>,
    /// The version that the next new or changed name takes.
    next: u16,
    /// The tempo that the last native build set, if any.
    pub(in crate::runtime) cps: Option<f64>,
    /// Optional trailing numeric method descriptor for direct single-number edits.
    pub(in crate::runtime) tail_num: Option<TailNumericEdit>,
}

impl Memo {
    pub(in crate::runtime) fn remap_nodes(&mut self, f: &mut dyn FnMut(&mut PatternNodeRef)) {
        for part in self.parts.iter_mut() {
            f(&mut part.node);
        }
    }

    #[inline]
    pub(in crate::runtime) fn has_no_names(&self) -> bool {
        self.names.is_empty()
    }

    #[inline]
    pub(in crate::runtime) fn part_index_of(&self, written: Written) -> Option<u8> {
        let key = (
            u16::try_from(written.start).ok()?,
            u16::try_from(written.len).ok()?,
        );
        let at = self.parts.binary_search_by_key(&key, Part::key).ok()?;
        u8::try_from(at).ok()
    }

    #[inline]
    pub(in crate::runtime) fn part_pattern(&self, index: usize) -> Pattern {
        let part = &self.parts[index];
        let meta = u16::try_from(index)
            .ok()
            .and_then(|idx| self.part_meta.binary_search_by_key(&idx, |(i, _)| *i).ok())
            .map(|at| &self.part_meta[at].1);
        Pattern::from_compact(
            &part.node,
            part.flags,
            part.step_numer,
            part.step_denom,
            part.meta_kind,
            self.shared_extras.as_ref(),
            meta,
        )
    }

    #[inline]
    fn part_slot(&self, index: usize) -> u16 {
        self.part_slots.get(index).copied().unwrap_or(NO_SLOTS)
    }

    #[inline]
    fn name_text(&self, index: usize) -> &str {
        let Some(name) = self.names.get(index) else {
            return "";
        };
        let Some(source) = self.source.as_deref() else {
            return "";
        };
        let start = usize::from(name.text_start);
        let len = usize::from(name.text_len_flags & LEN_MASK);
        source.get(start..start + len).unwrap_or("")
    }
}

#[inline]
fn subslice_span(haystack: &str, needle: &str) -> Option<(u16, u16)> {
    let base = haystack.as_ptr() as usize;
    let ptr = needle.as_ptr() as usize;
    let offset = ptr.checked_sub(base)?;
    let end = offset.checked_add(needle.len())?;
    if end > haystack.len() || haystack.get(offset..end) != Some(needle) {
        return None;
    }
    let len = u16::try_from(needle.len()).ok()?;
    if len > LEN_MASK {
        return None;
    }
    Some((u16::try_from(offset).ok()?, len))
}

/// How many bytes two texts are compared at a time.
const WIDTH: usize = 16;

/// How many of the first bytes of `a` and of `b` are the same.
fn same_start(a: &[u8], b: &[u8]) -> usize {
    let chunks = a
        .as_chunks::<WIDTH>()
        .0
        .iter()
        .zip(b.as_chunks::<WIDTH>().0);
    let from = chunks.take_while(|(a, b)| a == b).count() * WIDTH;
    let rest = a[from..].iter().zip(&b[from..]);
    from + rest.take_while(|(a, b)| a == b).count()
}

/// How many of the last bytes of `a` and of `b` are the same.
fn same_end(a: &[u8], b: &[u8]) -> usize {
    let chunks = a.as_rchunks::<WIDTH>().1.iter().rev();
    let chunks = chunks.zip(b.as_rchunks::<WIDTH>().1.iter().rev());
    let from = chunks.take_while(|(a, b)| a == b).count() * WIDTH;
    let rest = a[..a.len() - from].iter().rev();
    let rest = rest.zip(b[..b.len() - from].iter().rev());
    from + rest.take_while(|(a, b)| a == b).count()
}

/// Where the text of a build is the text of the build before: an edit
/// changes a few bytes of a score, and each part reads the others as they
/// were.
struct Unchanged {
    /// How many of the first bytes of the two texts are the same.
    start: usize,
    /// The byte from which the two texts are the same to their end, when
    /// they are of one length: a byte is then at the same place in both.
    end: Option<usize>,
}

impl Unchanged {
    fn of(source: &str, before: &str) -> Self {
        let (source, before) = (source.as_bytes(), before.as_bytes());
        let start = same_start(source, before);
        let end = (source.len() == before.len())
            .then(|| source.len() - same_end(&source[start..], &before[start..]));
        Self { start, end }
    }

    /// Whether the text from byte `start` to byte `end`, which each of the
    /// two texts holds, is the same in both.
    fn holds(&self, start: usize, end: usize) -> bool {
        end <= self.start || self.end.is_some_and(|same| same <= start)
    }
}

/// A part or a statement that a build evaluates now.
struct Open {
    /// Where the names that it read start in the list of the build. What
    /// a part reads, each part and statement around it reads too, so the
    /// list of each ends where the list of the build ends.
    read: usize,
    /// How many function calls and arrows were in progress when it began.
    frames: usize,
    /// The count of effects of the build when it began.
    effects: u32,
    /// Whether it read a name of a call or an arrow around it.
    bound: bool,
}

/// An evaluated part or statement: where the names that it read start in
/// the list of the build, and whether its value is a function of its text
/// and of those names alone.
pub(super) struct Closed {
    read: usize,
    kept: bool,
}

/// One name of the program that a build evaluates.
struct Current<'p> {
    text: &'p str,
    written: Written,
    /// The number of the name among the names of the build before.
    was: Option<usize>,
    /// The version of the name, when the build asked for it.
    version: Option<u16>,
    /// What the statement of the name read, when the build evaluated it
    /// (indexes [`Reuse::made_reads`]).
    read: Option<ReadSpan>,
}

/// One build's use of the memo of the build before: it takes parts from
/// that memo and makes the memo of this build.
pub(super) struct Reuse<'p> {
    source: &'p str,
    before: Memo,
    /// Where `source` is the text of `before`.
    unchanged: Unchanged,
    /// Whether this build took each part of `before`, or a part around it.
    /// Inline bitset for up to 64 parts; overflows to `Vec`.
    taken_bits: u64,
    taken_overflow: Vec<bool>,
    /// The parts that this build evaluated and keeps, paired with their metadata.
    made: Vec<(MadePart, bool)>,
    /// Shared plain `runtime_settings` extras handle across parts in this build.
    shared_extras: Option<PatternSharedExtras>,
    /// Flat pool of `(name_number, version)` pairs recorded during this build.
    made_reads: Vec<(u16, u16)>,
    /// The names of the program, each at its number.
    names: Vec<Current<'p>>,
    /// The number that each name of `before` has in this build.
    now: Vec<Option<u32>>,
    open: Vec<Open>,
    /// The names of the score that the open parts and statements read, in
    /// the order of the reads.
    read: Vec<u32>,
    /// Reusable scratch buffer for node-identity deduplication in [`Self::finish`].
    ids: Vec<usize>,
    /// How many effects of the score the build ran.
    effects: u32,
    next: u16,
}

#[derive(Default)]
struct ReuseScratch {
    made: Vec<(MadePart, bool)>,
    made_reads: Vec<(u16, u16)>,
    open: Vec<Open>,
    read: Vec<u32>,
    ids: Vec<usize>,
}

thread_local! {
    /// Reusable cleared buffers for [`Reuse`] so repeated evaluations avoid
    /// allocating temporary build vectors while holding zero node references.
    static SPARE_REUSE: std::cell::RefCell<Option<ReuseScratch>> =
        const { std::cell::RefCell::new(None) };
}

impl Drop for Reuse<'_> {
    fn drop(&mut self) {
        self.made.clear();
        self.made_reads.clear();
        self.open.clear();
        self.read.clear();
        self.ids.clear();
        if self.made.capacity() <= 512
            && self.made_reads.capacity() <= 1024
            && self.open.capacity() <= 256
            && self.read.capacity() <= 1024
            && self.ids.capacity() <= 512
        {
            let scratch = ReuseScratch {
                made: mem::take(&mut self.made),
                made_reads: mem::take(&mut self.made_reads),
                open: mem::take(&mut self.open),
                read: mem::take(&mut self.read),
                ids: mem::take(&mut self.ids),
            };
            let _ = SPARE_REUSE.try_with(|cell| *cell.borrow_mut() = Some(scratch));
        }
    }
}

impl<'p> Reuse<'p> {
    /// The use of `before` for a build of `program`, which was read from
    /// `source`.
    pub(super) fn new(source: &'p str, before: Memo, program: &'p ScoreProgram<'p>) -> Self {
        let name_count = program
            .statements
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    ScoreStatement::Binding { .. } | ScoreStatement::Function { .. }
                )
            })
            .count();
        let mut names: Vec<Current<'p>> = Vec::with_capacity(name_count);
        for statement in &program.statements {
            match statement {
                ScoreStatement::Binding { name, written, .. }
                | ScoreStatement::Function { name, written, .. } => names.push(Current {
                    text: program.held.text(*name),
                    written: *written,
                    was: None,
                    version: None,
                    read: None,
                }),
                ScoreStatement::Expression(_) => {}
            }
        }
        // The same name of the build before, found by its text.
        let mut now = if before.names.is_empty() {
            Vec::new()
        } else {
            vec![None; before.names.len()]
        };
        if !before.by_text.is_empty() {
            for (number, name) in names.iter_mut().enumerate() {
                let target = (name.text.len(), name.text);
                let found = before.by_text.binary_search_by_key(&target, |was| {
                    let t = before.name_text(*was as usize);
                    (t.len(), t)
                });
                if let (Ok(at), Ok(number)) = (found, u32::try_from(number)) {
                    let was = before.by_text[at] as usize;
                    name.was = Some(was);
                    now[was] = Some(number);
                }
            }
        }
        // Drop kept parts and name versions when the set of declared names
        // changed (a new binding can shadow a host global that a kept part
        // resolved before without recording a read), or when assigning fresh
        // versions in this build could wrap `next` back onto an old version.
        let same_declared_names = names.len() == before.names.len()
            && names.iter().all(|name| name.was.is_some())
            && now.iter().all(Option::is_some);
        let would_wrap =
            usize::from(before.next).saturating_add(names.len()) >= usize::from(UNREAD_VERSION);
        let (before, next) = if same_declared_names && !would_wrap {
            let next = before.next;
            (before, next)
        } else {
            for name in &mut names {
                name.was = None;
            }
            now.clear();
            (Memo::default(), 0)
        };
        let shared_extras = before.shared_extras.clone();
        let mut scratch = SPARE_REUSE
            .try_with(|cell| cell.borrow_mut().take())
            .ok()
            .flatten()
            .unwrap_or_default();
        scratch.made.clear();
        scratch.made_reads.clear();
        scratch.open.clear();
        scratch.read.clear();
        scratch.ids.clear();
        let unchanged = if before.parts.is_empty() && before.names.is_empty() {
            Unchanged {
                start: 0,
                end: None,
            }
        } else {
            match before.source.as_deref() {
                Some(prev) => Unchanged::of(source, prev),
                None => Unchanged {
                    start: 0,
                    end: None,
                },
            }
        };
        let taken_overflow = if before.parts.len() <= 64 {
            Vec::new()
        } else {
            vec![false; before.parts.len()]
        };
        Self {
            source,
            unchanged,
            taken_bits: 0,
            taken_overflow,
            next,
            before,
            made: scratch.made,
            shared_extras,
            made_reads: scratch.made_reads,
            names,
            now,
            open: scratch.open,
            read: scratch.read,
            ids: scratch.ids,
            effects: 0,
        }
    }

    /// Whether `written` in the text of this build and `kept` in the text
    /// of the build before are the same text at the same place, under the
    /// same reading.
    fn same_text(&self, written: &Written, kept: &Written) -> bool {
        let same_place = (written.start, written.len, written.slots, written.mini)
            == (kept.start, kept.len, kept.slots, kept.mini);
        let Some(text) = written.text(self.source).filter(|_| same_place) else {
            return false;
        };
        let start = written.start as usize;
        self.unchanged.holds(start, start + text.len())
            || Some(text) == kept.text(self.before.source.as_deref().unwrap_or_default())
    }

    /// A part or a statement begins, with `frames` function calls and
    /// arrows in progress.
    pub(super) fn begin(&mut self, frames: usize) {
        // With nothing open, no part reads what the list holds.
        if self.open.is_empty() {
            self.read.clear();
        }
        self.open.push(Open {
            read: self.read.len(),
            frames,
            effects: self.effects,
            bound: false,
        });
    }

    /// The part or the statement that began last ends.
    pub(super) fn end(&mut self) -> Closed {
        match self.open.pop() {
            Some(open) => Closed {
                read: open.read,
                kept: !open.bound && open.effects == self.effects,
            },
            None => Closed {
                read: self.read.len(),
                kept: false,
            },
        }
    }

    /// The build read the name of the score with `number`.
    pub(super) fn reads(&mut self, number: u32) {
        if !self.open.is_empty() {
            self.read.push(number);
        }
    }

    /// The build read a name of the function call or the arrow that is
    /// number `frame` of those in progress. A part that began inside that
    /// call has a value for each call, not one for its text.
    pub(super) fn reads_frame(&mut self, frame: usize) {
        for open in &mut self.open {
            open.bound |= open.frames > frame;
        }
    }

    /// The build ran an effect of the score: a part with an effect is
    /// evaluated at each build.
    pub(super) fn effect(&mut self) {
        self.effects += 1;
    }

    /// The pattern that the build before made of the call at `written`,
    /// when it is the pattern that this build makes of it.
    pub(super) fn find(&mut self, written: &Written) -> Option<Pattern> {
        if self.before.parts.is_empty() {
            return None;
        }
        let key = (
            u16::try_from(written.start).ok()?,
            u16::try_from(written.len).ok()?,
        );
        let at = self
            .before
            .parts
            .binary_search_by_key(&key, Part::key)
            .ok()?;
        let kept_written = self.before.parts[at].written(self.before.part_slot(at));
        if !self.same_text(written, &kept_written) {
            return None;
        }
        // Each name that the part read is a name of this build, with the
        // version that the part read.
        let read_span = self.before.parts[at].read();
        let count = usize::from(read_span.len);
        for index in 0..count {
            let (was, version) = self.before.reads[usize::from(read_span.start) + index];
            let number = self.now.get(usize::from(was)).copied().flatten()?;
            if self.version(number) != version {
                return None;
            }
        }
        // What the part read, the part or the statement around it reads.
        if !self.open.is_empty() {
            let now = &self.now;
            let numbers = read_span.get(&self.before.reads).iter();
            let numbers =
                numbers.filter_map(|(was, _)| now.get(usize::from(*was)).copied().flatten());
            self.read.extend(numbers);
        }
        // The parts inside the call stay with it: each starts after the
        // start of the call and before its end.
        let end = written.start.saturating_add(written.len);
        let inside = self.before.parts[at..]
            .iter()
            .take_while(|part| u32::from(part.start) < end)
            .count();
        if self.taken_overflow.is_empty() {
            for idx in at..at + inside {
                self.taken_bits |= 1u64 << idx;
            }
        } else {
            self.taken_overflow[at..at + inside].fill(true);
        }
        // A handle of the kept pattern.
        Some(self.before.part_pattern(at))
    }

    /// Keep `pattern`, which the build made of the call at `written` and
    /// which `closed` says is a function of its text and of what it read.
    pub(super) fn keep(&mut self, written: Written, pattern: &Pattern, closed: Closed) {
        if !closed.kept {
            return;
        }
        let Some(read) = self.versions_of(closed.read) else {
            return;
        };
        let Some(entry) =
            Part::from_written(written, pattern.clone(), read, &mut self.shared_extras)
        else {
            return;
        };
        self.made.push((entry, false));
    }

    /// How many parts the build keeps so far: a mark for [`Self::release`].
    pub(super) fn mark(&self) -> usize {
        self.made.len()
    }

    /// Whether [`Self::finish`] retains unshared sub-parts for tiny
    /// binding-free scores.
    pub(super) fn keeps_unshared(&self) -> bool {
        self.names.is_empty() && self.source.len() <= 1024
    }

    /// Let go of each part kept since `mark` whose pattern no graph and no
    /// value holds now: an argument of a function of the score that the
    /// body made another pattern of. The argument of a list is kept, also
    /// when the list folded it into its node: the node of the argument is
    /// then held here alone, and the next edit of the list builds only the
    /// arguments that changed.
    pub(super) fn release(&mut self, mark: usize) {
        let mut at = mark;
        while at < self.made.len() {
            if self.made[at].0.part.node.strong_count() > 1 {
                at += 1;
            } else {
                // The order of the parts is made when the build ends.
                self.made.swap_remove(at);
            }
        }
    }

    /// The statement of the name of the score with `number` read what
    /// `closed` says.
    pub(super) fn named(&mut self, number: u32, closed: Closed) {
        if self
            .names
            .get(number as usize)
            .is_some_and(|name| name.read.is_none())
            && let Some(read) = self.versions_of(closed.read)
        {
            self.names[number as usize].read = Some(read);
        }
    }

    /// Each name that the build read from place `start` of its list on, one
    /// time, with its version, appended to [`Self::made_reads`]. The list then
    /// holds each of them one time: it is the end of the list of each part and
    /// statement that is open.
    fn versions_of(&mut self, start: usize) -> Option<ReadSpan> {
        let Some(read) = self.read.get_mut(start..).filter(|read| !read.is_empty()) else {
            return Some(ReadSpan::default());
        };
        read.sort_unstable();
        let mut kept = 1;
        for at in 1..read.len() {
            if read[at] != read[kept - 1] {
                read[kept] = read[at];
                kept += 1;
            }
        }
        self.read.truncate(start + kept);
        let pool_start = u16::try_from(self.made_reads.len()).ok()?;
        let pool_len = u16::try_from(kept).ok()?;
        self.made_reads.reserve(kept);
        for at in start..start + kept {
            let number = self.read[at];
            let number_u16 = u16::try_from(number).ok()?;
            let version = self.version(number);
            self.made_reads.push((number_u16, version));
        }
        Some(ReadSpan {
            start: pool_start,
            len: pool_len,
        })
    }

    /// The version of the name with `number`: the version of the build
    /// before when the text of its statement is the same and each name
    /// that the statement read keeps its version, and a new one otherwise.
    fn version(&mut self, number: u32) -> u16 {
        let at = number as usize;
        // A number of no name is of no build: it has a version of its own.
        let Some(name) = self.names.get(at) else {
            return UNREAD_VERSION;
        };
        if let Some(version) = name.version {
            return version;
        }
        // A name that reads itself, through any number of names, is new.
        if self.next == UNREAD_VERSION {
            self.names[at].version = Some(UNREAD_VERSION);
            return UNREAD_VERSION;
        }
        let new = self.next;
        self.next = new + 1;
        self.names[at].version = Some(new);
        let Some(was) = self.names[at].was else {
            return new;
        };
        let kept_written = self.before.names[was].written();
        if !self.same_text(&self.names[at].written, &kept_written) {
            return new;
        }
        let version = self.before.names[was].version;
        let read_span = self.before.names[was].read;
        for index in 0..usize::from(read_span.len) {
            let (read_was, read_version) = self.before.reads[usize::from(read_span.start) + index];
            let now = self.now.get(read_was as usize).copied().flatten();
            if now.map(|number| self.version(number)) != Some(read_version) {
                return new;
            }
        }
        self.names[at].version = Some(version);
        version
    }

    /// The memo of this build: the parts it took and the parts it made, and
    /// each name of the program, together with the already-canonical node
    /// references reused from the prior build.
    pub(super) fn finish(
        mut self,
        cps: Option<f64>,
        shared_source: Option<Arc<str>>,
        collect_reused: bool,
    ) -> (Memo, Vec<PatternNodeRef>) {
        if self.next == UNREAD_VERSION {
            return (
                Memo {
                    cps,
                    ..Memo::default()
                },
                Vec::new(),
            );
        }
        // Fast path for tiny binding-free edits where every retained prior part
        // was reused unchanged and only the root expression was rebuilt: move
        // `before.parts` straight into the new `Memo` with zero slice/Vec allocs.
        if self.names.is_empty()
            && self.before.names.is_empty()
            && self.before.reads.is_empty()
            && self.before.part_meta.is_empty()
            && self.before.part_slots.is_empty()
            && !self.before.parts.is_empty()
            && self.before.parts.len() <= 16
            && self.taken_bits == (1u64 << self.before.parts.len()) - 1
            && self.source.len() <= 1024
        {
            let trimmed = self.source.trim();
            let trimmed_start = trimmed.as_ptr() as usize - self.source.as_ptr() as usize;
            let trimmed_len = trimmed.len();
            let only_root_made = self.made.iter().all(|(entry, _)| {
                usize::from(entry.part.start) == trimmed_start
                    && usize::from(entry.part.len) == trimmed_len
            });
            if only_root_made
                && self
                    .before
                    .parts
                    .iter()
                    .all(|part| part.node.strong_count() > 1)
            {
                let source = match (&self.before.source, shared_source) {
                    (Some(kept), _) if **kept == *self.source => Arc::clone(kept),
                    (_, Some(shared)) if *shared == *self.source => shared,
                    _ => Arc::from(self.source),
                };
                let parts = mem::take(&mut self.before.parts);
                let reused_nodes = if collect_reused {
                    parts.iter().map(|part| part.node.clone()).collect()
                } else {
                    Vec::new()
                };
                let shared_extras = self.shared_extras.take();
                return (
                    Memo {
                        source: Some(source),
                        parts,
                        part_meta: Box::new([]),
                        shared_extras,
                        part_slots: Box::new([]),
                        names: Box::new([]),
                        reads: Box::new([]),
                        by_text: Box::new([]),
                        next: self.next,
                        cps,
                        tail_num: None,
                    },
                    reused_nodes,
                );
            }
        }
        let now = mem::take(&mut self.now);
        let taken_bits = self.taken_bits;
        let taken_overflow = mem::take(&mut self.taken_overflow);
        let before_slots = mem::take(&mut self.before.part_slots);
        let before_meta = mem::take(&mut self.before.part_meta);
        let mut meta_iter = before_meta.into_vec().into_iter().peekable();
        if !self.before.parts.is_empty() {
            self.made.extend(
                mem::take(&mut self.before.parts)
                    .into_vec()
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, part)| {
                        let meta = match meta_iter.peek() {
                            Some((idx, _)) if usize::from(*idx) == i => {
                                meta_iter.next().map(|(_, meta)| meta)
                            }
                            _ => None,
                        };
                        let is_taken = if taken_overflow.is_empty() {
                            (taken_bits >> i) & 1 != 0
                        } else {
                            taken_overflow[i]
                        };
                        if !is_taken {
                            return None;
                        }
                        let slot = before_slots.get(i).copied().unwrap_or(NO_SLOTS);
                        Some((MadePart { part, meta, slot }, true))
                    }),
            );
        }
        let entries = &mut self.made;
        // A part that the build evaluated two times is kept one time: each
        // of the two holds what the call builds.
        entries.sort_unstable_by_key(|(entry, _)| entry.part.key());
        entries.dedup_by_key(|(entry, _)| entry.part.key());
        // Drop parts not shared with the live graph (such as folded
        // sub-expressions). Counting duplicate handles inside `entries` itself
        // catches dead pairs that only keep each other alive, and repeating to
        // a fixpoint releases dead parent-child chains too.
        //
        // Tiny binding-free scores keep their few folded sub-expressions so an
        // incremental edit still reuses the unchanged arguments of a stack or
        // control chain.
        let keep_tiny_parts =
            self.names.is_empty() && self.source.len() <= 1024 && entries.len() <= 16;
        if keep_tiny_parts {
            let trimmed = self.source.trim();
            let trimmed_start = trimmed.as_ptr() as usize - self.source.as_ptr() as usize;
            let trimmed_len = trimmed.len();
            // Drop the root expression covering the whole trimmed score (an
            // identical re-eval hits `source` directly and any edit invalidates
            // it) and unshared tiny sub-literals cheaper to rebuild than hold.
            entries.retain(|(entry, _)| {
                let start = usize::from(entry.part.start);
                let len = usize::from(entry.part.len);
                let is_root = start == trimmed_start && len == trimmed_len;
                if is_root {
                    return false;
                }
                !(len <= 10 && entry.part.node.strong_count() == 1)
            });
            // Bound unshared folded parts proportionally to score length so
            // short scores stay compact while keeping enough sub-expressions
            // for fast incremental edits.
            let max_tiny_unshared = (self.source.len() / 180).clamp(2, 6);
            while entries
                .iter()
                .filter(|(entry, _)| entry.part.node.strong_count() == 1)
                .count()
                > max_tiny_unshared
            {
                if let Some((drop_idx, _)) = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, (entry, _))| entry.part.node.strong_count() == 1)
                    .min_by_key(|(_, (entry, _))| entry.part.len)
                {
                    entries.remove(drop_idx);
                } else {
                    break;
                }
            }
        } else {
            entries.retain(|(entry, _)| entry.part.node.strong_count() > 1);
            loop {
                let before = entries.len();
                if before <= 1 {
                    break;
                }
                self.ids.clear();
                self.ids
                    .extend(entries.iter().map(|(entry, _)| entry.part.node.identity()));
                self.ids.sort_unstable();
                if self.ids.windows(2).all(|w| w[0] != w[1]) {
                    break;
                }
                let ids = &self.ids;
                entries.retain(|(entry, _)| {
                    let id = entry.part.node.identity();
                    let lo = ids.partition_point(|&x| x < id);
                    let hi = ids.partition_point(|&x| x <= id);
                    let inside = hi - lo;
                    entry.part.node.strong_count() > inside
                });
                if entries.len() == before {
                    break;
                }
            }
        }
        // A single unshared expression with no bindings has nothing to reuse on
        // an edit, while an identical re-evaluation hits `source` directly.
        if self.names.is_empty() && entries.len() == 1 && entries[0].0.part.node.strong_count() == 1
        {
            entries.clear();
        }
        // A re-evaluation of the same text shares the handle the build
        // before kept, so the text lives one time. An aborted build makes
        // no handle.
        let source = match (&self.before.source, shared_source) {
            (Some(kept), _) if **kept == *self.source => Arc::clone(kept),
            (_, Some(shared)) if *shared == *self.source => shared,
            _ => Arc::from(self.source),
        };
        if entries.is_empty() {
            return (
                Memo {
                    source: Some(source),
                    parts: Box::new([]),
                    part_meta: Box::new([]),
                    shared_extras: None,
                    part_slots: Box::new([]),
                    names: Box::new([]),
                    reads: Box::new([]),
                    by_text: Box::new([]),
                    next: self.next,
                    cps,
                    tail_num: None,
                },
                Vec::new(),
            );
        }
        let mut has_part_slots = false;
        let mut meta_count = 0usize;
        let mut reads_cap = 0usize;
        for (entry, _) in &self.made {
            if entry.slot != NO_SLOTS {
                has_part_slots = true;
            }
            if entry.meta.is_some() {
                meta_count += 1;
            }
            reads_cap += usize::from(entry.part.read().len);
        }
        let before_names = mem::take(&mut self.before.names);
        for name in &self.names {
            if let Some(span) = name.read {
                reads_cap += usize::from(span.len);
            } else if let Some(was) = name.was
                && name.version == Some(before_names[was].version)
            {
                reads_cap += usize::from(before_names[was].read.len);
            }
        }
        let before_reads = mem::take(&mut self.before.reads);
        let made_reads = &self.made_reads;
        let mut reads: Vec<(u16, u16)> = Vec::with_capacity(reads_cap);
        let push_renumbered = |reads: &mut Vec<(u16, u16)>, span: ReadSpan| -> Option<ReadSpan> {
            if span.len == 0 {
                return Some(ReadSpan::default());
            }
            let start = reads.len();
            for &(was, version) in span.get(&before_reads) {
                let Some(number) = now.get(usize::from(was)).copied().flatten() else {
                    reads.truncate(start);
                    return None;
                };
                let Ok(number) = u16::try_from(number) else {
                    reads.truncate(start);
                    return None;
                };
                reads.push((number, version));
            }
            let len = reads.len() - start;
            match (u16::try_from(start), u16::try_from(len)) {
                (Ok(start), Ok(len)) if len > 0 => Some(ReadSpan { start, len }),
                _ => {
                    reads.truncate(start);
                    None
                }
            }
        };
        let push_made = |reads: &mut Vec<(u16, u16)>, span: ReadSpan| -> Option<ReadSpan> {
            if span.len == 0 {
                return Some(ReadSpan::default());
            }
            let start = reads.len();
            reads.extend_from_slice(span.get(made_reads));
            match u16::try_from(start) {
                Ok(start) => Some(ReadSpan {
                    start,
                    len: span.len,
                }),
                Err(_) => {
                    reads.truncate(start);
                    None
                }
            }
        };
        let mut part_slots: Vec<u16> = if has_part_slots {
            Vec::with_capacity(self.made.len())
        } else {
            Vec::new()
        };
        let mut part_meta: Vec<(u16, PatternHandleMeta)> = Vec::with_capacity(meta_count);
        let mut reused_nodes: Vec<PatternNodeRef> = Vec::new();
        let mut parts_vec: Vec<Part> = Vec::with_capacity(self.made.len());
        for (mut entry, from_before) in self.made.drain(..) {
            let Some(next_read) = (if from_before {
                push_renumbered(&mut reads, entry.part.read())
            } else {
                push_made(&mut reads, entry.part.read())
            }) else {
                // Drop parts whose read set overflowed or lost a dependency
                // instead of recording an empty read set that allows stale reuse.
                continue;
            };
            if from_before && collect_reused {
                reused_nodes.push(entry.part.node.clone());
            }
            entry.part.set_read(next_read);
            let idx = parts_vec.len();
            if let Some(meta) = entry.meta
                && let Ok(idx_u16) = u16::try_from(idx)
            {
                part_meta.push((idx_u16, meta));
            }
            if has_part_slots {
                part_slots.push(entry.slot);
            }
            parts_vec.push(entry.part);
        }
        let parts: Box<[Part]> = parts_vec.into_boxed_slice();
        let mut names: Vec<Name> = Vec::with_capacity(self.names.len());
        let mut by_text: Vec<u16> = Vec::with_capacity(self.names.len());
        for (index, name) in mem::take(&mut self.names).into_iter().enumerate() {
            let kept = name
                .was
                .map(|was| (before_names[was].read, before_names[was].version));
            // What the statement read now, or what it read before when
            // this build took each part that called it.
            let (read, read_ok) = match (kept, name.read) {
                (_, Some(span)) => match push_made(&mut reads, span) {
                    Some(span) => (span, true),
                    None => (ReadSpan::default(), false),
                },
                (Some((span, version)), None) if name.version == Some(version) => {
                    match push_renumbered(&mut reads, span) {
                        Some(span) => (span, true),
                        None => (ReadSpan::default(), false),
                    }
                }
                _ => (ReadSpan::default(), true),
            };
            let (text_start, text_len) = match subslice_span(self.source, name.text) {
                Some((start, len)) => {
                    if let Ok(idx) = u16::try_from(index) {
                        by_text.push(idx);
                    }
                    (start, len)
                }
                None => (0, 0),
            };
            let slots = pack_slots(name.written.slots).unwrap_or(NO_SLOTS);
            let (written_start, written_len) = match (
                u16::try_from(name.written.start),
                u16::try_from(name.written.len),
            ) {
                (Ok(s), Ok(l)) => (s, l),
                _ => (0, 0),
            };
            let text_len_flags = text_len | if name.written.mini { MINI_FLAG } else { 0 };
            names.push(Name {
                start: written_start,
                len: written_len,
                // A name that nothing read takes a version that no name
                // has, so the next build finds it new.
                version: if read_ok {
                    name.version.unwrap_or(UNREAD_VERSION)
                } else {
                    UNREAD_VERSION
                },
                text_start,
                slots,
                read,
                text_len_flags,
            });
        }
        let source_str: &str = &source;
        by_text.sort_unstable_by_key(|&number| {
            let name = &names[usize::from(number)];
            let start = usize::from(name.text_start);
            let len = usize::from(name.text_len_flags & LEN_MASK);
            let text = source_str.get(start..start + len).unwrap_or("");
            (len, text)
        });
        let has_shared = parts
            .iter()
            .any(|p| p.meta_kind == rustel_core::COMPACT_META_SHARED_EXTRAS);
        (
            Memo {
                source: Some(source),
                parts,
                part_meta: part_meta.into_boxed_slice(),
                shared_extras: if has_shared {
                    self.shared_extras.take()
                } else {
                    None
                },
                part_slots: part_slots.into_boxed_slice(),
                names: names.into_boxed_slice(),
                reads: reads.into_boxed_slice(),
                by_text: by_text.into_boxed_slice(),
                next: self.next,
                cps,
                tail_num: None,
            },
            reused_nodes,
        )
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn version_near_wrap_resets_memo_instead_of_reusing_old_versions() {
        let source = "const a = 1\n$: s(\"bd\")";
        let program = rustel_transpiler::transpile(
            source,
            &rustel_transpiler::TranspileOptions {
                add_return: false,
                wrap_async: false,
                allow_module_syntax: false,
                ..Default::default()
            },
        )
        .program
        .expect("read score");
        let near_wrap = super::Memo {
            next: super::UNREAD_VERSION - 1,
            ..super::Memo::default()
        };
        let reuse = super::Reuse::new(source, near_wrap, &program);
        assert_eq!(reuse.next, 0);
        assert!(reuse.before.parts.is_empty());
        assert!(reuse.before.names.is_empty());
    }
}
