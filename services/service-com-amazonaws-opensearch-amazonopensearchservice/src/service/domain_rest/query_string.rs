// SPDX-License-Identifier: CC0-1.0

//! The QUERY-STRING form of a query: the one syntax a `query_string` clause and the URI form
//! of a search (`?q=…`) both state their clauses in.
//!
//! What it reads is BOUNDED and stated, because a query this plane cannot evaluate must not
//! look like one it did:
//!
//! * a clause is a term, or a phrase in double quotes, asked of the field named before a
//!   colon (`published:2026-01-01`) or of the default field where it names none — which is
//!   every field the index maps unless the request names another (`df`,
//!   `default_field`);
//! * a clause may be made REQUIRED with `+` or PROHIBITED with `-`, and two clauses may be
//!   joined by `AND`, `OR` or `NOT` — written in capitals, as the syntax writes its
//!   operators, with `&&` and `||` as the second spellings of the first two. Clauses the
//!   request joins by nothing are joined by its default operator, which is `OR`;
//! * every other metacharacter of the syntax — grouping, a wildcard or regular expression, a
//!   fuzziness, a proximity, a boost, a bracketed range — is syntax this plane does not read,
//!   and a query carrying one is REFUSED with the engine's own parse failure rather than
//!   answered as though those characters were part of a term.
//!
//! What a clause COMPARES is not decided here: a clause names a field and a text, and the
//! index's own mappings and analysis chains decide how that text is compared against the
//! values the field holds ([`super::selection`]).

use super::*;

/// The query-string parameters the URI form of a search carries: the query itself, the
/// default field its fieldless clauses ask, and the operator its unjoined clauses are
/// joined by.
const QUERY_PARAMETER: &str = "q";
const DEFAULT_FIELD_PARAMETER: &str = "df";
const DEFAULT_OPERATOR_PARAMETER: &str = "default_operator";

/// The engine's error type for a query it could not parse against the index's own fields.
const QUERY_SHARD_EXCEPTION: &str = "query_shard_exception";

/// How many characters one query string may carry.
///
/// The emulator's OWN bound on the reading below: every character is walked once and every
/// clause it produces is asked of every document the scan reaches, so this is the first of
/// the two factors that work is a product of ([`MAX_CLAUSES`] is the second).
const MAX_CHARACTERS: usize = 4_096;

/// How many clauses one query string may state, for the same reason.
const MAX_CLAUSES: usize = 32;

/// The character a clause names its field before.
const FIELD_SEPARATOR: char = ':';

/// The character a phrase is written between.
const PHRASE_QUOTE: char = '"';

/// The prefixes one clause may carry: required, and prohibited.
const REQUIRED_PREFIX: char = '+';
const PROHIBITED_PREFIX: char = '-';

/// The operators two clauses may be joined by, as the syntax writes them.
const AND_OPERATOR: [&str; 2] = ["AND", "&&"];
const OR_OPERATOR: [&str; 2] = ["OR", "||"];
const NOT_OPERATOR: &str = "NOT";

/// The metacharacters of the syntax this plane does not read: grouping, a bracketed range, a
/// fuzziness or proximity, a boost, a regular expression, and an escape.
///
/// A query carrying one is refused rather than read as a term, because every one of them
/// states a question about the documents that dropping it would answer differently.
const UNREAD_SYNTAX: [char; 10] = ['(', ')', '[', ']', '{', '}', '~', '^', '/', '\\'];

/// The metacharacters a TERM may not carry beside those: a wildcard matches terms this
/// reading does not expand, so a term holding one states a question of its own — except for
/// the term that is NOTHING but a wildcard ([`PRESENCE_TERM`]), which asks for the presence
/// of a value rather than for a term matching a pattern.
const UNREAD_TERM_SYNTAX: [char; 2] = ['*', '?'];

/// The term asking for the PRESENCE of a value: `field:*` selects every document holding a
/// value at that field, and `*:*` — which is what a bare `*` is under the default field
/// pattern — selects every document.
const PRESENCE_TERM: &str = "*";

