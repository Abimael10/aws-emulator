// SPDX-License-Identifier: CC0-1.0

//! The application read path: where `GetApplication` reports the application's content
//! is replicated, and how many bytes of it the application holds.
//!
//! Every case goes through the same front door a client uses — a `restJson1` request
//! against a router that mounts this service beside this emulator's own Amazon S3 on ONE
//! engine — so the bytes `StorageBytes` reports are the bytes of real objects, staged
//! through Amazon S3's own API, and nothing here reaches into a state struct.
//!
//! The two rules under test, from this service's Smithy model
//! (`smithy_query(query="shape", shape="GetApplicationOutput,ReplicationStatus")`) and
//! the operation's documentation:
//!
//! * `ReplicationStatuses` is "a set of replication statuses for each location", each row
//!   a `ReplicationStatus` of `{Location, Status}` with `Status` one of `REPLICATING` or
//!   `COMPLETED`, and "an application cannot be streamed from a location until it has
//!   finished replicating there". So a location reads `COMPLETED` exactly when the
//!   content can be streamed from it: the application's own Region once the content has
//!   been ingested there ("PROCESSING: ... Amazon GameLift Streams is copying the content
//!   and caching for future deployment in a stream group", "READY: The application is
//!   ready to deploy in a stream group"), and a stream group's location once that
//!   location is usable for streaming ("ACTIVATING: ... You cannot stream from ... this
//!   location yet"). The application's own row belongs to the application, so no
//!   association creates it and no disassociation takes it away.
//! * `StorageBytes` reports the size in bytes of the content the application stores,
//!   which is the content at the Amazon S3 location `ApplicationSourceUri` names ("The
//!   original Amazon S3 location of uploaded stream content for the application"). The
//!   model declares the member on neither `GetApplicationOutput`,
//!   `CreateApplicationOutput` nor `ApplicationSummary`, and the API reports it on this
//!   read, so that is the one answer asserted to carry it.
//!
//! The tables below span those classes — how many objects the content holds, how they are
//! laid out under the prefix, how many locations a group has and which Regions they are,
//! and whether a location was configured at create or added later — rather than one
//! recorded case. Nothing here waits a fixed time for a transition: every wait polls for
//! the CONDITION it needs, bounded by one generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_gameliftstreams_gameliftstreams::EmulatedService;

use super::stream_workflow::{as_label, call, text, REGION};

/// How long any wait in this file gives a transition the clock owes before failing.
///
/// Generous on purpose: the only timing an assertion here depends on is that the
/// transition eventually falls due, so a loaded or slow host must not change any
/// outcome.
pub(crate) const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits before looking again.
pub(crate) const LOOK_AGAIN: Duration = Duration::from_millis(10);

/// The bucket every content layout below is staged in.
const BUCKET: &str = "gameliftstreams-content";

/// The objects the content bucket holds, as `(key, bytes)`.
///
/// Four distinct sizes, none of them a multiple of another, so no byte total can be an
/// object count in disguise; one empty object, because a zero-byte file is content too;
/// one nested two levels under its prefix; and one under a prefix NO case names, so a
/// total that ignores the prefix is visible.
const CONTENT: &[(&str, usize)] = &[
    ("single/game.exe", 17),
    ("many/game.exe", 5),
    ("many/data/pak0.bin", 11),
    ("many/data/pak1.bin", 300),
    ("zero/empty.dat", 0),
    ("other/decoy.bin", 1000),
];

/// The content location every suite that needs a staged, readable source URI creates
/// its applications from: a prefix [`stage_content`] put objects under.
pub(crate) fn staged_source_uri() -> String {
    format!("s3://{BUCKET}/many/")
}

/// This service and this emulator's own Amazon S3 on one shared engine, with
/// [`CONTENT`] already staged — so [`staged_source_uri`] is a location the store holds
/// content at, which is what `CreateApplication` requires of it.
pub(crate) async fn emulator_with_staged_content() -> MultiServiceRouter {
    let router = emulator_with_object_store();
    stage_content(&router).await;
    router
}

