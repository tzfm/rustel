//! Sound names mentioned in a score, read from the text.
//!
//! A public scan that panics answers as if it found nothing. These scans
//! run on every save, also in the studio's interface thread, and a fault in
//! one must not end a performance.
//!
//! A warm that queries the pattern finds only what the pattern plays now.
//! An artist keeps the next section commented out, or muted with `_$:`, and
//! enables it mid-set. The samples then download while the section already
//! plays. A scan of the source covers every lane the file mentions, playing
//! or not, and costs one pass.
//!
//! The scan does not evaluate the score. It also runs on saves that did not
//! parse (a tape keeps those), because their names are still worth warming.
//! For the same reason an open string, as the score stands mid-keystroke,
//! is read to the end of its line.
//!
//! The text also says which of a sound's variants it can play (`bd:3`,
//! `.n("<0 1>")`), and so which of a bank's files to load ahead and keep:
//! see [`to_keep`].

use std::collections::BTreeSet;

/// Sound names from `s("…")` / `sound("…")`, in first-seen order.
///
/// A name is read the way mini-notation reads a word (see [`name_char`]),
/// so `"bd*4 [hh mlkr-grsl:3]"` yields `bd`, `hh` and `mlkr-grsl:3`. A token
/// with no letters is an index rather than a name and is dropped - while
/// `808bd:1`, which starts with digits, is kept. The pattern may sit past a
/// space or on the lines below the call, and in any of the three quotes:
/// see [`string_argument`].
pub fn in_score(source: &str) -> Vec<String> {
    crate::catch_score_panic(|| in_score_scan(source)).unwrap_or_default()
}

fn in_score_scan(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pattern in sound_patterns(source) {
        for token in pattern.split(|c: char| !name_char(c) && c != ':') {
            let token = token.trim_start_matches('^').trim_matches(':');
            // A token with no letters is an index or a number, not a
            // name - but `808bd:1` is a real bank, so leading digits alone
            // cannot be the test.
            if token.is_empty() || !token.chars().any(|c| c.is_alphabetic()) {
                continue;
            }
            if !out.iter().any(|seen| seen == token) {
                out.push(token.to_string());
            }
        }
    }
    out
}

/// The pattern text of every `s(…)` and `sound(…)` in a score: every `s(`
/// first, then every `sound(`, each in the order written.
fn sound_patterns(source: &str) -> Vec<std::borrow::Cow<'_, str>> {
    let mut patterns = Vec::new();
    for call in ["s(", "sound("] {
        let mut from = 0usize;
        while let Some(at) = source[from..].find(call) {
            let start = from + at;
            from = start + call.len();
            // `samples('https://.../strudel.json')` ends in `s(`. It is not a
            // sound call, and its URL parts are not sound names. A call
            // starts only where the character before it cannot be part of an
            // identifier.
            if source[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            if let Some(pattern) = string_argument(&source[from..]) {
                patterns.push(pattern);
            }
        }
    }
    patterns
}

/// Which of a sound's variants a text can play: the files of a bank that
/// `n` picks between, or the fonts of a General MIDI name.
///
/// A variant is held the way playback reads `n`: rounded, not yet wrapped.
/// `0.5` is 1, and `-1` stays -1 until the library, which knows the number
/// of files, wraps it into the bank. The library decides which files a
/// bank holds and whether the note or `n` selects among them. This is
/// only what the text allows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Variants {
    /// These and no others.
    Only(BTreeSet<i64>),
    /// Any of them: the text computes `n`, or picks it in a way a scan
    /// cannot follow.
    All,
}

impl Variants {
    /// What a name written with no `:k` and no `n` plays: its first.
    pub fn first() -> Self {
        Self::Only(BTreeSet::from([0]))
    }

    /// `n` as playback rounds it before picking - `Math.round`, NaN as 0 -
    /// so the variant kept is the one the voice asks for.
    pub fn index(n: f64) -> i64 {
        if n.is_nan() {
            0
        } else {
            crate::samples::js_round(n) as i64
        }
    }

    /// Widen this to everything `other` can play as well.
    pub fn merge(&mut self, other: &Variants) {
        match (&mut *self, other) {
            (Variants::All, _) => {}
            (_, Variants::All) => *self = Variants::All,
            (Variants::Only(mine), Variants::Only(theirs)) => mine.extend(theirs),
        }
    }
}

/// How a text picks variants with `n`, wherever in it `n` is set: `None`
/// when it never is, `Only` the values written when every `n` is a
/// literal, `All` when any is computed.
///
/// Read like the sound names, commented lanes included: a lane enabled
/// mid-set plays what its `n` says at once. Anything this scan cannot
/// follow reads as `All`, which only ever keeps more:
///
/// - an `n(…)` whose argument is not a literal - a variable, `irand(8)`,
///   `run(4)`, a template with `${…}`, a string with letters or a `..`
///   range in it, or a string with more chained on it;
/// - an object key `n` (`.set({ n: … })`, `{ 'n': … }`, or the shorthand
///   `{ n }` for a variable of that name), an `.as("n …")`, and a
///   `withValue(` or `fmap(`, which can set any control;
/// - a `note` string whose `:` is not a plain number: `note` is the pair
///   `note:n`, so `note("c3:2")` plays the third variant as surely as
///   `n(2)` does;
/// - once any `n` is written, arithmetic anywhere in the text that could
///   shift it: `.add(…)`, `.add.squeeze(…)`, `.bor(…)`, `.lt(…)`,
///   `range(…)`, `Math.round(…)` and their kin. Which pattern each applies
///   to is not a scan's to know.
///
/// Two ways of writing `n` pick nothing. An `n(…)` with `.scale(…)` later
/// in its own chain plays no variant of it: `scale` turns `n` into a note,
/// so `n("0 2 4").scale("C:major").s("gm_piano")` plays the first font
/// and only that. And an `n(` still being typed - nothing after it yet but
/// the next lane - is a pause, not a computed `n`.
///
/// A literal's numbers are all collected, operator arguments with them -
/// the `2` of `"0*2"` - since keeping one variant too many costs memory
/// and missing one costs a late first hit.
pub fn variant_selection(source: &str) -> Option<Variants> {
    crate::catch_score_panic(|| variant_selection_scan(source)).unwrap_or_default()
}

fn variant_selection_scan(source: &str) -> Option<Variants> {
    // One pass over the text's identifiers: the engine reads every tab
    // this way whenever the tabs change, on the thread that feeds the
    // output.
    let mut uses_n = false;
    let mut computed = false;
    let mut shifted = false;
    let mut values = BTreeSet::new();
    for (at, word) in identifiers(source) {
        let after = &source[at + word.len()..];
        let arguments = after.trim_start().strip_prefix('(');
        match word {
            "withValue" | "fmap" if arguments.is_some() => return Some(Variants::All),
            "n" => match arguments {
                Some(arguments) if scaled(arguments) => {}
                Some(arguments) if still_typing(arguments) => uses_n = true,
                Some(arguments) => {
                    uses_n = true;
                    match literal_variants(arguments) {
                        Some(found) => values.extend(found),
                        None => computed = true,
                    }
                }
                None if n_is_an_object_key(source, at) => {
                    uses_n = true;
                    computed = true;
                }
                None => {}
            },
            "note" => match arguments.and_then(|arguments| note_variants(source, at, arguments)) {
                Some(Variants::Only(found)) => {
                    uses_n = true;
                    values.extend(found);
                }
                Some(Variants::All) => {
                    uses_n = true;
                    computed = true;
                }
                None => {}
            },
            "as" if arguments.and_then(string_argument).is_some_and(|fields| {
                fields
                    .split(|c: char| c == ':' || c.is_whitespace())
                    .any(|field| field == "n")
            }) =>
            {
                uses_n = true;
                computed = true;
            }
            word if shifts_n(word) && after.trim_start().starts_with(['(', '.']) => {
                shifted = true;
            }
            _ => {}
        }
    }
    if !uses_n {
        return None;
    }
    if computed || shifted {
        return Some(Variants::All);
    }
    Some(Variants::Only(values))
}

