// SPDX-License-Identifier: CC0-1.0

//! A declaration root says nothing twice in its LIST either: past the first item, no line of
//! it repeats a line the file has already said.
//!
//! Read against THIS crate's root only while its target keeps one: where the suites are
//! declared with `automod` from the vendored `tests/all/main.rs` instead, the rule has no
//! subject here and the workspace-wide siblings read it for every crate that does keep a
//! hand-maintained root.
//!
//! [`super::all_rs_carries_one_header_block`] reads the header — the block ahead of the first
//! item, where the module's own prose belongs — and stops there on purpose: past that item a
//! line belongs to the list, which is a different rule's region. This suite reads that region,
//! against the whole file: a line of the list that repeats a header line, or an earlier list
//! line, is a COPY, and a copy of the header is what a landing branch leaves behind when it
//! appends its paragraph to the end of the root instead of inserting its `#[path]`/`mod` pair
//! into the sorted list. This root's tail had reached hundreds of such lines, each re-typed,
//! re-worded or merged from the last, and one of them merged as `//!` is `E0753`, which
//! compiles no suite of this target at all.
//!
//! [`super::all_rs_is_a_canonical_declaration_list`] reports such a line too — for not being
//! half of a declaration pair — and that is the rule that keeps the list clean. What this one
//! adds is where the line came from: the earlier line it copies, which is the line that
//! invited it, and which no report about pair shape names. A copy is reported here wherever it
//! stands in the list, whether or not it is also past the last pair
//! ([`super::declaration_roots_end_with_their_last_declaration`]).
//!
//! Both halves of the root are answered here, the header one through the sibling's own reader,
//! so the two cannot disagree about what a repeat is: a line that says nothing — a blank line,
//! or the bare `//` that separates two paragraphs — is no copy of the next such line, and a
//! line is read by what it says rather than by how far it is indented, so a copy pasted one
//! level in is still a copy.
//!
//! The LIST half of the reader is read for the OTHER declaration roots of the workspace by
//! [`super::declaration_roots_hold_canonical_lists`], which is why it is `pub(crate)`: one
//! reading of what a copied line is, so the two suites cannot disagree about it.
//!
//! It reads one file; it writes nothing, starts no router and reaches no network.

use std::collections::BTreeMap;

// The declaration root this rule is about, read through the sibling declaration suite rather
// than opened a second time, and what that sibling states for a crate holding no such root.
use crate::all_rs_declares_each_suite_once::all_rs;
// The header half of the rule, and where a root's header ends, from the siblings that own
// them: the header block is read by its own suite's reader, not by a second copy here.
use crate::all_rs_carries_one_header_block::repeated_header_lines;
use crate::all_rs_inner_doc_comments_precede_the_first_item::first_item_line;

/// Whether `line` says something a copy of it could repeat: a blank line and a bare `//`
/// paragraph separator carry no words, so neither is a copy of the next.
fn says_something(line: &str) -> bool {
    let said = line.trim().trim_start_matches('/').trim();
    !said.is_empty()
}

/// One report per line of the LIST of `source` — the region from its first item on — that says
/// what an earlier line of the file already said, naming both lines.
///
/// Lines are compared by what they say, so indenting a copy does not hide it, and the report is
/// always against the FIRST line that said it: three copies of one line are two reports against
/// the original rather than a chain. A root with no item at all has no list, which is what the
/// sibling guards report; it is answered nothing here.
///
/// `pub(crate)` because the sibling that reads this rule for the other declaration roots of the
/// workspace ([`super::declaration_roots_hold_canonical_lists`]) reads it through this reader
/// rather than a second copy of it.
pub(crate) fn repeated_list_lines(source: &str) -> Vec<String> {
    let Some(first_item) = first_item_line(source) else {
        return Vec::new();
    };
    let mut first_said_on: BTreeMap<&str, usize> = BTreeMap::new();
    let mut reports = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if !says_something(line) {
            continue;
        }
        let number = index + 1;
        let said = line.trim();
        let first = *first_said_on.entry(said).or_insert(number);
        if first == number || number < first_item {
            continue;
        }
        reports.push(format!(
            "line {number}: expected a declaration the file does not hold already, got a copy \
             of line {first}: {said:?}"
        ));
    }
    reports
}

