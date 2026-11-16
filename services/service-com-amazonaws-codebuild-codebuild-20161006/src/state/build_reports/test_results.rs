// SPDX-License-Identifier: CC0-1.0

//! The TEST CASES one test-result file holds, read from the file itself.
//!
//! # The rule this module implements
//!
//! A test report's data is the test cases the files the buildspec declared hold, in the
//! format it declared them in: every case the file states, with the status that case ended
//! in and how long it took. `TestCase$status` fixes the statuses a case is reported as —
//! "Valid statuses are `SUCCEEDED`, `FAILED`, `ERROR`, `SKIPPED`, and `UNKNOWN`" — so what
//! the framework wrote is mapped onto those and onto nothing else.
//!
//! Each reading below is of ONE published schema, and reads only the elements and
//! attributes that schema states a case's identity, outcome and duration in. A file that is
//! not that schema — or not well-formed at all — holds no case to read, which is what the
//! run reports ([`super::ReportRun`]).

use super::{
    duration_nanos_of_seconds, outcome_status, TestResult, STATUS_ERROR, STATUS_FAILED,
    STATUS_SKIPPED, STATUS_SUCCEEDED, STATUS_UNKNOWN,
};

/// The element a JUnit XML document states one test case in, and the children that carry
/// the outcome of a case that did not simply pass: `<testcase name= classname= time=>` with
/// `<failure>`, `<error>` or `<skipped>` inside it.
const JUNIT_CASE: &str = "testcase";
/// See [`JUNIT_CASE`].
const JUNIT_FAILURE: &str = "failure";
/// See [`JUNIT_CASE`].
const JUNIT_ERROR: &str = "error";
/// See [`JUNIT_CASE`].
const JUNIT_SKIPPED: &str = "skipped";
/// The attribute a JUnit, NUnit 2 or Clover document states a span of SECONDS in.
const SECONDS_ATTRIBUTE: &str = "time";

/// The element an NUnit document states one test case in — `<test-case>` in both the
/// version 2 and the version 3 schema.
const NUNIT_CASE: &str = "test-case";
/// The attribute NUnit 3 states a case's outcome in (`result="Passed"`), and NUnit 2 the
/// same where it states one.
const RESULT_ATTRIBUTE: &str = "result";
/// The two attributes NUnit 2 states a case's outcome in: whether it ran at all, and
/// whether it passed.
const NUNIT2_EXECUTED: &str = "executed";
/// See [`NUNIT2_EXECUTED`].
const NUNIT2_SUCCESS: &str = "success";
/// The attribute NUnit 3 states a case's span of SECONDS in.
const NUNIT3_DURATION: &str = "duration";

/// The element a TestNG document states one test case in, and the attributes it states the
/// case's outcome and its span of MILLISECONDS in: `<test-method status="PASS"
/// duration-ms="12">`.
const TESTNG_CASE: &str = "test-method";
/// See [`TESTNG_CASE`].
const TESTNG_STATUS: &str = "status";
/// See [`TESTNG_CASE`].
const TESTNG_DURATION_MILLIS: &str = "duration-ms";

/// The element a Visual Studio test-results document states one test case in, and the
/// attributes carrying its outcome and its span: `<UnitTestResult outcome="Passed"
/// duration="00:00:00.1234567">`.
const TRX_CASE: &str = "UnitTestResult";
/// See [`TRX_CASE`].
const TRX_OUTCOME: &str = "outcome";
/// See [`TRX_CASE`]: a `HH:MM:SS.fffffff` span rather than a number of seconds.
const TRX_DURATION: &str = "duration";

/// The members a Cucumber JSON document states its scenarios and their steps in: a list of
/// features, each with `elements`, each of those with `steps`, each step with a `result`
/// carrying the step's `status` and its span in NANOSECONDS.
const CUCUMBER_ELEMENTS: &str = "elements";
/// See [`CUCUMBER_ELEMENTS`].
const CUCUMBER_STEPS: &str = "steps";
/// See [`CUCUMBER_ELEMENTS`].
const CUCUMBER_RESULT: &str = "result";
/// See [`CUCUMBER_ELEMENTS`].
const CUCUMBER_STATUS: &str = "status";
/// See [`CUCUMBER_ELEMENTS`].
const CUCUMBER_DURATION_NANOS: &str = "duration";

