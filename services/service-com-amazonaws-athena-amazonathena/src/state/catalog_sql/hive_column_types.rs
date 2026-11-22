// SPDX-License-Identifier: CC0-1.0

//! The COLUMN TYPES Hive's DDL grammar takes that the general SQL parser does not read.
//!
//! A Data Catalog column may be declared with a COLLECTION or RECORD type —
//! `array<string>`, `map<string,string>`, `struct<city:string,zip:int>`, nested inside each
//! other — and those are types Athena's DDL defines a table with and its queries read
//! ([`super::hive_types`] reads every one of them as a type). The maintained parser this
//! crate reads a submitted statement with has productions for only some of that syntax: a
//! `CREATE EXTERNAL TABLE` whose column list carries a map or a record type does not parse at
//! all, so the whole statement was refused as text that is no statement of the language —
//! while AWS performs it.
//!
//! What this module does is make the column list READABLE without changing anything else
//! about the statement: each type the parser cannot read is replaced, in the submitted text,
//! by a type name it can, PADDED to exactly the length of the text it replaces — so every
//! byte after it stays where it was and every line and column a later diagnostic reports is
//! the submitted statement's own. The text each column was really declared with is handed
//! back beside the readable statement, and the caller puts it back on the column it belongs
//! to, so what the catalog records is what the client wrote.
//!
//! The reading is on the TOKENS, through the same lexer every other reading of a submitted
//! statement here goes through ([`super::grammar::significant_tokens`]): what is located is
//! the type of a declared column, never a substring of the text.

use ::std::collections::HashMap;

use ::sqlparser::tokenizer::{Token, TokenWithSpan};

use super::grammar::{significant_tokens, word_is};

/// The type name a column type this parser cannot read is made readable as: the unbounded
/// character type, which every value of a collection or record column is carried as while the
/// statement is read — the declaration itself is put back by the caller.
const READABLE_TYPE: &str = "string";

/// How many columns one declaration this reader will walk may carry: the bound on the work
/// one submitted statement can ask of it.
const MAX_DECLARED_COLUMNS: usize = 1_024;

/// `sql` with every column type the general parser cannot read made readable, and the text
/// each such column was declared with, keyed by its position in the declaration — or `None`
/// when the statement declares no such type, which is every statement the parser already
/// reads.
#[must_use]
pub fn readable(sql: &str) -> Option<(String, HashMap<usize, String>)> {
    let tokens = significant_tokens(sql);
    let columns = declared_columns(&tokens)?;
    let offsets = Offsets::of(sql);
    let mut declared: HashMap<usize, String> = HashMap::new();
    let mut readable = sql.to_string();
    for (position, column) in columns.iter().enumerate() {
        if !column.nested {
            continue;
        }
        let (from, to) = (
            offsets.byte_at(column.from)?,
            offsets.byte_at(column.to)?,
        );
        let text = readable.get(from..to)?.trim().to_string();
        let span = to.checked_sub(from)?;
        if span < READABLE_TYPE.len() {
            return None;
        }
        // Padded to the length of the text it replaces, so nothing after it moves. The range
        // is the one the lexer reported for the type's own tokens, so it is a span of whole
        // characters and the replacement lands exactly on it.
        let padded = format!(
            "{READABLE_TYPE}{}",
            " ".repeat(span - READABLE_TYPE.len())
        );
        readable.replace_range(from..to, &padded);
        declared.insert(position, text);
    }
    (!declared.is_empty()).then_some((readable, declared))
}

/// Where in the submitted text one declared column's TYPE is written, and whether that type
/// is one the general parser cannot read.
#[derive(Debug, Clone, Copy)]
struct DeclaredType {
    /// Where the type text starts, as a character position in the submitted text.
    from: (u64, u64),
    /// Where it ends, exclusive.
    to: (u64, u64),
    /// Whether it is a COLLECTION or RECORD type — one written with the angle brackets the
    /// general parser has no production for.
    nested: bool,
}

