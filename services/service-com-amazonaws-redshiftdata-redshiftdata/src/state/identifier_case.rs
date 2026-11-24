// SPDX-License-Identifier: CC0-1.0

//! Whether the engine behind a database reads a DELIMITED IDENTIFIER's CASE,
//! and how it holds one when it does.
//!
//! `enable_case_sensitive_identifier` is a server configuration parameter: with
//! it off — its published default — the server folds a delimited identifier, so
//! `"c"` and `"C"` name one column; with it on the server keeps the case, so
//! they are two. Its value comes from the cluster's parameter group and a
//! reboot, which is what [`super::session_settings::in_force`] already reports
//! for it, so the value a statement runs under is read from there and nowhere
//! else.
//!
//! The engine behind this service resolves an identifier WITHOUT regard to case
//! and cannot hold two names differing only in it. So a statement running with
//! the parameter on has each delimited identifier re-spelled, before anything
//! else reads the statement, to a name that carries the case it was declared
//! with ([`stored_name`]): the engine then holds `"c"` and `"C"` as two
//! columns, and resolves each quoted reference to its own. The spelling is a
//! STORAGE name, so every answer about it — a result column, a catalogue read,
//! a refusal naming it — reports the name the caller wrote ([`spelled`]).
//!
//! A name that is already lower-case is held exactly as it is: a table created
//! with the parameter off, and every unquoted name (this dialect folds an
//! unquoted identifier to lower case whatever the parameter says), are the names
//! they always were. Only a name carrying an upper-case letter is re-spelled,
//! which is why turning the parameter on changes nothing a client had already
//! created.
//!
//! The mark that separates a stored name from its case is a character no
//! identifier of this dialect can carry: "Names and identifiers … delimited
//! identifiers … can contain any standard UTF-8 printable characters other than
//! the double quotation mark", and this one is not printable — so no caller's
//! name is ever read as a stored one, and no stored name collides with another.

use std::collections::BTreeMap;

use sqlparser::dialect::RedshiftSqlDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use super::session_settings::{self, SessionSettings};

/// The server configuration parameter that decides it.
///
/// Published as a STATIC cluster parameter, so its value reaches the engine at
/// the restart that applies it and not when the parameter group changes.
pub const CASE_SENSITIVE_IDENTIFIER_PARAMETER: &str = "enable_case_sensitive_identifier";

/// The mark that separates a stored name from the case it was declared with.
const CASE_MARK: char = '\u{1}';

/// Whether the engine a statement runs on reads a delimited identifier's case:
/// the value in force for [`CASE_SENSITIVE_IDENTIFIER_PARAMETER`] on this
/// connection — the session's own where it set one, else the value the
/// warehouse's engine is running with, else the published default.
pub fn case_sensitive(session: &SessionSettings, applied: &BTreeMap<String, String>) -> bool {
    enabled(&session_settings::in_force(
        CASE_SENSITIVE_IDENTIFIER_PARAMETER,
        session,
        applied,
    ))
}

/// Whether a parameter value says "on". The value this parameter publishes is
/// `true` or `false`; a PostgreSQL-derived server reads the other spellings of a
/// boolean setting too, and reads anything else as off.
fn enabled(value: &str) -> bool {
    let value = value.trim();
    ["true", "on", "1", "yes"]
        .iter()
        .any(|spelling| value.eq_ignore_ascii_case(spelling))
}

