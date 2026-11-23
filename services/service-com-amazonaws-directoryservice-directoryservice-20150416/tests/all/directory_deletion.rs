// SPDX-License-Identifier: CC0-1.0

//! The teardown `DeleteDirectory` performs, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router (the harness is the sibling suite's, reached as
//! `crate::directory_lifecycle::<item>`). Nothing a RESPONSE claims is read from the state
//! struct, so an answer that is not computed from persisted state cannot pass.
//!
//! The rule under test, asserted over a TABLE of directories rather than one recorded call:
//! a delete takes the directory OUT OF SERVICE inside the call. Everything the directory was
//! given is released by the time the delete answers — the interfaces its servers answered on
//! and the group they sat behind, in the service that owns them
//! (`crate::directory_server_interfaces` and `crate::directory_network_release` assert that
//! half), and the entry the data plane reads — and the record leaves the account's
//! directories with them, so every call that has to resolve the id to a live directory is
//! refused from that instant, while an id this account was NEVER issued is refused with the
//! other sentence, which is what lets a client tell its own deletion from an id that was
//! never there.
//!
//! What the delete does NOT do is make the directory vanish from the account's reporting: the
//! teardown is REPORTED, through the modelled `Deleting` stage the delete answers in and the
//! chain's terminal `Deleted`, and only once the record has left does the id FILTER the
//! reporting call to an empty page and stop being named by the unfiltered inventory. That is
//! the window a delete-and-wait client polls, so each case here waits for the CONDITION it is
//! about rather than for a span of time.
//!
//! The rows differ along what the two creates persist — create operation, directory type,
//! size, edition and network type — and every pass keeps a control directory in the same
//! store, so a teardown is shown to be per-directory rather than per-store.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::directory_lifecycle::{
    await_in_service, call, create_directory, describe_all, descriptions, minimal_create, ok,
    service, string, PASSWORD, STAGE_ACTIVE,
};
use crate::harness::{poll_until_absent, poll_until_reported, POLL_DEADLINE};
use crate::microsoft_ad::{create_microsoft_ad, minimal_microsoft_ad};
use service_com_amazonaws_directoryservice_directoryservice_20150416::state::{
    STAGE_DELETED, STAGE_DELETING,
};

/// Well-formed `DirectoryId`s (`^d-[0-9a-f]{10}$`) this account is never issued, so each one
/// reaches a read as a value the account has NEVER held — neither stored nor deleted. Varied
/// along their digits, so no case can pass by matching one value.
const NEVER_ISSUED: [&str; 3] = ["d-0000000000", "d-ffffffffff", "d-1234abcdef"];

/// One row of the directory table: how the directory is created.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
}

/// Create the row's directory through whichever create operation it names.
async fn create(router: &MultiServiceRouter, row: &Row) -> String {
    match row.operation {
        "CreateMicrosoftAD" => create_microsoft_ad(router, row.body.clone(), row.case).await,
        _ => create_directory(router, row.body.clone(), row.case).await,
    }
}

/// The table: rows differing along everything the two creates persist, so the rule is not
/// asserted on one shape of directory.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a minimal Simple AD directory",
            operation: "CreateDirectory",
            body: minimal_create("gone-simple.example.com"),
        },
        Row {
            case: "a large Simple AD directory with a dual-stack network",
            operation: "CreateDirectory",
            body: json!({
                "Name": "gone-large.example.com",
                "Password": PASSWORD,
                "Size": "Large",
                "NetworkType": "Dual-stack",
                "ShortName": "GONE"
            }),
        },
        Row {
            case: "a minimal Managed Microsoft AD directory",
            operation: "CreateMicrosoftAD",
            body: minimal_microsoft_ad("gone-corp.example.com"),
        },
        Row {
            case: "a Standard-edition Managed Microsoft AD directory",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "gone-standard.example.com",
                "Password": PASSWORD,
                "Edition": "Standard",
                "VpcSettings": {
                    "VpcId": "vpc-0a1b2c3d",
                    "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"]
                }
            }),
        },
    ]
}