/// Calls and methods that can move a pattern's values, and so an `n`
/// written as a literal to one the literal does not name, besides the
/// operators the runtime composes values with: see [`shifts_n`].
const SHIFTS_N: [&str; 6] = ["range", "rangex", "range2", "round", "floor", "ceil"];

/// Whether a call or method named `word` can move an `n` written as a
/// literal: [`SHIFTS_N`], or any operator the runtime composes values with
/// but those that answer with one side's value as it stands (`set`,
/// `keep`, `keepif`, `and`, `or`), both of which the scan has read.
/// `.bor(n(2))` plays 3 over an `n("1")`, and `.lt(n(5))` makes it a truth
/// that plays take 0 or 1; reading the operators off the runtime's own
/// table keeps this list in step with it.
fn shifts_n(word: &str) -> bool {
    use rustel_core::compose::ComposeOp;
    SHIFTS_N.contains(&word)
        || ComposeOp::from_name(word).is_some_and(|op| {
            !matches!(
                op,
                ComposeOp::Set
                    | ComposeOp::Keep
                    | ComposeOp::KeepIf
                    | ComposeOp::And
                    | ComposeOp::Or
            )
        })
}

/// Whether the `n` at `at` is an object's key - `{ n: … }`, `{ s: "bd",
/// n: … }`, `{ 'n': … }` - or an object's shorthand for a variable of that
/// name, `{ n }`: either way the text sets `n` to something a scan cannot
/// read. A list of arguments with an `n` among them reads the same, which
/// only keeps more.
fn n_is_an_object_key(source: &str, at: usize) -> bool {
    let before = &source[..at];
    let after = &source[at + 1..];
    let opens = |text: &str| text.trim_end().ends_with(['{', ',']);
    if let Some(quote) = before
        .chars()
        .next_back()
        .filter(|c| matches!(c, '"' | '\'' | '`'))
        && let Some(rest) = after.strip_prefix(quote)
    {
        return opens(&before[..before.len() - quote.len_utf8()])
            && rest.trim_start().starts_with(':');
    }
    opens(before) && after.trim_start().starts_with([':', '}', ','])
}

/// What a `note` at `at` sets `n` to, from the text after its `(`: `note`
/// is the pair `note:n`, so `note("c3:2")` - or `"c3:2".note()` - plays the
/// third variant. `None` for a note that sets no `n`, a string with no
/// `:`, and one the scan cannot read: reading every `note(chord)` as any
/// would keep every file of every bank a melodic tab names.
fn note_variants(source: &str, at: usize, arguments: &str) -> Option<Variants> {
    if scaled(arguments) {
        return None;
    }
    let pattern = match string_argument(arguments) {
        Some(pattern) => pattern,
        // `"c3:2".note()`: the string it is called on.
        None if arguments.trim_start().starts_with(')') => receiver_string(&source[..at])?,
        None => return None,
    };
    if !pattern.contains(':') {
        return None;
    }
    let mut values = BTreeSet::new();
    for (colon, _) in pattern.match_indices(':') {
        match index_variants(&pattern[colon + 1..]) {
            Some(Variants::Only(found)) => values.extend(found),
            Some(Variants::All) => return Some(Variants::All),
            None => {}
        }
    }
    Some(Variants::Only(values))
}

/// The string a method is called on, from the text before the method's
/// name: `"c3:2".` gives `c3:2`. A plain string is looked for on its own
/// line, as it cannot cross one.
fn receiver_string(before: &str) -> Option<std::borrow::Cow<'_, str>> {
    let before = before.trim_end().strip_suffix('.')?.trim_end();
    let quote = before
        .chars()
        .next_back()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let body = &before[..before.len() - quote.len_utf8()];
    let from = if quote == '`' {
        0
    } else {
        body.rfind('\n').map_or(0, |line| line + 1)
    };
    let start = from + body[from..].rfind(quote)? + quote.len_utf8();
    let text = &body[start..];
    Some(if quote == '`' {
        std::borrow::Cow::Owned(without_interpolations(text))
    } else {
        std::borrow::Cow::Borrowed(text)
    })
}

/// Whether the call whose arguments begin `arguments` - the text after its
/// `(` - has `scale` applied after it in its own chain:
/// `n("0 2 4").s("gm_piano").scale("C:major")`. `scale` turns `n` into
/// `note`, so no value of that call picks a file or a font. A call still
/// open, a chain broken by anything but a method, or a `scale` applied to
/// what holds the call, is not: the `n` counts, which only keeps more.
fn scaled(arguments: &str) -> bool {
    let Some(close) = call_end(arguments) else {
        return false;
    };
    let mut rest = &arguments[close..];
    loop {
        let Some(method) = rest.trim_start().strip_prefix('.') else {
            return false;
        };
        let method = method.trim_start();
        let end = method
            .find(|c: char| !identifier_char(c))
            .unwrap_or(method.len());
        if end == 0 {
            return false;
        }
        let after = method[end..].trim_start();
        match after.strip_prefix('(') {
            Some(_) if &method[..end] == "scale" => return true,
            Some(inner) => match call_end(inner) {
                Some(close) => rest = &inner[close..],
                None => return false,
            },
            // `.add.squeeze(…)`: the chain goes on past a property.
            None => rest = after,
        }
    }
}

