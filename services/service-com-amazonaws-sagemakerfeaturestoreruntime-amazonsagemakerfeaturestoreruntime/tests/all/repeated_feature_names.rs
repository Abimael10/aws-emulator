// SPDX-License-Identifier: CC0-1.0

//! A write that names the same feature more than once, driven over the wire.
//!
//! The rule under test: a feature of a record has ONE value. A `FeatureValue` is "The value
//! associated with a feature" carrying "The name of a feature that a feature value
//! corresponds to", and a read reports "the latest value for all the Features"
//! (`GetRecord$FeatureNames`) — while the `Record` a write carries is modelled `list` with
//! `length min 1` and NO `uniqueItems` (`smithy_query(query="shape", shape="Record")`), so a
//! request may legitimately name one feature twice. The value the record then holds for that
//! feature is the LAST the write names, so the record the OnlineStore holds carries each
//! feature once and the Parquet object replicated into the OfflineStore declares each column
//! once.
//!
//! The rows differ along what the rule generalises over: HOW MANY times a name is repeated,
//! WHICH feature is repeated (an ordinary one, the group's record-identifier feature, its
//! event-time feature), the FORM of the values a repeated name carries (`ValueAsString`, a
//! `ValueAsStringList`, one after the other) and WHICH operation writes the record
//! (`PutRecord`, `BatchWriteRecord`) — with a record whose names are all distinct as the
//! control row.
//!
//! The second rule under test is the other way one object could declare a column twice: the
//! three names Feature Store adds as columns to every offline row are RESERVED
//! ("FeatureNames cannot be any of the following: is_deleted, write_time,
//! api_invocation_time", `FeatureDefinitions`), so no feature group can declare a feature of
//! one of those names and every write of this data plane refuses a feature value posted under
//! one — while a name that merely resembles one is a feature like any other.
//!
//! Four services are mounted on ONE shared engine, exactly as the router mounts them: this
//! data plane, the SageMaker control plane that defines the feature group, IAM which holds
//! the role the offline write is made under, and Amazon S3 which holds the written object.
//! Every call goes through the real front door.

use std::sync::Arc;

use parquet::file::reader::FileReader;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{json, Value};
use smithy_server_runtime::{http, MultiServiceRouter};

use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::EmulatedService;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The host the requests name; the emulator serves one endpoint for every service.
const HOST: &str = "s3.us-east-1.amazonaws.com";

/// The feature group every row writes to, and its two key feature names.
const GROUP: &str = "customers";
const IDENTIFIER_FEATURE: &str = "customer_id";
const EVENT_TIME_FEATURE: &str = "event_time";

/// The bucket the group's offline store lives in, and the prefix inside it.
const BUCKET: &str = "features-repeated";
const S3_URI: &str = "s3://features-repeated/offline";

/// The name of the role's one inline policy, and the resource it grants the object write
/// on.
const OFFLINE_POLICY: &str = "offline-store-write";
const GRANTED_RESOURCE: &str = "arn:aws:s3:::features-repeated/*";

/// The three columns Feature Store adds to every offline record, in the order it adds them
/// ("FeatureNames cannot be any of the following: is_deleted, write_time,
/// api_invocation_time", `FeatureDefinitions`).
const ADDED_COLUMNS: [&str; 3] = ["write_time", "api_invocation_time", "is_deleted"];

/// An event time well in the future, so no row's record is already expired when it is read
/// back.
const EVENT_TIME: &str = "2035-01-01T00:00:00Z";

/// Feature names that merely RESEMBLE one of [`ADDED_COLUMNS`]: a feature group may declare
/// either, so a write may post a value under either.
const NEAR_MISS_FEATURES: [&str; 2] = ["is_deleted_by", "write_times"];

/// A second event time, for the row that names the event-time feature twice.
const LATER_EVENT_TIME: &str = "2036-02-03T04:05:06Z";

/// The four services on one shared engine.
struct Aws {
    router: MultiServiceRouter,
}