/// The descriptions `DescribeDirectories` reports for the one id `directory_id`, which is
/// an empty page for an id the account no longer holds.
async fn page_for(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Vec<Value> {
    let answer = ok(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
        case,
    )
    .await;
    descriptions(&answer, case)
}

/// The `Stage` `DescribeDirectories` reports for `directory_id`, or `None` once the account
/// no longer holds the directory at all.
async fn reported_stage(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> Option<String> {
    let page = page_for(router, directory_id, case).await;
    assert!(
        page.len() <= 1,
        "case {case:?}: expected at most one description for {directory_id}, got {page:?}"
    );
    page.first().map(|described| string(described, "Stage", case))
}

/// `DeleteDirectory`, asserting it answers 200 with the id it was asked about.
async fn delete(router: &MultiServiceRouter, directory_id: &str, case: &str) {
    let answer = ok(
        router,
        "DeleteDirectory",
        json!({ "DirectoryId": directory_id }),
        case,
    )
    .await;
    assert_eq!(
        answer["DirectoryId"],
        json!(directory_id),
        "case {case:?}: expected the delete to report the id it was asked about, got {answer}"
    );
}

/// This service's refusal envelope: the diagnostic, then the call's OWN request id — which is
/// the one the body reports, never a value written into a case.
fn with_request_id(diagnostic: &str, request_id: &str) -> String {
    format!("{diagnostic} : RequestId: {request_id}")
}

/// Assert that `operation` with `body` is refused HTTP 400 / `EntityDoesNotExistException`
/// with `diagnostic` followed by the request id the refusal itself reports.
async fn refused_with(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    diagnostic: &str,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to be refused, got {status} with {parsed}"
    );
    assert_eq!(
        parsed["__type"],
        json!("EntityDoesNotExistException"),
        "case {case:?}: expected the modelled not-found refusal from {operation}, got {parsed}"
    );
    let request_id = string(&parsed, "RequestId", case);
    assert_eq!(
        string(&parsed, "Message", case),
        with_request_id(diagnostic, &request_id),
        "case {case:?}: expected {operation} to state the not-found sentence for the id the \
         request named, got {parsed}"
    );
}

/// The sentence a call whose subject has to be a directory this account holds is refused
/// with, for the id the REQUEST named (`state::no_such_directory`).
fn does_not_exist(directory_id: &str) -> String {
    format!("Directory: '{directory_id}' does not exist.")
}

/// The sentence a reporting call FILTERED by ids the account has never held is refused with,
/// naming those ids (`state::invalid_directory_ids`).
fn invalid_directory_ids(directory_ids: &[&str]) -> String {
    format!("Invalid directory ID(s): {}.", directory_ids.join(","))
}

/// Every call of this slice whose subject has to be a directory the account holds, named by
/// `DirectoryId`, for the id `directory_id`: each of them must refuse an id whose directory
/// is gone, with the sentence for an id that resolves to no directory
/// (`state::no_such_directory`).
fn calls_needing_a_live_directory(directory_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DeleteDirectory", json!({ "DirectoryId": directory_id })),
        ("GetSnapshotLimits", json!({ "DirectoryId": directory_id })),
        (
            "CreateSnapshot",
            json!({ "DirectoryId": directory_id, "Name": "after-the-delete" }),
        ),
        (
            "CreateAlias",
            json!({ "DirectoryId": directory_id, "Alias": "revived" }),
        ),
    ]
}

/// The calls of the TAG family for the same id, which take it as a `ResourceId`: they are
/// refused too, with that family's own sentence for an identifier that resolves to no
/// resource (`state::invalid_resource_id`, whose wording `crate::error_envelopes` pins), so
/// what is asserted here is that the directory is gone for them as well.
fn tag_calls(directory_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("ListTagsForResource", json!({ "ResourceId": directory_id })),
        (
            "AddTagsToResource",
            json!({
                "ResourceId": directory_id,
                "Tags": [{ "Key": "env", "Value": "dev" }],
            }),
        ),
        (
            "RemoveTagsFromResource",
            json!({ "ResourceId": directory_id, "TagKeys": ["owner"] }),
        ),
    ]
}

/// Assert that `operation` with `body` is refused HTTP 400 / `EntityDoesNotExistException`,
/// whatever the family's diagnostic says.
async fn refused_as_absent(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to be refused, got {status} with {parsed}"
    );
    assert_eq!(
        parsed["__type"],
        json!("EntityDoesNotExistException"),
        "case {case:?}: expected the modelled not-found refusal from {operation}, got {parsed}"
    );
}

