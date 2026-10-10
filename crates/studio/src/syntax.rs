//! One-pass lexical colouring shared by the source pane and the minimap.
//!
//! This is a colouring heuristic, not a parser: it is driven per grapheme in
//! the draw loop and has to stay proportional to what is on screen. Classes
//! are resolved into colours by the theme, so the same scan serves any
//! palette.

use ratatui::style::Color;

use super::theme::Theme;

/// What a character is being drawn as.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Token {
    #[default]
    Text,
    Comment,
    String,
    Number,
    Punctuation,
    /// A word the language owns: `const`, `let`, `return`, `await`.
    Keyword,
    /// A word being called: `s`, `note`, `fast` in `.fast(2)`.
    Function,
}

impl Token {
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Text => theme.syntax.text,
            Self::Comment => theme.syntax.comment,
            Self::String => theme.syntax.string,
            Self::Number => theme.syntax.number,
            Self::Punctuation => theme.syntax.punctuation,
            // Older palettes may not name these separately. Calls need a
            // distinct colour because a score is mostly function calls.
            Self::Keyword => theme.syntax.keyword.unwrap_or(theme.syntax.punctuation),
            Self::Function => theme.syntax.function.unwrap_or(theme.accent),
        }
    }
}

/// JavaScript's own words. Short list on purpose: a score is mostly calls,
/// and colouring `if` like `sometimesBy` helps nobody.
const KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];

/// Classify a whole line, one token per cluster.
///
/// The per-cluster [`Lexer`] cannot see the end of a word, so it calls every
/// letter `Text`. A drawing pass has the line in hand anyway, so words are
/// resolved here: a run of word characters becomes a [`Token::Keyword`] when
/// the language owns it, and a [`Token::Function`] when a `(` follows -
/// which is what `s(`, `.fast(` and `stack(` all are.
pub fn classify<'a>(clusters: impl IntoIterator<Item = &'a str>) -> Vec<Token> {
    classify_from(Lexer::default(), clusters)
}

/// The same pass, started from a lexer that has already read something.
///
/// The two halves of the colouring have to compose. A screen row is not a
/// line: one that begins mid-line begins mid-whatever-its-line-was-saying,
/// so it needs [`Lexer::primed`] to know it is inside a string. But whether
/// `stack` is a call depends on a `(` that a per-cluster lexer has not
/// reached yet, so words still have to be resolved over the whole run. The
/// caller primes, this resolves.
pub fn classify_from<'a>(lexer: Lexer, clusters: impl IntoIterator<Item = &'a str>) -> Vec<Token> {
    let clusters: Vec<&str> = clusters.into_iter().collect();
    let mut lexer = lexer;
    let mut tokens: Vec<Token> = (0..clusters.len())
        .map(|index| lexer.next(clusters[index], clusters.get(index + 1).copied()))
        .collect();

    let is_word = |cluster: &str| {
        cluster
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
    };
    let mut start = 0usize;
    while start < clusters.len() {
        if tokens[start] != Token::Text || !is_word(clusters[start]) {
            start += 1;
            continue;
        }
        // A digit after the first letter is part of the name: `filter1on`.
        let mut end = start;
        while end < clusters.len()
            && matches!(tokens[end], Token::Text | Token::Number)
            && is_word(clusters[end])
        {
            end += 1;
        }
        let word: String = clusters[start..end].concat();
        let called = clusters[end..]
            .iter()
            .find(|cluster| !cluster.chars().all(char::is_whitespace))
            .is_some_and(|cluster| cluster.starts_with('('));
        let class = if KEYWORDS.contains(&word.as_str()) {
            Token::Keyword
        } else if called {
            Token::Function
        } else {
            Token::Text
        };
        tokens[start..end].fill(class);
        start = end;
    }
    tokens
}

/// Line-local lexer state. A string or a comment ends with its line, which
/// is what the languages this colours do.
///
/// A line is not a screen ROW. One line becomes several rows when it is
/// wrapped or scrolled sideways, and a row that begins in the middle of a
/// line begins in the middle of whatever that line was saying: read from
/// there, the closing quote of `s("bd")` looks like an opening one and
/// everything after it is a string that never ends. [`Lexer::primed`] is
/// how a row starts in the state its line had actually reached.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lexer {
    quote: Option<char>,
    slash: bool,
    comment: bool,
    block_comment: bool,
    block_star: bool,
}