impl Aws {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One SageMaker control-plane call, as a client makes it.
    async fn sagemaker(&self, case: &str, target: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", HOST)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("SageMaker.{target}"))
            .header("authorization", authorization("sagemaker"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {target}, got {status} with {answered}"
        );
        answered
    }

    /// One request to this data plane, answering its status and parsed body.
    async fn data_plane(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", HOST)
            .header("content-type", "application/json")
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

    /// Write `record` through the operation `via` names, which must be served.
    async fn write_record(&self, case: &str, via: Via, record: &[Value]) {
        let (status, answered) = match via {
            Via::PutRecord => {
                self.data_plane(
                    "PUT",
                    &format!("/FeatureGroup/{GROUP}"),
                    Some(json!({ "Record": record })),
                )
                .await
            }
            Via::BatchWriteRecord => {
                let (status, answered) = self
                    .data_plane(
                        "POST",
                        "/BatchWriteRecord",
                        Some(json!({ "Entries": [
                            { "FeatureGroupName": GROUP, "Record": record }
                        ] })),
                    )
                    .await;
                let errors = answered
                    .get("Errors")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    errors.is_empty(),
                    "case {case:?}: expected the batch entry to be written, got {errors:?}"
                );
                (status, answered)
            }
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the write, got {status} with {answered}"
        );
    }

    /// `GET /FeatureGroup/<group>?…` — one `GetRecord`.
    async fn get_record(&self, identifier: &str) -> (u16, Value) {
        self.data_plane(
            "GET",
            &format!("/FeatureGroup/{GROUP}?RecordIdentifierValueAsString={identifier}"),
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
        let (status, body) = self.s3("PUT", &format!("/{BUCKET}")).await;
        assert_eq!(
            status,
            200,
            "the bucket {BUCKET} must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Every key the offline bucket holds, read through `ListObjectsV2`.
    async fn keys(&self) -> Vec<String> {
        let (status, body) = self.s3("GET", &format!("/{BUCKET}?list-type=2")).await;
        assert_eq!(
            status,
            200,
            "listing {BUCKET} must be served, got {status} {}",
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

    /// The bytes the offline bucket holds at `key`, read through `GetObject`.
    async fn object(&self, key: &str) -> Vec<u8> {
        let (status, body) = self.s3("GET", &format!("/{BUCKET}/{key}")).await;
        assert_eq!(
            status,
            200,
            "the object {key} must be served, got {status} {}",
            String::from_utf8_lossy(&body)
        );
        body.to_vec()
    }

    /// One IAM call (`awsQuery`) that must be served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        assert_eq!(
            status,
            200,
            "case {case:?}: {action} must be served, got {status} with {}",
            String::from_utf8_lossy(response.body())
        );
    }

    /// Hold the role the offline write is made under, granted that write, and answer its
    /// ARN.
    async fn create_granted_role(&self, case: &str, name: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "sagemaker.amazonaws.com" },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": GRANTED_RESOURCE,
            }],
        })
        .to_string();
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", OFFLINE_POLICY.to_string()),
                ("PolicyDocument", document),
            ],
        )
        .await;
        format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
    }

    /// Create the feature group, with both stores, through the control plane's own
    /// `CreateFeatureGroup`.
    async fn create_feature_group(&self, case: &str, role_arn: &str) {
        let body = json!({
            "FeatureGroupName": GROUP,
            "RecordIdentifierFeatureName": IDENTIFIER_FEATURE,
            "EventTimeFeatureName": EVENT_TIME_FEATURE,
            "FeatureDefinitions": [
                { "FeatureName": IDENTIFIER_FEATURE, "FeatureType": "String" },
                { "FeatureName": EVENT_TIME_FEATURE, "FeatureType": "String" },
                { "FeatureName": "city", "FeatureType": "String" },
                { "FeatureName": "tags", "FeatureType": "String" },
                // The two names that merely RESEMBLE a column Feature Store adds: a feature
                // group may declare either, which is what the control rows of the reserved
                // -name table below post.
                { "FeatureName": NEAR_MISS_FEATURES[0], "FeatureType": "String" },
                { "FeatureName": NEAR_MISS_FEATURES[1], "FeatureType": "String" },
            ],
            "OnlineStoreConfig": { "EnableOnlineStore": true },
            // `DisableGlueTableCreation`: this suite manages no Glue table.
            "OfflineStoreConfig": {
                "S3StorageConfig": { "S3Uri": S3_URI },
                "DisableGlueTableCreation": true,
            },
            "RoleArn": role_arn,
        });
        self.sagemaker(case, "CreateFeatureGroup", &body).await;
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

/// Percent-encode one `application/x-www-form-urlencoded` component with the maintained
/// RFC 3986 encoder, so a policy document's braces and quotes survive the round trip.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// One `FeatureValue` carrying a scalar.
fn feature(name: &str, value: &str) -> Value {
    json!({ "FeatureName": name, "ValueAsString": value })
}

/// One `FeatureValue` carrying a list, which is what a collection-typed feature carries.
fn list_feature(name: &str, values: &[&str]) -> Value {
    json!({ "FeatureName": name, "ValueAsStringList": values })
}

/// The `Record` member of a `GetRecord` response, as `(FeatureName, Value)` pairs where the
/// value is the scalar or the list the response carried.
fn record_pairs(body: &Value) -> Vec<(String, Value)> {
    body.get("Record")
        .and_then(Value::as_array)
        .map(|features| {
            features
                .iter()
                .map(|feature| {
                    let name = feature["FeatureName"].as_str().unwrap_or_default().to_owned();
                    let value = feature
                        .get("ValueAsString")
                        .or_else(|| feature.get("ValueAsStringList"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    (name, value)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Which operation writes a row's record. The same record is read the same way whichever
/// of the two whole-record writes carries it.
#[derive(Debug, Clone, Copy)]
enum Via {
    PutRecord,
    BatchWriteRecord,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The operation the record is written through.
    via: Via,
    /// The `Record` the write posts, names repeated and all.
    posted: Vec<Value>,
    /// The `RecordIdentifierValueAsString` the written record must be readable under.
    identifier: &'static str,
    /// An identifier the record must NOT be readable under: the value a repeated
    /// record-identifier feature superseded.
    superseded_identifier: Option<&'static str>,
    /// The `(FeatureName, value)` pairs the record must read back as, in order.
    expected: Vec<(&'static str, Value)>,
    /// Values no longer held, which neither the record nor the offline row may carry.
    superseded_values: Vec<&'static str>,
}

/// RULE: a feature of a record has one value — the LAST the write names for it — so a write
/// that names the same feature more than once stores that feature once, the record reads
/// back with one `FeatureValue` per name, and the Parquet object replicated into the
/// OfflineStore declares each column exactly once.
#[tokio::test]
async fn a_feature_named_twice_in_one_write_holds_the_last_value_named() {
    let rows = vec![
        Row {
            case: "a feature posted twice",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C1"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("city", "Seattle"),
                feature("city", "Boston"),
            ],
            identifier: "C1",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C1")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("city", json!("Boston")),
            ],
            superseded_values: vec!["Seattle"],
        },
        Row {
            case: "a feature posted three times",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C2"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("city", "Seattle"),
                feature("city", "Boston"),
                feature("city", "Austin"),
            ],
            identifier: "C2",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C2")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("city", json!("Austin")),
            ],
            superseded_values: vec!["Seattle", "Boston"],
        },
        Row {
            case: "the group's record-identifier feature posted twice",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C3first"),
                feature(IDENTIFIER_FEATURE, "C3last"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("city", "Seattle"),
            ],
            identifier: "C3last",
            superseded_identifier: Some("C3first"),
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C3last")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("city", json!("Seattle")),
            ],
            superseded_values: vec!["C3first"],
        },
        Row {
            case: "the group's event-time feature posted twice",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C4"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature(EVENT_TIME_FEATURE, LATER_EVENT_TIME),
                feature("city", "Seattle"),
            ],
            identifier: "C4",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C4")),
                (EVENT_TIME_FEATURE, json!(LATER_EVENT_TIME)),
                ("city", json!("Seattle")),
            ],
            superseded_values: vec![EVENT_TIME],
        },
        Row {
            case: "a scalar value superseded by a list value",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C5"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("tags", "scalarvalue"),
                list_feature("tags", &["listfirst", "listsecond"]),
            ],
            identifier: "C5",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C5")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("tags", json!(["listfirst", "listsecond"])),
            ],
            superseded_values: vec!["scalarvalue"],
        },
        Row {
            case: "a list value superseded by a scalar value",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C6"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                list_feature("tags", &["listfirst", "listsecond"]),
                feature("tags", "scalarvalue"),
            ],
            identifier: "C6",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C6")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("tags", json!("scalarvalue")),
            ],
            superseded_values: vec!["listfirst", "listsecond"],
        },
        Row {
            case: "a repeated feature written as one entry of a batch",
            via: Via::BatchWriteRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C7"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("city", "Seattle"),
                feature("city", "Boston"),
            ],
            identifier: "C7",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C7")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("city", json!("Boston")),
            ],
            superseded_values: vec!["Seattle"],
        },
        Row {
            case: "every feature posted once",
            via: Via::PutRecord,
            posted: vec![
                feature(IDENTIFIER_FEATURE, "C8"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("city", "Boston"),
                list_feature("tags", &["listfirst", "listsecond"]),
            ],
            identifier: "C8",
            superseded_identifier: None,
            expected: vec![
                (IDENTIFIER_FEATURE, json!("C8")),
                (EVENT_TIME_FEATURE, json!(EVENT_TIME)),
                ("city", json!("Boston")),
                ("tags", json!(["listfirst", "listsecond"])),
            ],
            superseded_values: vec![],
        },
    ];

    for row in rows {
        let case = row.case;
        // A fresh emulator per row, so each row's offline bucket holds only its own object.
        let aws = Aws::new();
        aws.create_bucket().await;
        let role_arn = aws.create_granted_role(case, "offline-store-role").await;
        aws.create_feature_group(case, &role_arn).await;

        aws.write_record(case, row.via, &row.posted).await;

        // The ONLINE record: one `FeatureValue` per name, each carrying the last value the
        // write named for it, in the order the names first appeared.
        let (status, body) = aws.get_record(row.identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the record to be readable under {:?}, got {status} with \
             {body}",
            row.identifier
        );
        let pairs = record_pairs(&body);
        let expected: Vec<(String, Value)> = row
            .expected
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        assert_eq!(
            pairs, expected,
            "case {case:?}: expected the record to read back as {expected:?}, got {pairs:?}"
        );

        // A value a later occurrence of the same name superseded is held nowhere.
        let rendered_record = body.to_string();
        for gone in &row.superseded_values {
            assert!(
                !rendered_record.contains(gone),
                "case {case:?}: expected the record not to carry the superseded value \
                 {gone:?}, got {rendered_record}"
            );
        }

        // A superseded record identifier identifies no record: the record's identifier is
        // the value its record-identifier feature HOLDS. "If no Record with
        // RecordIdentifierValue is found, then an empty result is returned" (`GetRecord`).
        if let Some(absent) = row.superseded_identifier {
            let (status, body) = aws.get_record(absent).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected an empty result under the superseded identifier \
                 {absent:?}, got {status} with {body}"
            );
            let held = body.get("Record").cloned().unwrap_or(Value::Null);
            assert!(
                held.is_null(),
                "case {case:?}: expected no record under the superseded identifier \
                 {absent:?}, got {held}"
            );
        }

        // The OFFLINE object: one Parquet column per feature name plus the three Feature
        // Store adds, and one row carrying the values the record holds.
        let keys = aws.keys().await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: expected one replicated object, got {keys:?}"
        );
        let object = aws.object(&keys[0]).await;
        let reader = parquet::file::reader::SerializedFileReader::new(bytes::Bytes::from(object))
            .expect("the replicated object must be a Parquet file");
        let metadata = reader.metadata().file_metadata().clone();
        let columns: Vec<String> = metadata
            .schema_descr()
            .columns()
            .iter()
            .map(|column| column.name().to_owned())
            .collect();
        let expected_columns: Vec<String> = row
            .expected
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .chain(ADDED_COLUMNS.iter().map(|name| (*name).to_owned()))
            .collect();
        assert_eq!(
            columns, expected_columns,
            "case {case:?}: expected the object's columns to be {expected_columns:?}, got \
             {columns:?}"
        );
        let rows_held: Vec<String> = reader
            .get_row_iter(None)
            .expect("the object's rows must be readable")
            .map(|held| held.expect("each row must be readable").to_string())
            .collect();
        assert_eq!(
            rows_held.len(),
            1,
            "case {case:?}: expected the object to hold the one written record, got \
             {rows_held:?}"
        );
        let rendered_row = &rows_held[0];
        for (name, value) in &row.expected {
            let held = match value {
                Value::Array(_) => value.to_string(),
                _ => value.as_str().unwrap_or_default().to_owned(),
            };
            assert!(
                rendered_row.contains(&held),
                "case {case:?}: expected the replicated row's {name} column to carry \
                 {held:?}, got {rendered_row}"
            );
        }
        for gone in &row.superseded_values {
            assert!(
                !rendered_row.contains(gone),
                "case {case:?}: expected the replicated row not to carry the superseded \
                 value {gone:?}, got {rendered_row}"
            );
        }
    }
}

