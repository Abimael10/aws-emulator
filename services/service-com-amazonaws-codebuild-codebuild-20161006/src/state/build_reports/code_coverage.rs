// SPDX-License-Identifier: CC0-1.0

//! The COVERAGE one code-coverage file holds, read from the file itself.
//!
//! # The rule this module implements
//!
//! A code-coverage report's data is what the coverage files the buildspec declared counted:
//! how many lines and conditional branches of the code under test the tests reached and how
//! many they did not. `CodeCoverageReportSummary` states exactly that pair per dimension —
//! "Line coverage measures how many statements your tests cover … Branch coverage determines
//! if your tests cover every possible branch of a control structure" — and the percentages
//! are those counts, so nothing is read that the file does not count.
//!
//! Each reading below is of ONE published schema and of only the elements the schema states
//! those totals in. A file holding no total is a file this reading counts nothing from, which
//! is what the run reports ([`super::ReportRun`]).

/// What one coverage file counted: the covered and the uncovered count of each dimension the
/// file states, and `None` for a dimension it does not state at all.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct CoverageTally {
    /// `(covered, missed)` lines, when the file counts lines.
    pub(super) lines: Option<(i64, i64)>,
    /// `(covered, missed)` conditional branches, when the file counts branches.
    pub(super) branches: Option<(i64, i64)>,
}

impl CoverageTally {
    /// Whether this tally counted anything at all.
    pub(super) fn counted(self) -> bool {
        self.lines.is_some() || self.branches.is_some()
    }

    /// Add `other`'s counts to these, so a report declared over several files is the total of
    /// what they counted — a dimension only one of them states is that one's count.
    pub(super) fn added(self, other: Self) -> Self {
        Self {
            lines: added(self.lines, other.lines),
            branches: added(self.branches, other.branches),
        }
    }
}

/// One dimension's counts added, keeping the counted side when only one side counted it.
fn added(left: Option<(i64, i64)>, right: Option<(i64, i64)>) -> Option<(i64, i64)> {
    match (left, right) {
        (Some((left_covered, left_missed)), Some((right_covered, right_missed))) => Some((
            left_covered.saturating_add(right_covered),
            left_missed.saturating_add(right_missed),
        )),
        (counted, None) | (None, counted) => counted,
    }
}

/// The element a JaCoCo XML report states a total in, and the attributes of one: `<counter
/// type="LINE" missed="3" covered="7"/>`, repeated per dimension at each level of the report.
const JACOCO_COUNTER: &str = "counter";
/// See [`JACOCO_COUNTER`].
const JACOCO_TYPE: &str = "type";
/// See [`JACOCO_COUNTER`].
const JACOCO_MISSED: &str = "missed";
/// See [`JACOCO_COUNTER`].
const JACOCO_COVERED: &str = "covered";
/// The `counter` types that are the two dimensions a coverage summary states.
const JACOCO_LINE: &str = "LINE";
/// See [`JACOCO_LINE`].
const JACOCO_BRANCH: &str = "BRANCH";
/// The element whose own counters are a per-file total, which is what a report that states
/// no total of its own is added up from.
const JACOCO_SOURCE_FILE: &str = "sourcefile";

/// The attributes a Cobertura XML report states its totals in, on the root `<coverage>`
/// element: how many lines and branches there are, and how many are covered.
const COBERTURA_LINES_VALID: &str = "lines-valid";
/// See [`COBERTURA_LINES_VALID`].
const COBERTURA_LINES_COVERED: &str = "lines-covered";
/// See [`COBERTURA_LINES_VALID`].
const COBERTURA_BRANCHES_VALID: &str = "branches-valid";
/// See [`COBERTURA_LINES_VALID`].
const COBERTURA_BRANCHES_COVERED: &str = "branches-covered";
/// The element a Cobertura report states ONE line of the code under test in, and the
/// attribute carrying how many times the tests reached it — what a report that states no
/// totals of its own is counted from.
const COBERTURA_LINE: &str = "line";
/// See [`COBERTURA_LINE`].
const COBERTURA_HITS: &str = "hits";

/// The element a Clover XML report states a total in, and the attributes of one: `<metrics
/// statements="10" coveredstatements="7" conditionals="4" coveredconditionals="2"/>`, whose
/// first occurrence is the whole project's.
const CLOVER_METRICS: &str = "metrics";
/// See [`CLOVER_METRICS`].
const CLOVER_STATEMENTS: &str = "statements";
/// See [`CLOVER_METRICS`].
const CLOVER_COVERED_STATEMENTS: &str = "coveredstatements";
/// See [`CLOVER_METRICS`].
const CLOVER_CONDITIONALS: &str = "conditionals";
/// See [`CLOVER_METRICS`].
const CLOVER_COVERED_CONDITIONALS: &str = "coveredconditionals";

/// The member a SimpleCov result set states its per-file line counts under: a map of file
/// name to the hit count of each line — `null` for a line coverage does not apply to, `0`
/// for one the tests did not reach, and a count for one they did.
const SIMPLECOV_COVERAGE: &str = "coverage";
/// See [`SIMPLECOV_COVERAGE`]: the newer result set states the counts under this member of
/// the per-file entry rather than as the entry itself.
const SIMPLECOV_LINES: &str = "lines";

