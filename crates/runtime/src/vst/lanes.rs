//! The lanes of the scores of a session, each with a number that stays
//! through an edit.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use rustel_transpiler::LaneStatement;

use super::*;
use crate::PREVIEW_DOCUMENT;

/// A lane of a score: a labeled statement, or a score with no label.
#[derive(Clone)]
struct Lane {
    /// The number stays while an edit keeps the lane in the text.
    number: u64,
    label: String,
    body: String,
    /// The line of the label, counted from 1. With no label, the line where
    /// the body starts.
    line: usize,
    /// The plugin names of the calls in the body: see [`body_plugin_names`].
    plugins: Vec<String>,
}

/// The lanes of one score text, as the chains of a session read them: see
/// [`LaneTable::playing`] and [`LaneTable::read_of`].
#[derive(Clone, Default)]
pub(super) struct ScoreLanes {
    /// The lanes in text order. A score with no label has one lane, with
    /// the empty label.
    lanes: Vec<Lane>,
    /// True when the label rewrite of the text wrote places: see
    /// [`LaneTable::wrote_places`].
    places: bool,
}

impl ScoreLanes {
    /// Each lane in text order: its number, its label, its line and the
    /// plugin names of its body.
    pub(super) fn lanes(&self) -> impl Iterator<Item = (u64, &str, usize, &[String])> {
        let lanes = self.lanes.iter();
        lanes.map(|lane| {
            (
                lane.number,
                lane.label.as_str(),
                lane.line,
                &lane.plugins[..],
            )
        })
    }

    /// True when the tag `#n` of a note of the text is the place of its
    /// statement among the labeled statements, and not a lane number.
    pub(super) fn places(&self) -> bool {
        self.places
    }
}

/// The lanes of a document at rest: see [`LaneTable`].
struct RestDocument {
    /// The lanes of its score in play when the document went to rest.
    lanes: Vec<Lane>,
    /// The lanes that left the score of the document while a chain has
    /// their number: see [`LaneTable::forget_away`].
    away: Vec<Lane>,
    /// The rest order: a document that goes to rest later has a higher
    /// order. See [`REST_ORDERS`].
    order: u64,
}

/// The count of the rest orders. One count serves each lane table of the
/// process, so the host compares the documents at rest of 2 sessions.
static REST_ORDERS: AtomicU64 = AtomicU64::new(0);

/// The lanes of the scores of one session.
///
/// An edit gives a lane a new place, a new label, a mute mark or a new
/// body. The lane keeps its number through such an edit: see [`pair`]. A
/// plugin note of a labeled statement carries the number, so the owner of
/// its chain keeps its lane: see `ChainOwner`. A lane that leaves the text
/// keeps its number while a chain has it, so a lane that comes back from a
/// comment finds its chain.
///
/// Each score text is of a document: a key the caller gives, as the scene
/// id in Studio. The CLI, an embedder and a test read in document 0. An
/// open document that does not play is at rest, and the table keeps its
/// lanes, so a document that plays again finds its lanes. A document that
/// plays for the first time shares the lanes of the score in play: see
/// [`Self::read`]. A document closes when its tab leaves: see
/// [`Self::forget`].
#[derive(Default)]
pub(crate) struct LaneTable {
    /// The lanes of the score in play.
    playing: ScoreLanes,
    /// The document of the score in play.
    document: u64,
    /// The document of the next read: see [`Self::set_reading`].
    reading: u64,
    /// The last text the session read that is not in play, with its
    /// document and its lanes. A new lane then keeps its number from one
    /// try of an edit to the next. After a read of another text, the read
    /// of the text the session holds takes its place again: see
    /// [`Self::adopt_read`].
    candidate: Option<(u64, String, ScoreLanes)>,
    /// True when the candidate is a read that moved to the reading
    /// document from another document: see [`Self::adopt_read`]. The next
    /// read pairs its lanes after the lanes of that document, and ends the
    /// mark.
    adopted: bool,
    /// The read of the text the session holds for its plugin copies, with
    /// its document and its lanes: see [`Self::hold`]. A read of another
    /// text, such as a text that failed, takes the place of the candidate,
    /// and this read stays.
    #[cfg(any(feature = "device-audio", test))]
    held: Option<(u64, String, ScoreLanes)>,
    /// The lanes of the score the output plays, with its document, until
    /// the output plays the score in play. A rollback puts that score back.
    /// Each of its lanes then keeps its number.
    audible: (u64, Vec<Lane>),
    /// The lanes that left the score in play, while a chain has their
    /// number: see [`Self::forget_away`].
    away: Vec<Lane>,
    /// The lanes of each document at rest, by document.
    rest: HashMap<u64, RestDocument>,
    /// The closed documents: see [`Self::forget`].
    closed: HashSet<u64>,
    /// True when the output plays the score in play.
    heard: bool,
    /// The number of the next new lane. No number comes a second time.
    next: u64,
}

