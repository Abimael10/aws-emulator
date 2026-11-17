// SPDX-License-Identifier: CC0-1.0

//! The REPORTS a build produces: the report runs its buildspec declared, read from the
//! result files the build's own commands wrote.
//!
//! # The rule this module implements
//!
//! A buildspec's `reports` section declares, for each report group, which files of the
//! build's working tree hold that group's results and what FORMAT those files are in. A
//! build that ran mints ONE report run per declared group: its data is read from those
//! files — the test cases a test-result file holds ([`test_results`]), or the line and
//! branch totals a coverage file counts ([`code_coverage`]) — and its status is what they
//! say, `SUCCEEDED` when every test case passed, `FAILED` when one did not, and
//! `INCOMPLETE` when the declared files held no result to read at all.
//!
//! The run belongs to the group the section names, and the two forms that section takes name
//! it differently: a report group ARN names a group the account already holds, while a plain
//! name is the group `<project name>-<declared name>`, which the build CREATES when the
//! account holds none. A run can only belong to a group that holds its kind of report, since
//! a report group is of one `ReportType`.
//!
//! Which files of the tree a section declares is the same selection a buildspec's `artifacts`
//! section makes — `files` patterns under a `base-directory`, with `discard-paths` — so it is
//! made by the same reading ([`super::declared_files`]).
//!
//! # What one run costs
//!
//! The files are caller-supplied documents, so every step that reads them is bounded: the
//! working tree they come out of holds what `::service_common::posix_shell_volume`'s limits
//! allow and no more, the files one run is read from are bounded by [`MAX_REPORT_FILES`], the
//! test cases it is counted from by [`MAX_TEST_CASES`], and the runs the store retains by
//! [`MAX_REPORTS`]. Each document is read by this workspace's XML and JSON readers rather
//! than by a parser written here.

use std::collections::{BTreeMap, HashMap};

use super::{
    datetime, declared_files, is_arn, report_arn, report_group_arn, report_group_name_from_arn,
    ArtifactDeclaration, ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState,
    StoredReportGroup, MAX_REPORT_GROUPS, REPORT_GROUP_ACTIVE_STATUS,
};
use crate::ops::types::{
    CodeCoverageReportSummary, Report, ReportExportConfig, ReportGroup, TestCase,
    TestCaseFilter, TestReportSummary,
};

/// What ONE coverage file counted, and how a coverage file is read.
mod code_coverage;
/// The test cases ONE test-result file holds, and how each format is read.
mod test_results;

/// `ReportType` values a report and its group carry (`crate::ops::types::REPORT_TYPE_VALUES`
/// is the model's own list): a report of test cases, and one of code coverage.
pub const REPORT_TYPE_TEST: &str = "TEST";
/// See [`REPORT_TYPE_TEST`].
pub const REPORT_TYPE_CODE_COVERAGE: &str = "CODE_COVERAGE";

/// `ReportStatusType` values a report run reports
/// (`crate::ops::types::REPORT_STATUS_TYPE_VALUES`): every test case passed, one did not,
/// and the run's declared files held no result to read.
pub const REPORT_STATUS_SUCCEEDED: &str = "SUCCEEDED";
/// See [`REPORT_STATUS_SUCCEEDED`].
pub const REPORT_STATUS_FAILED: &str = "FAILED";
/// See [`REPORT_STATUS_SUCCEEDED`].
pub const REPORT_STATUS_INCOMPLETE: &str = "INCOMPLETE";

/// The statuses ONE test case is reported as, from `TestCase$status`: "Valid statuses are
/// SUCCEEDED, FAILED, ERROR, SKIPPED, and UNKNOWN." They are also the keys of
/// `TestReportSummary$statusCounts`, "a map that contains the number of each type of status
/// returned by the test results".
pub const STATUS_SUCCEEDED: &str = "SUCCEEDED";
/// See [`STATUS_SUCCEEDED`].
pub const STATUS_FAILED: &str = "FAILED";
/// See [`STATUS_SUCCEEDED`].
pub const STATUS_ERROR: &str = "ERROR";
/// See [`STATUS_SUCCEEDED`].
pub const STATUS_SKIPPED: &str = "SKIPPED";
/// See [`STATUS_SUCCEEDED`].
pub const STATUS_UNKNOWN: &str = "UNKNOWN";

/// The `ReportExportConfigType` of a group whose raw data is not exported anywhere: "NO
/// EXPORT: The report results are not exported."
const NO_EXPORT: &str = "NO_EXPORT";

/// How long a report run is readable for, from `Report$expired`: "A report expires 30 days
/// after it is created."
const REPORT_RETENTION_MILLIS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// Test cases ONE report run is read from.
///
/// CHOSEN, not an AWS limit: the cases are read out of files a build's own commands wrote, so
/// how many one run may be counted from is bounded here. A run whose files hold more than
/// this is reported `truncated`, which is what `Report$truncated` states — "the list of test
/// cases is truncated after the maximum number of test cases is reached".
const MAX_TEST_CASES: usize = 5_000;

