//! Minimal stdio Language Server Protocol (LSP) server for external editors.
//!
//! Serves completion, hover, and static lint diagnostics directly from the
//! engine's own [`rustel_studio::reference::Reference`] catalogue and
//! [`rustel_runtime::lint`] checker without starting audio or JavaScript
//! evaluation.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, Write};
use std::ops::Range;

use rustel_runtime::RuntimeError;
use rustel_runtime::lint::{self, Level, scan};
use rustel_studio::reference::{Entry, Reference};
use serde_json::{Value, json};

/// Maximum number of open documents kept in memory at once.
const MAX_OPEN_DOCUMENTS: usize = 64;
/// Maximum byte length accepted for one open document.
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Maximum byte length accepted for one JSON-RPC message body.
const MAX_RPC_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Maximum number of HTTP-style headers accepted on one LSP frame.
const MAX_RPC_HEADERS: usize = 64;
/// Maximum byte length accepted for one header line.
const MAX_RPC_HEADER_LINE_BYTES: usize = 8 * 1024;

/// Maximum nesting depth tracked when scanning for an enclosing function call.
const MAX_CALL_STACK_DEPTH: usize = 64;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_SERVER_NOT_INITIALIZED: i64 = -32002;

/// Explicit LSP lifecycle state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Uninitialized,
    Running,
    ShuttingDown,
}

/// Scanner state while classifying the cursor position inside a document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanState {
    Code,
    LineComment,
    BlockComment,
    String { quote: u8, start: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StringArgKind {
    Sound,
    Scale,
    Chord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CursorContext<'a> {
    Comment,
    OtherString {
        prefix: &'a str,
    },
    StringArg {
        kind: StringArgKind,
        prefix: &'a str,
    },
    Member {
        prefix: &'a str,
    },
    TopLevel {
        prefix: &'a str,
    },
}

enum LoopAction {
    Continue,
    Exit { clean: bool },
}

/// Run the LSP server on process stdin and stdout.
pub(crate) fn run_stdio() -> Result<(), RuntimeError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(stdin.lock(), stdout.lock())
}

/// Serve LSP JSON-RPC messages over arbitrary buffered streams.
pub(crate) fn serve<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
) -> Result<(), RuntimeError> {
    let mut server = LspServer::new();
    while let Some(message) = read_message(&mut reader)? {
        match server.handle_message(&message, &mut writer)? {
            LoopAction::Continue => {}
            LoopAction::Exit { clean: true } => return Ok(()),
            LoopAction::Exit { clean: false } => {
                return Err(RuntimeError::Message(
                    "lsp exit received before shutdown".into(),
                ));
            }
        }
    }
    Ok(())
}

struct LspServer {
    lifecycle: Lifecycle,
    snippet_support: bool,
    reference: Reference,
    documents: HashMap<String, String>,
}

impl LspServer {
    fn new() -> Self {
        Self {
            lifecycle: Lifecycle::Uninitialized,
            snippet_support: false,
            reference: Reference::load_all(),
            documents: HashMap::new(),
        }
    }

