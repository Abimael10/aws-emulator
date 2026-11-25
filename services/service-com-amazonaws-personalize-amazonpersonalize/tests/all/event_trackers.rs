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
    avro_schema, create_active_group, create_dataset, create_schema, error_code, error_message,
    list_all, Emulator, ACCOUNT, REGION,
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

/// The dataset type an event tracker's events are appended to: "Amazon Personalize then
/// appends the event data to the Item interactions dataset of the dataset group you specify
/// in your event tracker" (`smithy_query(query="doc", shape="CreateEventTracker")`).
const INTERACTIONS: &str = "Interactions";

/// Create a dataset group that already holds the Item interactions dataset an event tracker
/// appends its events to, and return the group's ARN.
///
/// A group that holds no such dataset takes no tracker at all, which is the rule
/// `a_create_is_refused_until_its_group_holds_an_interactions_dataset` states, so every
/// group that is to carry a tracker is built this way.
async fn create_trackable_group(emulator: &Emulator, name: &str, case: &str) -> String {
    let group = create_active_group(emulator, name, case).await;
    let schema = create_schema(
        emulator,
        &format!("{name}-interactions"),
        &avro_schema(INTERACTIONS),
        case,
    )
    .await;
    create_dataset(emulator, &group, &schema, INTERACTIONS, case).await;
    group
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
        let group = create_trackable_group(&emulator, &format!("et-dsg-{index}"), case).await;
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
        let group =
            create_trackable_group(&emulator, &format!("et-token-dsg-{index}"), case).await;
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

    let first = create_trackable_group(&emulator, "et-refusal-1", "refusals").await;
    let second = create_trackable_group(&emulator, "et-refusal-2", "refusals").await;
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

/// One row of the interactions-precondition table: what the dataset group holds when the
/// create is attempted, and whether a tracker may be created in it.
struct PreconditionRow {
    case: &'static str,
    /// The `datasetType` of each dataset the group holds, spelled as the request sends it.
    /// The values come from the documented set for `CreateDatasetRequest$datasetType` —
    /// "Interactions, Items, Users, Actions, Action_Interactions", case insensitive
    /// (`smithy_query(query="doc", shape="CreateDatasetRequest", member="datasetType")`).
    holds: &'static [&'static str],
    /// Whether a group holding exactly that takes an event tracker.
    takes_a_tracker: bool,
}

/// What a group may hold, and whether an event tracker may be created in it. The rows vary
/// what the rule generalises over: how many datasets the group holds (none through several),
/// which types they are (every documented type that is NOT Item interactions, including the
/// separate Action interactions type), the position of the Item interactions dataset among
/// them, and the casing its `datasetType` was created with.
const PRECONDITION_CATALOG: &[PreconditionRow] = &[
    // The recorded case: a group that exists and holds nothing at all.
    PreconditionRow {
        case: "a group holding no dataset at all",
        holds: &[],
        takes_a_tracker: false,
    },
    PreconditionRow {
        case: "a group holding an Items dataset only",
        holds: &["Items"],
        takes_a_tracker: false,
    },
    PreconditionRow {
        case: "a group holding Users and Actions datasets",
        holds: &["Users", "Actions"],
        takes_a_tracker: false,
    },
    // Action interactions is its own dataset type: it is not the Item interactions dataset
    // an event tracker's event data is appended to.
    PreconditionRow {
        case: "a group holding an Action_Interactions dataset only",
        holds: &["Action_Interactions"],
        takes_a_tracker: false,
    },
    PreconditionRow {
        case: "a group holding the Item interactions dataset",
        holds: &["Interactions"],
        takes_a_tracker: true,
    },
    PreconditionRow {
        case: "a group holding the Item interactions dataset among others",
        holds: &["Items", "Interactions", "Users"],
        takes_a_tracker: true,
    },
    // The type is matched the way `CreateDataset` accepts it — case insensitively — so the
    // casing the dataset was created with is not part of the precondition.
    PreconditionRow {
        case: "a group whose interactions dataset was created in lower case",
        holds: &["interactions"],
        takes_a_tracker: true,
    },
];