/// Files ONE report run is read from, bounding the same growth [`MAX_TEST_CASES`] bounds for
/// the same reason: the patterns a section declares are matched against a whole working tree.
const MAX_REPORT_FILES: usize = 100;

/// Report runs this emulator retains, across every report group.
///
/// CHOSEN, not an AWS limit: a run is minted by a build rather than being a resource the
/// caller deletes, so the store needs a ceiling at that growth site. Past this, the oldest
/// run goes — the one an operation that analyses "the most recent reports" would reach last.
pub const MAX_REPORTS: usize = 10_000;

/// The `file-format` a `reports` section declares its files are in.
///
/// The catalogue is the `file-format` list the CodeBuild buildspec reference states for the
/// `reports` section: `CUCUMBERJSON`, `JUNITXML`, `NUNITXML`, `NUNIT3XML`, `TESTNGXML` and
/// `VISUALSTUDIOTRX` hold test cases, and `CLOVERXML`, `COBERTURAXML`, `JACOCOXML` and
/// `SIMPLECOV` hold code coverage — which is what makes the declared format decide the
/// `ReportType` of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// Cucumber's JSON report.
    CucumberJson,
    /// JUnit's XML report, which is the format a section that declares none is in.
    JUnitXml,
    /// NUnit's version 2 XML report.
    NUnitXml,
    /// NUnit's version 3 XML report.
    NUnit3Xml,
    /// TestNG's XML report.
    TestNgXml,
    /// Visual Studio's test-results document.
    VisualStudioTrx,
    /// Clover's XML coverage report.
    CloverXml,
    /// Cobertura's XML coverage report.
    CoberturaXml,
    /// JaCoCo's XML coverage report.
    JacocoXml,
    /// SimpleCov's result set.
    SimpleCov,
}

/// Every declarable format, as the word the buildspec declares it with.
const DECLARED_FORMATS: [(&str, ReportFormat); 10] = [
    ("CUCUMBERJSON", ReportFormat::CucumberJson),
    ("JUNITXML", ReportFormat::JUnitXml),
    ("NUNITXML", ReportFormat::NUnitXml),
    ("NUNIT3XML", ReportFormat::NUnit3Xml),
    ("TESTNGXML", ReportFormat::TestNgXml),
    ("VISUALSTUDIOTRX", ReportFormat::VisualStudioTrx),
    ("CLOVERXML", ReportFormat::CloverXml),
    ("COBERTURAXML", ReportFormat::CoberturaXml),
    ("JACOCOXML", ReportFormat::JacocoXml),
    ("SIMPLECOV", ReportFormat::SimpleCov),
];

/// The format of a section that declares none: the buildspec reference states `JUNITXML` as
/// the default of `file-format`.
const DEFAULT_FILE_FORMAT: &str = "JUNITXML";

impl ReportFormat {
    /// The format `declared` names — the default for a section that declares none, and `None`
    /// for a word that names no format a report is read from at all.
    #[must_use]
    pub fn declared(declared: Option<&str>) -> Option<Self> {
        let declared = declared
            .map(str::trim)
            .filter(|declared| !declared.is_empty())
            .unwrap_or(DEFAULT_FILE_FORMAT);
        DECLARED_FORMATS
            .iter()
            .find(|(word, _)| word.eq_ignore_ascii_case(declared))
            .map(|&(_, format)| format)
    }

    /// The `ReportType` of a run read from this format.
    #[must_use]
    pub fn report_type(self) -> &'static str {
        match self {
            Self::CucumberJson
            | Self::JUnitXml
            | Self::NUnitXml
            | Self::NUnit3Xml
            | Self::TestNgXml
            | Self::VisualStudioTrx => REPORT_TYPE_TEST,
            Self::CloverXml | Self::CoberturaXml | Self::JacocoXml | Self::SimpleCov => {
                REPORT_TYPE_CODE_COVERAGE
            }
        }
    }
}

/// One `reports/<report group>` section of a buildspec: which files of the working tree hold
/// the group's results, and the format they are in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReportDeclaration {
    /// The `files`, `base-directory` and `discard-paths` the section declares, read as the
    /// same selection an `artifacts` section declares.
    pub files: ArtifactDeclaration,
    /// The `file-format` the section declares — `None` when it declares none, which is the
    /// default format.
    pub file_format: Option<String>,
}

