// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateRunGroup`.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes. The HTTP harness is the one
//! `private_workflow_runs` owns.
//!
//! The rule under test: an update applies each setting its request names to the one stored
//! run group its path id identifies and leaves every member it does not name exactly as it
//! stood, so each later read of that group — `GetRunGroup` and `ListRunGroups` alike —
//! reports the settings last written and nothing else about the record moves.
//!
//! Asserted over the class rather than one request:
//!
//! * the table below varies which members an update names (one cap, every settable member,
//!   the name alone, a cap the create never set, none at all), the values they carry across
//!   the whole published cap range, and which members the create left unset;
//! * a group an update never names is untouched by it, and an update's own group keeps its
//!   id, ARN, creation time and tags;
//! * successive updates accumulate: the value a read reports for a member is the one the
//!   last update naming it wrote;
//! * a value outside a cap's published range, or a name outside the modelled length, is
//!   refused and changes nothing;
//! * an id no group holds is the modelled 404, however the id is shaped, and stores nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::private_workflow_runs::{
    assert_absent, assert_arn, assert_members, assert_timestamp, call, created_id, emulator,
    error_code, items, tag_map,
};
// The run-group constants and the create/read/list calls this suite shares with the
// create-and-list suite are that module's, so the published cap range, the ARN resource and
// each route are stated once for the whole slice.
use crate::run_groups::{
    create_group, get_group, list_groups, member_object, CAP_MEMBERS, CAP_RANGE,
    RUN_GROUP_ARN_RESOURCE,
};

/// The published bounds of the modelled `RunGroupName` length (1..128).
/// Source: the `name` member of `UpdateRunGroupRequest`.
const NAME_LENGTH: (usize, usize) = (1, 128);

/// The members of a stored run group no update can move: they are minted or recorded by the
/// create and `UpdateRunGroupRequest` models none of them.
/// Source: `UpdateRunGroupRequest` -> id | name | maxCpus | maxRuns | maxDuration | maxGpus,
/// against `GetRunGroupResponse` -> arn | id | name | maxCpus | maxRuns | maxDuration |
/// maxGpus | creationTime | tags.
const UNTOUCHED_BY_AN_UPDATE: &[&str] = &["arn", "id", "creationTime", "tags"];

/// `POST /runGroup/{id}` — the modelled `UpdateRunGroup` binding.
async fn update_group(router: &MultiServiceRouter, id: &str, body: &Value) -> (u16, Value) {
    call(router, "POST", &format!("/runGroup/{id}"), Some(body)).await
}

/// Create one group from `request` (the row's `requestId` is filled in from `case`) and
/// answer its id.
async fn group_for(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let mut body = request.as_object().cloned().unwrap_or_default();
    body.insert("requestId".to_string(), Value::String(case.to_string()));
    let (status, created) = create_group(router, &Value::Object(body)).await;
    created_id(case, status, &created)
}

/// Read a group back, asserting the 200 every stored group answers.
async fn stored(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = get_group(router, id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetRunGroup, got {body:?}"
    );
    body
}

/// Apply an update, asserting the modelled 202-with-no-body a successful one answers.
async fn apply_update(router: &MultiServiceRouter, case: &str, id: &str, body: &Value) {
    let (status, answered) = update_group(router, id, body).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected 202 from UpdateRunGroup, got {answered:?}"
    );
    assert_eq!(
        answered,
        Value::Null,
        "case {case:?}: expected no response body from UpdateRunGroup, got {answered:?}"
    );
}

/// A row of the update table: what the group was created with, what one update names, and
/// what every later read must then report.
struct UpdateCase {
    /// How the row is reported in a failure, and the group's `requestId`.
    case: &'static str,
    /// The create body, without the `requestId` the row's `case` fills in.
    create: Value,
    /// The `UpdateRunGroup` body, which carries no id: the id travels in the path.
    update: Value,
    /// Every member a read must carry with exactly this value after the update.
    expected: Value,
    /// Members a read must NOT carry after the update: an update invents nothing for a
    /// member neither it nor the create named.
    absent: &'static [&'static str],
}

