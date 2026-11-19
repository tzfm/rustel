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

use rustel_core::Pattern;
use rustel_transpiler::{ScoreProgram, ScoreStatement, Written};
use std::mem;
use std::sync::Arc;

/// Each name of the score that a part or a statement read, by its number
/// among the names of its build, with the version the name had.
type Read = Box<[(u32, u64)]>;

/// One kept part: where the score writes it, what it built, and what it
/// read.
struct Part {
    written: Written,
    pattern: Pattern,
    read: Read,
}

impl Part {
    /// The place of the part among the parts: where its text starts, then
    /// how long the text is.
    fn key(&self) -> (u32, u32) {
        (self.written.start, self.written.len)
    }
}

/// One name of the score: where the score writes its statement, its
/// version, and what the statement read.
struct Name {
    text: Box<str>,
    written: Written,
    version: u64,
    read: Read,
}

/// What the last native build of a score kept.
#[derive(Default)]
pub(in crate::runtime) struct Memo {
    /// The text of the score of that build, one handle of it: the session
    /// that keeps the text for a replay shares this handle.
    /// None before the first build.
    pub(in crate::runtime) source: Option<Arc<str>>,
    /// The kept parts, in the order of their places.
    parts: Vec<Part>,
    /// The names of the score, each at its number: the place of its
    /// statement among the statements with a name.
    names: Vec<Name>,
    /// The numbers of the names, in the order of the texts of the names.
    by_text: Vec<u32>,
    /// The version that the next new or changed name takes.
    next: u64,
    /// The tempo that the last native build set, if any.
    pub(in crate::runtime) cps: Option<f64>,
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
    version: Option<u64>,
    /// What the statement of the name read, when the build evaluated it.
    read: Option<Read>,
}

/// One build's use of the memo of the build before: it takes parts from
/// that memo and makes the memo of this build.
pub(super) struct Reuse<'p> {
    source: &'p str,
    before: Memo,
    /// Where `source` is the text of `before`.
    unchanged: Unchanged,
    /// Whether this build took each part of `before`, or a part around it.
    taken: Vec<bool>,
    /// The parts that this build evaluated and keeps.
    made: Vec<Part>,
    /// The names of the program, each at its number.
    names: Vec<Current<'p>>,
    /// The number that each name of `before` has in this build.
    now: Vec<Option<u32>>,
    open: Vec<Open>,
    /// The names of the score that the open parts and statements read, in
    /// the order of the reads.
    read: Vec<u32>,
    /// How many effects of the score the build ran.
    effects: u32,
    next: u64,
}

