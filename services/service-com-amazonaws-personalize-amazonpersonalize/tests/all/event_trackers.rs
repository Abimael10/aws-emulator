// SPDX-License-Identifier: CC0-1.0

//! Event trackers, end to end over the wire: the resource `CreateEventTracker` persists,
//! what `ListEventTrackers` and `DescribeEventTracker` report of it, and the `PutEvents`
//! ingestion its tracking id addresses.
//!
//! RULE: an event tracker is a STORED resource of a dataset group — one per group,
//! addressed by an ARN minted from its name and carrying a tracking id minted with it —
//! and `ListEventTrackers` REPORTS what the account holds: HTTP 200 with the summary of
//! every tracker (narrowed to one group when `datasetGroupArn` is given), writing nothing,
//! never refusing because the account holds none, `maxResults` bounding one page, the
//! `nextToken` a page hands back resuming after the item that page ended on, the last page
//! carrying no token, and a token the listing did not itself issue answered with
//! `InvalidNextTokenException`. Every member a read reports comes from the record the
//! create kept, so a created tracker is listed, described and ingested through, and a
//! deleted one is none of those.
//!
//! The rows vary what the rule generalises over: how many trackers exist (none through
//! several), the shape of each tracker's name (its length bounds and every character class
//! the model's pattern permits), the page size (absent, each published bound and sizes
//! between), whether the listing is filtered and by which group (one holding a tracker, one
//! holding none, one that does not exist), and the form of the `nextToken` offered.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dataset_group_lifecycle::{
    create_active_group, error_code, error_message, list_all, Emulator, ACCOUNT, REGION,
};

/// The statuses a tracker reports on the way to being created. Source: the model's own
/// documentation for `CreateEventTracker` — "CREATE PENDING > CREATE IN_PROGRESS > ACTIVE
/// -or- CREATE FAILED".
const CREATE_STATUSES: &[&str] = &["CREATE PENDING", "CREATE IN_PROGRESS", "ACTIVE"];

/// How long a case waits for a create to complete before calling it stuck. Far above the
/// emulator's own create span, so a slow machine does not fail the suite.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(20);

/// The longest `name` the model allows: `Name` is `len 1..63`
/// (`smithy_query(query="operation", operation="CreateEventTracker")`).
const LONGEST_NAME: usize = 63;

/// The tracker names the catalog is built from: a plain one, one using every character
/// class the model's `^[a-zA-Z0-9][a-zA-Z0-9\-_]*$` pattern permits, and each bound of its
/// `len 1..63`.
fn tracker_names() -> Vec<(&'static str, String)> {
    vec![
        ("a plain name", "clickTracker".to_string()),
        (
            "dashes, digits and underscores",
            "Tracker-9_again".to_string(),
        ),
        ("the shortest name", "t".to_string()),
        ("the longest name", "t".repeat(LONGEST_NAME)),
    ]
}

/// The ARN this service mints for the event tracker `name`, as
/// `src/state/trackers.rs::event_tracker_arn` builds it — so a row can predict the key the
/// listing orders by.
fn expected_tracker_arn(name: &str) -> String {
    format!("arn:aws:personalize:{REGION}:{ACCOUNT}:event-tracker/{name}")
}

/// Create an event tracker in `group_arn` and return its ARN and tracking id.
async fn create_tracker(
    emulator: &Emulator,
    group_arn: &str,
    name: &str,
    case: &str,
) -> (String, String) {
    let (status, body) = emulator
        .call(
            "CreateEventTracker",
            json!({"name": name, "datasetGroupArn": group_arn}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateEventTracker to answer 200, got {status} {body:?}"
    );
    let arn = body["eventTrackerArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an eventTrackerArn, got {body:?}"))
        .to_string();
    let tracking_id = body["trackingId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a trackingId, got {body:?}"))
        .to_string();
    assert_eq!(
        arn,
        expected_tracker_arn(name),
        "case {case:?}: expected the ARN this service mints for the name"
    );
    assert!(
        !tracking_id.is_empty() && tracking_id.len() <= 256,
        "case {case:?}: expected a tracking id inside the modelled length, got \
         {tracking_id:?}"
    );
    (arn, tracking_id)
}