/// `sql` with every DELIMITED identifier re-spelled as the name the engine
/// holds it under when the case of such a name is read ([`stored_name`]), or
/// `None` when nothing in the statement needed re-spelling — a statement whose
/// names are all lower-case is run exactly as it was submitted.
///
/// Read off the dialect's own TOKEN stream, so a quoted word inside a string
/// literal or a comment is part of that literal or comment and no identifier of
/// the statement is missed: the tokenizer is what tells a delimited identifier
/// from an unquoted word and from a value.
///
/// A text longer than the engine itself accepts ([`super::MAX_SQL_BYTES`], its
/// `SQLITE_LIMIT_SQL_LENGTH`) is not read here at all: the engine is what
/// refuses it, so this one pass is bounded by the length of text the engine
/// accepts rather than by whatever a request carries.
pub fn respell(sql: &str) -> Option<String> {
    if !usize::try_from(super::MAX_SQL_BYTES).is_ok_and(|max| sql.len() <= max) {
        return None;
    }
    let dialect = RedshiftSqlDialect {};
    let tokens = Tokenizer::new(&dialect, sql).tokenize().ok()?;
    let mut respelled = String::with_capacity(sql.len());
    let mut changed = false;
    for token in &tokens {
        match token {
            // A delimited identifier: the name it carries is re-spelled, and
            // the identifier is written back with the quote doubling this
            // dialect escapes an embedded quote with — the token carries the
            // name itself, not the text it was written as.
            Token::Word(word) if word.quote_style == Some(DELIMITER) => {
                let stored = stored_name(&word.value);
                changed |= stored != word.value;
                respelled.push(DELIMITER);
                respelled.push_str(&stored.replace(DELIMITER, "\"\""));
                respelled.push(DELIMITER);
            }
            other => respelled.push_str(&other.to_string()),
        }
    }
    changed.then_some(respelled)
}

/// The character a delimited identifier is written between.
const DELIMITER: char = '"';

/// The name the engine holds the identifier `name` under when the case of a
/// delimited identifier is read.
///
/// A name that carries no upper-case letter is its own stored name, so nothing
/// a client created with the parameter off moves. Any other name is held under
/// itself followed by the mark and the CASE it was declared with, which is what
/// makes two names differing only in case two names to an engine that resolves
/// them without regard to it.
#[must_use]
pub fn stored_name(name: &str) -> String {
    if !name
        .chars()
        .any(|character| character.is_ascii_uppercase() || character == CASE_MARK)
    {
        return name.to_string();
    }
    format!("{name}{CASE_MARK}{}", case_of(name))
}

/// Which characters of `name` are upper-case, as hexadecimal digits — four
/// characters to a digit, the first character in the digit's high bit.
///
/// Two names differing only in case fold to the same name and have the same
/// length, so this is what tells their stored names apart. It carries no
/// hexadecimal digit the mark could be read as.
fn case_of(name: &str) -> String {
    let mut digits = String::new();
    let mut nibble: u32 = 0;
    let mut filled = 0;
    for character in name.chars() {
        nibble = (nibble << 1) | u32::from(character.is_ascii_uppercase());
        filled += 1;
        if filled == 4 {
            digits.push(hexadecimal(nibble));
            nibble = 0;
            filled = 0;
        }
    }
    if filled > 0 {
        digits.push(hexadecimal(nibble << (4 - filled)));
    }
    digits
}

/// One hexadecimal digit of a case pattern.
fn hexadecimal(nibble: u32) -> char {
    char::from_digit(nibble & 0xF, 16).unwrap_or('0')
}

/// `text` as it is SPELLED for a caller: every stored name in it written as the
/// name it was declared with.
///
/// Applied to everything that reports a name the engine holds — a result
/// column, a catalogue entry, the message of a refusal that names one — so a
/// stored spelling never reaches a caller. A text carrying no stored name is
/// answered unchanged, which is every text of a database whose names are all
/// lower-case.
#[must_use]
pub fn spelled(text: &str) -> String {
    if !text.contains(CASE_MARK) {
        return text.to_string();
    }
    let mut written = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != CASE_MARK {
            written.push(character);
            continue;
        }
        // The case that follows the mark is the stored spelling's, not the
        // name's: the name itself was written before it.
        while characters
            .peek()
            .is_some_and(|next| next.is_ascii_hexdigit())
        {
            characters.next();
        }
    }
    written
}