/// The table: one row per way an update can differ from the next. The rows vary along which
/// members the update names, how many, whether the create had set them, and the values they
/// carry across the published cap range.
fn update_cases() -> Vec<UpdateCase> {
    let (low, high) = CAP_RANGE;
    vec![
        UpdateCase {
            case: "one cap named, every other setting left alone",
            create: json!({
                "name": "autumn cohort",
                "maxCpus": 4,
                "maxRuns": 2,
                "maxDuration": 60,
                "maxGpus": 1,
            }),
            update: json!({ "maxCpus": 16 }),
            expected: json!({
                "name": "autumn cohort",
                "maxCpus": 16,
                "maxRuns": 2,
                "maxDuration": 60,
                "maxGpus": 1,
            }),
            absent: &[],
        },
        UpdateCase {
            case: "every settable member at once",
            create: json!({
                "name": "winter cohort",
                "maxCpus": 4,
                "maxRuns": 2,
                "maxDuration": 60,
                "maxGpus": 1,
            }),
            update: json!({
                "name": "winter cohort (revised)",
                "maxCpus": 8,
                "maxRuns": 9,
                "maxDuration": 120,
                "maxGpus": 3,
            }),
            expected: json!({
                "name": "winter cohort (revised)",
                "maxCpus": 8,
                "maxRuns": 9,
                "maxDuration": 120,
                "maxGpus": 3,
            }),
            absent: &[],
        },
        UpdateCase {
            case: "the name alone, leaving every cap as created",
            create: json!({ "name": "spring cohort", "maxCpus": 5, "maxDuration": 45 }),
            update: json!({ "name": "spring cohort — renamed" }),
            expected: json!({
                "name": "spring cohort — renamed",
                "maxCpus": 5,
                "maxDuration": 45,
            }),
            absent: &["maxRuns", "maxGpus"],
        },
        UpdateCase {
            case: "a cap the create never set becomes set",
            create: json!({ "name": "summer cohort" }),
            update: json!({ "maxGpus": 7 }),
            expected: json!({ "name": "summer cohort", "maxGpus": 7 }),
            absent: &["maxCpus", "maxRuns", "maxDuration"],
        },
        UpdateCase {
            case: "every cap at a published bound of its range",
            create: json!({ "name": "bounds cohort", "maxCpus": 4, "maxRuns": 4 }),
            update: json!({
                "maxCpus": low,
                "maxRuns": high,
                "maxDuration": high,
                "maxGpus": low,
            }),
            expected: json!({
                "name": "bounds cohort",
                "maxCpus": low,
                "maxRuns": high,
                "maxDuration": high,
                "maxGpus": low,
            }),
            absent: &[],
        },
        UpdateCase {
            case: "a name at each published length bound",
            create: json!({ "name": "x".repeat(NAME_LENGTH.1), "maxRuns": 3 }),
            update: json!({ "name": "y".repeat(NAME_LENGTH.0) }),
            expected: json!({ "name": "y".repeat(NAME_LENGTH.0), "maxRuns": 3 }),
            absent: &["maxCpus", "maxDuration", "maxGpus"],
        },
        UpdateCase {
            case: "an update naming nothing changes nothing",
            create: json!({ "name": "unchanged cohort", "maxCpus": 4, "maxRuns": 2 }),
            update: json!({}),
            expected: json!({ "name": "unchanged cohort", "maxCpus": 4, "maxRuns": 2 }),
            absent: &["maxDuration", "maxGpus"],
        },
        UpdateCase {
            case: "a cap re-set to the value it already holds",
            create: json!({ "name": "idempotent cohort", "maxCpus": 4 }),
            update: json!({ "maxCpus": 4 }),
            expected: json!({ "name": "idempotent cohort", "maxCpus": 4 }),
            absent: &["maxRuns", "maxDuration", "maxGpus"],
        },
    ]
}

#[tokio::test]
async fn an_update_persists_what_it_names_and_leaves_the_rest() {
    let router = emulator();
    // Every row's group lives in the same store, so an update that reached past its own
    // record would show up as another row's read-back changing.
    let cases = update_cases();
    let mut groups = Vec::with_capacity(cases.len());
    for case in &cases {
        let mut create = case.create.as_object().cloned().unwrap_or_default();
        // Tags are not settable through this operation, so every row carries some: they are
        // part of what an update must leave alone.
        create.insert("tags".to_string(), tag_map(2));
        let id = group_for(&router, case.case, &Value::Object(create)).await;
        let before = stored(&router, case.case, &id).await;
        assert_arn(
            case.case,
            before
                .get("arn")
                .and_then(Value::as_str)
                .unwrap_or("<no arn>"),
            RUN_GROUP_ARN_RESOURCE,
            &id,
        );
        assert_timestamp(case.case, "creationTime", &before);
        groups.push((id, before));
    }

    for (case, (id, _)) in cases.iter().zip(&groups) {
        apply_update(&router, case.case, id, &case.update).await;
    }

    for (case, (id, before)) in cases.iter().zip(&groups) {
        let after = stored(&router, case.case, id).await;
        assert_members(case.case, "GetRunGroup", &case.expected, &after);
        assert_absent(case.case, case.absent, &after);
        // Nothing the update did not name moved, including the members it cannot name.
        for member in UNTOUCHED_BY_AN_UPDATE {
            assert_eq!(
                after.get(*member),
                before.get(*member),
                "case {:?}: expected {member} to stay {:?}, got {:?}",
                case.case,
                before.get(*member),
                after.get(*member)
            );
        }
    }

    // The listing reads the same one record, so every item agrees with the read-back.
    let (status, body) = list_groups(&router, "").await;
    let listed = items("the whole listing", status, &body);
    assert_eq!(
        listed.len(),
        cases.len(),
        "the whole listing: expected {} items, got {listed:?}",
        cases.len()
    );
    for (case, (id, _)) in cases.iter().zip(&groups) {
        let item = listed
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: group {id} is missing from the listing {listed:?}",
                    case.case
                )
            });
        assert_members(case.case, "ListRunGroups", &case.expected, item);
        assert_absent(case.case, case.absent, item);
    }
}