    fn handle_message<W: Write>(
        &mut self,
        message: &Value,
        writer: &mut W,
    ) -> Result<LoopAction, RuntimeError> {
        let id = message.get("id");
        let method = message.get("method").and_then(Value::as_str);

        if method == Some("exit") {
            return Ok(LoopAction::Exit {
                clean: self.lifecycle == Lifecycle::ShuttingDown,
            });
        }

        match self.lifecycle {
            Lifecycle::Uninitialized => {
                if method == Some("initialize") {
                    self.lifecycle = Lifecycle::Running;
                    self.snippet_support = client_supports_snippets(message.get("params"));
                    if let Some(id) = id {
                        write_result(writer, id, initialize_result())?;
                    }
                } else if let Some(id) = id {
                    write_error(
                        writer,
                        id,
                        JSONRPC_SERVER_NOT_INITIALIZED,
                        "server is not initialized",
                    )?;
                }
                return Ok(LoopAction::Continue);
            }
            Lifecycle::ShuttingDown => {
                if let Some(id) = id {
                    write_error(
                        writer,
                        id,
                        JSONRPC_INVALID_REQUEST,
                        "server is shutting down",
                    )?;
                }
                return Ok(LoopAction::Continue);
            }
            Lifecycle::Running => {}
        }

        let Some(method) = method else {
            return Ok(LoopAction::Continue);
        };
        let params = message.get("params").unwrap_or(&Value::Null);

        match method {
            "initialize" => {
                if let Some(id) = id {
                    write_error(
                        writer,
                        id,
                        JSONRPC_INVALID_REQUEST,
                        "server is already initialized",
                    )?;
                }
            }
            "initialized" | "$/cancelRequest" | "$/setTrace" => {}
            "shutdown" => {
                self.lifecycle = Lifecycle::ShuttingDown;
                if let Some(id) = id {
                    write_result(writer, id, Value::Null)?;
                }
            }
            "textDocument/didOpen" => {
                if let Some((uri, text)) = parse_did_open(params)
                    && text.len() <= MAX_DOCUMENT_BYTES
                    && (self.documents.contains_key(uri)
                        || self.documents.len() < MAX_OPEN_DOCUMENTS)
                {
                    let diagnostics = diagnostics_for(text);
                    self.documents.insert(uri.to_owned(), text.to_owned());
                    publish_diagnostics(writer, uri, diagnostics)?;
                }
            }
            "textDocument/didChange" => {
                if let Some((uri, text)) = parse_did_change(params)
                    && text.len() <= MAX_DOCUMENT_BYTES
                    && (self.documents.contains_key(uri)
                        || self.documents.len() < MAX_OPEN_DOCUMENTS)
                {
                    let diagnostics = diagnostics_for(text);
                    self.documents.insert(uri.to_owned(), text.to_owned());
                    publish_diagnostics(writer, uri, diagnostics)?;
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = params
                    .get("textDocument")
                    .and_then(|doc| doc.get("uri"))
                    .and_then(Value::as_str)
                {
                    self.documents.remove(uri);
                    publish_diagnostics(writer, uri, Vec::new())?;
                }
            }
            "textDocument/completion" => {
                if let Some(id) = id {
                    let result = self.handle_completion(params);
                    write_result(writer, id, result)?;
                }
            }
            "textDocument/hover" => {
                if let Some(id) = id {
                    let result = self.handle_hover(params);
                    write_result(writer, id, result)?;
                }
            }
            "textDocument/signatureHelp" => {
                if let Some(id) = id {
                    let result = self.handle_signature_help(params);
                    write_result(writer, id, result)?;
                }
            }
            _ => {
                if let Some(id) = id {
                    write_error(
                        writer,
                        id,
                        JSONRPC_METHOD_NOT_FOUND,
                        &format!("unsupported method: {method}"),
                    )?;
                }
            }
        }

        Ok(LoopAction::Continue)
    }

    fn handle_completion(&self, params: &Value) -> Value {
        let Some((uri, line, character)) = parse_text_document_position(params) else {
            return empty_completion_list();
        };
        let Some(doc) = self.documents.get(uri) else {
            return empty_completion_list();
        };
        let offset = utf16_position_to_offset(doc, line, character);
        let context = cursor_context(doc, offset);
        let items = self.completion_items(doc, offset, context);
        json!({
            "isIncomplete": false,
            "items": items,
        })
    }

    fn handle_signature_help(&self, params: &Value) -> Value {
        let Some((uri, line, character)) = parse_text_document_position(params) else {
            return Value::Null;
        };
        let Some(doc) = self.documents.get(uri) else {
            return Value::Null;
        };
        let offset = utf16_position_to_offset(doc, line, character);
        let Some((callee, active_param)) = enclosing_call_at(doc, offset) else {
            return Value::Null;
        };
        let Some(entry) = super::resolve_doc_entry(&self.reference, callee) else {
            return Value::Null;
        };
        if !entry.inserts_as_call() {
            return Value::Null;
        }
        format_signature_help(&entry, active_param)
    }

    fn handle_hover(&self, params: &Value) -> Value {
        let Some((uri, line, character)) = parse_text_document_position(params) else {
            return Value::Null;
        };
        let Some(doc) = self.documents.get(uri) else {
            return Value::Null;
        };
        let offset = utf16_position_to_offset(doc, line, character);
        let Some(word_range) = word_range_at(doc, offset) else {
            return Value::Null;
        };
        let word = &doc[word_range.start..word_range.end];
        let Some(entry) = super::resolve_doc_entry(&self.reference, word) else {
            return Value::Null;
        };
        let (start_line, start_char) = offset_to_utf16_position(doc, word_range.start);
        let (end_line, end_char) = offset_to_utf16_position(doc, word_range.end);
        json!({
            "contents": {
                "kind": "markdown",
                "value": format_entry_markdown(&entry),
            },
            "range": {
                "start": { "line": start_line, "character": start_char },
                "end": { "line": end_line, "character": end_char },
            }
        })
    }

    fn completion_items(&self, doc: &str, offset: usize, context: CursorContext<'_>) -> Vec<Value> {
        if let CursorContext::OtherString { prefix } = context {
            return self.parameter_choice_items(doc, offset, prefix);
        }

        let (prefix, member_only, string_tag) = match context {
            CursorContext::Comment | CursorContext::OtherString { .. } => return Vec::new(),
            CursorContext::StringArg { kind, prefix } => {
                let tag = match kind {
                    StringArgKind::Sound => "sound",
                    StringArgKind::Scale => "scale",
                    StringArgKind::Chord => "chord",
                };
                (prefix, false, Some(tag))
            }
            CursorContext::Member { prefix } => (prefix, true, None),
            CursorContext::TopLevel { prefix } => (prefix, false, None),
        };

        let prefix_lower = prefix.to_ascii_lowercase();
        let wants_underscore = prefix.starts_with('_');
        let include_hydra = doc.contains("initHydra");
        let include_kabel = doc_uses_kabel(doc);
        let insert_call_snippet = self.snippet_support && !has_following_paren(doc, offset);
        let chord_root = if string_tag == Some("chord") {
            split_chord_root(prefix)
        } else {
            None
        };
        let chord_quality_lower = chord_root.map(|(_, quality)| quality.to_ascii_lowercase());
        let mut seen = HashSet::<&str>::new();
        let mut items = Vec::new();

        for (index, entry) in self.reference.entries().iter().enumerate() {
            if let Some(tag) = string_tag {
                if !entry.tags.iter().any(|candidate| candidate == tag) {
                    continue;
                }
                if !entry.name.is_empty() && seen.insert(entry.name.as_str()) {
                    if let (Some((root, _)), Some(quality_lower)) =
                        (chord_root, chord_quality_lower.as_deref())
                    {
                        let full = format!("{root}{}", entry.name);
                        push_completion_item_matching(
                            &mut items,
                            &full,
                            entry.name.as_str(),
                            entry,
                            quality_lower,
                            12, // LSP CompletionItemKind::Value
                            false,
                        );
                    } else {
                        push_completion_item(
                            &mut items,
                            entry.name.as_str(),
                            entry,
                            &prefix_lower,
                            12, // LSP CompletionItemKind::Value
                            false,
                        );
                    }
                }
                continue;
            }

            if entry.no_autocomplete {
                continue;
            }
            if entry.is_hydra() && !include_hydra {
                continue;
            }
            if self.reference.is_kabel_entry(index) && !include_kabel {
                continue;
            }
            if entry
                .tags
                .iter()
                .any(|tag| matches!(tag.as_str(), "sound" | "scale" | "chord"))
            {
                continue;
            }
            if member_only
                && (entry.snippet.is_some() || entry.tags.iter().any(|tag| tag == "value"))
            {
                continue;
            }

            let kind = if entry.snippet.is_some() {
                15 // Snippet
            } else if member_only {
                if entry.inserts_as_call() { 2 } else { 10 } // Method / Property
            } else if entry.inserts_as_call() {
                3 // Function
            } else {
                6 // Variable
            };

            for label in std::iter::once(entry.name.as_str())
                .chain(entry.synonyms.iter().map(String::as_str))
            {
                if label.is_empty() || (label.starts_with('_') && !wants_underscore) {
                    continue;
                }
                if seen.insert(label) {
                    push_completion_item(
                        &mut items,
                        label,
                        entry,
                        &prefix_lower,
                        kind,
                        insert_call_snippet,
                    );
                }
            }
        }

        items
    }

    fn parameter_choice_items(&self, doc: &str, offset: usize, prefix: &str) -> Vec<Value> {
        let Some((callee, active_param)) = enclosing_call_at(doc, offset) else {
            return Vec::new();
        };
        let Some(entry) = super::resolve_doc_entry(&self.reference, callee) else {
            return Vec::new();
        };
        let Some(param) = entry.params.get(active_param) else {
            return Vec::new();
        };
        if param.choices.is_empty() {
            return Vec::new();
        }
        let prefix_lower = prefix.to_ascii_lowercase();
        let mut items = Vec::new();
        for choice in &param.choices {
            let value_lower = choice.value.to_ascii_lowercase();
            let rank = if prefix_lower.is_empty() || value_lower.starts_with(&prefix_lower) {
                0u8
            } else if value_lower.contains(&prefix_lower) {
                1u8
            } else {
                continue;
            };
            let mut item = json!({
                "label": choice.value,
                "kind": 12, // LSP CompletionItemKind::Value
                "detail": format!("{} ({})", param.name, entry.name),
                "sortText": format!("{rank}_{value_lower}"),
            });
            if !choice.description.is_empty() {
                item["documentation"] = json!({
                    "kind": "markdown",
                    "value": choice.description,
                });
            }
            items.push(item);
        }
        items
    }
}

/// Split a leading chord root note (`A`..`G` plus optional `#` or `b`) from a
/// chord completion prefix so quality symbols in `CHORD_REFERENCE` match after
/// typing a root such as `C`, `Cm`, `F#7`, or `Bb^`.
fn split_chord_root(prefix: &str) -> Option<(&str, &str)> {
    let bytes = prefix.as_bytes();
    let &first = bytes.first()?;
    if !matches!(first, b'A'..=b'G') {
        return None;
    }
    let root_len = if bytes.get(1).is_some_and(|b| matches!(b, b'#' | b'b')) {
        2
    } else {
        1
    };
    Some(prefix.split_at(root_len))
}

/// Whether `doc` contains a KabelSalat graph (`K(...)` or `worklet(...)`).
fn doc_uses_kabel(doc: &str) -> bool {
    fn has_call(doc: &str, name: &str) -> bool {
        let bytes = doc.as_bytes();
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$');
        for (at, _) in doc.match_indices(name) {
            if at > 0 && is_ident(bytes[at - 1]) {
                continue;
            }
            let after = doc[at + name.len()..].trim_start();
            if after.starts_with('(') {
                return true;
            }
        }
        false
    }
    has_call(doc, "K") || has_call(doc, "worklet") || doc.contains("kabel")
}

fn push_completion_item(
    items: &mut Vec<Value>,
    label: &str,
    entry: &Entry,
    prefix_lower: &str,
    kind: u32,
    insert_call_snippet: bool,
) {
    push_completion_item_matching(
        items,
        label,
        label,
        entry,
        prefix_lower,
        kind,
        insert_call_snippet,
    );
}

fn push_completion_item_matching(
    items: &mut Vec<Value>,
    label: &str,
    match_target: &str,
    entry: &Entry,
    prefix_lower: &str,
    kind: u32,
    insert_call_snippet: bool,
) {
    let target_lower = match_target.to_ascii_lowercase();
    let rank = if prefix_lower.is_empty() || target_lower.starts_with(prefix_lower) {
        0u8
    } else if target_lower.contains(prefix_lower) {
        1u8
    } else {
        return;
    };
    let label_lower = label.to_ascii_lowercase();
    let mut item = json!({
        "label": label,
        "kind": kind,
        "detail": entry.signature(),
        "sortText": format!("{rank}_{label_lower}"),
        "documentation": {
            "kind": "markdown",
            "value": format_entry_markdown(entry),
        }
    });
    if let Some(snippet) = &entry.snippet {
        item["insertText"] = json!(snippet);
        item["insertTextFormat"] = json!(1); // PlainText: keep `$:` verbatim
    } else if insert_call_snippet && entry.inserts_as_call() {
        let call_snippet = if entry.params.is_empty() {
            format!("{label}()$0")
        } else if takes_vocabulary_string(entry) {
            format!("{label}(\"$0\")")
        } else {
            format!("{label}($0)")
        };
        item["insertText"] = json!(call_snippet);
        item["insertTextFormat"] = json!(2); // Snippet
    }
    if entry.deprecated {
        item["deprecated"] = json!(true);
    }
    items.push(item);
}

fn takes_vocabulary_string(entry: &Entry) -> bool {
    if matches!(
        entry.name.as_str(),
        "s" | "sound" | "scale" | "chord" | "voicing"
    ) {
        return true;
    }
    entry
        .params
        .first()
        .is_some_and(|param| !param.choices.is_empty())
}

fn client_supports_snippets(params: Option<&Value>) -> bool {
    params
        .and_then(|value| value.get("capabilities"))
        .and_then(|value| value.get("textDocument"))
        .and_then(|value| value.get("completion"))
        .and_then(|value| value.get("completionItem"))
        .and_then(|value| value.get("snippetSupport"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn has_following_paren(source: &str, offset: usize) -> bool {
    let offset = clamp_char_boundary(source, offset);
    let end = scan::name_starting_at(source, offset).end;
    source[end..].trim_start().starts_with('(')
}

fn format_signature_help(entry: &Entry, active_param: usize) -> Value {
    let mut doc_text = String::new();
    if !entry.summary.is_empty() {
        doc_text.push_str(&entry.summary);
    }
    if !entry.description.is_empty() && entry.description != entry.summary {
        if !doc_text.is_empty() {
            doc_text.push_str("\n\n");
        }
        doc_text.push_str(&entry.description);
    }

    let parameters: Vec<Value> = entry
        .params
        .iter()
        .map(|param| {
            let mut pdoc = String::new();
            if !param.r#type.is_empty() {
                pdoc.push('`');
                pdoc.push_str(&param.r#type);
                pdoc.push('`');
            }
            if !param.description.is_empty() {
                if !pdoc.is_empty() {
                    pdoc.push_str(": ");
                }
                pdoc.push_str(&param.description);
            }
            if !param.choices.is_empty() {
                if !pdoc.is_empty() {
                    pdoc.push_str("\n\n");
                }
                pdoc.push_str("Choices: ");
                for (index, choice) in param.choices.iter().enumerate() {
                    if index > 0 {
                        pdoc.push_str(", ");
                    }
                    pdoc.push('`');
                    pdoc.push_str(&choice.value);
                    pdoc.push('`');
                }
            }
            let mut value = json!({ "label": param.name });
            if !pdoc.is_empty() {
                value["documentation"] = json!({
                    "kind": "markdown",
                    "value": pdoc,
                });
            }
            value
        })
        .collect();

    let mut signature = json!({
        "label": entry.signature(),
        "parameters": parameters,
    });
    if !doc_text.is_empty() {
        signature["documentation"] = json!({
            "kind": "markdown",
            "value": doc_text,
        });
    }

    let active_parameter = if entry.params.is_empty() {
        0
    } else {
        active_param.min(entry.params.len() - 1)
    };

    json!({
        "signatures": [signature],
        "activeSignature": 0,
        "activeParameter": active_parameter,
    })
}

fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": 1,
            "completionProvider": {
                "triggerCharacters": [".", "\"", "'"]
            },
            "hoverProvider": true,
            "signatureHelpProvider": {
                "triggerCharacters": ["(", ","]
            }
        },
        "serverInfo": {
            "name": "rustel",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn empty_completion_list() -> Value {
    json!({
        "isIncomplete": false,
        "items": []
    })
}

fn diagnostics_for(source: &str) -> Vec<Value> {
    lint::lint(source, false, None)
        .into_iter()
        .map(|diagnostic| {
            let severity = match diagnostic.level {
                Level::Syntax => 1,
                Level::Value => 2,
                Level::Note => 3,
            };
            let from = diagnostic.from.min(source.len());
            let to = diagnostic.to.max(from).min(source.len());
            let (start_line, start_char) = offset_to_utf16_position(source, from);
            let (end_line, end_char) = offset_to_utf16_position(source, to);
            json!({
                "range": {
                    "start": { "line": start_line, "character": start_char },
                    "end": { "line": end_line, "character": end_char },
                },
                "severity": severity,
                "source": "rustel",
                "message": diagnostic.message,
            })
        })
        .collect()
}

fn format_entry_markdown(entry: &Entry) -> String {
    let mut out = String::new();
    out.push_str("```js\n");
    out.push_str(&entry.signature());
    out.push_str("\n```");
    if !entry.synonyms.is_empty() {
        out.push_str("\n\nAliases: ");
        for (index, alias) in entry.synonyms.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push('`');
            out.push_str(alias);
            out.push('`');
        }
    }
    if entry.deprecated {
        out.push_str("\n\nDeprecated.");
    }
    if !entry.summary.is_empty() {
        out.push_str("\n\n");
        out.push_str(&entry.summary);
    }
    if !entry.description.is_empty() && entry.description != entry.summary {
        out.push_str("\n\n");
        out.push_str(&entry.description);
    }
    if !entry.params.is_empty() {
        out.push_str("\n\nParameters:\n");
        for param in &entry.params {
            out.push_str("- `");
            out.push_str(&param.name);
            out.push('`');
            if !param.r#type.is_empty() {
                out.push_str(" (`");
                out.push_str(&param.r#type);
                out.push_str("`)");
            }
            if !param.description.is_empty() {
                out.push_str(": ");
                out.push_str(&param.description);
            }
            out.push('\n');
        }
    }
    for example in entry.examples.iter().take(3) {
        out.push_str("\n```js\n");
        out.push_str(example);
        out.push_str("\n```\n");
    }
    out
}

fn parse_did_open(params: &Value) -> Option<(&str, &str)> {
    let doc = params.get("textDocument")?;
    let uri = doc.get("uri")?.as_str()?;
    let text = doc.get("text")?.as_str()?;
    Some((uri, text))
}

fn parse_did_change(params: &Value) -> Option<(&str, &str)> {
    let uri = params.get("textDocument")?.get("uri")?.as_str()?;
    let changes = params.get("contentChanges")?.as_array()?;
    let text = changes.last()?.get("text")?.as_str()?;
    Some((uri, text))
}

fn parse_text_document_position(params: &Value) -> Option<(&str, u32, u32)> {
    let uri = params.get("textDocument")?.get("uri")?.as_str()?;
    let position = params.get("position")?;
    let line = u32::try_from(position.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(position.get("character")?.as_u64()?).ok()?;
    Some((uri, line, character))
}

fn cursor_context(source: &str, offset: usize) -> CursorContext<'_> {
    let offset = clamp_char_boundary(source, offset);
    let bytes = source.as_bytes();
    let mut state = ScanState::Code;
    let mut index = 0usize;
    while index < offset {
        let byte = bytes[index];
        match state {
            ScanState::Code => {
                if byte == b'/' && index + 1 < offset && bytes[index + 1] == b'/' {
                    state = ScanState::LineComment;
                    index += 2;
                    continue;
                }
                if byte == b'/' && index + 1 < offset && bytes[index + 1] == b'*' {
                    state = ScanState::BlockComment;
                    index += 2;
                    continue;
                }
                if matches!(byte, b'"' | b'\'' | b'`') {
                    state = ScanState::String {
                        quote: byte,
                        start: index,
                    };
                }
                index += 1;
            }
            ScanState::LineComment => {
                if byte == b'\n' {
                    state = ScanState::Code;
                }
                index += 1;
            }
            ScanState::BlockComment => {
                if byte == b'*' && index + 1 < offset && bytes[index + 1] == b'/' {
                    state = ScanState::Code;
                    index += 2;
                    continue;
                }
                index += 1;
            }
            ScanState::String { quote, .. } => {
                if byte == b'\\' && index + 1 < offset {
                    index += 2;
                    continue;
                }
                if byte == quote {
                    state = ScanState::Code;
                }
                index += 1;
            }
        }
    }

    match state {
        ScanState::LineComment | ScanState::BlockComment => CursorContext::Comment,
        ScanState::String { start, .. } => {
            let callee = lint::callee_before(source, start);
            let content_start = (start + 1).min(offset);
            match callee {
                Some("s" | "sound") => CursorContext::StringArg {
                    kind: StringArgKind::Sound,
                    prefix: tail_token(source, content_start, offset, |b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
                    }),
                },
                Some("scale") => CursorContext::StringArg {
                    kind: StringArgKind::Scale,
                    prefix: tail_token(source, content_start, offset, |b| {
                        !b.is_ascii_whitespace()
                            && !matches!(b, b':' | b'<' | b'>' | b'[' | b']' | b',')
                    }),
                },
                Some("chord" | "voicing") => CursorContext::StringArg {
                    kind: StringArgKind::Chord,
                    prefix: tail_token(source, content_start, offset, |b| {
                        !b.is_ascii_whitespace() && !matches!(b, b'<' | b'>' | b'[' | b']' | b',')
                    }),
                },
                _ => CursorContext::OtherString {
                    prefix: tail_token(source, content_start, offset, |b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
                    }),
                },
            }
        }
        ScanState::Code => {
            let prefix_range = scan::name_ending_at(source, offset);
            let before = scan::space_before(source, prefix_range.start);
            if before > 0 && bytes[before - 1] == b'.' {
                CursorContext::Member {
                    prefix: &source[prefix_range],
                }
            } else {
                CursorContext::TopLevel {
                    prefix: &source[prefix_range],
                }
            }
        }
    }
}

fn tail_token(
    source: &str,
    content_start: usize,
    offset: usize,
    keep: impl Fn(u8) -> bool,
) -> &str {
    let bytes = source.as_bytes();
    let mut start = offset;
    while start > content_start && keep(bytes[start - 1]) {
        start -= 1;
    }
    while start < offset && !source.is_char_boundary(start) {
        start += 1;
    }
    &source[start..offset]
}

fn enclosing_call_at(source: &str, offset: usize) -> Option<(&str, usize)> {
    let offset = clamp_char_boundary(source, offset);
    let bytes = source.as_bytes();
    let mut state = ScanState::Code;
    let mut index = 0usize;
    let mut stack: Vec<(Option<&str>, usize, usize)> = Vec::new();

    while index < offset {
        let byte = bytes[index];
        match state {
            ScanState::Code => {
                if byte == b'/' && index + 1 < offset && bytes[index + 1] == b'/' {
                    state = ScanState::LineComment;
                    index += 2;
                    continue;
                }
                if byte == b'/' && index + 1 < offset && bytes[index + 1] == b'*' {
                    state = ScanState::BlockComment;
                    index += 2;
                    continue;
                }
                if matches!(byte, b'"' | b'\'' | b'`') {
                    state = ScanState::String {
                        quote: byte,
                        start: index,
                    };
                    index += 1;
                    continue;
                }
                match byte {
                    b'(' => {
                        let before = scan::space_before(source, index);
                        let range = scan::name_ending_at(source, before);
                        let callee = (range.start < range.end).then_some(&source[range]);
                        if stack.len() < MAX_CALL_STACK_DEPTH {
                            stack.push((callee, 0, 0));
                        }
                    }
                    b')' => {
                        stack.pop();
                    }
                    b'[' | b'{' => {
                        if let Some((_, _, inner_delims)) = stack.last_mut() {
                            *inner_delims = inner_delims.saturating_add(1);
                        }
                    }
                    b']' | b'}' => {
                        if let Some((_, _, inner_delims)) = stack.last_mut() {
                            *inner_delims = inner_delims.saturating_sub(1);
                        }
                    }
                    b',' => {
                        if let Some((_, commas, inner_delims)) = stack.last_mut()
                            && *inner_delims == 0
                        {
                            *commas = commas.saturating_add(1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            ScanState::LineComment => {
                if byte == b'\n' {
                    state = ScanState::Code;
                }
                index += 1;
            }
            ScanState::BlockComment => {
                if byte == b'*' && index + 1 < offset && bytes[index + 1] == b'/' {
                    state = ScanState::Code;
                    index += 2;
                    continue;
                }
                index += 1;
            }
            ScanState::String { quote, .. } => {
                if byte == b'\\' && index + 1 < offset {
                    index += 2;
                    continue;
                }
                if byte == quote {
                    state = ScanState::Code;
                }
                index += 1;
            }
        }
    }

    if matches!(state, ScanState::LineComment | ScanState::BlockComment) {
        return None;
    }
    stack
        .into_iter()
        .rev()
        .find_map(|(callee, commas, _)| callee.map(|name| (name, commas)))
}

fn word_range_at(source: &str, offset: usize) -> Option<Range<usize>> {
    let offset = clamp_char_boundary(source, offset);
    let bytes = source.as_bytes();
    let probe = if offset < bytes.len() && scan::is_name_byte(bytes[offset]) {
        offset
    } else if offset > 0 && scan::is_name_byte(bytes[offset - 1]) {
        offset - 1
    } else {
        return None;
    };
    let start = scan::name_ending_at(source, probe).start;
    let end = scan::name_starting_at(source, probe).end;
    (start < end).then_some(start..end)
}

fn clamp_char_boundary(text: &str, offset: usize) -> usize {
    let mut index = offset.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn utf16_position_to_offset(text: &str, target_line: u32, target_char: u32) -> usize {
    let mut line = 0u32;
    let mut offset = 0usize;
    for segment in text.split_inclusive('\n') {
        if line == target_line {
            let line_body = segment.trim_end_matches(['\r', '\n']);
            let mut units = 0u32;
            for (byte_index, ch) in line_body.char_indices() {
                if units >= target_char {
                    return offset + byte_index;
                }
                units = units.saturating_add(ch.len_utf16() as u32);
            }
            return offset + line_body.len();
        }
        offset += segment.len();
        line = line.saturating_add(1);
    }
    text.len()
}

fn offset_to_utf16_position(text: &str, offset: usize) -> (u32, u32) {
    let clamped = clamp_char_boundary(text, offset);
    let mut line = 0u32;
    let mut character = 0u32;
    for ch in text[..clamped].chars() {
        if ch == '\n' {
            line = line.saturating_add(1);
            character = 0;
        } else if ch != '\r' {
            character = character.saturating_add(ch.len_utf16() as u32);
        }
    }
    (line, character)
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>, RuntimeError> {
    let mut content_length: Option<usize> = None;
    let mut headers = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        let read = read_bounded_line(reader, &mut line)?;
        if read == 0 {
            if headers == 0 {
                return Ok(None);
            }
            return Err(RuntimeError::Message(
                "unexpected end of stream inside LSP headers".into(),
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if headers == 0 {
                continue;
            }
            break;
        }
        headers += 1;
        if headers > MAX_RPC_HEADERS {
            return Err(RuntimeError::ResourceLimit(
                "LSP header count exceeded limit".into(),
            ));
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| RuntimeError::Message("invalid LSP Content-Length header".into()))?;
            if length > MAX_RPC_FRAME_BYTES {
                return Err(RuntimeError::ResourceLimit(format!(
                    "LSP frame exceeds {MAX_RPC_FRAME_BYTES} byte limit"
                )));
            }
            content_length = Some(length);
        }
    }

    let length = content_length
        .ok_or_else(|| RuntimeError::Message("missing LSP Content-Length header".into()))?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)
        .map_err(|error| RuntimeError::Message(format!("invalid LSP JSON payload: {error}")))?;
    Ok(Some(value))
}

fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut String) -> Result<usize, RuntimeError> {
    let mut bytes = Vec::new();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if total + take > MAX_RPC_HEADER_LINE_BYTES {
                return Err(RuntimeError::ResourceLimit(
                    "LSP header line exceeded limit".into(),
                ));
            }
            bytes.extend_from_slice(&available[..take]);
            reader.consume(take);
            total += take;
            break;
        }
        let take = available.len();
        if total + take > MAX_RPC_HEADER_LINE_BYTES {
            return Err(RuntimeError::ResourceLimit(
                "LSP header line exceeded limit".into(),
            ));
        }
        bytes.extend_from_slice(available);
        reader.consume(take);
        total += take;
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| RuntimeError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    line.push_str(&text);
    Ok(total)
}

fn publish_diagnostics<W: Write>(
    writer: &mut W,
    uri: &str,
    diagnostics: Vec<Value>,
) -> Result<(), RuntimeError> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": diagnostics,
            }
        }),
    )
}