/// This service and this emulator's own Amazon S3 on one shared engine.
pub(crate) fn emulator_with_object_store() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Deterministic regardless of the process-wide authentication switch: these tests
    // exercise handlers, not SigV4.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    // The content an application is created from is a real object of this store.
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// An Amazon S3 client over the in-process seam, built the one way the tree builds it.
pub(crate) fn s3(router: &MultiServiceRouter) -> ::aws_sdk_s3::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    ::service_common::s3_client::s3_client(&callback, REGION)
}

/// Create [`BUCKET`] and put every object of [`CONTENT`] in it.
async fn stage_content(router: &MultiServiceRouter) {
    let created = s3(router)
        .create_bucket()
        .bucket(BUCKET)
        .create_bucket_configuration(
            ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(REGION))
                .build(),
        )
        .send()
        .await;
    assert!(
        created.is_ok(),
        "Amazon S3 must create the content bucket {BUCKET}, got {created:?}"
    );
    for (key, size) in CONTENT {
        let stored = s3(router)
            .put_object()
            .bucket(BUCKET)
            .key(*key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(vec![
                b'c';
                *size
            ]))
            .send()
            .await;
        assert!(
            stored.is_ok(),
            "Amazon S3 must store {key} in {BUCKET}, got {stored:?}"
        );
    }
}

/// How many bytes of [`CONTENT`] lie under `prefix` — the number the store itself holds
/// there, computed from the same table the objects were staged from.
fn staged_bytes_under(prefix: &str) -> i64 {
    CONTENT
        .iter()
        .filter(|(key, _)| key.starts_with(prefix))
        .map(|(_, size)| *size as i64)
        .sum()
}

/// `POST /applications` with `source_uri` as the content location, asserting the 201.
async fn create_application(
    router: &MultiServiceRouter,
    description: &str,
    source_uri: &str,
) -> (String, Value) {
    let (status, created) = call(
        router,
        "POST",
        "/applications",
        Some(json!({
            "Description": description,
            "RuntimeEnvironment": {"Type": "PROTON", "Version": "20250516"},
            "ExecutablePath": "game.exe",
            "ApplicationSourceUri": source_uri,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {description:?}: expected CreateApplication 201, got {status} {created}"
    );
    (text(&created, "/Id", description), created)
}

/// `GET /applications/<identifier>`, asserting the 200.
async fn get_application(router: &MultiServiceRouter, identifier: &str, case: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/applications/{}", as_label(identifier)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetApplication 200, got {status} {body}"
    );
    body
}

/// `GET /streamgroups/<identifier>`, asserting the 200.
async fn get_stream_group(router: &MultiServiceRouter, identifier: &str, case: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/streamgroups/{}", as_label(identifier)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetStreamGroup 200, got {status} {body}"
    );
    body
}

/// Look at `path` again and again until `settled` holds for the answered body, and
/// answer that body.
///
/// Waits for the CONDITION, never for a fixed time, so the outcome does not depend on
/// how fast the host runs.
pub(crate) async fn poll_until(
    router: &MultiServiceRouter,
    path: &str,
    case: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = call(router, "GET", path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from {path}, got {status} {body}"
        );
        if settled(&body) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {path} never settled within {SETTLE_DEADLINE:?}, last answer {body}"
        );
        std::thread::sleep(LOOK_AGAIN);
    }
}

/// The application `identifier` once the clock has made its content `READY`.
pub(crate) async fn application_in_ready_status(
    router: &MultiServiceRouter,
    identifier: &str,
    case: &str,
) -> Value {
    poll_until(
        router,
        &format!("/applications/{}", as_label(identifier)),
        case,
        |body| body["Status"] == json!("READY"),
    )
    .await
}

/// The stream group `identifier` once every location it holds is `ACTIVE` and the group
/// itself is `ACTIVE`.
pub(crate) async fn stream_group_fully_active(
    router: &MultiServiceRouter,
    identifier: &str,
    case: &str,
) -> Value {
    poll_until(
        router,
        &format!("/streamgroups/{}", as_label(identifier)),
        case,
        |body| {
            body["Status"] == json!("ACTIVE")
                && body["LocationStates"]
                    .as_array()
                    .is_some_and(|states| {
                        !states.is_empty()
                            && states.iter().all(|state| state["Status"] == json!("ACTIVE"))
                    })
        },
    )
    .await
}