#[tokio::test]
async fn a_delete_takes_the_directory_out_of_service_and_reports_its_teardown() {
    let router = service();
    let rows = rows();

    // The control directory is created first and never deleted, so every assertion below
    // is about the row's own directory rather than about the store emptying.
    let control = create_directory(
        &router,
        minimal_create("control.example.com"),
        "the control directory",
    )
    .await;
    let mut ids: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        ids.push((row, create(&router, row).await));
    }
    for (row, id) in &ids {
        await_in_service(&router, id, row.case).await;
    }

    for (row, id) in &ids {
        let case = row.case;
        // The directory is in service before the delete, so the directory the delete takes
        // away is one that really was being served.
        assert_eq!(
            reported_stage(&router, id, case).await.as_deref(),
            Some(STAGE_ACTIVE),
            "case {case:?}: expected the directory to be in service before the delete"
        );
        // A tag, so the record the delete takes away is one with content of its own: what
        // hangs off a directory goes when the directory does.
        ok(
            &router,
            "AddTagsToResource",
            json!({ "ResourceId": id, "Tags": [{ "Key": "owner", "Value": "platform" }] }),
            case,
        )
        .await;

        delete(&router, id, case).await;

        // The FIRST read after the delete, with nothing waited for: the directory is still
        // described, in the stage its teardown starts in — the window a delete-and-wait
        // client polls.
        assert_eq!(
            reported_stage(&router, id, case).await.as_deref(),
            Some(STAGE_DELETING),
            "case {case:?}: expected the teardown's first stage straight after the delete"
        );
        // Every call whose subject has to be a live directory is refused from that same
        // instant, with the sentence for an id that resolves to no directory and the
        // refusal's own request id: the directory has left service, however long its
        // teardown is still reported for.
        for (operation, body) in calls_needing_a_live_directory(id) {
            refused_with(&router, operation, body, &does_not_exist(id), case).await;
        }
        // ...and the tag calls, which name the same id as a RESOURCE, are refused too: the
        // tags that hung off the directory went with it.
        for (operation, body) in tag_calls(id) {
            refused_as_absent(&router, operation, body, case).await;
        }

        // The stage after that one is the terminal stage of the modelled chain, reached by
        // the clock alone — the polls only read.
        let settled = poll_until_reported(
            case,
            &format!("{id} to leave {STAGE_DELETING}"),
            POLL_DEADLINE,
            || reported_stage(&router, id, case),
            |observed: &Option<String>| observed.as_deref() != Some(STAGE_DELETING),
        )
        .await;
        assert_eq!(
            settled.as_deref(),
            Some(STAGE_DELETED),
            "case {case:?}: expected the teardown to reach the chain's terminal stage, got \
             {settled:?}"
        );
        // ...and the end of the teardown is the record leaving the account's records: the id
        // then FILTERS the reporting call to an empty page, which is what a delete-and-wait
        // client reads its own completed deletion off.
        poll_until_absent(&router, id, case).await;
        assert_eq!(
            page_for(&router, id, case).await,
            Vec::<Value>::new(),
            "case {case:?}: expected the deleted id to narrow the page to nothing"
        );
        // ...and the unfiltered inventory does not name it either.
        let inventory: Vec<String> = describe_all(&router, case)
            .await
            .iter()
            .map(|described| string(described, "DirectoryId", case))
            .collect();
        assert!(
            !inventory.contains(id),
            "case {case:?}: expected {id} to be absent from the account's inventory, got \
             {inventory:?}"
        );
        // The calls that must resolve the id to a live directory are refused past the
        // teardown as well, in the same words.
        for (operation, body) in calls_needing_a_live_directory(id) {
            refused_with(&router, operation, body, &does_not_exist(id), case).await;
        }
    }

    // The control directory went through none of it: a teardown is the named directory's.
    assert_eq!(
        reported_stage(&router, &control, "the control directory")
            .await
            .as_deref(),
        Some(STAGE_ACTIVE),
        "case \"the control directory\": expected the untouched directory to still be in \
         service"
    );
}

