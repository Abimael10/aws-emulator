// SPDX-License-Identifier: CC0-1.0

//! What a batch write leaves behind for an entry it reports in `UnprocessedEntries`.
//!
//! The rule under test: an entry a whole-record write cannot take because the emulator's
//! bounded online store has no room for it is NOT processed — `UnprocessedEntries` is "A
//! list of entries that were not processed. These entries can be retried." — so the write
//! leaves nothing behind in either of the feature group's stores, exactly as the
//! single-record writer's identical capacity refusal does, and the retry the response
//! invites is therefore the record's FIRST write rather than its second. An entry the
//! online side does not take for any OTHER reason — an entry naming only the OfflineStore,
//! or a historic one whose event time is not later than the stored record's — is processed
//! and is replicated, whether or not the online store has room for anything new.
//!
//! Both services the rule spans are mounted on ONE shared engine, as the router mounts
//! them: this data plane and Amazon S3, which holds the objects the offline store is made
//! of. Every call goes through the real front door — this service's own restJson1 requests
//! and the path-style Amazon S3 requests a client sends.
//!
//! The two capacity bounds are reached by filling the online store through its OWN API
//! (`crate::state`), which is the store the handlers read: a run of ten thousand wire
//! writes would fill the same map and prove nothing more about the refusal. That the
//! filled store IS the one the handlers serve is asserted first, by reading a seeded record
//! back through `GetRecord`.

use std::sync::Arc;
use std::time::SystemTime;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{json, Value};
use service_common::feature_store_groups::{FeatureGroupDirectory, FeatureGroupRecord};
use service_common::feature_store_offline_store::{resolved_output_s3_uri, OfflineStoreDelivery};
use smithy_server_runtime::{http, MultiServiceRouter};

use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::state::{
    FeatureGroupStore, StoredFeatureValue, StoredRecord, MAX_FEATURE_GROUPS,
    MAX_RECORDS_PER_GROUP,
};
use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::{
    ComAmazonawsSagemakerfeaturestoreruntimeAmazonsagemakerfeaturestoreruntimeState as OnlineStore,
    EmulatedService,
};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The host the requests name; the emulator serves one endpoint for every service.
const HOST: &str = "s3.us-east-1.amazonaws.com";

/// The feature groups' record-identifier feature.
const IDENTIFIER_FEATURE: &str = "customer_id";

/// The feature groups' event-time feature.
const EVENT_TIME_FEATURE: &str = "event_time";

/// A feature carrying the record's payload, so a row's bytes can be told apart.
const PAYLOAD_FEATURE: &str = "payload";

/// The feature group whose write runs into a capacity bound.
const REFUSED_GROUP: &str = "capacityrefused";

/// The feature group whose write does not, so a batch can mix the two.
const ACCEPTED_GROUP: &str = "capacityaccepted";

/// The event time the seeded records carry, so a posted record can be made HISTORIC
/// against them.
const SEEDED_EVENT_TIME: &str = "2026-06-01T00:00:00Z";

/// The event time a posted entry carries: later than [`SEEDED_EVENT_TIME`], so a write of a
/// seeded identifier is not historic unless the case means it to be.
const POSTED_EVENT_TIME: &str = "2026-07-01T00:00:00Z";

/// An event time EARLIER than [`SEEDED_EVENT_TIME`]: a write carrying it is historic.
const HISTORIC_EVENT_TIME: &str = "2025-01-01T00:00:00Z";

/// The identifier of the first seeded record of a filled group, read back through
/// `GetRecord` to show the filled store is the one the handlers serve.
const SEEDED_IDENTIFIER: &str = "seed0";

/// Which of the online store's two capacity bounds the refused entry runs into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// The store already holds the most feature groups it keeps, so a group it has taken
    /// no record for yet cannot be created at all.
    GroupCount,
    /// The entry's own feature group already holds the most records it keeps, so a NEW
    /// record cannot be stored in it.
    RecordCount,
}

/// This data plane and Amazon S3 on one shared engine.
struct Aws {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    /// The bucket both feature groups' offline stores live in.
    bucket: String,
}

