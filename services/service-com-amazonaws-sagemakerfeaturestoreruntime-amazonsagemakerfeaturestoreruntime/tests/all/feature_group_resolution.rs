// SPDX-License-Identifier: CC0-1.0

//! How this data plane resolves the `FeatureGroupName` every one of its operations
//! carries, and what it answers when that name resolves to no feature group.
//!
//! RULE: a SageMaker Feature Store runtime operation serves a `FeatureGroupName` only
//! when it resolves to a feature group the SageMaker control plane holds; a name that
//! resolves to none is a request defect, refused with the modelled `ValidationError`
//! (HTTP 400) — never with `ResourceNotFound` (404), which says a RECORD is missing from
//! a feature group that exists, and never accepted, which would report an ingestion no
//! later read could find. A group that resolves with NO `OnlineStore` is refused the same
//! way on the READS, which have nothing to serve from it, while a write that states a
//! whole record of its own leaves the online store alone and succeeds: AWS replicates such
//! a write to the group's OfflineStore. `UpdateRecord` is neither — it states a PARTIAL
//! update of a record that must already exist — so in a group holding no live record under
//! the requested identifier it answers the `ResourceNotFound` (404) its own documentation
//! states, whatever stores that group has.
//!
//! The runtime API carries no feature-group definition, so the resolution goes through
//! the shared feature-group directory `CreateFeatureGroup` writes
//! (`service_common::feature_store_groups`). These tests create a group exactly as that
//! handler does and then drive the real front door — the multi-service router, the
//! restJson1 protocol, the modelled constraint validation — so what is asserted is the
//! wire behaviour a client sees.
//!
//! That ONE resolution is what the BATCH operations perform too, for every element of a
//! batch at once and before they take this service's own record-store guard: each
//! element's answer is paired with the feature group THAT element named, and a name is
//! classified exactly as the single-record call carrying the same name classifies it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_common::feature_store_groups::{FeatureGroupDirectory, FeatureGroupRecord};
use smithy_server_runtime::{http, MultiServiceRouter};

use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::EmulatedService;

/// The record identifier every row writes and reads.
const RECORD: &str = "R1";

/// The feature group's own `RecordIdentifierFeatureName`, as its definition fixes it.
const IDENTIFIER_FEATURE: &str = "record_id";

/// The feature group's own `EventTimeFeatureName`.
const EVENT_TIME_FEATURE: &str = "event_time";

/// The event time the seeded record carries, and a later one for a delete.
const EVENT_TIME: &str = "2024-01-01T00:00:00Z";
const LATER_EVENT_TIME: &str = "2024-06-01T00:00:00Z";

/// One operation of this data plane, named by the `FeatureGroupName` it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    PutRecord,
    BatchWriteRecord,
    GetRecord,
    UpdateRecord,
    ListRecords,
    BatchGetRecord,
    DeleteRecord,
}

/// Every operation that names a feature group, in an order that leaves the delete last
/// so a row's earlier calls still see the record.
const EVERY_CALL: [Call; 7] = [
    Call::PutRecord,
    Call::BatchWriteRecord,
    Call::GetRecord,
    Call::UpdateRecord,
    Call::ListRecords,
    Call::BatchGetRecord,
    Call::DeleteRecord,
];

impl Call {
    /// Whether this operation READS the OnlineStore (rather than writing it), which is
    /// what decides how it answers a feature group that exists with no `OnlineStore`:
    /// there is nothing for a read to serve, while a write of a whole record to such a
    /// group is the OfflineStore's business and leaves the online store alone.
    fn reads(self) -> bool {
        matches!(
            self,
            Call::GetRecord | Call::ListRecords | Call::BatchGetRecord
        )
    }