/// Where a call closes, as the index just past its `)` in `arguments` -
/// the text after its `(` - stepping over strings and nested brackets;
/// `None` while it is still open.
fn call_end(arguments: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut at = 0;
    while let Some(c) = arguments[at..].chars().next() {
        match c {
            '"' | '\'' | '`' => {
                let body = &arguments[at + 1..];
                let end = string_end(body, c);
                // Past its closing quote; one still open ends at its line.
                at += 1 + end + usize::from(body[end..].starts_with(c));
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
        at += c.len_utf8();
    }
    None
}

/// Whether an `n(` is still being typed: nothing follows it yet but the
/// end of the text or the next lane's label (`$:`, `bass:`), which no
/// argument can start with. Read as computed, a pause there made every
/// name of the tab play any, and a pinned tab queued every take of every
/// bank it names while the performer chose one. An `n()` closed empty is
/// not still being typed.
fn still_typing(arguments: &str) -> bool {
    let text = arguments.trim_start();
    let label = text
        .find(|c: char| !identifier_char(c))
        .unwrap_or(text.len());
    text.is_empty() || (label > 0 && text[label..].starts_with(':'))
}

/// Each sound `s("…")` / `sound("…")` names, in first-seen order, with the
/// variants its own spelling picks: `bd` the first, `bd:3` the one `n = 3`
/// picks, and `bd:<0 2>` - an index that is not a plain number - any. A
/// name written twice picks both: `bd bd:3` is the first and the fourth.
/// A `:` with no name before it, as in `<bd sd>:3`, indexes names this
/// scan cannot pair it with, so every name of its pattern picks any.
pub fn variants_in_score(source: &str) -> Vec<(String, Variants)> {
    crate::catch_score_panic(|| variants_in_score_scan(source)).unwrap_or_default()
}

fn variants_in_score_scan(source: &str) -> Vec<(String, Variants)> {
    sounds_in_score(source).list
}

/// [`variants_in_score`], with the plain indices each name is spelled with.
fn sounds_in_score(source: &str) -> Named {
    let mut out = Named::default();
    for pattern in sound_patterns(source) {
        let mut here = Named::default();
        let loose = spelled_variants(&pattern, &mut here);
        for (name, mut variants) in here.list {
            if loose {
                variants = Variants::All;
            }
            out.merge(name, &variants);
        }
        for (name, indices) in &here.spelled {
            out.spell(name, indices);
        }
    }
    out
}

/// The sound names a text can make the engine look up, whether the lane
/// is live, muted or commented out, each with the variants the text can
/// play: see [`variants_in_score`] and [`variant_selection`], whose picks
/// are added to every name's own. Every name also comes under each bank
/// the text's `.bank()` calls name - `s("bd:3").bank("tr909")` reaches
/// `tr909_bd` as well as `bd`, both at the fourth variant - banked
/// spelling first, the one the voice actually looks up.
///
/// With no `n`, `s("recordings recordings:3")` can play takes 0 and 3 and
/// nothing else; `.n("<0 1 2>")` adds 0, 1 and 2 to every name; a computed
/// `n` makes every name play any.
pub fn to_keep(source: &str) -> Vec<(String, Variants)> {
    crate::catch_score_panic(|| to_keep_scan(source)).unwrap_or_default()
}

fn to_keep_scan(source: &str) -> Vec<(String, Variants)> {
    kept_in_score(source).list
}

/// [`to_keep`] narrowed to what a tab loads before its first play. A name
/// that can play any variant asks for its first file and for the indices its
/// text spells, not for every file. Set `every_variant` when a setup can
/// write `n` for every name; the indices in the text are then kept as well.
pub fn to_load_ahead(source: &str, every_variant: bool) -> Vec<(String, Variants)> {
    crate::catch_score_panic(|| to_load_ahead_scan(source, every_variant)).unwrap_or_default()
}

fn to_load_ahead_scan(source: &str, every_variant: bool) -> Vec<(String, Variants)> {
    let kept = kept_in_score(source);
    kept.list
        .iter()
        .map(|(name, variants)| {
            let mut ahead = match variants {
                Variants::Only(_) if !every_variant => return (name.clone(), variants.clone()),
                Variants::Only(values) => values.clone(),
                Variants::All => kept.spelled.get(name).cloned().unwrap_or_default(),
            };
            // A bare name plays the first file.
            ahead.insert(0);
            (name.clone(), Variants::Only(ahead))
        })
        .collect()
}

/// [`to_keep`], with the plain indices each name is spelled with.
fn kept_in_score(source: &str) -> Named {
    let selection = variant_selection(source);
    let banks = banks_in_score(source);
    let sounds = sounds_in_score(source);
    let mut kept = Named::default();
    for (sound, mut variants) in sounds.list {
        if let Some(selection) = &selection {
            variants.merge(selection);
        }
        let spelled = sounds.spelled.get(&sound);
        for bank in &banks {
            let banked = format!("{bank}_{sound}");
            if let Some(spelled) = spelled {
                kept.spell(&banked, spelled);
            }
            kept.merge(banked, &variants);
        }
        if let Some(spelled) = spelled {
            kept.spell(&sound, spelled);
        }
        kept.merge(sound, &variants);
    }
    kept
}

/// Names in first-seen order, each with the variants merged over every
/// place it is written.
#[derive(Default)]
struct Named {
    list: Vec<(String, Variants)>,
    at: std::collections::HashMap<String, usize>,
    /// The plain indices each name is written with - the `2` of
    /// `gm_epiano1:2` - which `list` loses to `All` once the name can play
    /// any. A warm asking for the whole of such a name still asks for
    /// these: a General MIDI name asked for bare loads its first font, and
    /// the one its text spells is the one it plays.
    spelled: std::collections::HashMap<String, BTreeSet<i64>>,
}

impl Named {
    fn merge(&mut self, name: String, variants: &Variants) {
        match self.at.get(&name) {
            Some(&index) => self.list[index].1.merge(variants),
            None => {
                self.at.insert(name.clone(), self.list.len());
                self.list.push((name, variants.clone()));
            }
        }
    }

    fn spell(&mut self, name: &str, indices: &BTreeSet<i64>) {
        match self.spelled.get_mut(name) {
            Some(spelled) => spelled.extend(indices),
            None => {
                self.spelled.insert(name.to_owned(), indices.clone());
            }
        }
    }
}

/// Read one pattern's sound names into `named`, each with the variant its
/// `:` picks, and say whether a `:` stood with no name before it.
///
/// Tokens are split where [`in_score`] splits them, so the names are the
/// same; the index after a name's `:`, such as `-1` or `0.5`, is the variant
/// it picks. Fields past the index - the gain of `bd:3:0.5` - pick no
/// variant. A `:` with no index after it yet is read as though it were not
/// there: see [`index_variants`].
fn spelled_variants(pattern: &str, named: &mut Named) -> bool {
    let mut loose = false;
    let mut rest = pattern;
    while let Some(c) = rest.chars().next() {
        if c == ':' {
            rest = &rest[1..];
            loose |= index_variants(rest).is_some();
            continue;
        }
        if !name_char(c) {
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let end = rest.find(|c: char| !name_char(c)).unwrap_or(rest.len());
        let name = rest[..end].trim_start_matches('^');
        rest = &rest[end..];
        let picked = match rest.strip_prefix(':') {
            None => None,
            Some(index) => {
                let tail = index
                    .find(|c: char| !(name_char(c) || c == ':'))
                    .unwrap_or(index.len());
                rest = &index[tail..];
                index_variants(index)
            }
        };
        // A token with no letters is a number, not a name: see `in_score`.
        if name.chars().any(char::is_alphabetic) {
            if let Some(Variants::Only(indices)) = &picked {
                named.spell(name, indices);
            }
            named.merge(
                name.to_owned(),
                picked.as_ref().unwrap_or(&Variants::first()),
            );
        }
    }
    loose
}

/// What the index after a `:` picks: the plain number it starts with, or
/// any for one that is not a plain number (`<0 1>`, `[0 2]`). `None` when
/// there is no index yet: nothing after the colon, or a bracket or the end
/// of the quote directly after it. Mini-notation cannot play that form. It
/// occurs while the performer types the index, as in `s("recordings:`, and
/// must not select every variant.
fn index_variants(index: &str) -> Option<Variants> {
    let index = index.trim_start();
    if index.is_empty() || index.starts_with([']', '>', '}', ',', '|', ')']) {
        return None;
    }
    Some(
        leading_number(index)
            .filter(|(_, used)| !index[*used..].starts_with(name_char))
            .map_or(Variants::All, |(n, _)| {
                Variants::Only(BTreeSet::from([Variants::index(n)]))
            }),
    )
}

/// Whether `c` can be part of a sound name: whatever mini-notation reads
/// as part of one word, so `mlkr-grsl` and `a.b` are each one name. A `^`
/// opening a name is the mini-notation's steps marker, and the readers
/// here leave it out.
fn name_char(c: char) -> bool {
    rustel_mini::is_step_char(c)
}

/// Whether `c` can continue a JavaScript identifier.
fn identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Every identifier in `source`, as where it starts and what it is. A
/// word is only ever read whole, so `pan` holds no `n`.
fn identifiers(source: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut chars = source.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars.next_if(|&(_, c)| !identifier_char(c)).is_some() {}
        let (start, first) = chars.next()?;
        let mut end = start + first.len_utf8();
        while let Some((at, c)) = chars.next_if(|&(_, c)| identifier_char(c)) {
            end = at + c.len_utf8();
        }
        Some((start, &source[start..end]))
    })
}

/// The variants an `n(…)` picks when its argument is a literal, from the
/// text after its `(`; `None` when it is anything else. A string still
/// being typed is read as far as it goes.
fn literal_variants(arguments: &str) -> Option<Vec<i64>> {
    let text = arguments.trim_start();
    let quote = text.chars().next()?;
    if matches!(quote, '"' | '\'' | '`') {
        let body = &text[1..];
        let end = string_end(body, quote);
        let pattern = &body[..end];
        // A template's `${…}` is code.
        if quote == '`' && pattern.contains("${") {
            return None;
        }
        // The literal has to BE the argument: `n("0 1".add(2))` computes.
        if body[end..].starts_with(quote) && !body[end + 1..].trim_start().starts_with(')') {
            return None;
        }
        return mini_numbers(pattern);
    }
    let (value, used) = leading_number(text)?;
    text[used..]
        .trim_start()
        .starts_with(')')
        .then(|| vec![Variants::index(value)])
}

/// Every number in a mini-notation string of numbers, or `None` when the
/// string holds anything else: a letter, a `..` range, a character no
/// numeric pattern needs.
fn mini_numbers(pattern: &str) -> Option<Vec<i64>> {
    if pattern.contains("..")
        || !pattern
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || "~?!@*/<>[]{},|().-_:%".contains(c))
    {
        return None;
    }
    let mut values = Vec::new();
    let mut after_digit = false;
    let mut at = 0;
    while at < pattern.len() {
        let rest = &pattern[at..];
        let Some((value, used)) = leading_number(rest) else {
            let c = rest.chars().next().expect("not at the end");
            after_digit = c.is_ascii_digit();
            at += c.len_utf8();
            continue;
        };
        values.push(Variants::index(value));
        // `.5` may be a half or a step separator before a 5, and the `-`
        // of `0-1` a sign or a rest: both readings are kept.
        if (rest.starts_with('.') || (rest.starts_with('-') && after_digit))
            && let Some((other, _)) = leading_number(rest.trim_start_matches(['-', '.']))
        {
            values.push(Variants::index(other));
        }
        after_digit = true;
        at += used;
    }
    Some(values)
}