#[test]
fn the_declaration_root_says_nothing_twice() {
    // No subject where this crate's target declares its suites with `automod` instead of a
    // hand-maintained root; the workspace-wide siblings read this rule for every crate
    // that keeps one.
    let Some(source) = all_rs() else {
        return;
    };
    let mut reports = repeated_header_lines(&source);
    reports.extend(repeated_list_lines(&source));
    assert!(
        reports.is_empty(),
        "expected tests/all.rs to state its header once and declare each suite once, got {} \
         copied line(s): {reports:#?}",
        reports.len()
    );
    // Without this, a file the reader could not read — or one whose list had been lost — would
    // satisfy the report above by holding nothing to break it.
    let said = source.lines().filter(|line| says_something(line)).count();
    assert!(
        said > 200,
        "expected tests/all.rs to hold its header and the router's declarations, read {said} \
         line(s) that say anything"
    );
}

#[test]
fn the_reader_reports_a_line_of_the_list_that_the_file_already_said() {
    // One synthetic root per shape a copied line can take, with how many reports the reader
    // owes it. The rows differ in WHAT is copied (a header line, a whole declaration pair, the
    // licence line), in how many copies there are, in where in the list they stand (between the
    // pairs, past the last one), in whether the copy is indented, and in whether the repetition
    // is in the header at all — which is the sibling's region, not this one's.
    let licence = "// SPDX-License-Identifier: CC0-1.0\n";
    let head = "// The target: a declaration list.\n//\n// Put a new suite in it in order.\n";
    let first = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let second = "#[path = \"all/query_wire.rs\"]\nmod query_wire;\n";
    let copied = "// The target: a declaration list.\n";
    let cases: [(&str, String, usize); 10] = [
        ("a root with no item at all", format!("{licence}\n{head}"), 0),
        (
            "a root that says nothing twice",
            format!("{licence}\n{head}{first}{second}"),
            0,
        ),
        (
            // The shape this suite exists for: a landing leaving a copy of the header behind
            // instead of inserting its pair into the list.
            "a header line re-typed past the declarations",
            format!("{licence}\n{head}{first}{second}{copied}"),
            1,
        ),
        (
            "a header line re-typed between the pairs",
            format!("{licence}\n{head}{first}{copied}{second}"),
            1,
        ),
        (
            "a header line re-typed three times, each reported against the original",
            format!("{licence}\n{head}{first}{copied}{copied}"),
            2,
        ),
        (
            "a copy indented one level, which says the same thing",
            format!("{licence}\n{head}{first}    // The target: a declaration list.\n"),
            1,
        ),
        (
            "a declaration pair that stands twice, which compiles neither copy",
            format!("{licence}\n{head}{first}{first}"),
            2,
        ),
        (
            "a copy of the licence line",
            format!("{licence}\n{head}{first}{licence}"),
            1,
        ),
        (
            // Two lines that read alike are two lines: the rule is about copies, not about
            // prose that covers the same ground twice, which is the reviewer's business.
            "a line that differs from an earlier one by a word",
            format!("{licence}\n{head}{first}// The target: a declaration LIST.\n"),
            0,
        ),
        (
            // A repeat INSIDE the header is the sibling's report, so this reader passes it
            // over: one line is reported by one rule.
            "a header line written twice in the header",
            format!("{licence}\n{head}{copied}{first}"),
            0,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = repeated_list_lines(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_reads_what_a_line_says() {
    // Each form a line of a declaration root can take, and whether it says something a copy
    // could repeat: the vocabulary the rule is measured in, stated rather than assumed.
    let cases: [(&str, &str, bool); 9] = [
        ("a blank line", "", false),
        ("a line of spaces", "   ", false),
        ("a bare paragraph separator", "//", false),
        ("an indented separator", "    //", false),
        ("a separator with trailing spaces", "//   ", false),
        ("a comment that carries words", "// The target.", true),
        ("an inner doc comment", "//! The target.", true),
        ("a path attribute", "#[path = \"all/boot_serve.rs\"]", true),
        ("a module declaration", "mod boot_serve;", true),
    ];
    for (name, line, expected) in cases {
        let actual = says_something(line);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected says_something({line:?}) to be {expected}, got {actual}"
        );
    }
}
