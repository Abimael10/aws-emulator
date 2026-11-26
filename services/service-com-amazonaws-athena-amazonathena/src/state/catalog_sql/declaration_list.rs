// SPDX-License-Identifier: CC0-1.0

//! The TERMS a parenthesised declaration list of Hive's DDL holds.
//!
//! Two statements of that DDL declare a column list in parentheses — the one that DEFINES a
//! table and the one that CHANGES its columns — and a column is declared with a type that may
//! itself carry a parenthesised parameter list or an angle-bracketed element list
//! (`decimal(10,2)`, `map<string,string>`, `struct<tags:array<string>>`). So what separates
//! two columns is the comma at the OUTERMOST level of those brackets, and that is the one rule
//! this module holds, for both readers of such a list ([`super::hive_column_types`] and
//! [`super::hive_alter_table`]) and under one bound on how many columns either will walk.
//!
//! The reading is on the TOKENS, through the same lexer every other reading of a submitted
//! statement here goes through ([`super::grammar::significant_tokens`]).

use ::sqlparser::tokenizer::{Token, TokenWithSpan};

/// How many columns one declaration list this reader will walk may carry: the bound on the
/// work one submitted statement can ask of it.
pub(super) const MAX_DECLARED_COLUMNS: usize = 1_024;

/// The terms of the declaration list `tokens` OPENS with, and what is left after the list.
///
/// One term per declared column, each the tokens that column was declared with and without the
/// comma that closed it; `None` for tokens that do not open a list, for a list that is never
/// closed, and for one carrying more columns than this reader walks.
pub(super) fn column_terms(
    tokens: &[TokenWithSpan],
) -> Option<(Vec<Vec<&TokenWithSpan>>, &[TokenWithSpan])> {
    let (open, mut rest) = tokens.split_first()?;
    if !matches!(open.token, Token::LParen) {
        return None;
    }
    let mut terms: Vec<Vec<&TokenWithSpan>> = Vec::new();
    let mut term: Vec<&TokenWithSpan> = Vec::new();
    let mut depth = 0usize;
    loop {
        let (token, after) = rest.split_first()?;
        rest = after;
        match &token.token {
            Token::LParen | Token::Lt => depth += 1,
            Token::ShiftLeft => depth += 2,
            Token::RParen | Token::Gt if depth > 0 => depth -= 1,
            Token::ShiftRight if depth > 1 => depth -= 2,
            // The `)` that closes the list, which is where the declaration ends.
            Token::RParen => {
                terms.push(term);
                return (terms.len() <= MAX_DECLARED_COLUMNS).then_some((terms, rest));
            }
            Token::Comma if depth == 0 => {
                terms.push(::std::mem::take(&mut term));
                if terms.len() > MAX_DECLARED_COLUMNS {
                    return None;
                }
                continue;
            }
            _ => {}
        }
        term.push(token);
    }
}

#[cfg(test)]
mod tests {
    use super::super::grammar::significant_tokens;
    use super::*;

    /// The terms of one list, as the text each was declared with, and what is left after it.
    fn read(sql: &str) -> Option<(Vec<String>, usize)> {
        let tokens = significant_tokens(sql);
        let (terms, rest) = column_terms(&tokens)?;
        Some((
            terms
                .iter()
                .map(|term| {
                    term.iter()
                        .map(|token| token.token.to_string())
                        .collect::<Vec<String>>()
                        .join(" ")
                })
                .collect(),
            rest.len(),
        ))
    }

    /// A list is split at the commas of its OUTERMOST level, and text that opens no list — or
    /// never closes one — is no list at all.
    ///
    /// The rows differ along how many columns the list declares, the brackets their types
    /// carry, and what follows the list.
    #[test]
    fn a_declaration_list_is_split_at_its_outermost_commas() {
        /// What one row states: the terms the list holds, and how many tokens follow it.
        type Split = Option<(Vec<&'static str>, usize)>;

        // (case, the text, the terms and how many tokens follow the list)
        let cases: Vec<(&str, &str, Split)> = vec![
            ("one column", "(id int)", Some((vec!["id int"], 0))),
            (
                "two columns, with tokens after the list",
                "(id int, label string) LOCATION 's3://b/p/'",
                Some((vec!["id int", "label string"], 2)),
            ),
            (
                "a parameterised type, whose own comma separates no column",
                "(price decimal(10,2))",
                Some((vec!["price decimal ( 10 , 2 )"], 0)),
            ),
            (
                "an element list, closing with two brackets at once",
                "(payload struct<tags:array<string>>, n int)",
                Some((vec!["payload struct < tags : array < string >>", "n int"], 0)),
            ),
            ("text that opens no list", "LOCATION 's3://b/p/'", None),
            ("a list that is never closed", "(id int", None),
        ];

        for (case, sql, expected) in cases {
            let actual = read(sql);
            let expected = expected.map(|(terms, rest)| {
                (
                    terms.iter().map(|term| (*term).to_string()).collect::<Vec<String>>(),
                    rest,
                )
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