/// The test cases a JUnit XML document holds.
///
/// A case's outcome is the child element the schema puts inside `<testcase>` for it, and a
/// case with none of them passed; the span is its `time`, in seconds.
pub(super) fn junit(document: &str) -> Vec<TestResult> {
    read_xml(document, JUNIT_CASE, |case| TestResult {
        status: if has_child(case, JUNIT_FAILURE) {
            STATUS_FAILED
        } else if has_child(case, JUNIT_ERROR) {
            STATUS_ERROR
        } else if has_child(case, JUNIT_SKIPPED) {
            STATUS_SKIPPED
        } else {
            STATUS_SUCCEEDED
        },
        duration_nanos: duration_nanos_of_seconds(case.attribute(SECONDS_ATTRIBUTE)),
    })
}

/// The test cases an NUnit XML document holds — version 2 and version 3 alike, which state
/// a case in the same element and differ in the attributes they state its outcome and its
/// span in.
///
/// Version 3 states the outcome as one `result` word; version 2 states whether the case was
/// `executed` and whether it was a `success`, so a case that did not run is one that was
/// skipped and a case that ran without succeeding is one that failed.
pub(super) fn nunit(document: &str) -> Vec<TestResult> {
    read_xml(document, NUNIT_CASE, |case| TestResult {
        status: match case.attribute(RESULT_ATTRIBUTE) {
            Some(result) => outcome_status(result),
            None => {
                let executed = case
                    .attribute(NUNIT2_EXECUTED)
                    .is_none_or(|executed| !executed.eq_ignore_ascii_case("false"));
                let succeeded = case
                    .attribute(NUNIT2_SUCCESS)
                    .is_some_and(|success| success.eq_ignore_ascii_case("true"));
                if !executed {
                    STATUS_SKIPPED
                } else if succeeded {
                    STATUS_SUCCEEDED
                } else {
                    STATUS_FAILED
                }
            }
        },
        duration_nanos: duration_nanos_of_seconds(
            case.attribute(NUNIT3_DURATION)
                .or_else(|| case.attribute(SECONDS_ATTRIBUTE)),
        ),
    })
}

/// The test cases a TestNG XML document holds: one `<test-method>` each, whose `status` is
/// its outcome and whose `duration-ms` is its span in milliseconds.
pub(super) fn testng(document: &str) -> Vec<TestResult> {
    read_xml(document, TESTNG_CASE, |case| TestResult {
        status: case
            .attribute(TESTNG_STATUS)
            .map_or(STATUS_UNKNOWN, outcome_status),
        duration_nanos: case
            .attribute(TESTNG_DURATION_MILLIS)
            .and_then(|millis| millis.trim().parse::<f64>().ok())
            .map_or(0, |millis| (millis * 1_000_000.0) as i64),
    })
}

/// The test cases a Visual Studio test-results document holds: one `<UnitTestResult>` each,
/// whose `outcome` is its outcome and whose `duration` is a `HH:MM:SS.fffffff` span.
pub(super) fn visual_studio(document: &str) -> Vec<TestResult> {
    read_xml(document, TRX_CASE, |case| TestResult {
        status: case
            .attribute(TRX_OUTCOME)
            .map_or(STATUS_UNKNOWN, outcome_status),
        duration_nanos: timespan_nanos(case.attribute(TRX_DURATION)),
    })
}

/// The test cases a Cucumber JSON document holds: one per SCENARIO (an `elements` entry of
/// a feature), whose outcome is what its steps came to and whose span is their total.
///
/// A scenario is the case a framework-agnostic report is of, so its outcome is read off the
/// steps it is made of: a scenario with a failing step failed, one with a step that did not
/// run was skipped, and one whose every step passed passed. Each step's `result/duration` is
/// already stated in nanoseconds.
pub(super) fn cucumber(document: &str) -> Vec<TestResult> {
    let Ok(parsed) = ::serde_json::from_str::<::serde_json::Value>(document) else {
        return Vec::new();
    };
    // A document is a list of features; one feature written on its own is read as that one.
    let features: Vec<&::serde_json::Value> = match parsed.as_array() {
        Some(listed) => listed.iter().collect(),
        None => vec![&parsed],
    };
    let mut cases = Vec::new();
    for feature in features {
        let Some(elements) = feature.get(CUCUMBER_ELEMENTS).and_then(|it| it.as_array()) else {
            continue;
        };
        for scenario in elements {
            let steps = scenario
                .get(CUCUMBER_STEPS)
                .and_then(|it| it.as_array())
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut status = STATUS_SUCCEEDED;
            let mut duration_nanos: i64 = 0;
            for step in steps {
                let result = step.get(CUCUMBER_RESULT);
                let stated = result
                    .and_then(|result| result.get(CUCUMBER_STATUS))
                    .and_then(::serde_json::Value::as_str)
                    .map_or(STATUS_UNKNOWN, outcome_status);
                duration_nanos = duration_nanos.saturating_add(
                    result
                        .and_then(|result| result.get(CUCUMBER_DURATION_NANOS))
                        .and_then(::serde_json::Value::as_i64)
                        .unwrap_or_default(),
                );
                // The step that is furthest from having passed is what the scenario came
                // to: a failure or an error outranks a skip, which outranks a pass.
                if severity(stated) > severity(status) {
                    status = stated;
                }
            }
            cases.push(TestResult {
                status,
                duration_nanos,
            });
        }
    }
    cases
}