impl LaneTable {
    /// Reads the lanes of a score text of the reading document before its
    /// evaluation: see [`Self::set_reading`]. Gives the lane number of each
    /// labeled statement, in text order, for
    /// [`rustel_transpiler::TranspileOptions::lane_numbers`]. A text with
    /// no scan gets no numbers and changes no lane, also when it goes in
    /// play.
    ///
    /// A statement first takes a lane of its own document: a lane of its
    /// score in play or at rest, of its last read, a lane away, or a lane of
    /// its score the output plays. See [`pair`]. A last read that moved to
    /// the document from another document gives a lane after these, and
    /// before the lanes of the score in play: see [`Self::adopt_read`]. For
    /// a document that is not in play, each statement with no lane then
    /// takes a lane of the score in play as a statement of the document in
    /// play does. Such a lane is a lane of both documents. So a document
    /// that plays for the first time keeps the lanes it shares with the
    /// score in play, and a document that plays again finds its own lanes
    /// first.
    pub(crate) fn read(&mut self, source: &str) -> Vec<u64> {
        // The read takes the place of the candidate, with its mark.
        let adopted = std::mem::take(&mut self.adopted);
        let Some(statements) = rustel_transpiler::lane_statements(source) else {
            self.candidate = None;
            return Vec::new();
        };
        let reading = self.reading;
        let none: &[Lane] = &[];
        // The lanes of the document of the read come first.
        let (playing, away) = if reading == self.document {
            (&self.playing.lanes[..], &self.away[..])
        } else {
            let rest = self.rest.get(&reading);
            rest.map_or((none, none), |rest| (&rest.lanes[..], &rest.away[..]))
        };
        let tried = self.candidate.as_ref();
        let tried = tried.filter(|(document, ..)| *document == reading);
        let tried = tried.map_or(none, |(.., read)| &read.lanes);
        // A read that moved here from another document comes after the
        // lanes of the document: see `Self::adopt_read`.
        let (tried, moved) = if adopted {
            (none, tried)
        } else {
            (tried, none)
        };
        let (audible_in, audible) = &self.audible;
        let (own, other) = if *audible_in == reading {
            (&audible[..], none)
        } else {
            (none, &audible[..])
        };
        let mut paired = vec![None; statements.len()];
        pair(&statements, &mut paired, playing, tried, away, own);
        if adopted {
            pair(&statements, &mut paired, none, moved, none, none);
        }
        // A document that is not in play shares the lanes of the score in
        // play.
        if reading != self.document {
            let (playing, away) = (&self.playing.lanes, &self.away);
            pair(&statements, &mut paired, playing, none, away, other);
        }
        let lane = |(statement, number): (LaneStatement, Option<u64>)| Lane {
            number: number.unwrap_or_else(|| {
                self.next += 1;
                self.next - 1
            }),
            plugins: body_plugin_names(&statement.body),
            label: statement.label,
            body: statement.body,
            line: statement.line,
        };
        let lanes: Vec<Lane> = statements.into_iter().zip(paired).map(lane).collect();
        // A score with no label has no statement that carries a number.
        let labeled = lanes.iter().filter(|lane| !lane.label.is_empty());
        let numbers = labeled.map(|lane| lane.number).collect();
        let read = ScoreLanes {
            lanes,
            places: false,
        };
        self.candidate = Some((reading, source.to_owned(), read));
        numbers
    }

    /// The next reads are of `document`: see [`Self::read`].
    pub(crate) fn set_reading(&mut self, document: u64) {
        self.reading = document;
    }

    /// The read of the text `source` is the last read, of the reading
    /// document. The session holds that text for its plugin copies, and the
    /// same text takes the place of the text that waits: see
    /// `Session::reload_live`. After a read of another text, such as a text
    /// that failed, the read of the hold is the last read again: see
    /// [`Self::hold`]. Gives true when that read was of another document.
    /// The next read then pairs the lanes of that read after the lanes of
    /// the reading document, so a lane of that document keeps its number:
    /// see [`Self::read`]. With no read of the text, the last read stays,
    /// and the call gives false.
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn adopt_read(&mut self, source: &str) -> bool {
        let last = self.candidate.as_ref();
        if last.is_none_or(|(_, text, _)| text != source) {
            let held = self.held.as_ref();
            let Some(held) = held.filter(|(_, text, _)| text == source) else {
                return false;
            };
            self.candidate = Some(held.clone());
        }
        let reading = self.reading;
        if let Some((document, ..)) = &mut self.candidate
            && *document != reading
        {
            *document = reading;
            self.adopted = true;
            return true;
        }
        false
    }

    /// The session holds the text `source` for its plugin copies, or no
    /// text with `None`. The last read, when it read `source`, stays for
    /// [`Self::adopt_read`] until the next call.
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn hold(&mut self, source: Option<&str>) {
        let read = self.candidate.as_ref();
        let read = read.filter(|(_, text, _)| Some(text.as_str()) == source);
        self.held = read.cloned();
    }

    /// The score with the text `source` goes in play: the lanes of the last
    /// read are now the lanes of the score in play, with the flag of
    /// [`Self::wrote_places`], and the document of that read is in play. A
    /// read of a different text changes nothing here. A score with no read,
    /// such as a mini-notation score, has no lanes of its own. Each lane of
    /// the score before with no statement in this text is away: see
    /// [`Self::forget_away`]. The lanes of the score before stay for a
    /// rollback when the output plays that score: see
    /// [`Self::mark_audible`].
    ///
    /// The install of a read of another document puts the document in play
    /// at rest with its lanes and its lanes away, or drops them when that
    /// document is closed: see [`Self::is_closed`]. The document of the read
    /// leaves rest. The score before is then its score at rest, and its
    /// lanes away stay away unless they came back.
    pub(crate) fn install(&mut self, source: &str) {
        let read = self.candidate.take_if(|(_, text, _)| text == source);
        if let Some((document, _, read)) = read {
            self.put_in_play(document, read);
        }
    }

