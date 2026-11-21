// SPDX-License-Identifier: CC0-1.0

//! WHAT a suite run executes, and for how long: the test cases its definition's root group
//! names, the window each one is given, and the per-case result a reader projects from it.
//!
//! A Device Advisor suite run executes the test cases of its suite definition's
//! `rootGroup`, which is a DOCUMENT: a tree of groups, each holding test cases, where a
//! case names the catalog test it runs (`test.id`) and a `configuration` sets that case's
//! `EXECUTION_TIMEOUT` — the seconds the service gives the case to be satisfied by the
//! device under test. The model carries the member as an opaque string
//! (`smithy_query(query="shape",shape="SuiteDefinitionConfiguration")`: `rootGroup` is a
//! `RootGroup`, `len 0..2048`, documented as "the test suite root group"), so the document
//! is read here rather than constrained there, and a root group this reader cannot find a
//! test case in simply names none.
//!
//! Two rules of the suite-run lifecycle follow from that document, and both live here:
//!
//! * the RUNNING window — a run executes its cases in sequence, each for the window its
//!   `EXECUTION_TIMEOUT` configures, so a run is RUNNING for as long as its suite asks to
//!   run and a client polling it observes that status, rather than for a span of the
//!   emulator's own choosing; and
//! * the TERMINAL OUTCOME — a case the run's device never satisfies within its window
//!   times out and FAILS, so a run of a suite with test cases ends `FAIL` carrying a
//!   per-group, per-case result, while a run whose suite names no test case to execute at
//!   all could not be executed and ends `ERROR` (see
//!   [`super::NO_TEST_RUNNER_REASON`]).
//!
//! Nothing is stored for any of this: the plan is READ from the definition the run names
//! and the schedule is a function of the run's own start instant, so every reader derives
//! the same answer and a read writes nothing beyond the status transitions the clock had
//! already made due.

use std::time::{Duration, SystemTime};

use ::serde_json::Value;
use ::sha2::{Digest, Sha256};

use crate::ops::types::{GroupResult, TestCaseRun, TestResult};

use super::{
    is_successful, SuiteDefinition, SuiteRun, NO_TEST_RUNNER_REASON, STATUS_TRANSITION,
};

/// The `configuration` key a Device Advisor root group sets a test case's execution window
/// with: the seconds the service gives the case to be satisfied, as the documented test
/// suite definition document writes it (`{"configuration":{"EXECUTION_TIMEOUT":120}, …}`),
/// at the root, at a group, or at the case itself.
pub const EXECUTION_TIMEOUT_KEY: &str = "EXECUTION_TIMEOUT";

/// The window a test case that configures no [`EXECUTION_TIMEOUT_KEY`] anywhere above it
/// is given.
///
/// A CHOSEN span (the suite's own configuration names none), long enough that a client
/// polling the run at a realistic interval observes it RUNNING while the case is executing.
pub const UNCONFIGURED_EXECUTION_WINDOW: Duration = Duration::from_secs(3);

/// The longest window one test case, or a whole run, is scheduled over.
///
/// A REPRESENTABILITY bound this emulator chooses, not a claim about a published AWS
/// quota: the configured seconds are caller data and are added to an instant, so without a
/// ceiling a root group could ask for a schedule no clock value can hold.
pub const MAX_EXECUTION_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// How deeply the walk follows a root group's nested groups.
///
/// The document is caller data, so the depth it nests to is bounded here rather than
/// followed to wherever the value happens to end: a group below this depth contributes no
/// test case, exactly as one the document does not hold contributes none. Far past the two
/// levels a Device Advisor root group is written in, and far inside what `RootGroup`'s
/// `len 0..2048` could spell.
const MAX_GROUP_DEPTH: usize = 16;

/// The status a scheduled test case carries before its window opens — `Status$PENDING`,
/// "Test has not started running but is scheduled"
/// (`smithy_query(query="shape",shape="Status")`).
const SCHEDULED: &str = "PENDING";

