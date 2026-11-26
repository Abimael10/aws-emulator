// SPDX-License-Identifier: CC0-1.0

//! The `ALTER TABLE … ADD COLUMNS` and `ALTER TABLE … REPLACE COLUMNS` statements Hive's
//! DDL grammar takes, which the general SQL parser does not read.
//!
//! Athena serves the Hive DDL forms that change a table's COLUMN LIST —
//! `ALTER TABLE <name> ADD COLUMNS (<column> <type> [COMMENT '<text>'], …)`, which appends
//! columns to the ones the table declares, and
//! `ALTER TABLE <name> REPLACE COLUMNS (<column> <type>, …)`, which replaces the whole list
//! — and the parser this crate reads statements with has a production for neither: it reads
//! only the singular `ADD COLUMN <column>` form, so the plural declaration list does not
//! parse at all and the statement was refused as text that is no statement of the language,
//! while AWS performs it.
//!
//! This module reads THAT clause sequence off the same maintained TOKENIZER the parser lexes
//! with, exactly as the sibling that reads Hive's own database clauses does
//! ([`super::hive_database`]): nothing here parses SQL in general, and anything that is not
//! one of those two statements is answered `None`.

use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use ::sqlparser::keywords::Keyword;
use ::sqlparser::tokenizer::{Token, TokenWithSpan, Word};

use super::declaration_list;
use super::grammar::{position_of, significant_tokens};
use super::statement::{DeclaredColumn, Position};

/// The word Hive's grammar names a column list of this statement by, which the tokenizer
/// recognises as its own keyword.
const COLUMNS: Keyword = Keyword::COLUMNS;

/// One `ALTER TABLE … ADD`/`REPLACE COLUMNS` statement, as its clauses state it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnChange {
    /// The parts of the table's name, in the order the statement wrote them: the table on
    /// its own, or qualified by its database and its data catalog.
    pub name: Vec<String>,
    /// The name as the statement WROTE it, quoting included.
    pub written: String,
    /// Where the name starts in the submitted text.
    pub at: Position,
    /// Whether the statement REPLACES the table's column list rather than appending to it.
    pub replaces: bool,
    /// The columns it declares, in declaration order.
    pub columns: Vec<DeclaredColumn>,
}

/// The column change `sql` states, or `None` for text that is not one of these two
/// statements.
#[must_use]
pub fn read_column_change(sql: &str) -> Option<ColumnChange> {
    let tokens = significant_tokens(sql);
    let mut rest = tokens.as_slice();
    rest = keyword(rest, Keyword::ALTER)?;
    rest = keyword(rest, Keyword::TABLE)?;
    let (name, written, at, after) = table_name(rest)?;
    rest = after;
    let replaces = match keyword(rest, Keyword::ADD) {
        Some(after) => {
            rest = after;
            false
        }
        None => {
            rest = keyword(rest, Keyword::REPLACE)?;
            true
        }
    };
    rest = keyword(rest, COLUMNS)?;
    let (columns, rest) = declared_columns(rest)?;
    // A statement is read to its END: text carrying anything after the column list is not
    // this statement, whatever its first words said.
    let read = match rest {
        [] => true,
        [last] if matches!(last.token, Token::SemiColon) => true,
        _ => false,
    };
    read.then_some(ColumnChange {
        name,
        written,
        at,
        replaces,
        columns,
    })
}

/// The table name the statement addresses: its parts as written, the whole name as the
/// statement wrote it, where it starts, and what is left after it.
///
/// A name of one, two or three parts, exactly as a relation is written anywhere else — the
/// table on its own, qualified by its database, or by its data catalog as well.
fn table_name(tokens: &[TokenWithSpan]) -> Option<(Vec<String>, String, Position, &[TokenWithSpan])> {
    let (first, mut rest) = tokens.split_first()?;
    let at = position_of(first);
    let mut parts = vec![name_of(first)?];
    let mut written = first.token.to_string();
    while let Some((part, after)) = rest
        .split_first()
        .filter(|(separator, _)| matches!(separator.token, Token::Period))
        .and_then(|(_, after)| after.split_first())
    {
        parts.push(name_of(part)?);
        written.push('.');
        written.push_str(&part.token.to_string());
        rest = after;
        if parts.len() > 3 {
            return None;
        }
    }
    Some((parts, written, at, rest))
}