/// The number `text` starts with, and how many bytes it takes: an
/// optional `-`, digits, and a fraction - `3`, `-1`, `0.5`, `.5`.
pub(crate) fn leading_number(text: &str) -> Option<(f64, usize)> {
    let bytes = text.as_bytes();
    let mut end = usize::from(bytes.first() == Some(&b'-'));
    let mut digits = 0usize;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
        digits += 1;
    }
    if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    text[..end].parse::<f64>().ok().map(|value| (value, end))
}

/// The names a score hands to `.bank("…")`, in first-seen order, so
/// `s("bd")` can be looked up as `<bank>_bd` too. A bank pattern -
/// `"<tr808 tr909>"` - yields every machine in it.
pub fn banks_in_score(source: &str) -> Vec<String> {
    crate::catch_score_panic(|| banks_in_score_scan(source)).unwrap_or_default()
}

fn banks_in_score_scan(source: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("bank(") {
        rest = &rest[at + "bank(".len()..];
        // Read the way the sound names are: past a space, in any quote, and
        // to the end of the line while the string is still open.
        patterns.extend(string_argument(rest));
    }
    bank_pattern_machines(&patterns.join(" "))
}

/// The machines a bank pattern names, each once, in the order written:
/// `"<tr808 tr909> tr808"` names `tr808` and `tr909`. A machine is read the
/// way a sound name is (see [`name_char`]); a word with no letter or digit
/// in it, such as the rest `~`, is none.
pub fn bank_pattern_machines(pattern: &str) -> Vec<String> {
    crate::catch_score_panic(|| bank_pattern_machines_scan(pattern)).unwrap_or_default()
}

fn bank_pattern_machines_scan(pattern: &str) -> Vec<String> {
    let mut machines: Vec<String> = Vec::new();
    for machine in pattern.split(|c: char| !name_char(c)) {
        let machine = machine.trim_start_matches('^');
        if machine.chars().any(char::is_alphanumeric)
            && !machines.iter().any(|seen| seen == machine)
        {
            machines.push(machine.to_owned());
        }
    }
    machines
}

/// The sound names a score's text asks to have loaded ahead, each spelled
/// the way a live warm reads a variant: `name:k,j,…` for the variants the
/// text can play (see [`to_keep`]), and the bare `name` - its first file -
/// when it can play any. `s("bd").bank("tr909")` asks
/// for `tr909_bd:0` and `bd:0`, not every kick either kit holds.
///
/// One ask a name, however many variants. An ask for each variant ran
/// past what a manifest may queue once a text's `n` values met many names
/// under several banks, and the warm, refused whole, loaded nothing at
/// all; each ask is also a lookup on the thread that feeds the output.
///
/// A bank the note picks from is not special here: a bare name asks for
/// its first file, and the note about to play fetches its own. A General MIDI name
/// asked for bare loads its first font alone, as a live warm always has:
/// a font is megabytes, and the playing score's query-driven warm fetches
/// the fonts it actually reaches. So a name that can play any is also
/// asked for at the variants its text names - `gm_epiano1:2` under an `n`
/// the text computes still warms font 2, the one it plays - after the bare
/// name, so a newest-first line of bets fetches them first.
///
/// `every_variant` is for a setup that picks variants: its helpers can set
/// `n` for any score, so every name asks for the lot, and for what its
/// text picks besides.
///
/// The banked spelling comes first: it is the one the voice actually looks
/// up, and a plain `bd` that resolves out of a different bank is the wrong
/// file to have spent the fetch on.
pub fn to_warm(source: &str, every_variant: bool) -> Vec<String> {
    crate::catch_score_panic(|| to_warm_scan(source, every_variant)).unwrap_or_default()
}

fn to_warm_scan(source: &str, every_variant: bool) -> Vec<String> {
    let kept = kept_in_score(source);
    let mut names = Vec::new();
    for (name, variants) in &kept.list {
        let named = match variants {
            Variants::Only(values) if !every_variant => {
                names.extend(variant_spec(name, values.iter()));
                continue;
            }
            Variants::Only(values) => Some(values),
            Variants::All => kept.spelled.get(name),
        };
        names.push(name.clone());
        // The bare name has the first already.
        names.extend(
            named.and_then(|values| variant_spec(name, values.iter().filter(|&&index| index != 0))),
        );
    }
    names
}

/// `name:k,j,…` for these variants, or nothing for none.
fn variant_spec<'a>(name: &str, values: impl Iterator<Item = &'a i64>) -> Option<String> {
    let mut spec = format!("{name}:");
    let bare = spec.len();
    for value in values {
        if spec.len() > bare {
            spec.push(',');
        }
        spec.push_str(&value.to_string());
    }
    (spec.len() > bare).then_some(spec)
}

/// One `samples(...)` a score wrote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SamplesImport {
    /// The string argument as written - `github:user/repo`, a URL, a JSON
    /// map - or `None` for a call whose argument is not a string: an
    /// object, a variable, a string still being typed.
    pub spec: Option<String>,
    /// The banks an inline map brings - `samples({ bd: 'kick.wav' })`
    /// brings `bd` - when the argument is an object literal this text can
    /// read; `None` for anything else.
    pub keys: Option<Vec<String>>,
    /// Where the argument's text sits in the source: between the quotes
    /// of a readable one, else the call's own name.
    pub from: usize,
    pub to: usize,
}

/// The sources a score hands to `samples(...)`, in order.
pub fn samples_specs(source: &str) -> Vec<Option<String>> {
    crate::catch_score_panic(|| samples_specs_scan(source)).unwrap_or_default()
}

fn samples_specs_scan(source: &str) -> Vec<Option<String>> {
    samples_imports(source)
        .into_iter()
        .map(|import| import.spec)
        .collect()
}

/// Every `samples(...)` a score wrote, in order, with where it is. A call
/// inside a comment or a string is not one: the score never makes it.
pub fn samples_imports(source: &str) -> Vec<SamplesImport> {
    crate::catch_score_panic(|| samples_imports_scan(source)).unwrap_or_default()
}

