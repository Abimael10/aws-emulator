// SPDX-License-Identifier: CC0-1.0

//! `ListRestoreTestingSelections` driven as real HTTP through the router.
//!
//! The rules under test:
//!
//! * a listing REPORTS the restore testing selections the plan its path names
//!   holds: every selection `CreateRestoreTestingSelection` stored under that
//!   plan appears exactly once, in `RestoreTestingSelectionName` order, carrying
//!   the list projection's members — the plan it belongs to, its own name, the
//!   role a restore test runs as, the protected resource type, the validation
//!   window in force and the instant the create stored it — and a selection of
//!   ANOTHER plan appears not at all;
//! * a listed selection and the same selection read back through
//!   `GetRestoreTestingSelection` agree on every member they share, because both
//!   project ONE stored record;
//! * the listing is cut into pages of at most `MaxResults` entries plus an
//!   opaque cursor that resumes exactly where the page stopped, and the pages
//!   concatenate to the whole result set in that one order;
//! * a `MaxResults` outside the model's `@range(1..1000)` is
//!   `InvalidParameterValueException`, and so is a `NextToken` that names no
//!   page of this result set;
//! * a `RestoreTestingPlanName` the account holds no plan under addresses no
//!   result set at all, which is the plan's own not-found refusal rather than an
//!   empty page;
//! * a listing is a REPORT: repeating it answers the same thing, and what it
//!   answers follows the store — a selection deleted is listed no more.
//!
//! The request harness (`router`, `call`, `error_code`, `string_at`) and the
//! selection helpers (`create_selection`, `create_body`, `get_selection`,
//! `read_selection`, `minimal_selection`, `name_of_length`,
//! `protected_resource_arn`, `role_arn`, `stored_plan`) are the ones the sibling
//! suites expose to this test binary, so there is ONE wire path for this
//! service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::restore_testing_selection::{
    create_body, create_selection, get_selection, minimal_selection, name_of_length,
    protected_resource_arn, read_selection, role_arn, stored_plan,
};
use crate::vault_and_backup_job::{call, error_code, router, string_at};

/// The ends of the modelled page-size range (`smithy_query` operation
/// `ListRestoreTestingSelections`: `MaxResults` targets
/// `ListRestoreTestingSelectionsInputMaxResultsInteger`, `@range` 1..1000).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 1000;

/// The modelled default of `ValidationWindowHours` (`smithy_query` shape
/// `RestoreTestingSelectionForList`: the member carries `default: 0`), so a
/// response the protocol lets omit it still states that window.
const DEFAULT_VALIDATION_WINDOW_HOURS: i64 = 0;

/// The members `RestoreTestingSelectionForList` declares (`smithy_query` shape
/// `RestoreTestingSelectionForList`) — the WHOLE list projection, so an entry
/// carrying anything else would be reporting a member this shape does not have.
const LIST_PROJECTION_MEMBERS: [&str; 6] = [
    "CreationTime",
    "IamRoleArn",
    "ProtectedResourceType",
    "RestoreTestingPlanName",
    "RestoreTestingSelectionName",
    "ValidationWindowHours",
];

/// `GET /restore-testing/plans/{RestoreTestingPlanName}/selections` —
/// `ListRestoreTestingSelections`'s HTTP binding, with whatever query string the
/// case under test asks for.
async fn list_selections(
    router: &MultiServiceRouter,
    plan: &str,
    query: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/restore-testing/plans/{plan}/selections{query}"),
        None,
    )
    .await
}

/// `DELETE /restore-testing/plans/{RestoreTestingPlanName}/selections/{RestoreTestingSelectionName}`.
async fn delete_selection(router: &MultiServiceRouter, plan: &str, name: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/restore-testing/plans/{plan}/selections/{name}"),
        None,
    )
    .await
}

