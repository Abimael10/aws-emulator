// SPDX-License-Identifier: CC0-1.0

//! Writing a document tree back out as the YAML Systems Manager converts a document to.
//!
//! A read may ask for a document in another `DocumentFormat` than the one it was written
//! in ([`super::document_format`]), and "the document format can be either JSON or YAML":
//! a JSON-authored document asked for as YAML is CONVERTED, and the text of that
//! conversion is part of what the read answers — a client that keeps the YAML a read
//! gives it (the usual way a document is reviewed and committed) diffs the next read's
//! text against it.
//!
//! RECORDED BEHAVIOUR: the conversion's serializer writes
//!
//! * a `---` document-start marker on its own line, then the document;
//! * every STRING scalar in double quotes, so a value like a schema version is read back
//!   as the string it is rather than as a number, while a key is written plain;
//! * a number, a boolean and a null as themselves, unquoted;
//! * block sequences whose `- ` indicator sits at the INDENTATION OF THE KEY the sequence
//!   belongs to rather than indented under it, with each item's own members indented one
//!   level past the indicator;
//! * one trailing newline.
//!
//! The tree written here is the tree the document was parsed into
//! ([`super::load_single_document`]), which is the same tree every other read derives its
//! report from, so nothing is re-written by hand and no value is invented along the way.
//! The parser's own limits are what bound the walk: content is admitted only within the
//! documented size limit ([`super::MAX_DOCUMENT_CONTENT`]) and only after its nesting was
//! guarded, and the walk stops at [`super::MAX_DOCUMENT_DEPTH`] levels either way.

use ::std::fmt::Write as _;

use ::yaml_rust2::Yaml;

/// How many spaces one level of nesting is indented by.
const INDENT: usize = 2;

/// YAML's document-start marker, which the conversion opens with.
const DOCUMENT_START: &str = "---\n";

/// `document` written as the YAML a format conversion answers with, or `None` when the
/// tree nests deeper than this service reads a document to
/// ([`super::MAX_DOCUMENT_DEPTH`]).
pub fn rendered(document: &Yaml) -> Option<String> {
    let mut out = String::from(DOCUMENT_START);
    write_node(&mut out, document, 0, super::MAX_DOCUMENT_DEPTH)?;
    Some(out)
}

/// Write `node` at `indent` as the whole value of a document or of one mapping key,
/// ending in a newline.
///
/// `depth` is how many levels of nesting are left to spend. Every writer here checks it
/// on entry and spends it with [`usize::checked_sub`], so a tree that drains it stops the
/// walk with `None` — whichever of the three it runs out in — rather than taking a level
/// it has no budget for.
fn write_node(out: &mut String, node: &Yaml, indent: usize, depth: usize) -> Option<()> {
    if depth == 0 {
        return None;
    }
    match node {
        Yaml::Hash(entries) if !entries.is_empty() => {
            for (key, value) in entries {
                write_indent(out, indent);
                out.push_str(&key_text(key));
                write_member(out, value, indent, depth)?;
            }
            Some(())
        }
        Yaml::Array(items) if !items.is_empty() => {
            for item in items {
                write_indent(out, indent);
                out.push_str("- ");
                write_item(out, item, indent, depth)?;
            }
            Some(())
        }
        // An empty collection has no block form, so it is written in the flow form YAML
        // states one with; every other node is a scalar.
        other => {
            out.push_str(&scalar_text(other));
            out.push('\n');
            Some(())
        }
    }
}

/// Write the value of one mapping key: a scalar on the key's own line, a nested mapping
/// one level in, and a sequence at the KEY's own indentation.
fn write_member(out: &mut String, value: &Yaml, indent: usize, depth: usize) -> Option<()> {
    if depth == 0 {
        return None;
    }
    match value {
        Yaml::Hash(entries) if !entries.is_empty() => {
            out.push('\n');
            write_node(out, value, indent + INDENT, depth.checked_sub(1)?)
        }
        Yaml::Array(items) if !items.is_empty() => {
            out.push('\n');
            // The sequence indicator sits at the key's indentation, not past it.
            write_node(out, value, indent, depth.checked_sub(1)?)
        }
        scalar => {
            out.push(' ');
            out.push_str(&scalar_text(scalar));
            out.push('\n');
            Some(())
        }
    }
}

/// Write one sequence item, whose first line follows the `- ` indicator and whose
/// remaining lines are indented past it.
fn write_item(out: &mut String, item: &Yaml, indent: usize, depth: usize) -> Option<()> {
    if depth == 0 {
        return None;
    }
    match item {
        Yaml::Hash(entries) if !entries.is_empty() => {
            let mut members = entries.iter();
            let (key, value) = members.next()?;
            out.push_str(&key_text(key));
            write_member(out, value, indent + INDENT, depth.checked_sub(1)?)?;
            for (key, value) in members {
                write_indent(out, indent + INDENT);
                out.push_str(&key_text(key));
                write_member(out, value, indent + INDENT, depth.checked_sub(1)?)?;
            }
            Some(())
        }
        Yaml::Array(items) if !items.is_empty() => {
            let mut rest = String::new();
            write_node(&mut rest, item, indent + INDENT, depth.checked_sub(1)?)?;
            // The nested sequence's first indicator follows this one on the same line.
            out.push_str(rest.trim_start());
            Some(())
        }
        scalar => {
            out.push_str(&scalar_text(scalar));
            out.push('\n');
            Some(())
        }
    }
}

