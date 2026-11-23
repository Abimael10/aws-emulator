// SPDX-License-Identifier: CC0-1.0

//! `DeleteNetworkConnector` is idempotent while the connector exists, and a transition the
//! clock owes is reported by the first read taken once it falls due.
//!
//! THE RULES, each asserted over a SET of inputs rather than over one recorded call:
//!
//! 1. A delete of a connector this account and Region still HAS is accepted — `HTTP 202`
//!    with the resource's identity (`Arn`, `Name`, `Id` are `@required`) and the state it
//!    carries — however often it is asked for and whichever identifier FORM asks:
//!    `DeleteNetworkConnector` is modelled `idempotent: true` with success code 202
//!    (`smithy_query(query="operation",operation="DeleteNetworkConnector")`) and documented
//!    "this operation is idempotent — calling delete on a connector that is already deleting
//!    or has been deleted succeeds without error", so a connector already `DELETING` is
//!    deleted again without error. Once the deletion has COMPLETED the resource is gone, so
//!    the identifier names no connector of this account and Region and the call is refused
//!    with the modelled `ResourceNotFoundException` — "the specified network connector does
//!    not exist" — exactly as an identifier that never named one is. The rows below span
//!    what the rule generalises over: which identifier form repeats the delete (name,
//!    connector id, unqualified ARN, version-qualified ARN), how many versions the connector
//!    held, whether the repeat follows the delete while the record is still there or after
//!    the deletion completed, and the delete of a single historical version repeated on its
//!    own.
//!
//! 2. Every operation reports the state the resource has REACHED: the outcome of
//!    asynchronous work is owed from the instant the call that started it answered, so the
//!    FIRST call after it reports that outcome and no reached state is withheld from a
//!    caller that polls the documented way — however often, or seldom, it looks. The
//!    transient (`PENDING`, `InProgress`, `DELETING`) is carried by the answer of the call
//!    that starts the work, and a deletion keeps the window the model's idempotent repeat
//!    is stated over.
//!
//! The harness (router, request helpers) is the lifecycle suite's; it is `pub(crate)` there
//! so every suite drives the emulator the same way. A case that needs a transition NOT to
//! have fallen due yet, or to have fallen due already, moves the instant the record itself
//! carries (`lifecycle_convergence::reschedule`) rather than racing a span.

use std::time::SystemTime;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::lifecycle_convergence::{reschedule, ALREADY_DUE, NOT_YET_DUE};
use super::network_connector_lifecycle::{
    at, create, delete, emulator, emulator_with_engine, expected_arn, get, list,
    minimal_configuration, minimal_create, text, update, vpc_egress, Answer, OPERATOR_ROLE,
};
use super::not_found_message::{expected_message, expected_update_message};

/// The state `DeleteNetworkConnectorResponse$State` reports — "the State field is typically
/// `DELETING` after this call" — and the state a connector whose deletion completed last
/// carried.
const DELETING: &str = "DELETING";