/// The types the columns of a `CREATE TABLE`'s declaration list are written with, in
/// declaration order, or `None` for a statement that declares no column list at all.
fn declared_columns(tokens: &[TokenWithSpan]) -> Option<Vec<DeclaredType>> {
    let mut rest = after_table_name(tokens)?;
    // The column list itself.
    let (first, inside) = rest.split_first()?;
    if !matches!(first.token, Token::LParen) {
        return None;
    }
    rest = inside;
    let mut columns: Vec<DeclaredType> = Vec::new();
    let mut depth = 0usize;
    let mut term: Vec<&TokenWithSpan> = Vec::new();
    for token in rest {
        match &token.token {
            Token::LParen | Token::Lt => depth += 1,
            Token::ShiftLeft => depth += 2,
            Token::RParen | Token::Gt if depth > 0 => depth -= 1,
            Token::ShiftRight if depth > 1 => depth -= 2,
            // The `)` that closes the column list, which is where the declaration ends.
            Token::RParen => {
                columns.push(declared_type(&term)?);
                return Some(columns);
            }
            Token::Comma if depth == 0 => {
                columns.push(declared_type(&term)?);
                if columns.len() > MAX_DECLARED_COLUMNS {
                    return None;
                }
                term.clear();
                continue;
            }
            _ => {}
        }
        term.push(token);
    }
    None
}

/// What is left after `CREATE [EXTERNAL] TABLE [IF NOT EXISTS] <name>`, or `None` for tokens
/// that are not a table definition.
fn after_table_name(tokens: &[TokenWithSpan]) -> Option<&[TokenWithSpan]> {
    let mut rest = after_word(tokens, "CREATE")?;
    // `EXTERNAL` is the word Hive's own DDL defines a table over existing data with; a
    // definition without it is read here just the same.
    if let Some(after) = after_word(rest, "EXTERNAL") {
        rest = after;
    }
    rest = after_word(rest, "TABLE")?;
    if let Some(after) = after_word(rest, "IF")
        .and_then(|rest| after_word(rest, "NOT"))
        .and_then(|rest| after_word(rest, "EXISTS"))
    {
        rest = after;
    }
    // The name, and every further part a `.` qualifies it with.
    rest = after_name(rest)?;
    while let Some(after) = rest
        .split_first()
        .filter(|(first, _)| matches!(first.token, Token::Period))
        .and_then(|(_, after)| after_name(after))
    {
        rest = after;
    }
    Some(rest)
}

/// What is left after the word `word`, or `None` when the next token is not that word.
fn after_word<'t>(tokens: &'t [TokenWithSpan], word: &str) -> Option<&'t [TokenWithSpan]> {
    let (first, rest) = tokens.split_first()?;
    word_is(first, word).then_some(rest)
}

/// What is left after a NAME, or `None` when the next token is not one.
fn after_name(tokens: &[TokenWithSpan]) -> Option<&[TokenWithSpan]> {
    let (first, rest) = tokens.split_first()?;
    matches!(first.token, Token::Word(..)).then_some(rest)
}

/// Where the TYPE of one declared column is written: everything after the column's name, up
/// to the `COMMENT` its declaration may close with.
fn declared_type(term: &[&TokenWithSpan]) -> Option<DeclaredType> {
    let (_, after_name) = term.split_first()?;
    let type_tokens: Vec<&&TokenWithSpan> = after_name
        .iter()
        .take_while(|token| !word_is(token, "COMMENT"))
        .collect();
    let first = type_tokens.first()?;
    let last = type_tokens.last()?;
    Some(DeclaredType {
        from: (first.span.start.line, first.span.start.column),
        to: (last.span.end.line, last.span.end.column),
        nested: type_tokens
            .iter()
            .any(|token| matches!(token.token, Token::Lt | Token::ShiftLeft)),
    })
}

/// Where each LINE of the submitted text starts, so a position the lexer reports as a line
/// and a column can be read as a byte offset into it.
#[derive(Debug)]
struct Offsets {
    /// The byte offset of each line's first character, in line order.
    lines: Vec<usize>,
    /// The text itself, for walking a line's characters to the column asked for.
    text: String,
}

impl Offsets {
    /// The line offsets of `text`.
    fn of(text: &str) -> Self {
        let mut lines = vec![0usize];
        for (at, character) in text.char_indices() {
            if character == '\n' {
                lines.push(at + character.len_utf8());
            }
        }
        Self {
            lines,
            text: text.to_string(),
        }
    }