/// What one declared report group's files came to: the run a build mints for that group.
#[derive(Debug, Clone)]
pub struct ReportRun {
    /// The report group as the buildspec named it: an ARN, or the plain name whose group is
    /// `<project name>-<name>`.
    pub declared_group: String,
    /// The `ReportType` of what was read, which the declared format decides.
    pub report_type: &'static str,
    /// The `ReportStatusType` the run ended in.
    pub status: &'static str,
    /// Whether the declared files held more results than were read ([`MAX_TEST_CASES`]).
    pub truncated: bool,
    /// The test cases read, summarised — `None` for a run that is not of test cases, or one
    /// whose files held none.
    pub test_summary: Option<TestReportSummary>,
    /// The test cases read, each as the file stated it — empty for a run that is not of test
    /// cases, or one whose files held none. They are what `DescribeTestCases` reports for
    /// the report this run mints.
    pub test_cases: Vec<TestResult>,
    /// The coverage counted — `None` for a run that is not of code coverage, or one whose
    /// files counted nothing.
    pub code_coverage_summary: Option<CodeCoverageReportSummary>,
}

/// One test case as a report run counts it: the status it ended in, how long it took, the
/// identity the result file gave it and what that file said about it.
///
/// The members beside the status and the span are what `TestCase` reports for one case:
/// `name` is the case's own, `prefix` is "a string that is applied to a series of related
/// test cases" (the class or feature the document groups it under), `testSuiteName` is the
/// suite it was stated inside and `message` is "a message associated with a test case. For
/// example, an error message or stack trace". A member the document stated nothing for is
/// absent rather than empty: nothing is invented for a case the file did not describe.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestResult {
    /// One of the five statuses `TestCase$status` names ([`STATUS_SUCCEEDED`]).
    pub status: &'static str,
    /// How long the case took, in the unit `TestCase$durationInNanoSeconds` reports.
    pub duration_nanos: i64,
    /// The case's own name, as the document states it.
    pub name: Option<String>,
    /// The series of related cases this one belongs to.
    pub prefix: Option<String>,
    /// The suite the document stated the case inside.
    pub suite_name: Option<String>,
    /// What the document said about a case that did not pass.
    pub message: Option<String>,
    /// The file of the build's working tree the case was read from, which is what
    /// `TestCase$testRawDataPath` reports — filled in by the run that read the file rather
    /// than by the reading of the document itself.
    pub raw_data_path: Option<String>,
}

/// The status one of the five `TestCase$status` values a framework's own outcome word means.
///
/// The words are the ones the schemas this module reads write an outcome with — NUnit's
/// `Passed`/`Failed`/`Skipped`/`Inconclusive`, TestNG's `PASS`/`FAIL`/`SKIP`, Visual
/// Studio's `Passed`/`NotExecuted`/`Error`/`Timeout` and Cucumber's
/// `passed`/`failed`/`skipped`/`pending`/`undefined` — and an outcome word none of them
/// writes is one this reading cannot place, which is what `UNKNOWN` is for.
pub(super) fn outcome_status(outcome: &str) -> &'static str {
    let outcome = outcome.trim().to_ascii_lowercase();
    match outcome.as_str() {
        "passed" | "pass" | "success" | "succeeded" | "ok" => STATUS_SUCCEEDED,
        "failed" | "fail" | "failure" | "timeout" => STATUS_FAILED,
        "error" | "broken" => STATUS_ERROR,
        "skipped" | "skip" | "ignored" | "notexecuted" | "pending" | "undefined" => STATUS_SKIPPED,
        _ => STATUS_UNKNOWN,
    }
}