/// The `RestoreTestingSelections` member of a successful listing.
fn entries(body: &Value) -> Vec<Value> {
    body.get("RestoreTestingSelections")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `RestoreTestingSelectionName` of every entry of a page, in the order the
/// page lists them.
fn names(body: &Value) -> Vec<String> {
    entries(body)
        .iter()
        .map(|entry| string_at(entry, "RestoreTestingSelectionName"))
        .collect()
}

/// An integer member of a body, or the modelled default of a member the
/// response may legitimately omit.
fn int_or(body: &Value, member: &str, default: i64) -> i64 {
    body.get(member).and_then(Value::as_i64).unwrap_or(default)
}

/// The refusal sentence of an error body, whichever casing the envelope spells
/// the member with: a refusal the boundary validator words carries `message`,
/// one the handler returns carries the modelled `Message`.
fn refusal_message(body: &Value) -> String {
    for member in ["Message", "message"] {
        let text = string_at(body, member);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

/// A selection body: a name, a role, the protected resource type this row asks
/// for, the resources named either by wildcard or by `specific_arns` of them,
/// and, where the row states one, a validation window.
fn selection_request(
    name: &str,
    resource_type: &str,
    specific_arns: Option<usize>,
    validation_window_hours: Option<i64>,
) -> Value {
    let mut selection = minimal_selection(name);
    selection["ProtectedResourceType"] = json!(resource_type);
    selection["IamRoleArn"] = json!(role_arn(&format!("RestoreTesting_{resource_type}")));
    if let Some(count) = specific_arns {
        selection["ProtectedResourceArns"] =
            json!((0..count).map(protected_resource_arn).collect::<Vec<String>>());
    }
    if let Some(hours) = validation_window_hours {
        selection["ValidationWindowHours"] = json!(hours);
    }
    selection
}

/// Store the plan, then every selection the row asks for under it, returning the
/// create answers keyed by selection name.
async fn seed(
    router: &MultiServiceRouter,
    plan: &str,
    requests: &[Value],
) -> Vec<(String, Value)> {
    stored_plan(router, plan).await;
    let mut created = Vec::new();
    for request in requests {
        let (status, answer) = create_selection(router, plan, &create_body(request.clone())).await;
        assert_eq!(
            status, 201,
            "setup: expected CreateRestoreTestingSelection 201 for {request:?}, got {status} \
             {answer:?}"
        );
        created.push((
            string_at(request, "RestoreTestingSelectionName"),
            answer,
        ));
    }
    created
}

/// The rows the "report what is stored" table walks: they differ in HOW MANY
/// selections the plan holds, in the protected resource type, in whether the
/// resources are named by wildcard or by a list of specific ARNs, in whether a
/// validation window was stated, and in the order the creates ran relative to
/// the name order a listing must report.
fn stored_set_cases() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        ("a plan holding no selection at all", vec![]),
        (
            "one wildcard selection, no stated window",
            vec![selection_request("only_selection", "EBS", None, None)],
        ),
        (
            "one selection naming specific ARNs, the window at its ceiling",
            vec![selection_request(
                &name_of_length(50),
                "S3",
                Some(3),
                Some(168),
            )],
        ),
        (
            "three selections created in reverse name order",
            vec![
                selection_request("zulu_selection", "EC2", None, Some(12)),
                selection_request("mike_selection", "DynamoDB", Some(1), None),
                selection_request("alfa_selection", "EBS", None, Some(0)),
            ],
        ),
        (
            "names that differ only in case and in digits",
            vec![
                selection_request("Sel_2", "EBS", None, None),
                selection_request("Sel_10", "RDS", None, Some(1)),
                selection_request("sel_2", "EC2", Some(2), None),
            ],
        ),
        (
            "the shortest admitted name beside five walked ones",
            (0..6)
                .map(|index| {
                    selection_request(
                        &if index == 0 {
                            name_of_length(1)
                        } else {
                            format!("walked_{index}")
                        },
                        ["EBS", "EC2", "S3", "RDS"][index % 4],
                        (index % 2 == 0).then_some(index + 1),
                        (index % 3 == 0).then_some(i64::try_from(index).unwrap_or_default()),
                    )
                })
                .collect(),
        ),
    ]
}

/// Every selection the named plan holds is listed exactly once, in name order,
/// reporting the list projection of the stored record; nothing else is listed,
/// and the listing agrees with the read.
#[tokio::test]
async fn a_listing_reports_every_selection_of_the_named_plan_once_in_name_order() {
    for (case, requests) in stored_set_cases() {
        // One router per row: the row's selections are the only ones the
        // account holds, so a row cannot see what another row stored.
        let router = router();
        let plan = "plan_listed";
        let created = seed(&router, plan, &requests).await;

        let (status, listed) = list_selections(&router, plan, "").await;
        assert_eq!(
            (status, error_code(&listed).as_str()),
            (200, "<none>"),
            "case {case:?}: expected ListRestoreTestingSelections 200, got {status} {listed:?}"
        );

        let mut expected: Vec<String> = created.iter().map(|(name, _)| name.clone()).collect();
        expected.sort();
        assert_eq!(
            names(&listed),
            expected,
            "case {case:?}: expected every stored selection once in name order, got {listed:?}"
        );
        assert!(
            listed.get("NextToken").is_none(),
            "case {case:?}: one page holds the whole set, so there is no cursor, got {listed:?}"
        );

        for entry in entries(&listed) {
            let name = string_at(&entry, "RestoreTestingSelectionName");
            let request = requests
                .iter()
                .find(|request| string_at(request, "RestoreTestingSelectionName") == name)
                .unwrap_or_else(|| panic!("case {case:?}: {name:?} was never created"));
            let (_, create_answer) = created
                .iter()
                .find(|(created_name, _)| *created_name == name)
                .unwrap_or_else(|| panic!("case {case:?}: {name:?} was never created"));

            assert_eq!(
                (
                    string_at(&entry, "RestoreTestingPlanName"),
                    string_at(&entry, "IamRoleArn"),
                    string_at(&entry, "ProtectedResourceType"),
                    int_or(&entry, "ValidationWindowHours", -1),
                ),
                (
                    plan.to_string(),
                    string_at(request, "IamRoleArn"),
                    string_at(request, "ProtectedResourceType"),
                    int_or(request, "ValidationWindowHours", DEFAULT_VALIDATION_WINDOW_HOURS),
                ),
                "case {case:?}: {name:?} must report the plan that holds it and the body its \
                 create carried, got {entry:?}"
            );
            assert_eq!(
                entry.get("CreationTime"),
                create_answer.get("CreationTime"),
                "case {case:?}: {name:?} must be listed with the instant its create reported, got \
                 {entry:?}"
            );

            // The list projection is the shape's members and no others: the
            // resources a selection selects are reported by the read alone.
            let mut reported: Vec<String> = entry
                .as_object()
                .map(|members| members.keys().cloned().collect())
                .unwrap_or_default();
            reported.sort();
            assert_eq!(
                reported,
                LIST_PROJECTION_MEMBERS
                    .iter()
                    .map(|member| (*member).to_string())
                    .collect::<Vec<String>>(),
                "case {case:?}: {name:?} must report exactly the list projection, got {entry:?}"
            );

            // The listing and the read project ONE stored record, so they
            // cannot disagree about the selection.
            let (status, read) = get_selection(&router, plan, &name).await;
            let stored = read_selection(&read);
            assert_eq!(
                (
                    status,
                    stored.get("RestoreTestingPlanName").cloned(),
                    stored.get("IamRoleArn").cloned(),
                    stored.get("ProtectedResourceType").cloned(),
                    int_or(&stored, "ValidationWindowHours", DEFAULT_VALIDATION_WINDOW_HOURS),
                    stored.get("CreationTime").cloned(),
                ),
                (
                    200,
                    entry.get("RestoreTestingPlanName").cloned(),
                    entry.get("IamRoleArn").cloned(),
                    entry.get("ProtectedResourceType").cloned(),
                    int_or(&entry, "ValidationWindowHours", DEFAULT_VALIDATION_WINDOW_HOURS),
                    entry.get("CreationTime").cloned(),
                ),
                "case {case:?}: {name:?} listed and read must agree, got {status} {read:?}"
            );
        }
    }
}

/// A listing is scoped to the plan its path names: a selection of another plan
/// is listed nowhere under this one, even when the two plans hold the same
/// selection name and even when one plan's name is a prefix of the other's.
#[tokio::test]
async fn a_listing_reports_only_the_selections_of_the_plan_it_names() {
    let router = router();
    // `plan_scope` and `plan_scope_wider` — the first name is a PREFIX of the
    // second, so a listing that walked the store by prefix rather than by the
    // plan it names would mix the two.
    let cases: [(&str, &[&str]); 3] = [
        ("plan_scope", &["shared_name", "only_here"]),
        ("plan_scope_wider", &["shared_name"]),
        ("plan_scope_empty", &[]),
    ];
    for (plan, selections) in cases {
        let requests: Vec<Value> = selections
            .iter()
            .map(|name| selection_request(name, "EBS", None, None))
            .collect();
        seed(&router, plan, &requests).await;
    }

    for (plan, selections) in cases {
        let (status, listed) = list_selections(&router, plan, "").await;
        let mut expected: Vec<String> = selections.iter().map(|n| (*n).to_string()).collect();
        expected.sort();
        assert_eq!(
            (status, names(&listed)),
            (200, expected),
            "case {plan:?}: a listing reports this plan's selections only, got {status} {listed:?}"
        );
        for entry in entries(&listed) {
            assert_eq!(
                string_at(&entry, "RestoreTestingPlanName"),
                plan,
                "case {plan:?}: every entry belongs to the plan the path names, got {entry:?}"
            );
        }
    }
}

/// The listing is paged: a page holds at most `MaxResults` entries plus a
/// cursor, and walking the cursors yields the whole result set exactly once in
/// the order one unpaginated call reports.
#[tokio::test]
async fn a_listing_pages_by_max_results_with_a_resumable_cursor() {
    let router = router();
    let plan = "plan_paged";
    let requests: Vec<Value> = (0..7)
        .map(|index| selection_request(&format!("paged_{index}"), "EBS", None, Some(index)))
        .collect();
    seed(&router, plan, &requests).await;

    let (status, whole) = list_selections(&router, plan, "").await;
    assert_eq!(
        status, 200,
        "expected the unpaginated listing 200, got {status} {whole:?}"
    );
    let expected = names(&whole);
    assert_eq!(
        expected.len(),
        requests.len(),
        "setup: expected every selection in one page, got {whole:?}"
    );

    // Page sizes below, at and above the number of selections held, plus the
    // published minimum and maximum.
    for page_size in [1_usize, 2, 3, 7, 8, 1000] {
        let mut walked: Vec<String> = Vec::new();
        let mut query = format!("?MaxResults={page_size}");
        let mut pages = 0;
        loop {
            let (status, page) = list_selections(&router, plan, &query).await;
            assert_eq!(
                status, 200,
                "case page_size={page_size}: expected 200, got {status} {page:?}"
            );
            let page_names = names(&page);
            assert!(
                page_names.len() <= page_size,
                "case page_size={page_size}: a page must not exceed MaxResults, got {page:?}"
            );
            walked.extend(page_names);
            pages += 1;
            assert!(
                pages <= requests.len() + 1,
                "case page_size={page_size}: the walk must terminate, got {walked:?}"
            );
            match page.get("NextToken").and_then(Value::as_str) {
                Some(token) => query = format!("?MaxResults={page_size}&NextToken={token}"),
                None => break,
            }
        }
        assert_eq!(
            walked, expected,
            "case page_size={page_size}: the pages must concatenate to the whole set in order, \
             got {walked:?}"
        );
    }
}

/// A `MaxResults` the model's `@range` admits is served; one outside it is this
/// service's `InvalidParameterValueException`.
#[tokio::test]
async fn a_max_results_outside_the_published_range_is_refused() {
    let router = router();
    let plan = "plan_bounded";
    seed(
        &router,
        plan,
        &[selection_request("bounded_selection", "EBS", None, None)],
    )
    .await;

    let cases: [(&str, i64, u16); 7] = [
        ("at the published minimum", MAX_RESULTS_MIN, 200),
        ("at the published maximum", MAX_RESULTS_MAX, 200),
        ("inside the range", 42, 200),
        ("one below the published minimum", MAX_RESULTS_MIN - 1, 400),
        ("negative", -1, 400),
        ("one above the published maximum", MAX_RESULTS_MAX + 1, 400),
        ("far above", 100_000, 400),
    ];
    for (case, value, expected) in cases {
        let (status, body) = list_selections(&router, plan, &format!("?MaxResults={value}")).await;
        assert_eq!(
            status, expected,
            "case {case:?} (MaxResults={value}): expected {expected}, got {status} {body:?}"
        );
        if expected == 400 {
            assert_eq!(
                error_code(&body).as_str(),
                "InvalidParameterValueException",
                "case {case:?} (MaxResults={value}): expected the modelled refusal, got {body:?}"
            );
            assert!(
                !refusal_message(&body).is_empty(),
                "case {case:?} (MaxResults={value}): the refusal states the condition, got \
                 {body:?}"
            );
        } else {
            assert_eq!(
                names(&body),
                vec!["bounded_selection".to_string()],
                "case {case:?} (MaxResults={value}): expected the stored selection, got {body:?}"
            );
        }
    }
}

/// A `NextToken` this result set never handed out names no page of it, and is
/// refused rather than silently answering the first page.
#[tokio::test]
async fn a_next_token_outside_this_result_set_is_refused() {
    let router = router();
    let plan = "plan_cursored";
    seed(
        &router,
        plan,
        &[selection_request("cursored_one", "EBS", None, None)],
    )
    .await;

    let cases: [(&str, &str); 3] = [
        ("a bare word", "not-a-token"),
        // The hex encoding of `nevercreated` — a cursor of this service's own
        // form pointing at a selection the plan does not hold.
        (
            "this service's form naming an unheld selection",
            "6e6576657263726561746564",
        ),
        ("odd-length hex", "abc"),
    ];
    for (case, token) in cases {
        let (status, body) = list_selections(&router, plan, &format!("?NextToken={token}")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?} ({token:?}): expected the modelled refusal, got {status} {body:?}"
        );
    }

    // A cursor the listing DID hand out resolves to the next selection.
    let (status, answer) = create_selection(
        &router,
        plan,
        &create_body(selection_request("cursored_two", "EC2", None, None)),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: the second selection should be stored, got {status} {answer:?}"
    );
    let (status, first) = list_selections(&router, plan, "?MaxResults=1").await;
    let token = string_at(&first, "NextToken");
    assert_eq!(
        (status, token.is_empty()),
        (200, false),
        "expected a cursor from the first page, got {status} {first:?}"
    );
    let (status, second) =
        list_selections(&router, plan, &format!("?MaxResults=1&NextToken={token}")).await;
    assert_eq!(
        (status, names(&second)),
        (200, vec!["cursored_two".to_string()]),
        "the cursor must resume at the next selection, got {status} {second:?}"
    );
}

