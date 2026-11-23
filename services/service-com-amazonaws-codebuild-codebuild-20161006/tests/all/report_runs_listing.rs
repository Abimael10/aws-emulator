// SPDX-License-Identifier: CC0-1.0

//! How `ListReportsForReportGroup` reports the report runs one report group holds, driven
//! end to end over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! RULE under test: a reporting read of a report group's runs reports the ARNs of exactly
//! the runs that group holds, ordered by the instant each run occurred — newest first
//! unless the request asks for ascending order — narrowed to one status when the request
//! filters on one, and cut into pages of at most `maxResults` entries (at most this
//! service's own ceiling when the request names none) whose token resumes the walk at the
//! entry it names. A page size outside the range the model declares for it, a value that is
//! no report group ARN and a token opening no page are refused as input; a group this
//! account does not hold is refused as not found, with the same sentence the service's other
//! reporting read of a report group states for it. Being a reporting read, it leaves every
//! run exactly as a later caller finds it.
//!
//! `build_report_ingestion.rs` beside this one covers what a BUILD puts into a group, and
//! owns the ingestion rule. The rows here differ along what the listing rule generalises
//! over: the page size (each bound of `PageSize` and one past it, and the member left out),
//! the order asked for, the status filtered on (every word of the `ReportStatusType`
//! catalogue), where a walk resumes, and the shape of a refused identifier. Nothing is
//! seeded into the store: every run listed here was minted by a build whose own commands
//! wrote the file it was read from.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{
    call, create_project, emulator, error_type, settled_build, REGION,
};
use super::report_group_lifecycle::create_report_group_body;

/// The `ReportType` of a group whose runs are test reports — one word of the model's
/// `ReportType` catalogue
/// (`smithy_query(query="shape",shape="ReportType",model_path="services/service-com-amazonaws-codebuild-codebuild-20161006/smithy-model/com-amazonaws-codebuild-codebuild-20161006.json")`).
const TYPE_TEST: &str = "TEST";

/// Every word of the model's `ReportStatusType` catalogue
/// (`smithy_query(query="shape",shape="ReportStatusType",…)`: `GENERATING`, `SUCCEEDED`,
/// `FAILED`, `INCOMPLETE`, `DELETING`), which is what `ReportFilter$status` — "the status
/// used to filter reports" — takes.
const REPORT_STATUSES: [&str; 5] = [
    "GENERATING",
    "SUCCEEDED",
    "FAILED",
    "INCOMPLETE",
    "DELETING",
];

/// The file a case's buildspec writes its results into.
const RESULTS_FILE: &str = "results-file.xml";

/// A document whose every case passed, so the run it is read into succeeded.
const PASSING_DOCUMENT: &str = "<testsuite><testcase name=\"a\"/></testsuite>";

/// A document one of whose cases did not pass, so the run it is read into failed.
const FAILING_DOCUMENT: &str =
    "<testsuite><testcase name=\"b\"><failure message=\"no\"/></testcase></testsuite>";

/// How many pages a walk may take before it is called non-terminating: the biggest listing
/// below holds [`RUNS_IN_THE_PAGED_GROUP`] runs and the smallest page carries one.
const MAX_PAGES_PER_WALK: usize = 50;

/// How many runs the group whose paging is walked holds — more than the smallest page size
/// under test, so at least one walk takes several pages.
const RUNS_IN_THE_PAGED_GROUP: usize = 5;

/// One buildspec command line, as a YAML double-quoted scalar: the documents written below
/// carry the double quotes an XML attribute is written with, so the command is quoted in
/// the one YAML form that escapes them.
fn command_line(command: &str) -> String {
    format!(
        "      - \"{}\"\n",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// A buildspec whose build writes `document` into [`RESULTS_FILE`] and declares the report
/// group `group` to be read from the files `files` names.
fn buildspec_for(group: &str, document: &str, files: &str) -> String {
    format!(
        "version: 0.2\nphases:\n  build:\n    commands:\n{}reports:\n  \"{group}\":\n    \
         files:\n      - \"{files}\"\n    file-format: JUNITXML\n",
        command_line(&format!("echo '{document}' > {RESULTS_FILE}"))
    )
}

/// A `CreateProject` source carrying `buildspec` inline, with no input source code of its
/// own — the form that lets a suite fix exactly what a build runs and declares.
fn source_with(buildspec: &str) -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}})
}