#[tokio::test]
async fn successive_updates_accumulate_per_member() {
    let router = emulator();
    let id = group_for(
        &router,
        "accumulating updates",
        &json!({ "name": "rolling cohort", "maxCpus": 2 }),
    )
    .await;

    // Each step names a different subset; the expected read-back after it is the value the
    // last update naming each member wrote, never a value an earlier step carried.
    let steps = [
        (
            "first step sets two caps",
            json!({ "maxCpus": 10, "maxRuns": 5 }),
            json!({ "name": "rolling cohort", "maxCpus": 10, "maxRuns": 5 }),
        ),
        (
            "second step renames and raises one cap",
            json!({ "name": "rolling cohort II", "maxRuns": 50 }),
            json!({ "name": "rolling cohort II", "maxCpus": 10, "maxRuns": 50 }),
        ),
        (
            "third step lowers a cap and adds another",
            json!({ "maxCpus": 1, "maxDuration": 30 }),
            json!({
                "name": "rolling cohort II",
                "maxCpus": 1,
                "maxRuns": 50,
                "maxDuration": 30,
            }),
        ),
    ];
    for (case, update, expected) in steps {
        apply_update(&router, case, &id, &update).await;
        let after = stored(&router, case, &id).await;
        assert_members(case, "GetRunGroup", &expected, &after);
        assert_absent(case, &["maxGpus"], &after);
    }
}

#[tokio::test]
async fn an_update_outside_a_published_constraint_is_refused_and_changes_nothing() {
    let router = emulator();
    let created = json!({
        "name": "constrained cohort",
        "maxCpus": 4,
        "maxRuns": 4,
        "maxDuration": 4,
        "maxGpus": 4,
    });
    let id = group_for(&router, "constrained cohort", &created).await;
    let (low, high) = CAP_RANGE;

    // Each cap carries the same published range, so each is refused one step outside either
    // bound; the name carries a published length, so the empty name and one character past
    // the maximum are refused too.
    let mut rejected: Vec<(String, Value)> = Vec::new();
    for member in CAP_MEMBERS {
        for value in [low - 1, high + 1] {
            rejected.push((
                format!("{member} at {value}"),
                member_object(member, json!(value)),
            ));
        }
    }
    rejected.push((
        format!("a name {} characters long", NAME_LENGTH.0 - 1),
        json!({ "name": "" }),
    ));
    rejected.push((
        format!("a name {} characters long", NAME_LENGTH.1 + 1),
        json!({ "name": "z".repeat(NAME_LENGTH.1 + 1) }),
    ));

    for (case, update) in rejected {
        let (status, body) = update_group(&router, &id, &update).await;
        assert_eq!(status, 400, "case {case:?}: expected 400, got {body:?}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        // A refused update is not a partial one: the record still reads as created.
        let after = stored(&router, &case, &id).await;
        assert_members(&case, "GetRunGroup", &created, &after);
    }
}

#[tokio::test]
async fn an_update_of_an_id_no_group_holds_is_not_found() {
    let router = emulator();
    let existing = group_for(
        &router,
        "the only group",
        &json!({ "name": "lonely cohort", "maxCpus": 3 }),
    )
    .await;
    let minted: u64 = existing.parse().expect("a decimal run group id");

    // Every id below is admissible for the modelled `RunGroupId` (`^[0-9]+$`, len 1..18) and
    // names no stored group: the shortest decimal, one the counter has not reached, one at
    // the widest the pattern allows, and the id another store in this service could mint.
    for id in [
        "0".to_string(),
        "9".to_string(),
        (minted + 1).to_string(),
        "9".repeat(18),
        (minted + 1_000_000).to_string(),
    ] {
        let (status, body) = update_group(&router, &id, &json!({ "maxCpus": 9 })).await;
        assert_eq!(
            status, 404,
            "case {id:?}: expected 404 from UpdateRunGroup, got {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {id:?}: expected ResourceNotFoundException, got {body:?}"
        );
    }

    // A refused update minted nothing and left the one stored group as created.
    let (status, body) = list_groups(&router, "").await;
    let listed = items("after the refusals", status, &body);
    assert_eq!(
        listed.len(),
        1,
        "after the refusals: expected exactly the one created group, got {listed:?}"
    );
    let after = stored(&router, "the only group", &existing).await;
    assert_members(
        "the only group",
        "GetRunGroup",
        &json!({ "id": existing, "name": "lonely cohort", "maxCpus": 3 }),
        &after,
    );
}