/// The `eventTrackerArn` of each listed summary.
fn listed_arns(listed: &[Value]) -> Vec<String> {
    listed
        .iter()
        .map(|summary| {
            summary["eventTrackerArn"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Assert one summary reports what the record holds, and nothing a summary does not
/// carry.
fn assert_summary(case: &str, summary: &Value, names: &[String]) {
    let name = summary["name"].as_str().unwrap_or_default().to_string();
    assert!(
        names.contains(&name),
        "case {case:?}: unexpected tracker {summary:?}"
    );
    assert_eq!(
        summary["eventTrackerArn"].as_str(),
        Some(expected_tracker_arn(&name).as_str()),
        "case {case:?}: expected {name:?}'s own ARN, got {summary:?}"
    );
    let status = summary["status"].as_str().unwrap_or_default();
    assert!(
        CREATE_STATUSES.contains(&status),
        "case {case:?}: expected one of {CREATE_STATUSES:?} for {name:?}, got {summary:?}"
    );
    assert!(
        summary["creationDateTime"].is_number() && summary["lastUpdatedDateTime"].is_number(),
        "case {case:?}: expected {name:?} to carry both timestamps, got {summary:?}"
    );
    // "Provides a SUMMARY of the properties of an event tracker. For a complete listing,
    // call the DescribeEventTracker API": the members that address the tracker's ingestion
    // are the describe's answer, and `EventTrackerSummary` declares none of them.
    for absent in ["trackingId", "datasetGroupArn", "accountId"] {
        assert!(
            summary[absent].is_null(),
            "case {case:?}: expected {name:?}'s summary to carry no {absent}, got {summary:?}"
        );
    }
}

#[tokio::test]
async fn list_event_trackers_reports_every_tracker_at_every_page_size() {
    let emulator = Emulator::new();

    // An account holding no tracker at all: a 200 with an empty list, never an error —
    // which is the step every blocked recording made first.
    for max_results in [None, Some(1), Some(100)] {
        let case = format!("an empty account at maxResults {max_results:?}");
        let listed = list_all(&emulator, "ListEventTrackers", "eventTrackers", None, max_results)
            .await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no event tracker, got {listed:?}"
        );
    }

    // One tracker per group, because only one may be associated with a group, plus a group
    // that holds none so the filter has something to exclude.
    let names = tracker_names();
    let mut groups: Vec<(String, String)> = Vec::new();
    for (index, (case, name)) in names.iter().enumerate() {
        let group = create_active_group(&emulator, &format!("et-dsg-{index}"), case).await;
        let (arn, _) = create_tracker(&emulator, &group, name, case).await;
        groups.push((group, arn));
    }
    let empty_group = create_active_group(&emulator, "et-dsg-empty", "a group with no tracker").await;

    let mut expected: Vec<String> = names
        .iter()
        .map(|(_, name)| expected_tracker_arn(name))
        .collect();
    expected.sort();
    let created_names: Vec<String> = names.iter().map(|(_, name)| name.clone()).collect();

    // `maxResults` is modelled 1..100: every size inside the range reports exactly the
    // catalog, as does an absent `maxResults`, and both published bounds are accepted.
    for max_results in [None, Some(1), Some(2), Some(3), Some(100)] {
        let case = format!("maxResults {max_results:?}");
        let listed = list_all(&emulator, "ListEventTrackers", "eventTrackers", None, max_results)
            .await;
        assert_eq!(
            listed_arns(&listed),
            expected,
            "case {case:?}: expected every created tracker exactly once, in ARN order"
        );
        for summary in &listed {
            assert_summary(&case, summary, &created_names);
        }
    }

    // The filter narrows the listing to ONE group's tracker, whatever page size pages it.
    for (group, arn) in &groups {
        for max_results in [None, Some(1), Some(100)] {
            let case = format!("filtered by {group} at maxResults {max_results:?}");
            let listed = list_all(
                &emulator,
                "ListEventTrackers",
                "eventTrackers",
                Some(("datasetGroupArn", group)),
                max_results,
            )
            .await;
            assert_eq!(
                listed_arns(&listed),
                vec![arn.clone()],
                "case {case:?}: expected only that group's tracker"
            );
        }
    }

    // A group that holds no tracker, and a group ARN this account holds nothing for: a
    // filter selects, so both report an empty page rather than a refusal.
    for (case, group) in [
        ("a group holding no tracker", empty_group),
        (
            "a group that does not exist",
            format!("arn:aws:personalize:{REGION}:{ACCOUNT}:dataset-group/absent-et"),
        ),
    ] {
        let listed = list_all(
            &emulator,
            "ListEventTrackers",
            "eventTrackers",
            Some(("datasetGroupArn", &group)),
            None,
        )
        .await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected an empty listing, got {listed:?}"
        );
    }

    // A listing REPORTS: two listings a call apart agree.
    let again = list_all(&emulator, "ListEventTrackers", "eventTrackers", None, None).await;
    assert_eq!(
        listed_arns(&again),
        expected,
        "case \"a second listing\": expected a read to change nothing"
    );
}

#[tokio::test]
async fn list_event_trackers_refuses_a_page_size_outside_the_modelled_range() {
    let emulator = Emulator::new();

    // The modelled range is 1..100, so each value just outside it — and a negative one —
    // is refused before the handler runs.
    for max_results in [0, 101, -1] {
        let case = format!("maxResults {max_results}");
        let (status, body) = emulator
            .call("ListEventTrackers", json!({"maxResults": max_results}))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a maxResults outside 1..100, got {status} \
             {body:?}"
        );
    }
}

#[tokio::test]
async fn list_event_trackers_resumes_only_from_a_token_it_issued() {
    let emulator = Emulator::new();

    let names = tracker_names();
    for (index, (case, name)) in names.iter().enumerate() {
        let group = create_active_group(&emulator, &format!("et-token-dsg-{index}"), case).await;
        create_tracker(&emulator, &group, name, case).await;
    }
    let mut expected: Vec<String> = names
        .iter()
        .map(|(_, name)| expected_tracker_arn(name))
        .collect();
    expected.sort();

    // A real page of this listing, and a real page of a SIBLING listing: the first is the
    // only thing `ListEventTrackers` resumes from.
    let (status, body) = emulator
        .call("ListEventTrackers", json!({"maxResults": 1}))
        .await;
    assert_eq!(
        status, 200,
        "case \"a real page\": expected HTTP 200, got {status} {body:?}"
    );
    let own_token = body["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"a real page\": expected a nextToken, got {body:?}"))
        .to_string();

    let (status, body) = emulator
        .call("ListDatasetGroups", json!({"maxResults": 1}))
        .await;
    assert_eq!(
        status, 200,
        "case \"a sibling listing's page\": expected HTTP 200, got {status} {body:?}"
    );
    let sibling_token = body["nextToken"].as_str().map(ToString::to_string);

    let mut refused: Vec<(String, String)> = vec![
        ("an arbitrary string".to_string(), "bogus".to_string()),
        ("an ARN pasted in raw".to_string(), expected[0].clone()),
        (
            "hex that decodes to no tagged key".to_string(),
            "6e6f742d612d746f6b656e".to_string(),
        ),
        ("the empty string".to_string(), String::new()),
    ];
    if let Some(sibling_token) = sibling_token {
        refused.push(("another listing's token".to_string(), sibling_token));
    }

    for (case, token) in refused {
        let (status, body) = emulator
            .call("ListEventTrackers", json!({"nextToken": token}))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body:?}"
        );
        assert!(
            error_code(&body).contains("InvalidNextTokenException"),
            "case {case:?}: expected InvalidNextTokenException, got {body:?}"
        );
    }

    // The token this listing issued resumes it, after the item its page ended on.
    let (status, body) = emulator
        .call("ListEventTrackers", json!({"nextToken": own_token}))
        .await;
    assert_eq!(
        status, 200,
        "case \"its own token\": expected HTTP 200, got {status} {body:?}"
    );
    let resumed = listed_arns(body["eventTrackers"].as_array().unwrap_or_else(|| {
        panic!("case \"its own token\": expected an eventTrackers list, got {body:?}")
    }));
    assert_eq!(
        resumed,
        expected[1..].to_vec(),
        "case \"its own token\": expected the listing to resume after the first tracker"
    );
}