#[tokio::test]
async fn a_create_is_refused_until_its_group_holds_an_interactions_dataset() {
    let emulator = Emulator::new();

    for (index, row) in PRECONDITION_CATALOG.iter().enumerate() {
        let case = row.case;
        let group = create_active_group(&emulator, &format!("precondition-{index}"), case).await;
        for dataset_type in row.holds {
            let schema = create_schema(
                &emulator,
                &format!("precondition-{index}-{dataset_type}"),
                &avro_schema(dataset_type),
                case,
            )
            .await;
            create_dataset(&emulator, &group, &schema, dataset_type, case).await;
        }

        let name = format!("preconditionTracker{index}");
        let (status, answered) = emulator
            .call(
                "CreateEventTracker",
                json!({"name": name, "datasetGroupArn": group}),
            )
            .await;

        if row.takes_a_tracker {
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateEventTracker to answer 200, got {status} \
                 {answered:?}"
            );
            assert_eq!(
                answered["eventTrackerArn"].as_str(),
                Some(expected_tracker_arn(&name).as_str()),
                "case {case:?}: expected the minted tracker ARN, got {answered:?}"
            );
            let tracking_id = answered["trackingId"].as_str().unwrap_or_default();
            assert!(
                !tracking_id.is_empty() && tracking_id.len() <= 256,
                "case {case:?}: expected a tracking id inside the modelled length, got \
                 {answered:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected CreateEventTracker to answer HTTP 400, got {status} \
             {answered:?}"
        );
        assert!(
            error_code(&answered).contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            format!("Missing INTERACTIONS dataset with datasetgroup: [{group}]"),
            "case {case:?}: expected the missing-dataset wording naming the group asked for"
        );
        assert!(
            answered["eventTrackerArn"].is_null() && answered["trackingId"].is_null(),
            "case {case:?}: expected a refusal to carry no tracker, got {answered:?}"
        );

        // The refused create stored nothing: the group holds no tracker, and the ARN it
        // would have minted resolves to nothing.
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
            "case {case:?}: expected the refused create to store no tracker, got {listed:?}"
        );
        let (status, read) = emulator
            .call_with_arn(
                "DescribeEventTracker",
                "eventTrackerArn",
                &expected_tracker_arn(&name),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected DescribeEventTracker to refuse the ARN the refused \
             create would have minted, got {status} {read:?}"
        );

        // And the precondition is the only thing in the way: once the group holds the Item
        // interactions dataset, the SAME create is served.
        let schema = create_schema(
            &emulator,
            &format!("precondition-{index}-late"),
            &avro_schema(INTERACTIONS),
            case,
        )
        .await;
        create_dataset(&emulator, &group, &schema, INTERACTIONS, case).await;
        let (status, answered) = emulator
            .call(
                "CreateEventTracker",
                json!({"name": name, "datasetGroupArn": group}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served once the group holds the \
             interactions dataset, got {status} {answered:?}"
        );
        assert_eq!(
            answered["eventTrackerArn"].as_str(),
            Some(expected_tracker_arn(&name).as_str()),
            "case {case:?}: expected the minted tracker ARN, got {answered:?}"
        );
        assert!(
            answered["trackingId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected a tracking id, got {answered:?}"
        );
    }
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

    // The Item interactions dataset the tracker's events are appended to: the group holds it
    // before it holds a tracker, which is what makes the create below a legitimate one.
    let (status, body) = emulator
        .control(
            "CreateSchema",
            json!({"name": "et-ingest-interactions", "schema": avro_schema(INTERACTIONS)}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"setup\": expected CreateSchema to answer 200, got {status} {body:?}"
    );
    let schema = body["schemaArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"setup\": expected a schemaArn, got {body:?}"))
        .to_string();
    let (status, body) = emulator
        .control(
            "CreateDataset",
            json!({
                "name": "et-ingest-interactions",
                "datasetGroupArn": group,
                "datasetType": INTERACTIONS,
                "schemaArn": schema,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"setup\": expected CreateDataset to answer 200, got {status} {body:?}"
    );

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