    /// A direct pattern goes in play: a score with no text, so with no
    /// lanes. Each lane of the score before is away. The document in play
    /// stays in play.
    pub(crate) fn install_pattern(&mut self) {
        self.put_in_play(self.document, ScoreLanes::default());
    }

    /// The lanes of `read`, a read of `document`, are now the lanes of the
    /// score in play: see [`Self::install`].
    fn put_in_play(&mut self, document: u64, read: ScoreLanes) {
        let mut before = std::mem::replace(&mut self.playing, read).lanes;
        let left = std::mem::replace(&mut self.document, document);
        // The output plays the score before until it plays this one.
        let heard = std::mem::take(&mut self.heard);
        if left != document {
            // The document before goes to rest. The score before is the
            // score of the document of the read at rest.
            let back = self.rest.remove(&document);
            let back = back.map(|rest| (rest.lanes, rest.away));
            let (lanes, away) = back.unwrap_or_default();
            let played = std::mem::replace(&mut before, lanes);
            let away = std::mem::replace(&mut self.away, away);
            if heard {
                self.audible = (left, played.clone());
            }
            if !self.is_closed(left) {
                let order = REST_ORDERS.fetch_add(1, Ordering::Relaxed);
                let rest = RestDocument {
                    lanes: played,
                    away,
                    order,
                };
                self.rest.insert(left, rest);
            }
        }
        let kept: HashSet<u64> = self.playing.lanes.iter().map(|lane| lane.number).collect();
        // A lane that came back is in play again.
        self.away.retain(|lane| !kept.contains(&lane.number));
        let gone = before.iter().filter(|lane| !kept.contains(&lane.number));
        self.away.extend(gone.cloned());
        if heard && left == document {
            self.audible = (left, before);
        }
    }

    /// Forgets each lane away with no chain, and each lane of a document at
    /// rest with no chain: a document at rest with no chain keeps no lane.
    /// `chained` gives true for the number of a lane with a chain. A lane
    /// away comes back with its number by rules 1 to 3 of [`pair`]. A lane
    /// that comes back after this is a new lane.
    pub(super) fn forget_away(&mut self, chained: impl Fn(u64) -> bool) {
        self.away.retain(|lane| chained(lane.number));
        for rest in self.rest.values_mut() {
            rest.lanes.retain(|lane| chained(lane.number));
            rest.away.retain(|lane| chained(lane.number));
        }
    }

    /// The tab of `document` left: the document is closed, and never goes
    /// to rest again. A document at rest leaves rest, and the table drops
    /// its lanes and its lanes away. The document in play keeps its lanes
    /// until another document goes in play.
    pub(super) fn forget(&mut self, document: u64) {
        self.closed.insert(document);
        self.rest.remove(&document);
    }

    /// The output plays the score in play. A rollback then puts this score
    /// back, so the lanes of the score before go.
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn mark_audible(&mut self) {
        self.heard = true;
        self.audible.1.clear();
    }

    /// The label rewrite of the last read wrote places, and not the lane
    /// numbers of the read: see
    /// [`rustel_transpiler::TranspileOutput::lanes_by_place`]. The tag `#n`
    /// of a note of that text is then a place. The score in play keeps the
    /// flag when the text goes in play.
    pub(crate) fn wrote_places(&mut self) {
        if let Some((.., read)) = &mut self.candidate {
            read.places = true;
        }
    }

    /// The lanes of the score in play.
    pub(super) fn playing(&self) -> &ScoreLanes {
        &self.playing
    }

    /// The document and the lanes of the score the install of the text
    /// `source` puts in play: the last read, when it read `source`, and
    /// else the score in play. See [`Self::install`].
    pub(super) fn read_of(&self, source: &str) -> (u64, &ScoreLanes) {
        let read = self.candidate.as_ref();
        let read = read.filter(|(_, text, _)| text == source);
        read.map_or((self.document, &self.playing), |(document, _, read)| {
            (*document, read)
        })
    }

    /// The document of the score in play.
    pub(crate) fn document(&self) -> u64 {
        self.document
    }

    /// True for a closed document, and for [`PREVIEW_DOCUMENT`]: such a
    /// document never goes to rest.
    pub(super) fn is_closed(&self, document: u64) -> bool {
        document == PREVIEW_DOCUMENT || self.closed.contains(&document)
    }

    /// Each document at rest, with its rest order: see
    /// [`RestDocument::order`].
    pub(super) fn rest_orders(&self) -> impl Iterator<Item = (u64, u64)> {
        let rest = self.rest.iter();
        rest.map(|(document, rest)| (*document, rest.order))
    }

    /// The documents at rest, with their rest orders, as the install of a
    /// read of `document` leaves them: `document` leaves rest, and the
    /// document in play goes to rest with the next order when it is another
    /// document that is not closed.
    pub(super) fn rest_orders_for(&self, document: u64) -> impl Iterator<Item = (u64, u64)> {
        let resting = self.document != document && !self.is_closed(self.document);
        let next = REST_ORDERS.load(Ordering::Relaxed);
        let in_play = resting.then_some((self.document, next));
        let rest = self.rest_orders();
        let rest = rest.filter(move |(at_rest, _)| *at_rest != document);
        rest.chain(in_play)
    }
}

