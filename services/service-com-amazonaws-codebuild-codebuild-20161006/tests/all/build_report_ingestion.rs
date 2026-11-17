// SPDX-License-Identifier: CC0-1.0

//! What a BUILD produces for the report groups its buildspec declares, driven end to end
//! over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! RULE under test: a buildspec's `reports` section declares, per report group, which files
//! of the build's working tree hold that group's results and what format they are in; a build
//! that ran mints ONE report run per declared group, whose data is read from those very files
//! and whose status is what they say — `SUCCEEDED` when every test case passed, `FAILED` when
//! one did not, `INCOMPLETE` when the declared files held no result at all. A report group
//! ARN names a group the account already holds; a plain name is the group
//! `<project name>-<declared name>`, which the build creates when the account holds none. The
//! runs a group holds are then what `ListReportsForReportGroup` lists, what
//! `BatchGetReports` reports and what `GetReportGroupTrend` derives its trend from. The BUILD
//! that minted a run states that run's ARN in its own `reportArns`, and the run's raw data is
//! the test cases `DescribeTestCases` reports for it — one per case the declared file stated,
//! with the status, identity, span and message that file gave it.
//!
//! Each row below differs along what the rule generalises over: the declared `file-format`
//! (every word the buildspec reference lists, test and code coverage alike), what the results
//! in the file say, how the group is named, and whether the declared files were written at
//! all. Nothing is seeded into the store: every report asserted here was produced by a build
//! whose own commands wrote the file it was read from.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state::{
    self, ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState,
};
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{
    call, create_project, emulator, emulator_on, error_type, settled_build, REGION,
};
use super::report_group_lifecycle::create_report_group_body;

/// The `ReportType` of a group whose runs are test reports, and of one whose runs are code
/// coverage reports — the model's own `ReportType` catalogue.
const TYPE_TEST: &str = "TEST";
/// See [`TYPE_TEST`].
const TYPE_CODE_COVERAGE: &str = "CODE_COVERAGE";

/// The span AWS documents on `Report$expired`: "A report expires 30 days after it is
/// created."
const EXPIRY_SECONDS: i64 = 30 * 24 * 60 * 60;

/// The file a case's buildspec writes its results into.
const RESULTS_FILE: &str = "results-file.xml";

/// One buildspec command line, as a YAML double-quoted scalar: the documents written below
/// carry the double quotes an XML attribute and a JSON member are written with, so the
/// command is quoted in the one YAML form that escapes them.
fn command_line(command: &str) -> String {
    format!(
        "      - \"{}\"\n",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// A buildspec whose build writes `document` into [`RESULTS_FILE`] and declares the report
/// group `group` to be read from the files `files` name, in `file_format` when the case
/// declares one.
fn buildspec(group: &str, document: &str, files: &str, file_format: Option<&str>) -> String {
    let format_line = file_format
        .map(|format| format!("    file-format: {format}\n"))
        .unwrap_or_default();
    format!(
        "version: 0.2\nphases:\n  build:\n    commands:\n{}reports:\n  \"{group}\":\n    \
         files:\n      - \"{files}\"\n{format_line}",
        command_line(&format!("echo '{document}' > {RESULTS_FILE}"))
    )
}

/// A `CreateProject` source carrying `buildspec` inline, with no input source code of its
/// own — the form that lets a suite fix exactly what a build runs and declares.
fn source_with(buildspec: &str) -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}})
}