/// The status a test case that COMPLETED carries when the run it belongs to did not pass.
///
/// A case completes here by its execution window elapsing with nothing having satisfied it,
/// which is the case timing out: `FAIL`.
const TIMED_OUT: &str = "FAIL";

/// The status a test case that completed inside a run that PASSED carries.
const PASSED: &str = "PASS";

/// The greatest test-case count a listing may report, as the model bounds the member
/// (`SuiteRunResultCount`, `range 0..500`).
const MAX_RESULT_COUNT: i32 = 500;

/// What one suite run executes: the groups of its definition's root group, in document
/// order, each with the test cases it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuiteExecution {
    groups: Vec<PlannedGroup>,
}

/// One group of a root group: the name the document gives it and the cases under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedGroup {
    /// `name` as the document writes it, which is what a result reports as its `groupName`.
    pub name: String,
    /// The test cases of this group, in document order — the order they execute in.
    pub cases: Vec<PlannedCase>,
}

/// One test case of a root group: what it runs, what it is called, and the window it is
/// given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCase {
    /// `name` as the document writes it, falling back to [`Self::test_id`] for a case the
    /// document named only by the test it runs.
    pub name: String,
    /// `test.id`: the catalog test this case runs (`MQTT_Connect`, …).
    pub test_id: String,
    /// How long this case has to be satisfied: its own `EXECUTION_TIMEOUT`, else the
    /// nearest one configured above it, else [`UNCONFIGURED_EXECUTION_WINDOW`].
    pub window: Duration,
}

impl SuiteExecution {
    /// What a run of `definition` executes — nothing at all when the store no longer holds
    /// the definition the run names.
    pub fn of(definition: Option<&SuiteDefinition>) -> Self {
        definition.map_or_else(Self::default, |definition| {
            Self::of_root_group(&definition.configuration.root_group)
        })
    }

    /// The groups and cases `root_group` names.
    ///
    /// A value that is not a root-group document — the member is an opaque string, so a
    /// caller may store anything inside its length bound — names no test case, exactly as
    /// a document with an empty `tests` list does.
    pub fn of_root_group(root_group: &str) -> Self {
        let Ok(document) = ::serde_json::from_str::<Value>(root_group) else {
            return Self::default();
        };
        Self {
            groups: groups_of(&document, None, "", MAX_GROUP_DEPTH),
        }
    }

    /// The groups this execution runs, in document order.
    pub fn groups(&self) -> &[PlannedGroup] {
        &self.groups
    }

    /// Every case this execution runs, in the order they execute in.
    pub fn cases(&self) -> impl Iterator<Item = &PlannedCase> {
        self.groups.iter().flat_map(|group| group.cases.iter())
    }

    /// True when this execution has a test case to run.
    pub fn has_cases(&self) -> bool {
        self.cases().next().is_some()
    }

    /// How long a run of this execution is RUNNING: its cases run in SEQUENCE, so the sum
    /// of their windows, bounded by [`MAX_EXECUTION_WINDOW`].
    ///
    /// An execution with no case to run is given one transition span instead — the run
    /// could not be executed, and reporting that is not instantaneous either, so a caller
    /// that reads the run straight after the start still observes it executing.
    pub fn window(&self) -> Duration {
        let total = self
            .cases()
            .map(|case| case.window)
            .fold(Duration::ZERO, |total, window| {
                total.saturating_add(window)
            });
        if total.is_zero() {
            STATUS_TRANSITION
        } else {
            total.min(MAX_EXECUTION_WINDOW)
        }
    }

