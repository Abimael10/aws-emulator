// SPDX-License-Identifier: CC0-1.0

//! Listing the versions of ONE State Manager association, end to end:
//! `ListAssociationVersions` reporting the version `CreateAssociation` minted and every
//! version `UpdateAssociation` appended to it.
//!
//! RULE: a version listing reports every version the association its id names holds, oldest
//! first, each carrying the configuration the call that wrote it supplied and the id and
//! document name of the association that holds it; it serves them in pages of at most
//! `MaxResults`, where the `NextToken` is the version number the next page starts at, and an
//! id naming no association of the Region the request was signed for is
//! `AssociationDoesNotExist`.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention and one reading of the store with the create, update and listing suites.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! how many versions the association holds, which members each call supplies, how big a page
//! is, which Region the listing is taken in and which shapes a resume token can take.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, call_in_region, created, error_code, service, AGENT, NODE, OTHER_REGION, REGION, SHELL,
};

/// `ListAssociationVersions` with `body`, signed for `region`.
async fn list_in_region(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "ListAssociationVersions", body).await
}

/// The whole `ListAssociationVersions` response of a listing that must have succeeded.
async fn listed(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = list_in_region(router, REGION, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListAssociationVersions, got {status} with \
         {parsed:?}"
    );
    parsed
}