fn samples_imports_scan(source: &str) -> Vec<SamplesImport> {
    // The call is found in the code alone; its argument, blanked there, is
    // read from the source at the same offsets.
    let code = crate::lint::code_only(source);
    let mut imports = Vec::new();
    let mut from = 0usize;
    while let Some(at) = code[from..].find("samples") {
        let start = from + at;
        from = start + "samples".len();
        // `resamples(` is somebody else's function, and `samples` followed
        // by anything but a call is a word.
        if code[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let Some(arguments) = code[from..].trim_start().strip_prefix('(') else {
            continue;
        };
        let rest = source[code.len() - arguments.len()..].trim_start();
        let rest_at = source.len() - rest.len();
        let spec = rest
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')
            .and_then(|quote| {
                let body = &rest[1..];
                let end = string_end(body, quote);
                if !body[end..].starts_with(quote) {
                    return None;
                }
                // The literal has to BE the argument: `'github:a/b' + kit`
                // registers a map this text does not name.
                let after = body[end + quote.len_utf8()..].trim_start();
                (after.starts_with(')') || after.starts_with(','))
                    .then(|| (body[..end].to_owned(), rest_at + 1, rest_at + 1 + end))
            });
        imports.push(match spec {
            Some((spec, from, to)) => SamplesImport {
                spec: Some(spec),
                keys: None,
                from,
                to,
            },
            None => SamplesImport {
                spec: None,
                keys: rest
                    .starts_with('{')
                    .then(|| inline_map_keys(rest))
                    .flatten(),
                from: start,
                to: start + "samples".len(),
            },
        });
    }
    imports
}

/// The top-level keys of an object literal starting at `text` - the bank
/// names a `samples({ ... })` map brings - or `None` if the braces never
/// close. Keys are bare or quoted; `_base` is the map's address, not a
/// bank; a value may hold objects, arrays, calls and strings of its own.
fn inline_map_keys(text: &str) -> Option<Vec<String>> {
    let mut keys = Vec::new();
    let mut chars = text.char_indices().peekable();
    // Past the opening brace.
    chars.next()?;
    let mut depth = 1usize;
    let mut expect_key = true;
    while let Some((at, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => {
                let body = &text[at + 1..];
                let mut end = 0usize;
                let mut escaped = false;
                for (offset, inner) in body.char_indices() {
                    if escaped {
                        escaped = false;
                    } else if inner == '\\' {
                        escaped = true;
                    } else if inner == c {
                        end = offset;
                        break;
                    }
                }
                if end == 0 && !body.starts_with(c) {
                    return None;
                }
                let key = body[..end].to_owned();
                // Step past the string.
                let close = at + 1 + end;
                while chars.peek().is_some_and(|&(index, _)| index <= close) {
                    chars.next();
                }
                if expect_key && depth == 1 && follows_colon(&text[close + c.len_utf8()..]) {
                    if key != "_base" {
                        keys.push(key);
                    }
                    expect_key = false;
                }
            }
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(keys);
                }
            }
            ',' if depth == 1 => expect_key = true,
            '/' if text[at..].starts_with("//") => {
                while chars.peek().is_some_and(|&(_, inner)| inner != '\n') {
                    chars.next();
                }
            }
            c if c.is_whitespace() => {}
            c if expect_key && depth == 1 && (c.is_alphanumeric() || c == '_' || c == '$') => {
                let mut end = at + c.len_utf8();
                while chars.peek().is_some_and(|&(_, inner)| {
                    inner.is_alphanumeric() || inner == '_' || inner == '$'
                }) {
                    let (index, inner) = chars.next().expect("peeked");
                    end = index + inner.len_utf8();
                }
                if follows_colon(&text[end..]) {
                    let key = &text[at..end];
                    if key != "_base" {
                        keys.push(key.to_owned());
                    }
                }
                expect_key = false;
            }
            _ => expect_key = false,
        }
    }
    None
}

fn follows_colon(text: &str) -> bool {
    text.trim_start().starts_with(':')
}

/// The text of the string a call's first argument opens, if it opens one.
///
/// Whitespace may come first, as in `s( "bd")` or a pattern written on the
/// lines below its `s(`. The quote may be any of JavaScript's three. A
/// backtick string can cross lines, and a lane written that way is often
/// one a set keeps commented out for later, so it must be read too. A
/// template's `${...}` holds code, not names, and is left out. The studio's
/// bank list reads a `.bank(...)` argument here too, so the bank it
/// connects is the one preload reads.
pub fn string_argument(rest: &str) -> Option<std::borrow::Cow<'_, str>> {
    let trimmed = rest.trim_start();
    let quote = trimmed
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let body = &trimmed[quote.len_utf8()..];
    let text = &body[..string_end(body, quote)];
    Some(if quote == '`' {
        std::borrow::Cow::Owned(without_interpolations(text))
    } else {
        std::borrow::Cow::Borrowed(text)
    })
}