/// Which way the clauses a query string does not join are joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Operator {
    /// Any one of them selects the document — the default.
    Or,
    /// Every one of them has to.
    And,
}

impl Operator {
    /// The operator `named` names, the default where it names none, and a REFUSAL for a value
    /// that is neither: an operator this syntax does not write states a way of joining the
    /// clauses that nothing here can apply, and joining them the other way would answer a
    /// question the caller did not ask.
    pub(super) fn named(named: Option<&str>) -> Result<Self, Box<RtResponse>> {
        let Some(named) = named else {
            return Ok(DEFAULT_OPERATOR);
        };
        let spelled = named.to_ascii_uppercase();
        if AND_OPERATOR.contains(&spelled.as_str()) {
            return Ok(Self::And);
        }
        if OR_OPERATOR.contains(&spelled.as_str()) {
            return Ok(Self::Or);
        }
        Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("Unsupported {DEFAULT_OPERATOR_PARAMETER} [{named}]"),
        )))
    }
}

/// Which way the clauses a request does not join are joined when it names no operator: "OR",
/// the default operator of the query-string syntax.
const DEFAULT_OPERATOR: Operator = Operator::Or;

/// The query the REQUEST states in its query string, or `None` where it states none there.
///
/// Read only where the body states no query of its own, which is what makes the URI form of
/// a search the form a client uses when it sends no body at all.
pub(super) fn requested(request: &Request<'_>) -> Result<Option<Selector>, Box<RtResponse>> {
    let Some(query) = request.parameter(QUERY_PARAMETER) else {
        return Ok(None);
    };
    let default_field = request
        .parameter(DEFAULT_FIELD_PARAMETER)
        .unwrap_or_else(|| ALL_FIELDS.to_string());
    let operator = Operator::named(request.parameter(DEFAULT_OPERATOR_PARAMETER).as_deref())?;
    parsed(&query, &default_field, operator).map(Some)
}

/// The selector the query string `query` states, with the clauses that name no field asking
/// `default_field` and the ones nothing joins joined by `default_operator`.
pub(super) fn parsed(
    query: &str,
    default_field: &str,
    default_operator: Operator,
) -> Result<Selector, Box<RtResponse>> {
    if query.chars().count() > MAX_CHARACTERS {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "a query string may be at most {MAX_CHARACTERS} characters, and this one is {}",
                query.chars().count()
            ),
        )));
    }
    if query.contains(UNREAD_SYNTAX) {
        return Err(unparsed(query));
    }
    let mut required: Vec<Selector> = Vec::new();
    let mut forbidden: Vec<Selector> = Vec::new();
    let mut optional: Vec<Selector> = Vec::new();
    // Where the clause read before this one went, and whether an operator or a prefix put it
    // there — an `AND` or an `OR` joins the clause BEFORE it as well as the one after, and
    // may only move one that nothing but the default placed.
    let mut last: Option<(Occur, bool)> = None;
    // The place the NEXT clause takes, where an operator has already decided it.
    let mut next: Option<Occur> = None;
    for word in words(query)? {
        match word {
            Word::And | Word::Or | Word::Not => {
                let joined = match word {
                    Word::And => Occur::Required,
                    Word::Or => Occur::Optional,
                    _ => Occur::Forbidden,
                };
                // `NOT` states where the clause AFTER it goes and nothing about the one
                // before it.
                if joined != Occur::Forbidden {
                    if let Some((placed, pinned)) = last {
                        if !pinned && placed != joined {
                            moved(placed, joined, &mut required, &mut forbidden, &mut optional);
                            last = Some((joined, true));
                        }
                    }
                }
                next = Some(joined);
            }
            Word::Clause { prefix, text } => {
                let (field, text, phrase) = clause_of(&text, default_field)?;
                let placed = prefix.or(next.take());
                let occur = placed.unwrap_or(match default_operator {
                    Operator::Or => Occur::Optional,
                    Operator::And => Occur::Required,
                });
                let clause = clause_selector(&field, &text, phrase);
                match occur {
                    Occur::Required => required.push(clause),
                    Occur::Forbidden => forbidden.push(clause),
                    Occur::Optional => optional.push(clause),
                }
                last = Some((occur, placed.is_some()));
            }
        }
    }
    // Composed exactly as a `bool` composing the same clauses is, so one query written in
    // either form selects the same documents.
    let mut composed = vec![Selector::All_(required), Selector::None_(forbidden)];
    if !optional.is_empty() {
        composed.push(Selector::Any(optional));
    }
    Ok(Selector::All_(composed))
}

