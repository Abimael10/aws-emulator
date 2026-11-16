// SPDX-License-Identifier: CC0-1.0

//! The router's `tests/all.rs` is a canonical declaration list: pairs, sorted, nothing else.
//!
//! A declaration root is appended to by every landing branch, and it is the one file in the
//! target whose breakage takes every suite down with it: a module declared twice is `E0428`,
//! and an inner doc comment past the first item is `E0753`. Sibling suites pin those two
//! rules for every root in the workspace, and the build-time layout check already fails the
//! build for a suite file no `mod` names. This one pins the shape that keeps a root out of
//! their way: the list holds one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite,
//! sorted by name, and no other line — so two branches landing at once insert two lines in
//! two places and have no prose to duplicate, re-word or mistype as `//!`.
//!
//! Sorted STRICTLY, so the same name twice is a report here as well as the `E0428` the
//! sibling reports, and a declaration appended past the end of the list rather than into it
//! is caught before it collides with the next branch's.
//!
//! It reads one file; it starts no router and reaches no network.

// The declaration root this rule is about, read through the sibling declaration suite rather
// than opened a second time.
use crate::all_rs_declares_each_suite_once::all_rs;

/// Every suite name the list in `source` declares, and one report per line of that list which
/// is not half of a well-formed declaration pair.
///
/// The header — the run of blank lines, comments and inner attributes that opens the file — is
/// skipped, since that is where the module doc belongs. Everything from the first item on is
/// the list, and each pair must be exactly a `#[path = "all/<name>.rs"]` line followed by the
/// matching `mod <name>;` line, both in column one. That is stricter than the compiler, and
/// than the sibling suite's reader of the same file: a visibility, a trailing comment or a
/// note between two pairs all compile, and all are reported here.
fn read_list(source: &str) -> (Vec<String>, Vec<String>) {
    let mut names = Vec::new();
    let mut reports = Vec::new();
    let mut lines = source.lines().enumerate();
    let mut in_list = false;
    while let Some((index, line)) = lines.next() {
        if !in_list {
            let header = line.trim_start();
            if header.is_empty() || header.starts_with("//") || header.starts_with("#![") {
                continue;
            }
            in_list = true;
        }
        let number = index + 1;
        let attribute = line
            .strip_prefix("#[path = \"all/")
            .and_then(|rest| rest.strip_suffix(".rs\"]"));
        let Some(name) = attribute else {
            reports.push(format!(
                "line {number}: expected a `#[path = \"all/<name>.rs\"]` attribute opening a \
                 declaration pair, got {line:?}"
            ));
            continue;
        };
        let expected = format!("mod {name};");
        match lines.next() {
            Some((_, next)) if next == expected => names.push(name.to_string()),
            Some((next_index, next)) => reports.push(format!(
                "line {}: expected {expected:?} under the attribute on line {number}, got \
                 {next:?}",
                next_index + 1
            )),
            None => reports.push(format!(
                "line {number}: expected {expected:?} under the attribute, got the end of the \
                 file"
            )),
        }
    }
    (names, reports)
}

/// One report per adjacent pair of `names` that is not strictly ascending — a name out of
/// order, or a name repeated.
fn out_of_order(names: &[String]) -> Vec<String> {
    names
        .windows(2)
        .filter(|pair| pair[0] >= pair[1])
        .map(|pair| {
            format!(
                "case {:?}: expected it to sort strictly after {:?}, which it follows in the \
                 list",
                pair[1], pair[0]
            )
        })
        .collect()
}

#[test]
fn the_declaration_list_holds_nothing_but_declaration_pairs() {
    // This crate keeps a hand-maintained declaration root only while its target has one;
    // where its suites are declared with `automod` instead, this rule has no subject here
    // and the workspace-wide siblings read it for every crate that does have one.
    let Some(source) = all_rs() else {
        return;
    };
    let (names, reports) = read_list(&source);
    assert!(
        reports.is_empty(),
        "expected tests/all.rs to hold its module doc, then one `#[path]`/`mod` pair per \
         suite and no other line, got {} report(s): {reports:#?}",
        reports.len()
    );
    // Without this, a file that is nothing but a header would satisfy the report above.
    assert!(
        !names.is_empty(),
        "expected tests/all.rs to declare the router's cross-service suites, read no \
         declaration at all"
    );
}