impl<'p> Reuse<'p> {
    /// The use of `before` for a build of `program`, which was read from
    /// `source`.
    pub(super) fn new(source: &'p str, before: Memo, program: &'p ScoreProgram<'p>) -> Self {
        let mut names: Vec<Current<'p>> = program
            .statements
            .iter()
            .filter_map(|statement| match statement {
                ScoreStatement::Binding { name, written, .. }
                | ScoreStatement::Function { name, written, .. } => Some(Current {
                    text: name,
                    written: *written,
                    was: None,
                    version: None,
                    read: None,
                }),
                ScoreStatement::Expression(_) => None,
            })
            .collect();
        // The same name of the build before, found by its text.
        let mut now = vec![None; before.names.len()];
        for (number, name) in names.iter_mut().enumerate() {
            let found = before.by_text.binary_search_by_key(&name.text, |was| {
                before
                    .names
                    .get(*was as usize)
                    .map_or("", |kept| &*kept.text)
            });
            if let (Ok(at), Ok(number)) = (found, u32::try_from(number)) {
                let was = before.by_text[at] as usize;
                name.was = Some(was);
                now[was] = Some(number);
            }
        }
        Self {
            source,
            unchanged: Unchanged::of(source, before.source.as_deref().unwrap_or_default()),
            taken: vec![false; before.parts.len()],
            next: before.next,
            before,
            made: Vec::new(),
            names,
            now,
            open: Vec::new(),
            read: Vec::new(),
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
        let key = (written.start, written.len);
        let at = self
            .before
            .parts
            .binary_search_by_key(&key, Part::key)
            .ok()?;
        if !self.same_text(written, &self.before.parts[at].written) {
            return None;
        }
        // Each name that the part read is a name of this build, with the
        // version that the part read.
        let count = self.before.parts[at].read.len();
        for index in 0..count {
            let (was, version) = self.before.parts[at].read[index];
            let number = self.now.get(was as usize).copied().flatten()?;
            if self.version(number) != version {
                return None;
            }
        }
        // What the part read, the part or the statement around it reads.
        if !self.open.is_empty() {
            let now = &self.now;
            let numbers = self.before.parts[at].read.iter();
            let numbers = numbers.filter_map(|(was, _)| now.get(*was as usize).copied().flatten());
            self.read.extend(numbers);
        }
        // The parts inside the call stay with it: each starts after the
        // start of the call and before its end.
        let end = written.start.saturating_add(written.len);
        let inside = self.before.parts[at..]
            .iter()
            .take_while(|part| part.written.start < end)
            .count();
        self.taken[at..at + inside].fill(true);
        // A handle of the kept pattern.
        Some(self.before.parts[at].pattern.clone())
    }

    /// Keep `pattern`, which the build made of the call at `written` and
    /// which `closed` says is a function of its text and of what it read.
    pub(super) fn keep(&mut self, written: Written, pattern: &Pattern, closed: Closed) {
        if !closed.kept {
            return;
        }
        let read = self.versions_of(closed.read);
        self.made.push(Part {
            written,
            // A handle of the pattern, which the graph holds too.
            pattern: pattern.clone(),
            read,
        });
    }

    /// How many parts the build keeps so far: a mark for [`Self::release`].
    pub(super) fn mark(&self) -> usize {
        self.made.len()
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
            if self.made[at].pattern.is_held_elsewhere() {
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
        {
            let read = self.versions_of(closed.read);
            self.names[number as usize].read = Some(read);
        }
    }

    /// Each name that the build read from place `start` of its list on, one
    /// time, with its version. The list then holds each of them one time:
    /// it is the end of the list of each part and statement that is open.
    fn versions_of(&mut self, start: usize) -> Read {
        let Some(read) = self.read.get_mut(start..).filter(|read| !read.is_empty()) else {
            return Read::default();
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
        let mut versions = Vec::with_capacity(kept);
        for at in start..start + kept {
            let number = self.read[at];
            versions.push((number, self.version(number)));
        }
        versions.into_boxed_slice()
    }

    /// The version of the name with `number`: the version of the build
    /// before when the text of its statement is the same and each name
    /// that the statement read keeps its version, and a new one otherwise.
    fn version(&mut self, number: u32) -> u64 {
        let at = number as usize;
        // A number of no name is of no build: it has a version of its own.
        let Some(name) = self.names.get(at) else {
            return u64::MAX;
        };
        if let Some(version) = name.version {
            return version;
        }
        // A name that reads itself, through any number of names, is new.
        let new = self.next;
        self.next += 1;
        self.names[at].version = Some(new);
        let Some(was) = self.names[at].was else {
            return new;
        };
        let kept = &self.before.names[was];
        if !self.same_text(&self.names[at].written, &kept.written) {
            return new;
        }
        let version = kept.version;
        for index in 0..self.before.names[was].read.len() {
            let (read_was, read_version) = self.before.names[was].read[index];
            let now = self.now.get(read_was as usize).copied().flatten();
            if now.map(|number| self.version(number)) != Some(read_version) {
                return new;
            }
        }
        self.names[at].version = Some(version);
        version
    }

    /// The memo of this build: the parts it took and the parts it made, and
    /// each name of the program.
    pub(super) fn finish(mut self, cps: Option<f64>) -> Memo {
        let now = mem::take(&mut self.now);
        // Each name that a taken part or statement read is a name of this
        // build, and takes its number in it.
        let renumbered = |read: Read| -> Read {
            let number = |was: u32| now.get(was as usize).copied().flatten();
            read.into_iter()
                .filter_map(|(was, version)| Some((number(was)?, version)))
                .collect()
        };
        let taken = mem::take(&mut self.taken);
        let mut parts: Vec<Part> = mem::take(&mut self.before.parts)
            .into_iter()
            .zip(taken)
            .filter_map(|(part, taken)| taken.then_some(part))
            .map(|part| Part {
                read: renumbered(part.read),
                ..part
            })
            .collect();
        parts.append(&mut self.made);
        // A part that the build evaluated two times is kept one time: each
        // of the two holds what the call builds.
        parts.sort_unstable_by_key(Part::key);
        parts.dedup_by_key(|part| part.key());
        // The next build reads this list for every part it finds, so the
        // room a part left when its duplicate went is not kept.
        parts.shrink_to_fit();
        let mut before = mem::take(&mut self.before.names);
        // A list of its own, with room for the names alone: the list of
        // the build is larger.
        let mut names: Vec<Name> = Vec::with_capacity(self.names.len());
        names.extend(self.names.into_iter().map(|name| {
            let kept = name.was.map(|was| {
                let kept = &mut before[was];
                (
                    mem::take(&mut kept.text),
                    mem::take(&mut kept.read),
                    kept.version,
                )
            });
            // What the statement read now, or what it read before when
            // this build took each part that called it.
            let (text, read) = match (kept, name.read) {
                (Some((text, ..)), Some(read)) => (text, read),
                (Some((text, read, version)), None) if name.version == Some(version) => {
                    (text, renumbered(read))
                }
                (Some((text, ..)), None) => (text, Read::default()),
                (None, read) => (Box::from(name.text), read.unwrap_or_default()),
            };
            Name {
                text,
                written: name.written,
                // A name that nothing read takes a version that no name
                // has, so the next build finds it new.
                version: name.version.unwrap_or(u64::MAX),
                read,
            }
        }));
        // A re-evaluation of the same text shares the handle the build
        // before kept, so the text lives one time. An aborted build makes
        // no handle.
        let source = match &self.before.source {
            Some(kept) if **kept == *self.source => Arc::clone(kept),
            _ => Arc::from(self.source),
        };
        let mut by_text: Vec<u32> = (0..names.len())
            .filter_map(|number| u32::try_from(number).ok())
            .collect();
        by_text.sort_unstable_by_key(|number| &*names[*number as usize].text);
        Memo {
            source: Some(source),
            parts,
            names,
            by_text,
            next: self.next,
            cps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Unchanged;

    #[test]
    fn a_text_outside_an_edit_is_the_text_before() {
        let before: String = ('a'..='z').cycle().take(50).collect();
        // An edit of one byte, at each side of the width of a comparison.
        for at in [0, 1, 15, 16, 17, 31, 32, 33, 48, 49] {
            let mut edited = before.clone().into_bytes();
            edited[at] = b'#';
            let edited = String::from_utf8(edited).expect("a text of ASCII");
            let unchanged = Unchanged::of(&edited, &before);
            for start in 0..50 {
                for end in start + 1..=50 {
                    let outside = !(start..end).contains(&at);
                    assert_eq!(unchanged.holds(start, end), outside, "{at}: {start}..{end}");
                }
            }
        }
        assert!(Unchanged::of(&before, &before).holds(0, 50));
        // A byte more moves each byte after it: the start alone stays.
        let longer = format!("{}#{}", &before[..20], &before[20..]);
        let unchanged = Unchanged::of(&longer, &before);
        assert!(unchanged.holds(0, 20));
        assert!(!unchanged.holds(0, 21));
        assert!(!unchanged.holds(30, 40));
    }
}