/// Where one clause of a query string goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Occur {
    /// Every document it selects, and no other.
    Required,
    /// No document it selects.
    Forbidden,
    /// Any one of these selects the document.
    Optional,
}

/// Move the clause read last from where it was placed to where an operator puts it.
fn moved(
    from: Occur,
    to: Occur,
    required: &mut Vec<Selector>,
    forbidden: &mut Vec<Selector>,
    optional: &mut Vec<Selector>,
) {
    let taken = match from {
        Occur::Required => required.pop(),
        Occur::Forbidden => forbidden.pop(),
        Occur::Optional => optional.pop(),
    };
    let Some(taken) = taken else {
        return;
    };
    match to {
        Occur::Required => required.push(taken),
        Occur::Forbidden => forbidden.push(taken),
        Occur::Optional => optional.push(taken),
    }
}

/// One word of a query string: a clause, or an operator joining two of them.
#[derive(Debug)]
enum Word {
    Clause {
        /// Where the clause's own prefix puts it, where it carries one.
        prefix: Option<Occur>,
        text: String,
    },
    And,
    Or,
    Not,
}

/// The words `query` is written as: its clauses and operators, split on the spaces between
/// them, with a phrase in quotes kept whole however many spaces it holds.
fn words(query: &str) -> Result<Vec<Word>, Box<RtResponse>> {
    let mut words: Vec<Word> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    // One pass, so what the reading costs is the query's own length.
    for character in query.chars() {
        match character {
            PHRASE_QUOTE => {
                quoted = !quoted;
                current.push(character);
            }
            space if space.is_whitespace() && !quoted => {
                take(&mut current, &mut words)?;
            }
            other => current.push(other),
        }
    }
    take(&mut current, &mut words)?;
    Ok(words)
}

/// Take what has been read as one word, where anything has been.
fn take(current: &mut String, words: &mut Vec<Word>) -> Result<(), Box<RtResponse>> {
    if current.is_empty() {
        return Ok(());
    }
    let word = ::std::mem::take(current);
    if AND_OPERATOR.contains(&word.as_str()) {
        words.push(Word::And);
        return Ok(());
    }
    if OR_OPERATOR.contains(&word.as_str()) {
        words.push(Word::Or);
        return Ok(());
    }
    if word == NOT_OPERATOR {
        words.push(Word::Not);
        return Ok(());
    }
    if words.len() >= MAX_CLAUSES {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("a query string may state at most {MAX_CLAUSES} clauses"),
        )));
    }
    // The prefix, where the clause carries one, taken off the text it qualifies.
    let (prefix, text) = if let Some(text) = word.strip_prefix(REQUIRED_PREFIX) {
        (Some(Occur::Required), text.to_string())
    } else if let Some(text) = word.strip_prefix(PROHIBITED_PREFIX) {
        (Some(Occur::Forbidden), text.to_string())
    } else {
        (None, word)
    };
    if !text.is_empty() {
        words.push(Word::Clause { prefix, text });
    }
    Ok(())
}

/// The clause one field, text and phrase flag state: the presence of a value where the text
/// asks for nothing but that ([`PRESENCE_TERM`]), and otherwise the question the field's own
/// mapping and analysis chain decide ([`field_clause`]).
fn clause_selector(field: &str, text: &str, phrase: bool) -> Selector {
    if text != PRESENCE_TERM {
        return field_clause(field, text, phrase);
    }
    if field == ALL_FIELDS {
        return Selector::All;
    }
    Selector::Exists {
        field: field.to_string(),
    }
}

