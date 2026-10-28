//! Score metadata: the `@key value` fields of a score's leading comments,
//! the way Strudel songs carry their title and author. The fields are read
//! off the file when the panel lists it, never stored beside it: the file is
//! the one owner of what it says about itself.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// How much of a file the scan reads. Leading comments run to a few hundred
/// bytes; anything past this is code.
const HEAD_BYTES: u64 = 8 * 1024;

/// The `@key value` fields of one score, in file order. Repeated keys stay
/// repeated: a song may list several `@tag` lines.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScoreMeta {
    fields: Vec<(String, String)>,
}

impl ScoreMeta {
    /// The fields of a score's text.
    pub fn parse(text: &str) -> Self {
        let mut fields = Vec::new();
        for line in leading_comment_text(text).lines() {
            let line = comment_line(line);
            let Some(rest) = line.strip_prefix('@') else {
                continue;
            };
            let (key, value) = match rest.find(char::is_whitespace) {
                Some(at) => (&rest[..at], rest[at..].trim()),
                None => (rest, ""),
            };
            if !key.is_empty() {
                fields.push((key.to_string(), value.to_string()));
            }
        }
        Self { fields }
    }

    /// The fields of a score on disk, read off its head. A file that cannot
    /// be read has no fields.
    pub fn read(path: &Path) -> Self {
        let mut bytes = Vec::new();
        let read = File::open(path)
            .and_then(|mut file| (&mut file).take(HEAD_BYTES).read_to_end(&mut bytes));
        match read {
            Ok(_) => Self::parse(&String::from_utf8_lossy(&bytes)),
            Err(_) => Self::default(),
        }
    }

    /// Every field, key and value, in file order.
    pub fn fields(&self) -> &[(String, String)] {
        &self.fields
    }

    /// The value of the first field with this key, lowercased or as written.
    pub fn first(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field == key)
            .map(|(_, value)| value.as_str())
    }
}

/// The leading run of the text that is comments: blank lines may sit between
/// them, and the first line that is neither ends the run. When the text ends
/// inside a comment - a last line with no newline, or a block the head cut
/// short - the whole text is the run.
fn leading_comment_text(text: &str) -> &str {
    let mut end = 0;
    loop {
        let rest = text[end..].trim_start();
        let start = text.len() - rest.len();
        if let Some(stripped) = rest.strip_prefix("//") {
            match stripped.find('\n') {
                Some(nl) => end = start + nl + 3,
                None => return text,
            }
        } else if let Some(stripped) = rest.strip_prefix("/*") {
            match stripped.find("*/") {
                Some(close) => end = start + close + 4,
                None => return text,
            }
        } else {
            return &text[..end];
        }
    }
}

/// One line of a comment with its markers taken off, ready for a field.
fn comment_line(line: &str) -> &str {
    let line = line.trim();
    let line = line.strip_prefix("/*").unwrap_or(line);
    let line = line.strip_prefix("//").unwrap_or(line);
    let line = line.strip_prefix('*').unwrap_or(line);
    line.strip_suffix("*/").unwrap_or(line).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_header_reads_its_fields_in_order() {
        let score = "\
/*
  @title distortedguitar
  @by froos
  @origin KabelSalat examples
  @duration 8
  SPDX-License-Identifier: AGPL-3.0-or-later
*/
saw(55).out();
";
        let meta = ScoreMeta::parse(score);
        assert_eq!(
            meta.fields(),
            &[
                ("title".to_string(), "distortedguitar".to_string()),
                ("by".to_string(), "froos".to_string()),
                ("origin".to_string(), "KabelSalat examples".to_string()),
                ("duration".to_string(), "8".to_string()),
            ]
        );
        assert_eq!(meta.first("title"), Some("distortedguitar"));
        assert_eq!(meta.first("by"), Some("froos"));
        assert_eq!(meta.first("license"), None);
    }

    #[test]
    fn line_comments_and_blanks_between_them_stay_in_the_run() {
        let score = "\
// @title night drive

// @tag drums, bass
// @by someone
$: s(\"bd sd\")
";
        let meta = ScoreMeta::parse(score);
        assert_eq!(meta.first("title"), Some("night drive"));
        assert_eq!(meta.first("tag"), Some("drums, bass"));
        assert_eq!(meta.first("by"), Some("someone"));
    }

    #[test]
    fn code_first_leaves_no_fields() {
        let meta = ScoreMeta::parse("$: s(\"bd\") // @tag hidden\n");
        assert!(meta.fields().is_empty());
    }

    #[test]
    fn comments_after_code_are_not_read() {
        let score = "\
// @title first
$: s(\"bd\")
// @title second
";
        let meta = ScoreMeta::parse(score);
        assert_eq!(meta.first("title"), Some("first"));
    }

    #[test]
    fn one_line_block_and_starred_lines_work() {
        let score = "\
/**
 * @title starred
 */
$: s(\"bd\")
";
        let meta = ScoreMeta::parse(score);
        assert_eq!(meta.first("title"), Some("starred"));
        let one_line = ScoreMeta::parse("/* @title single */\n$: s(\"bd\")\n");
        assert_eq!(one_line.first("title"), Some("single"));
    }

    #[test]
    fn a_head_cut_mid_block_keeps_the_fields_before_the_cut() {
        let score = "/*\n@title cut\n@by mid-way thro"; // no close, cut short
        let meta = ScoreMeta::parse(score);
        assert_eq!(meta.first("title"), Some("cut"));
        assert_eq!(meta.first("by"), Some("mid-way thro"));
    }

    #[test]
    fn a_run_that_reaches_the_end_keeps_its_earlier_fields() {
        // The last line comment has no newline after it.
        let meta = ScoreMeta::parse("// @title night drive\n// @by me");
        assert_eq!(meta.first("title"), Some("night drive"));
        assert_eq!(meta.first("by"), Some("me"));
        // The head is cut inside a block that follows a line comment.
        let meta = ScoreMeta::parse("// @title cut\n/*\n@by mid-way thro");
        assert_eq!(meta.first("title"), Some("cut"));
        assert_eq!(meta.first("by"), Some("mid-way thro"));
    }

    #[test]
    fn bare_markers_and_empty_keys_give_no_fields() {
        let meta = ScoreMeta::parse("//\n// @\n// @  spaced\n/* */\n$: s(\"bd\")\n");
        assert!(meta.fields().is_empty());
    }

    #[test]
    fn repeated_keys_stay_repeated() {
        let meta = ScoreMeta::parse("// @tag drums\n// @tag bass\n$: s(\"bd\")\n");
        assert_eq!(
            meta.fields(),
            &[
                ("tag".to_string(), "drums".to_string()),
                ("tag".to_string(), "bass".to_string()),
            ]
        );
    }

    #[test]
    fn read_takes_the_fields_off_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("song.strudel");
        std::fs::write(
            &path,
            "/*\n@title on disk\n@by the author\n*/\n$: s(\"bd\")\n",
        )
        .unwrap();
        let meta = ScoreMeta::read(&path);
        assert_eq!(meta.first("title"), Some("on disk"));
        assert_eq!(meta.first("by"), Some("the author"));
    }

    #[test]
    fn a_missing_file_has_no_fields() {
        let meta = ScoreMeta::read(Path::new("/no/such/score.strudel"));
        assert!(meta.fields().is_empty());
    }
}