/// What rule 1, 2 or 3 of [`pair`] compares. A statement and a lane with
/// the same key fit the rule. `None` for a label that rule 3 does not pair.
fn rule_key<'a>(rule: u8, label: &'a str, body: &'a str) -> Option<(&'a str, &'a str)> {
    match rule {
        1 => Some((label, body)),
        2 => Some(("", body)),
        _ => {
            // `_lead` and `lead_` are the lane `lead` with a mute mark.
            let name = label.trim_matches('_');
            (name != "$").then_some((name, ""))
        }
    }
}

/// The most pairs of a statement and a lane that rule 4 of [`pair`]
/// compares: 512 statements with 512 lanes. The compare runs before each
/// evaluation, so its time has a limit. With more pairs, each statement
/// that rules 1 to 3 did not pair is a new lane. A read of a document that
/// is not in play makes the compare 2 times, and a read after
/// [`LaneTable::adopt_read`] one time more: see [`LaneTable::read`].
const MAX_LANE_PAIRS: usize = 1 << 18;

/// Gives each statement of a new score text with no lane its lane.
/// `numbers` has the number of the lane of each statement, or `None` for a
/// statement with no lane yet. No number of `numbers` goes to a second
/// statement.
///
/// `playing` has the lanes of the score in play of a document: see
/// [`LaneTable::read`]. `tried` has the lanes of the last text the session
/// read. It gives only its new lanes: a text that did not go in play moves
/// no lane of the score in play. `away` has the lanes that left the score
/// in play while a chain has them. It gives a lane by rules 1 to 3, after
/// `playing` and `tried`. `audible` has the lanes of the score the output
/// plays, when that score is not in play. It gives a lane by rule 1 only: a
/// rollback reads the text of that score again. `numbers` then has the
/// number of the lane of each statement, or `None` for a new lane. A
/// statement takes a lane by the first rule that fits:
///
/// ```text
///   1. the same label and the same body
///   2. the same body: the label is new, or has a new mark
///   3. the same label name, with no `_` at its ends: the body is new.
///      Not for `$`: this label names no lane.
///   4. the 2 bodies name the same plugin, or their equal start plus
///      their equal end is half of the shorter body or more
/// ```
///
/// Rules 1 to 3 pair in text order: see [`in_order`]. In rule 4 the best
/// pair goes first: the most plugin names in common, then the most equal
/// bytes, then the nearest place. An edited lane then keeps its number
/// when a new lane comes above it in the same edit. A statement with no
/// pair is a new lane, and a lane with no pair left the score.
fn pair(
    statements: &[LaneStatement],
    numbers: &mut [Option<u64>],
    playing: &[Lane],
    tried: &[Lane],
    away: &[Lane],
    audible: &[Lane],
) {
    // The numbers with a statement, and the numbers of the score in play.
    let mut taken: HashSet<u64> = numbers.iter().flatten().copied().collect();
    let kept: HashSet<u64> = playing.iter().map(|lane| lane.number).collect();
    // True for a lane with no statement yet. `tried` gives its new lanes
    // only.
    let free = |taken: &HashSet<u64>, lane: &Lane, new_only: bool| {
        let kept = new_only && kept.contains(&lane.number);
        !(kept || taken.contains(&lane.number))
    };
    let known = [(playing, false), (tried, true)];
    for rule in 1..=3 {
        let heard = (rule == 1).then_some((audible, false));
        let lists = known.into_iter().chain([(away, false)]).chain(heard);
        for (lanes, new_only) in lists {
            // The places of the lanes and of the statements with no pair
            // yet, by key.
            let mut olds: HashMap<_, Vec<usize>> = HashMap::new();
            for (place, lane) in lanes.iter().enumerate() {
                let key = rule_key(rule, &lane.label, &lane.body);
                if let Some(key) = key.filter(|_| free(&taken, lane, new_only)) {
                    olds.entry(key).or_default().push(place);
                }
            }
            let mut news: HashMap<_, Vec<usize>> = HashMap::new();
            for (place, statement) in statements.iter().enumerate() {
                let key = rule_key(rule, &statement.label, &statement.body);
                let open = numbers[place].is_none();
                if let Some(key) = key.filter(|key| open && olds.contains_key(key)) {
                    news.entry(key).or_default().push(place);
                }
            }
            for (key, news) in news {
                for (new, old) in in_order(&news, &olds[&key]) {
                    numbers[new] = Some(lanes[old].number);
                    taken.insert(lanes[old].number);
                }
            }
        }
    }
    // Rule 4. The statements and the lanes with no pair yet. A lane away
    // takes no pair here.
    let open: Vec<usize> = (0..statements.len())
        .filter(|at| numbers[*at].is_none())
        .collect();
    let mut left = Vec::new();
    for (lanes, new_only) in known {
        let lanes = lanes.iter().enumerate();
        left.extend(lanes.filter(|(_, lane)| free(&taken, lane, new_only)));
    }
    let count = open.len().saturating_mul(left.len());
    if count == 0 || count > MAX_LANE_PAIRS {
        return;
    }
    let mut pairs = Vec::new();
    for at in open {
        let statement = &statements[at];
        let names = body_plugin_names(&statement.body);
        for (place, lane) in &left {
            let shared = names
                .iter()
                .filter(|name| lane.plugins.contains(name))
                .count();
            let equal = equal_ends(&statement.body, &lane.body);
            let shorter = statement.body.len().min(lane.body.len());
            if shared > 0 || (shorter > 0 && 2 * equal >= shorter) {
                let nearest = place.abs_diff(at);
                let order = (std::cmp::Reverse(shared), std::cmp::Reverse(equal), nearest);
                pairs.push((order, at, lane.number));
            }
        }
    }
    // Pairs of the same order stay in text order.
    pairs.sort_by_key(|(order, ..)| *order);
    for (_, at, number) in pairs {
        if numbers[at].is_none() && taken.insert(number) {
            numbers[at] = Some(number);
        }
    }
}