/// How far from having passed one status is, so a scenario made of several steps reports the
/// worst of them.
fn severity(status: &str) -> u8 {
    match status {
        STATUS_SUCCEEDED => 0,
        STATUS_SKIPPED => 1,
        STATUS_UNKNOWN => 2,
        STATUS_ERROR => 3,
        _ => 4,
    }
}

/// Read every `element` of `document` through `case`, or nothing at all when the text is not
/// well-formed XML — a file this reading holds no case of.
fn read_xml(
    document: &str,
    element: &str,
    case: impl Fn(::roxmltree::Node<'_, '_>) -> TestResult,
) -> Vec<TestResult> {
    let Ok(parsed) = ::roxmltree::Document::parse(document) else {
        return Vec::new();
    };
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name(element))
        .map(case)
        .collect()
}

/// Whether `case` holds a child element named `element`.
fn has_child(case: ::roxmltree::Node<'_, '_>, element: &str) -> bool {
    case.children()
        .any(|child| child.is_element() && child.has_tag_name(element))
}

/// The nanoseconds a `HH:MM:SS.fffffff` span states, or none for a value that is not one.
fn timespan_nanos(stated: Option<&str>) -> i64 {
    let Some(stated) = stated else {
        return 0;
    };
    let mut seconds = 0f64;
    for part in stated.trim().split(':') {
        let Ok(value) = part.parse::<f64>() else {
            return 0;
        };
        seconds = seconds * 60.0 + value;
    }
    (seconds * 1_000_000_000.0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statuses `cases` came to, as the report's own status words.
    fn statuses(cases: &[TestResult]) -> Vec<&'static str> {
        cases.iter().map(|case| case.status).collect()
    }

    /// RULE: each published schema's own elements and attributes are what a case's outcome
    /// and span are read from, mapped onto the five statuses `TestCase$status` names.
    #[test]
    fn each_schema_is_read_as_the_cases_it_states() {
        let junit = junit(
            "<testsuites><testsuite name=\"s\"><testcase name=\"a\" time=\"0.5\"/>\
             <testcase name=\"b\"><failure message=\"no\"/></testcase>\
             <testcase name=\"c\"><error message=\"bad\"/></testcase>\
             <testcase name=\"d\"><skipped/></testcase></testsuite></testsuites>",
        );
        assert_eq!(
            statuses(&junit),
            vec![
                STATUS_SUCCEEDED,
                STATUS_FAILED,
                STATUS_ERROR,
                STATUS_SKIPPED
            ],
            "case \"junit\": expected the children to decide each outcome, got {junit:?}"
        );
        assert_eq!(
            junit[0].duration_nanos, 500_000_000,
            "case \"junit\": `time` is a span of seconds, got {junit:?}"
        );

        let nunit3 = nunit(
            "<test-run><test-case name=\"a\" result=\"Passed\" duration=\"0.25\"/>\
             <test-case name=\"b\" result=\"Failed\"/>\
             <test-case name=\"c\" result=\"Skipped\"/>\
             <test-case name=\"d\" result=\"Inconclusive\"/></test-run>",
        );
        assert_eq!(
            statuses(&nunit3),
            vec![
                STATUS_SUCCEEDED,
                STATUS_FAILED,
                STATUS_SKIPPED,
                STATUS_UNKNOWN
            ],
            "case \"nunit3\": expected `result` to decide each outcome, got {nunit3:?}"
        );
        assert_eq!(
            nunit3[0].duration_nanos, 250_000_000,
            "case \"nunit3\": `duration` is a span of seconds, got {nunit3:?}"
        );

        let nunit2 = nunit(
            "<test-results><test-suite><results>\
             <test-case name=\"a\" executed=\"True\" success=\"True\" time=\"1\"/>\
             <test-case name=\"b\" executed=\"True\" success=\"False\"/>\
             <test-case name=\"c\" executed=\"False\"/>\
             </results></test-suite></test-results>",
        );
        assert_eq!(
            statuses(&nunit2),
            vec![STATUS_SUCCEEDED, STATUS_FAILED, STATUS_SKIPPED],
            "case \"nunit2\": expected `executed`/`success` to decide each outcome, \
             got {nunit2:?}"
        );
        assert_eq!(
            nunit2[0].duration_nanos, 1_000_000_000,
            "case \"nunit2\": `time` is a span of seconds, got {nunit2:?}"
        );

        let testng = testng(
            "<testng-results><suite><test><class name=\"C\">\
             <test-method status=\"PASS\" name=\"a\" duration-ms=\"12\"/>\
             <test-method status=\"FAIL\" name=\"b\"/>\
             <test-method status=\"SKIP\" name=\"c\"/>\
             </class></test></suite></testng-results>",
        );
        assert_eq!(
            statuses(&testng),
            vec![STATUS_SUCCEEDED, STATUS_FAILED, STATUS_SKIPPED],
            "case \"testng\": expected `status` to decide each outcome, got {testng:?}"
        );
        assert_eq!(
            testng[0].duration_nanos, 12_000_000,
            "case \"testng\": `duration-ms` is a span of milliseconds, got {testng:?}"
        );

        let trx = visual_studio(
            "<TestRun><Results>\
             <UnitTestResult testName=\"a\" outcome=\"Passed\" duration=\"00:00:01.5000000\"/>\
             <UnitTestResult testName=\"b\" outcome=\"Failed\"/>\
             <UnitTestResult testName=\"c\" outcome=\"NotExecuted\"/>\
             <UnitTestResult testName=\"d\" outcome=\"Error\"/>\
             </Results></TestRun>",
        );
        assert_eq!(
            statuses(&trx),
            vec![
                STATUS_SUCCEEDED,
                STATUS_FAILED,
                STATUS_SKIPPED,
                STATUS_ERROR
            ],
            "case \"trx\": expected `outcome` to decide each outcome, got {trx:?}"
        );
        assert_eq!(
            trx[0].duration_nanos, 1_500_000_000,
            "case \"trx\": `duration` is a HH:MM:SS span, got {trx:?}"
        );

        let cucumber = cucumber(
            r#"[{"name":"f","elements":[
                {"name":"one","steps":[{"result":{"status":"passed","duration":100}},
                                       {"result":{"status":"passed","duration":200}}]},
                {"name":"two","steps":[{"result":{"status":"passed","duration":1}},
                                       {"result":{"status":"failed","duration":2}}]},
                {"name":"three","steps":[{"result":{"status":"skipped","duration":0}}]}]}]"#,
        );
        assert_eq!(
            statuses(&cucumber),
            vec![STATUS_SUCCEEDED, STATUS_FAILED, STATUS_SKIPPED],
            "case \"cucumber\": a scenario comes to the worst of its steps, got {cucumber:?}"
        );
        assert_eq!(
            cucumber[0].duration_nanos, 300,
            "case \"cucumber\": a scenario's span is its steps' total in nanoseconds, \
             got {cucumber:?}"
        );
    }

    /// RULE: a file that is not the document it was declared to be holds no case to read, so
    /// nothing is invented for it.
    #[test]
    fn a_file_that_is_not_the_declared_document_holds_no_case() {
        // (case, what the reading answered)
        let rows: Vec<(&str, Vec<TestResult>)> = vec![
            ("not XML at all", junit("this is not a document")),
            ("XML holding no case", junit("<testsuites/>")),
            ("another schema's XML", junit("<test-run><test-case/></test-run>")),
            ("not JSON at all", cucumber("<testsuites/>")),
            ("JSON holding no scenario", cucumber("[]")),
        ];
        for (case, read) in rows {
            assert!(
                read.is_empty(),
                "case {case:?}: expected no test case, got {read:?}"
            );
        }
    }
}