/// The field, the text and whether it is a PHRASE that one clause states — the field it
/// names before a colon, or `default_field` where it names none.
fn clause_of(written: &str, default_field: &str) -> Result<(String, String, bool), Box<RtResponse>> {
    // A colon inside a phrase is part of the phrase, and the one that names a field comes
    // before the text — so the field is read off the part before the FIRST colon, and only
    // where that part is a field name rather than the start of a phrase.
    let (field, text) = match written.split_once(FIELD_SEPARATOR) {
        Some((field, text))
            if !field.is_empty()
                && !field.starts_with(PHRASE_QUOTE)
                && !field.contains(PHRASE_QUOTE) =>
        {
            (field.to_string(), text.to_string())
        }
        _ => (default_field.to_string(), written.to_string()),
    };
    let phrase = text.len() >= 2 && text.starts_with(PHRASE_QUOTE) && text.ends_with(PHRASE_QUOTE);
    let text = text.trim_matches(PHRASE_QUOTE).to_string();
    if text != PRESENCE_TERM && (text.contains(UNREAD_TERM_SYNTAX) || text.contains(PHRASE_QUOTE))
    {
        return Err(unparsed(written));
    }
    Ok((field, text, phrase))
}

/// The refusal a query string carrying syntax this plane does not read earns: the engine's
/// own failure to parse a query against an index's fields, naming the query it could not
/// parse.
fn unparsed(query: &str) -> Box<RtResponse> {
    Box::new(error(
        400,
        QUERY_SHARD_EXCEPTION,
        &format!("Failed to parse query [{query}]"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clause tree a query string states, rendered as the text a test can compare: the
    /// place each clause takes, the field it asks and the text it asks for.
    fn rendered(query: &str, default_field: &str, operator: Operator) -> String {
        let select = parsed(query, default_field, operator)
            .map_err(|_| ())
            .unwrap_or_else(|()| panic!("case {query:?}: expected the query to be read"));
        format!("{select:?}")
    }

    /// RULE: a query string states its clauses as terms and phrases, each asked of the field
    /// it names or of the default field, placed by the prefixes and operators it carries.
    #[test]
    fn a_query_string_states_its_clauses() {
        // (the case, the query, the default field, the operator, what the tree must hold)
        let rows: &[(&str, &str, &str, Operator, &[&str])] = &[
            (
                "a term asked of a named field",
                "published:2026-01-01",
                ALL_FIELDS,
                Operator::Or,
                &["Any([Analyzed { field: \"published\", text: \"2026-01-01\""],
            ),
            (
                "a term asked of no field",
                "red",
                ALL_FIELDS,
                Operator::Or,
                &["Any([AnyField { pattern: \"*\", text: \"red\""],
            ),
            (
                "a term asked of the default field the request named",
                "red",
                "title",
                Operator::Or,
                &["Any([Analyzed { field: \"title\", text: \"red\""],
            ),
            (
                "a phrase",
                "title:\"red apple\"",
                ALL_FIELDS,
                Operator::Or,
                &["text: \"red apple\", phrase: true"],
            ),
            (
                "a required and a prohibited clause",
                "+title:red -title:green",
                ALL_FIELDS,
                Operator::Or,
                &[
                    "All_([Analyzed { field: \"title\", text: \"red\"",
                    "None_([Analyzed { field: \"title\", text: \"green\"",
                ],
            ),
            (
                "two clauses joined by AND",
                "title:red AND body:fruit",
                ALL_FIELDS,
                Operator::Or,
                &[
                    "All_([Analyzed { field: \"title\", text: \"red\", phrase: false }, \
                     Analyzed { field: \"body\", text: \"fruit\"",
                ],
            ),
            (
                "two clauses joined by nothing under the AND default",
                "title:red body:fruit",
                ALL_FIELDS,
                Operator::And,
                &[
                    "All_([Analyzed { field: \"title\", text: \"red\", phrase: false }, \
                     Analyzed { field: \"body\", text: \"fruit\"",
                ],
            ),
            (
                "a clause after NOT",
                "title:red NOT title:green",
                ALL_FIELDS,
                Operator::Or,
                &["None_([Analyzed { field: \"title\", text: \"green\""],
            ),
        ];
        for (case, query, default_field, operator, held) in rows {
            let tree = rendered(query, default_field, *operator);
            for expected in *held {
                assert!(
                    tree.contains(expected),
                    "case {case:?}: expected the tree to hold {expected:?}, got {tree}"
                );
            }
        }
    }

    /// RULE: a query string carrying syntax this plane does not read is refused rather than
    /// read as a term.
    #[test]
    fn syntax_this_plane_does_not_read_is_refused() {
        // (the case, the query)
        let rows: &[(&str, &str)] = &[
            ("a grouping", "(title:red OR title:green)"),
            ("a bracketed range", "published:[2026-01-01 TO 2026-12-31]"),
            ("a fuzziness", "title:red~2"),
            ("a boost", "title:red^2"),
            ("a regular expression", "title:/re[dk]/"),
            ("a wildcard term", "title:re*"),
            ("an escape", "title:re\\-d"),
        ];
        for (case, query) in rows {
            let answered = parsed(query, ALL_FIELDS, Operator::Or);
            assert!(
                answered.is_err(),
                "case {case:?}: expected the query to be refused, got {answered:?}"
            );
        }
        // A query longer than one may be is refused too, rather than walked.
        let long = "a ".repeat(MAX_CHARACTERS);
        assert!(
            parsed(&long, ALL_FIELDS, Operator::Or).is_err(),
            "case \"a query longer than one may be\": expected a refusal"
        );
    }

    /// RULE: a term of nothing but the wildcard asks for the PRESENCE of a value: on a named
    /// field every document holding one, and on the default field pattern every document.
    #[test]
    fn a_term_of_nothing_but_the_wildcard_asks_for_a_value() {
        // (the case, the query, the default field, what the tree must hold)
        let rows: &[(&str, &str, &str, &str)] = &[
            ("every document", "*", ALL_FIELDS, "All_([All_([]), None_([]), Any([All])])"),
            ("every document, written in full", "*:*", ALL_FIELDS, "Any([All])"),
            (
                "every document holding a value at one field",
                "published:*",
                ALL_FIELDS,
                "Exists { field: \"published\" }",
            ),
            (
                "every document holding a value at the default field",
                "*",
                "title",
                "Exists { field: \"title\" }",
            ),
        ];
        for (case, query, default_field, held) in rows {
            let tree = rendered(query, default_field, Operator::Or);
            assert!(
                tree.contains(held),
                "case {case:?}: expected the tree to hold {held:?}, got {tree}"
            );
        }
    }

    /// RULE: a query string this reading cannot make a term of is answered by the clause the
    /// rule gives it, not by walking off the end of the input — and what one query may state
    /// is bounded.
    #[test]
    fn malformed_and_truncated_input_is_answered() {
        // (the case, the query, the tree it must come to, or `None` for a refusal)
        let rows: &[(&str, &str, Option<&str>)] = &[
            // A query stating no clause at all selects every document, as a body stating no
            // query does.
            ("an empty query", "", Some("All_([All_([]), None_([])])")),
            // A clause whose text is empty asks its field for an empty term, which no
            // document's value analyzes to.
            (
                "a field with no text",
                "title:",
                Some("Analyzed { field: \"title\", text: \"\", phrase: false }"),
            ),
            // A colon with nothing before it names no field, so the whole token is the text
            // the default field is asked for.
            (
                "a text with no field",
                ":red",
                Some("AnyField { pattern: \"*\", text: \":red\", phrase: false }"),
            ),
            // A phrase nothing closes is not a phrase: the quote is dropped and the token is
            // read as the term it holds.
            (
                "an unterminated phrase",
                "title:\"red",
                Some("Analyzed { field: \"title\", text: \"red\", phrase: false }"),
            ),
            (
                "an empty phrase",
                "title:\"\"",
                Some("Analyzed { field: \"title\", text: \"\", phrase: true }"),
            ),
            // An operator joins two clauses; one standing alone joins nothing, so the query
            // states no clause.
            ("an operator alone", "AND", Some("All_([All_([]), None_([])])")),
            (
                "an operator with nothing after it",
                "title:red AND",
                Some("All_([Analyzed { field: \"title\", text: \"red\", phrase: false }]"),
            ),
            (
                "two operators in a row, the second deciding",
                "title:red AND OR title:green",
                Some("Any([Analyzed { field: \"title\", text: \"green\", phrase: false }]"),
            ),
            // A prefix qualifies a clause; one with no clause after it qualifies nothing.
            ("a prefix with nothing after it", "+", Some("All_([All_([]), None_([])])")),
            ("nothing but separators", "   ", Some("All_([All_([]), None_([])])")),
            (
                "a colon alone",
                ":",
                Some("AnyField { pattern: \"*\", text: \":\", phrase: false }"),
            ),
            // Only the FIRST colon names the field; the rest is part of the text, which is
            // what carries an ISO instant's own colons.
            (
                "more colons than one clause names",
                "title:a:b",
                Some("Analyzed { field: \"title\", text: \"a:b\", phrase: false }"),
            ),
            // And what one query may state is bounded: a query longer than the reading takes,
            // or stating more clauses than it evaluates, is refused rather than walked.
            ("a query longer than one may be", "", None),
            ("more clauses than one query may state", "", None),
        ];
        let long = "a ".repeat(MAX_CHARACTERS);
        let many = "a ".repeat(MAX_CLAUSES.saturating_add(10));
        for (case, query, expected) in rows {
            let query = match *case {
                "a query longer than one may be" => long.as_str(),
                "more clauses than one query may state" => many.as_str(),
                _ => query,
            };
            let answered = parsed(query, ALL_FIELDS, Operator::Or);
            match expected {
                None => assert!(
                    answered.is_err(),
                    "case {case:?}: expected a refusal, got {answered:?}"
                ),
                Some(held) => {
                    let tree = answered
                        .map_err(|_| ())
                        .unwrap_or_else(|()| panic!("case {case:?}: expected the query to be read"));
                    let tree = format!("{tree:?}");
                    assert!(
                        tree.contains(held),
                        "case {case:?}: expected the tree to hold {held:?}, got {tree}"
                    );
                }
            }
        }
    }

    /// RULE: the operator a request names its default by is the one the clauses are joined
    /// with, and a value that names neither of the two the syntax writes is refused rather
    /// than answered under the other one.
    #[test]
    fn the_default_operator_a_request_names_is_the_one_it_is_read_under() {
        // (the case, the value named, the operator it names, or `None` for a refusal)
        let rows: &[(&str, Option<&str>, Option<Operator>)] = &[
            ("none at all", None, Some(Operator::Or)),
            ("AND", Some("AND"), Some(Operator::And)),
            ("OR", Some("OR"), Some(Operator::Or)),
            ("and, in lower case", Some("and"), Some(Operator::And)),
            ("the second spelling of AND", Some("&&"), Some(Operator::And)),
            ("the second spelling of OR", Some("||"), Some(Operator::Or)),
            ("an operator this syntax does not write", Some("XOR"), None),
            ("a value that is no operator", Some("maybe"), None),
            ("an empty value", Some(""), None),
        ];
        for (case, named, expected) in rows {
            let answered = Operator::named(*named);
            match expected {
                Some(expected) => assert_eq!(
                    answered.map_err(|_| ()),
                    Ok(*expected),
                    "case {case:?}: expected {expected:?}"
                ),
                None => assert!(
                    answered.is_err(),
                    "case {case:?}: expected the value to be refused"
                ),
            }
        }
    }
}