/// Create `name` and answer the connector's id. Provisioning settles from the instant the
/// create answered, so the next call finds the connector `ACTIVE` and deletable.
async fn active_connector(router: &MultiServiceRouter, name: &str, subnets: &[&str]) -> String {
    let created = create(
        router,
        json!({
            "Name": name,
            "Configuration": vpc_egress(subnets, &[], None, None),
            "OperatorRole": OPERATOR_ROLE,
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "creating {name:?} must succeed, got {} with {:?}",
        created.status, created.body
    );
    text(&created.body, "/Id")
}

/// Append `count` versions to `name` by changing its configuration, so there are historical
/// versions to address.
async fn append_versions(router: &MultiServiceRouter, name: &str, count: usize) {
    // Each row is a configuration the connector does not already hold, since "each
    // configuration update creates a new immutable version" only for a real change.
    let subnets = [
        "subnet-0fedcba9876543210",
        "subnet-00112233445566778",
        "subnet-0a1b2c3d4e5f60718",
        "subnet-01234567890abcdef",
    ];
    for index in 0..count {
        let updated = update(
            router,
            name,
            json!({ "Configuration": vpc_egress(&subnets[index % subnets.len()..][..1], &[], None, None) }),
        )
        .await;
        assert_eq!(
            updated.status, 202,
            "appending a version to {name:?} must succeed, got {} with {:?}",
            updated.status, updated.body
        );
    }
}

/// Assert `answer` is the accepted, idempotent delete of the connector `id` names.
fn assert_accepted(case: &str, answer: &Answer, arn: &str, name: &str, id: &str) {
    assert_eq!(
        answer.status, 202,
        "case {case:?}: an idempotent delete is accepted with HTTP 202, got {} with {:?}",
        answer.status, answer.body
    );
    for (member, expected) in [("/Arn", arn), ("/Name", name), ("/Id", id)] {
        assert_eq!(
            text(&answer.body, member),
            expected,
            "case {case:?}: the answer must report the stored {member} {expected:?}, got {:?}",
            at(&answer.body, member)
        );
    }
    assert_eq!(
        text(&answer.body, "/State"),
        DELETING,
        "case {case:?}: expected State {DELETING:?}, got {:?}",
        at(&answer.body, "/State")
    );
}

/// Assert `answer` is the modelled 404 for `identifier`, as the read, the delete and the
/// version listing word it.
///
/// What the refusal names follows from the IDENTIFIER: the connector's ARN for an id, the
/// value as supplied for an ARN or a name. `UpdateNetworkConnector` words it naming the
/// identifier the request supplied ([`assert_not_found_update`]);
/// `crate::not_found_message` states both over every identifier form and every operation
/// addressed by one.
fn assert_not_found(case: &str, answer: &Answer, identifier: &str) {
    assert_not_found_naming(case, answer, &expected_message(identifier));
}

/// [`assert_not_found`] as `UpdateNetworkConnector` words it: naming the identifier the
/// request supplied.
fn assert_not_found_update(case: &str, answer: &Answer, identifier: &str) {
    assert_not_found_naming(case, answer, &expected_update_message(identifier));
}

/// Assert `answer` is the modelled 404 carrying exactly `expected`.
fn assert_not_found_naming(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected HTTP 404, got {} with {:?}",
        answer.status, answer.body
    );
    let code = text(&answer.body, "/__type");
    assert!(
        code.ends_with("ResourceNotFoundException"),
        "case {case:?}: expected the error code ResourceNotFoundException, got {code:?}"
    );
    let message = text(&answer.body, "/Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// RULE 1: repeating a delete of a connector that is still there succeeds, whichever
/// identifier form repeats it and however many versions the connector held — and once the
/// deletion has completed the same call is refused, because the identifier now names
/// nothing.
#[tokio::test]
async fn a_repeated_delete_is_accepted_until_the_deletion_completes() {
    // Rows differ along: which form the repeat addresses the connector by, how many
    // versions it held, and whether the repeat comes before or after the deletion
    // completes.
    let cases: [(&str, usize, bool); 8] = [
        ("by name, one version, still deleting", 0, false),
        ("by name, one version, deletion completed", 0, true),
        ("by id, one version, deletion completed", 0, true),
        ("by unqualified ARN, one version, still deleting", 0, false),
        ("by version-qualified ARN, still deleting", 2, false),
        ("by id, three versions, still deleting", 2, false),
        ("by id, three versions, deletion completed", 2, true),
        ("by name, four versions, deletion completed", 3, true),
    ];

    for (index, (case, extra_versions, completed)) in cases.iter().enumerate() {
        let (router, engine) = emulator_with_engine();
        let name = format!("repeat-{index}");
        let id = active_connector(&router, &name, &["subnet-0123456789abcdef0"]).await;
        let arn = expected_arn(&id);
        append_versions(&router, &name, *extra_versions).await;

        let first = delete(&router, &name).await;
        assert_accepted(&format!("{case}: the first delete"), &first, &arn, &name, &id);

        if *completed {
            // The deletion the call started is owed by the clock; once it falls due the
            // record is forgotten and `GetNetworkConnector` refuses the identifier.
            reschedule(&engine, &name, case, SystemTime::now() - ALREADY_DUE);
            let gone = get(&router, &name).await;
            assert_not_found(
                &format!("{case}: the connector after deletion"),
                &gone,
                &name,
            );
        } else {
            // The repeat this row is about arrives while the record is still there, which
            // is the window the clock owes the removal: the case holds that window open
            // rather than racing it.
            reschedule(&engine, &name, case, SystemTime::now() + NOT_YET_DUE);
        }

        // Every form the caller could repeat the delete with resolves to the same
        // connector, so the row's form is the one asserted and the rest are checked too.
        let identifier = match *case {
            c if c.contains("by id") => id.clone(),
            c if c.contains("by unqualified ARN") => arn.clone(),
            c if c.contains("by version-qualified ARN") => format!("{arn}:1"),
            _ => name.clone(),
        };
        let repeated = delete(&router, &identifier).await;
        if *completed {
            assert_not_found(&format!("{case}: the repeat"), &repeated, &identifier);
        } else {
            assert_accepted(&format!("{case}: the repeat"), &repeated, &arn, &name, &id);
            if identifier.ends_with(":1") {
                assert_eq!(
                    at(&repeated.body, "/Version"),
                    json!(1),
                    "case {case:?}: the answer reports the version the request addressed, \
                     got {:?}",
                    at(&repeated.body, "/Version")
                );
            }
        }

        // A third delete answers the same way: the rule is about whether the connector is
        // still there, not about how many times it was asked for.
        let third = delete(&router, &identifier).await;
        if *completed {
            assert_not_found(
                &format!("{case}: the third delete"),
                &third,
                &identifier,
            );
        } else {
            assert_accepted(&format!("{case}: the third delete"), &third, &arn, &name, &id);
        }

        // And the connector is still gone from every reporting operation.
        if *completed {
            let after = get(&router, &name).await;
            assert_not_found(
                &format!("{case}: a repeated delete must not bring the connector back"),
                &after,
                &name,
            );
            let listed = list(&router, "").await;
            assert_eq!(
                listed.body["NetworkConnectors"],
                json!([]),
                "case {case:?}: a deleted connector stays absent from ListNetworkConnectors, \
                 got {:?}",
                listed.body
            );
        }
    }
}

/// RULE 1, the other side of it: an identifier that names no connector of this account and
/// Region is refused with the modelled 404 — one that never named anything, one whose
/// connector's deletion has completed, and a version number above the last version that
/// connector ever held.
#[tokio::test]
async fn an_identifier_that_names_no_connector_is_refused() {
    let (router, engine) = emulator_with_engine();
    let name = "short-lived";
    let id = active_connector(&router, name, &["subnet-0123456789abcdef0"]).await;
    let arn = expected_arn(&id);
    // Two versions, so the connector's last version number is 2.
    append_versions(&router, name, 1).await;
    let accepted = delete(&router, name).await;
    assert_accepted("the delete under test", &accepted, &arn, name, &id);
    reschedule(
        &engine,
        name,
        "the deletion under test",
        SystemTime::now() - ALREADY_DUE,
    );

    // Each row is an identifier that names no connector of this account: a name, an id and
    // an ARN that were never minted, a version one past the deleted connector's last one, a
    // far higher version, and — the deletion having completed — every form that DID name the
    // connector, including the versions it held.
    let absent = [
        ("a name that never existed", "never-created".to_string()),
        (
            "an id the service never minted",
            "nc-ffffffffffffffff".to_string(),
        ),
        (
            "the ARN of a name that never existed",
            expected_arn("never-created"),
        ),
        ("the version one past the last", format!("{arn}:3")),
        ("a far higher version", format!("{arn}:4096")),
        ("the deleted connector's name", name.to_string()),
        ("the deleted connector's id", id.clone()),
        ("the deleted connector's ARN", arn.clone()),
        ("a version the deleted connector held", format!("{arn}:1")),
        (
            "its latest version, after the deletion",
            format!("{arn}:2"),
        ),
    ];
    for (case, identifier) in &absent {
        let answer = delete(&router, identifier).await;
        assert_not_found(case, &answer, identifier);
    }
}

/// RULE 1, one level down: deleting a historical version again succeeds, while the latest
/// version is still refused on its own and a version the connector never minted is still
/// the modelled 404.
#[tokio::test]
async fn a_repeated_delete_of_a_historical_version_is_accepted() {
    let router = emulator();
    let name = "versions";
    let id = active_connector(&router, name, &["subnet-0123456789abcdef0"]).await;
    let arn = expected_arn(&id);
    // Versions 1..=4, so there are three historical versions to remove on their own.
    append_versions(&router, name, 3).await;

    for version in [1, 2, 3] {
        let identifier = format!("{arn}:{version}");
        let first = delete(&router, &identifier).await;
        assert_eq!(
            first.status, 202,
            "case version {version}: deleting a historical version must succeed, got {} \
             with {:?}",
            first.status, first.body
        );
        assert_eq!(
            at(&first.body, "/Version"),
            json!(version),
            "case version {version}: the answer reports the version it removed, got {:?}",
            at(&first.body, "/Version")
        );

        let repeated = delete(&router, &identifier).await;
        assert_eq!(
            repeated.status, 202,
            "case version {version}: repeating the delete of a version already removed \
             succeeds, got {} with {:?}",
            repeated.status, repeated.body
        );
        for (member, expected) in [("/Arn", arn.clone()), ("/Name", name.to_string()), ("/Id", id.clone())] {
            assert_eq!(
                text(&repeated.body, member),
                expected,
                "case version {version}: the repeat reports the stored {member}, got {:?}",
                at(&repeated.body, member)
            );
        }
        assert_eq!(
            at(&repeated.body, "/Version"),
            json!(version),
            "case version {version}: the repeat reports the version addressed, got {:?}",
            at(&repeated.body, "/Version")
        );
    }

    // The latest version is never deletable on its own, however often it is asked for.
    for attempt in [1, 2] {
        let latest = delete(&router, &format!("{arn}:4")).await;
        assert_eq!(
            latest.status, 409,
            "attempt {attempt}: \"you can't delete the latest version on its own\", got {} \
             with {:?}",
            latest.status, latest.body
        );
    }

    // And a version this connector never minted names nothing.
    for version in [5, 99] {
        let identifier = format!("{arn}:{version}");
        let answer = delete(&router, &identifier).await;
        assert_not_found(
            &format!("a version never minted ({version})"),
            &answer,
            &identifier,
        );
    }

    // The connector itself still answers, with the versions the deletes left.
    let alive = get(&router, name).await;
    assert_eq!(
        alive.status, 200,
        "the connector survives every version delete, got {} with {:?}",
        alive.status, alive.body
    );
}

/// RULE 1 and the name-release contract together: a completed deletion frees the `Name`, a
/// re-create on it mints a NEW id, reads by name rebind to the new connector, and the old id
/// names nothing through any operation.
#[tokio::test]
async fn a_freed_name_rebinds_while_the_old_id_names_nothing() {
    let (router, engine) = emulator_with_engine();
    let name = "reused";
    let first_id = active_connector(&router, name, &["subnet-0123456789abcdef0"]).await;
    let arn = expected_arn(&first_id);
    let accepted = delete(&router, name).await;
    assert_accepted("the first delete", &accepted, &arn, name, &first_id);
    reschedule(
        &engine,
        name,
        "the deletion that frees the name",
        SystemTime::now() - ALREADY_DUE,
    );

    let recreated = create(&router, minimal_create(name)).await;
    assert_eq!(
        recreated.status, 202,
        "the name a deleted connector held is free again, got {} with {:?}",
        recreated.status, recreated.body
    );
    let second_id = text(&recreated.body, "/Id");
    assert_ne!(
        second_id, first_id,
        "the re-created connector gets its own id, got {second_id:?}"
    );
    assert_eq!(
        text(&recreated.body, "/State"),
        "PENDING",
        "a connector is created PENDING, got {:?}",
        at(&recreated.body, "/State")
    );

    // A read by name resolves the LIVE connector, never the deletion left behind.
    let by_name = get(&router, name).await;
    assert_eq!(
        text(&by_name.body, "/Id"),
        second_id,
        "GetNetworkConnector by name must report the live connector, got {:?}",
        at(&by_name.body, "/Id")
    );

    // The dead id is refused by every operation addressed by one, each naming what IT
    // addressed — the ARN that id determines from the read, the id as supplied from the
    // writes, which is the rule `crate::not_found_message` states over every form.
    let dead = get(&router, &first_id).await;
    assert_not_found("the dead id through Get", &dead, &first_id);
    let dead_update = update(
        &router,
        &first_id,
        json!({ "Configuration": minimal_configuration() }),
    )
    .await;
    assert_not_found_update("the dead id through Update", &dead_update, &first_id);

    // … and the delete too: the connector that id named is gone, so the identifier names
    // no connector of this account — not the live one that took its name either.
    let repeated = delete(&router, &first_id).await;
    assert_not_found("the dead id through Delete", &repeated, &first_id);

    // Deleting by NAME now addresses the live connector, which is the one that goes — and
    // it answers under its OWN ARN, whose resource segment is its own Id.
    let live = delete(&router, name).await;
    assert_accepted(
        "the live connector by name",
        &live,
        &expected_arn(&second_id),
        name,
        &second_id,
    );
}

/// RULE 2: the first call after the one that started asynchronous work already reports the
/// value the work reached — nothing is withheld for a later call — and the transient is
/// carried by the starting call's own answer.
#[tokio::test]
async fn the_first_read_after_a_transition_falls_due_reports_the_reached_state() {
    // Provisioning: the create answers PENDING, and the FIRST call after it reports ACTIVE.
    let (router, engine) = emulator_with_engine();
    let created = create(&router, minimal_create("settling")).await;
    assert_eq!(
        text(&created.body, "/State"),
        "PENDING",
        "the create itself carries the transient state, got {:?}",
        at(&created.body, "/State")
    );
    let settled = get(&router, "settling").await;
    assert_eq!(
        text(&settled.body, "/State"),
        "ACTIVE",
        "the first read after the create reports the state provisioning reached, got {:?}",
        at(&settled.body, "/State")
    );

    // The same with NO read in between: nothing was waiting for an earlier call to
    // materialise it, so a client whose budget allows a single look still sees it.
    let (second_router, _second_engine) = emulator_with_engine();
    create(&second_router, minimal_create("unread")).await;
    let first_read = get(&second_router, "unread").await;
    assert_eq!(
        text(&first_read.body, "/State"),
        "ACTIVE",
        "the very first read after a create reports ACTIVE, got {:?}",
        at(&first_read.body, "/State")
    );

    // An update's own asynchronous work, reported by the first read after it.
    let updated = update(
        &router,
        "settling",
        json!({ "Configuration": vpc_egress(&["subnet-0fedcba9876543210"], &[], None, None) }),
    )
    .await;
    assert_eq!(
        text(&updated.body, "/LastUpdateStatus"),
        "InProgress",
        "the update itself carries the transient status, got {:?}",
        at(&updated.body, "/LastUpdateStatus")
    );
    let after_update = get(&router, "settling").await;
    assert_eq!(
        text(&after_update.body, "/LastUpdateStatus"),
        "Successful",
        "the first read after the update reports Successful, got {:?}",
        at(&after_update.body, "/LastUpdateStatus")
    );

    // And a deletion, whose removal the clock owes over a window the record is still there
    // for: DELETING in the answer and for as long as that window is open, and once it falls
    // due the identifier is refused — "after deletion completes, subsequent calls to
    // GetNetworkConnector return ResourceNotFoundException".
    let removed = delete(&router, "settling").await;
    assert_eq!(
        text(&removed.body, "/State"),
        DELETING,
        "the delete itself carries the transient state, got {:?}",
        at(&removed.body, "/State")
    );
    reschedule(
        &engine,
        "settling",
        "a deletion the clock still owes",
        SystemTime::now() + NOT_YET_DUE,
    );
    let while_deleting = get(&router, "settling").await;
    assert_eq!(
        text(&while_deleting.body, "/State"),
        DELETING,
        "a read inside the window the removal is owed over reports DELETING, got {:?}",
        at(&while_deleting.body, "/State")
    );
    reschedule(
        &engine,
        "settling",
        "a deletion that has fallen due",
        SystemTime::now() - ALREADY_DUE,
    );
    let after_delete = get(&router, "settling").await;
    assert_not_found(
        "the first read after the deletion is due",
        &after_delete,
        "settling",
    );
}