/// Pairs the statements and the lanes that fit one rule, each as its place
/// in its text. No pair crosses a pair, so lanes with equal text keep their
/// order: the k-th statement takes the k-th lane when the 2 lists have the
/// same length. If not, each place of the shorter list takes the nearest
/// place of the longer list that leaves a place for each place after it.
fn in_order(news: &[usize], olds: &[usize]) -> Vec<(usize, usize)> {
    let flip = news.len() > olds.len();
    let (short, long) = if flip { (olds, news) } else { (news, olds) };
    let mut pairs = Vec::with_capacity(short.len());
    let mut at = 0;
    for (count, place) in short.iter().enumerate() {
        let last = long.len() - (short.len() - count);
        // The longer list is in order: the next place is nearer until it
        // is past this one.
        while at < last && long[at + 1].abs_diff(*place) < long[at].abs_diff(*place) {
            at += 1;
        }
        pairs.push(if flip {
            (long[at], *place)
        } else {
            (*place, long[at])
        });
        at += 1;
    }
    pairs
}

/// The count of the bytes that 2 lane bodies have in common at their start
/// and at their end. No byte counts 2 times.
fn equal_ends(one: &str, other: &str) -> usize {
    let (one, other) = (one.as_bytes(), other.as_bytes());
    let equal = |(byte, same): &(&u8, &u8)| byte == same;
    let start = one.iter().zip(other).take_while(equal).count();
    let rest = one.len().min(other.len()) - start;
    let ends = one.iter().rev().zip(other.iter().rev());
    start + ends.take(rest).take_while(equal).count()
}

