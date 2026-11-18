// SPDX-License-Identifier: CC0-1.0

//! Probes of the warehouse-type READER itself, driven DIRECTLY against
//! `state::warehouse_types` rather than over the wire.
//!
//! The suites beside this one assert what a caller sees: a statement submitted
//! over HTTP, and the column metadata and cells the service reports for it.
//! What is asserted here is the reading underneath them — the input syntax each
//! of these types accepts, and what it does with text that is not a value of
//! the type — over inputs a wire test cannot reach as sharply: text truncated
//! mid-value, text carrying no value at all, and text longer than the engine
//! holds a value in, which no statement can carry because the engine refuses it
//! first.
//!
//! The warehouse's own input syntax for these types is read HERE, by hand,
//! because it is the data warehouse's text format and no crates.io crate reads
//! it: the PostgreSQL client crates decode these types from the BINARY wire
//! form of a connection this emulator does not speak. So the reader is bounded
//! the way a hand-written reader of caller input has to be — one forward pass,
//! no recursion, no nesting to descend, and an explicit ceiling on the text it
//! reads at all — and that is what these probes pin.

use service_com_amazonaws_redshiftdata_redshiftdata::state::warehouse_types::{
    parse_money, GeometricType, GEOMETRIC_TYPES,
};

/// The ceiling the reader refuses a text past, in bytes: the engine's own value
/// ceiling, which is the longest string it holds.
const VALUE_CEILING: usize = 1024 * 1024;

/// Every spelling of a value each type accepts, and the printed form it reads
/// to: one row per (type, input).
const ACCEPTED: &[(&str, &str, &str)] = &[
    // A point: with and without the parentheses the syntax makes optional, and
    // with whitespace between the parts.
    ("point", "(3,4)", "(3.0,4.0)"),
    ("point", "3,4", "(3.0,4.0)"),
    ("point", " ( 3 , 4 ) ", "(3.0,4.0)"),
    ("point", "(-1.5,2.25)", "(-1.5,2.25)"),
    ("point", "(1e2,0.5)", "(100.0,0.5)"),
    // A line segment: its two endpoints, in the order they are named.
    ("lseg", "[(1,2),(8,9)]", "[(1.0,2.0),(8.0,9.0)]"),
    ("lseg", "((1,2),(8,9))", "[(1.0,2.0),(8.0,9.0)]"),
    ("lseg", "1,2,8,9", "[(1.0,2.0),(8.0,9.0)]"),
    // A rectangle: the same pair of corners whichever order they are named in.
    ("box", "((2,3),(9,7))", "(9.0,7.0),(2.0,3.0)"),
    ("box", "((9,3),(2,7))", "(9.0,7.0),(2.0,3.0)"),
    ("box", "2,3,9,7", "(9.0,7.0),(2.0,3.0)"),
    // A path: open or closed, as its brackets said, and reported as the
    // warehouse printed it.
    ("path", "[(1,1),(4,2),(6,5)]", "[(1,1),(4,2),(6,5)]"),
    ("path", "((1,1),(4,2),(6,5))", "((1,1),(4,2),(6,5))"),
    ("path", "[(1,1),(4,2)]", "[(1,1),(4,2)]"),
    // A polygon: closed by definition.
    ("polygon", "((0,0),(4,0),(4,3),(0,3))", "((0.0,0.0),(4.0,0.0),(4.0,3.0),(0.0,3.0))"),
    ("polygon", "(0,0),(4,0)", "((0.0,0.0),(4.0,0.0))"),
    // A circle: a centre and a radius, including a radius of zero.
    ("circle", "<(5,6),4>", "<(5.0,6.0),4.0>"),
    ("circle", "((5,6),4)", "<(5.0,6.0),4.0>"),
    ("circle", "<(5,6),0>", "<(5.0,6.0),0.0>"),
];

/// Each type reads every spelling of a value it accepts, and answers that
/// value's own printed form.
#[test]
fn every_accepted_spelling_reads_to_the_values_printed_form() {
    for (type_name, input, expected) in ACCEPTED {
        let kind = GeometricType::named(type_name)
            .unwrap_or_else(|| panic!("case {type_name}: {type_name} is one of these types"));
        let read = kind.parse(input);
        assert_eq!(
            read.as_deref(),
            Ok(*expected),
            "case {type_name} {input:?}: expected {expected:?}, got {read:?}"
        );
    }
}

/// A printed form reads back to ITSELF: what a caller gets back is a value of
/// the type, so reading it again is the same value.
#[test]
fn a_printed_form_reads_back_to_itself() {
    for (type_name, input, expected) in ACCEPTED {
        let kind = GeometricType::named(type_name)
            .unwrap_or_else(|| panic!("case {type_name}: {type_name} is one of these types"));
        let again = kind.parse(expected);
        assert_eq!(
            again.as_deref(),
            Ok(*expected),
            "case {type_name} {input:?}: the printed form {expected:?} must read back as \
             itself, got {again:?}"
        );
    }
}