/// Create the report group `name` over the wire, asserting it succeeded, and hand back the
/// ARN the service minted for it.
async fn create_group(router: &MultiServiceRouter, name: &str) -> String {
    let body = create_report_group_body(
        name,
        TYPE_TEST,
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
        .unwrap_or_else(|| panic!("CreateReportGroup {name:?} must mint an ARN, got {response}"))
        .to_string()
}

/// The `reports` member of a listing, as strings.
fn report_arns(case: &str, response: &Value) -> Vec<String> {
    response["reports"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the answer must carry a reports list, got {response}")
        })
        .iter()
        .map(|arn| {
            arn.as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every listed report ARN is a string, got {response}")
                })
                .to_string()
        })
        .collect()
}

/// The report ARNs `ListReportsForReportGroup` lists for `group_arn`, with `extra`'s members
/// merged into the request, asserting the call succeeded.
async fn listed(
    router: &MultiServiceRouter,
    group_arn: &str,
    extra: Value,
) -> (Vec<String>, Value) {
    let mut body = json!({"reportGroupArn": group_arn});
    if let (Value::Object(target), Value::Object(more)) = (&mut body, extra) {
        target.extend(more);
    }
    let (status, response) = call(router, "ListReportsForReportGroup", body.clone()).await;
    assert_eq!(
        status, 200,
        "ListReportsForReportGroup {body}: expected 200, got {status} with {response}"
    );
    let arns = report_arns("a listing", &response);
    (arns, response)
}

/// The report records `BatchGetReports` reports for `arns`, in the order it answers them.
async fn read_reports(router: &MultiServiceRouter, arns: &[String]) -> Vec<Value> {
    let (status, response) = call(router, "BatchGetReports", json!({"reportArns": arns})).await;
    assert_eq!(
        status, 200,
        "BatchGetReports {arns:?}: expected 200, got {status} with {response}"
    );
    let reports = response["reports"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        reports.len(),
        arns.len(),
        "every run a listing reported must resolve through the implemented reader, \
         got {response}"
    );
    reports
}