#[tokio::test]
async fn a_deleted_id_is_told_apart_from_one_the_account_was_never_issued() {
    let router = service();
    let case = "the two absences";
    let deleted = create_directory(&router, minimal_create("told-apart.example.com"), case).await;
    let kept = create_directory(&router, minimal_create("kept.example.com"), case).await;
    await_in_service(&router, &deleted, case).await;

    delete(&router, &deleted, case).await;
    // An id whose teardown is still being reported is still described, so the narrowing
    // below is asserted once the record has LEFT — the condition, not a span of time.
    poll_until_absent(&router, &deleted, case).await;

    // An id this account HAD: the reporting call filtered by it is SERVED, with an empty
    // page, and a live id beside it is still reported.
    let mixed = ok(
        &router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [&deleted, &kept] }),
        case,
    )
    .await;
    let reported: Vec<String> = descriptions(&mixed, case)
        .iter()
        .map(|described| string(described, "DirectoryId", case))
        .collect();
    assert_eq!(
        reported,
        vec![kept.clone()],
        "case {case:?}: expected the deleted id to narrow away and the live one to remain"
    );

    // An id the account was never issued is a value the listing cannot be narrowed by, so it
    // is refused instead — naming exactly the ids that were never held, in filter order.
    for never in NEVER_ISSUED {
        refused_with(
            &router,
            "DescribeDirectories",
            json!({ "DirectoryIds": [never] }),
            &invalid_directory_ids(&[never]),
            case,
        )
        .await;
        refused_with(
            &router,
            "DescribeDirectories",
            json!({ "DirectoryIds": [&deleted, never, &kept] }),
            &invalid_directory_ids(&[never]),
            case,
        )
        .await;
    }
    refused_with(
        &router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [NEVER_ISSUED[2], &deleted, NEVER_ISSUED[0]] }),
        &invalid_directory_ids(&[NEVER_ISSUED[2], NEVER_ISSUED[0]]),
        case,
    )
    .await;

    // ...and for the calls that must resolve the id to a live directory, the two absences are
    // one and the same answer: there is no directory to act on either way.
    for subject in [deleted.as_str(), NEVER_ISSUED[1]] {
        for (operation, body) in calls_needing_a_live_directory(subject) {
            refused_with(&router, operation, body, &does_not_exist(subject), case).await;
        }
        for (operation, body) in tag_calls(subject) {
            refused_as_absent(&router, operation, body, case).await;
        }
    }

    assert_eq!(
        reported_stage(&router, &kept, case).await.as_deref(),
        Some(STAGE_ACTIVE),
        "case {case:?}: expected the other directory to be untouched"
    );
}

#[tokio::test]
async fn what_describes_nothing_without_the_directory_goes_with_its_record() {
    let router = service();
    let case = "the cascade";
    let doomed =
        create_microsoft_ad(&router, minimal_microsoft_ad("doomed.example.com"), case).await;
    let kept = create_microsoft_ad(&router, minimal_microsoft_ad("kept.example.com"), case).await;
    for id in [&doomed, &kept] {
        await_in_service(&router, id, case).await;
    }

    // A snapshot OF each directory, so the one taken of the doomed directory is a record
    // that describes nothing once its directory's record is gone.
    let mut snapshots = Vec::new();
    for (directory_id, name) in [(&doomed, "doomed-nightly"), (&kept, "kept-nightly")] {
        let answer = ok(
            &router,
            "CreateSnapshot",
            json!({ "DirectoryId": directory_id, "Name": name }),
            case,
        )
        .await;
        snapshots.push(string(&answer, "SnapshotId", case));
    }

    delete(&router, &doomed, case).await;

    // The record is gone, and with it the snapshot taken of that directory — the cascade
    // follows the record, and the record goes with the delete.
    let (status, parsed) = call(
        &router,
        "DescribeSnapshots",
        json!({ "SnapshotIds": [&snapshots[0]] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a snapshot of the deleted directory to resolve to nothing, \
         got {status} with {parsed}"
    );
    // ...while the other directory's snapshot is untouched.
    let surviving = ok(
        &router,
        "DescribeSnapshots",
        json!({ "SnapshotIds": [&snapshots[1]] }),
        case,
    )
    .await;
    assert_eq!(
        surviving["Snapshots"]
            .as_array()
            .and_then(|infos| infos.first())
            .map(|info| info["SnapshotId"].clone()),
        Some(json!(&snapshots[1])),
        "case {case:?}: expected the other directory's snapshot to survive, got {surviving}"
    );
}