/// `indent` spaces.
fn write_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

/// One mapping key followed by its `:`: a key is written PLAIN where YAML reads it back
/// as the same text, and double-quoted otherwise.
fn key_text(key: &Yaml) -> String {
    let text = scalar_source(key);
    if is_plain_key(&text) {
        format!("{text}:")
    } else {
        format!("{}:", quoted(&text))
    }
}

/// One scalar as this serializer writes it: a string in double quotes, and a number, a
/// boolean or a null as itself.
fn scalar_text(scalar: &Yaml) -> String {
    match scalar {
        Yaml::String(text) => quoted(text),
        Yaml::Integer(number) => number.to_string(),
        Yaml::Real(number) => number.clone(),
        Yaml::Boolean(flag) => flag.to_string(),
        Yaml::Null => "null".to_string(),
        // An empty mapping and an empty sequence have no block form; a node the parser
        // could not place carries no text of its own.
        Yaml::Hash(_) => "{}".to_string(),
        Yaml::Array(_) => "[]".to_string(),
        Yaml::Alias(_) | Yaml::BadValue => String::new(),
    }
}

/// A scalar's own text, for a node used as a mapping KEY.
fn scalar_source(scalar: &Yaml) -> String {
    match scalar {
        Yaml::String(text) => text.clone(),
        other => scalar_text(other),
    }
}

/// `text` in double quotes, with the characters YAML's double-quoted style escapes
/// escaped — the same escapes JSON states, so a line break or a quote inside a value
/// survives the conversion.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                // A control character with no short escape is written as the escape YAML
                // states for one, so the text parses back to the same characters.
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            plain => out.push(plain),
        }
    }
    out.push('"');
    out
}

/// Whether `text` is a key YAML reads back as itself when it is written plain: a
/// non-empty run of the characters a document's keys are spelled with, which cannot be
/// mistaken for a number, a boolean or a null and carries nothing YAML would read as an
/// indicator.
fn is_plain_key(text: &str) -> bool {
    if text.is_empty() || text.len() > MAX_PLAIN_KEY {
        return false;
    }
    let spelled_plainly = text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character));
    if !spelled_plainly {
        return false;
    }
    let first = text.chars().next().is_some_and(|first| first.is_ascii_alphanumeric() || first == '_');
    first && !reads_as_another_type(text)
}

/// How long a key may be and still be written plain: past it the key is quoted, so one
/// line of the conversion cannot be grown without bound by a key a caller wrote.
const MAX_PLAIN_KEY: usize = 1024;