    /// The terminal `SuiteRunStatus` a run of this execution reaches once its window has
    /// elapsed.
    ///
    /// A run that executed test cases none of which was satisfied inside its window has
    /// FAILED its suite — the test cases ran and timed out — whereas a run whose suite
    /// names no test case to execute could not be executed at all, which is what `ERROR`
    /// reports.
    pub fn outcome(&self) -> &'static str {
        if self.has_cases() {
            TIMED_OUT
        } else {
            "ERROR"
        }
    }

    /// The `errorReason` that terminal status carries: why nothing could be executed, for
    /// the run that could not be, and none for a run whose cases ran and failed.
    ///
    /// This reason is the ONLY "could not be executed" signal the lifecycle gives, and it
    /// belongs to the `ERROR` outcome alone: a `FAIL` reports test cases that executed and
    /// were not satisfied, which is an outcome of the suite rather than a statement about
    /// what this emulator cannot do.
    pub fn error_reason(&self) -> Option<&'static str> {
        (!self.has_cases()).then_some(NO_TEST_RUNNER_REASON)
    }

    /// How many of `run`'s test cases have PASSED and how many have FAILED as of `at`.
    ///
    /// The pair a listing reports (`SuiteRunInformation$passed` / `$failed`, "Number of
    /// test cases that passed/failed in the suite run", `range 0..500`), counted from the
    /// SAME projection [`Self::result`] reports case by case, so a listing and a read of
    /// one run never disagree about its outcome. `None` where there is no result to count:
    /// a run with no test case, or one that has not left the queue.
    pub fn counts(&self, run: &SuiteRun, at: SystemTime) -> Option<(i32, i32)> {
        let result = self.result(run, at)?;
        let mut passed = 0i32;
        let mut failed = 0i32;
        for case in result
            .groups
            .into_iter()
            .flatten()
            .flat_map(|group| group.tests.into_iter().flatten())
        {
            match case.status.as_deref() {
                Some(PASSED) => passed = passed.saturating_add(1),
                Some(TIMED_OUT) => failed = failed.saturating_add(1),
                _ => {}
            }
        }
        Some((passed.min(MAX_RESULT_COUNT), failed.min(MAX_RESULT_COUNT)))
    }

    /// The `testResult` of `run` as of `at`: one entry per group of this execution, each
    /// with one entry per test case and the status that case is in.
    ///
    /// `None` while there is nothing to report: an execution with no test case, or a run
    /// that has not left the queue, so no case has been scheduled against a start instant
    /// yet.
    ///
    /// The schedule is derived, not stored: the cases run in sequence from the run's start
    /// stamp, each for its own window, so a case is PENDING before its window opens,
    /// carries the run's own status while it is in it (`Status` and `SuiteRunStatus`
    /// enumerate the same nine values — `smithy_query(query="shape",shape="Status")`), and
    /// reports its outcome once the window has closed. A run that has ENDED is reported as
    /// of its end instant rather than of now, so the result a finished run reports stands
    /// still.
    pub fn result(&self, run: &SuiteRun, at: SystemTime) -> Option<TestResult> {
        if !self.has_cases() {
            return None;
        }
        let started = run.started_at?;
        let observed = run.end_at.map_or(at, |ended| ended.min(at));
        let mut opens = started;
        let mut groups = Vec::with_capacity(self.groups.len());
        for (group_index, group) in self.groups.iter().enumerate() {
            let mut tests = Vec::with_capacity(group.cases.len());
            for (case_index, case) in group.cases.iter().enumerate() {
                let case_opens = opens;
                let case_closes = case_opens
                    .checked_add(case.window)
                    .unwrap_or(case_opens);
                opens = case_closes;
                let (status, start_time, end_time) = if observed < case_opens {
                    (SCHEDULED, None, None)
                } else if observed < case_closes {
                    (run.status.as_str(), Some(case_opens), None)
                } else if is_successful(&run.status) {
                    (PASSED, Some(case_opens), Some(case_closes))
                } else {
                    (TIMED_OUT, Some(case_opens), Some(case_closes))
                };
                tests.push(TestCaseRun {
                    test_case_run_id: Some(derived_id(&[
                        "testcaserun",
                        &run.id,
                        &group_index.to_string(),
                        &case_index.to_string(),
                    ])),
                    // The case's DEFINITION is the suite's, so two runs of one definition
                    // report one definition id for the same case and two run ids.
                    test_case_definition_id: Some(derived_id(&[
                        "testcasedefinition",
                        &run.suite_definition_id,
                        &group_index.to_string(),
                        &case_index.to_string(),
                    ])),
                    test_case_definition_name: Some(case.name.clone()),
                    status: Some(status.to_string()),
                    start_time: start_time.map(Into::into),
                    end_time: end_time.map(Into::into),
                    ..TestCaseRun::default()
                });
            }
            groups.push(GroupResult {
                group_id: Some(derived_id(&[
                    "group",
                    &run.suite_definition_id,
                    &group_index.to_string(),
                ])),
                group_name: Some(group.name.clone()),
                tests: Some(tests),
            });
        }
        Some(TestResult {
            groups: Some(groups),
        })
    }
}