/// The `AssociationVersions` of a response.
fn reported(response: &Value) -> Vec<Value> {
    response["AssociationVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `AssociationVersion` values a response reports, in the order it reported them.
fn version_numbers(response: &Value) -> Vec<String> {
    reported(response)
        .iter()
        .map(|version| {
            version["AssociationVersion"]
                .as_str()
                .unwrap_or("<unset>")
                .to_string()
        })
        .collect()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// `UpdateAssociation` with `body`, which must have succeeded.
async fn update(router: &MultiServiceRouter, case: &str, body: &Value) {
    let (status, parsed) = call(router, "UpdateAssociation", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateAssociation, got {status} with {parsed:?}"
    );
}

/// What one version of an association must report: the members it carries, and the members it
/// carries nothing for because the call that wrote it supplied none.
struct ExpectedVersion {
    reported: Vec<(&'static str, Value)>,
    unset: Vec<&'static str>,
}

/// One association's whole history: the create that minted its first version, the updates
/// that appended the rest, and what each version must then report.
struct HistoryRow {
    case: &'static str,
    /// The members the create supplies, beside `Name`.
    created_with: Value,
    /// The members each update supplies, beside the `AssociationId`, in order.
    updates: Vec<Value>,
    /// What each version reports, oldest first — one entry per version the history holds.
    expected: Vec<ExpectedVersion>,
}

fn history_rows() -> Vec<HistoryRow> {
    vec![
        HistoryRow {
            case: "a create and nothing else, so the association holds its first version alone",
            created_with: json!({"InstanceId": NODE, "ScheduleExpression": "rate(30 minutes)"}),
            updates: vec![],
            expected: vec![ExpectedVersion {
                reported: vec![
                    ("AssociationVersion", json!("1")),
                    ("Name", json!(AGENT)),
                    ("ScheduleExpression", json!("rate(30 minutes)")),
                ],
                unset: vec!["AssociationName", "ComplianceSeverity", "Targets"],
            }],
        },
        HistoryRow {
            case: "a create and one update, which renames and reschedules the association",
            created_with: json!({
                "InstanceId": NODE,
                "ScheduleExpression": "rate(30 minutes)",
                "AssociationName": "the-first-name"
            }),
            updates: vec![json!({
                "Name": AGENT,
                "ScheduleExpression": "rate(2 hours)",
                "AssociationName": "renamed",
                "ComplianceSeverity": "HIGH"
            })],
            expected: vec![
                ExpectedVersion {
                    reported: vec![
                        ("AssociationVersion", json!("1")),
                        ("ScheduleExpression", json!("rate(30 minutes)")),
                        ("AssociationName", json!("the-first-name")),
                    ],
                    unset: vec!["ComplianceSeverity"],
                },
                ExpectedVersion {
                    reported: vec![
                        ("AssociationVersion", json!("2")),
                        ("ScheduleExpression", json!("rate(2 hours)")),
                        ("AssociationName", json!("renamed")),
                        ("ComplianceSeverity", json!("HIGH")),
                    ],
                    unset: vec!["MaxErrors"],
                },
            ],
        },
        HistoryRow {
            case: "a create and three updates, spanning the document, the targeting, the rate \
                   controls and the schedule form",
            created_with: json!({"Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            updates: vec![
                json!({
                    "Name": SHELL,
                    "Parameters": {"commands": ["echo second"]},
                    "ComplianceSeverity": "LOW",
                    "MaxErrors": "2",
                    "MaxConcurrency": "50%"
                }),
                json!({
                    "Name": AGENT,
                    "ScheduleExpression": "cron(0 2 ? * SUN *)",
                    "ApplyOnlyAtCronInterval": true,
                    "ScheduleOffset": 3
                }),
                json!({"Name": AGENT, "AssociationName": "the-third-name", "SyncCompliance": "MANUAL"}),
            ],
            expected: vec![
                ExpectedVersion {
                    reported: vec![
                        ("AssociationVersion", json!("1")),
                        ("Name", json!(AGENT)),
                        ("Targets", json!([{"Key": "InstanceIds", "Values": [NODE]}])),
                        ("ApplyOnlyAtCronInterval", json!(false)),
                    ],
                    unset: vec!["Parameters", "ScheduleExpression"],
                },
                ExpectedVersion {
                    reported: vec![
                        ("AssociationVersion", json!("2")),
                        ("Name", json!(SHELL)),
                        ("Parameters", json!({"commands": ["echo second"]})),
                        ("ComplianceSeverity", json!("LOW")),
                        ("MaxErrors", json!("2")),
                        ("MaxConcurrency", json!("50%")),
                    ],
                    // An update carries the whole configuration, so the targets the create
                    // named are reported by the version that named them and by no later one.
                    unset: vec!["Targets", "ScheduleExpression"],
                },
                ExpectedVersion {
                    reported: vec![
                        ("AssociationVersion", json!("3")),
                        ("Name", json!(AGENT)),
                        ("ScheduleExpression", json!("cron(0 2 ? * SUN *)")),
                        ("ApplyOnlyAtCronInterval", json!(true)),
                        ("ScheduleOffset", json!(3)),
                    ],
                    unset: vec!["Parameters", "MaxErrors", "ComplianceSeverity"],
                },
                ExpectedVersion {
                    reported: vec![
                        ("AssociationVersion", json!("4")),
                        ("Name", json!(AGENT)),
                        ("AssociationName", json!("the-third-name")),
                        ("SyncCompliance", json!("MANUAL")),
                        ("ApplyOnlyAtCronInterval", json!(false)),
                    ],
                    unset: vec!["ScheduleExpression", "ScheduleOffset"],
                },
            ],
        },
    ]
}

/// Create the association of `row` and apply its updates, answering the id it was minted
/// under.
async fn with_history(router: &MultiServiceRouter, row: &HistoryRow) -> String {
    let mut create_body = json!({"Name": AGENT});
    merge(&mut create_body, &row.created_with);
    let created = created(router, row.case, &create_body).await;
    let association_id = created["AssociationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !association_id.is_empty(),
        "case {:?}: expected the create to mint an id, got {created:?}",
        row.case
    );
    for members in &row.updates {
        let mut body = json!({"AssociationId": association_id});
        merge(&mut body, members);
        update(router, row.case, &body).await;
    }
    association_id
}

/// RULE: a version listing reports every version the association holds, oldest first, each
/// carrying what the call that wrote it supplied beside the association's own id and name.
#[tokio::test]
async fn a_version_listing_reports_every_version_the_association_holds() {
    for row in history_rows() {
        let case = row.case;
        let (router, _engine) = service();
        // A second association, whose own history must appear in no listing of the first:
        // a version listing walks ONE association's versions.
        let other = created(
            &router,
            case,
            &json!({"Name": SHELL, "InstanceId": "i-0abc1234", "Parameters": {"commands": ["echo other"]}}),
        )
        .await;
        let other_id = other["AssociationId"].as_str().unwrap_or_default().to_string();
        let association_id = with_history(&router, &row).await;

        let response = listed(&router, case, &json!({"AssociationId": association_id})).await;
        let versions = reported(&response);
        assert_eq!(
            versions.len(),
            row.expected.len(),
            "case {case:?}: expected {} versions, got {versions:?}",
            row.expected.len()
        );
        assert!(
            response["NextToken"].is_null(),
            "case {case:?}: expected a whole history to be served in one page, got {response:?}"
        );
        // Oldest first, counting up from the version the create minted with no gaps.
        let numbered: Vec<String> = (1..=row.expected.len()).map(|n| n.to_string()).collect();
        assert_eq!(
            version_numbers(&response),
            numbered,
            "case {case:?}: expected the versions oldest first, got {versions:?}"
        );
        for (version, expected) in versions.iter().zip(&row.expected) {
            assert_eq!(
                version["AssociationId"].as_str(),
                Some(association_id.as_str()),
                "case {case:?}: expected every version to name its association, got {version:?}"
            );
            assert!(
                version["CreatedDate"].is_number() || version["CreatedDate"].is_string(),
                "case {case:?}: expected the version to report when it was created, got \
                 {version:?}"
            );
            for (member, value) in &expected.reported {
                assert_eq!(
                    &version[*member], value,
                    "case {case:?}: expected {member} to be {value}, got {version:?}"
                );
            }
            for member in &expected.unset {
                assert!(
                    version[*member].is_null(),
                    "case {case:?}: expected {member} to be reported by nothing, got {version:?}"
                );
            }
        }

        // The other association's history is its own, and reading a history leaves both
        // exactly as they were: a second listing reports what the first did.
        let theirs = listed(&router, case, &json!({"AssociationId": other_id})).await;
        assert_eq!(
            version_numbers(&theirs),
            vec!["1".to_string()],
            "case {case:?}: expected the other association to hold its first version alone, got \
             {theirs:?}"
        );
        let again = listed(&router, case, &json!({"AssociationId": association_id})).await;
        assert_eq!(
            again, response,
            "case {case:?}: expected a second listing to report what the first did"
        );
    }
}

/// RULE: a listing serves at most `MaxResults` versions per page, and the `NextToken` is the
/// version the next page starts at — so walking the pages reports every version exactly once,
/// oldest first.
#[tokio::test]
async fn paging_a_version_listing_reports_every_version_exactly_once() {
    let (router, _engine) = service();
    // The longest history the table above builds, so there is something left over at every
    // page size below it.
    let row = history_rows()
        .into_iter()
        .next_back()
        .expect("the table holds a row");
    let held = row.expected.len();
    let association_id = with_history(&router, &row).await;
    let whole: Vec<String> = (1..=held).map(|n| n.to_string()).collect();

    // The page sizes span the modelled range (`ListAssociationVersionsRequest$MaxResults`,
    // 1..50): the minimum, a size that does not divide the held count, the count itself, the
    // maximum, and the request that names no `MaxResults` at all.
    let sizes: &[Option<usize>] = &[Some(1), Some(3), Some(held), Some(50), None];
    for size in sizes {
        let case = format!("a page size of {size:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = json!({"AssociationId": association_id});
            if let Some(size) = size {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = &token {
                request["NextToken"] = json!(token);
            }
            let response = listed(&router, &case, &request).await;
            let page = version_numbers(&response);
            let bound = size.unwrap_or(held);
            assert!(
                page.len() <= bound,
                "case {case:?}: expected at most {bound} versions in a page, got {page:?}"
            );
            if let Some(resumed) = &token {
                assert_eq!(
                    page.first().map(String::as_str),
                    Some(resumed.as_str()),
                    "case {case:?}: expected the page to start at the token {resumed}, got \
                     {page:?}"
                );
            }
            walked.extend(page.iter().cloned());
            token = response["NextToken"].as_str().map(str::to_string);
            pages += 1;
            assert!(
                pages <= held + 1,
                "case {case:?}: expected the walk to finish within {} pages, got {pages}",
                held + 1
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: expected the walk to report every version once, oldest first, got \
             {walked:?}"
        );
        let expected_pages = held.div_ceil(size.unwrap_or(held));
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages, got {pages}"
        );
    }
}

/// RULE: a listing that names no association of the Region the request was signed for is
/// refused `AssociationDoesNotExist`, naming the association it asked about.
#[tokio::test]
async fn a_listing_of_an_association_the_region_holds_none_of_is_refused() {
    let (router, _engine) = service();
    let created = created(
        &router,
        "the association that is held",
        &json!({"Name": AGENT, "InstanceId": NODE}),
    )
    .await;
    let association_id = created["AssociationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // (case, the region the listing is signed for, the id it carries)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "an id nothing holds",
            REGION,
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "an id of every hexadecimal digit, which nothing holds either",
            REGION,
            "abcdefab-cdef-abcd-efab-cdefabcdefab".to_string(),
        ),
        (
            "the held association, read through another Region",
            OTHER_REGION,
            association_id.clone(),
        ),
    ];
    for (case, region, id) in rows {
        let (status, parsed) = list_in_region(&router, region, &json!({"AssociationId": id})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from ListAssociationVersions, got {status} with \
             {parsed:?}"
        );
        let code = error_code(&parsed);
        assert_eq!(
            code, "AssociationDoesNotExist",
            "case {case:?}: expected AssociationDoesNotExist, got {code} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message,
            format!("Association {id} does not exist."),
            "case {case:?}: expected the refusal to name the association, got {message:?}"
        );
    }
}

/// RULE: a `NextToken` this listing could not have issued is `InvalidNextToken`, and a page
/// size outside the modelled range is refused rather than served.
#[tokio::test]
async fn a_token_this_listing_never_issued_is_refused() {
    let (router, _engine) = service();
    let row = history_rows()
        .into_iter()
        .next_back()
        .expect("the table holds a row");
    let held = row.expected.len();
    let association_id = with_history(&router, &row).await;

    // Each row is a token that names no version this association's history could have been
    // resumed at: a word, a version below the first, a version past the newest, a version
    // far past it, and a number with something else beside it.
    let tokens: Vec<(&str, String)> = vec![
        ("a word", "next-page".to_string()),
        ("a version below the first", "0".to_string()),
        ("the version past the newest", (held + 1).to_string()),
        ("a version far past the newest", "1000".to_string()),
        ("a number with something beside it", "2x".to_string()),
        ("a negative version", "-1".to_string()),
    ];
    for (case, token) in tokens {
        let (status, parsed) = list_in_region(
            &router,
            REGION,
            &json!({"AssociationId": association_id, "NextToken": token}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a token this listing never issued, got {status} \
             with {parsed:?}"
        );
        let code = error_code(&parsed);
        assert_eq!(
            code, "InvalidNextToken",
            "case {case:?}: expected InvalidNextToken, got {code} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message, "The NextToken value is not valid",
            "case {case:?}: expected the service's own sentence, got {message:?}"
        );
    }

    // Every version this association holds is a position its history can be resumed from, so
    // a listing resumed at one reports that version and the ones after it.
    for version in 1..=held {
        let case = format!("resumed at version {version}");
        let response = listed(
            &router,
            &case,
            &json!({"AssociationId": association_id, "NextToken": version.to_string()}),
        )
        .await;
        let expected: Vec<String> = (version..=held).map(|n| n.to_string()).collect();
        assert_eq!(
            version_numbers(&response),
            expected,
            "case {case:?}: expected the versions at or after {version}, got {response:?}"
        );
    }

    // A page size outside the modelled range is refused: the minimum is 1 and the maximum 50
    // (`ListAssociationVersionsRequest$MaxResults`).
    for size in [0, -1, 51, 1_000] {
        let case = format!("a page size of {size}");
        let (status, parsed) = list_in_region(
            &router,
            REGION,
            &json!({"AssociationId": association_id, "MaxResults": size}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a page size outside the modelled range, got \
             {status} with {parsed:?}"
        );
    }
}

/// Merge the members of `extra` onto the object `body`.
fn merge(body: &mut Value, extra: &Value) {
    let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (member, value) in source {
        target.insert(member.clone(), value.clone());
    }
}