impl Aws {
    /// A booted emulator whose online store is filled to `bound`, with both feature groups
    /// registered as the control plane registers them: an OnlineStore and an OfflineStore
    /// in `bucket`.
    async fn with_store_filled_to(bound: Bound, bucket: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        let aws = Self {
            engine,
            router,
            bucket: bucket.to_owned(),
        };
        aws.create_bucket().await;
        for group in [REFUSED_GROUP, ACCEPTED_GROUP] {
            aws.register_group(group);
        }
        aws.fill(bound);
        aws
    }

    /// Register one feature group the way `CreateFeatureGroup` does: in the ONE shared
    /// feature-group directory this data plane resolves names against, with an OnlineStore
    /// and an OfflineStore under this run's bucket.
    fn register_group(&self, group: &str) {
        let s3_uri = format!("s3://{}/offline", self.bucket);
        let resolved = resolved_output_s3_uri(
            &s3_uri,
            engine::emulator_account_id(),
            REGION,
            group,
            SystemTime::now(),
        )
        .expect("the configured S3Uri names a location");
        self.engine
            .state::<FeatureGroupDirectory>()
            .lock()
            .register(FeatureGroupRecord {
                name: group.to_owned(),
                arn: format!(
                    "arn:aws:sagemaker:{REGION}:{}:feature-group/{group}",
                    engine::emulator_account_id()
                ),
                record_identifier_feature_name: IDENTIFIER_FEATURE.to_owned(),
                event_time_feature_name: EVENT_TIME_FEATURE.to_owned(),
                online_store_enabled: true,
                offline_store: Some(OfflineStoreDelivery {
                    resolved_output_s3_uri: resolved,
                    // No IAM is mounted here, so no permission is withheld: this suite is
                    // about what the batch handler queues, not about authorization, which
                    // `offline_store_replication.rs` pins.
                    role_arn: format!(
                        "arn:aws:iam::{}:role/offline-store",
                        engine::emulator_account_id()
                    ),
                }),
                default_ttl_seconds: None,
                creation_failed: false,
            });
    }

    /// Fill the online store up to `bound`, through the store's own API.
    fn fill(&self, bound: Bound) {
        let state = self.engine.state::<OnlineStore>();
        let mut store = state.lock();
        match bound {
            // Every feature group the store keeps is taken, and the group the refused
            // entry names is NOT one of them — while the group the accepted entry names
            // is, so that entry's write still has a store to land in.
            Bound::GroupCount => {
                seed(
                    store
                        .group_or_create(ACCEPTED_GROUP)
                        .expect("an empty store has room for a group"),
                    1,
                );
                for index in 0..MAX_FEATURE_GROUPS - 1 {
                    store
                        .group_or_create(&format!("filler{index}"))
                        .expect("the store has room up to its bound");
                }
            }
            // The refused entry's own group is full of records; the accepted entry's group
            // is empty.
            Bound::RecordCount => {
                seed(
                    store
                        .group_or_create(REFUSED_GROUP)
                        .expect("an empty store has room for a group"),
                    MAX_RECORDS_PER_GROUP,
                );
            }
        }
    }