/// The groups `node` holds, for a document level whose enclosing group is called `name`,
/// whose nearest configured execution window is `inherited`, and which may nest `depth`
/// further levels ([`MAX_GROUP_DEPTH`]).
///
/// One level of a root group holds either test cases (an entry carrying a `test`) or
/// further groups (an entry carrying its own `tests`), and a real document nests the two,
/// so the walk is recursive and flattens what it finds: the cases of this level become one
/// group, and each nested group follows it. `EXECUTION_TIMEOUT` is inherited downwards —
/// the nearest one above a case is the one that applies to it — which is what lets a
/// document set the window once at the root for every case under it.
fn groups_of(
    node: &Value,
    inherited: Option<Duration>,
    name: &str,
    depth: usize,
) -> Vec<PlannedGroup> {
    let window = execution_timeout(node).or(inherited);
    let mut cases = Vec::new();
    let mut nested = Vec::new();
    for entry in node
        .get("tests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let entry_name = entry.get("name").and_then(Value::as_str).unwrap_or_default();
        if entry.get("tests").is_some() {
            if let Some(remaining) = depth.checked_sub(1) {
                nested.extend(groups_of(entry, window, entry_name, remaining));
            }
        } else if let Some(case) = planned_case(entry, entry_name, window) {
            cases.push(case);
        }
    }
    let mut groups = Vec::new();
    if !cases.is_empty() {
        groups.push(PlannedGroup {
            name: name.to_string(),
            cases,
        });
    }
    groups.extend(nested);
    groups
}

/// The test case `entry` names, or `None` when it names no catalog test and so nothing
/// this run could execute.
fn planned_case(entry: &Value, name: &str, inherited: Option<Duration>) -> Option<PlannedCase> {
    let test_id = entry.get("test")?.get("id")?.as_str()?.trim();
    if test_id.is_empty() {
        return None;
    }
    let window = execution_timeout(entry)
        .or(inherited)
        .unwrap_or(UNCONFIGURED_EXECUTION_WINDOW);
    Some(PlannedCase {
        name: if name.is_empty() {
            test_id.to_string()
        } else {
            name.to_string()
        },
        test_id: test_id.to_string(),
        window: window.min(MAX_EXECUTION_WINDOW),
    })
}

/// The execution window `node`'s own `configuration` sets, when it sets one.
///
/// The documented document writes the seconds as a NUMBER; a document whose values were
/// templated as text carries the same number as a string, so both are read. A value that
/// is not a positive count of seconds configures no window.
fn execution_timeout(node: &Value) -> Option<Duration> {
    let configured = node.get("configuration")?.get(EXECUTION_TIMEOUT_KEY)?;
    let seconds = match configured {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        seconds.min(MAX_EXECUTION_WINDOW.as_secs_f64()),
    ))
}