fn write_result<W: Write>(writer: &mut W, id: &Value, result: Value) -> Result<(), RuntimeError> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
    )
}

fn write_error<W: Write>(
    writer: &mut W,
    id: &Value,
    code: i64,
    message: &str,
) -> Result<(), RuntimeError> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            }
        }),
    )
}

fn write_message<W: Write>(writer: &mut W, payload: &Value) -> Result<(), RuntimeError> {
    let body =
        serde_json::to_vec(payload).map_err(|error| RuntimeError::Message(error.to_string()))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(message: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        write_message(&mut out, message).expect("frame LSP message");
        out
    }

    fn read_all_frames(bytes: &[u8]) -> Vec<Value> {
        let mut cursor = io::Cursor::new(bytes);
        let mut out = Vec::new();
        while let Some(message) = read_message(&mut cursor).expect("parse LSP frame") {
            out.push(message);
        }
        out
    }

    #[test]
    fn utf16_position_roundtrip_handles_crlf_and_supplementary_chars() {
        let doc = "note(\"c4\")\r\n// 😀 hi\n.lpf(800)";
        let offset = doc.find("hi").expect("find hi");
        let (line, ch) = offset_to_utf16_position(doc, offset);
        assert_eq!((line, ch), (1, 6)); // "// " (3) + 😀 (2 UTF-16) + " " (1)
        assert_eq!(utf16_position_to_offset(doc, line, ch), offset);
    }

    #[test]
    fn lsp_lifecycle_diagnostics_completion_and_hover_roundtrip() {
        let mut input = Vec::new();
        // Reject request before initialize.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "textDocument/hover",
            "params": {}
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        })));
        let uri = "file:///score.strudel";
        // Open invalid score -> diagnostics published.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "strudel",
                    "version": 1,
                    "text": "s(\"bd\").lpf("
                }
            }
        })));
        // Change to valid score for completion and hover.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{
                    "text": "setcp\ns(\"saw\").lp"
                }]
            }
        })));
        // Top-level completion on `setcp`.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 5 }
            }
        })));
        // Sound completion inside `s("saw")`.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 6 }
            }
        })));
        // Member completion after `.lp`.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 11 }
            }
        })));
        // Hover over `lpf`.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 3 },
                "contentChanges": [{
                    "text": "s(\"bd\").lpf(800)"
                }]
            }
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 9 }
            }
        })));
        // Close clears diagnostics.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } }
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "shutdown",
            "params": null
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "exit"
        })));

        let mut output = Vec::new();
        serve(io::Cursor::new(input), &mut output).expect("LSP session exits cleanly");
        let messages = read_all_frames(&output);

        // Request 0 rejected before initialize.
        let uninit = messages
            .iter()
            .find(|msg| msg.get("id") == Some(&json!(0)))
            .expect("uninit response");
        assert_eq!(uninit["error"]["code"], JSONRPC_SERVER_NOT_INITIALIZED);

        // Request 1 returns capabilities.
        let init = messages
            .iter()
            .find(|msg| msg.get("id") == Some(&json!(1)))
            .expect("init response");
        assert_eq!(init["result"]["capabilities"]["hoverProvider"], true);
        assert_eq!(init["result"]["serverInfo"]["name"], "rustel");

        // First publishDiagnostics carries syntax error.
        let diag_messages: Vec<&Value> = messages
            .iter()
            .filter(|msg| msg.get("method") == Some(&json!("textDocument/publishDiagnostics")))
            .collect();
        assert!(diag_messages.len() >= 3);
        assert!(
            !diag_messages[0]["params"]["diagnostics"]
                .as_array()
                .expect("diagnostics array")
                .is_empty(),
            "syntax error must publish diagnostics"
        );
        // Closing document clears diagnostics.
        assert!(
            diag_messages.last().expect("close diagnostics")["params"]["diagnostics"]
                .as_array()
                .expect("diagnostics array")
                .is_empty(),
            "didClose must clear diagnostics"
        );

        // Completion 2 finds setCps/setcps.
        let top_labels: Vec<&str> = messages
            .iter()
            .find(|msg| msg.get("id") == Some(&json!(2)))
            .expect("top completion")["result"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(top_labels.contains(&"setCps"), "top labels: {top_labels:?}");
        assert!(top_labels.contains(&"setcps"), "top labels: {top_labels:?}");

        // Completion 3 inside s("saw") finds sawtooth.
        let sound_labels: Vec<&str> = messages
            .iter()
            .find(|msg| msg.get("id") == Some(&json!(3)))
            .expect("sound completion")["result"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(
            sound_labels.contains(&"sawtooth"),
            "sound labels: {sound_labels:?}"
        );

        // Completion 4 after `.lp` finds `lpf`.
        let member_labels: Vec<&str> = messages
            .iter()
            .find(|msg| msg.get("id") == Some(&json!(4)))
            .expect("member completion")["result"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(
            member_labels.contains(&"lpf"),
            "member labels: {member_labels:?}"
        );

        // Hover 5 documents `lpf`.
        let hover = messages
            .iter()
            .find(|msg| msg.get("id") == Some(&json!(5)))
            .expect("hover response");
        let markdown = hover["result"]["contents"]["value"]
            .as_str()
            .expect("hover markdown");
        assert!(markdown.contains("lpf("), "hover markdown: {markdown}");
    }

    #[test]
    fn lsp_call_snippets_and_signature_help_suggest_arguments() {
        let uri = "file:///sig.strudel";
        let mut input = Vec::new();
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "textDocument": {
                        "completion": {
                            "completionItem": { "snippetSupport": true }
                        }
                    }
                }
            }
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "strudel",
                    "version": 1,
                    "text": "note(\"c4\").lp\nnote(\"c4\").s\nnote(\"c4\").rev\nnote(\"c4\").lp(800)\nnote(\"c4\").euclid(3, )\nfoo({ a: euclid(3, ) })"
                }
            }
        })));
        // Line 0: `.lp` -> `lpf($0)` snippet.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 0, "character": 13 } }
        })));
        // Line 1: `.s` -> `s("$0")` snippet.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 12 } }
        })));
        // Line 2: `.rev` -> `rev()$0` zero-arg snippet.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 2, "character": 14 } }
        })));
        // Line 3: `.lp(800)` already has `(` -> no duplicate call snippet.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 3, "character": 13 } }
        })));
        // Line 4: signatureHelp inside `euclid(3, |)` -> activeParameter = 1.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "textDocument/signatureHelp",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 4, "character": 21 } }
        })));
        // Line 5: signatureHelp inside nested `foo({ a: euclid(3, |) })` -> activeParameter = 1.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "textDocument/signatureHelp",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 5, "character": 19 } }
        })));
        input.extend(frame(
            &json!({ "jsonrpc": "2.0", "id": 8, "method": "shutdown", "params": null }),
        ));
        input.extend(frame(&json!({ "jsonrpc": "2.0", "method": "exit" })));

        let mut output = Vec::new();
        serve(io::Cursor::new(input), &mut output).expect("LSP session exits cleanly");
        let messages = read_all_frames(&output);

        let init = messages
            .iter()
            .find(|m| m.get("id") == Some(&json!(1)))
            .expect("init");
        assert_eq!(
            init["result"]["capabilities"]["signatureHelpProvider"]["triggerCharacters"],
            json!(["(", ","])
        );

        let find_item = |req_id: i64, label: &str| {
            messages
                .iter()
                .find(|m| m.get("id") == Some(&json!(req_id)))
                .expect("completion response")["result"]["items"]
                .as_array()
                .expect("items")
                .iter()
                .find(|item| item["label"] == label)
                .cloned()
                .unwrap_or_else(|| panic!("missing completion {label} in request {req_id}"))
        };

        let lpf = find_item(2, "lpf");
        assert_eq!(lpf["insertText"], "lpf($0)");
        assert_eq!(lpf["insertTextFormat"], 2);

        let sound = find_item(3, "s");
        assert_eq!(sound["insertText"], "s(\"$0\")");
        assert_eq!(sound["insertTextFormat"], 2);

        let rev = find_item(4, "rev");
        assert_eq!(rev["insertText"], "rev()$0");
        assert_eq!(rev["insertTextFormat"], 2);

        let lpf_existing = find_item(5, "lpf");
        assert!(lpf_existing.get("insertText").is_none());

        for req_id in [6, 7] {
            let sig = messages
                .iter()
                .find(|m| m.get("id") == Some(&json!(req_id)))
                .expect("signatureHelp response")["result"]
                .clone();
            assert_eq!(sig["signatures"][0]["label"], "euclid(pulses, steps)");
            assert_eq!(sig["activeParameter"], 1);
            assert_eq!(sig["signatures"][0]["parameters"][1]["label"], "steps");
        }
    }

    #[test]
    fn lsp_chords_with_root_kabelsalat_k_and_vocabulary_signatures() {
        let reference = Reference::load_all();
        for name in ["major", "^7", "sawtooth"] {
            let entry = reference
                .entries()
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("missing reference entry {name}"));
            assert!(
                !entry.inserts_as_call(),
                "{name} must not be marked callable"
            );
            assert_eq!(
                entry.signature(),
                name,
                "{name} signature must not carry call parentheses"
            );
        }

        let uri = "file:///chords_kabel.strudel";
        let text = "chord(\"Cm\")\nchord(\"F#7\")\n$: note(\"c3\").K(sFreq)\n$: note(\"c3\").K(si)";
        let mut input = Vec::new();
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "strudel",
                    "version": 1,
                    "text": text
                }
            }
        })));
        // Inside `chord("Cm|")` -> offers `Cm`, `Cm7`, etc.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 0, "character": 9 } }
        })));
        // Inside `chord("F#7|")` -> offers `F#7`, `F#7b9`, etc.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 10 } }
        })));
        // Inside `.K(si|)` -> offers KabelSalat `sine`.
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 3, "character": 18 } }
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "shutdown"
        })));
        input.extend(frame(&json!({
            "jsonrpc": "2.0",
            "method": "exit"
        })));

        let mut output = Vec::new();
        serve(io::Cursor::new(input), &mut output).expect("serve");
        let messages = read_all_frames(&output);

        let labels = |req_id: i64| -> Vec<String> {
            messages
                .iter()
                .find(|m| m.get("id") == Some(&json!(req_id)))
                .expect("completion response")["result"]["items"]
                .as_array()
                .expect("items")
                .iter()
                .filter_map(|item| item["label"].as_str().map(str::to_owned))
                .collect()
        };

        let cm = labels(2);
        assert!(cm.iter().any(|l| l == "Cm7"), "Cm completions: {cm:?}");
        let fsharp = labels(3);
        assert!(
            fsharp.iter().any(|l| l == "F#7") && fsharp.iter().any(|l| l == "F#7b9"),
            "F#7 completions: {fsharp:?}"
        );
        let kabel = labels(4);
        assert!(
            kabel.iter().any(|l| l == "sine"),
            "KabelSalat completions inside K(...): {kabel:?}"
        );
    }
}