    /// The feature group holding [`SEEDED_IDENTIFIER`] under `bound`.
    fn seeded_group(bound: Bound) -> &'static str {
        match bound {
            Bound::GroupCount => ACCEPTED_GROUP,
            Bound::RecordCount => REFUSED_GROUP,
        }
    }

    /// Send one request to this data plane, answering its status and parsed body.
    async fn data_plane(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", HOST)
            .header("authorization", authorization("sagemaker"))
            .body(
                body.map(|value| bytes::Bytes::from(value.to_string()))
                    .unwrap_or_default(),
            )
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice::<Value>(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// `POST /BatchWriteRecord`.
    async fn batch_write_record(&self, entries: Value) -> (u16, Value) {
        self.data_plane("POST", "/BatchWriteRecord", Some(json!({ "Entries": entries })))
            .await
    }

    /// `GET /FeatureGroup/<group>?…` — one `GetRecord`.
    async fn get_record(&self, group: &str, identifier: &str) -> (u16, Value) {
        self.data_plane(
            "GET",
            &format!("/FeatureGroup/{group}?RecordIdentifierValueAsString={identifier}"),
            None,
        )
        .await
    }

    /// One path-style Amazon S3 request.
    async fn s3(&self, method: &str, path: &str) -> (u16, bytes::Bytes) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{HOST}{path}"))
            .header("host", HOST)
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::new())
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        (parts.status.as_u16(), body)
    }

    /// `PUT /<bucket>` — Amazon S3's `CreateBucket`.
    async fn create_bucket(&self) {
        let (status, body) = self.s3("PUT", &format!("/{}", self.bucket)).await;
        assert_eq!(
            status,
            200,
            "the bucket {} must be created, got {status} {}",
            self.bucket,
            String::from_utf8_lossy(&body)
        );
    }

    /// Every key the run's bucket holds, read through `ListObjectsV2`.
    async fn keys(&self) -> Vec<String> {
        let (status, body) = self
            .s3("GET", &format!("/{}?list-type=2", self.bucket))
            .await;
        assert_eq!(
            status,
            200,
            "listing {} must be served, got {status} {}",
            self.bucket,
            String::from_utf8_lossy(&body)
        );
        let document = String::from_utf8_lossy(&body).into_owned();
        let mut reader = Reader::from_str(&document);
        let mut keys = Vec::new();
        let mut element = String::new();
        loop {
            match reader
                .read_event()
                .unwrap_or_else(|error| panic!("the listing must be well-formed XML: {error}"))
            {
                Event::Start(start) => {
                    element = String::from_utf8_lossy(start.name().as_ref()).into_owned();
                }
                Event::Text(text) => {
                    if element == "Key" {
                        keys.push(text.xml10_content().unwrap_or_default().into_owned());
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        keys
    }
}

/// Fill `group` with `count` records, each readable through `GetRecord`.
fn seed(group: &mut FeatureGroupStore, count: usize) {
    for index in 0..count {
        let identifier = format!("seed{index}");
        let stored = StoredRecord {
            identifier: identifier.clone(),
            features: vec![
                StoredFeatureValue {
                    feature_name: IDENTIFIER_FEATURE.to_owned(),
                    value_as_string: Some(identifier),
                    value_as_string_list: None,
                },
                StoredFeatureValue {
                    feature_name: EVENT_TIME_FEATURE.to_owned(),
                    value_as_string: Some(SEEDED_EVENT_TIME.to_owned()),
                    value_as_string_list: None,
                },
            ],
            event_time: Some(SEEDED_EVENT_TIME.to_owned()),
            expires_at: None,
            expires_at_secs: None,
            ttl_secs: None,
            soft_deleted: false,
        };
        assert!(
            group.insert_record(stored),
            "case seeding the online store: record {index} of {count} must be stored"
        );
    }
}

/// A SigV4 `Authorization` header whose credential scope names `service` — the routing key
/// the router claims a request by. Neither the key id nor the signature is a credential:
/// this harness turns request authentication off, so nothing here is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `BatchWriteRecord` entry.
fn entry(group: &str, identifier: &str, event_time: &str, target_stores: Option<&[&str]>) -> Value {
    let mut built = json!({
        "FeatureGroupName": group,
        "Record": [
            { "FeatureName": IDENTIFIER_FEATURE, "ValueAsString": identifier },
            { "FeatureName": EVENT_TIME_FEATURE, "ValueAsString": event_time },
            { "FeatureName": PAYLOAD_FEATURE, "ValueAsString": "written" },
        ],
    });
    if let Some(stores) = target_stores {
        built
            .as_object_mut()
            .expect("a JSON object entry")
            .insert("TargetStores".to_string(), json!(stores));
    }
    built
}

/// The `(FeatureGroupName, record identifier)` of every entry a response reports under
/// `member`.
fn reported(body: &Value, member: &str) -> Vec<(String, String)> {
    body[member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            let group = entry["FeatureGroupName"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let identifier = entry["Record"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .find(|feature| feature["FeatureName"] == json!(IDENTIFIER_FEATURE))
                .and_then(|feature| feature["ValueAsString"].as_str())
                .unwrap_or_default()
                .to_owned();
            (group, identifier)
        })
        .collect()
}

/// One row of the refusal table.
struct Row {
    case: &'static str,
    /// The bucket this row's offline stores live in; a row's own, so its listing holds only
    /// its own objects.
    bucket: &'static str,
    /// Which capacity bound the refused entry runs into.
    bound: Bound,
    /// The `TargetStores` the refused entry names, and `None` for an entry that names none
    /// — "By default, Feature Store adds the record to all of the stores that you're using
    /// for the FeatureGroup."
    target_stores: Option<&'static [&'static str]>,
    /// Whether the batch also carries an entry that IS processed, so the refusal of one
    /// entry can be told from the batch doing nothing at all.
    with_accepted_entry: bool,
}

/// RULE: an entry a batch write reports in `UnprocessedEntries` was not processed, so the
/// write leaves nothing for it in EITHER of its feature group's stores — no online record
/// and no offline object — and retrying it changes neither; every other entry of the same
/// batch is written and replicated as usual.
///
/// The rows differ along what the rule generalises over: WHICH of the online store's two
/// capacity bounds refuses the entry, WHICH stores the refused entry names, and whether the
/// batch mixes the refused entry with one that is processed.
#[tokio::test]
async fn an_entry_reported_unprocessed_is_written_into_neither_store() {
    let rows: [Row; 6] = [
        Row {
            case: "the group-count bound, both stores by default, the refused entry alone",
            bucket: "features-cap-groups-alone",
            bound: Bound::GroupCount,
            target_stores: None,
            with_accepted_entry: false,
        },
        Row {
            case: "the group-count bound, both stores named, beside an accepted entry",
            bucket: "features-cap-groups-mixed",
            bound: Bound::GroupCount,
            target_stores: Some(&["OnlineStore", "OfflineStore"]),
            with_accepted_entry: true,
        },
        Row {
            case: "the group-count bound, the OnlineStore only",
            bucket: "features-cap-groups-online",
            bound: Bound::GroupCount,
            target_stores: Some(&["OnlineStore"]),
            with_accepted_entry: false,
        },
        Row {
            case: "the record bound, both stores by default, the refused entry alone",
            bucket: "features-cap-records-alone",
            bound: Bound::RecordCount,
            target_stores: None,
            with_accepted_entry: false,
        },
        Row {
            case: "the record bound, both stores named, beside an accepted entry",
            bucket: "features-cap-records-mixed",
            bound: Bound::RecordCount,
            target_stores: Some(&["OnlineStore", "OfflineStore"]),
            with_accepted_entry: true,
        },
        Row {
            case: "the record bound, the OnlineStore only, beside an accepted entry",
            bucket: "features-cap-records-online",
            bound: Bound::RecordCount,
            target_stores: Some(&["OnlineStore"]),
            with_accepted_entry: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let aws = Aws::with_store_filled_to(row.bound, row.bucket).await;

        // The filled store is the one the handlers serve: a seeded record reads back.
        let seeded_group = Aws::seeded_group(row.bound);
        let (status, answered) = aws.get_record(seeded_group, SEEDED_IDENTIFIER).await;
        assert_eq!(
            (status, answered.get("Record").is_some()),
            (200, true),
            "case {case:?}: the seeded record must be served from the online store, got \
             {status} with {answered}"
        );

        let mut entries = vec![entry(
            REFUSED_GROUP,
            "C1",
            POSTED_EVENT_TIME,
            row.target_stores,
        )];
        if row.with_accepted_entry {
            entries.push(entry(ACCEPTED_GROUP, "C2", POSTED_EVENT_TIME, None));
        }
        let (status, answered) = aws.batch_write_record(json!(entries)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from BatchWriteRecord, got {status} with \
             {answered}"
        );
        assert_eq!(
            answered["Errors"],
            json!([]),
            "case {case:?}: a capacity refusal is no entry error, got {answered}"
        );
        assert_eq!(
            reported(&answered, "UnprocessedEntries"),
            vec![(REFUSED_GROUP.to_owned(), "C1".to_owned())],
            "case {case:?}: expected exactly the refused entry reported unprocessed, got \
             {answered}"
        );

        // Nothing online for the refused entry.
        let (status, answered) = aws.get_record(REFUSED_GROUP, "C1").await;
        assert_eq!(
            (status, answered.get("Record")),
            (200, None),
            "case {case:?}: an unprocessed entry stores no online record, got {status} with \
             {answered}"
        );

        // …and nothing offline for it either: the only object in the store belongs to the
        // entry that WAS processed.
        let keys = aws.keys().await;
        let expected_objects = usize::from(row.with_accepted_entry);
        assert_eq!(
            keys.len(),
            expected_objects,
            "case {case:?}: expected {expected_objects} replicated object(s), one per \
             processed entry, got {keys:?}"
        );
        if row.with_accepted_entry {
            assert!(
                keys[0].contains(&format!("offline-store/{ACCEPTED_GROUP}-")),
                "case {case:?}: expected the one object under the accepted entry's group, \
                 got {keys:?}"
            );
            let (status, answered) = aws.get_record(ACCEPTED_GROUP, "C2").await;
            assert_eq!(
                (status, answered.get("Record").is_some()),
                (200, true),
                "case {case:?}: the accepted entry must be stored online, got {status} with \
                 {answered}"
            );
        }

        // The retry the response invites: the entry is refused again, and the offline store
        // is not appended to a second time for a record it never took a first.
        let (status, answered) = aws
            .batch_write_record(json!([entry(
                REFUSED_GROUP,
                "C1",
                POSTED_EVENT_TIME,
                row.target_stores
            )]))
            .await;
        assert_eq!(
            (status, reported(&answered, "UnprocessedEntries")),
            (200, vec![(REFUSED_GROUP.to_owned(), "C1".to_owned())]),
            "case {case:?}: the retried entry is refused again, got {status} with {answered}"
        );
        let retried = aws.keys().await;
        assert_eq!(
            retried, keys,
            "case {case:?}: the retry must append no offline object, got {retried:?} after \
             {keys:?}"
        );
    }
}

/// One row of the table below: an entry whose online side stores nothing for a reason that
/// is NOT a capacity refusal.
struct Accepted {
    case: &'static str,
    bucket: &'static str,
    /// The `TargetStores` the entry names.
    target_stores: Option<&'static [&'static str]>,
    /// The record the entry writes.
    identifier: &'static str,
    /// Its event time.
    event_time: &'static str,
}

/// RULE: only an entry the write could not TAKE is unprocessed, so an entry whose online
/// side stores nothing for another reason — it names only the OfflineStore, or it is
/// historic ("Otherwise, the record is a historic record and it is written only to the
/// OfflineStore") — is processed and replicated even into a feature group whose online
/// store is full.
///
/// The rows differ along the reason the online store holds nothing new for the entry: the
/// stores it names, and the ordering of its event time against the stored record's.
#[tokio::test]
async fn an_entry_the_online_store_does_not_take_is_still_replicated() {
    let rows: [Accepted; 3] = [
        Accepted {
            case: "an entry naming only the OfflineStore",
            bucket: "features-cap-offline-only",
            target_stores: Some(&["OfflineStore"]),
            identifier: "C1",
            event_time: POSTED_EVENT_TIME,
        },
        Accepted {
            case: "a historic entry of a record the full group already holds",
            bucket: "features-cap-historic",
            target_stores: None,
            identifier: SEEDED_IDENTIFIER,
            event_time: HISTORIC_EVENT_TIME,
        },
        Accepted {
            case: "a write of a record the full group already holds, with a later event time",
            bucket: "features-cap-overwrite",
            target_stores: None,
            identifier: SEEDED_IDENTIFIER,
            event_time: POSTED_EVENT_TIME,
        },
    ];

    for row in rows {
        let case = row.case;
        let aws = Aws::with_store_filled_to(Bound::RecordCount, row.bucket).await;
        let (status, answered) = aws
            .batch_write_record(json!([entry(
                REFUSED_GROUP,
                row.identifier,
                row.event_time,
                row.target_stores
            )]))
            .await;
        assert_eq!(
            (
                status,
                answered["Errors"].clone(),
                reported(&answered, "UnprocessedEntries")
            ),
            (200, json!([]), Vec::<(String, String)>::new()),
            "case {case:?}: the entry must be processed, got {status} with {answered}"
        );
        let keys = aws.keys().await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: expected the processed entry replicated once, got {keys:?}"
        );
        assert!(
            keys[0].contains(&format!("offline-store/{REFUSED_GROUP}-")),
            "case {case:?}: expected the object under the entry's own group, got {keys:?}"
        );
    }
}