/// Whether YAML would read `text` back as something other than a string: a number, a
/// boolean or a null in any of the spellings YAML gives them.
fn reads_as_another_type(text: &str) -> bool {
    const OTHER_TYPES: &[&str] = &[
        "true", "false", "yes", "no", "on", "off", "null", "nil", "y", "n", "~",
    ];
    if OTHER_TYPES
        .iter()
        .any(|spelling| text.eq_ignore_ascii_case(spelling))
    {
        return true;
    }
    text.parse::<i64>().is_ok() || text.parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of `levels` levels, each nesting the one below it inside a SEQUENCE inside a
    /// MAPPING — the path whose depth budget is spent in [`write_item`] as well as in
    /// [`write_member`], which is where a budget checked in only one of the three writers
    /// would run past zero.
    fn nested(levels: usize) -> Yaml {
        let mut node = Yaml::String("leaf".to_string());
        for _ in 0..levels {
            let mut level = ::yaml_rust2::yaml::Hash::new();
            level.insert(Yaml::String("key".to_string()), Yaml::Array(vec![node]));
            node = Yaml::Hash(level);
        }
        node
    }

    /// RULE (bound): the walk takes at most [`super::super::MAX_DOCUMENT_DEPTH`] levels —
    /// a tree that drains the budget stops with `None`, whichever writer runs out, and no
    /// depth of nesting makes the walk spend a level it has no budget for.
    #[test]
    fn a_tree_that_drains_the_depth_budget_stops_the_walk() {
        const BUDGET: usize = super::super::MAX_DOCUMENT_DEPTH;
        // One row per nesting depth either side of the budget, including the two bounds
        // and one past the larger of them.
        for levels in [0, 1, 2, 8, BUDGET - 1, BUDGET, BUDGET + 1, 4 * BUDGET] {
            let written = rendered(&nested(levels));
            if levels <= 8 {
                assert!(
                    written.is_some(),
                    "case {levels} level(s): a tree within the budget is written, got None"
                );
            }
            if levels >= BUDGET {
                assert!(
                    written.is_none(),
                    "case {levels} level(s): a tree past the budget stops the walk, got \
                     {written:?}"
                );
            }
        }
    }

    /// The YAML `content` converts to, read through the one parser a document is read
    /// with.
    fn converted(content: &str) -> String {
        let document = super::super::load_single_document(content).expect("content parses");
        rendered(&document).expect("the tree is within the depth a document is read to")
    }

    /// RULE: the conversion writes the document-start marker, double-quotes string
    /// scalars, writes a number, a boolean and a null unquoted, and puts a block
    /// sequence's dashes at the indentation of the key the sequence belongs to.
    #[test]
    fn the_conversion_is_the_serializer_style_systems_manager_writes() {
        // (case, JSON content, the YAML it converts to)
        let rows: &[(&str, &str, &str)] = &[
            (
                "a string scalar is quoted and the document starts with the marker",
                r#"{"schemaVersion":"2.2"}"#,
                "---\nschemaVersion: \"2.2\"\n",
            ),
            (
                "a sequence of scalars sits at its key's indentation",
                r#"{"commands":["echo one","echo two"]}"#,
                "---\ncommands:\n- \"echo one\"\n- \"echo two\"\n",
            ),
            (
                "a nested mapping is indented one level",
                r#"{"inputs":{"runCommand":"echo hello"}}"#,
                "---\ninputs:\n  runCommand: \"echo hello\"\n",
            ),
            (
                "a sequence of mappings, each with a nested sequence",
                r#"{"mainSteps":[{"action":"aws:runShellScript","name":"run",
                   "inputs":{"runCommand":["echo hello"]}}]}"#,
                "---\nmainSteps:\n- action: \"aws:runShellScript\"\n  name: \"run\"\n  \
                 inputs:\n    runCommand:\n    - \"echo hello\"\n",
            ),
            (
                "a number, a boolean and a null keep their own spelling",
                r#"{"minItems":1,"ratio":1.5,"enabled":true,"nothing":null}"#,
                "---\nminItems: 1\nratio: 1.5\nenabled: true\nnothing: null\n",
            ),
            (
                "an empty mapping and an empty sequence are written in the flow form",
                r#"{"default":{},"values":[]}"#,
                "---\ndefault: {}\nvalues: []\n",
            ),
            (
                "a value carrying a quote, a backslash and a newline is escaped",
                r#"{"commands":["echo \"hi\" \\ there\nagain"]}"#,
                "---\ncommands:\n- \"echo \\\"hi\\\" \\\\ there\\nagain\"\n",
            ),
            (
                "text outside ASCII survives the conversion",
                r#"{"description":"日本語"}"#,
                "---\ndescription: \"日本語\"\n",
            ),
            (
                "a key YAML would read as another type is quoted",
                r#"{"true":"a","1":"b","plain_key.1":"c"}"#,
                "---\n\"true\": \"a\"\n\"1\": \"b\"\nplain_key.1: \"c\"\n",
            ),
            (
                "a key spelled with a character a plain key cannot carry is quoted",
                r#"{"aws:runShellScript":{"id":"0"}}"#,
                "---\n\"aws:runShellScript\":\n  id: \"0\"\n",
            ),
        ];
        for (case, content, expected) in rows {
            let written = converted(content);
            assert_eq!(
                written, *expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE (invariant): the conversion is YAML this service reads back as the same tree,
    /// so a document converted and re-read declares what it declared before.
    #[test]
    fn the_conversion_reads_back_as_the_same_tree() {
        let rows: &[(&str, &str)] = &[
            ("one scalar", r#"{"schemaVersion":"1.2"}"#),
            (
                "the shape of a schema 1.2 command document",
                r#"{"schemaVersion":"1.2","description":"run",
                   "parameters":{"commands":{"type":"StringList","minItems":1}},
                   "runtimeConfig":{"aws:runShellScript":{"properties":[
                     {"id":"0.aws:runShellScript","runCommand":"{{ commands }}"}]}}}"#,
            ),
            (
                "the shape of a schema 2.2 command document",
                r#"{"schemaVersion":"2.2","mainSteps":[{"action":"aws:runShellScript",
                   "name":"run","inputs":{"runCommand":["echo hello","echo again"]}}]}"#,
            ),
            (
                "values that are not strings",
                r#"{"a":1,"b":true,"c":null,"d":[1,2,3],"e":{"f":{"g":"h"}}}"#,
            ),
        ];
        for (case, content) in rows {
            let written = converted(content);
            let reread = super::super::load_single_document(&written)
                .unwrap_or_else(|why| panic!("case {case:?}: {written:?} must parse ({why:?})"));
            let original =
                super::super::load_single_document(content).expect("the content parses");
            assert_eq!(
                reread, original,
                "case {case:?}: the conversion {written:?} must read back as the same tree"
            );
        }
    }
}