    /// The byte offset of the 1-based line and column `at`, or `None` when the text has no
    /// such position.
    ///
    /// The lexer counts a column in CHARACTERS, so the offset is reached by walking the
    /// line's characters rather than by adding the column to the line's start.
    fn byte_at(&self, at: (u64, u64)) -> Option<usize> {
        let (line, column) = at;
        let start = *self.lines.get(usize::try_from(line).ok()?.checked_sub(1)?)?;
        let ahead = usize::try_from(column).ok()?.checked_sub(1)?;
        let rest = self.text.get(start..)?;
        match rest.char_indices().nth(ahead) {
            Some((at, _)) => Some(start + at),
            // The position just past the last character of the text, which is where a token
            // at its end closes.
            None if rest.chars().count() == ahead => Some(self.text.len()),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the readable-column-list table.
    struct Case {
        /// What this row is about.
        case: &'static str,
        /// The statement submitted.
        sql: &'static str,
        /// The statement with its column types made readable, or `None` where the statement
        /// is read exactly as written.
        readable: Option<&'static str>,
        /// The text each column was really declared with, by its position in the list.
        declared: &'static [(usize, &'static str)],
    }

    /// A column list carrying a type the general parser cannot read is made readable in
    /// place, every other byte of the statement is left where it was, and the text each such
    /// column was declared with is handed back.
    ///
    /// The rows differ along the kind of type (a map, a record, a nested collection), how
    /// many columns carry one and at which positions, and the statements that carry none.
    #[test]
    fn a_column_type_the_parser_cannot_read_is_made_readable_in_place() {
        let cases: Vec<Case> = vec![
            Case {
                case: "no column type the parser cannot read",
                sql: "CREATE EXTERNAL TABLE t (id int, label string) LOCATION 's3://b/p/'",
                readable: None,
                declared: &[],
            },
            Case {
                case: "a statement that is no table definition",
                sql: "SELECT 1",
                readable: None,
                declared: &[],
            },
            Case {
                case: "a map type, which the parser has no production for",
                sql: "CREATE EXTERNAL TABLE t (attrs map<string,string>) LOCATION 's3://b/p/'",
                readable: Some(
                    "CREATE EXTERNAL TABLE t (attrs string            ) LOCATION 's3://b/p/'",
                ),
                declared: &[(0, "map<string,string>")],
            },
            Case {
                case: "a record type written with Hive's own separator",
                sql: "CREATE EXTERNAL TABLE t (addr struct<city:string>) LOCATION 's3://b/p/'",
                readable: Some(
                    "CREATE EXTERNAL TABLE t (addr string             ) LOCATION 's3://b/p/'",
                ),
                declared: &[(0, "struct<city:string>")],
            },
            Case {
                case: "two such columns among readable ones",
                sql: "CREATE EXTERNAL TABLE t (id int, tags array<string>, attrs map<string,int>, \
                      label string) LOCATION 's3://b/p/'",
                readable: Some(
                    "CREATE EXTERNAL TABLE t (id int, tags string       , attrs string         , \
                      label string) LOCATION 's3://b/p/'",
                ),
                declared: &[(1, "array<string>"), (2, "map<string,int>")],
            },
            Case {
                case: "a collection nested in a record, closing with two brackets at once",
                sql: "CREATE EXTERNAL TABLE t (payload struct<tags:array<string>>) \
                      LOCATION 's3://b/p/'",
                readable: Some(
                    "CREATE EXTERNAL TABLE t (payload string                    ) \
                      LOCATION 's3://b/p/'",
                ),
                declared: &[(0, "struct<tags:array<string>>")],
            },
        ];

        for Case {
            case,
            sql,
            readable: expected,
            declared,
        } in &cases
        {
            let actual = readable(sql);
            match expected {
                None => assert!(
                    actual.is_none(),
                    "case {case:?}: expected the statement to be read as written, got {actual:?}"
                ),
                Some(readable_sql) => {
                    let (made, found) = actual.unwrap_or_else(|| {
                        panic!("case {case:?}: expected the statement to be made readable")
                    });
                    assert_eq!(
                        made, *readable_sql,
                        "case {case:?}: expected the readable statement {readable_sql:?}, got \
                         {made:?}"
                    );
                    assert_eq!(
                        made.len(),
                        sql.len(),
                        "case {case:?}: the readable statement must be the same length as the \
                         submitted one, got {made:?}"
                    );
                    let mut expected_types: Vec<(usize, String)> = declared
                        .iter()
                        .map(|(position, text)| (*position, (*text).to_string()))
                        .collect();
                    expected_types.sort_unstable();
                    let mut found_types: Vec<(usize, String)> = found.into_iter().collect();
                    found_types.sort_unstable();
                    assert_eq!(
                        found_types, expected_types,
                        "case {case:?}: expected the declared types {expected_types:?}, got \
                         {found_types:?}"
                    );
                }
            }
        }
    }
}