/// The nanoseconds a span of SECONDS states, or none for a value that states no span.
pub(super) fn duration_nanos_of_seconds(stated: Option<&str>) -> i64 {
    stated
        .and_then(|seconds| seconds.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map_or(0, |seconds| (seconds * 1_000_000_000.0) as i64)
}

/// The report runs a build's `reports` sections came to, read from the files `produced` holds.
///
/// One run per declared group, in the order the groups sort — a section whose declared
/// `file-format` names no format a report is read from declares a run nothing can be read
/// for, so none is minted for it.
#[must_use]
pub fn report_runs(
    declared: &BTreeMap<String, ReportDeclaration>,
    produced: &[(String, String)],
) -> Vec<ReportRun> {
    let mut runs = Vec::new();
    for (group, declaration) in declared {
        let Some(format) = ReportFormat::declared(declaration.file_format.as_deref()) else {
            ::tracing::debug!(
                %group,
                file_format = ?declaration.file_format,
                "a buildspec's reports section declared a file format no report is read from"
            );
            continue;
        };
        let files = declared_files(&declaration.files, produced);
        runs.push(read_run(group, format, &files));
    }
    runs
}

/// What ONE declared group's `files` came to, read in `format`.
fn read_run(group: &str, format: ReportFormat, files: &[(String, String)]) -> ReportRun {
    let report_type = format.report_type();
    let mut run = ReportRun {
        declared_group: group.to_string(),
        report_type,
        // A run whose files held nothing to read is not a run that passed: nothing was read
        // for it, which is the status CodeBuild has for a report its raw data did not
        // complete.
        status: REPORT_STATUS_INCOMPLETE,
        truncated: false,
        test_summary: None,
        test_cases: Vec::new(),
        code_coverage_summary: None,
    };
    let read_from = files.iter().take(MAX_REPORT_FILES);
    if report_type == REPORT_TYPE_TEST {
        let mut cases: Vec<TestResult> = Vec::new();
        for (path, contents) in read_from {
            let mut held = test_cases(format, contents);
            // The file a case was read from is the raw data of the run, which is what the
            // case reports as its own `testRawDataPath`.
            for case in &mut held {
                case.raw_data_path = Some(path.clone());
            }
            if cases.len() + held.len() > MAX_TEST_CASES {
                held.truncate(MAX_TEST_CASES.saturating_sub(cases.len()));
                run.truncated = true;
            }
            cases.append(&mut held);
            if run.truncated {
                break;
            }
        }
        if cases.is_empty() {
            return run;
        }
        // A report of test cases FAILED when a case did not pass, which is a case that
        // failed or errored; a skipped case is one the run did not have an answer for.
        run.status = if cases
            .iter()
            .any(|case| case.status == STATUS_FAILED || case.status == STATUS_ERROR)
        {
            REPORT_STATUS_FAILED
        } else {
            REPORT_STATUS_SUCCEEDED
        };
        run.test_summary = Some(test_summary(&cases));
        run.test_cases = cases;
        return run;
    }
    let mut tally = code_coverage::CoverageTally::default();
    for (_, contents) in read_from {
        tally = tally.added(coverage(format, contents));
    }
    if !tally.counted() {
        return run;
    }
    // A coverage report has no outcome of its own to fail on: it counted what it counted.
    run.status = REPORT_STATUS_SUCCEEDED;
    run.code_coverage_summary = Some(coverage_summary(tally));
    run
}

/// The test cases one file holds, in the format it was declared in.
fn test_cases(format: ReportFormat, contents: &str) -> Vec<TestResult> {
    match format {
        ReportFormat::CucumberJson => test_results::cucumber(contents),
        ReportFormat::JUnitXml => test_results::junit(contents),
        ReportFormat::NUnitXml | ReportFormat::NUnit3Xml => test_results::nunit(contents),
        ReportFormat::TestNgXml => test_results::testng(contents),
        ReportFormat::VisualStudioTrx => test_results::visual_studio(contents),
        // Reached for no coverage format: `read_run` reads those through `coverage`.
        _ => Vec::new(),
    }
}

/// What one coverage file counted, in the format it was declared in.
fn coverage(format: ReportFormat, contents: &str) -> code_coverage::CoverageTally {
    match format {
        ReportFormat::CloverXml => code_coverage::clover(contents),
        ReportFormat::CoberturaXml => code_coverage::cobertura(contents),
        ReportFormat::JacocoXml => code_coverage::jacoco(contents),
        ReportFormat::SimpleCov => code_coverage::simplecov(contents),
        // Reached for no test format: `read_run` reads those through `test_cases`.
        _ => code_coverage::CoverageTally::default(),
    }
}

/// `cases` as `TestReportSummary` states them: how many there were, how many ended in each
/// status, and how long they took together.
///
/// `statusCounts` carries an entry for each status the cases ACTUALLY ended in — the member
/// is "the number of each type of status returned by the test results in this
/// `TestReportSummary`", so a status no case ended in is not one the results returned.
fn test_summary(cases: &[TestResult]) -> TestReportSummary {
    let mut status_counts: HashMap<String, i32> = HashMap::new();
    let mut duration_in_nano_seconds: i64 = 0;
    for case in cases {
        *status_counts.entry(case.status.to_string()).or_default() += 1;
        duration_in_nano_seconds = duration_in_nano_seconds.saturating_add(case.duration_nanos);
    }
    TestReportSummary {
        total: i32::try_from(cases.len()).unwrap_or(i32::MAX),
        status_counts,
        duration_in_nano_seconds,
    }
}

/// What a tally counted as `CodeCoverageReportSummary` states it: the covered and uncovered
/// count of each dimension the files counted, and that dimension's percentage.
///
/// A dimension the files counted nothing for carries no member at all, rather than a zero
/// nothing measured.
fn coverage_summary(tally: code_coverage::CoverageTally) -> CodeCoverageReportSummary {
    CodeCoverageReportSummary {
        line_coverage_percentage: tally.lines.map(percentage),
        lines_covered: tally.lines.map(|(covered, _)| count(covered)),
        lines_missed: tally.lines.map(|(_, missed)| count(missed)),
        branch_coverage_percentage: tally.branches.map(percentage),
        branches_covered: tally.branches.map(|(covered, _)| count(covered)),
        branches_missed: tally.branches.map(|(_, missed)| count(missed)),
    }
}

/// The share of one dimension that is covered, as the percentage `Percentage`
/// (`@range(min: 0, max: 100)`) carries: the covered count over the whole of it, to two
/// decimal places, and zero for a dimension with nothing in it to cover.
fn percentage((covered, missed): (i64, i64)) -> f64 {
    let total = covered.saturating_add(missed);
    if total <= 0 {
        return 0.0;
    }
    let share = covered as f64 / total as f64 * 100.0;
    (share * 100.0).round() / 100.0
}

/// One counted total as `NonNegativeInt` (`@range(min: 0)`) carries it.
fn count(counted: i64) -> i32 {
    i32::try_from(counted.max(0)).unwrap_or(i32::MAX)
}

/// A report run's unique identifier — the id a report ARN carries after its group's name.
///
/// Derived the way every other identifier of this service is ([`super::build_uuid`]), from
/// the build that produced the run, the group the run belongs to, the run's position among
/// the runs of that build and the instant it was produced at, so two runs of one build can
/// never collide.
#[must_use]
pub fn report_uuid(
    build_id: &str,
    report_group_name: &str,
    sequence: i64,
    created_millis: i64,
) -> String {
    super::derived_uuid(
        "report",
        &format!("{build_id}/{report_group_name}"),
        sequence,
        created_millis,
    )
}

/// Write the report runs `runs` describes into the store, as the reports of the groups the
/// build's buildspec named, and answer the ARN of each run that was written.
///
/// A run whose group cannot take it writes nothing and is not answered: an ARN naming a
/// group the account does not hold names no group to put a run in, and a group holds one
/// `ReportType`, so a run of the other kind belongs to no group of that name. A plain name is
/// the group `<project name>-<name>`, which is created here when the account holds none —
/// the one place beside `CreateReportGroup` a report group is written, and bounded by the
/// same [`MAX_REPORT_GROUPS`].
pub fn record_build_reports(
    state: &mut CodeBuildState,
    claimed: &super::ClaimedRun,
    region: &str,
    runs: &[ReportRun],
) -> Vec<String> {
    let created_millis = claimed.due_millis;
    let mut written = Vec::new();
    for (sequence, run) in runs.iter().enumerate() {
        let Some(group) = group_of(state, run, &claimed.project_name, region, created_millis) else {
            ::tracing::debug!(
                declared_group = %run.declared_group,
                report_type = %run.report_type,
                build = %claimed.id,
                "a build's report run named no report group of this account that holds its kind \
                 of report"
            );
            continue;
        };
        let report_id = report_uuid(
            &claimed.id,
            &group.name,
            i64::try_from(sequence).unwrap_or_default(),
            created_millis,
        );
        let arn = report_arn(region, &group.name, &report_id);
        let report = Report {
            arn: Some(arn.clone()),
            r#type: Some(run.report_type.to_string()),
            // "The name of the report that was run", which is the group whose series of runs
            // it is one of.
            name: Some(group.name.clone()),
            report_group_arn: Some(group.arn.clone()),
            // "The ARN of the build run that generated this report."
            execution_id: claimed.build.arn.clone(),
            status: Some(run.status.to_string()),
            created: Some(datetime(created_millis)),
            expired: Some(datetime(created_millis + REPORT_RETENTION_MILLIS)),
            // "Information about where the raw data used to generate this report was
            // exported": the group's own configuration is stated only where it exports the
            // raw data NOWHERE, since this build put the raw data at no other destination.
            export_config: group.export_config.filter(|export| {
                export.export_config_type.as_deref() == Some(NO_EXPORT)
            }),
            truncated: Some(run.truncated),
            test_summary: run.test_summary.clone(),
            code_coverage_summary: run.code_coverage_summary.clone(),
        };
        let expired = report.expired;
        state.reports.insert(
            arn.clone(),
            StoredReport {
                report,
                // The cases this run was read from, each as the report that holds them
                // reports it: they are written once, by the build that minted the run, and
                // never again.
                test_cases: run
                    .test_cases
                    .iter()
                    .map(|case| test_case(case, &arn, expired))
                    .collect(),
            },
        );
        written.push(arn);
    }
    enforce_report_ceiling(state);
    written
}

/// One report run as the store holds it: the report a reader reports, and the test cases
/// `DescribeTestCases` lists for it.
///
/// The cases live with the report rather than in a map beside it, because they are the
/// report's own data: a run is minted with them by the build that produced it, and evicting
/// or deleting the report takes them with it.
#[derive(Debug, Clone)]
pub struct StoredReport {
    /// The report itself, as every reader of a report reports it.
    pub report: Report,
    /// The cases the run was read from, in the order the files stated them — empty for a
    /// run of code coverage, and for one whose files held no case.
    pub test_cases: Vec<TestCase>,
}

/// One read case as `TestCase` reports it, for the report `report_arn`.
///
/// `reportArn` is the report the case belongs to and `expired` is that report's own expiry —
/// the case is part of the run's raw data, so it is readable for exactly as long as the run
/// is. Every other member is what the result file stated ([`TestResult`]); one it stated
/// nothing for is absent.
fn test_case(
    read: &TestResult,
    report_arn: &str,
    expired: Option<::aws_smithy_types::DateTime>,
) -> TestCase {
    TestCase {
        report_arn: Some(report_arn.to_string()),
        test_raw_data_path: read.raw_data_path.clone(),
        prefix: read.prefix.clone(),
        name: read.name.clone(),
        status: Some(read.status.to_string()),
        duration_in_nano_seconds: Some(read.duration_nanos),
        message: read.message.clone(),
        expired,
        test_suite_name: read.suite_name.clone(),
    }
}

/// The report group one run belongs to, as the parts a report states of it.
struct ReportGroupOfRun {
    /// The group's own name, which the report states as its own.
    name: String,
    /// The group's own ARN, as the group states it.
    arn: String,
    /// What the group states about where its raw data is exported.
    export_config: Option<ReportExportConfig>,
}

/// The group `run` belongs to, creating it when the run named a plain name the account holds
/// no group under — and `None` when the run belongs to no group at all.
fn group_of(
    state: &mut CodeBuildState,
    run: &ReportRun,
    project_name: &str,
    region: &str,
    created_millis: i64,
) -> Option<ReportGroupOfRun> {
    let declared = run.declared_group.trim();
    if declared.is_empty() {
        return None;
    }
    // "If you specify a report group ARN, CodeBuild uses the existing report group": an ARN
    // names a group the account already holds and nothing is created for it.
    if is_arn(declared) {
        let name = report_group_name_from_arn(declared)?.to_string();
        let held = state.report_group_by_arn(declared)?;
        return holding(&held.report_group, run.report_type).map(|export_config| {
            ReportGroupOfRun {
                name,
                arn: held.report_group.arn.clone().unwrap_or_default(),
                export_config,
            }
        });
    }
    // "If you specify a report group name, CodeBuild creates a report group using your
    // project name and the name you specify, in the format project-name-report-group-name."
    let name = format!("{project_name}-{declared}");
    if let Some(held) = state.report_groups.get(&name) {
        return holding(&held.report_group, run.report_type).map(|export_config| {
            ReportGroupOfRun {
                name: held.report_group.name.clone().unwrap_or(name),
                arn: held.report_group.arn.clone().unwrap_or_default(),
                export_config,
            }
        });
    }
    if state.report_groups.len() >= MAX_REPORT_GROUPS {
        return None;
    }
    let arn = report_group_arn(region, &name);
    let created = ReportGroup {
        arn: Some(arn.clone()),
        name: Some(name.clone()),
        // The group takes the kind of report the build produces for it, since that is the
        // only kind of run it will ever hold.
        r#type: Some(run.report_type.to_string()),
        // A group the BUILD created was configured by nobody, so its raw data is exported
        // nowhere until `UpdateReportGroup` says otherwise.
        export_config: Some(ReportExportConfig {
            export_config_type: Some(NO_EXPORT.to_string()),
            s3_destination: None,
        }),
        created: Some(datetime(created_millis)),
        last_modified: Some(datetime(created_millis)),
        tags: Some(Vec::new()),
        status: Some(REPORT_GROUP_ACTIVE_STATUS.to_string()),
    };
    state.report_groups.insert(
        name.clone(),
        StoredReportGroup {
            report_group: created.clone(),
            created_millis,
            last_modified_millis: created_millis,
        },
    );
    Some(ReportGroupOfRun {
        name,
        arn,
        export_config: created.export_config,
    })
}

/// What `group` states about its exports, when it is a group that holds `report_type` runs —
/// and `None` when it holds the other kind, there being one `ReportType` per group.
fn holding(group: &ReportGroup, report_type: &str) -> Option<Option<ReportExportConfig>> {
    (group.r#type.as_deref() == Some(report_type)).then(|| group.export_config.clone())
}

/// Evict report runs until at most [`MAX_REPORTS`] are retained, oldest run first.
fn enforce_report_ceiling(state: &mut CodeBuildState) {
    while state.reports.len() > MAX_REPORTS {
        let oldest = state
            .reports
            .iter()
            .min_by_key(|(arn, stored)| {
                (
                    stored.report.created.as_ref().map_or(i64::MIN, |created| {
                        created.secs()
                    }),
                    (*arn).clone(),
                )
            })
            .map(|(arn, _)| arn.clone());
        let Some(arn) = oldest else { break };
        state.reports.remove(&arn);
    }
}

/// What CodeBuild states as the reason the ARN a read of ONE report's contents names
/// addresses no report of this account, carried by the modelled
/// `ResourceNotFoundException`.
///
/// Wholly fixed text — recorded behaviour, as the model states no message for the error:
/// the sentence names the absent resource and the one state a held report is unreadable in,
/// and quotes neither the ARN the request sent nor the member it arrived on, so every
/// unresolvable report ARN is refused with the same words and a client can key on them.
pub const REPORT_NOT_FOUND_MESSAGE: &str = "Report does not exist or is in DELETING status";

/// The cases of one report `filter` selects, in the order the report holds them.
///
/// `TestCaseFilter` is "a filter used to return specific types of test cases. In order to
/// pass the filter, the report must meet ALL of the filter properties": `status` keeps the
/// cases of that status — one of the five fixed words `TestCase$status` names, so it is
/// matched exactly — and `keyword` keeps those "where the keyword is a substring of the
/// `name` or the `prefix`". A member the filter does not state constrains nothing, and a
/// filter stating neither selects every case.
#[must_use]
pub fn filtered_test_cases<'a>(
    cases: &'a [TestCase],
    filter: Option<&TestCaseFilter>,
) -> Vec<&'a TestCase> {
    let status = filter.and_then(|filter| filter.status.as_deref());
    let keyword = filter.and_then(|filter| filter.keyword.as_deref());
    cases
        .iter()
        .filter(|case| status.is_none_or(|status| case.status.as_deref() == Some(status)))
        .filter(|case| {
            keyword.is_none_or(|keyword| {
                [case.name.as_deref(), case.prefix.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|held| held.contains(keyword))
            })
        })
        .collect()
}