/// Which write of this data plane posts a row's feature values.
#[derive(Debug, Clone, Copy)]
enum Write {
    Put,
    Batch,
    Update,
}

impl Write {
    /// The operation this write is, as the API names it.
    fn operation(self) -> &'static str {
        match self {
            Self::Put => "PutRecord",
            Self::Batch => "BatchWriteRecord",
            Self::Update => "UpdateRecord",
        }
    }
}

/// One row of the reserved-name table.
struct ReservedRow {
    case: String,
    /// The operation the features are posted through.
    via: Write,
    /// The name the row posts a feature value under, beside the record's own two features.
    posted_name: &'static str,
    /// Whether the write must be refused.
    refused: bool,
}

/// The record every row of that table seeds and then writes to, and the value it posts under
/// the name under test.
const SEEDED_IDENTIFIER: &str = "C1";
const POSTED_VALUE: &str = "postedvalue";

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

/// The message an error response carries, in either spelling: the framework's own refusal
/// uses `message`, while this service's modelled errors declare a `Message` member.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Post `features` through the write `via` names, answering its status and body. Nothing is
/// asserted: a row of the table below is expected to be refused.
async fn post_features(aws: &Aws, via: Write, features: &[Value]) -> (u16, Value) {
    match via {
        Write::Put => {
            aws.data_plane(
                "PUT",
                &format!("/FeatureGroup/{GROUP}"),
                Some(json!({ "Record": features })),
            )
            .await
        }
        Write::Batch => {
            aws.data_plane(
                "POST",
                "/BatchWriteRecord",
                Some(json!({ "Entries": [
                    { "FeatureGroupName": GROUP, "Record": features }
                ] })),
            )
            .await
        }
        Write::Update => {
            aws.data_plane(
                "POST",
                &format!("/FeatureGroup/{GROUP}/Record"),
                Some(json!({
                    "RecordIdentifierValueAsString": SEEDED_IDENTIFIER,
                    "Features": features,
                })),
            )
            .await
        }
    }
}