impl Lexer {
    /// The state a line is in `prefix` characters along.
    ///
    /// What a row scrolled or wrapped past the start of its line needs
    /// before it colours its first cell: the part of the line in front of
    /// it, read but not drawn.
    pub fn primed(prefix: &str) -> Self {
        let mut lexer = Self::default();
        let mut characters = prefix.chars().peekable();
        while let Some(character) = characters.next() {
            lexer.advance(character, characters.peek().copied());
        }
        lexer
    }

    /// Classify the next grapheme cluster and advance.
    ///
    /// `lookahead` is the cluster that follows, when there is one. It exists
    /// so that the first slash of a `//` is coloured as part of the comment
    /// rather than as ordinary text - without it the opening marker of every
    /// commented line is the wrong colour.
    pub fn next(&mut self, cluster: &str, lookahead: Option<&str>) -> Token {
        self.advance(
            cluster.chars().next().unwrap_or(' '),
            lookahead.and_then(|next| next.chars().next()),
        )
    }

    /// The same step, for a caller that already has characters and would
    /// otherwise spell each one back into a string to ask.
    pub fn advance(&mut self, character: char, lookahead: Option<char>) -> Token {
        if self.comment {
            return Token::Comment;
        }
        if self.block_comment {
            let token = Token::Comment;
            if self.block_star && character == '/' {
                self.block_comment = false;
            }
            self.block_star = character == '*';
            return token;
        }
        if let Some(quote) = self.quote {
            if character == quote && !self.slash {
                self.quote = None;
            }
            self.slash = character == '\\' && !self.slash;
            return Token::String;
        }
        if self.slash && character == '/' {
            self.comment = true;
            return Token::Comment;
        }
        if character == '/' && lookahead == Some('*') {
            self.block_comment = true;
            self.block_star = false;
            return Token::Comment;
        }
        self.slash = character == '/';
        if self.slash && lookahead == Some('/') {
            return Token::Comment;
        }
        if matches!(character, '\'' | '"' | '`') {
            self.quote = Some(character);
            return Token::String;
        }
        if character.is_ascii_digit() {
            Token::Number
        } else if matches!(
            character,
            '$' | '.' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '*' | '!' | '@' | '/'
        ) {
            Token::Punctuation
        } else {
            Token::Text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(line: &str) -> Vec<Token> {
        let mut lexer = Lexer::default();
        let clusters = line.chars().map(|c| c.to_string()).collect::<Vec<_>>();
        (0..clusters.len())
            .map(|index| {
                lexer.next(
                    &clusters[index],
                    clusters.get(index + 1).map(String::as_str),
                )
            })
            .collect()
    }

    /// The same text is the same colour wherever the row drawing it begins.
    ///
    /// A row scrolled sideways, and the second row of a wrapped line, both
    /// start in the middle of a line. A lexer started fresh there reads the
    /// closing quote of `s("sbd")` as an opening quote and paints the rest
    /// of the line as one string. A lexer primed with the part of the line
    /// before the row gives the same colours as the whole line.
    #[test]
    fn a_row_that_begins_mid_line_is_coloured_as_that_line() {
        for line in [
            r#"$: s("sbd").seg(4).lpf(slider(100, 100, 1, 1000))"#,
            r#"// $: s("bd") - a comment holding a quote"#,
            r#"s("b\"d").gain(1)"#,
            "$: note(\"c a f e\").sound('piano')",
        ] {
            let whole = classify(line);
            for from in 0..line.chars().count() {
                let prefix: String = line.chars().take(from).collect();
                let rest: String = line.chars().skip(from).collect();
                let mut lexer = Lexer::primed(&prefix);
                let clusters = rest.chars().map(|c| c.to_string()).collect::<Vec<_>>();
                let drawn = (0..clusters.len())
                    .map(|index| {
                        lexer.next(
                            &clusters[index],
                            clusters.get(index + 1).map(String::as_str),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    &whole[from..],
                    &drawn[..],
                    "{line:?} drawn from character {from} ({rest:?})"
                );
            }
        }
    }

    /// Nothing in front of it is the state it always had.
    #[test]
    fn a_row_that_begins_at_the_start_of_its_line_is_primed_with_nothing() {
        let line = r#"$: s("sbd").lpf(500)"#;
        let mut primed = Lexer::primed("");
        let mut fresh = Lexer::default();
        for character in line.chars() {
            assert_eq!(
                primed.advance(character, None),
                fresh.advance(character, None)
            );
        }
    }

    #[test]
    fn a_line_comment_runs_to_the_end_of_the_line() {
        let tokens = classify("a // \"not a string\"");
        assert_eq!(tokens[0], Token::Text);
        assert!(tokens[2..].iter().all(|token| *token == Token::Comment));
    }

    #[test]
    fn strings_survive_escaped_quotes_and_close_on_the_matching_one() {
        let tokens = classify(r#"s("b\"d") x"#);
        // The escaped quote stays inside the string; the final one closes it.
        assert_eq!(tokens[2], Token::String);
        assert_eq!(tokens[7], Token::String);
        assert_eq!(tokens[8], Token::Punctuation, "the closing paren");
        assert_eq!(tokens[10], Token::Text);
    }

    #[test]
    fn mini_notation_operators_read_as_punctuation_and_digits_as_numbers() {
        let tokens = classify("$: s(bd*2)");
        assert_eq!(tokens[0], Token::Punctuation);
        assert_eq!(tokens[8], Token::Number);
        assert_eq!(tokens[7], Token::Punctuation, "the repeat operator");
    }

    #[test]
    fn a_lone_slash_is_not_a_comment() {
        let tokens = classify("1/2");
        assert!(tokens.iter().all(|token| *token != Token::Comment));
        assert_eq!(tokens[1], Token::Punctuation);
    }

    #[test]
    fn both_slashes_of_a_comment_marker_are_coloured_as_comment() {
        let tokens = classify("x // y");
        assert_eq!(tokens[2], Token::Comment, "the opening slash");
        assert_eq!(tokens[3], Token::Comment);
    }

    #[test]
    fn block_comments_are_coloured_until_their_closing_marker() {
        let tokens = classify("x /* muted */ y");
        assert!(tokens[2..13].iter().all(|token| *token == Token::Comment));
        assert_eq!(tokens[14], Token::Text, "text after the closing marker");
    }

    #[test]
    fn a_slash_inside_a_string_stays_a_string() {
        let tokens = classify(r#"s("a//b")"#);
        assert_eq!(tokens[4], Token::String);
        assert_eq!(tokens[5], Token::String);
    }

    /// A word answers to what follows it: a `(` makes it a call, and the
    /// language's own words are neither calls nor ordinary text.
    #[test]
    fn a_word_is_read_as_a_call_or_a_keyword() {
        let line = "const x = stack(s(\"bd\").fast(2), osc2(), { filter1on: 1.5 })";
        let clusters: Vec<String> = line.chars().map(|c| c.to_string()).collect();
        let tokens = super::classify(clusters.iter().map(String::as_str));
        let at = |needle: &str| tokens[line.find(needle).expect("in the line")];

        assert_eq!(at("const"), Token::Keyword);
        assert_eq!(at("stack"), Token::Function);
        assert_eq!(at("fast"), Token::Function);
        // A name that is not called stays ordinary text.
        assert_eq!(at("x ="), Token::Text);
        // A digit in a name has the class of the name, and a number keeps
        // its own.
        assert_eq!(at("2()"), Token::Function);
        assert_eq!(at("1on"), Token::Text);
        assert_eq!(at("1.5"), Token::Number);
        assert_eq!(at("5 }"), Token::Number);
        // And a word inside a string is still a string.
        assert_eq!(at("bd"), Token::String);
    }

    /// A call written with a space before its bracket is still a call, and a
    /// theme that names no colour for one still draws it.
    #[test]
    fn spacing_does_not_hide_a_call() {
        let line = "note (60)";
        let clusters: Vec<String> = line.chars().map(|c| c.to_string()).collect();
        let tokens = super::classify(clusters.iter().map(String::as_str));
        assert_eq!(tokens[0], Token::Function);
    }
}