/// A template's text with each `${…}` blanked, so the code inside is not
/// read as names and the names either side of it stay apart.
fn without_interpolations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if depth > 0 {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        } else if c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            depth = 1;
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Where a string that opened with `quote` ends, as an index into `body`:
/// at its closing quote, or - still being typed - at the end of its line,
/// so an open `s("bd sd` already names `bd` and `sd` and nothing on the
/// line below.
pub fn string_end(body: &str, quote: char) -> usize {
    let line = body.find('\n').unwrap_or(body.len());
    // A template crosses lines. It ends at a backtick the argument can end
    // at - before `)`, `,`, `.`, `+` or `;`, or at the end of the text - so
    // a backtick that opens another string further down does not close it.
    // One still open is read like an open quote, to the end of its line.
    if quote == '`' {
        let mut from = 0;
        while let Some(at) = body[from..].find('`') {
            let end = from + at;
            let after = body[end + 1..].trim_start();
            if after.is_empty() || after.starts_with([')', ',', '.', '+', ';']) {
                return end;
            }
            from = end + 1;
        }
        return line;
    }
    // A plain string cannot cross a line, so the line bounds the search:
    // a quote on the line below opens that line's string, not closes this.
    body[..line].find(quote).unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An inline map's keys are the banks it brings, however the map is
    /// laid out; a map that cannot be read - a variable, an open brace -
    /// brings nothing this text can name.
    #[test]
    fn an_inline_maps_keys_are_read_off_it() {
        let source = "samples({\n  pipi: 'bd/BT0AADA.wav', // the kick\n  'hi hat': 'hh27/000.wav',\n  snaredrum: ['sd/a.wav', 'sd/b:c.wav'],\n  _base: 'https://x/',\n  nested: { bd: 'no' },\n  picked: pick(['a', 'b'], 0),\n}, 'https://raw.githubusercontent.com/tidalcycles/Dirt-Samples/master/');\nsamples(banks)\nsamples({ open: 'x'";
        let imports = samples_imports(source);
        assert_eq!(imports.len(), 3);
        assert_eq!(
            imports[0].keys.as_deref(),
            Some(&["pipi", "hi hat", "snaredrum", "nested", "picked"].map(str::to_owned)[..])
        );
        assert_eq!(imports[0].spec, None);
        assert_eq!(imports[1].keys, None);
        assert_eq!(imports[2].keys, None, "an open map brings nothing yet");
    }

    #[test]
    fn an_import_is_placed_between_its_quotes() {
        let source =
            "samples('github:a/b')\nsamples(banks)\nsamples (\"https://x/y.json\", 'base/')";
        let imports = samples_imports(source);
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].spec.as_deref(), Some("github:a/b"));
        assert_eq!(&source[imports[0].from..imports[0].to], "github:a/b");
        assert_eq!(imports[1].spec, None);
        assert_eq!(&source[imports[1].from..imports[1].to], "samples");
        assert_eq!(imports[2].spec.as_deref(), Some("https://x/y.json"));
        assert_eq!(&source[imports[2].from..imports[2].to], "https://x/y.json");
    }

    /// A `samples(…)` inside a comment or a string is no call: nothing
    /// registers it and nothing looks it up, so it can never refuse the score.
    #[test]
    fn a_samples_call_in_a_comment_or_a_string_is_not_an_import() {
        let source = "// samples('github:a/b')\n/* samples('github:c/d')\n*/ s(\"samples('github:e/f')\")\n  samples( 'github:g/h') // samples('github:i/j')";
        assert_eq!(samples_specs(source), [Some("github:g/h".to_owned())]);
        let imports = samples_imports(source);
        assert_eq!(&source[imports[0].from..imports[0].to], "github:g/h");
        assert!(samples_imports("// samples({ bd: 'x.wav' })").is_empty());
    }

    #[test]
    fn a_scores_samples_calls_are_read_back_when_they_can_be() {
        let specs = samples_specs(
            "samples('github:a/b')\nsamples (\"https://x/y.json\", 'base/')\nsamples({bd: 'x'})\nsamples(banks)\nresamples('no')\nsamples('github:a/b' + kit)\nsamples('open",
        );
        assert_eq!(
            specs,
            [
                Some("github:a/b".to_owned()),
                Some("https://x/y.json".to_owned()),
                None,
                None,
                None,
                None,
            ]
        );
    }

    /// The score as it stands mid-keystroke: the string is still open, and
    /// the names typed so far are already worth asking for.
    #[test]
    fn an_open_string_names_what_has_been_typed_so_far() {
        let names = in_score("$: s(\"bd sd\n$: s(\"hh\")");
        assert_eq!(names, ["bd", "sd", "hh"]);
        assert_eq!(in_score("s(\"cp"), ["cp"]);
        assert!(in_score("s(\"").is_empty());
    }

    /// A space after the parenthesis is still the call's pattern.
    #[test]
    fn a_space_before_the_pattern_is_still_its_call() {
        assert_eq!(
            in_score("s( \"bd\")\n$: s(\t\"hh\")\nsound(  'sd')"),
            ["bd", "hh", "sd"]
        );
        assert!(
            in_score("s( drums)").is_empty(),
            "a variable names nothing here"
        );
    }

    /// Backticks are how a pattern is written across lines - and such a
    /// lane, kept commented out for later, is exactly one that must be
    /// loaded ahead.
    #[test]
    fn a_pattern_in_backticks_is_read_across_its_lines() {
        let names = in_score(
            "// $: s(`<bd sd>\n//   hh*4 [cp rim]`)\n$: s(\n  `oh:2\n   lt`\n)\n$: s(\"ht\")",
        );
        assert_eq!(names, ["bd", "sd", "hh", "cp", "rim", "oh:2", "lt", "ht"]);
    }

    /// A template's `${…}` is code: its words are not sound names.
    #[test]
    fn a_templates_code_is_not_read_as_names() {
        assert_eq!(
            in_score("s(`bd ${pick(kit, {a: 'x'})} sd${n}hh`)"),
            ["bd", "sd", "hh"]
        );
        assert_eq!(
            in_score("s(`bd ${fill ? `sd` : `hh`} cp`).fast(2)"),
            ["bd", "cp"],
            "a template inside one is code too"
        );
    }

    /// A backtick still being typed is read like an open quote: to the end
    /// of its line, not to whatever backtick comes further down.
    #[test]
    fn an_open_backtick_reads_to_the_end_of_its_line() {
        assert_eq!(
            in_score("s(`bd sd\nconst later = `note`\n$: s(\"cp\")"),
            ["bd", "sd", "cp"]
        );
    }

    #[test]
    fn a_commented_out_lane_is_still_warmed() {
        // The lane an artist enables mid-set is exactly the one whose samples
        // would otherwise download while it is already playing.
        let names = in_score(
            "$: s(\"bd*4\")\n// $: s(\"gm_choir_aahs:1\").note(\"c0\")\n_$: s(\"clap:6\")\n",
        );
        assert!(names.contains(&"bd".to_string()));
        assert!(names.contains(&"gm_choir_aahs:1".to_string()), "{names:?}");
        assert!(names.contains(&"clap:6".to_string()), "{names:?}");
    }

    #[test]
    fn mini_notation_operators_are_separators_and_indexes_are_not_names() {
        let names = in_score(r#"s("[bd*4 hh:3?] <sd cp:2>!2 ~ 808bd:1")"#);
        assert_eq!(
            names,
            vec!["bd", "hh:3", "sd", "cp:2", "808bd:1"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }

    /// A name is a mini-notation word, so a `-`, a `.` or a `#` inside it is
    /// part of it: `mlkr-grsl:3` is the fourth `mlkr-grsl`, as the parser
    /// reads it.
    #[test]
    fn hyphenated_and_dotted_names_are_one_name() {
        let text = r#"s("mlkr-grsl:3 [tr-808:2 a.b]*2 kit#1")"#;
        assert_eq!(in_score(text), ["mlkr-grsl:3", "tr-808:2", "a.b", "kit#1"]);
        let one = |value: i64| Variants::Only(BTreeSet::from([value]));
        assert_eq!(
            to_keep(text),
            [
                ("mlkr-grsl".to_owned(), one(3)),
                ("tr-808".to_owned(), one(2)),
                ("a.b".to_owned(), one(0)),
                ("kit#1".to_owned(), one(0)),
            ]
        );
        assert_eq!(
            to_warm(text, false),
            ["mlkr-grsl:3", "tr-808:2", "a.b:0", "kit#1:0"]
        );
    }

    /// Whitespace, commas and the mini-notation operators still part two
    /// names, a rest is none, and a `^` opening a sequence marks its steps
    /// rather than starting a name.
    #[test]
    fn names_still_part_at_separators_and_operators() {
        assert_eq!(
            in_score(r#"s("bd,sd hh*2 [cp cp]!2 <oh rim>@3 lt? mt(3,8) ht/2 cb|cr ~ - [^rd sh]")"#),
            [
                "bd", "sd", "hh", "cp", "oh", "rim", "lt", "mt", "ht", "cb", "cr", "rd", "sh"
            ]
        );
        assert_eq!(
            to_keep(r#"s("^bd:2 - sd")"#),
            [
                ("bd".to_owned(), Variants::Only(BTreeSet::from([2]))),
                ("sd".to_owned(), Variants::first()),
            ]
        );
    }

    #[test]
    fn single_quotes_count_and_names_are_not_repeated() {
        let names = in_score("s('bd').fast(2)\ns(\"bd sd\")\nsound('sd')");
        assert_eq!(names, vec!["bd".to_string(), "sd".to_string()]);
    }

    /// `.bank()` makes the sound `tr909_bd`, so the banked spelling comes
    /// first. The plain spelling follows for a score whose bank does not
    /// hold the name. Each is requested at the variants the text plays: a
    /// name with no index and no `n` plays its first.
    #[test]
    fn names_to_warm_carry_every_bank_a_score_names() {
        assert_eq!(
            to_warm(r#"s("bd:3 hh").bank("tr909")"#, false),
            ["tr909_bd:3", "bd:3", "tr909_hh:0", "hh:0"]
        );
        // No bank is the ordinary case, and one name is one entry however
        // many times the score strikes it.
        assert_eq!(to_warm(r#"s("bd*4 hh"); s("bd")"#, false), ["bd:0", "hh:0"]);
        assert!(to_warm("// nothing here", false).is_empty());
    }

    /// A load-ahead asks for the same files as a warm of the same text.
    #[test]
    fn a_load_ahead_keeps_the_indices_the_text_spells() {
        let only = |values: &[i64]| Variants::Only(values.iter().copied().collect());
        assert_eq!(
            to_load_ahead(r#"s("bd hh:2").n("<0 1>")"#, false),
            [
                ("bd".to_owned(), only(&[0, 1])),
                ("hh".to_owned(), only(&[0, 1, 2]))
            ]
        );
        let any = [
            ("bd".to_owned(), only(&[0])),
            ("hh".to_owned(), only(&[0, 2])),
        ];
        assert_eq!(to_load_ahead(r#"s("bd hh:2").n(irand(4))"#, false), any);
        assert_eq!(to_load_ahead(r#"s("bd hh:2")"#, true), any);
        assert_eq!(
            to_load_ahead(r#"s("bd:-1")"#, false),
            [("bd".to_owned(), only(&[-1]))]
        );
    }

    /// A text that can play any variant asks for the bare name, which loads
    /// the first file. A setup that picks variants makes every text such a
    /// text. A name with literal variants is one ask that lists them.
    #[test]
    fn a_warm_asks_for_the_whole_bank_only_when_any_variant_can_play() {
        assert_eq!(
            to_warm(r#"s("bd hh:2").n("<0 1>")"#, false),
            ["bd:0,1", "hh:0,1,2"]
        );
        assert_eq!(
            to_warm(r#"s("bd hh:2").n(irand(4))"#, false),
            ["bd", "hh", "hh:2"]
        );
        assert_eq!(to_warm(r#"s("bd hh:2")"#, true), ["bd", "hh", "hh:2"]);
        assert_eq!(to_warm(r#"s("bd:-1")"#, false), ["bd:-1"]);
        let counting = format!(
            "s(\"bd\").n(\"{}\")",
            (0..40).map(|n| n.to_string()).collect::<Vec<_>>().join(" ")
        );
        assert_eq!(
            to_warm(&counting, false),
            [format!(
                "bd:{}",
                (0..40).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
            )],
            "forty takes are still one ask"
        );
    }

    /// A General MIDI name asked for bare warms its first font alone, so a
    /// name widened to any still asks for the fonts its text spells: under
    /// a setup that picks variants, under an `n` the text computes, and
    /// written beside a spelling that is not a plain number. The spelled
    /// ones come after the bare name, to come off a newest-first line of
    /// bets first.
    #[test]
    fn a_name_widened_to_any_still_warms_the_variants_it_spells() {
        assert_eq!(
            to_warm(r#"s("gm_epiano1:2").note("c e g")"#, true),
            ["gm_epiano1", "gm_epiano1:2"]
        );
        assert_eq!(
            to_warm(
                "$: s(\"gm_epiano1:2\").note(\"c e g\")\n$: s(\"bd\").n(irand(4))",
                false
            ),
            ["gm_epiano1", "gm_epiano1:2", "bd"]
        );
        assert_eq!(
            to_warm(r#"s("gm_piano:3 gm_piano:<0 1>")"#, false),
            ["gm_piano", "gm_piano:3"]
        );
        assert_eq!(
            to_warm(r#"s("gm_piano:3").n(run(4)).bank("mine")"#, false),
            ["mine_gm_piano", "mine_gm_piano:3", "gm_piano", "gm_piano:3"]
        );
    }

    /// A large tab with many sounds, banks and literal `n` values makes one
    /// request per name. A request per variant would exceed what a manifest
    /// may queue, and the warm would be refused whole.
    #[test]
    fn a_big_tab_warms_one_ask_a_name_however_many_variants_it_plays() {
        let text = concat!(
            "$: s(\"bd*4, [~ sd]*2, hh*8, oh, cp, rim, lt mt ht, cr, rd, sh, cb, tb, perc\")",
            ".bank(\"<RolandTR909 RolandTR808 RolandTR707 LinnDrum>\")\n",
            "$: n(\"<0 2 4 5 7 9 11 12 14 16 17 19 21 23 24 -1 -3 -5>*8\")",
            ".s(\"gm_epiano1, gm_acoustic_bass, gm_string_ensemble_1, gm_pad_warm\")\n",
            "$: n(\"[0 3 7 10](5,8,2)\").s(\"gm_lead_2_sawtooth\")",
        );
        let kept = to_keep(text);
        let warm = to_warm(text, false);
        assert_eq!(kept.len(), 100);
        assert_eq!(warm.len(), kept.len(), "one ask a name");
        assert!(warm.iter().all(|spec| spec.contains(':')), "{warm:?}");
        assert!(warm.contains(
            &"RolandTR909_bd:-5,-3,-1,0,2,3,4,5,7,8,9,10,11,12,14,16,17,19,21,23,24".to_owned()
        ));
    }

    fn only(values: &[i64]) -> Option<Variants> {
        Some(Variants::Only(values.iter().copied().collect()))
    }

    /// A text that never sets `n` picks nothing beyond what its names
    /// spell - and a word ending in `n`, like `pan(` or `run(`, is not an
    /// `n` call.
    #[test]
    fn a_text_without_n_selects_no_variants() {
        assert_eq!(variant_selection(r#"s("bd sd").pan(sine).gain(0.8)"#), None);
        assert_eq!(variant_selection(r#"s("bd").scan(2).fn(x).mun("0")"#), None);
        assert_eq!(
            variant_selection(r#"note("c e g").add(12).s("piano")"#),
            None,
            "arithmetic moves only an n there is"
        );
    }

    /// Every `n` written as a literal - in any of the three quotes, or a
    /// plain number - picks the numbers written, operator arguments too.
    #[test]
    fn literal_n_selects_the_numbers_written() {
        assert_eq!(
            variant_selection(r#"s("bd").n("<0 1 2>")"#),
            only(&[0, 1, 2])
        );
        assert_eq!(
            variant_selection("s('bd').n('0 [3 4]*2')"),
            only(&[0, 2, 3, 4])
        );
        assert_eq!(variant_selection("s(`bd`).n(`<0\n 5>`)"), only(&[0, 5]));
        assert_eq!(variant_selection("n(3).s('bd')"), only(&[3]));
        assert_eq!(variant_selection("s('bd').n(-1)"), only(&[-1]));
        assert_eq!(
            variant_selection("s('bd').n( 0.5 )"),
            only(&[1]),
            "rounded as playback rounds it"
        );
        assert_eq!(variant_selection(r#"s("bd").n("-1 ~ 2?")"#), only(&[-1, 2]));
        assert_eq!(
            variant_selection("s('bd').n('.5 0-1')"),
            only(&[-1, 0, 1, 5]),
            "a `.5` or a `0-1` that reads two ways keeps both readings"
        );
        assert_eq!(
            variant_selection("s('bd').n('0')\n// $: s('sd').n('7')"),
            only(&[0, 7]),
            "a commented lane counts"
        );
        assert_eq!(
            variant_selection(r#"s("bd").n("0 1"#),
            only(&[0, 1]),
            "still typing"
        );
    }

    /// Whatever computes `n`, or sets it where a scan cannot read, picks
    /// any variant.
    #[test]
    fn computed_n_selects_every_variant() {
        for text in [
            "s('bd').n(irand(8))",
            "s('bd').n(run(4))",
            "s('bd').n(takes)",
            "s('bd').n(`<0 ${k}>`)",
            "s('bd').n('0 .. 3')",
            "s('bd').n('<0 a>')",
            "s('bd').n('0 1'.add(2))",
            "s('bd').n('0 1').add(2)",
            "s('bd').n('0 1').add.squeeze('<0 1>')",
            "s('bd').n('0').off(1/8, x => x.sub(1))",
            "n('0').s('bd').sometimes(x => x.set({ n: 3 }))",
            "s('bd').n('0').range(0, 3)",
            "s('bd').n(Math.round(3.2))",
            "\"0:bd 1:sd\".as(\"n:s\")",
            "s('bd').withValue(v => v)",
            "s('bd').fmap(v => v)",
            "s('bd').n()",
            "s('recordings').set({'n': 2})",
            "const n = 2; s('recordings').set({ n })",
            "const take = n => s('recordings').set({ n, gain: 1 }); take(2)",
            "s('rec').n('1').blshift(n(2))",
            "s('rec').n('1').bor(n(2))",
            "s('rec').n('3').lt(n(5))",
            "note('c3:<0 1>').s('rec')",
        ] {
            assert_eq!(variant_selection(text), Some(Variants::All), "{text}");
        }
    }

    /// `note` is the pair `note:n`, so a `:` in its string picks a variant
    /// as `n` does - called on the string or given it - while a note with
    /// no `:`, or one the scan cannot read, picks none.
    #[test]
    fn a_notes_colon_selects_the_variant_it_names() {
        let one = |values: &[i64]| Variants::Only(values.iter().copied().collect());
        assert_eq!(
            to_keep(r#"note("c3:2 e3").s("recordings")"#),
            [("recordings".to_owned(), one(&[0, 2]))]
        );
        assert_eq!(
            to_keep(r#""c3:2".note().s("gm_piano")"#),
            [("gm_piano".to_owned(), one(&[0, 2]))]
        );
        assert_eq!(
            to_keep("s('rec').note(`c3:1\n e3:4`)"),
            [("rec".to_owned(), one(&[0, 1, 4]))]
        );
        assert_eq!(variant_selection(r#"note("c3 e3").s("piano")"#), None);
        assert_eq!(variant_selection(r#"note(chord).s("piano")"#), None);
    }

    /// `scale` turns `n` into a note, so an `n` with `.scale(…)` later in
    /// its own chain picks nothing - however it is computed - while an `n`
    /// after the `scale`, or in another lane, picks as ever.
    #[test]
    fn an_n_a_scale_turns_into_a_note_selects_no_variant() {
        let one = |values: &[i64]| Variants::Only(values.iter().copied().collect());
        assert_eq!(
            to_keep(r#"n("0 2 4 <3 5>").scale("C:major").s("gm_epiano1")"#),
            [("gm_epiano1".to_owned(), one(&[0]))]
        );
        for text in [
            r#"n(irand(8)).s("gm_piano").scale("C:major")"#,
            r#"n("0 2").add("<0 3>").scale("C:major").s("gm_piano")"#,
            "s('gm_piano').n(run(8))\n  .fast(2)\n  .scale('C:minor')",
            r#"n(run(8)).add.squeeze("<0 1>").scale("C:minor").s("gm_piano")"#,
        ] {
            assert_eq!(variant_selection(text), None, "{text}");
        }
        assert_eq!(
            variant_selection(r#"n("0 2").scale("C:major").s("gm_piano").n(3)"#),
            only(&[3]),
            "an n after the scale picks"
        );
        assert_eq!(
            variant_selection(
                "$: n(\"0 2 4\").scale(\"C:major\").s(\"gm_epiano1\")\n$: note(\"c3\").s(\"gm_piano\").n(\"<0 3>\")"
            ),
            only(&[0, 3]),
            "another lane's n picks"
        );
        assert_eq!(
            variant_selection(r#"stack(n("0 1").s("bd"), note("c").scale("C:major"))"#),
            only(&[0, 1]),
            "a scale in a sibling pattern is not this n's"
        );
    }

    /// An incomplete entry (a `:` with no index yet, or an `n(` with only
    /// the next lane after it) plays what the text played before the entry,
    /// not every variant. A complete index or argument is read as usual.
    #[test]
    fn a_pause_mid_typing_does_not_select_every_variant() {
        let one = |values: &[i64]| Variants::Only(values.iter().copied().collect());
        for text in [
            "s(\"recordings:",
            r#"s("recordings:")"#,
            r#"s("[recordings:] ~")"#,
            "s(\"recordings\").n(",
            "s(\"recordings\").n(\n$: s(\"recordings\")",
            "s(\"recordings\").n(\nbass: s(\"recordings\")",
        ] {
            assert_eq!(
                to_keep(text),
                [("recordings".to_owned(), one(&[0]))],
                "{text:?}"
            );
        }
        assert_eq!(
            to_keep(r#"s("recordings: 3")"#),
            [("recordings".to_owned(), one(&[3]))]
        );
        assert_eq!(
            to_keep(r#"s("bd: <0 1>")"#),
            [("bd".to_owned(), Variants::All)]
        );
        assert_eq!(
            to_keep("s(\"<bd sd>:"),
            [("bd".to_owned(), one(&[0])), ("sd".to_owned(), one(&[0]))]
        );
        assert_eq!(
            variant_selection("s(\"bd\").n(\n  \"<0 1>\"\n)"),
            only(&[0, 1]),
            "an argument on the lines below is read"
        );
        assert_eq!(
            variant_selection("s(\"bd\").n(\n  irand(4)\n)"),
            Some(Variants::All)
        );
    }

    /// A name picks what its own spelling says, merged over every place it
    /// is written, and whatever the text's `n` picks besides.
    #[test]
    fn each_name_keeps_its_spelled_and_selected_variants() {
        let kept = |text: &str| to_keep(text);
        let one = |values: &[i64]| Variants::Only(values.iter().copied().collect());
        assert_eq!(
            kept(r#"s("bd bd:3 hh:-1 cp:0.5 oh:2:0.4")"#),
            [
                ("bd".to_owned(), one(&[0, 3])),
                ("hh".to_owned(), one(&[-1])),
                ("cp".to_owned(), one(&[1])),
                ("oh".to_owned(), one(&[2])),
            ]
        );
        assert_eq!(
            kept(r#"s("bd:<0 1> sd:[0 2] hh")"#),
            [
                ("bd".to_owned(), Variants::All),
                ("sd".to_owned(), Variants::All),
                ("hh".to_owned(), one(&[0])),
            ]
        );
        assert_eq!(
            kept(r#"s("<bd sd>:3")"#),
            [
                ("bd".to_owned(), Variants::All),
                ("sd".to_owned(), Variants::All)
            ],
            "a colon with no name before it indexes names a scan cannot pair it with"
        );
        assert_eq!(
            kept(r#"s("recordings:3").n("<0 1>").bank("mine")"#),
            [
                ("mine_recordings".to_owned(), one(&[0, 1, 3])),
                ("recordings".to_owned(), one(&[0, 1, 3])),
            ]
        );
        assert_eq!(
            kept(r#"s("bd:2").n(irand(4))"#),
            [("bd".to_owned(), Variants::All)]
        );
        assert_eq!(
            kept(r#"s("bd").add(note(3))"#),
            [("bd".to_owned(), one(&[0]))],
            "with no n, arithmetic moves nothing a bank's files hang on"
        );
        assert_eq!(
            kept(r#"s("bd").n("0").add(1)"#),
            [("bd".to_owned(), Variants::All)]
        );
        assert_eq!(
            kept(r#"s("bd").set({n: 2})"#),
            [("bd".to_owned(), Variants::All)]
        );
    }

    /// The names are the ones `in_score` reads, only with the index taken
    /// off and read further.
    #[test]
    fn the_names_kept_are_the_names_read() {
        let text = "s(\"[bd*4 hh:3?] <sd cp:2>!2 ~ 808bd:1 a.b x-y:1 3:2\")\n// s(`gm_pad:0`)";
        let read: Vec<String> = in_score(text)
            .into_iter()
            .map(|token| token.split(':').next().expect("a name").to_owned())
            .fold(Vec::new(), |mut names, name| {
                if !names.contains(&name) {
                    names.push(name);
                }
                names
            });
        let kept: Vec<String> = variants_in_score(text)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(kept, read);
    }

    /// A bank pattern names each machine once, in the order written, each
    /// read the way a sound name is; a rest names none.
    #[test]
    fn a_bank_pattern_names_each_machine_once() {
        assert_eq!(
            bank_pattern_machines("<tr808 tr909> tr808"),
            ["tr808", "tr909"]
        );
        assert_eq!(
            bank_pattern_machines("wt_digital,Metal-Tin é2 ~ - [^linn]"),
            ["wt_digital", "Metal-Tin", "é2", "linn"]
        );
        assert!(bank_pattern_machines(" <> ").is_empty());
        // Banks on two calls stay two machines.
        assert_eq!(
            banks_in_score("s(\"bd\").bank(\"a\").bank('b')"),
            ["a", "b"]
        );
    }

    /// A bank is read the way a sound is: past a space, in backticks.
    #[test]
    fn a_bank_in_backticks_or_past_a_space_is_read() {
        assert_eq!(
            banks_in_score("s(`bd`).bank( `<tr909\n tr808>`)\n.bank( 'linn')"),
            ["tr909", "tr808", "linn"]
        );
    }
}