/// One page of `cases`, resuming where `next_token` left off, and the token of the page
/// after it — `None` for a token that opens no page of this report's cases, which is a
/// refusable request rather than an empty page.
///
/// A test case has no id of its own to resume by, so what a token names is the case's
/// ORDINAL within the report: a report's cases are written ONCE, by the build that minted the
/// run, and never change, so nothing can be inserted before the entry a token names. The cut
/// itself is this service's one pager ([`super::page_limited`]), so the page size a request
/// may ask for, and the refusal of a token that opens no page, are decided in one place for
/// every paginated operation.
#[must_use]
pub fn page_of_test_cases(
    cases: &[&TestCase],
    next_token: Option<&str>,
    max_results: Option<i32>,
) -> Option<(Vec<TestCase>, Option<String>)> {
    let ordinals: Vec<String> = (0..cases.len()).map(|at| at.to_string()).collect();
    let (page, next_token) = super::page_limited(&ordinals, next_token, max_results)?;
    let page: Vec<TestCase> = page
        .iter()
        .filter_map(|ordinal| ordinal.parse::<usize>().ok())
        .filter_map(|at| cases.get(at))
        .map(|case| (*case).clone())
        .collect();
    Some((page, next_token))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A section declaring `files` and a `file-format`.
    fn declaration(files: &[&str], file_format: Option<&str>) -> ReportDeclaration {
        ReportDeclaration {
            files: ArtifactDeclaration {
                files: files.iter().map(|file| (*file).to_string()).collect(),
                ..ArtifactDeclaration::default()
            },
            file_format: file_format.map(str::to_string),
        }
    }

    /// RULE: a declared format decides the kind of report a run is, and a section that
    /// declares none is read as the default format the buildspec reference states.
    #[test]
    fn the_declared_format_decides_the_kind_of_report() {
        // (case, declared, the type a run of it is)
        let rows: Vec<(&str, Option<&str>, Option<&str>)> = vec![
            ("no format at all", None, Some(REPORT_TYPE_TEST)),
            ("the default, declared", Some("JUNITXML"), Some(REPORT_TYPE_TEST)),
            ("in another case", Some("junitxml"), Some(REPORT_TYPE_TEST)),
            ("cucumber's JSON", Some("CUCUMBERJSON"), Some(REPORT_TYPE_TEST)),
            ("NUnit 2", Some("NUNITXML"), Some(REPORT_TYPE_TEST)),
            ("NUnit 3", Some("NUNIT3XML"), Some(REPORT_TYPE_TEST)),
            ("TestNG", Some("TESTNGXML"), Some(REPORT_TYPE_TEST)),
            (
                "Visual Studio",
                Some("VISUALSTUDIOTRX"),
                Some(REPORT_TYPE_TEST),
            ),
            ("Clover", Some("CLOVERXML"), Some(REPORT_TYPE_CODE_COVERAGE)),
            (
                "Cobertura",
                Some("COBERTURAXML"),
                Some(REPORT_TYPE_CODE_COVERAGE),
            ),
            ("JaCoCo", Some("JACOCOXML"), Some(REPORT_TYPE_CODE_COVERAGE)),
            ("SimpleCov", Some("SIMPLECOV"), Some(REPORT_TYPE_CODE_COVERAGE)),
            ("a word that names no format", Some("TAPYAML"), None),
        ];
        for (case, declared, expected) in rows {
            let read = ReportFormat::declared(declared).map(ReportFormat::report_type);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a run's data is read from the files its section declared, and its status is what
    /// those results say — `SUCCEEDED` when every case passed, `FAILED` when one did not, and
    /// `INCOMPLETE` when the files held no result at all.
    #[test]
    fn a_runs_status_and_summary_are_what_its_files_held() {
        let produced: Vec<(String, String)> = [
            (
                "reports/passing.xml",
                "<testsuite><testcase name=\"a\" time=\"0.5\"/>\
                 <testcase name=\"b\" time=\"0.25\"/></testsuite>",
            ),
            (
                "reports/failing.xml",
                "<testsuite><testcase name=\"c\"><failure/></testcase>\
                 <testcase name=\"d\"><skipped/></testcase></testsuite>",
            ),
            ("reports/empty.xml", "<testsuite/>"),
            (
                "coverage/jacoco.xml",
                "<report><counter type=\"LINE\" missed=\"1\" covered=\"3\"/></report>",
            ),
        ]
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.to_string()))
        .collect();

        let declared: BTreeMap<String, ReportDeclaration> = BTreeMap::from([
            (
                "passing".to_string(),
                declaration(&["reports/passing.xml"], None),
            ),
            (
                "mixed".to_string(),
                declaration(&["reports/passing.xml", "reports/failing.xml"], None),
            ),
            (
                "empty".to_string(),
                declaration(&["reports/empty.xml"], None),
            ),
            (
                "missing".to_string(),
                declaration(&["reports/never-written.xml"], None),
            ),
            (
                "coverage".to_string(),
                declaration(&["coverage/jacoco.xml"], Some("JACOCOXML")),
            ),
        ]);

        let runs = report_runs(&declared, &produced);
        let read: BTreeMap<&str, &str> = runs
            .iter()
            .map(|run| (run.declared_group.as_str(), run.status))
            .collect();
        assert_eq!(
            read,
            BTreeMap::from([
                ("passing", REPORT_STATUS_SUCCEEDED),
                ("mixed", REPORT_STATUS_FAILED),
                ("empty", REPORT_STATUS_INCOMPLETE),
                ("missing", REPORT_STATUS_INCOMPLETE),
                ("coverage", REPORT_STATUS_SUCCEEDED),
            ]),
            "each run's status must be what its own files held, got {read:?}"
        );

        let passing = runs
            .iter()
            .find(|run| run.declared_group == "passing")
            .expect("the run of the passing group");
        let summary = passing
            .test_summary
            .clone()
            .expect("a run read from test cases carries a summary");
        assert_eq!(
            (summary.total, summary.duration_in_nano_seconds),
            (2, 750_000_000),
            "case \"passing\": the summary counts the cases read and their total span, \
             got {summary:?}"
        );
        assert_eq!(
            summary.status_counts,
            HashMap::from([(STATUS_SUCCEEDED.to_string(), 2)]),
            "case \"passing\": only the statuses the cases ended in are counted, \
             got {summary:?}"
        );

        let mixed = runs
            .iter()
            .find(|run| run.declared_group == "mixed")
            .expect("the run of the mixed group");
        let summary = mixed.test_summary.clone().expect("a summary");
        assert_eq!(
            summary.status_counts,
            HashMap::from([
                (STATUS_SUCCEEDED.to_string(), 2),
                (STATUS_FAILED.to_string(), 1),
                (STATUS_SKIPPED.to_string(), 1),
            ]),
            "case \"mixed\": every declared file's cases are counted, got {summary:?}"
        );

        let coverage = runs
            .iter()
            .find(|run| run.declared_group == "coverage")
            .expect("the run of the coverage group");
        assert_eq!(
            coverage.report_type, REPORT_TYPE_CODE_COVERAGE,
            "case \"coverage\": a coverage format is a coverage report, got {coverage:?}"
        );
        let counted = coverage
            .code_coverage_summary
            .clone()
            .expect("a run read from coverage files carries a summary");
        assert_eq!(
            (
                counted.line_coverage_percentage,
                counted.lines_covered,
                counted.lines_missed,
                counted.branch_coverage_percentage,
                counted.branches_covered,
                counted.branches_missed,
            ),
            (Some(75.0), Some(3), Some(1), None, None, None),
            "case \"coverage\": the summary is what the file counted, and a dimension it \
             counted nothing for carries no member, got {counted:?}"
        );
        assert!(
            coverage.test_summary.is_none() && mixed.code_coverage_summary.is_none(),
            "a run carries only the summary of the kind of report it is"
        );
    }
}