/// Whether the answer to one write is a REFUSAL, with the wire error code and the message it
/// carried.
///
/// A batch reports a refused entry in its own `Errors` element and answers HTTP 200 — "the
/// `ErrorCode` a batch operation of this data plane reports a refused entry with" is the code
/// a single-record write refuses the same request with — so the two are read here into one
/// verdict.
fn refusal(via: Write, status: u16, body: &Value) -> (bool, String, String) {
    if let Write::Batch = via {
        let errors = body
            .get("Errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        return match errors.first() {
            Some(error) => (
                true,
                error["ErrorCode"].as_str().unwrap_or_default().to_owned(),
                error["ErrorMessage"].as_str().unwrap_or_default().to_owned(),
            ),
            None => (false, String::new(), String::new()),
        };
    }
    (
        status != 200,
        error_code(body),
        error_message(body),
    )
}

/// The columns a replicated Parquet object declares, in the order it declares them.
fn object_columns(bytes: Vec<u8>) -> Vec<String> {
    let reader = parquet::file::reader::SerializedFileReader::new(bytes::Bytes::from(bytes))
        .expect("the replicated object must be a Parquet file");
    reader
        .metadata()
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .map(|column| column.name().to_owned())
        .collect()
}

/// RULE: Feature Store RESERVES the three names it adds as columns to every offline row
/// ("FeatureNames cannot be any of the following: is_deleted, write_time,
/// api_invocation_time", `FeatureDefinitions`), so no feature group can declare a feature of
/// one of those names and a write that posts a feature value under one is refused with the
/// modelled `ValidationError` — nothing stored in the OnlineStore and nothing appended to the
/// OfflineStore — while a name that merely resembles one is a feature like any other. Every
/// object the offline store does hold therefore declares each column exactly once, so the
/// `is_deleted` column that marks a row deleted is identifiable by name.
///
/// The rows differ along WHICH of the three reserved names is posted, WHICH operation posts
/// it (`PutRecord`, `BatchWriteRecord`, `UpdateRecord`) and, as the control rows, names that
/// only resemble a reserved one — which the feature group declares.
#[tokio::test]
async fn a_feature_posted_under_a_name_feature_store_reserves_is_refused() {
    let mut rows: Vec<ReservedRow> = Vec::new();
    for via in [Write::Put, Write::Batch, Write::Update] {
        for posted_name in ADDED_COLUMNS {
            rows.push(ReservedRow {
                case: format!("{posted_name} posted by {}", via.operation()),
                via,
                posted_name,
                refused: true,
            });
        }
        for posted_name in NEAR_MISS_FEATURES {
            rows.push(ReservedRow {
                case: format!("{posted_name} posted by {}", via.operation()),
                via,
                posted_name,
                refused: false,
            });
        }
    }

    for row in rows {
        let case = row.case.as_str();
        // A fresh emulator per row, so each row's offline bucket holds only its own objects.
        let aws = Aws::new();
        aws.create_bucket().await;
        let role_arn = aws.create_granted_role(case, "offline-store-role").await;
        aws.create_feature_group(case, &role_arn).await;

        // The record the row writes to: it is what an `UpdateRecord` needs to exist, and the
        // one object the offline store holds before the row's own write.
        aws.write_record(
            case,
            Via::PutRecord,
            &[
                feature(IDENTIFIER_FEATURE, SEEDED_IDENTIFIER),
                feature(EVENT_TIME_FEATURE, EVENT_TIME),
                feature("city", "Seattle"),
            ],
        )
        .await;
        let seeded_keys = aws.keys().await;
        assert_eq!(
            seeded_keys.len(),
            1,
            "case {case:?}: expected the seeded record to be the one replicated object, got \
             {seeded_keys:?}"
        );

        let posted = vec![
            feature(IDENTIFIER_FEATURE, SEEDED_IDENTIFIER),
            // Later than the seeded record's event time, so an accepted write is not a
            // historic one that only the OfflineStore would take.
            feature(EVENT_TIME_FEATURE, LATER_EVENT_TIME),
            feature(row.posted_name, POSTED_VALUE),
        ];
        let (status, body) = post_features(&aws, row.via, &posted).await;
        let (refused, code, message) = refusal(row.via, status, &body);
        assert_eq!(
            refused, row.refused,
            "case {case:?}: expected refused={}, got HTTP {status} with {body}",
            row.refused
        );
        if row.refused {
            assert_eq!(
                code, "ValidationError",
                "case {case:?}: expected the refusal to be a ValidationError, got {code:?} \
                 with {body}"
            );
            assert!(
                message.contains(row.posted_name),
                "case {case:?}: expected the refusal to name the posted feature {:?}, got \
                 {message:?}",
                row.posted_name
            );
            assert!(
                message.contains(GROUP),
                "case {case:?}: expected the refusal to name the feature group {GROUP:?}, got \
                 {message:?}"
            );
        }

        // The ONLINE record: the refused write left it alone, so it carries no feature of the
        // reserved name; the accepted write stored the posted feature.
        let (status, served) = aws.get_record(SEEDED_IDENTIFIER).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the record to be readable, got {status} with {served}"
        );
        let held: Vec<String> = record_pairs(&served)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            held.contains(&row.posted_name.to_owned()),
            !row.refused,
            "case {case:?}: expected the record to hold {:?} only when the write was served, \
             got {held:?}",
            row.posted_name
        );

        // The OFFLINE store: the refused write appended nothing, the accepted one appended one
        // object — and every object it holds declares each column exactly once.
        let keys = aws.keys().await;
        let expected_objects = if row.refused { 1 } else { 2 };
        assert_eq!(
            keys.len(),
            expected_objects,
            "case {case:?}: expected {expected_objects} replicated object(s), got {keys:?}"
        );
        for key in &keys {
            let columns = object_columns(aws.object(key).await);
            let mut once = columns.clone();
            once.sort_unstable();
            once.dedup();
            assert_eq!(
                once.len(),
                columns.len(),
                "case {case:?}: expected the object {key} to declare each column once, got \
                 {columns:?}"
            );
            for added in ADDED_COLUMNS {
                assert_eq!(
                    columns.iter().filter(|column| *column == added).count(),
                    1,
                    "case {case:?}: expected one {added} column in {key}, got {columns:?}"
                );
            }
        }
        if !row.refused {
            let appended = keys
                .iter()
                .find(|key| !seeded_keys.contains(key))
                .expect("the accepted write appended an object");
            let columns = object_columns(aws.object(appended).await);
            assert!(
                columns.contains(&row.posted_name.to_owned()),
                "case {case:?}: expected the appended object to declare the posted feature \
                 {:?}, got {columns:?}",
                row.posted_name
            );
        }
    }
}