#[tokio::test]
async fn a_create_is_refused_when_its_group_is_unknown_or_already_tracked() {
    let emulator = Emulator::new();

    let first = create_active_group(&emulator, "et-refusal-1", "refusals").await;
    let second = create_active_group(&emulator, "et-refusal-2", "refusals").await;
    let (tracked_arn, _) = create_tracker(&emulator, &first, "refusalTracker", "refusals").await;

    // A group this account holds nothing for: the not-found class, worded for the KIND of
    // resource the lookup failed on.
    let absent = format!("arn:aws:personalize:{REGION}:{ACCOUNT}:dataset-group/absent-refusal");
    let (status, body) = emulator
        .call(
            "CreateEventTracker",
            json!({"name": "orphanTracker", "datasetGroupArn": absent}),
        )
        .await;
    assert_eq!(
        status, 400,
        "case \"a group that does not exist\": expected HTTP 400, got {status} {body:?}"
    );
    assert!(
        error_code(&body).contains("ResourceNotFoundException"),
        "case \"a group that does not exist\": expected ResourceNotFoundException, got {body:?}"
    );
    assert_eq!(
        error_message(&body),
        format!("The given dataset group does not exist: {absent}"),
        "case \"a group that does not exist\": expected the dataset-group not-found wording"
    );

    // "Only one event tracker can be associated with a dataset group": a second create
    // against the tracked group collides under ANY name, and the same name in another
    // group collides on the ARN it would mint. Both carry the ARN of the tracker already
    // there.
    for (case, body, colliding) in [
        (
            "a second tracker in a group that already has one",
            json!({"name": "aDifferentName", "datasetGroupArn": first}),
            tracked_arn.clone(),
        ),
        (
            "the same tracker name in another group",
            json!({"name": "refusalTracker", "datasetGroupArn": second}),
            tracked_arn.clone(),
        ),
    ] {
        let (status, answered) = emulator.call("CreateEventTracker", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {answered:?}"
        );
        assert!(
            error_code(&answered).contains("ResourceAlreadyExistsException"),
            "case {case:?}: expected ResourceAlreadyExistsException, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            format!("Another resource with Arn {colliding} already exists."),
            "case {case:?}: expected the colliding resource's ARN in the refusal"
        );
    }

    // Every refusal left the account exactly as it was: the one tracker is still the only
    // one listed.
    let listed = list_all(&emulator, "ListEventTrackers", "eventTrackers", None, None).await;
    assert_eq!(
        listed_arns(&listed),
        vec![tracked_arn],
        "case \"after the refusals\": expected the stored tracker and nothing else"
    );
}