/// Create the report group `name` of `report_type` over the wire, asserting it succeeded, and
/// hand back the ARN the service minted for it.
async fn create_group(router: &MultiServiceRouter, name: &str, report_type: &str) -> String {
    let body = create_report_group_body(
        name,
        report_type,
        json!({"exportConfigType": "NO_EXPORT"}),
        None,
    );
    let (status, response) = call(router, "CreateReportGroup", body).await;
    assert_eq!(
        status, 200,
        "CreateReportGroup {name:?}: expected 200, got {status} with {response}"
    );
    response["reportGroup"]["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The report ARNs `ListReportsForReportGroup` lists for `group_arn`, with `extra`'s members
/// merged into the request, asserting the call succeeded.
async fn listed(router: &MultiServiceRouter, group_arn: &str, extra: Value) -> (Vec<String>, Value) {
    let mut body = json!({"reportGroupArn": group_arn});
    if let (Value::Object(target), Value::Object(more)) = (&mut body, extra) {
        target.extend(more);
    }
    let (status, response) = call(router, "ListReportsForReportGroup", body.clone()).await;
    assert_eq!(
        status, 200,
        "ListReportsForReportGroup {body}: expected 200, got {status} with {response}"
    );
    let arns = response["reports"]
        .as_array()
        .expect("reports is a list")
        .iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect();
    (arns, response)
}

/// The report records `BatchGetReports` reports for `arns`, asserting the call succeeded.
async fn read_reports(router: &MultiServiceRouter, arns: &[String]) -> Vec<Value> {
    let (status, response) = call(router, "BatchGetReports", json!({"reportArns": arns})).await;
    assert_eq!(
        status, 200,
        "BatchGetReports {arns:?}: expected 200, got {status} with {response}"
    );
    response["reports"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Run one build of a project whose buildspec declares `group`, and hand back the build and
/// the ONE report run the group holds afterwards.
async fn produced_report(
    router: &MultiServiceRouter,
    project: &str,
    group_arn: &str,
    buildspec: &str,
) -> (Value, Value) {
    create_project(router, project, source_with(buildspec)).await;
    let build = settled_build(router, project, json!({})).await;
    let (arns, response) = listed(router, group_arn, json!({})).await;
    assert_eq!(
        arns.len(),
        1,
        "project {project:?}: expected exactly one run listed for its group, got {response}"
    );
    let reports = read_reports(router, &arns).await;
    let report = reports
        .first()
        .cloned()
        .unwrap_or(Value::Null);
    (build, report)
}

/// RULE: a run's data and status are read from the files its section declared, in the format
/// it declared them in — one row per `file-format` word the buildspec reference lists, and
/// one for declared files the build never wrote.
#[tokio::test]
async fn a_report_run_is_read_from_the_files_the_build_wrote() {
    let router = emulator();

    /// One row: the case, the declared `file-format`, the document the build writes, the
    /// files the section declares, the `ReportType` of the group, the status the run must
    /// end in, and the summary it must carry under that type's member.
    type Row = (
        &'static str,
        Option<&'static str>,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        Value,
    );
    let rows: Vec<Row> = vec![
        (
            "the default format, declared by nothing, with a failing case",
            None,
            "<testsuites><testsuite name=\"suite\"><testcase name=\"a\" time=\"0.5\"/>\
             <testcase name=\"b\" time=\"0.25\"/><testcase name=\"c\"><failure message=\"no\"/>\
             </testcase></testsuite></testsuites>",
            RESULTS_FILE,
            TYPE_TEST,
            "FAILED",
            json!({
                "total": 3,
                "statusCounts": {"SUCCEEDED": 2, "FAILED": 1},
                "durationInNanoSeconds": 750_000_000i64,
            }),
        ),
        (
            "JUnit XML whose every case passed",
            Some("JUNITXML"),
            "<testsuite><testcase name=\"a\" time=\"1\"/><testcase name=\"b\" time=\"0.5\"/>\
             </testsuite>",
            RESULTS_FILE,
            TYPE_TEST,
            "SUCCEEDED",
            json!({
                "total": 2,
                "statusCounts": {"SUCCEEDED": 2},
                "durationInNanoSeconds": 1_500_000_000i64,
            }),
        ),
        (
            "NUnit 2 XML, stating `executed` and `success`",
            Some("NUNITXML"),
            "<test-results><test-suite><results>\
             <test-case name=\"a\" executed=\"True\" success=\"True\" time=\"2\"/>\
             <test-case name=\"b\" executed=\"True\" success=\"False\"/>\
             </results></test-suite></test-results>",
            RESULTS_FILE,
            TYPE_TEST,
            "FAILED",
            json!({
                "total": 2,
                "statusCounts": {"SUCCEEDED": 1, "FAILED": 1},
                "durationInNanoSeconds": 2_000_000_000i64,
            }),
        ),
        (
            "NUnit 3 XML, stating one `result` word",
            Some("NUNIT3XML"),
            "<test-run><test-case name=\"a\" result=\"Passed\" duration=\"0.25\"/>\
             <test-case name=\"b\" result=\"Skipped\"/></test-run>",
            RESULTS_FILE,
            TYPE_TEST,
            "SUCCEEDED",
            json!({
                "total": 2,
                "statusCounts": {"SUCCEEDED": 1, "SKIPPED": 1},
                "durationInNanoSeconds": 250_000_000i64,
            }),
        ),
        (
            "TestNG XML, stating a span in milliseconds",
            Some("TESTNGXML"),
            "<testng-results><suite><test><class name=\"C\">\
             <test-method status=\"PASS\" name=\"a\" duration-ms=\"12\"/>\
             <test-method status=\"SKIP\" name=\"b\" duration-ms=\"0\"/>\
             </class></test></suite></testng-results>",
            RESULTS_FILE,
            TYPE_TEST,
            "SUCCEEDED",
            json!({
                "total": 2,
                "statusCounts": {"SUCCEEDED": 1, "SKIPPED": 1},
                "durationInNanoSeconds": 12_000_000i64,
            }),
        ),
        (
            "a Visual Studio document, stating a HH:MM:SS span and an errored case",
            Some("VISUALSTUDIOTRX"),
            "<TestRun><Results>\
             <UnitTestResult testName=\"a\" outcome=\"Passed\" duration=\"00:00:01.5000000\"/>\
             <UnitTestResult testName=\"b\" outcome=\"Error\"/></Results></TestRun>",
            RESULTS_FILE,
            TYPE_TEST,
            "FAILED",
            json!({
                "total": 2,
                "statusCounts": {"SUCCEEDED": 1, "ERROR": 1},
                "durationInNanoSeconds": 1_500_000_000i64,
            }),
        ),
        (
            "Cucumber JSON, one case per scenario and spans in nanoseconds",
            Some("CUCUMBERJSON"),
            "[{\"name\":\"f\",\"elements\":[\
             {\"name\":\"one\",\"steps\":[{\"result\":{\"status\":\"passed\",\
             \"duration\":300}}]},\
             {\"name\":\"two\",\"steps\":[{\"result\":{\"status\":\"failed\",\
             \"duration\":200}}]}]}]",
            RESULTS_FILE,
            TYPE_TEST,
            "FAILED",
            json!({
                "total": 2,
                "statusCounts": {"SUCCEEDED": 1, "FAILED": 1},
                "durationInNanoSeconds": 500i64,
            }),
        ),
        (
            "JaCoCo XML, counting lines and branches",
            Some("JACOCOXML"),
            "<report name=\"r\"><counter type=\"LINE\" missed=\"5\" covered=\"15\"/>\
             <counter type=\"BRANCH\" missed=\"2\" covered=\"6\"/></report>",
            RESULTS_FILE,
            TYPE_CODE_COVERAGE,
            "SUCCEEDED",
            json!({
                "lineCoveragePercentage": 75.0,
                "linesCovered": 15,
                "linesMissed": 5,
                "branchCoveragePercentage": 75.0,
                "branchesCovered": 6,
                "branchesMissed": 2,
            }),
        ),
        (
            "Cobertura XML, stating its totals on the root element",
            Some("COBERTURAXML"),
            "<coverage lines-valid=\"10\" lines-covered=\"8\" branches-valid=\"4\" \
             branches-covered=\"3\"/>",
            RESULTS_FILE,
            TYPE_CODE_COVERAGE,
            "SUCCEEDED",
            json!({
                "lineCoveragePercentage": 80.0,
                "linesCovered": 8,
                "linesMissed": 2,
                "branchCoveragePercentage": 75.0,
                "branchesCovered": 3,
                "branchesMissed": 1,
            }),
        ),
        (
            "Clover XML, counting statements and conditionals",
            Some("CLOVERXML"),
            "<coverage><project><metrics statements=\"20\" coveredstatements=\"13\" \
             conditionals=\"6\" coveredconditionals=\"2\"/></project></coverage>",
            RESULTS_FILE,
            TYPE_CODE_COVERAGE,
            "SUCCEEDED",
            json!({
                "lineCoveragePercentage": 65.0,
                "linesCovered": 13,
                "linesMissed": 7,
                "branchCoveragePercentage": 33.33,
                "branchesCovered": 2,
                "branchesMissed": 4,
            }),
        ),
        (
            "a SimpleCov result set, which counts no branch",
            Some("SIMPLECOV"),
            "{\"RSpec\":{\"coverage\":{\"a.rb\":{\"lines\":[1,0,null,3]}},\"timestamp\":1}}",
            RESULTS_FILE,
            TYPE_CODE_COVERAGE,
            "SUCCEEDED",
            json!({
                "lineCoveragePercentage": 66.67,
                "linesCovered": 2,
                "linesMissed": 1,
            }),
        ),
        (
            "declared files the build never wrote",
            Some("JUNITXML"),
            "<testsuite><testcase name=\"a\"/></testsuite>",
            "never-written.xml",
            TYPE_TEST,
            "INCOMPLETE",
            Value::Null,
        ),
    ];

    for (index, (case, file_format, document, files, report_type, status, summary)) in
        rows.iter().enumerate()
    {
        let group = format!("report-ingest-{index}");
        let group_arn = create_group(&router, &group, report_type).await;
        let project = format!("report-ingest-proj-{index}");
        let spec = buildspec(&group_arn, document, files, *file_format);
        let (build, report) = produced_report(&router, &project, &group_arn, &spec).await;

        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the build's own commands all exited zero, so it must have \
             succeeded, got {build}"
        );
        assert_eq!(
            report["status"],
            json!(status),
            "case {case:?}: expected the run's status to be {status}, got {report}"
        );
        assert_eq!(
            report["type"],
            json!(report_type),
            "case {case:?}: the declared format decides the kind of report, got {report}"
        );
        let member = if *report_type == TYPE_TEST {
            "testSummary"
        } else {
            "codeCoverageSummary"
        };
        assert_eq!(
            report[member], *summary,
            "case {case:?}: expected {member} {summary}, got {report}"
        );
        // A run carries only the summary of the kind of report it is.
        let other = if *report_type == TYPE_TEST {
            "codeCoverageSummary"
        } else {
            "testSummary"
        };
        assert_eq!(
            report[other],
            Value::Null,
            "case {case:?}: a run must carry no {other}, got {report}"
        );

        // The run states which group's series it belongs to, which build produced it, and
        // when it occurred — all of them the build's and the group's own values.
        assert_eq!(
            report["name"],
            json!(group),
            "case {case:?}: a run is named for the group whose series it is one of, \
             got {report}"
        );
        assert_eq!(
            report["reportGroupArn"],
            json!(group_arn),
            "case {case:?}: expected the declared group's ARN, got {report}"
        );
        assert_eq!(
            report["executionId"], build["arn"],
            "case {case:?}: a run states the ARN of the build that generated it, got {report}"
        );
        assert_eq!(
            report["truncated"],
            json!(false),
            "case {case:?}: nothing was truncated, got {report}"
        );
        assert_eq!(
            report["exportConfig"],
            json!({"exportConfigType": "NO_EXPORT"}),
            "case {case:?}: the group exports its raw data nowhere, got {report}"
        );
        // awsJson1_1 renders a timestamp as epoch seconds, fractional part included.
        let created = report["created"].as_f64().unwrap_or_default();
        let expired = report["expired"].as_f64().unwrap_or_default();
        assert_eq!(
            expired - created,
            EXPIRY_SECONDS as f64,
            "case {case:?}: a run expires 30 days after it is created, got {report}"
        );
        let arn = report["arn"].as_str().unwrap_or_default();
        assert!(
            arn.starts_with(&format!(
                "arn:aws:codebuild:{REGION}:{}:report/{group}:",
                engine::EMULATOR_ACCOUNT_ID
            )),
            "case {case:?}: a run's ARN names its group and its own id, got {arn:?}"
        );
    }
}

/// RULE: a `reports` section naming a plain name belongs to the group
/// `<project name>-<declared name>`, which the build CREATES when the account holds none, and
/// every later build of that project adds a run to that same group.
#[tokio::test]
async fn a_plain_name_is_the_projects_own_group_and_the_build_creates_it() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let project = "report-plain-proj";
    let declared = "unit-tests";
    let group = format!("{project}-{declared}");
    let group_arn = state::report_group_arn(REGION, &group);

    let spec = buildspec(
        declared,
        "<testsuite><testcase name=\"a\" time=\"0.5\"/></testsuite>",
        RESULTS_FILE,
        None,
    );
    create_project(&router, project, source_with(&spec)).await;
    let first = settled_build(&router, project, json!({})).await;
    assert_eq!(
        first["buildStatus"],
        json!("SUCCEEDED"),
        "the build's commands all exited zero, got {first}"
    );

    // The group the build created is a group of this account, readable through the operation
    // that reads report groups.
    let (status, read) = call(
        &router,
        "BatchGetReportGroups",
        json!({"reportGroupArns": [group_arn.clone()]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetReportGroups: expected 200, got {status} with {read}"
    );
    let created_group = read["reportGroups"][0].clone();
    assert_eq!(
        (
            created_group["name"].clone(),
            created_group["type"].clone(),
            created_group["status"].clone(),
            created_group["exportConfig"].clone(),
            created_group["tags"].clone(),
        ),
        (
            json!(group),
            json!(TYPE_TEST),
            json!("ACTIVE"),
            json!({"exportConfigType": "NO_EXPORT"}),
            json!([]),
        ),
        "the group a build created takes the kind of report the build produced for it and is \
         configured by nobody, got {read}"
    );

    let (first_arns, response) = listed(&router, &group_arn, json!({})).await;
    assert_eq!(
        first_arns.len(),
        1,
        "the first build must have produced one run, got {response}"
    );

    // A second build of the same project adds a run to the SAME group rather than creating
    // another one.
    let second = settled_build(&router, project, json!({})).await;
    let (second_arns, response) = listed(&router, &group_arn, json!({})).await;
    assert_eq!(
        second_arns.len(),
        2,
        "a second build must add a second run to the same group, got {response}"
    );
    assert!(
        second_arns.contains(&first_arns[0]),
        "the first build's run must still be listed, got {response}"
    );
    let reports = read_reports(&router, &second_arns).await;
    let newest = reports
        .first()
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        newest["executionId"], second["arn"],
        "the newest run must be the second build's, got {newest}"
    );
    let groups_held = engine
        .state::<CodeBuildState>()
        .lock()
        .report_groups
        .len();
    assert_eq!(
        groups_held, 1,
        "the builds must share one created group, got {groups_held}"
    );
}

/// RULE: a run belongs only to a group that can hold it, so a section naming an ARN this
/// account holds no group under, or a group that holds the OTHER kind of report, mints no run
/// at all — the build still runs and still succeeds.
#[tokio::test]
async fn a_section_naming_no_group_that_can_hold_the_run_mints_none() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let coverage_group = create_group(&router, "coverage-only", TYPE_CODE_COVERAGE).await;
    let absent = state::report_group_arn(REGION, "no-such-group");

    // (case, the group the section names)
    let rows: Vec<(&str, String)> = vec![
        ("an ARN this account holds no group under", absent.clone()),
        (
            "a group that holds the other kind of report",
            coverage_group.clone(),
        ),
    ];
    for (index, (case, named)) in rows.iter().enumerate() {
        let project = format!("report-unheld-proj-{index}");
        let spec = buildspec(
            named,
            "<testsuite><testcase name=\"a\"/></testsuite>",
            RESULTS_FILE,
            Some("JUNITXML"),
        );
        create_project(&router, &project, source_with(&spec)).await;
        let build = settled_build(&router, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the build's own commands decide its status, got {build}"
        );
        let held = engine.state::<CodeBuildState>().lock().reports.len();
        assert_eq!(
            held, 0,
            "case {case:?}: expected no run to have been minted, got {held}"
        );
    }

    // The group that holds the other kind of report still holds no run, and the ARN no group
    // is held under is refused as not found.
    let (arns, response) = listed(&router, &coverage_group, json!({})).await;
    assert!(
        arns.is_empty(),
        "a group of the other kind must hold no run, got {response}"
    );
    let (status, body) = call(
        &router,
        "ListReportsForReportGroup",
        json!({"reportGroupArn": absent}),
    )
    .await;
    assert_eq!(
        (status, error_type(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "an ARN this account holds no group under has no runs to list, got {status} {body}"
    );
    assert_eq!(
        body["message"],
        json!(state::provided_report_group_absent_message(&absent)),
        "the refusal must state CodeBuild's sentence for a group that does not exist, \
         got {body}"
    );
}

/// RULE: `ListReportsForReportGroup` lists the group's OWN runs, newest first unless the
/// request asks for ascending order, filtered to one status when the request filters, and one
/// `maxResults` page at a time; a value that is not a report group ARN and a token that opens
/// no page are refused as input.
#[tokio::test]
async fn the_runs_of_a_group_are_listed_newest_first_and_paged() {
    let router = emulator();
    let group_arn = create_group(&router, "listed-runs", TYPE_TEST).await;
    let other_arn = create_group(&router, "listed-runs-other", TYPE_TEST).await;

    // Three builds of one project, each writing a different result: the third run is the
    // newest, and the one that failed is the only one a status filter keeps.
    let documents = [
        "<testsuite><testcase name=\"a\"/></testsuite>",
        "<testsuite><testcase name=\"b\"><failure/></testcase></testsuite>",
        "<testsuite><testcase name=\"c\"/></testsuite>",
    ];
    let project = "listed-runs-proj";
    let mut build_arns: Vec<Value> = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let project = format!("{project}-{index}");
        let spec = buildspec(&group_arn, document, RESULTS_FILE, None);
        create_project(&router, &project, source_with(&spec)).await;
        let build = settled_build(&router, &project, json!({})).await;
        build_arns.push(build["arn"].clone());
    }

    let (descending, response) = listed(&router, &group_arn, json!({})).await;
    assert_eq!(
        descending.len(),
        3,
        "every run of the group must be listed, got {response}"
    );
    // Which build produced which run is what fixes the order, since each run states the ARN
    // of the build that generated it.
    let produced_by: Vec<Value> = read_reports(&router, &descending)
        .await
        .iter()
        .map(|report| report["executionId"].clone())
        .collect();
    let newest_first: Vec<Value> = build_arns.iter().rev().cloned().collect();
    assert_eq!(
        produced_by, newest_first,
        "the runs must be listed most recent first, got {produced_by:?}"
    );

    let (ascending, response) = listed(&router, &group_arn, json!({"sortOrder": "ASCENDING"})).await;
    let reversed: Vec<String> = descending.iter().rev().cloned().collect();
    assert_eq!(
        ascending, reversed,
        "ascending order must be the same runs in the other order, got {response}"
    );
    let (explicit, _) = listed(&router, &group_arn, json!({"sortOrder": "DESCENDING"})).await;
    assert_eq!(
        explicit, descending,
        "descending order is the order a request stating none gets"
    );

    // One page at a time, resuming exactly where the previous page ended.
    let (first_page, response) = listed(&router, &group_arn, json!({"maxResults": 2})).await;
    assert_eq!(
        first_page,
        descending[..2].to_vec(),
        "a page of two must be the first two runs, got {response}"
    );
    let token = response["nextToken"]
        .as_str()
        .expect("a page that leaves a run unlisted states a token")
        .to_string();
    let (second_page, response) = listed(
        &router,
        &group_arn,
        json!({"maxResults": 2, "nextToken": token}),
    )
    .await;
    assert_eq!(
        second_page,
        descending[2..].to_vec(),
        "the next page must resume where the first ended, got {response}"
    );
    assert_eq!(
        response["nextToken"],
        Value::Null,
        "the last page must state no token, got {response}"
    );

    // "A ReportFilter object used to filter the returned reports."
    let (failed, response) = listed(
        &router,
        &group_arn,
        json!({"filter": {"status": "FAILED"}}),
    )
    .await;
    assert_eq!(
        failed.len(),
        1,
        "only the run whose own cases failed may be listed, got {response}"
    );
    let filtered = read_reports(&router, &failed).await;
    assert_eq!(
        filtered[0]["executionId"], build_arns[1],
        "the filtered run must be the build whose case failed, got {filtered:?}"
    );
    let (none, response) = listed(
        &router,
        &group_arn,
        json!({"filter": {"status": "DELETING"}}),
    )
    .await;
    assert!(
        none.is_empty(),
        "a status no run is in must list nothing, got {response}"
    );

    // Another group's runs are never this group's.
    let (other, response) = listed(&router, &other_arn, json!({})).await;
    assert!(
        other.is_empty(),
        "a group of its own must hold no run of another, got {response}"
    );

    // (case, the request, the error it is refused with, the sentence it states)
    let refusals: Vec<(&str, Value, &str, String)> = vec![
        (
            "a bare report group name",
            json!({"reportGroupArn": "listed-runs"}),
            "InvalidInputException",
            state::INVALID_REPORT_GROUP_ARN_MESSAGE.to_string(),
        ),
        (
            "an ARN of another resource of this service",
            json!({"reportGroupArn": state::project_arn(REGION, "listed-runs")}),
            "InvalidInputException",
            state::INVALID_REPORT_GROUP_ARN_MESSAGE.to_string(),
        ),
        (
            "an empty value",
            json!({"reportGroupArn": ""}),
            "InvalidInputException",
            state::INVALID_REPORT_GROUP_ARN_MESSAGE.to_string(),
        ),
        (
            "a token opening no page of this listing",
            json!({"reportGroupArn": group_arn.clone(), "nextToken": "not-a-token"}),
            "InvalidInputException",
            state::INVALID_CONTINUATION_TOKEN_MESSAGE.to_string(),
        ),
    ];
    for (case, body, expected_error, expected_message) in refusals {
        let (status, response) = call(&router, "ListReportsForReportGroup", body.clone()).await;
        assert_eq!(
            (status, error_type(&response)),
            (400, expected_error.to_string()),
            "case {case:?}: expected 400 {expected_error}, got {status} with {response}"
        );
        assert_eq!(
            response["message"],
            json!(expected_message),
            "case {case:?}: expected {expected_message:?}, got {response}"
        );
    }
}

/// The test cases `DescribeTestCases` reports for `report_arn`, with `extra`'s members merged
/// into the request, asserting the call succeeded.
async fn described(
    router: &MultiServiceRouter,
    report_arn: &str,
    extra: Value,
) -> (Vec<Value>, Value) {
    let mut body = json!({"reportArn": report_arn});
    if let (Value::Object(target), Value::Object(more)) = (&mut body, extra) {
        target.extend(more);
    }
    let (status, response) = call(router, "DescribeTestCases", body.clone()).await;
    assert_eq!(
        status, 200,
        "DescribeTestCases {body}: expected 200, got {status} with {response}"
    );
    let cases = response["testCases"]
        .as_array()
        .expect("testCases is a list")
        .clone();
    (cases, response)
}

/// `expected` with the members every case of the report `report_arn` carries whatever the
/// document said — the report it belongs to, the file it was read from and the instant the
/// report expires, which is the report's own.
fn with_report_members(expected: &Value, report_arn: &str, report: &Value) -> Value {
    let mut case = expected.clone();
    if let Value::Object(members) = &mut case {
        members.insert("reportArn".to_string(), json!(report_arn));
        members.insert("testRawDataPath".to_string(), json!(RESULTS_FILE));
        members.insert("expired".to_string(), report["expired"].clone());
    }
    case
}

/// RULE: a build that minted a report REPORTS that report's ARN in its own `reportArns`, and
/// the report's test cases are one per case the declared file stated — each carrying the
/// status that file gave it, the identity it stated (the case's own name, the series it
/// belongs to and the suite it was stated inside), the span it took and, for a case that did
/// not pass, the message the file holds for it. An ingested test failure is the REPORT's
/// outcome and not the build's, so the build still succeeds.
///
/// The rows differ along what the rule generalises over: the declared `file-format` (one per
/// test-result schema this reading reads), which statuses the cases ended in, which identity
/// members the document states, and where it states the message of a case that did not pass.
#[tokio::test]
async fn a_build_reports_the_reports_it_minted_and_each_of_their_cases() {
    let router = emulator();

    /// One row: the case, the declared `file-format`, the document the build writes, the
    /// status the run ends in, and the cases it must report, in order.
    type Row = (&'static str, Option<&'static str>, &'static str, &'static str, Vec<Value>);
    let rows: Vec<Row> = vec![
        (
            "JUnit XML, the default format, with two passing cases and one failure",
            None,
            "<testsuites><testsuite name=\"suite\">\
             <testcase classname=\"pkg.Case\" name=\"a\" time=\"0.5\"/>\
             <testcase classname=\"pkg.Case\" name=\"b\" time=\"0.25\"/>\
             <testcase classname=\"pkg.Case\" name=\"c\">\
             <failure message=\"expected 1 but got 2\"/></testcase>\
             </testsuite></testsuites>",
            "FAILED",
            vec![
                json!({
                    "name": "a",
                    "prefix": "pkg.Case",
                    "testSuiteName": "suite",
                    "status": "SUCCEEDED",
                    "durationInNanoSeconds": 500_000_000i64,
                }),
                json!({
                    "name": "b",
                    "prefix": "pkg.Case",
                    "testSuiteName": "suite",
                    "status": "SUCCEEDED",
                    "durationInNanoSeconds": 250_000_000i64,
                }),
                json!({
                    "name": "c",
                    "prefix": "pkg.Case",
                    "testSuiteName": "suite",
                    "status": "FAILED",
                    "durationInNanoSeconds": 0,
                    "message": "expected 1 but got 2",
                }),
            ],
        ),
        (
            "JUnit XML stating an error, a skip and a failure whose message is its text",
            Some("JUNITXML"),
            "<testsuite name=\"s\">\
             <testcase name=\"e\"><error message=\"boom\"/></testcase>\
             <testcase name=\"k\"><skipped/></testcase>\
             <testcase name=\"t\"><failure>the stack trace</failure></testcase>\
             </testsuite>",
            "FAILED",
            vec![
                json!({
                    "name": "e",
                    "testSuiteName": "s",
                    "status": "ERROR",
                    "durationInNanoSeconds": 0,
                    "message": "boom",
                }),
                json!({
                    "name": "k",
                    "testSuiteName": "s",
                    "status": "SKIPPED",
                    "durationInNanoSeconds": 0,
                }),
                json!({
                    "name": "t",
                    "testSuiteName": "s",
                    "status": "FAILED",
                    "durationInNanoSeconds": 0,
                    "message": "the stack trace",
                }),
            ],
        ),
        (
            "NUnit 3 XML, whose message is a child of the failure element",
            Some("NUNIT3XML"),
            "<test-run><test-suite name=\"asm\">\
             <test-case name=\"n1\" classname=\"N.C\" result=\"Passed\" duration=\"0.25\"/>\
             <test-case name=\"n2\" classname=\"N.C\" result=\"Failed\">\
             <failure><message>no match</message></failure></test-case>\
             </test-suite></test-run>",
            "FAILED",
            vec![
                json!({
                    "name": "n1",
                    "prefix": "N.C",
                    "testSuiteName": "asm",
                    "status": "SUCCEEDED",
                    "durationInNanoSeconds": 250_000_000i64,
                }),
                json!({
                    "name": "n2",
                    "prefix": "N.C",
                    "testSuiteName": "asm",
                    "status": "FAILED",
                    "durationInNanoSeconds": 0,
                    "message": "no match",
                }),
            ],
        ),
        (
            "TestNG XML, which states the series as the element it nests the method in",
            Some("TESTNGXML"),
            "<testng-results><suite name=\"S\"><test><class name=\"C\">\
             <test-method status=\"PASS\" name=\"m1\" duration-ms=\"12\"/>\
             <test-method status=\"FAIL\" name=\"m2\">\
             <exception><message>assertion failed</message></exception></test-method>\
             </class></test></suite></testng-results>",
            "FAILED",
            vec![
                json!({
                    "name": "m1",
                    "prefix": "C",
                    "testSuiteName": "S",
                    "status": "SUCCEEDED",
                    "durationInNanoSeconds": 12_000_000i64,
                }),
                json!({
                    "name": "m2",
                    "prefix": "C",
                    "testSuiteName": "S",
                    "status": "FAILED",
                    "durationInNanoSeconds": 0,
                    "message": "assertion failed",
                }),
            ],
        ),
        (
            "a Visual Studio document, which names a case in its own attribute",
            Some("VISUALSTUDIOTRX"),
            "<TestRun><Results>\
             <UnitTestResult testName=\"v1\" outcome=\"Passed\" duration=\"00:00:01.5000000\"/>\
             <UnitTestResult testName=\"v2\" outcome=\"Failed\"><Output><ErrorInfo>\
             <Message>v2 blew up</Message></ErrorInfo></Output></UnitTestResult>\
             </Results></TestRun>",
            "FAILED",
            vec![
                json!({
                    "name": "v1",
                    "status": "SUCCEEDED",
                    "durationInNanoSeconds": 1_500_000_000i64,
                }),
                json!({
                    "name": "v2",
                    "status": "FAILED",
                    "durationInNanoSeconds": 0,
                    "message": "v2 blew up",
                }),
            ],
        ),
        (
            "Cucumber JSON, one case per scenario of a feature",
            Some("CUCUMBERJSON"),
            "[{\"name\":\"feature one\",\"elements\":[\
             {\"name\":\"scenario a\",\"steps\":[{\"result\":{\"status\":\"passed\",\
             \"duration\":300}}]},\
             {\"name\":\"scenario b\",\"steps\":[{\"result\":{\"status\":\"failed\",\
             \"duration\":200,\"error_message\":\"step blew up\"}}]}]}]",
            "FAILED",
            vec![
                json!({
                    "name": "scenario a",
                    "prefix": "feature one",
                    "status": "SUCCEEDED",
                    "durationInNanoSeconds": 300,
                }),
                json!({
                    "name": "scenario b",
                    "prefix": "feature one",
                    "status": "FAILED",
                    "durationInNanoSeconds": 200,
                    "message": "step blew up",
                }),
            ],
        ),
        (
            "declared files the build never wrote, so the run holds no case",
            Some("JUNITXML"),
            "<testsuite><testcase name=\"never\"/></testsuite>",
            "INCOMPLETE",
            vec![],
        ),
    ];

    for (index, (case, file_format, document, status, expected)) in rows.iter().enumerate() {
        let group = format!("report-cases-{index}");
        let group_arn = create_group(&router, &group, TYPE_TEST).await;
        let project = format!("report-cases-proj-{index}");
        // The row whose declared files were never written declares a file the build does
        // not produce; every other row declares the file it writes.
        let declared = if expected.is_empty() {
            "never-written.xml"
        } else {
            RESULTS_FILE
        };
        let spec = buildspec(&group_arn, document, declared, *file_format);
        create_project(&router, &project, source_with(&spec)).await;
        let build = settled_build(&router, &project, json!({})).await;

        // An ingested test failure is the REPORT's outcome, never the build's: every
        // command of these builds exited zero.
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: an ingested failure must not fail the build, got {build}"
        );
        // The build states the ARN of the report it minted — and it is a report this
        // account really holds.
        let reported = build["reportArns"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|arn| arn.as_str().unwrap_or_default().to_string())
            .collect::<Vec<String>>();
        let (listed_arns, response) = listed(&router, &group_arn, json!({})).await;
        assert_eq!(
            reported, listed_arns,
            "case {case:?}: the build must report the run its group holds, got {build} and \
             {response}"
        );
        let report = read_reports(&router, &reported)
            .await
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            report["status"],
            json!(status),
            "case {case:?}: expected the run's status to be {status}, got {report}"
        );

        let report_arn = reported.first().cloned().unwrap_or_default();
        let (cases, response) = described(&router, &report_arn, json!({})).await;
        let want: Vec<Value> = expected
            .iter()
            .map(|expected| with_report_members(expected, &report_arn, &report))
            .collect();
        assert_eq!(
            cases, want,
            "case {case:?}: expected the cases {want:#?}, got {response}"
        );
        assert_eq!(
            response["nextToken"],
            Value::Null,
            "case {case:?}: a page that leaves no case unlisted states no token, got {response}"
        );
    }

    // The control: a build whose buildspec declares no `reports` section mints nothing, so
    // it is associated with no report and states none.
    let plain = "version: 0.2\nphases:\n  build:\n    commands:\n      - echo building\n";
    create_project(&router, "report-cases-none", source_with(plain)).await;
    let build = settled_build(&router, "report-cases-none", json!({})).await;
    assert_eq!(
        build["reportArns"],
        Value::Null,
        "a build that declared no report is associated with none, got {build}"
    );
}

/// RULE: `DescribeTestCases` reports the cases of the report its ARN names — every one of
/// them, or those a `TestCaseFilter` selects (one `status`, and a `keyword` that is a
/// substring of a case's `name` or `prefix`), one `maxResults` page at a time; a value that
/// is not a report ARN and a token that opens no page are refused as input, and an ARN this
/// account holds no report under is refused as not found.
#[tokio::test]
async fn the_cases_of_a_report_are_filtered_and_paged() {
    let router = emulator();
    let group_arn = create_group(&router, "described-cases", TYPE_TEST).await;
    // Four cases: two that passed, one that failed and one that was skipped, with two
    // different series so a keyword can select on either member.
    let document = "<testsuite name=\"suite\">\
                    <testcase classname=\"alpha.Suite\" name=\"first\" time=\"1\"/>\
                    <testcase classname=\"alpha.Suite\" name=\"second\" time=\"1\"/>\
                    <testcase classname=\"beta.Suite\" name=\"third\">\
                    <failure message=\"nope\"/></testcase>\
                    <testcase classname=\"beta.Suite\" name=\"fourth\"><skipped/></testcase>\
                    </testsuite>";
    let spec = buildspec(&group_arn, document, RESULTS_FILE, Some("JUNITXML"));
    create_project(&router, "described-cases-proj", source_with(&spec)).await;
    let build = settled_build(&router, "described-cases-proj", json!({})).await;
    let report_arn = build["reportArns"][0]
        .as_str()
        .unwrap_or_else(|| panic!("the build must report the run it minted, got {build}"))
        .to_string();

    let (every, response) = described(&router, &report_arn, json!({})).await;
    let names: Vec<Value> = every.iter().map(|case| case["name"].clone()).collect();
    assert_eq!(
        names,
        json!(["first", "second", "third", "fourth"])
            .as_array()
            .cloned()
            .unwrap_or_default(),
        "every case the file stated must be reported, in the order it stated them, got \
         {response}"
    );

    // (case, the filter, the names it selects)
    let selections: Vec<(&str, Value, Vec<&str>)> = vec![
        ("no filter at all", json!({}), vec!["first", "second", "third", "fourth"]),
        (
            "one status",
            json!({"filter": {"status": "SUCCEEDED"}}),
            vec!["first", "second"],
        ),
        (
            "the status one case failed in",
            json!({"filter": {"status": "FAILED"}}),
            vec!["third"],
        ),
        (
            "the status one case was skipped in",
            json!({"filter": {"status": "SKIPPED"}}),
            vec!["fourth"],
        ),
        (
            "a status no case ended in",
            json!({"filter": {"status": "UNKNOWN"}}),
            vec![],
        ),
        (
            "a keyword inside a case's own name",
            json!({"filter": {"keyword": "ir"}}),
            vec!["first", "third"],
        ),
        (
            "a keyword inside the series a case belongs to",
            json!({"filter": {"keyword": "beta"}}),
            vec!["third", "fourth"],
        ),
        (
            "a keyword no case carries",
            json!({"filter": {"keyword": "gamma"}}),
            vec![],
        ),
        (
            "a status and a keyword together",
            json!({"filter": {"status": "SUCCEEDED", "keyword": "beta"}}),
            vec![],
        ),
    ];
    for (case, filter, expected) in selections {
        let (selected, response) = described(&router, &report_arn, filter.clone()).await;
        let names: Vec<String> = selected
            .iter()
            .map(|held| held["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected the cases {expected:?}, got {response}"
        );
    }

    // One page at a time, resuming exactly where the previous page ended.
    let (first_page, response) = described(&router, &report_arn, json!({"maxResults": 3})).await;
    assert_eq!(
        first_page,
        every[..3].to_vec(),
        "a page of three must be the first three cases, got {response}"
    );
    let token = response["nextToken"]
        .as_str()
        .expect("a page that leaves a case unlisted states a token")
        .to_string();
    let (second_page, response) = described(
        &router,
        &report_arn,
        json!({"maxResults": 3, "nextToken": token}),
    )
    .await;
    assert_eq!(
        second_page,
        every[3..].to_vec(),
        "the next page must resume where the first ended, got {response}"
    );
    assert_eq!(
        response["nextToken"],
        Value::Null,
        "the last page must state no token, got {response}"
    );
    // A page of a FILTERED listing resumes inside that same selection.
    let (page, response) = described(
        &router,
        &report_arn,
        json!({"maxResults": 1, "filter": {"keyword": "Suite"}}),
    )
    .await;
    assert_eq!(
        page,
        every[..1].to_vec(),
        "a filtered page must be the first case the filter selected, got {response}"
    );

    // A report of CODE COVERAGE holds no test case at all, which is an empty listing rather
    // than a refusal.
    let coverage_group = create_group(&router, "described-coverage", TYPE_CODE_COVERAGE).await;
    let coverage_spec = buildspec(
        &coverage_group,
        "<report name=\"r\"><counter type=\"LINE\" missed=\"1\" covered=\"3\"/></report>",
        RESULTS_FILE,
        Some("JACOCOXML"),
    );
    create_project(&router, "described-coverage-proj", source_with(&coverage_spec)).await;
    let coverage_build = settled_build(&router, "described-coverage-proj", json!({})).await;
    let coverage_arn = coverage_build["reportArns"][0]
        .as_str()
        .unwrap_or_else(|| {
            panic!("the coverage build must report its run, got {coverage_build}")
        })
        .to_string();
    let (none, response) = described(&router, &coverage_arn, json!({})).await;
    assert!(
        none.is_empty(),
        "a report of code coverage holds no test case, got {response}"
    );

    // (case, the request, the error it is refused with, the sentence it states)
    let refusals: Vec<(&str, Value, &str, String)> = vec![
        (
            "a value that is no ARN at all",
            json!({"reportArn": "described-cases"}),
            "InvalidInputException",
            state::INVALID_REPORT_ARN_MESSAGE.to_string(),
        ),
        (
            "an ARN of another resource of this service",
            json!({"reportArn": group_arn.clone()}),
            "InvalidInputException",
            state::INVALID_REPORT_ARN_MESSAGE.to_string(),
        ),
        (
            "an empty value",
            json!({"reportArn": ""}),
            "InvalidInputException",
            state::INVALID_REPORT_ARN_MESSAGE.to_string(),
        ),
        (
            "a report ARN this account holds no report under",
            json!({"reportArn": state::report_arn(REGION, "described-cases", "no-such-run")}),
            "ResourceNotFoundException",
            state::REPORT_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "a token opening no page of this report's cases",
            json!({"reportArn": report_arn.clone(), "nextToken": "not-a-token"}),
            "InvalidInputException",
            state::INVALID_CONTINUATION_TOKEN_MESSAGE.to_string(),
        ),
        (
            "a token past the cases the report holds",
            json!({"reportArn": report_arn.clone(), "nextToken": "9999"}),
            "InvalidInputException",
            state::INVALID_CONTINUATION_TOKEN_MESSAGE.to_string(),
        ),
    ];
    for (case, body, expected_error, expected_message) in refusals {
        let (status, response) = call(&router, "DescribeTestCases", body.clone()).await;
        assert_eq!(
            (status, error_type(&response)),
            (400, expected_error.to_string()),
            "case {case:?}: expected 400 {expected_error}, got {status} with {response}"
        );
        assert_eq!(
            response["message"],
            json!(expected_message),
            "case {case:?}: expected {expected_message:?}, got {response}"
        );
    }
}

/// RULE: a report's cases are READ: describing them twice answers the same list, and nothing
/// a later caller sees changes.
#[tokio::test]
async fn describing_a_reports_cases_changes_nothing() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let group_arn = create_group(&router, "reread-cases", TYPE_TEST).await;
    let spec = buildspec(
        &group_arn,
        "<testsuite name=\"s\"><testcase name=\"a\" time=\"1\"/>\
         <testcase name=\"b\"><failure message=\"no\"/></testcase></testsuite>",
        RESULTS_FILE,
        None,
    );
    create_project(&router, "reread-cases-proj", source_with(&spec)).await;
    let build = settled_build(&router, "reread-cases-proj", json!({})).await;
    let report_arn = build["reportArns"][0]
        .as_str()
        .unwrap_or_else(|| panic!("the build must report the run it minted, got {build}"))
        .to_string();

    let (first, _) = described(&router, &report_arn, json!({})).await;
    let (second, response) = described(&router, &report_arn, json!({})).await;
    assert_eq!(
        first, second,
        "a repeated read must answer the same cases, got {response}"
    );
    let held = engine.state::<CodeBuildState>().lock().reports.len();
    assert_eq!(
        held, 1,
        "the store must still hold exactly the one run the build minted, got {held}"
    );
}

/// RULE: a report group's trend is derived from the data the runs its builds produced carry,
/// so the points `GetReportGroupTrend` reports are the summary values those very runs hold.
#[tokio::test]
async fn a_trend_is_derived_from_the_data_the_builds_produced() {
    let router = emulator();
    let group_arn = create_group(&router, "trended-runs", TYPE_TEST).await;

    // Two builds, whose reports hold a different number of test cases: the trend over
    // `TOTAL` is those two counts.
    let documents = [
        "<testsuite><testcase name=\"a\" time=\"1\"/><testcase name=\"b\" time=\"1\"/>\
         </testsuite>",
        "<testsuite><testcase name=\"c\" time=\"1\"/><testcase name=\"d\" time=\"1\"/>\
         <testcase name=\"e\" time=\"1\"/></testsuite>",
    ];
    for (index, document) in documents.iter().enumerate() {
        let project = format!("trended-runs-proj-{index}");
        let spec = buildspec(&group_arn, document, RESULTS_FILE, None);
        create_project(&router, &project, source_with(&spec)).await;
        let build = settled_build(&router, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "build {index}: expected the build to succeed, got {build}"
        );
    }
    let (arns, response) = listed(&router, &group_arn, json!({})).await;
    assert_eq!(
        arns.len(),
        2,
        "each build must have produced one run, got {response}"
    );

    let (status, trend) = call(
        &router,
        "GetReportGroupTrend",
        json!({"reportGroupArn": group_arn, "trendField": "TOTAL"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetReportGroupTrend: expected 200, got {status} with {trend}"
    );
    assert_eq!(
        trend["rawData"],
        json!([
            {"reportArn": arns[0], "data": "3"},
            {"reportArn": arns[1], "data": "2"},
        ]),
        "the trend must report one point per run, most recent first, got {trend}"
    );
    assert_eq!(
        trend["stats"],
        json!({"average": "2.5", "max": "3", "min": "2"}),
        "the accumulation must be of the runs' own counts, got {trend}"
    );

    // The same runs, trended on the span their cases took: five cases of a second each.
    let (status, trend) = call(
        &router,
        "GetReportGroupTrend",
        json!({"reportGroupArn": group_arn, "trendField": "DURATION"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetReportGroupTrend: expected 200, got {status} with {trend}"
    );
    assert_eq!(
        trend["stats"],
        json!({
            "average": "2500000000",
            "max": "3000000000",
            "min": "2000000000",
        }),
        "a trend over DURATION must accumulate the spans the cases took, got {trend}"
    );
}