/// A `RestoreTestingPlanName` the account holds no plan under addresses no
/// result set, so it is the plan's not-found refusal rather than an empty page.
#[tokio::test]
async fn a_plan_the_account_does_not_hold_is_not_found() {
    let router = router();
    let plan = "plan_held";
    seed(&router, plan, &[selection_request("held", "EBS", None, None)]).await;

    // Names spanning the forms the documented alphabet admits: both published
    // ends of the length bound, digits, underscored words, and a case variant
    // of a name that IS held.
    let cases: [(&str, String); 5] = [
        ("the shortest admitted name", name_of_length(1)),
        ("the longest admitted name", name_of_length(50)),
        ("digits only", "20261119".to_string()),
        ("underscored words", "plan_never_created".to_string()),
        ("a case variant of a held name", "Plan_Held".to_string()),
    ];
    for (case, absent) in cases {
        let (status, body) = list_selections(&router, &absent, "").await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?} ({absent:?}): expected the plan's not-found refusal, got {status} \
             {body:?}"
        );
        assert!(
            !refusal_message(&body).is_empty(),
            "case {case:?} ({absent:?}): the refusal states the condition, got {body:?}"
        );
    }

    // The plan that IS held answers its selections, so the refusals above are
    // about the name and not about the operation.
    let (status, listed) = list_selections(&router, plan, "").await;
    assert_eq!(
        (status, names(&listed)),
        (200, vec!["held".to_string()]),
        "the held plan must list its selection, got {status} {listed:?}"
    );
}