/// The name `token` states, or `None` when it is not a name at all.
///
/// Any WORD is a name here, quoted or not: these are the positions the grammar has an
/// identifier in and nothing else, so a name that happens to be spelled like a word the
/// grammar keys elsewhere (`items`, `id`, `date`) is the name it is written as — which is how
/// the sibling readers of a submitted statement read one ([`super::grammar`]).
fn name_of(token: &TokenWithSpan) -> Option<String> {
    match &token.token {
        Token::Word(word) => Some(word.value.clone()),
        _ => None,
    }
}

/// What is left after `keyword`, or `None` when the next token is not that keyword written
/// without quotes.
fn keyword(tokens: &[TokenWithSpan], keyword: Keyword) -> Option<&[TokenWithSpan]> {
    let (first, rest) = tokens.split_first()?;
    is_keyword(first, keyword).then_some(rest)
}

/// Whether `token` is the keyword `keyword` written without quotes.
///
/// A KEYWORD is matched as the keyword the tokenizer recognised, so a name written in quotes
/// is never read as one — which is what tells this reading from the one the sibling
/// diagnostics make of a WORD (`super::grammar::word_is`).
fn is_keyword(token: &TokenWithSpan, keyword: Keyword) -> bool {
    matches!(
        &token.token,
        Token::Word(Word {
            keyword: written,
            quote_style: None,
            ..
        }) if *written == keyword
    )
}

/// The columns a `(<column> <type> [COMMENT '<text>'], …)` list declares, and what is left
/// after it.
///
/// The list is split at the commas of its outermost level by the reader both statements that
/// declare a column list share ([`declaration_list`]), so a type written with the collection
/// and record brackets Hive's own grammar takes (`array<string>`, `map<string,string>`,
/// `struct<city:string>`) and a parameterised one (`decimal(10,2)`) are each one type.
fn declared_columns(tokens: &[TokenWithSpan]) -> Option<(Vec<DeclaredColumn>, &[TokenWithSpan])> {
    let (terms, rest) = declaration_list::column_terms(tokens)?;
    let columns: Vec<DeclaredColumn> = terms
        .iter()
        .map(|term| declared_column(term))
        .collect::<Option<Vec<DeclaredColumn>>>()?;
    Some((columns, rest))
}

/// One declared column, as the catalog records it: the name folded the way the catalog folds
/// a column name, the type text the client wrote lowercased, and the `COMMENT` the
/// declaration closed with.
fn declared_column(term: &[&TokenWithSpan]) -> Option<DeclaredColumn> {
    let (name, after_name) = term.split_first()?;
    let name = name_of(name)?;
    let (declared, comment) = match after_name
        .iter()
        .position(|token| is_keyword(token, Keyword::COMMENT))
    {
        Some(at) => (
            after_name.get(..at)?,
            comment_text(after_name.get(at + 1..)?)?,
        ),
        None => (after_name, None),
    };
    if declared.is_empty() {
        return None;
    }
    let data_type: String = declared
        .iter()
        .map(|token| token.token.to_string())
        .collect();
    Some(DeclaredColumn {
        name: glue_state::fold_name(&name),
        data_type: data_type.to_ascii_lowercase(),
        comment,
    })
}