/// What a JaCoCo XML report counted.
///
/// A JaCoCo report states its counters at every level — the report, each package, each class
/// and each source file — so the ones the REPORT states are its own totals, and a document
/// that states none is added up from its source files' instead.
pub(super) fn jacoco(document: &str) -> CoverageTally {
    let Ok(parsed) = ::roxmltree::Document::parse(document) else {
        return CoverageTally::default();
    };
    let root = parsed.root_element();
    let mut tally = counters_of(root);
    if tally.counted() {
        return tally;
    }
    for file in parsed
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name(JACOCO_SOURCE_FILE))
    {
        tally = tally.added(counters_of(file));
    }
    tally
}

/// The totals the `counter` children of `element` state.
fn counters_of(element: ::roxmltree::Node<'_, '_>) -> CoverageTally {
    let mut tally = CoverageTally::default();
    for counter in element
        .children()
        .filter(|node| node.is_element() && node.has_tag_name(JACOCO_COUNTER))
    {
        let covered = attribute_count(counter, JACOCO_COVERED);
        let missed = attribute_count(counter, JACOCO_MISSED);
        let (Some(covered), Some(missed)) = (covered, missed) else {
            continue;
        };
        match counter.attribute(JACOCO_TYPE) {
            Some(JACOCO_LINE) => tally.lines = added(tally.lines, Some((covered, missed))),
            Some(JACOCO_BRANCH) => tally.branches = added(tally.branches, Some((covered, missed))),
            _ => {}
        }
    }
    tally
}

/// What a Cobertura XML report counted.
///
/// The root element states the totals, so they are read from it; a document that states none
/// is counted from the lines it reports instead — a line the tests reached is one with hits.
pub(super) fn cobertura(document: &str) -> CoverageTally {
    let Ok(parsed) = ::roxmltree::Document::parse(document) else {
        return CoverageTally::default();
    };
    let root = parsed.root_element();
    let stated = CoverageTally {
        lines: valid_and_covered(
            attribute_count(root, COBERTURA_LINES_VALID),
            attribute_count(root, COBERTURA_LINES_COVERED),
        ),
        branches: valid_and_covered(
            attribute_count(root, COBERTURA_BRANCHES_VALID),
            attribute_count(root, COBERTURA_BRANCHES_COVERED),
        ),
    };
    if stated.counted() {
        return stated;
    }
    let mut covered = 0i64;
    let mut missed = 0i64;
    let mut counted = false;
    for line in parsed
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name(COBERTURA_LINE))
    {
        let Some(hits) = attribute_count(line, COBERTURA_HITS) else {
            continue;
        };
        counted = true;
        if hits > 0 {
            covered += 1;
        } else {
            missed += 1;
        }
    }
    CoverageTally {
        lines: counted.then_some((covered, missed)),
        branches: None,
    }
}

/// What a Clover XML report counted: its first `metrics` element, which is the project's own
/// total, stated as the number of statements and conditionals and how many of each are
/// covered.
pub(super) fn clover(document: &str) -> CoverageTally {
    let Ok(parsed) = ::roxmltree::Document::parse(document) else {
        return CoverageTally::default();
    };
    let Some(metrics) = parsed
        .descendants()
        .find(|node| node.is_element() && node.has_tag_name(CLOVER_METRICS))
    else {
        return CoverageTally::default();
    };
    CoverageTally {
        lines: valid_and_covered(
            attribute_count(metrics, CLOVER_STATEMENTS),
            attribute_count(metrics, CLOVER_COVERED_STATEMENTS),
        ),
        branches: valid_and_covered(
            attribute_count(metrics, CLOVER_CONDITIONALS),
            attribute_count(metrics, CLOVER_COVERED_CONDITIONALS),
        ),
    }
}

/// What a SimpleCov result set counted: every per-file line count of every suite in it. The
/// format states no branch total, so a report read from one carries none.
pub(super) fn simplecov(document: &str) -> CoverageTally {
    let Ok(parsed) = ::serde_json::from_str::<::serde_json::Value>(document) else {
        return CoverageTally::default();
    };
    let Some(suites) = parsed.as_object() else {
        return CoverageTally::default();
    };
    let mut covered = 0i64;
    let mut missed = 0i64;
    let mut counted = false;
    for suite in suites.values() {
        let Some(files) = suite.get(SIMPLECOV_COVERAGE).and_then(|it| it.as_object()) else {
            continue;
        };
        for file in files.values() {
            // The result set states a file's counts either as the entry itself or under its
            // `lines` member; both are the same list of per-line hit counts.
            let lines = file
                .as_array()
                .or_else(|| file.get(SIMPLECOV_LINES).and_then(|it| it.as_array()));
            let Some(lines) = lines else {
                continue;
            };
            for line in lines {
                // A line coverage does not apply to is stated as null and counts for
                // neither side.
                let Some(hits) = line.as_i64() else {
                    continue;
                };
                counted = true;
                if hits > 0 {
                    covered += 1;
                } else {
                    missed += 1;
                }
            }
        }
    }
    CoverageTally {
        lines: counted.then_some((covered, missed)),
        branches: None,
    }
}