/// The status every run `arns` names is in, by ARN, read back through `BatchGetReports` —
/// what a status filter is asserted against.
async fn statuses_of(
    router: &MultiServiceRouter,
    arns: &[String],
) -> std::collections::BTreeMap<String, String> {
    read_reports(router, arns)
        .await
        .iter()
        .map(|report| {
            (
                report["arn"].as_str().unwrap_or_default().to_string(),
                report["status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Create `project` so that its buildspec declares `group_arn`, reading it from the files
/// `files` names and writing `document` into [`RESULTS_FILE`], then build it `count` times,
/// asserting each build minted exactly one run; hand back the run ARNs in build order.
async fn built_runs(
    router: &MultiServiceRouter,
    project: &str,
    group_arn: &str,
    document: &str,
    files: &str,
    count: usize,
) -> Vec<String> {
    create_project(
        router,
        project,
        source_with(&buildspec_for(group_arn, document, files)),
    )
    .await;
    let mut minted = Vec::with_capacity(count);
    for index in 0..count {
        let build = settled_build(router, project, json!({})).await;
        let runs = build["reportArns"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            runs.len(),
            1,
            "build {index} of {project:?}: a build mints one run per group its buildspec \
             declares, got {build}"
        );
        minted.push(runs[0].as_str().unwrap_or_default().to_string());
    }
    minted
}

/// Every run of `group_arn`, walked page by page in the order `sort_order` asks for,
/// asserting each page is held to the ceiling in force — `max_results` when the request
/// names one, this service's own page ceiling otherwise — that only the last page of the
/// walk closes it, and that the walk terminates.
async fn walk_pages(
    router: &MultiServiceRouter,
    case: &str,
    group_arn: &str,
    sort_order: Option<&str>,
    max_results: Option<i32>,
) -> Vec<String> {
    let ceiling = max_results.map_or(state::MAX_PAGE_SIZE, |limit| limit as usize);
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for page_number in 0..MAX_PAGES_PER_WALK {
        let mut extra = json!({});
        if let Value::Object(members) = &mut extra {
            if let Some(order) = sort_order {
                members.insert("sortOrder".to_string(), json!(order));
            }
            if let Some(limit) = max_results {
                members.insert("maxResults".to_string(), json!(limit));
            }
            if let Some(resume) = &token {
                members.insert("nextToken".to_string(), json!(resume));
            }
        }
        let (page, response) = listed(router, group_arn, extra).await;
        assert!(
            page.len() <= ceiling,
            "case {case:?}: page {page_number} must hold at most {ceiling} ARNs, got {} \
             in {response}",
            page.len()
        );
        let next = response["nextToken"].as_str().map(str::to_string);
        if next.is_some() {
            assert_eq!(
                page.len(),
                ceiling,
                "case {case:?}: page {page_number} is not the last, so it must be full, \
                 got {response}"
            );
        }
        collected.extend(page);
        match next {
            Some(resume) => token = Some(resume),
            // The walk ended: the last page carried no token.
            None => return collected,
        }
    }
    panic!("case {case:?}: the walk did not terminate within {MAX_PAGES_PER_WALK} pages");
}

/// RULE: a page holds at most `maxResults` run ARNs — at most this service's page ceiling
/// when the request names none — and the `nextToken` it carries resumes the walk at the
/// entry it names, so walking the pages reproduces the group's whole ordering in whichever
/// order was asked for.
///
/// The table walks the page sizes `PageSize` allows
/// (`ListReportsForReportGroupInput$maxResults`, `@range(min: 1, max: 100)`): the minimum,
/// one between, exactly as many as the group holds, the published maximum, and the member
/// left out — each with no `sortOrder`, with each word of `SortOrderType`.
#[tokio::test]
async fn the_runs_of_a_group_are_paged_at_max_results_in_the_order_asked_for() {
    let router = emulator();
    let group_arn = create_group(&router, "paged-runs").await;
    let minted = built_runs(
        &router,
        "paged-runs-proj",
        &group_arn,
        PASSING_DOCUMENT,
        RESULTS_FILE,
        RUNS_IN_THE_PAGED_GROUP,
    )
    .await;

    let (newest_first, response) = listed(&router, &group_arn, json!({})).await;
    let built_latest_first: Vec<String> = minted.iter().rev().cloned().collect();
    assert_eq!(
        newest_first, built_latest_first,
        "every run the group holds must be listed most recent first, got {response}"
    );
    let oldest_first: Vec<String> = newest_first.iter().rev().cloned().collect();

    let sizes: [Option<i32>; 5] = [
        Some(1),
        Some(2),
        Some(RUNS_IN_THE_PAGED_GROUP as i32),
        Some(100),
        None,
    ];
    for size in sizes {
        for (order_case, sort_order, expected) in [
            ("no sortOrder", None, &newest_first),
            ("DESCENDING", Some("DESCENDING"), &newest_first),
            ("ASCENDING", Some("ASCENDING"), &oldest_first),
        ] {
            let case = format!("maxResults {size:?} / {order_case}");
            let walked = walk_pages(&router, &case, &group_arn, sort_order, size).await;
            assert_eq!(
                &walked, expected,
                "case {case:?}: paging must reproduce the whole ordering, got {walked:?}"
            );
        }
    }

    // A token names the first entry of the page it opens, for every entry of the listing.
    for (index, arn) in newest_first.iter().enumerate() {
        let case = format!("resuming at entry {index}");
        let (page, response) = listed(&router, &group_arn, json!({"nextToken": arn})).await;
        assert_eq!(
            page,
            newest_first[index..].to_vec(),
            "case {case:?}: the page must open at {arn:?}, got {response}"
        );
    }
}

/// RULE: a `maxResults` inside the range the model declares for it is honoured as the page's
/// ceiling; one outside that range is refused with HTTP 400 `InvalidInputException` rather
/// than clamped into it.
///
/// The bounds are `PageSize`'s own (`@range(min: 1, max: 100)`), tested at each bound and
/// one past each.
#[tokio::test]
async fn a_max_results_outside_its_declared_range_is_refused() {
    let router = emulator();
    let group_arn = create_group(&router, "bounded-runs").await;
    let minted = built_runs(
        &router,
        "bounded-runs-proj",
        &group_arn,
        PASSING_DOCUMENT,
        RESULTS_FILE,
        2,
    )
    .await;

    for (case, max_results, expected_len) in [
        ("at the minimum", 1, 1),
        ("between the bounds", 2, minted.len()),
        ("at the maximum", 100, minted.len()),
    ] {
        let (page, response) = listed(&router, &group_arn, json!({"maxResults": max_results})).await;
        assert_eq!(
            page.len(),
            expected_len,
            "case {case:?} (maxResults {max_results}): expected {expected_len} ARNs, \
             got {response}"
        );
    }

    for (case, max_results) in [
        ("one below the minimum", 0),
        ("one past the maximum", 101),
        ("negative", -1),
    ] {
        let (status, body) = call(
            &router,
            "ListReportsForReportGroup",
            json!({"reportGroupArn": group_arn, "maxResults": max_results}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?} (maxResults {max_results}): expected 400, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains("InvalidInputException"),
            "case {case:?} (maxResults {max_results}): expected InvalidInputException, got {body}"
        );
    }
}

/// RULE: a request that filters on a status lists exactly the runs of the group that are in
/// that status, in the same order the unfiltered listing puts them in; a status no run of
/// the group is in lists nothing, and the group's own runs are never another group's.
///
/// The table is every word of the `ReportStatusType` catalogue ([`REPORT_STATUSES`]),
/// against a group holding one run per status a build can leave a run in — read from a
/// document whose cases all passed, one whose case did not, and declared files the build
/// never wrote.
#[tokio::test]
async fn a_status_filter_lists_exactly_the_runs_in_that_status() {
    let router = emulator();
    let group_arn = create_group(&router, "filtered-runs").await;
    let other_arn = create_group(&router, "filtered-runs-other").await;

    // (project, the document its build writes, the files its section declares)
    let projects: [(&str, &str, &str); 3] = [
        ("filtered-runs-passing", PASSING_DOCUMENT, RESULTS_FILE),
        ("filtered-runs-failing", FAILING_DOCUMENT, RESULTS_FILE),
        // Declared files the build never wrote: the run holds no result at all.
        ("filtered-runs-empty", PASSING_DOCUMENT, "never-written.xml"),
    ];
    for (project, document, files) in projects {
        built_runs(&router, project, &group_arn, document, files, 1).await;
    }
    built_runs(
        &router,
        "filtered-runs-other-proj",
        &other_arn,
        PASSING_DOCUMENT,
        RESULTS_FILE,
        1,
    )
    .await;

    let (all_runs, response) = listed(&router, &group_arn, json!({})).await;
    assert_eq!(
        all_runs.len(),
        projects.len(),
        "the group must hold one run per build that declared it, got {response}"
    );
    let held = statuses_of(&router, &all_runs).await;

    let mut filtered_total = 0;
    for status in REPORT_STATUSES {
        // The order an unfiltered listing puts the group's runs in, narrowed to the status
        // the request filters on.
        let expected: Vec<String> = all_runs
            .iter()
            .filter(|arn| held.get(*arn).map(String::as_str) == Some(status))
            .cloned()
            .collect();
        let (listed_in_status, response) =
            listed(&router, &group_arn, json!({"filter": {"status": status}})).await;
        assert_eq!(
            listed_in_status, expected,
            "case {status:?}: expected exactly the runs in that status, got {response}"
        );
        filtered_total += listed_in_status.len();
    }
    assert_eq!(
        filtered_total,
        all_runs.len(),
        "every run of the group must be in exactly one status of the catalogue, \
         got {held:?}"
    );

    // Another group's runs are never this group's, filtered or not.
    let (other, response) = listed(&router, &other_arn, json!({})).await;
    assert_eq!(
        other.len(),
        1,
        "the other group must hold only its own run, got {response}"
    );
    assert!(
        !all_runs.contains(&other[0]),
        "a group's listing must not report another group's run, got {all_runs:?}"
    );
}

/// RULE: listing a group's runs is a reporting read — it leaves the group holding the same
/// runs, with the same records, however many times and in whatever form it is called.
#[tokio::test]
async fn listing_a_groups_runs_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let group_arn = create_group(&router, "unchanged-runs").await;
    built_runs(
        &router,
        "unchanged-runs-proj",
        &group_arn,
        PASSING_DOCUMENT,
        RESULTS_FILE,
        2,
    )
    .await;

    let (before, _) = listed(&router, &group_arn, json!({})).await;
    let records_before = read_reports(&router, &before).await;

    // Every form of the call a caller can make, each repeated: a page, a resumed page, each
    // order, and a filter.
    let forms: [Value; 6] = [
        json!({}),
        json!({"maxResults": 1}),
        json!({"maxResults": 1, "nextToken": before[1]}),
        json!({"sortOrder": "ASCENDING"}),
        json!({"sortOrder": "DESCENDING"}),
        json!({"filter": {"status": "SUCCEEDED"}}),
    ];
    for round in 0..2 {
        for form in &forms {
            let case = format!("round {round} of {form}");
            let (page, response) = listed(&router, &group_arn, form.clone()).await;
            assert!(
                page.iter().all(|arn| before.contains(arn)),
                "case {case:?}: a listing must report only runs the group already held, \
                 got {response}"
            );
        }
    }

    let (after, response) = listed(&router, &group_arn, json!({})).await;
    assert_eq!(
        after, before,
        "the group must hold the same runs in the same order after being read, \
         got {response}"
    );
    let records_after = read_reports(&router, &after).await;
    assert_eq!(
        records_after, records_before,
        "every run's record must be exactly what an earlier caller saw, got {records_after:?}"
    );
}

/// RULE: a value that is no report group ARN of this service is refused as input, a group
/// this account does not hold is refused as not found, and a token that opens no page of
/// this listing is refused as input — a value the model lets through to the handler being
/// refused with the very sentence the service's other reporting read of a report group
/// states for it, since both are asked the same question about the same identifier.
#[tokio::test]
async fn a_report_group_a_listing_cannot_be_cut_from_is_refused_as_the_other_read_refuses_it() {
    let router = emulator();
    let group_arn = create_group(&router, "refused-runs").await;

    // (case, the ARN asked about, the error the listing states, whether the other read is
    // asked the same question of the same shape).
    //
    // The empty value is the one row the two reads do NOT share: this operation's
    // `reportGroupArn` targets a plain `String`, so the value reaches the handler, while
    // `GetReportGroupTrendInput$reportGroupArn` targets `NonEmptyString` (`@length(min: 1)`,
    // `smithy_query(query="operation",operation="GetReportGroupTrend")`), so there it is the
    // constraint walk that refuses it and states the constraint's own sentence.
    let rows: Vec<(&str, String, &str, bool)> = vec![
        (
            "a bare report group name",
            "refused-runs".to_string(),
            "InvalidInputException",
            true,
        ),
        (
            "an empty value",
            String::new(),
            "InvalidInputException",
            false,
        ),
        (
            "an ARN of another resource type of this service",
            state::project_arn(REGION, "refused-runs"),
            "InvalidInputException",
            true,
        ),
        (
            "an ARN whose region field is no Region",
            format!(
                "arn:aws:codebuild:region:{}:report-group/refused-runs",
                engine::EMULATOR_ACCOUNT_ID
            ),
            "InvalidInputException",
            true,
        ),
        (
            "an ARN this account holds no group under",
            state::report_group_arn(REGION, "no-such-group"),
            "ResourceNotFoundException",
            true,
        ),
    ];
    for (case, arn, expected_error, same_as_the_other_read) in rows {
        let (status, listing) = call(
            &router,
            "ListReportsForReportGroup",
            json!({"reportGroupArn": arn}),
        )
        .await;
        assert_eq!(
            (status, error_type(&listing)),
            (400, expected_error.to_string()),
            "case {case:?}: expected 400 {expected_error}, got {status} with {listing}"
        );
        if !same_as_the_other_read {
            assert_eq!(
                listing["message"],
                json!(state::INVALID_REPORT_GROUP_ARN_MESSAGE),
                "case {case:?}: a value that addresses no report group at all must be \
                 refused as one, got {listing}"
            );
            continue;
        }
        // `trendField` is @required on the other read and targets
        // `ReportGroupTrendFieldType`.
        let (trend_status, trend) = call(
            &router,
            "GetReportGroupTrend",
            json!({"reportGroupArn": arn, "trendField": "DURATION"}),
        )
        .await;
        assert_eq!(
            (status, error_type(&listing), listing["message"].clone()),
            (trend_status, error_type(&trend), trend["message"].clone()),
            "case {case:?}: both reporting reads of a report group must refuse the same \
             identifier the same way, got {listing} and {trend}"
        );
    }

    // A token is resolved against this listing, so one that opens no page of it is refused
    // rather than answered with a page.
    let minted = built_runs(
        &router,
        "refused-runs-proj",
        &group_arn,
        PASSING_DOCUMENT,
        RESULTS_FILE,
        1,
    )
    .await;
    let held = minted[0].clone();
    for (case, token) in [
        ("a token of no resource at all", "not-a-token".to_string()),
        (
            "a truncated ARN of a run this group holds",
            held[..held.len() - 1].to_string(),
        ),
        ("an ARN of this group's own run with a suffix", format!("{held}x")),
        ("the group's ARN itself", group_arn.clone()),
    ] {
        let (status, body) = call(
            &router,
            "ListReportsForReportGroup",
            json!({"reportGroupArn": group_arn, "nextToken": token}),
        )
        .await;
        assert_eq!(
            (status, error_type(&body)),
            (400, "InvalidInputException".to_string()),
            "case {case:?} ({token:?}): expected 400 InvalidInputException, \
             got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(state::INVALID_CONTINUATION_TOKEN_MESSAGE),
            "case {case:?}: the refusal must state the sentence a token opening no page \
             gets, got {body}"
        );
    }
}