    /// The status and wire error code this operation answers for a feature group that
    /// resolves but has no `OnlineStore`.
    ///
    /// A read has nothing to serve from such a group, and an `UpdateRecord` has no record
    /// to update: its documented precondition is a record that already exists, and a group
    /// with no online store holds none — so the absence of the record is stated as the
    /// modelled `ResourceNotFound`, exactly as it is in a group that has an online store
    /// holding no such record.
    fn answer_without_an_online_store(self) -> (u16, &'static str) {
        match self {
            _ if self.reads() => (400, "ValidationError"),
            Call::UpdateRecord => (404, "ResourceNotFound"),
            _ => (200, ""),
        }
    }
}

/// A booted emulator holding this service only.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// Create a feature group the way the SageMaker control plane's `CreateFeatureGroup`
    /// does: by registering its definition — its two key feature names and whether its
    /// `OnlineStore` is on — in the ONE shared directory this data plane resolves against.
    fn create_feature_group(&self, name: &str, online_store_enabled: bool) {
        self.engine
            .state::<FeatureGroupDirectory>()
            .lock()
            .register(FeatureGroupRecord {
                name: name.to_owned(),
                arn: format!("arn:aws:sagemaker:us-east-1:123456789012:feature-group/{name}"),
                record_identifier_feature_name: IDENTIFIER_FEATURE.to_owned(),
                event_time_feature_name: EVENT_TIME_FEATURE.to_owned(),
                online_store_enabled,
                // Online-only feature groups: no offline store to replicate into.
                offline_store: None,
                default_ttl_seconds: None,
                creation_failed: false,
            });
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        // The SigV4 credential scope is how the router picks the claiming service: the
        // 4th field is this service's signing name.
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20240101/us-east-1/sagemaker/aws4_request, \
                 SignedHeaders=host, Signature=0000",
            )
            .header("host", "featurestore-runtime.sagemaker.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .body(
                body.map(|value| bytes::Bytes::from(value.to_string()))
                    .unwrap_or_default(),
            )
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice::<Value>(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Perform `call` against the feature group named by `group`, as it travels on the
    /// wire (an `@httpLabel` carries an ARN percent-encoded).
    async fn perform(&self, call: Call, group: &str) -> (u16, Value) {
        match call {
            Call::PutRecord => {
                self.send(
                    "PUT",
                    &format!("/FeatureGroup/{group}"),
                    Some(json!({ "Record": [
                        feature(IDENTIFIER_FEATURE, RECORD),
                        feature(EVENT_TIME_FEATURE, EVENT_TIME),
                        feature("feature_a", "original")
                    ] })),
                )
                .await
            }
            Call::BatchWriteRecord => {
                self.send(
                    "POST",
                    "/BatchWriteRecord",
                    Some(json!({ "Entries": [{
                        "FeatureGroupName": percent_decoded(group),
                        "Record": [
                            feature(IDENTIFIER_FEATURE, RECORD),
                            feature(EVENT_TIME_FEATURE, EVENT_TIME),
                            feature("feature_a", "original")
                        ]
                    }] })),
                )
                .await
            }
            Call::GetRecord => {
                self.send(
                    "GET",
                    &format!("/FeatureGroup/{group}?RecordIdentifierValueAsString={RECORD}"),
                    None,
                )
                .await
            }
            Call::UpdateRecord => {
                self.send(
                    "POST",
                    &format!("/FeatureGroup/{group}/Record"),
                    Some(json!({
                        "RecordIdentifierValueAsString": RECORD,
                        "Features": [feature("feature_a", "updated")]
                    })),
                )
                .await
            }
            Call::ListRecords => {
                self.send(
                    "POST",
                    &format!("/FeatureGroup/{group}/ListRecords"),
                    Some(json!({})),
                )
                .await
            }
            Call::BatchGetRecord => {
                self.send(
                    "POST",
                    "/BatchGetRecord",
                    Some(json!({ "Identifiers": [{
                        "FeatureGroupName": percent_decoded(group),
                        "RecordIdentifiersValueAsString": [RECORD]
                    }] })),
                )
                .await
            }
            Call::DeleteRecord => {
                self.send(
                    "DELETE",
                    &format!(
                        "/FeatureGroup/{group}?RecordIdentifierValueAsString={RECORD}\
                         &EventTime={LATER_EVENT_TIME}"
                    ),
                    None,
                )
                .await
            }
        }
    }
}