/// One emulator mounting the Personalize control plane and the Personalize Events
/// ingestion plane on ONE shared engine — which is what the assembled router does. A
/// tracking id is minted by `CreateEventTracker` here and used by `PutEvents` there, so
/// both have to be up for the round trip to mean anything.
///
/// Its own harness rather than `dataset_export.rs`'s `Exporter`: this suite has to observe
/// a put that is REFUSED (an unregistered and then a withdrawn tracking id), which that
/// one's `ingest` asserts away, and it needs no object storage mounted.
struct Ingestion {
    router: MultiServiceRouter,
}

impl Ingestion {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are verified by the emulator, never by a service; this suite is about
        // behaviour, so it signs with a fixed scope and verifies nothing.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_personalize_amazonpersonalize::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            ::service_com_amazonaws_personalizeevents_amazonpersonalizeevents::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// The SigV4 credential scope every request carries: the signing name is
    /// `personalize` for both services (`smithy_query(query="service_info")`).
    fn authorization() -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/personalize/aws4_request, \
             SignedHeaders=host, Signature=0"
        )
    }

    /// One control-plane call (`AmazonPersonalize.<Operation>` on `POST /`).
    async fn control(&self, operation: &str, body: Value) -> (u16, Value) {
        let host = format!("personalize.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("AmazonPersonalize.{operation}"))
            .header("authorization", Self::authorization())
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One `PutEvents` on the ingestion plane (`POST /events`).
    async fn put_events(&self, tracking_id: &str, event_type: &str) -> (u16, Value) {
        let host = format!("personalize-events.{REGION}.amazonaws.com");
        let body = json!({
            "trackingId": tracking_id,
            "sessionId": "session-1",
            "userId": "user-1",
            "eventList": [{"eventType": event_type, "sentAt": 1_764_000_000.0}],
        });
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/events"))
            .header("host", host)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization())
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Poll `DescribeEventTracker` until the tracker reports `ACTIVE`, and return it.
    ///
    /// A freshly created tracker is NOT ready — "CREATE PENDING > CREATE IN_PROGRESS >
    /// ACTIVE" — so every status seen on the way has to be one of the documented ones. The
    /// wait is for the CONDITION, bounded by a deadline far above the emulator's own span.
    async fn await_active(&self, arn: &str, case: &str) -> Value {
        let deadline = Instant::now() + ACTIVE_DEADLINE;
        loop {
            let (status, body) = self
                .control("DescribeEventTracker", json!({"eventTrackerArn": arn}))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DescribeEventTracker to answer 200, got {status} \
                 {body:?}"
            );
            let tracker = body["eventTracker"].clone();
            let reported = tracker["status"].as_str().unwrap_or_default().to_string();
            assert!(
                CREATE_STATUSES.contains(&reported.as_str()),
                "case {case:?}: expected one of {CREATE_STATUSES:?}, got {reported:?} in {body:?}"
            );
            if reported == "ACTIVE" {
                return tracker;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: DescribeEventTracker never reported ACTIVE, last status \
                 {reported:?}"
            );
            ::tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test]
async fn a_created_tracker_describes_back_and_its_tracking_id_ingests_until_it_is_deleted() {
    let emulator = Ingestion::new();

    let (status, body) = emulator
        .control("CreateDatasetGroup", json!({"name": "et-ingest-dsg"}))
        .await;
    assert_eq!(
        status, 200,
        "case \"setup\": expected CreateDatasetGroup to answer 200, got {status} {body:?}"
    );
    let group = body["datasetGroupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"setup\": expected a datasetGroupArn, got {body:?}"))
        .to_string();

    let (status, body) = emulator
        .control(
            "CreateEventTracker",
            json!({"name": "ingestTracker", "datasetGroupArn": group}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the create\": expected CreateEventTracker to answer 200, got {status} {body:?}"
    );
    let arn = body["eventTrackerArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"the create\": expected an eventTrackerArn, got {body:?}"))
        .to_string();
    let tracking_id = body["trackingId"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"the create\": expected a trackingId, got {body:?}"))
        .to_string();

    // The describe serves the complete listing of what the create kept, and the id it
    // reports is the one the create answered with — not a second value.
    let tracker = emulator.await_active(&arn, "the created tracker").await;
    for (member, expected) in [
        ("name", "ingestTracker".to_string()),
        ("eventTrackerArn", arn.clone()),
        ("datasetGroupArn", group.clone()),
        ("trackingId", tracking_id.clone()),
        ("accountId", ACCOUNT.to_string()),
    ] {
        assert_eq!(
            tracker[member].as_str(),
            Some(expected.as_str()),
            "case \"the created tracker\": expected {member} {expected:?}, got {tracker:?}"
        );
    }
    assert!(
        tracker["creationDateTime"].is_number() && tracker["lastUpdatedDateTime"].is_number(),
        "case \"the created tracker\": expected both timestamps, got {tracker:?}"
    );

    // The tracking id the create minted is one the INGESTION plane resolves, which is the
    // whole point of returning it; an id nothing registered is still refused, so the 200
    // belongs to this tracker and not to `PutEvents` accepting anything.
    let (status, body) = emulator.put_events(&tracking_id, "click").await;
    assert_eq!(
        status, 200,
        "case \"PutEvents through the created tracker\": expected HTTP 200, got {status} {body:?}"
    );
    let unknown = format!("{tracking_id}-not-registered");
    let (status, body) = emulator.put_events(&unknown, "click").await;
    assert_eq!(
        status, 400,
        "case \"PutEvents through an unregistered id\": expected HTTP 400, got {status} {body:?}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case \"PutEvents through an unregistered id\": expected InvalidInputException, got \
         {body:?}"
    );

    // The listing reports the stored tracker, by its ARN.
    let (status, body) = emulator.control("ListEventTrackers", json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the listing\": expected HTTP 200, got {status} {body:?}"
    );
    assert_eq!(
        listed_arns(body["eventTrackers"].as_array().unwrap_or(&Vec::new())),
        vec![arn.clone()],
        "case \"the listing\": expected the stored tracker"
    );

    // The delete removes the record and withdraws the tracking id with it.
    let (status, body) = emulator
        .control("DeleteEventTracker", json!({"eventTrackerArn": arn}))
        .await;
    assert_eq!(
        status, 200,
        "case \"the delete\": expected HTTP 200, got {status} {body:?}"
    );

    for (case, operation) in [
        ("the describe after the delete", "DescribeEventTracker"),
        ("a repeated delete", "DeleteEventTracker"),
    ] {
        let (status, body) = emulator
            .control(operation, json!({"eventTrackerArn": arn}))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body:?}"
        );
        assert!(
            error_code(&body).contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            error_message(&body),
            format!("Resource Arn {arn} does not exist."),
            "case {case:?}: expected the Arn-generic not-found wording"
        );
    }

    let (status, body) = emulator.control("ListEventTrackers", json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the listing after the delete\": expected HTTP 200, got {status} {body:?}"
    );
    assert!(
        body["eventTrackers"]
            .as_array()
            .is_some_and(|listed| listed.is_empty()),
        "case \"the listing after the delete\": expected an empty listing, got {body:?}"
    );

    let (status, body) = emulator.put_events(&tracking_id, "click").await;
    assert_eq!(
        status, 400,
        "case \"PutEvents after the delete\": expected HTTP 400, got {status} {body:?}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case \"PutEvents after the delete\": expected InvalidInputException, got {body:?}"
    );
}