/// A listing is a REPORT: repeating it answers the same thing and changes
/// nothing a later reader sees, and what it answers follows the store.
#[tokio::test]
async fn a_listing_repeats_its_answer_and_follows_the_store() {
    let router = router();
    let plan = "plan_reported";
    seed(
        &router,
        plan,
        &[
            selection_request("reported_one", "EBS", None, None),
            selection_request("reported_two", "EC2", Some(1), Some(24)),
        ],
    )
    .await;

    let (status, first) = list_selections(&router, plan, "").await;
    assert_eq!(
        status, 200,
        "expected ListRestoreTestingSelections 200, got {status} {first:?}"
    );
    for repeat in 1..=3 {
        let (status, again) = list_selections(&router, plan, "").await;
        assert_eq!(
            (status, &again),
            (200, &first),
            "repeat {repeat}: two listings of an unchanged store must agree, got {status} \
             {again:?}"
        );
    }

    // A read cannot have consumed anything: the selections are still readable
    // one by one after the listings above.
    for name in ["reported_one", "reported_two"] {
        let (status, read) = get_selection(&router, plan, name).await;
        assert_eq!(
            status, 200,
            "case {name:?}: a listing must leave the selection readable, got {status} {read:?}"
        );
    }

    // The listing follows the store: what a delete removed is listed no more.
    let (status, deleted) = delete_selection(&router, plan, "reported_one").await;
    assert_eq!(
        status, 204,
        "setup: DeleteRestoreTestingSelection should succeed, got {status} {deleted:?}"
    );
    let (status, after) = list_selections(&router, plan, "").await;
    assert_eq!(
        (status, names(&after)),
        (200, vec!["reported_two".to_string()]),
        "a deleted selection must be listed no more, got {status} {after:?}"
    );
}