/// One `FeatureValue` with a scalar value.
fn feature(name: &str, value: &str) -> Value {
    json!({ "FeatureName": name, "ValueAsString": value })
}

/// The name as a BODY member carries it: `BatchGetRecordIdentifier$FeatureGroupName` is
/// no `@httpLabel`, so it is not percent-encoded on the wire.
fn percent_decoded(group: &str) -> String {
    group.replace("%3A", ":").replace("%2F", "/")
}

/// The wire error code of an error response: restJson1 carries it as the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The per-entry `ErrorCode`s a batch response reports — `BatchGetRecord` per
/// identifier, `BatchWriteRecord` per entry.
fn batch_error_codes(body: &Value) -> Vec<String> {
    body.get("Errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .map(|error| {
                    error["ErrorCode"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How a feature group was (or was not) created before the row ran.
#[derive(Debug, Clone, Copy)]
enum Group {
    /// Created with `OnlineStoreConfig.EnableOnlineStore` true.
    WithOnlineStore,
    /// Created with no `OnlineStore` — the documented default of `EnableOnlineStore`.
    WithoutOnlineStore,
    /// Never created.
    Absent,
}

#[tokio::test]
async fn every_operation_refuses_a_feature_group_name_that_resolves_to_no_feature_group() {
    // The class the rule spans: how the group was created, crossed with every spelling
    // of the name the modelled `FeatureGroupNameOrArn` admits. Each row is run against
    // EVERY operation that names one feature group.
    //
    // (case, how the group was created, the group's name, the spelling the call carries)
    let rows: Vec<(&str, Group, &str, String)> = vec![
        (
            "a created group, by bare name",
            Group::WithOnlineStore,
            "customers",
            "customers".to_owned(),
        ),
        (
            "a created group, by the ARN the control plane minted",
            Group::WithOnlineStore,
            "orders",
            "arn%3Aaws%3Asagemaker%3Aus-east-1%3A123456789012%3Afeature-group%2Forders".to_owned(),
        ),
        (
            "a created group, by an ARN of another Region and account",
            Group::WithOnlineStore,
            "events",
            "arn%3Aaws%3Asagemaker%3Aeu-west-1%3A000000000000%3Afeature-group%2Fevents".to_owned(),
        ),
        (
            "a name never created",
            Group::Absent,
            "nosuchgroup",
            "nosuchgroup".to_owned(),
        ),
        (
            "a name never created, as an ARN",
            Group::Absent,
            "alsonosuchgroup",
            "arn%3Aaws%3Asagemaker%3Aus-east-1%3A123456789012%3Afeature-group%2Falsonosuchgroup"
                .to_owned(),
        ),
        (
            "a name that differs from a created one by one character",
            Group::Absent,
            "customer",
            "customer".to_owned(),
        ),
        (
            "a group created without an OnlineStore",
            Group::WithoutOnlineStore,
            "offlineonly",
            "offlineonly".to_owned(),
        ),
    ];

    for (case, group, name, spelling) in rows {
        let emulator = Emulator::new();
        // Every row has one OTHER group created, so no row's verdict can come from the
        // directory being empty.
        emulator.create_feature_group("customers", true);
        match group {
            Group::WithOnlineStore => emulator.create_feature_group(name, true),
            Group::WithoutOnlineStore => emulator.create_feature_group(name, false),
            Group::Absent => {}
        }
        let resolves = matches!(group, Group::WithOnlineStore);
        if resolves {
            // Seed the record the reads and the update of this row work on, through the
            // bare name, so the row's own spelling is the only thing that varies.
            let (status, body) = emulator.perform(Call::PutRecord, name).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (200, ""),
                "case {case:?}: expected the seeding PutRecord to be accepted, got {status} \
                 with body {body:?}"
            );
        }

        for call in EVERY_CALL {
            // What each class of group answers. A name that resolves to NO feature group
            // is refused everywhere, as a request defect. A group that resolves but has no
            // `OnlineStore` answers per operation
            // ([`Call::answer_without_an_online_store`]).
            let (expected_status, expected_code) = match group {
                Group::WithOnlineStore => (200, ""),
                Group::Absent => (400, "ValidationError"),
                Group::WithoutOnlineStore => call.answer_without_an_online_store(),
            };
            let refused = expected_status != 200;
            let (status, body) = emulator.perform(call, &spelling).await;
            let code = error_code(&body);
            if matches!(call, Call::BatchGetRecord | Call::BatchWriteRecord) {
                // A batch operation reports a failure that concerns one identifier or
                // one entry in `Errors` rather than failing the batch, so the same
                // classification arrives as that element's own `ErrorCode`.
                let reported = batch_error_codes(&body);
                let expected: Vec<String> = if refused {
                    vec![expected_code.to_owned()]
                } else {
                    Vec::new()
                };
                assert_eq!(
                    (status, reported.clone()),
                    (200, expected.clone()),
                    "case {case:?}/{call:?}: expected HTTP 200 with per-identifier errors \
                     {expected:?}, got {status} with {reported:?} in {body:?}"
                );
                continue;
            }
            assert_eq!(
                (status, code.as_str()),
                (expected_status, expected_code),
                "case {case:?}/{call:?}: expected HTTP {expected_status} \
                 {expected_code:?}, got {status} {code:?} with body {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_record_written_to_a_name_that_does_not_resolve_is_never_stored() {
    let emulator = Emulator::new();
    emulator.create_feature_group("customers", true);

    // The refusal is what keeps the silent data-loss path shut: the write is refused,
    // and the name stays unreadable afterwards rather than holding a record no feature
    // group accounts for.
    let (status, body) = emulator.perform(Call::PutRecord, "nosuchgroup").await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationError"),
        "case \"PutRecord against an unresolvable name\": expected HTTP 400 ValidationError, \
         got {status} with body {body:?}"
    );

    // And once the group IS created, the same write is accepted and reads back.
    emulator.create_feature_group("nosuchgroup", true);
    let (status, _) = emulator.perform(Call::PutRecord, "nosuchgroup").await;
    assert_eq!(
        status, 200,
        "case \"PutRecord after the group is created\": expected 200, got {status}"
    );
    let (status, body) = emulator.perform(Call::GetRecord, "nosuchgroup").await;
    let served: Vec<String> = body["Record"]
        .as_array()
        .map(|features| {
            features
                .iter()
                .map(|f| f["FeatureName"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        (status, served.clone()),
        (
            200,
            vec![
                IDENTIFIER_FEATURE.to_owned(),
                EVENT_TIME_FEATURE.to_owned(),
                "feature_a".to_owned()
            ]
        ),
        "case \"GetRecord after the group is created\": expected the written features, got \
         {status} with {served:?} in {body:?}"
    );
}

#[tokio::test]
async fn the_records_identifier_is_the_groups_record_identifier_feature_whatever_order_it_arrives_in()
{
    let emulator = Emulator::new();
    emulator.create_feature_group("customers", true);

    // RULE: the record's identifier is the value of the group's own
    // `RecordIdentifierFeatureName`, not of whichever feature the request happens to
    // list first. Rows: the identifier first, last, in the middle, and absent.
    //
    // (case, the posted Record, the identifier the write must store)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "the identifier first",
            json!([
                feature(IDENTIFIER_FEATURE, "first"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("feature_a", "a")
            ]),
            Some("first"),
        ),
        (
            "the identifier last",
            json!([
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("feature_a", "a"),
                feature(IDENTIFIER_FEATURE, "last")
            ]),
            Some("last"),
        ),
        (
            "the identifier in the middle",
            json!([
                feature("feature_a", "a"),
                feature(IDENTIFIER_FEATURE, "middle"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME)
            ]),
            Some("middle"),
        ),
        (
            "no identifier feature at all",
            json!([
                feature("feature_a", "a"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME)
            ]),
            None,
        ),
    ];

    for (case, record, stored) in rows {
        let (status, body) = emulator
            .send(
                "PUT",
                "/FeatureGroup/customers",
                Some(json!({ "Record": record })),
            )
            .await;
        match stored {
            Some(identifier) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected PutRecord 200, got {status} with body {body:?}"
                );
                let (status, body) = emulator
                    .send(
                        "GET",
                        &format!(
                            "/FeatureGroup/customers?RecordIdentifierValueAsString={identifier}"
                        ),
                        None,
                    )
                    .await;
                let served = body.get("Record").cloned().unwrap_or(Value::Null);
                assert!(
                    status == 200 && served.is_array(),
                    "case {case:?}: expected the record to be stored under {identifier:?}, \
                     got {status} with body {body:?}"
                );
            }
            None => {
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (400, "ValidationError"),
                    "case {case:?}: expected HTTP 400 ValidationError for a record carrying \
                     no value for the group's RecordIdentifierFeatureName, got {status} \
                     {code:?} with body {body:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_write_to_a_group_without_an_online_store_is_still_judged_by_the_requests_own_rules() {
    let emulator = Emulator::new();
    emulator.create_feature_group("customers", true);
    emulator.create_feature_group("offlineonly", false);

    // RULE: whether a feature group has an `OnlineStore` decides only whether the WRITE
    // reaches this store, never whether the request is well-formed — so the same record
    // is judged the same way in both groups, and only a well-formed one differs (stored
    // online, or accepted with nothing stored here).
    //
    // (case, the group to write to, the posted Record, expected status)
    let rows: Vec<(&str, &str, Value, u16)> = vec![
        (
            "an empty Record, to a group with an OnlineStore",
            "customers",
            json!([]),
            400,
        ),
        (
            "an empty Record, to a group without one",
            "offlineonly",
            json!([]),
            400,
        ),
        (
            "a Record missing the group's identifier feature, with an OnlineStore",
            "customers",
            json!([feature("feature_a", "a")]),
            400,
        ),
        (
            "a Record missing the group's identifier feature, without one",
            "offlineonly",
            json!([feature("feature_a", "a")]),
            400,
        ),
        (
            "an event time that is no timestamp, without an OnlineStore",
            "offlineonly",
            json!([
                feature(IDENTIFIER_FEATURE, RECORD),
                feature(EVENT_TIME_FEATURE, "yesterday")
            ]),
            400,
        ),
        (
            "a well-formed Record, to a group without an OnlineStore",
            "offlineonly",
            json!([
                feature(IDENTIFIER_FEATURE, RECORD),
                feature(EVENT_TIME_FEATURE, EVENT_TIME)
            ]),
            200,
        ),
        (
            "a well-formed Record, to a group with an OnlineStore",
            "customers",
            json!([
                feature(IDENTIFIER_FEATURE, RECORD),
                feature(EVENT_TIME_FEATURE, EVENT_TIME)
            ]),
            200,
        ),
    ];

    for (case, group, record, expected_status) in rows {
        let (status, body) = emulator
            .send(
                "PUT",
                &format!("/FeatureGroup/{group}"),
                Some(json!({ "Record": record })),
            )
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected PutRecord {expected_status}, got {status} with body \
             {body:?}"
        );
    }

    // The accepted write to the offline-only group stored nothing HERE, and the read of
    // that group is refused rather than answering an empty record.
    let (status, body) = emulator.perform(Call::GetRecord, "offlineonly").await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationError"),
        "case \"GetRecord of the offline-only group\": expected HTTP 400 ValidationError, \
         got {status} with body {body:?}"
    );
}

/// The ARN the control plane mints for the feature group `name`, the second spelling the
/// modelled `FeatureGroupNameOrArn` admits.
fn arn_of(name: &str) -> String {
    format!("arn:aws:sagemaker:us-east-1:123456789012:feature-group/{name}")
}

/// Create a feature group whose CREATION FAILED, as the control plane records one whose
/// `FeatureGroupStatus` reached `CREATE_FAILED`: the name resolves to a definition that
/// holds no provisioned store, so every operation is refused on it — the second way a
/// name this data plane resolves yields no servable group.
fn create_failed_feature_group(emulator: &Emulator, name: &str) {
    emulator
        .engine
        .state::<FeatureGroupDirectory>()
        .lock()
        .register(FeatureGroupRecord {
            name: name.to_owned(),
            arn: arn_of(name),
            record_identifier_feature_name: IDENTIFIER_FEATURE.to_owned(),
            event_time_feature_name: EVENT_TIME_FEATURE.to_owned(),
            online_store_enabled: true,
            offline_store: None,
            default_ttl_seconds: None,
            creation_failed: true,
        });
}

/// One element of a batch: the feature group it names, the record it writes and reads,
/// and whether that name resolves to a group this data plane can serve.
#[derive(Debug, Clone)]
struct Element {
    /// What this row is, named in every assertion.
    case: &'static str,
    /// The spelling the element carries. A batch names its feature group in a BODY
    /// member, so no spelling is percent-encoded here.
    name: String,
    /// The record identifier this element writes, and reads back.
    identifier: &'static str,
    /// Whether a READ of this element is refused — a name that resolves to no feature
    /// group, and a group that resolves with no `OnlineStore` to read.
    read_refused: bool,
    /// Whether a WRITE of this element is refused — a name that resolves to no feature
    /// group. A group with no `OnlineStore` takes the write without storing it here.
    write_refused: bool,
}

/// The `(FeatureGroupName, RecordIdentifierValueAsString)` pairs a list of
/// `BatchGetRecordResultDetail` or `BatchGetRecordError` elements carries, in the order
/// the response lists them.
fn named_pairs(body: &Value, member: &str) -> Vec<(String, String)> {
    body.get(member)
        .and_then(Value::as_array)
        .map(|elements| {
            elements
                .iter()
                .map(|element| {
                    (
                        element["FeatureGroupName"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        element["RecordIdentifierValueAsString"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `FeatureGroupName` of each entry a `BatchWriteRecord` response reports in
/// `Errors` ("The entry that failed to be written"), in the order it lists them.
fn refused_entry_names(body: &Value) -> Vec<String> {
    body.get("Errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .map(|error| {
                    error["Entry"]["FeatureGroupName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The feature groups the resolution table is created against: one with an `OnlineStore`,
/// a second with one, a third created without one, and one whose creation failed.
fn create_resolution_groups(emulator: &Emulator) {
    emulator.create_feature_group("customers", true);
    emulator.create_feature_group("orders", true);
    emulator.create_feature_group("offlineonly", false);
    create_failed_feature_group(emulator, "halfbuilt");
}

/// The class the resolution rule spans: how an element's name resolves, crossed with the
/// spelling it arrives in — including two elements naming the SAME feature group in its
/// two spellings, so an answer paired with the wrong element cannot pass unnoticed.
fn resolution_elements() -> Vec<Element> {
    vec![
        Element {
            case: "a created group, by bare name",
            name: "customers".to_owned(),
            identifier: "c1",
            read_refused: false,
            write_refused: false,
        },
        Element {
            case: "the same created group, by the ARN the control plane minted",
            name: arn_of("customers"),
            identifier: "c2",
            read_refused: false,
            write_refused: false,
        },
        Element {
            case: "a second created group, by its ARN",
            name: arn_of("orders"),
            identifier: "o1",
            read_refused: false,
            write_refused: false,
        },
        Element {
            case: "a name never created",
            name: "nosuchgroup".to_owned(),
            identifier: "x1",
            read_refused: true,
            write_refused: true,
        },
        Element {
            case: "a group created without an OnlineStore",
            name: "offlineonly".to_owned(),
            identifier: "f1",
            read_refused: true,
            write_refused: false,
        },
        Element {
            case: "a group whose creation failed",
            name: "halfbuilt".to_owned(),
            identifier: "h1",
            read_refused: true,
            write_refused: true,
        },
        Element {
            case: "a name never created, as an ARN",
            name: arn_of("ghost"),
            identifier: "g1",
            read_refused: true,
            write_refused: true,
        },
    ]
}

/// One `BatchWriteRecord` carrying every row of `elements`: each entry writes the row's
/// own record into the feature group that row names.
async fn write_the_batch(emulator: &Emulator, elements: &[Element]) -> (u16, Value) {
    emulator
        .send(
            "POST",
            "/BatchWriteRecord",
            Some(json!({
                "Entries": elements
                    .iter()
                    .map(|element| json!({
                        "FeatureGroupName": element.name,
                        "Record": [
                            feature(IDENTIFIER_FEATURE, element.identifier),
                            feature(EVENT_TIME_FEATURE, EVENT_TIME),
                            feature("feature_a", element.identifier)
                        ]
                    }))
                    .collect::<Vec<Value>>()
            })),
        )
        .await
}

#[tokio::test]
async fn one_batch_pairs_every_elements_answer_with_the_feature_group_that_element_named() {
    let emulator = Emulator::new();
    create_resolution_groups(&emulator);
    let elements = resolution_elements();
    let cases: Vec<&str> = elements.iter().map(|element| element.case).collect();

    // ONE BatchWriteRecord carrying every row: each entry is written to the group IT
    // named, and an entry whose name resolves to no servable group is that entry's own
    // error rather than a failure of the batch.
    let (status, body) = write_the_batch(&emulator, &elements).await;
    let refused = refused_entry_names(&body);
    let expected_refused: Vec<String> = elements
        .iter()
        .filter(|element| element.write_refused)
        .map(|element| element.name.clone())
        .collect();
    assert_eq!(
        (status, refused.clone()),
        (200, expected_refused.clone()),
        "cases {cases:?}: expected HTTP 200 with the entries {expected_refused:?} reported \
         in Errors, got {status} with {refused:?} in {body:?}"
    );
    assert_eq!(
        batch_error_codes(&body),
        vec!["ValidationError".to_owned(); expected_refused.len()],
        "cases {cases:?}: expected every reported entry to carry the ValidationError code, \
         got {body:?}"
    );
    let unprocessed = body["UnprocessedEntries"].as_array().map(Vec::len);
    assert_eq!(
        unprocessed,
        Some(0),
        "cases {cases:?}: expected no entry to be left unprocessed, got {unprocessed:?} in \
         {body:?}"
    );

    // Then ONE BatchGetRecord carrying every row, read back from the same batch: each
    // identifier is read from the group ITS element named, and reported under the
    // spelling that element used.
    let (status, body) = emulator
        .send(
            "POST",
            "/BatchGetRecord",
            Some(json!({
                "Identifiers": elements
                    .iter()
                    .map(|element| json!({
                        "FeatureGroupName": element.name,
                        "RecordIdentifiersValueAsString": [element.identifier]
                    }))
                    .collect::<Vec<Value>>()
            })),
        )
        .await;
    let served = named_pairs(&body, "Records");
    let expected_served: Vec<(String, String)> = elements
        .iter()
        .filter(|element| !element.read_refused)
        .map(|element| (element.name.clone(), element.identifier.to_owned()))
        .collect();
    assert_eq!(
        (status, served.clone()),
        (200, expected_served.clone()),
        "cases {cases:?}: expected the records {expected_served:?} to come back, got \
         {status} with {served:?} in {body:?}"
    );
    let reported = named_pairs(&body, "Errors");
    let expected_reported: Vec<(String, String)> = elements
        .iter()
        .filter(|element| element.read_refused)
        .map(|element| (element.name.clone(), element.identifier.to_owned()))
        .collect();
    assert_eq!(
        reported.clone(),
        expected_reported.clone(),
        "cases {cases:?}: expected the identifiers {expected_reported:?} to be reported in \
         Errors, got {reported:?} in {body:?}"
    );
    assert_eq!(
        batch_error_codes(&body),
        vec!["ValidationError".to_owned(); expected_reported.len()],
        "cases {cases:?}: expected every reported identifier to carry the ValidationError \
         code, got {body:?}"
    );
    let unprocessed = body["UnprocessedIdentifiers"].as_array().map(Vec::len);
    assert_eq!(
        unprocessed,
        Some(0),
        "cases {cases:?}: expected no identifier to be left unprocessed, got \
         {unprocessed:?} in {body:?}"
    );
}

/// The name as an `@httpLabel` carries it: `GetRecord$FeatureGroupName` IS a label, so an
/// ARN spelling travels percent-encoded there — the inverse of [`percent_decoded`], which
/// a batch's body member needs.
fn percent_encoded(name: &str) -> String {
    name.replace(':', "%3A").replace('/', "%2F")
}

#[tokio::test]
async fn a_batch_element_and_a_single_record_read_classify_one_name_the_same_way() {
    let emulator = Emulator::new();
    create_resolution_groups(&emulator);
    let elements = resolution_elements();

    // Seed every row's record through one batch write, so each read below has a record
    // to find wherever the name resolves to a group this store serves.
    let (status, body) = write_the_batch(&emulator, &elements).await;
    assert_eq!(
        status, 200,
        "case \"the seeding batch write\": expected HTTP 200, got {status} with body \
         {body:?}"
    );

    // RULE: one `FeatureGroupName` resolves ONE way for this data plane, whether the
    // operation that carries it names one feature group or several — so a batch element
    // and the single-record read of the same name and identifier agree about whether the
    // name denotes a group this store can serve. The batch carries the classification in
    // its own per-element `Errors`, the single-record read in the modelled
    // `ValidationError`; what cannot differ is WHICH names are refused.
    for element in &elements {
        let case = element.case;
        let (batch_status, batch_body) = emulator
            .send(
                "POST",
                "/BatchGetRecord",
                Some(json!({ "Identifiers": [{
                    "FeatureGroupName": element.name,
                    "RecordIdentifiersValueAsString": [element.identifier]
                }] })),
            )
            .await;
        let batch_served = named_pairs(&batch_body, "Records");
        let batch_reported = named_pairs(&batch_body, "Errors");
        let (status, body) = emulator
            .send(
                "GET",
                &format!(
                    "/FeatureGroup/{}?RecordIdentifierValueAsString={}",
                    percent_encoded(&element.name),
                    element.identifier
                ),
                None,
            )
            .await;
        let code = error_code(&body);
        let served = body
            .get("Record")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();

        let pair = (element.name.clone(), element.identifier.to_owned());
        if element.read_refused {
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationError"),
                "case {case:?}: expected the single-record read to be refused with HTTP 400 \
                 ValidationError, got {status} {code:?} with body {body:?}"
            );
            assert_eq!(
                (batch_status, batch_reported.clone(), batch_served.clone()),
                (200, vec![pair.clone()], Vec::new()),
                "case {case:?}: expected the batch element to be reported in Errors as \
                 {pair:?} and to serve no record, got {batch_status} with {batch_body:?}"
            );
        } else {
            assert!(
                status == 200 && served > 0,
                "case {case:?}: expected the single-record read to serve the record, got \
                 {status} with {served} features in {body:?}"
            );
            assert_eq!(
                (batch_status, batch_served.clone(), batch_reported.clone()),
                (200, vec![pair.clone()], Vec::new()),
                "case {case:?}: expected the batch element to serve {pair:?} and to report \
                 no error, got {batch_status} with {batch_body:?}"
            );
        }
    }
}