/// Text that is not a value of the type is refused, and the refusal names the
/// type and the text — for text carrying no value, text carrying the wrong
/// number of coordinates, text truncated mid-value, and a radius that is not a
/// length.
#[test]
fn text_that_is_not_a_value_of_the_type_is_refused() {
    let refused: &[(&str, &str)] = &[
        // No coordinate at all, and text that is not a number.
        ("point", ""),
        ("point", "   "),
        ("point", "origin"),
        ("point", "(x,y)"),
        ("point", "(3,4"),
        ("point", "(,)"),
        // The wrong number of coordinates, including half a point.
        ("point", "(3)"),
        ("point", "(1,2,3)"),
        ("lseg", "[(1,2)]"),
        ("lseg", "[(1,2),(8,9),(4,4)]"),
        ("box", "((2,3))"),
        ("path", "[(1,1),(4]"),
        ("polygon", "((0,0),(4))"),
        ("circle", "<(5,6)>"),
        // A radius is a length.
        ("circle", "<(5,6),-4>"),
        // Truncated mid-number and mid-exponent.
        ("point", "(1,"),
        ("point", "(1,2e)"),
        ("lseg", "[(1,1),(2,"),
        // An infinity is no position in the plane.
        ("point", "(inf,4)"),
    ];

    for (type_name, input) in refused {
        let kind = GeometricType::named(type_name)
            .unwrap_or_else(|| panic!("case {type_name}: {type_name} is one of these types"));
        let read = kind.parse(input);
        assert_eq!(
            read,
            Err(format!(
                "invalid input syntax for type {type_name}: \"{input}\""
            )),
            "case {type_name} {input:?}: expected the type's own refusal, got {read:?}"
        );
    }
}

/// Text longer than the engine holds a value in is refused BEFORE it is read,
/// with a refusal that carries the ceiling rather than the text.
///
/// Each type is driven, because the ceiling belongs to the reader and not to
/// one of them; the amount of money is driven too, and so is the longest text
/// that is still inside the ceiling, which is read rather than refused.
#[test]
fn text_past_the_engines_value_ceiling_is_refused_before_it_is_read() {
    // One point per 6 characters, so a text of more than the ceiling is built
    // out of values the reader would otherwise accept: what refuses it is its
    // LENGTH and nothing about its syntax.
    let oversized = "(1,1),".repeat(VALUE_CEILING / 6 + 1);
    assert!(
        oversized.len() > VALUE_CEILING,
        "the oversized case must really be past the ceiling, got {} bytes",
        oversized.len()
    );

    for kind in GEOMETRIC_TYPES.iter().copied() {
        let type_name = kind.type_name();
        let read = kind.parse(&oversized);
        assert_eq!(
            read,
            Err(format!(
                "string or blob too big: a value of type {type_name} may not exceed \
                 {VALUE_CEILING} bytes"
            )),
            "case {type_name}: a text of {} bytes must be refused for its length",
            oversized.len()
        );
    }

    let read = parse_money(&oversized);
    assert_eq!(
        read,
        Err(format!(
            "string or blob too big: a value of type money may not exceed {VALUE_CEILING} bytes"
        )),
        "case money: a text of {} bytes must be refused for its length, got {read:?}",
        oversized.len()
    );

    // And the ceiling is a ceiling, not a smaller bound: a path exactly at it
    // is read, however many points it carries.
    let at_ceiling = format!(
        "[{}]",
        "(1,1),".repeat((VALUE_CEILING - 2) / 6).trim_end_matches(',')
    );
    assert!(
        at_ceiling.len() <= VALUE_CEILING,
        "the at-ceiling case must be inside the ceiling, got {} bytes",
        at_ceiling.len()
    );
    let read = GeometricType::Path.parse(&at_ceiling);
    assert!(
        read.is_ok(),
        "case path at the ceiling: a text of {} bytes is a value and must be read, got {:?}",
        at_ceiling.len(),
        read.err()
    );
}

/// An amount of money is read from every spelling an amount is written in, and
/// held to the cent.
#[test]
fn an_amount_is_read_from_every_spelling_and_held_to_the_cent() {
    let amounts: &[(&str, f64)] = &[
        ("256.78", 256.78),
        ("$256.78", 256.78),
        ("$1,234.50", 1234.5),
        ("1234.5", 1234.5),
        // A thousands separator groups the digits of one amount and is not part
        // of its value, wherever it is written.
        ("1,,", 1.0),
        ("0", 0.0),
        ("7", 7.0),
        // A negative amount: with a sign before or after the symbol, and in the
        // parentheses an accounting notation writes it in.
        ("-7", -7.0),
        ("-$7", -7.0),
        ("$-7", -7.0),
        ("(42.50)", -42.5),
        ("($42.50)", -42.5),
        // An amount is a number of cents, so a third decimal place is rounded
        // to one.
        ("19.995", 20.0),
        ("0.014", 0.01),
        ("-0.015", -0.02),
    ];

    for (input, expected) in amounts {
        let read = parse_money(input);
        assert_eq!(
            read,
            Ok(*expected),
            "case {input:?}: expected {expected}, got {read:?}"
        );
    }

    for input in ["", "free", "$", "1.2.3", "(1", "nan"] {
        let read = parse_money(input);
        assert_eq!(
            read,
            Err(format!("invalid input syntax for type money: \"{input}\"")),
            "case {input:?}: text that is not an amount must be refused, got {read:?}"
        );
    }
}