/// A `UUID`-shaped id derived from `fields`, so every read of one result reports the same
/// ids and two results that are about different things never share one.
///
/// Derived rather than minted because nothing about a run's result is stored: the id has to
/// come out of the run and the position of the case it belongs to. The layout is the shared
/// one (`service_common::uuid_from_random_bytes`), over the leading bytes of a digest of
/// the fields, each terminated so two field lists cannot run together into one preimage.
fn derived_id(fields: &[&str]) -> String {
    let mut digest = Sha256::new();
    for field in fields {
        digest.update(field.as_bytes());
        digest.update([0u8]);
    }
    let digest = digest.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ::service_common::uuid_from_random_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root group holding one group with one `MQTT_Connect` case, with `seconds` as the
    /// case's configured `EXECUTION_TIMEOUT` — the document shape Device Advisor's test
    /// suite definition uses.
    fn one_case(seconds: &str) -> String {
        format!(
            r#"{{"configuration":{{}},"tests":[{{"name":"my_group","configuration":{{}},
               "tests":[{{"name":"my_test","configuration":{{"EXECUTION_TIMEOUT":{seconds}}},
               "test":{{"id":"MQTT_Connect","version":"0.0.0"}}}}]}}]}}"#
        )
    }

    /// RULE: a case's window is the `EXECUTION_TIMEOUT` configured for it, wherever above
    /// it the document sets one, and a case with none gets the chosen span.
    #[test]
    fn a_cases_window_is_the_execution_timeout_configured_for_it() {
        let rows: Vec<(&str, String, Duration)> = vec![
            (
                "the case configures 120 seconds",
                one_case("120"),
                Duration::from_secs(120),
            ),
            (
                "the case configures 300 seconds",
                one_case("300"),
                Duration::from_secs(300),
            ),
            (
                "the seconds are written as text",
                one_case("\"45\""),
                Duration::from_secs(45),
            ),
            (
                "a fractional window",
                one_case("1.5"),
                Duration::from_secs_f64(1.5),
            ),
            (
                "the group configures it, the case does not",
                r#"{"tests":[{"name":"g","configuration":{"EXECUTION_TIMEOUT":42},
                   "tests":[{"name":"t","test":{"id":"MQTT_Connect"}}]}]}"#
                    .to_string(),
                Duration::from_secs(42),
            ),
            (
                "the root configures it for everything under it",
                r#"{"configuration":{"EXECUTION_TIMEOUT":7},
                   "tests":[{"name":"g","tests":[{"name":"t","test":{"id":"MQTT_Connect"}}]}]}"#
                    .to_string(),
                Duration::from_secs(7),
            ),
            (
                "nothing configures one",
                r#"{"tests":[{"name":"g","tests":[{"name":"t","test":{"id":"MQTT_Connect"}}]}]}"#
                    .to_string(),
                UNCONFIGURED_EXECUTION_WINDOW,
            ),
            (
                "a timeout that is no count of seconds",
                one_case("\"soon\""),
                UNCONFIGURED_EXECUTION_WINDOW,
            ),
            (
                "a timeout of zero",
                one_case("0"),
                UNCONFIGURED_EXECUTION_WINDOW,
            ),
            (
                "a timeout past the representable ceiling",
                one_case("99999999999999999999"),
                MAX_EXECUTION_WINDOW,
            ),
        ];

        for (case, root_group, expected) in rows {
            let execution = SuiteExecution::of_root_group(&root_group);
            let windows: Vec<Duration> = execution.cases().map(|planned| planned.window).collect();
            assert_eq!(
                windows,
                vec![expected],
                "case {case:?}: expected one case of {expected:?}, got {windows:?}"
            );
            assert_eq!(
                execution.window(),
                expected,
                "case {case:?}: the run's window is its one case's window"
            );
            assert_eq!(
                execution.outcome(),
                "FAIL",
                "case {case:?}: a run with a test case ends FAIL, not ERROR"
            );
        }
    }

    /// RULE: a root group that names no test case names no execution — whatever it holds —
    /// so such a run is the one that could not be executed.
    #[test]
    fn a_root_group_naming_no_test_case_names_no_execution() {
        let rows: [(&str, &str); 7] = [
            ("not a document at all", "g"),
            ("an empty string", ""),
            ("an empty document", "{}"),
            ("an empty test list", r#"{"tests":[]}"#),
            (
                "groups with no cases",
                r#"{"tests":[{"name":"g","tests":[]}]}"#,
            ),
            (
                "a case naming no test",
                r#"{"tests":[{"name":"t","configuration":{}}]}"#,
            ),
            (
                "a case whose test has no id",
                r#"{"tests":[{"name":"t","test":{"version":"0.0.0"}}]}"#,
            ),
        ];

        for (case, root_group) in rows {
            let execution = SuiteExecution::of_root_group(root_group);
            assert!(
                !execution.has_cases(),
                "case {case:?}: expected no test case, got {:?}",
                execution.groups()
            );
            assert_eq!(
                (execution.outcome(), execution.window()),
                ("ERROR", STATUS_TRANSITION),
                "case {case:?}: a run with nothing to execute errors after one span"
            );
            assert_eq!(
                execution.error_reason(),
                Some(NO_TEST_RUNNER_REASON),
                "case {case:?}: the error must say why nothing ran"
            );
        }
    }

    /// RULE: the run's window is the sum of its cases' windows, because the cases execute
    /// in sequence — over documents of several shapes and case counts.
    #[test]
    fn a_runs_window_is_the_sum_of_its_cases_windows() {
        let two_groups = r#"{"configuration":{"EXECUTION_TIMEOUT":10},"tests":[
            {"name":"first","tests":[
                {"name":"a","test":{"id":"MQTT_Connect"}},
                {"name":"b","configuration":{"EXECUTION_TIMEOUT":5},
                 "test":{"id":"MQTT_Publish"}}]},
            {"name":"second","tests":[{"name":"c","test":{"id":"MQTT_Subscribe"}}]}]}"#;
        let execution = SuiteExecution::of_root_group(two_groups);

        let names: Vec<&str> = execution
            .groups()
            .iter()
            .map(|group| group.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["first", "second"],
            "the groups come out in document order, got {names:?}"
        );
        let windows: Vec<Duration> = execution.cases().map(|case| case.window).collect();
        assert_eq!(
            windows,
            vec![
                Duration::from_secs(10),
                Duration::from_secs(5),
                Duration::from_secs(10)
            ],
            "each case takes its own window, inheriting the root's, got {windows:?}"
        );
        assert_eq!(
            execution.window(),
            Duration::from_secs(25),
            "the run's window is the sum of its cases' windows"
        );
    }

    /// A root group nested past the depth the walk follows contributes no test case, and a
    /// document far larger than the member can hold is still read without running away.
    #[test]
    fn a_document_nested_past_the_bound_contributes_no_case() {
        // One case at the bound's depth, reached through nested groups, is still found.
        let mut document = r#"{"name":"deep","configuration":{"EXECUTION_TIMEOUT":4},
            "test":{"id":"MQTT_Connect"}}"#
            .to_string();
        for _ in 0..MAX_GROUP_DEPTH - 1 {
            document = format!(r#"{{"name":"g","tests":[{document}]}}"#);
        }
        let inside = SuiteExecution::of_root_group(&document);
        assert!(
            inside.has_cases(),
            "a case at the bound's depth is executed, got {:?}",
            inside.groups()
        );

        // One level deeper than the walk follows: the case is out of reach, so the run has
        // nothing to execute rather than the walk going on without limit.
        for _ in 0..4 {
            document = format!(r#"{{"name":"g","tests":[{document}]}}"#);
        }
        let beyond = SuiteExecution::of_root_group(&document);
        assert!(
            !beyond.has_cases(),
            "a case nested past the bound is not executed, got {:?}",
            beyond.groups()
        );
    }

    /// A case written directly under the root, which the document shape allows, is still a
    /// case of the run.
    #[test]
    fn a_case_written_directly_under_the_root_is_executed() {
        let execution = SuiteExecution::of_root_group(
            r#"{"name":"root","configuration":{"EXECUTION_TIMEOUT":9},
               "tests":[{"name":"lonely","test":{"id":"MQTT_Connect"}}]}"#,
        );
        let cases: Vec<(&str, &str, Duration)> = execution
            .cases()
            .map(|case| (case.name.as_str(), case.test_id.as_str(), case.window))
            .collect();
        assert_eq!(
            cases,
            vec![("lonely", "MQTT_Connect", Duration::from_secs(9))],
            "expected the root's own case, got {cases:?}"
        );
    }
}