/// The plugin names of the calls in the code of a lane body, in their
/// compare form. A call in a comment names no plugin: a comment is a
/// removal. The body has no label, so no mute takes out a call here.
fn body_plugin_names(body: &str) -> Vec<String> {
    // A body with no `vst` in its text has no plugin call.
    if !body.contains("vst") {
        return Vec::new();
    }
    let calls = crate::lint::live_plugins(body).into_iter();
    let mut names: Vec<String> = calls.map(|call| canonical(&call.name)).collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use rustel_fraction::Fraction;

    /// Reads a score text and puts the score in play. Gives the lane number
    /// of each labeled statement.
    pub(in crate::vst) fn play(lanes: &mut LaneTable, score: &str) -> Vec<u64> {
        let numbers = lanes.read(score);
        lanes.install(score);
        numbers
    }

    /// The number of each lane of `lanes`.
    fn numbers(lanes: &[Lane]) -> Vec<u64> {
        lanes.iter().map(|lane| lane.number).collect()
    }

    /// The rules of [`pair`], each before the next one, and the lanes of
    /// each document.
    #[test]
    fn a_statement_takes_its_lane_by_the_first_rule_that_fits() {
        // Rule 1 before rule 2: a mute mark stays with its lane when 2 lanes
        // with one body change places.
        let mut lanes = LaneTable::default();
        assert_eq!(play(&mut lanes, "$: s(\"bd\")\n_$: s(\"bd\")"), [0, 1]);
        assert_eq!(play(&mut lanes, "_$: s(\"bd\")\n$: s(\"bd\")"), [1, 0]);
        // Rule 2 before rule 3: the body is the lane when 2 lanes change
        // labels.
        let mut lanes = LaneTable::default();
        let score = "lead: note(\"c e\")\nbass: note(\"c1\")";
        assert_eq!(play(&mut lanes, score), [0, 1]);
        let score = "lead: note(\"c1\")\nbass: note(\"c e\")";
        assert_eq!(play(&mut lanes, score), [1, 0]);
        // Rule 3: a new body under the label name, with a mute mark at
        // each end. The label `$` is no name.
        let score = "_lead: s(\"hh*8\")\nbass_: s(\"cp\")\n$: note(\"g\")";
        assert_eq!(play(&mut lanes, score), [1, 0, 2]);
        let score = score.replace("note(\"g\")", "s(\"sd*4\").gain(.5)");
        assert_eq!(play(&mut lanes, &score), [1, 0, 3]);

        // Rule 4: a lane with half of its text is the same lane. A lane
        // with less is a new lane.
        let mut lanes = LaneTable::default();
        assert_eq!(play(&mut lanes, r#"$: note("c e g").lpf(800)"#), [0]);
        assert_eq!(play(&mut lanes, r#"$: note("c e g a").lpf(800)"#), [0]);
        assert_eq!(play(&mut lanes, r#"$: s("hh*8").gain(.5)"#), [1]);
        assert_eq!(equal_ends("aaaa", "aaaaaa"), 4);
        // A text with no code has no lane of the score before. With no
        // chain, the lane that left is a new lane when it comes back.
        assert!(play(&mut lanes, "// later").is_empty());
        lanes.forget_away(|_| false);
        assert_eq!(play(&mut lanes, r#"$: s("hh*8").gain(.5)"#), [3]);
        // The pair with the most plugin names goes before the pair with the
        // most equal bytes. A name ignores upper case.
        let mut lanes = LaneTable::default();
        let score = r#"$: note("c e").vsti("serum").vst("ott")"#;
        assert_eq!(play(&mut lanes, score), [0]);
        let score = "$: s(\"bd\").vsti(\"Serum\").vst(\"OTT\")\n$: note(\"c e\").vsti(\"serum\")";
        assert_eq!(play(&mut lanes, score), [0, 1]);
        // Then the nearest place: the last lane takes the lane at its place.
        let mut lanes = LaneTable::default();
        let score = "$: note(\"c\")\n$: note(\"e\")\n$: s(\"sd\")";
        assert_eq!(play(&mut lanes, score), [0, 1, 2]);
        let score = "$: s(\"hh\")\n$: note(\"e\")\n$: s(\"cp\")\n$: note(\"c\")";
        assert_eq!(play(&mut lanes, score), [3, 1, 2, 0]);

        // A text that does not go in play: its new lane keeps its number at
        // the next read, also with an edit.
        let mut lanes = LaneTable::default();
        let (lead, clap) = (
            r#"lead: note("c2").vsti("serum")"#,
            r#"$: s("cp").vst("ott")"#,
        );
        assert_eq!(play(&mut lanes, lead), [0]);
        let tried = format!("{lead}\n{clap}");
        assert_eq!(lanes.read(&tried), [0, 1]);
        assert_eq!(lanes.read(&tried.replace("cp", "cp*2")), [0, 1]);
        // A score with no read takes no lanes of a different text.
        lanes.install("bd sd");
        assert_eq!(lanes.playing.lanes.len(), 1);
        assert_eq!(play(&mut lanes, &tried), [0, 1]);
        // A text with no scan changes no lane when it goes in play.
        let deep = format!(
            "$: {}",
            "(".repeat(rustel_transpiler::MAX_SOURCE_NESTING + 1)
        );
        assert!(play(&mut lanes, &deep).is_empty());
        assert_eq!(play(&mut lanes, &tried), [0, 1]);
        // A text that does not go in play moves no lane of the score in
        // play: the lane that comes back keeps its number.
        let tried = format!("{lead}\n$: s(\"hh\")");
        assert_eq!(lanes.read(&tried), [0, 1]);
        let back = format!("{tried}\n{}", clap.replace("cp", "cp*4"));
        assert_eq!(lanes.read(&back), [0, 2, 1]);
        // The label rewrite of a read wrote places: the read says so, and
        // the score in play keeps the flag. The lane this read did not see
        // is away, and keeps its number.
        lanes.read(lead);
        lanes.wrote_places();
        assert!(lanes.read_of(lead).1.places());
        lanes.install(lead);
        assert!(lanes.playing().places());
        assert_eq!(play(&mut lanes, &format!("{lead}\n{clap}")), [0, 1]);
        assert!(!lanes.playing().places());
        // The view of a score: the number, the label, the line and the
        // plugin names of each lane. A score with no label has the lane of
        // the empty label.
        let (serum, ott) = (["serum".to_owned()], ["ott".to_owned()]);
        let view: Vec<_> = lanes.playing().lanes().collect();
        assert_eq!(view, [(0, "lead", 1, &serum[..]), (1, "$", 2, &ott[..])]);
        assert!(play(&mut lanes, "// lead\nnote(\"c2\").vsti(\"Serum\")").is_empty());
        let view: Vec<_> = lanes.playing().lanes().collect();
        assert_eq!(view, [(0, "", 2, &serum[..])]);

        // The output plays a score. 2 scores after it are not audible: an
        // edit of a lane, then a score with no such lane. A rollback gives
        // each lane of the first score its number.
        let mut lanes = LaneTable::default();
        let (kick, serum) = (r#"$: s("bd")"#, r#"$: note("c2").vsti("serum")"#);
        let score = format!("{kick}\n{serum}");
        assert_eq!(play(&mut lanes, &score), [0, 1]);
        lanes.mark_audible();
        assert_eq!(play(&mut lanes, &score.replace("c2", "c2 e2")), [0, 1]);
        assert_eq!(play(&mut lanes, kick), [0]);
        assert_eq!(play(&mut lanes, &score), [0, 1]);
        // The output plays the score with no such lane. The lane is away,
        // and keeps its number while a chain has it. With no chain, the
        // lane is new.
        assert_eq!(play(&mut lanes, kick), [0]);
        lanes.mark_audible();
        lanes.forget_away(|lane| lane == 1);
        assert_eq!(play(&mut lanes, &score), [0, 1]);
        assert_eq!(play(&mut lanes, kick), [0]);
        lanes.mark_audible();
        lanes.forget_away(|_| false);
        assert_eq!(play(&mut lanes, &score), [0, 2]);

        // A lane away takes a statement by rules 1 to 3, after the lanes in
        // play: of 2 lanes with equal text, the lane in play goes first.
        // The lead comes back with a new body by rule 3. Rule 4 takes no
        // lane away: an edit of a lane away gives a new lane.
        let mut lanes = LaneTable::default();
        let (lead, twin) = (
            r#"lead: note("c2").vsti("serum")"#,
            r#"$: s("bd").vst("ott")"#,
        );
        let score = format!("{lead}\n{twin}\n{twin}");
        assert_eq!(play(&mut lanes, &score), [0, 1, 2]);
        assert_eq!(play(&mut lanes, twin), [1]);
        let back = format!("{twin}\n{twin}\n{}", lead.replace("c2", "c3"));
        assert_eq!(play(&mut lanes, &back), [1, 2, 0]);
        assert_eq!(play(&mut lanes, twin), [1]);
        let edit = format!("{twin}\n$: s(\"bd*2\").vst(\"ott\")");
        assert_eq!(play(&mut lanes, &edit), [1, 3]);
        // A lane that came back is no longer away. An edit in play changes
        // the text the lane leaves with, so the text before the edit is a
        // new lane.
        let mut lanes = LaneTable::default();
        let (kick, hat) = (r#"$: s("bd")"#, r#"$: s("hh*4").vst("ott")"#);
        let both = |hat: &str| format!("{kick}\n{hat}");
        assert_eq!(play(&mut lanes, &both(hat)), [0, 1]);
        assert_eq!(play(&mut lanes, kick), [0]);
        assert_eq!(play(&mut lanes, &both(hat)), [0, 1]);
        assert_eq!(play(&mut lanes, &both(&hat.replace('4', "8"))), [0, 1]);
        assert_eq!(play(&mut lanes, kick), [0]);
        assert_eq!(play(&mut lanes, &both(hat)), [0, 2]);

        // An edit of each lane of a long score keeps each number. Past the
        // limit of the compare, each statement is a new lane.
        let long = |count: usize, tail: &str| -> String {
            let lane = |at: usize| format!("$: s(\"bd\").n({at}){tail}\n");
            (0..count).map(lane).collect()
        };
        for (count, first) in [(400, 0), (600, 600)] {
            let mut lanes = LaneTable::default();
            play(&mut lanes, &long(count, ""));
            let numbers = play(&mut lanes, &long(count, ".gain(.5)"));
            let kept: Vec<u64> = (first..first + count as u64).collect();
            assert_eq!(numbers, kept);
        }

        // Each commit of a session forgets the lanes away with no chain. A
        // note that plays gives its lane a chain. A session with no live
        // output ends that chain at once when the lane leaves, so the lane is
        // new when it comes back.
        let lane = |session: &crate::Session| {
            let report = session.query_report(Fraction::ZERO, Fraction::ONE);
            let value = serde_json::to_value(&report.expect("query").haps[0].value);
            value.expect("value")["__rustel_lane"].clone()
        };
        let (lead, kick) = (r#"lead: s("bd").vst("a plugin")"#, r#"kick: note("c e g")"#);
        let mut session = crate::Session::new().expect("session");
        for score in [lead, kick, lead] {
            session.evaluate(score).expect("score");
        }
        assert_eq!(lane(&session), "#2");
        session.schedule_audio_at(0.0, 48_000).expect("events");
        for score in [kick, lead] {
            session.evaluate(score).expect("score");
        }
        assert_eq!(lane(&session), "#4");

        // A document that plays for the first time shares the lanes of the
        // score in play: kick by rule 1, and the bass by rule 4 with the
        // plugin of the lead. The hat is a new lane.
        let play_in = |lanes: &mut LaneTable, document: u64, score: &str| {
            lanes.set_reading(document);
            play(lanes, score)
        };
        let mut lanes = LaneTable::default();
        let (lead, kick) = (r#"lead: note("c2").vsti("serum")"#, r#"kick: s("bd")"#);
        let (bass, hat) = (r#"bass: note("c1").vsti("serum")"#, r#"hat: s("hh")"#);
        assert_eq!(play_in(&mut lanes, 1, &format!("{lead}\n{kick}")), [0, 1]);
        let score = format!("{kick}\n{bass}\n{hat}");
        assert_eq!(play_in(&mut lanes, 2, &score), [1, 0, 2]);
        // Back in 1, the lead on a new note keeps its number, and kick is
        // away. A read of 1 with kick gives it its number by rule 1.
        let lead = lead.replace("c2", "e2");
        assert_eq!(play_in(&mut lanes, 1, &lead), [0]);
        assert_eq!((lanes.document(), numbers(&lanes.away)), (1, vec![1]));
        lanes.set_reading(1);
        assert_eq!(lanes.read(&format!("{lead}\n{kick}")), [0, 1]);

        // Own lanes first. 2 plays kick and a snare. 1 plays, and adds a
        // snare of its own: a new lane. A read of 2 gives its snare the
        // number of 2, and not the number of the snare of 1 in play.
        let mut lanes = LaneTable::default();
        let snare = r#"snare: sound("sd")"#;
        let (first, second) = (format!("{lead}\n{kick}"), format!("{kick}\n{snare}"));
        let third = format!("{first}\n{snare}");
        assert_eq!(play_in(&mut lanes, 1, &first), [0, 1]);
        assert_eq!(play_in(&mut lanes, 2, &second), [1, 2]);
        assert_eq!(play_in(&mut lanes, 1, &first), [0, 1]);
        assert_eq!(play_in(&mut lanes, 1, &third), [0, 1, 3]);
        lanes.set_reading(2);
        assert_eq!(lanes.read(&second), [1, 2]);
        // 1 is audible and 2 plays. An edit of 2 takes no lane of 1: its
        // lead is a new lane. A first read in 3 takes the lead of 1 by rule
        // 1, from the lanes the output plays. With no chain, no lane stays
        // at rest. A rollback reads the text of 1 in 1, and each lane of 1
        // keeps its number by the lanes the output plays.
        lanes.mark_audible();
        assert_eq!(play_in(&mut lanes, 2, &second), [1, 2]);
        lanes.set_reading(2);
        assert_eq!(lanes.read(&format!("{second}\n{lead}")), [1, 2, 4]);
        lanes.set_reading(3);
        assert_eq!(lanes.read(&lead), [0]);
        lanes.forget_away(|_| false);
        lanes.set_reading(1);
        assert_eq!(lanes.read(&third), [0, 1, 3]);
        // An install in the preview document puts the document in play at
        // rest. The next install drops the lanes of the preview.
        let preview = r#"$: note("c4").vsti("serum")"#;
        assert_eq!(play_in(&mut lanes, PREVIEW_DOCUMENT, preview), [5]);
        assert_eq!(lanes.document(), PREVIEW_DOCUMENT);
        assert_eq!(numbers(&lanes.rest[&2].lanes), [1, 2]);
        assert_eq!(play_in(&mut lanes, 1, &third), [0, 1, 3]);
        assert!(!lanes.rest.contains_key(&PREVIEW_DOCUMENT));
        // A close of 2 at rest drops its lanes and its lanes away. A close
        // of 1 in play keeps its lanes, and the next install drops them. A
        // direct pattern keeps the document in play.
        assert_eq!(play_in(&mut lanes, 2, kick), [1]);
        assert_eq!(play_in(&mut lanes, 1, &third), [0, 1, 3]);
        lanes.forget(2);
        assert!(lanes.is_closed(2) && !lanes.rest.contains_key(&2));
        lanes.forget(1);
        assert_eq!(numbers(&lanes.playing.lanes), [0, 1, 3]);
        assert_eq!(play_in(&mut lanes, 3, kick), [1]);
        assert!(!lanes.rest.contains_key(&1));
        lanes.install_pattern();
        assert_eq!((lanes.document(), numbers(&lanes.away)), (3, vec![1]));

        // The document that went to rest first has the lower rest order. An
        // install of a read of 2 while 1 plays puts 1 at rest with the
        // highest order, and takes 2 out of rest. One count serves each
        // table.
        let mut lanes = LaneTable::default();
        let first = first.as_str();
        for (document, score) in [(2, first), (2, kick), (3, kick), (1, first)] {
            play_in(&mut lanes, document, score);
        }
        let orders: HashMap<u64, u64> = lanes.rest_orders().collect();
        assert!(orders[&0] < orders[&2] && orders[&2] < orders[&3]);
        let after: HashMap<u64, u64> = lanes.rest_orders_for(2).collect();
        assert_eq!((after[&0], after[&3]), (orders[&0], orders[&3]));
        assert!(after.len() == 3 && after[&1] > orders[&3]);
        assert_eq!(lanes.rest_orders_for(1).collect::<HashMap<_, _>>(), orders);
        let mut other = LaneTable::default();
        play_in(&mut other, 1, kick);
        assert!(other.rest_orders().all(|(_, order)| order > orders[&3]));
        // A number serves one lane of a document: the second kick of 2 is a
        // new lane. The lanes away of 2 stay with 2.
        lanes.set_reading(2);
        assert_eq!(lanes.read(&format!("{kick}\n{kick}")), [1, 3]);
        assert_eq!(lanes.read(first), [0, 1]);
        assert_eq!(play_in(&mut lanes, 2, kick), [1]);
        assert_eq!(numbers(&lanes.away), [0]);
        // A lane at rest with a chain stays. With no chain, a document at
        // rest keeps no lane, and stays at rest. A closed document in play
        // does not go to rest.
        lanes.forget_away(|lane| lane == 1);
        assert_eq!(numbers(&lanes.rest[&1].lanes), [1]);
        lanes.forget_away(|_| false);
        let empty = |rest: &RestDocument| rest.lanes.is_empty() && rest.away.is_empty();
        assert!(lanes.rest.values().all(empty));
        assert_eq!(lanes.rest_orders().count(), 3);
        lanes.forget(2);
        assert!(lanes.rest_orders_for(3).all(|(document, _)| document != 2));

        // 2 reads a text with a new lane while 1 plays, and 3 rests with the
        // lead on another note. The session holds the text of 2, and 4 reads
        // another text. 3 takes the read of the hold: the lanes of 3 come
        // first, so the lead keeps its number by rule 3, and the pad that 3
        // lacks takes the number of the read of 2.
        let mut lanes = LaneTable::default();
        let (lead, pad) = (
            r#"lead: note("c3").vsti("serum")"#,
            r#"pad: note("e3").vst("ott")"#,
        );
        assert_eq!(play_in(&mut lanes, 3, lead), [0]);
        assert_eq!(play_in(&mut lanes, 1, kick), [1]);
        let text = format!("{}\n{pad}", lead.replace("c3", "e3"));
        lanes.set_reading(2);
        assert_eq!(lanes.read(&text), [2, 3]);
        lanes.hold(Some(text.as_str()));
        lanes.set_reading(4);
        assert_eq!(lanes.read(kick), [1]);
        lanes.set_reading(3);
        assert!(lanes.adopt_read(&text));
        assert_eq!(lanes.read(&text), [0, 3]);
    }
}
