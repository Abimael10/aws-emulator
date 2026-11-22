// SPDX-License-Identifier: CC0-1.0

//! `GetTables` whose `Expression` is not a regular expression at all: the refusal, and what
//! it says about the caller's own value.
//!
//! RULE: a listing compiles its `Expression` and, when the value is no pattern, RELAYS what
//! the engine compiling it said about it — the fault, the index into the CALLER's own
//! expression the parse stopped at, the expression on a line of its own and a caret under
//! that index — under the operation's modelled `InvalidInputException` at HTTP 400. Nothing
//! of the matcher behind the listing reaches the message: not the engine this emulator
//! compiles with, and not the expression the catalog expands the caller's wildcards into,
//! whose character positions are not the caller's.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: the CLASS of fault (an impossible
//! range, an unclosed class, an unclosed group, a closing parenthesis nothing opened, a
//! dangling quantifier, a backwards counted repetition, an escape that stops mid-sequence),
//! where in the expression the fault sits (its first character, inside it, past its last),
//! whether the expression also carries a WILDCARD before the fault (which the catalog
//! expands, so the index has to be placed back in the caller's text), and whether the
//! characters before the fault are single- or multi-byte.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, table_names};

/// The database the listings in this suite are served from.
const DATABASE: &str = "expression_fault_db";

/// The tables it holds, so a listing that is NOT refused has something to select from.
const TABLES: [&str; 3] = ["events", "events_raw", "orders"];

/// A database holding [`TABLES`].
async fn catalog(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    for table in TABLES {
        ok(
            router,
            "CreateTable",
            &json!({ "DatabaseName": DATABASE, "TableInput": { "Name": table } }),
        )
        .await;
    }
}

/// One `GetTables` call narrowed by `expression`.
async fn listed(
    router: &MultiServiceRouter,
    expression: &str,
) -> (u16, Value) {
    call(
        router,
        "GetTables",
        &json!({ "DatabaseName": DATABASE, "Expression": expression }),
    )
    .await
}

/// A value that is no regular expression is refused, and the refusal states the fault, the
/// index into the caller's own expression and the caret under it.
#[tokio::test]
async fn an_expression_that_is_no_pattern_is_refused_with_the_fault_placed_in_it() {
    let router = emulator();
    catalog(&router).await;

    // (case, the Expression sent, the whole Message the refusal carries)
    let cases: [(&str, &str, &str); 11] = [
        (
            "a character range whose bounds run backwards",
            "t[b-a]",
            "Illegal character range near index 4\nt[b-a]\n    ^",
        ),
        (
            "a backwards range further inside the expression",
            "ev[z-a]x",
            "Illegal character range near index 5\nev[z-a]x\n     ^",
        ),
        (
            "a backwards range at the expression's first class",
            "[9-0]",
            "Illegal character range near index 3\n[9-0]\n   ^",
        ),
        (
            "a character class the expression never closes",
            "ev[",
            "Unclosed character class near index 2\nev[\n  ^",
        ),
        (
            // The fault is one past the last character, so there is nothing to point at
            // and the statement carries no caret line.
            "a group the expression never closes",
            "(events",
            "Unclosed group near index 7\n(events",
        ),
        (
            "a closing parenthesis nothing opened",
            "events)",
            "Unmatched closing ')' near index 6\nevents)\n      ^",
        ),
        (
            "a quantifier with nothing to repeat",
            "+events",
            "Dangling meta character '+' near index 0\n+events\n^",
        ),
        (
            "a counted repetition whose bounds run backwards",
            "e{3,2}",
            "Illegal repetition range near index 5\ne{3,2}\n     ^",
        ),
        (
            "an escape the expression stops in the middle of",
            "ev\\",
            "Illegal/unsupported escape sequence near index 2\nev\\\n  ^",
        ),
        (
            // `*` is this language's wildcard, which the catalog EXPANDS before it
            // compiles: the index still names the character of the caller's own text.
            "a wildcard before the fault",
            "*[b-a]",
            "Illegal character range near index 4\n*[b-a]\n    ^",
        ),
        (
            "multi-byte characters before the fault",
            "é[b-a]",
            "Illegal character range near index 4\né[b-a]\n    ^",
        ),
    ];

    for (case, expression, expected) in cases {
        let (status, body) = listed(&router, expression).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {expression:?} is no pattern and must be refused, got {status} \
             with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// The relayed statement carries nothing of the matcher behind the listing: no engine of
/// this emulator's and no expression it expanded the caller's into.
///
/// The rows are the same classes of fault, each sent with a wildcard that the catalog
/// expands and with the anchoring a whole-name match needs — exactly what a message must
/// not disclose.
#[tokio::test]
async fn the_refusal_discloses_no_matcher_of_its_own() {
    let router = emulator();
    catalog(&router).await;

    for expression in ["t[b-a]", "*[b-a]", "ev[", "(events", "+events", "e{3,2}"] {
        let (status, body) = listed(&router, expression).await;
        assert_eq!(
            status, 400,
            "case {expression:?}: must be refused, got {status} with {body}"
        );
        let message = error_message(&body);
        // The expanded, anchored text the matcher is built from, and the name of the
        // parser that read it, are the implementation — never the caller's business.
        for leaked in ["^(?:", ")$", ".*", "regex", "parse error"] {
            assert!(
                !message.contains(leaked),
                "case {expression:?}: the refusal must not disclose {leaked:?}, got {message:?}"
            );
        }
        // What it DOES carry is the caller's own expression, once per line of the layout.
        assert!(
            message.contains(expression),
            "case {expression:?}: the refusal states the expression it was sent, got \
             {message:?}"
        );
    }
}

/// An `Expression` that IS a regular expression but that this catalog can build no matcher
/// for carries no fault to place, so the refusal states the value and nothing of the
/// matcher.
#[tokio::test]
async fn an_expression_no_matcher_can_be_built_for_states_no_index() {
    let router = emulator();
    catalog(&router).await;

    // A pattern whose compiled matcher is larger than the listing compiles: it parses, so
    // there is no position inside it to report.
    let oversized = format!("(?:{}){{200}}", "[a-z0-9]{200}");
    let (status, body) = listed(&router, &oversized).await;
    assert_eq!(
        status, 400,
        "case an oversized matcher: must be refused, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("InvalidInputException"),
        "case an oversized matcher: expected InvalidInputException, got {} in {body}",
        error_code(&body)
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        format!("Expression {oversized} is not a pattern this catalog can match"),
        "case an oversized matcher: unexpected wording, got {message:?}"
    );
    assert!(
        !message.contains("near index"),
        "case an oversized matcher: there is no fault to place, got {message:?}"
    );
}

/// The refusals are about the VALUE and not about the listing: an expression that is a
/// pattern still selects the tables whose whole name it matches, and one carrying a
/// wildcard still has that wildcard expanded.
#[tokio::test]
async fn an_expression_that_is_a_pattern_still_selects_tables() {
    let router = emulator();
    catalog(&router).await;

    // (the Expression, the names it selects)
    for (expression, expected) in [
        ("events", vec!["events"]),
        ("events*", vec!["events", "events_raw"]),
        ("[a-z_]*", vec!["events", "events_raw", "orders"]),
        ("e{1,2}vents", vec!["events"]),
    ] {
        let (status, page) = listed(&router, expression).await;
        assert_eq!(
            status, 200,
            "case {expression:?}: a pattern must be served, got {status} with {page}"
        );
        assert_eq!(
            table_names(&page),
            expected,
            "case {expression:?}: expected {expected:?}, got {:?}",
            table_names(&page)
        );
    }
}
