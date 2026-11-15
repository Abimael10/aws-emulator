// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the lifecycle state a stack reports.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rule under test, asserted over a TABLE of stacks and of writes rather than over one
//! recorded body: a stack's lifecycle state is part of the stack, written onto the record
//! by the create and projected from there — so EVERY operation that reports a stack
//! (`CreateStack`, `UpdateStack` and `DescribeStacks`, whether it reports one stack or a
//! page of them) reports the same state, and no configuration the request carries, no
//! member an update replaces and no attribute an update clears changes it.

use serde_json::{json, Value};

use crate::stack_fleet_lifecycle::Emulator;

/// The state a stack reports once it is created. A stack provisions nothing of its own —
/// it is the association of a fleet with the access policies and storage a session gets —
/// so it is reported as usable from the create onwards, and no operation of this service
/// moves it anywhere else.
const ACTIVE: &str = "ACTIVE";

/// The member the state is reported under, as the stack shape spells it.
const STATE: &str = "State";

/// One stack: the create request it is made with, and the update applied to it afterwards.
///
/// The rows differ along what the rule generalises over: how much configuration the create
/// carries (a name alone, up to every member the service settles itself given explicitly),
/// and what the update then does to the stored record — replace a scalar, replace a
/// collection the stack reports, clear an attribute, or change nothing but the name it
/// names. Member names and enum values come from `smithy_query(query="operation",
/// operation="CreateStack")` / `operation="UpdateStack"` and the `*_VALUES` constants in
/// `src/ops/types/shared_types.rs`.
struct StateRow {
    case: &'static str,
    create: Value,
    update: Value,
}

fn state_rows() -> Vec<StateRow> {
    vec![
        StateRow {
            case: "a name and nothing else, updated on its description",
            create: json!({ "Name": "state-name-only" }),
            update: json!({ "Name": "state-name-only", "Description": "after" }),
        },
        StateRow {
            case: "a create that sets the members the service would settle itself",
            create: json!({
                "Name": "state-fully-configured",
                "Description": "a stack that configures everything it can",
                "DisplayName": "fully configured",
                "UserSettings": [{ "Action": "FILE_UPLOAD", "Permission": "DISABLED" }],
                "ApplicationSettings": { "Enabled": true, "SettingsGroup": "group-a" },
                "StreamingExperienceSettings": { "PreferredProtocol": "UDP" },
                "StorageConnectors": [{ "ConnectorType": "HOMEFOLDERS" }],
                "EmbedHostDomains": ["a.example.invalid", "b.example.invalid"],
                "RedirectURL": "https://redirect.example.invalid/",
                "FeedbackURL": "https://feedback.example.invalid/",
            }),
            update: json!({
                "Name": "state-fully-configured",
                "StreamingExperienceSettings": { "PreferredProtocol": "TCP" },
            }),
        },
        StateRow {
            case: "an update that replaces a collection the stack reports",
            create: json!({
                "Name": "state-replaced-collection",
                "EmbedHostDomains": ["a.example.invalid"],
            }),
            update: json!({
                "Name": "state-replaced-collection",
                "EmbedHostDomains": ["b.example.invalid", "c.example.invalid"],
            }),
        },
        StateRow {
            case: "an update that clears an attribute",
            create: json!({
                "Name": "state-cleared-attribute",
                "RedirectURL": "https://redirect.example.invalid/",
            }),
            update: json!({
                "Name": "state-cleared-attribute",
                "AttributesToDelete": ["REDIRECT_URL"],
            }),
        },
        StateRow {
            case: "an update that changes nothing about the stack",
            create: json!({ "Name": "state-untouched", "Description": "unchanged" }),
            update: json!({ "Name": "state-untouched" }),
        },
    ]
}

/// The state the stack in `payload` reports, as a string, naming the case when the member
/// is missing or is not a string.
fn reported_state(case: &str, label: &str, stack: &Value) -> String {
    stack
        .get(STATE)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {label} reported no {STATE}, got {stack}"))
        .to_string()
}