/// The text a `COMMENT` clause states, or `None` for a clause that states no string literal
/// — or states anything after it, which is no declaration this grammar takes.
fn comment_text(tokens: &[&TokenWithSpan]) -> Option<Option<String>> {
    let [comment] = tokens else {
        return None;
    };
    match &comment.token {
        Token::SingleQuotedString(text) | Token::DoubleQuotedString(text) => {
            Some(Some(text.clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clause sequence is read whole, and text that is not one is not read as one.
    ///
    /// The rows differ along which change the statement makes, how the table name is
    /// qualified and quoted, how many columns it declares, the kind of type each is declared
    /// with, whether a comment closes a declaration — and, for the rows read as nothing,
    /// what makes the text another statement.
    #[test]
    fn the_hive_column_change_is_read_off_the_tokens() {
        /// What one row states: whether it replaces the column list, the name parts, and
        /// each column as its name and declared type.
        type Stated = (bool, Vec<&'static str>, Vec<(&'static str, &'static str)>);

        // (case, the text, what it states)
        let cases: Vec<(&str, &str, Option<Stated>)> = vec![
            (
                "columns appended to a bare name",
                "ALTER TABLE items ADD COLUMNS (label string)",
                Some((false, vec!["items"], vec![("label", "string")])),
            ),
            (
                "a qualified, quoted name and two columns",
                "ALTER TABLE \"shop\".\"items\" ADD COLUMNS (label string, count bigint)",
                Some((
                    false,
                    vec!["shop", "items"],
                    vec![("label", "string"), ("count", "bigint")],
                )),
            ),
            (
                "the whole column list replaced",
                "ALTER TABLE `shop`.`items` REPLACE COLUMNS (id bigint, label string)",
                Some((
                    true,
                    vec!["shop", "items"],
                    vec![("id", "bigint"), ("label", "string")],
                )),
            ),
            (
                "a name qualified by its data catalog as well",
                "ALTER TABLE awsdatacatalog.shop.items REPLACE COLUMNS (id int)",
                Some((
                    true,
                    vec!["awsdatacatalog", "shop", "items"],
                    vec![("id", "int")],
                )),
            ),
            (
                "the statement written in lower case",
                "alter table shop.items add columns (label string)",
                Some((false, vec!["shop", "items"], vec![("label", "string")])),
            ),
            (
                "a parameterised type and a record type",
                "ALTER TABLE items ADD COLUMNS (price decimal(10,2), addr struct<city:string>)",
                Some((
                    false,
                    vec!["items"],
                    vec![("price", "decimal(10,2)"), ("addr", "struct<city:string>")],
                )),
            ),
            (
                "a declaration closed with a comment",
                "ALTER TABLE items ADD COLUMNS (label string COMMENT 'what it is')",
                Some((false, vec!["items"], vec![("label", "string")])),
            ),
            (
                "a trailing statement separator",
                "ALTER TABLE items ADD COLUMNS (label string);",
                Some((false, vec!["items"], vec![("label", "string")])),
            ),
            ("a column list that is never closed", "ALTER TABLE items ADD COLUMNS (label string", None),
            ("a column declared with no type", "ALTER TABLE items ADD COLUMNS (label)", None),
            (
                "another clause of the same statement",
                "ALTER TABLE items SET TBLPROPERTIES ('a' = 'b')",
                None,
            ),
            (
                "the singular form the general parser already reads",
                "ALTER TABLE items ADD COLUMN label string",
                None,
            ),
            ("no statement of this grammar at all", "SELECT 1", None),
        ];

        for (case, sql, expected) in cases {
            let read = read_column_change(sql);
            let actual = read.as_ref().map(|change| {
                (
                    change.replaces,
                    change.name.iter().map(String::as_str).collect::<Vec<&str>>(),
                    change
                        .columns
                        .iter()
                        .map(|column| (column.name.as_str(), column.data_type.as_str()))
                        .collect::<Vec<(&str, &str)>>(),
                )
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The comment a declaration closes with is the text the catalog records for the column.
    #[test]
    fn a_declaration_closes_with_the_comment_the_catalog_records() {
        let change = read_column_change(
            "ALTER TABLE items ADD COLUMNS (label string COMMENT 'what it is', n int)",
        )
        .expect("the statement states a column change");
        let comments: Vec<Option<&str>> = change
            .columns
            .iter()
            .map(|column| column.comment.as_deref())
            .collect();
        assert_eq!(
            comments,
            vec![Some("what it is"), None],
            "case the declared comments: got {comments:?}"
        );
    }
}
