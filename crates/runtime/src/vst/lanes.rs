//! The lanes of the scores of a session, each with a number that stays
//! through an edit.

use std::collections::{HashMap, HashSet};

use rustel_transpiler::LaneStatement;

use super::*;

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
    /// The plugin name of each call in the body: see [`body_plugin_names`].
    plugins: Vec<String>,
}

/// The lanes of one score text, as the chains of a session read them: see
/// [`LaneTable::playing`] and [`LaneTable::candidate`].
#[derive(Default)]
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
    /// plugin name of each call in its body.
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

/// The lanes of the scores of one session.
///
/// An edit gives a lane a new place, a new label, a mute mark or a new
/// body. The lane keeps its number through such an edit: see [`pair`]. A
/// plugin note of a labeled statement carries the number, so the owner of
/// its chain keeps its lane: see `ChainOwner`. A lane that leaves the text
/// keeps its number while a chain has it, so a lane that comes back from a
/// comment finds its chain.
#[derive(Default)]
pub(crate) struct LaneTable {
    /// The lanes of the score in play.
    playing: ScoreLanes,
    /// The last text the session read that is not in play, with its lanes.
    /// A new lane then keeps its number from one try of an edit to the
    /// next.
    candidate: Option<(String, ScoreLanes)>,
    /// The lanes of the score the output plays, until the output plays the
    /// score in play. A rollback puts that score back. Each of its lanes
    /// then keeps its number.
    audible: Vec<Lane>,
    /// The lanes that left the score in play, while a chain has their
    /// number: see [`Self::forget_away`].
    away: Vec<Lane>,
    /// True when the output plays the score in play.
    heard: bool,
    /// The number of the next new lane. No number comes a second time.
    next: u64,
}

impl LaneTable {
    /// Reads the lanes of a score text before its evaluation. Gives the
    /// lane number of each labeled statement, in text order, for
    /// [`rustel_transpiler::TranspileOptions::lane_numbers`]. A text with
    /// no scan gets no numbers and changes no lane, also when it goes in
    /// play.
    pub(crate) fn read(&mut self, source: &str) -> Vec<u64> {
        let Some(statements) = rustel_transpiler::lane_statements(source) else {
            self.candidate = None;
            return Vec::new();
        };
        let tried = self.candidate.as_ref();
        let tried = tried.map_or(&[][..], |(_, read)| &read.lanes);
        let paired = pair(
            &statements,
            &self.playing.lanes,
            tried,
            &self.away,
            &self.audible,
        );
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
        self.candidate = Some((source.to_owned(), read));
        numbers
    }

    /// The score with the text `source` goes in play: the lanes of the last
    /// read are now the lanes of the score in play, with the flag of
    /// [`Self::wrote_places`]. A read of a different text changes nothing
    /// here. A score with no read, such as a mini-notation score, has no
    /// lanes of its own. Each lane of the score before with no statement in
    /// this text is away: see [`Self::forget_away`]. The lanes of the score
    /// before stay for a rollback when the output plays that score: see
    /// [`Self::mark_audible`].
    pub(crate) fn install(&mut self, source: &str) {
        let read = self.candidate.take_if(|(text, _)| text == source);
        if let Some((_, read)) = read {
            self.put_in_play(read);
        }
    }

    /// A direct pattern goes in play: a score with no text, so with no
    /// lanes. Each lane of the score before is away.
    pub(crate) fn install_pattern(&mut self) {
        self.put_in_play(ScoreLanes::default());
    }

    /// The lanes of `read` are now the lanes of the score in play: see
    /// [`Self::install`].
    fn put_in_play(&mut self, read: ScoreLanes) {
        let before = std::mem::replace(&mut self.playing, read).lanes;
        let kept: HashSet<u64> = self.playing.lanes.iter().map(|lane| lane.number).collect();
        // A lane that came back is in play again.
        self.away.retain(|lane| !kept.contains(&lane.number));
        let left = before.iter().filter(|lane| !kept.contains(&lane.number));
        self.away.extend(left.cloned());
        // The output plays the score before until it plays this one.
        if std::mem::take(&mut self.heard) {
            self.audible = before;
        }
    }

    /// Forgets each lane away with no chain. `chained` gives true for the
    /// number of a lane with a chain. A lane away comes back with its
    /// number by rules 1 to 3 of [`pair`]. A lane that comes back after
    /// this is a new lane.
    pub(super) fn forget_away(&mut self, chained: impl Fn(u64) -> bool) {
        self.away.retain(|lane| chained(lane.number));
    }

    /// The output plays the score in play. A rollback then puts this score
    /// back, so the lanes of the score before go.
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn mark_audible(&mut self) {
        self.heard = true;
        self.audible.clear();
    }

    /// The label rewrite of the last read wrote places, and not the lane
    /// numbers of the read: see
    /// [`rustel_transpiler::TranspileOutput::lanes_by_place`]. The tag `#n`
    /// of a note of that text is then a place. The score in play keeps the
    /// flag when the text goes in play.
    pub(crate) fn wrote_places(&mut self) {
        if let Some((_, read)) = &mut self.candidate {
            read.places = true;
        }
    }

    /// The lanes of the score in play.
    pub(super) fn playing(&self) -> &ScoreLanes {
        &self.playing
    }

    /// The lanes of the last read, when it read the text `source`.
    pub(super) fn candidate(&self, source: &str) -> Option<&ScoreLanes> {
        let read = self.candidate.as_ref().filter(|(text, _)| text == source);
        read.map(|(_, read)| read)
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
/// that rules 1 to 3 did not pair is a new lane.
const MAX_LANE_PAIRS: usize = 1 << 18;

/// Gives each statement of a new score text its lane.
///
/// `playing` has the lanes of the score in play. `tried` has the lanes of
/// the last text the session read. It gives only its new lanes: a text that
/// did not go in play moves no lane of the score in play. `away` has the
/// lanes that left the score in play while a chain has them. It gives a
/// lane by rules 1 to 3, after `playing` and `tried`. `audible` has the
/// lanes of the score the output plays, when that score is not in play. It
/// gives a lane by rule 1 only: a rollback reads the text of that score
/// again. The result has the number of the lane of each statement, or
/// `None` for a new lane. A statement takes a lane by the first rule that
/// fits:
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
    playing: &[Lane],
    tried: &[Lane],
    away: &[Lane],
    audible: &[Lane],
) -> Vec<Option<u64>> {
    let mut numbers: Vec<Option<u64>> = vec![None; statements.len()];
    // The numbers with a statement, and the numbers of the score in play.
    let mut taken = HashSet::new();
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
        return numbers;
    }
    let mut pairs = Vec::new();
    for at in open {
        let statement = &statements[at];
        let mut names = body_plugin_names(&statement.body);
        // A name with more calls counts one time.
        names.dedup();
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
    numbers
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

/// The plugin name of each call in the code of a lane body, in its compare
/// form, sorted: a name with 2 calls is in the list 2 times. A call in a
/// comment names no plugin: a comment is a removal. The body has no label,
/// so no mute takes out a call here.
fn body_plugin_names(body: &str) -> Vec<String> {
    // A body with no `vst` in its text has no plugin call.
    if !body.contains("vst") {
        return Vec::new();
    }
    let calls = crate::lint::live_plugins(body).into_iter();
    let mut names: Vec<String> = calls.map(|call| canonical(&call.name)).collect();
    names.sort();
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

    /// The rules of [`pair`], each before the next one.
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
        assert!(lanes.candidate(lead).is_some_and(ScoreLanes::places));
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
    }
}