#[tokio::test]
async fn every_operation_that_reports_a_stack_reports_its_state() {
    let emulator = Emulator::new();
    let rows = state_rows();

    for row in &rows {
        let StateRow {
            case,
            create,
            update,
        } = row;
        let name = create["Name"]
            .as_str()
            .expect("every row names a stack")
            .to_string();

        let created = emulator.ok(case, "CreateStack", create.clone()).await;
        let created_state = reported_state(case, "CreateStack", &created["Stack"]);
        assert_eq!(
            created_state, ACTIVE,
            "case {case:?}: expected CreateStack to report {STATE} {ACTIVE:?}, got \
             {created_state:?}"
        );

        // ROUND TRIP: a reader that did not perform the create reports the state off the
        // stored record, not off the request.
        let described = emulator
            .ok(case, "DescribeStacks", json!({ "Names": [&name] }))
            .await;
        assert_eq!(
            described["Stacks"][0], created["Stack"],
            "case {case:?}: expected DescribeStacks to report the created stack {}, got \
             {described}",
            created["Stack"]
        );

        // The update's own response is the one that tells a client what the stack looks
        // like after its change, so it projects the state as well.
        let updated = emulator.ok(case, "UpdateStack", update.clone()).await;
        let updated_state = reported_state(case, "UpdateStack", &updated["Stack"]);
        assert_eq!(
            updated_state, ACTIVE,
            "case {case:?}: expected UpdateStack to report {STATE} {ACTIVE:?}, got \
             {updated_state:?}"
        );

        let read_back = emulator
            .ok(case, "DescribeStacks", json!({ "Names": [&name] }))
            .await;
        assert_eq!(
            read_back["Stacks"][0], updated["Stack"],
            "case {case:?}: expected DescribeStacks to report the updated stack {}, got \
             {read_back}",
            updated["Stack"]
        );
    }

    // A read that reports a PAGE of stacks reports the state on every one of them, the
    // same as a read of a single name does.
    let case = "every stored stack reported at once";
    let all = emulator.ok(case, "DescribeStacks", json!({})).await;
    let stacks = all["Stacks"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a list of stacks, got {all}"));
    assert_eq!(
        stacks.len(),
        rows.len(),
        "case {case:?}: expected {} stacks, got {all}",
        rows.len()
    );
    for stack in stacks {
        let state = reported_state(case, "DescribeStacks", stack);
        assert_eq!(
            state, ACTIVE,
            "case {case:?}: expected {STATE} {ACTIVE:?} on every reported stack, got \
             {state:?} on {stack}"
        );
    }
}

#[tokio::test]
async fn a_refused_write_reports_no_state_and_a_deleted_stack_reports_nothing() {
    let emulator = Emulator::new();
    let case = "a stack created, refused a duplicate create, then deleted";

    let created = emulator
        .ok(case, "CreateStack", json!({ "Name": "state-lifecycle" }))
        .await;
    let created_state = reported_state(case, "CreateStack", &created["Stack"]);
    assert_eq!(
        created_state, ACTIVE,
        "case {case:?}: expected {STATE} {ACTIVE:?}, got {created_state:?}"
    );

    // A refusal carries no stack at all, so there is no state on it to read: the member is
    // projected from a stored record, never synthesised onto a response.
    let (status, refused) = emulator
        .call("CreateStack", json!({ "Name": "state-lifecycle" }))
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the second create to be refused, got {status} {refused}"
    );
    assert!(
        refused.get("Stack").is_none(),
        "case {case:?}: expected a refusal to carry no Stack, got {refused}"
    );

    emulator
        .ok(case, "DeleteStack", json!({ "Name": "state-lifecycle" }))
        .await;
    let (status, read) = emulator
        .call("DescribeStacks", json!({ "Names": ["state-lifecycle"] }))
        .await;
    assert_ne!(
        status, 200,
        "case {case:?}: expected a deleted stack to be reported by nothing, got {read}"
    );
}