/// One dimension's counts, from a total and how much of it is covered — `None` when the file
/// states no total, and neither side ever below zero, whatever numbers the file states.
fn valid_and_covered(valid: Option<i64>, covered: Option<i64>) -> Option<(i64, i64)> {
    let valid = valid?;
    let covered = covered.unwrap_or_default().max(0);
    Some((covered, valid.saturating_sub(covered).max(0)))
}

/// The whole number `attribute` of `element` states, or `None` when it states none this
/// reading can read as one.
fn attribute_count(element: ::roxmltree::Node<'_, '_>, attribute: &str) -> Option<i64> {
    element
        .attribute(attribute)
        .and_then(|value| value.trim().parse::<i64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: each published schema's own totals are what a coverage report is read from, as
    /// the covered and uncovered count of each dimension the file states.
    #[test]
    fn each_schema_is_read_as_the_totals_it_states() {
        // (case, what the reading answered, what the file states)
        let rows: Vec<(&str, CoverageTally, CoverageTally)> = vec![
            (
                "a JaCoCo report stating its own counters",
                jacoco(
                    "<report name=\"r\"><package name=\"p\">\
                     <counter type=\"LINE\" missed=\"1\" covered=\"1\"/></package>\
                     <counter type=\"LINE\" missed=\"5\" covered=\"15\"/>\
                     <counter type=\"BRANCH\" missed=\"2\" covered=\"6\"/>\
                     <counter type=\"INSTRUCTION\" missed=\"9\" covered=\"9\"/></report>",
                ),
                CoverageTally {
                    lines: Some((15, 5)),
                    branches: Some((6, 2)),
                },
            ),
            (
                "a JaCoCo report stating only its source files' counters",
                jacoco(
                    "<report name=\"r\"><package name=\"p\">\
                     <sourcefile name=\"a\"><counter type=\"LINE\" missed=\"1\" covered=\"3\"/>\
                     </sourcefile>\
                     <sourcefile name=\"b\"><counter type=\"LINE\" missed=\"2\" covered=\"4\"/>\
                     <counter type=\"BRANCH\" missed=\"1\" covered=\"1\"/></sourcefile>\
                     </package></report>",
                ),
                CoverageTally {
                    lines: Some((7, 3)),
                    branches: Some((1, 1)),
                },
            ),
            (
                "a Cobertura report stating its own totals",
                cobertura(
                    "<coverage lines-valid=\"10\" lines-covered=\"8\" branches-valid=\"4\" \
                     branches-covered=\"3\"/>",
                ),
                CoverageTally {
                    lines: Some((8, 2)),
                    branches: Some((3, 1)),
                },
            ),
            (
                "a Cobertura report stating only its lines",
                cobertura(
                    "<coverage><packages><package><classes><class><lines>\
                     <line number=\"1\" hits=\"2\"/><line number=\"2\" hits=\"0\"/>\
                     <line number=\"3\" hits=\"1\"/></lines></class></classes></package>\
                     </packages></coverage>",
                ),
                CoverageTally {
                    lines: Some((2, 1)),
                    branches: None,
                },
            ),
            (
                "a Clover report",
                clover(
                    "<coverage><project><metrics statements=\"20\" coveredstatements=\"13\" \
                     conditionals=\"6\" coveredconditionals=\"2\"/></project></coverage>",
                ),
                CoverageTally {
                    lines: Some((13, 7)),
                    branches: Some((2, 4)),
                },
            ),
            (
                "a SimpleCov result set stating a file's lines under `lines`",
                simplecov(
                    r#"{"RSpec":{"coverage":{"a.rb":{"lines":[1,0,null,3]},
                        "b.rb":{"lines":[0,0]}},"timestamp":1}}"#,
                ),
                CoverageTally {
                    lines: Some((2, 3)),
                    branches: None,
                },
            ),
            (
                "a SimpleCov result set stating a file's lines as the entry",
                simplecov(r#"{"Minitest":{"coverage":{"a.rb":[1,null,0]}}}"#),
                CoverageTally {
                    lines: Some((1, 1)),
                    branches: None,
                },
            ),
        ];
        for (case, read, expected) in rows {
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a file that states no total this reading can read counts nothing, so no
    /// coverage is invented for it.
    #[test]
    fn a_file_stating_no_total_counts_nothing() {
        // (case, what the reading answered)
        let rows: Vec<(&str, CoverageTally)> = vec![
            ("not XML at all", jacoco("not a document")),
            ("XML counting nothing", jacoco("<report/>")),
            ("another schema's XML", clover("<report/>")),
            ("an empty Cobertura report", cobertura("<coverage/>")),
            ("not JSON at all", simplecov("<coverage/>")),
            ("JSON counting nothing", simplecov("{}")),
        ];
        for (case, read) in rows {
            assert!(
                !read.counted(),
                "case {case:?}: expected nothing counted, got {read:?}"
            );
        }
    }
}