/// The `{Location: Status}` pairs a body reports, as a sorted list of `(location,
/// status)`.
fn pairs(body: &Value, member: &str, location_key: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = body[member]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    (
                        row[location_key].as_str().unwrap_or_default().to_string(),
                        row["Status"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort();
    rows
}

/// The application's replication rows, as sorted `(location, status)` pairs.
fn replication_rows(application: &Value) -> Vec<(String, String)> {
    pairs(application, "ReplicationStatuses", "Location")
}

/// The group's location rows, as sorted `(location, status)` pairs.
fn location_rows(group: &Value) -> Vec<(String, String)> {
    pairs(group, "LocationStates", "LocationName")
}

/// Every location of `locations`, plus the application's own Region, reported
/// `COMPLETED`, in the order the read answers them.
fn all_completed(locations: &[&str]) -> Vec<(String, String)> {
    let mut expected: Vec<(String, String)> = locations
        .iter()
        .chain(std::iter::once(&REGION))
        .map(|location| ((*location).to_string(), "COMPLETED".to_string()))
        .collect();
    expected.sort();
    expected.dedup();
    expected
}

/// The two halves of the rule, each checked between two reads whose ORDER makes the
/// check free of any race: a status only ever moves towards usable, so
///
/// * a location the group reported `ACTIVE` must be `COMPLETED` in the application read
///   that follows it, and
/// * a location the application reported `COMPLETED` must be `ACTIVE` in the group read
///   that follows it.
///
/// The application's own Region is exempt from the second half: its row is the ingested
/// content, which is replicated there whatever a group is doing with that Region.
async fn assert_replication_agrees_with_the_group(
    router: &MultiServiceRouter,
    application: &str,
    group: &str,
    case: &str,
) {
    let group_first = get_stream_group(router, group, case).await;
    let application_after = get_application(router, application, case).await;
    let rows = replication_rows(&application_after);
    for (location, status) in location_rows(&group_first) {
        if status != "ACTIVE" {
            continue;
        }
        let reported = rows
            .iter()
            .find(|(name, _)| *name == location)
            .map(|(_, status)| status.clone());
        assert_eq!(
            reported.as_deref(),
            Some("COMPLETED"),
            "case {case:?}: the group can stream from {location}, so replication there has \
             finished: expected COMPLETED, got {reported:?} in {application_after}"
        );
    }

    let application_first = get_application(router, application, case).await;
    let group_after = get_stream_group(router, group, case).await;
    let locations = location_rows(&group_after);
    for (location, status) in replication_rows(&application_first) {
        if status != "COMPLETED" || location == REGION {
            continue;
        }
        let reported = locations
            .iter()
            .find(|(name, _)| *name == location)
            .map(|(_, status)| status.clone());
        assert_eq!(
            reported.as_deref(),
            Some("ACTIVE"),
            "case {case:?}: replication to {location} reads finished, so the group must be \
             able to stream from it: expected ACTIVE, got {reported:?} in {group_after}"
        );
    }
}

/// Whatever the content at `ApplicationSourceUri` is, the application reports its size
/// in bytes as `StorageBytes` — and reporting it does not change it.
#[tokio::test]
async fn storage_bytes_reports_the_byte_size_of_the_stored_content() {
    let router = emulator_with_object_store();
    stage_content(&router).await;

    // The rows differ along what the rule generalises over: how many objects the content
    // holds, how deep under the prefix they lie, whether one of them is empty, whether
    // the URI names a prefix, a whole bucket or one object directly, and whether it
    // carries its trailing separator. A location the store holds nothing at is no
    // application at all (`content_preconditions`), so every row here is one the store
    // holds content at.
    let rows: Vec<(&str, String, i64)> = vec![
        (
            "one object under the prefix",
            format!("s3://{BUCKET}/single/"),
            staged_bytes_under("single/"),
        ),
        (
            "several objects, one nested deeper",
            format!("s3://{BUCKET}/many/"),
            staged_bytes_under("many/"),
        ),
        (
            "a single empty object",
            format!("s3://{BUCKET}/zero/"),
            staged_bytes_under("zero/"),
        ),
        (
            "every object of the bucket",
            format!("s3://{BUCKET}/"),
            staged_bytes_under(""),
        ),
        (
            "a prefix submitted without its trailing separator",
            format!("s3://{BUCKET}/many"),
            staged_bytes_under("many/"),
        ),
        (
            "one object named directly",
            format!("s3://{BUCKET}/single/game.exe"),
            staged_bytes_under("single/game.exe"),
        ),
    ];

    for (index, (case, uri, expected)) in rows.iter().enumerate() {
        // `Description` is modelled `pattern ^[a-zA-Z0-9-_.!+@/][a-zA-Z0-9-_.!+@/ ]*$`,
        // so the row's index names the application rather than the prose label.
        let (id, created) =
            create_application(&router, &format!("storage-case-{index}"), uri).await;
        // The model declares no `StorageBytes` on `CreateApplicationOutput`, and this API
        // reports it on the read: the create answers without it.
        assert!(
            created.get("StorageBytes").is_none(),
            "case {case:?}: CreateApplication reports no StorageBytes, got {created}"
        );

        let ready = application_in_ready_status(&router, &id, case).await;
        assert_eq!(
            ready["StorageBytes"],
            json!(expected),
            "case {case:?}: expected StorageBytes {expected}, got {ready}"
        );
        // A read reports; it does not change what a later caller sees.
        let again = get_application(&router, &id, case).await;
        assert_eq!(
            again["StorageBytes"],
            json!(expected),
            "case {case:?}: a second read must report the same StorageBytes {expected}, got \
             {again}"
        );
    }
}

/// Content staged AFTER the create does not change what the application stores: the size
/// is the size of what was copied, read once when the content was ingested.
#[tokio::test]
async fn storage_bytes_is_the_size_of_the_content_the_create_copied() {
    let router = emulator_with_object_store();
    stage_content(&router).await;
    let uri = format!("s3://{BUCKET}/single/");
    let (id, _) = create_application(&router, "storage-at-create", &uri).await;
    let ready = application_in_ready_status(&router, &id, "at the create").await;
    let ingested = staged_bytes_under("single/");
    assert_eq!(
        ready["StorageBytes"],
        json!(ingested),
        "expected StorageBytes {ingested}, got {ready}"
    );

    let stored = s3(&router)
        .put_object()
        .bucket(BUCKET)
        .key("single/added-later.bin")
        .body(::aws_sdk_s3::primitives::ByteStream::from(vec![b'c'; 64]))
        .send()
        .await;
    assert!(
        stored.is_ok(),
        "Amazon S3 must store the later object, got {stored:?}"
    );
    let after = get_application(&router, &id, "after a later upload").await;
    assert_eq!(
        after["StorageBytes"],
        json!(ingested),
        "an object added after the create is not content this application copied: expected \
         StorageBytes {ingested}, got {after}"
    );
}

/// An application's own Region is replicated by the create itself: the row appears once
/// the content is in place and belongs to the application, not to any stream group.
#[tokio::test]
async fn an_application_reports_its_own_region_once_its_content_is_in_place() {
    let router = emulator_with_object_store();
    stage_content(&router).await;
    let uri = format!("s3://{BUCKET}/many/");
    let (id, created) = create_application(&router, "home-region-row", &uri).await;

    // Nothing is replicated anywhere at the instant the create is answered: the content
    // has not been copied yet.
    assert_eq!(
        created["ReplicationStatuses"],
        json!([]),
        "the create answers before the content is copied, got {created}"
    );

    // Read in ONE answer, so the check does not depend on how fast the host runs: an
    // application whose content is not stored yet reports no row, and one whose content
    // is stored reports its own Region as replicated.
    let fresh = get_application(&router, &id, "while the content is being copied").await;
    let expected = if fresh["Status"] == json!("READY") {
        all_completed(&[])
    } else {
        Vec::new()
    };
    assert_eq!(
        replication_rows(&fresh),
        expected,
        "case \"one answer\": an application in status {} must report {expected:?}, got {fresh}",
        fresh["Status"]
    );

    let ready = application_in_ready_status(&router, &id, "the ingested content").await;
    assert_eq!(
        replication_rows(&ready),
        all_completed(&[]),
        "a READY application reports its own Region as replicated, got {ready}"
    );
}

/// Whatever locations a stream group holds, the application it is associated with
/// reports each of them, `COMPLETED` exactly once the group can stream from it — beside
/// its own Region, which no association creates and no disassociation takes away.
#[tokio::test]
async fn replication_rows_follow_the_locations_the_content_can_stream_from() {
    let router = emulator_with_object_store();
    stage_content(&router).await;
    let uri = format!("s3://{BUCKET}/many/");

    // The rows differ along what the rule generalises over: how many locations the group
    // holds and whether the application's own Region is among them.
    let sets: &[(&str, &[&str])] = &[
        ("the application's own Region alone", &[REGION]),
        ("one Region other than the application's", &["us-east-1"]),
        (
            "two Regions, one of them the application's",
            &[REGION, "us-east-1"],
        ),
        (
            "three Regions, none of them the application's",
            &["us-east-1", "eu-central-1", "ap-south-1"],
        ),
    ];

    for (index, (case, locations)) in sets.iter().enumerate() {
        let (application_id, _) =
            create_application(&router, &format!("replication-case-{index}"), &uri).await;
        application_in_ready_status(&router, &application_id, case).await;

        let configurations: Vec<Value> = locations
            .iter()
            .map(|location| json!({"LocationName": location, "AlwaysOnCapacity": 1}))
            .collect();
        let (status, group) = call(
            &router,
            "POST",
            "/streamgroups",
            Some(json!({
                "Description": format!("replication-group-{index}"),
                "StreamClass": "gen5n_high",
                "DefaultApplicationIdentifier": application_id.clone(),
                "LocationConfigurations": configurations,
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected CreateStreamGroup 201, got {status} {group}"
        );
        let group_id = text(&group, "/Id", case);

        // While the group's locations are still being prepared, the two halves of the
        // rule hold between ordered reads.
        assert_replication_agrees_with_the_group(&router, &application_id, &group_id, case).await;

        stream_group_fully_active(&router, &group_id, case).await;
        let settled = get_application(&router, &application_id, case).await;
        let expected = all_completed(locations);
        assert_eq!(
            replication_rows(&settled),
            expected,
            "case {case:?}: expected {expected:?}, got {settled}"
        );

        // A location added to a group that is already serving replicates the same way:
        // the row appears at once and reads finished when that location becomes usable.
        let added = "eu-west-1";
        let (status, body) = call(
            &router,
            "POST",
            &format!("/streamgroups/{group_id}/locations"),
            Some(json!({
                "LocationConfigurations": [{"LocationName": added, "AlwaysOnCapacity": 1}],
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AddStreamGroupLocations 200, got {status} {body}"
        );
        assert_replication_agrees_with_the_group(&router, &application_id, &group_id, case).await;
        stream_group_fully_active(&router, &group_id, case).await;
        let grown = get_application(&router, &application_id, case).await;
        let mut with_added: Vec<&str> = locations.to_vec();
        with_added.push(added);
        let expected = all_completed(&with_added);
        assert_eq!(
            replication_rows(&grown),
            expected,
            "case {case:?}: a location added later is replicated too: expected {expected:?}, \
             got {grown}"
        );

        // Unlinking the application takes the group's locations away with the link, and
        // leaves the application's own Region: that row is the ingested content.
        let (status, body) = call(
            &router,
            "POST",
            &format!("/streamgroups/{group_id}/disassociations"),
            Some(json!({"ApplicationIdentifiers": [application_id.clone()]})),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DisassociateApplications 200, got {status} {body}"
        );
        let unlinked = get_application(&router, &application_id, case).await;
        assert_eq!(
            replication_rows(&unlinked),
            all_completed(&[]),
            "case {case:?}: an unlinked application still reports its own Region, got \
             {unlinked}"
        );
    }
}