#[test]
fn the_declaration_list_is_sorted_by_suite_name() {
    let Some(source) = all_rs() else {
        return;
    };
    let (names, _) = read_list(&source);
    let reports = out_of_order(&names);
    assert!(
        reports.is_empty(),
        "expected the {} declarations in tests/all.rs to be sorted by name, got {} that are \
         not: {reports:#?}",
        names.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reports_a_list_line_that_is_not_half_of_a_pair() {
    // One synthetic root per shape a line in the list can take, with the names the reader owes
    // it and how many reports. The rows differ in what opens the file, in what kind of line
    // lands between the pairs, in which half of a pair is malformed, and in where the line
    // sits in the list.
    let head = "// SPDX-License-Identifier: CC0-1.0\n\n//! The target.\n//!\n//! Its rules.\n";
    let first = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let second = "#[path = \"all/query_wire.rs\"]\nmod query_wire;\n";
    let cases: [(&str, String, &[&str], usize); 16] = [
        ("an empty root", String::new(), &[], 0),
        ("a header and nothing else", head.to_string(), &[], 0),
        ("one pair under a header", format!("{head}{first}"), &["boot_serve"], 0),
        (
            "two pairs under a header",
            format!("{head}{first}{second}"),
            &["boot_serve", "query_wire"],
            0,
        ),
        (
            "a pair with no header at all",
            first.to_string(),
            &["boot_serve"],
            0,
        ),
        (
            "an inner attribute in the header",
            format!("#![allow(dead_code)]\n//! The target.\n{first}"),
            &["boot_serve"],
            0,
        ),
        (
            // The shape this suite exists for: a landing branch appending its own prose
            // between the pairs, which is where every duplicated and mistyped note has come
            // from.
            "a plain comment between two pairs",
            format!("{head}{first}// Declared here, for the reason the entry above records.\n{second}"),
            &["boot_serve", "query_wire"],
            1,
        ),
        (
            "a three-line comment block between two pairs",
            format!("{head}{first}// Declared here:\n// it spans two services,\n// so it lives here.\n{second}"),
            &["boot_serve", "query_wire"],
            3,
        ),
        (
            "an inner doc comment between two pairs",
            format!("{head}{first}//! A note typed as a doc comment.\n{second}"),
            &["boot_serve", "query_wire"],
            1,
        ),
        (
            "a blank line between two pairs",
            format!("{head}{first}\n{second}"),
            &["boot_serve", "query_wire"],
            1,
        ),
        (
            "a mod whose name disagrees with its path",
            format!("{head}#[path = \"all/boot_serve.rs\"]\nmod serve_boot;\n"),
            &[],
            1,
        ),
        (
            "a bare mod with no path attribute",
            format!("{head}mod boot_serve;\n{second}"),
            &["query_wire"],
            1,
        ),
        (
            "a visibility on the mod half of a pair",
            format!("{head}#[path = \"all/boot_serve.rs\"]\npub mod boot_serve;\n"),
            &[],
            1,
        ),
        (
            "a note trailing the mod half of a pair",
            format!("{head}#[path = \"all/boot_serve.rs\"]\nmod boot_serve; // the harness\n"),
            &[],
            1,
        ),
        (
            // An inner attribute is legal only before the first item, so one among the
            // declarations compiles no suite of this target at all; the header skip is over by
            // then, so the reader reports it like any other line.
            "an inner attribute among the declarations",
            format!("{head}{first}#![allow(dead_code)]\n{second}"),
            &["boot_serve", "query_wire"],
            1,
        ),
        (
            // The shape this root had grown: prose past the end of the list, with the
            // declarations a landing appended standing after it instead of in the list.
            "prose and a pair appended past the end of the list",
            format!(
                "{head}{first}{second}// Declared last, because the end of the file is where \
                 this landing wrote.\n#[path = \"all/ec2_network.rs\"]\nmod ec2_network;\n"
            ),
            &["boot_serve", "query_wire", "ec2_network"],
            1,
        ),
    ];

    for case in &cases {
        let (name, source, expected_names, expected_reports) = case;
        let (names, reports) = read_list(source);
        assert_eq!(
            names, *expected_names,
            "case {name:?}: expected the names {expected_names:?}, got {names:?}"
        );
        assert_eq!(
            reports.len(),
            *expected_reports,
            "case {name:?}: expected {expected_reports} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_reports_a_name_out_of_order() {
    // Each ordering a list of names can be in, and how many adjacent pairs break the sort.
    let cases: [(&str, &[&str], usize); 8] = [
        ("no names", &[], 0),
        ("one name", &["boot_serve"], 0),
        ("two sorted names", &["boot_serve", "query_wire"], 0),
        ("two names swapped", &["query_wire", "boot_serve"], 1),
        ("the same name twice", &["boot_serve", "boot_serve"], 1),
        (
            // A landing appending to the end of a sorted list rather than into it.
            "a sorted run with one name appended at the end",
            &["boot_serve", "query_wire", "ec2_network"],
            1,
        ),
        (
            "a reversed run",
            &["query_wire", "ec2_network", "boot_serve"],
            2,
        ),
        (
            // Several landings appending past the end of the sorted list rather than into it,
            // which is the shape this root had grown: the run break at each name that follows
            // one sorting after it.
            "a sorted run with three names appended past its end",
            &[
                "boot_serve",
                "query_wire",
                "zz_probe",
                "ec2_network",
                "s3_notification",
                "apigateway_stage",
            ],
            2,
        ),
    ];

    for case in &cases {
        let (name, names, expected) = case;
        let names: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        let reports = out_of_order(&names);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}
