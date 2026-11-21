// SPDX-License-Identifier: CC0-1.0

//! The OfflineStore side of the Feature Store data plane, driven over the wire.
//!
//! The rule under test: a record ingested into a feature group the SageMaker control plane
//! gave an `OfflineStoreConfig` is WRITTEN into that store — "If a new record's EventTime
//! is greater, the new record is written to both the OnlineStore and OfflineStore.
//! Otherwise, the record is a historic record and it is written only to the OfflineStore"
//! (`PutRecord`) — as an object under the output path the group resolved
//! (`S3StorageConfig$ResolvedOutputS3Uri`, "The S3 path where offline records are
//! written"), partitioned by the record's own event time, and the write is made under the
//! group's `RoleArn` ("the IAM execution role used to persist data into the OfflineStore"),
//! so a role whose permissions withhold `s3:PutObject` there retains nothing and the group
//! reports `OfflineStoreStatus` `Blocked` with a `BlockedReason` until the permission is
//! restored.
//!
//! A DELETION is replicated into the same store, under the same role — "In both cases,
//! Feature Store appends the deleted record marker to the OfflineStore" (`DeleteRecord`) — so
//! the store is APPEND-ONLY either way: every write this data plane accepts adds one more
//! object and changes none of the objects already there.
//!
//! The four services the rule spans are mounted on ONE shared engine, exactly as the router
//! mounts them: this data plane, the SageMaker control plane that defines the group, IAM
//! which holds the role and answers what it may do, and Amazon S3 which holds the objects.
//! Every call goes through the real front door — the control-plane calls as `awsJson1_1`
//! with `X-Amz-Target`, the ingestions as this service's own restJson1 requests, the IAM
//! calls as `awsQuery` form posts and the destination reads as the path-style Amazon S3
//! requests a client sends.

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

/// The group's record-identifier feature.
const IDENTIFIER_FEATURE: &str = "customer_id";

/// The group's event-time feature.
const EVENT_TIME_FEATURE: &str = "event_time";

/// A feature carrying the record's payload, so an object's bytes can be told apart from a
/// placeholder.
const PAYLOAD_FEATURE: &str = "payload";

/// The IAM action an object write is authorized under, as the rule names it.
const WRITE_ACTION: &str = "s3:PutObject";

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
        let answered: Value =
            serde_json::from_slice(response.body()).unwrap_or(Value::Null);
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

    /// `PUT /FeatureGroup/<group>` — one `PutRecord` that must be served.
    async fn put_record(&self, case: &str, group: &str, record: Value) {
        let (status, answered) = self
            .data_plane("PUT", &format!("/FeatureGroup/{group}"), Some(record))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an empty HTTP 200 from PutRecord, got {status} with \
             {answered}"
        );
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

    /// `DELETE /FeatureGroup/<group>?…` — one `DeleteRecord`, answering its status and body.
    async fn delete_record(&self, group: &str, query: &str) -> (u16, Value) {
        self.data_plane("DELETE", &format!("/FeatureGroup/{group}?{query}"), None)
            .await
    }

    /// One path-style Amazon S3 request.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> (u16, bytes::Bytes) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{HOST}{path}"))
            .header("host", HOST)
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        (parts.status.as_u16(), body)
    }

    /// `PUT /<bucket>` — Amazon S3's `CreateBucket`.
    async fn create_bucket(&self, bucket: &str) {
        let (status, body) = self.s3("PUT", &format!("/{bucket}"), b"").await;
        assert_eq!(
            status,
            200,
            "the bucket {bucket} must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Every key `bucket` holds, read through `ListObjectsV2`.
    async fn keys(&self, bucket: &str) -> Vec<String> {
        let (status, body) = self.s3("GET", &format!("/{bucket}?list-type=2"), b"").await;
        assert_eq!(
            status,
            200,
            "listing {bucket} must be served, got {status} {}",
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

    /// The bytes `bucket` holds at `key`, read through `GetObject`.
    async fn object(&self, bucket: &str, key: &str) -> Vec<u8> {
        let (status, body) = self.s3("GET", &format!("/{bucket}/{key}"), b"").await;
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

    /// Hold the role `name` in IAM, trusted by the SageMaker service principal, and answer
    /// its ARN.
    async fn create_role(&self, case: &str, name: &str) -> String {
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
        format!(
            "arn:aws:iam::{}:role/{name}",
            engine::emulator_account_id()
        )
    }

    /// Grant the role `name` the object write on `resource`, as its one inline policy.
    async fn grant_write(&self, case: &str, name: &str, resource: &str) {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": WRITE_ACTION,
                "Resource": resource,
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
    }

    /// Take that grant away again, the way a client revokes a permission.
    async fn revoke_write(&self, case: &str, name: &str) {
        self.iam(
            case,
            "DeleteRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", OFFLINE_POLICY.to_string()),
            ],
        )
        .await;
    }

    /// Create the feature group `group`, with the offline store `s3_uri` names where the
    /// case has one, and answer what `DescribeFeatureGroup` reports for it.
    async fn create_feature_group(
        &self,
        case: &str,
        group: &str,
        online: bool,
        offline: Option<(&str, &str)>,
    ) -> Value {
        let mut body = json!({
            "FeatureGroupName": group,
            "RecordIdentifierFeatureName": IDENTIFIER_FEATURE,
            "EventTimeFeatureName": EVENT_TIME_FEATURE,
            "FeatureDefinitions": [
                { "FeatureName": IDENTIFIER_FEATURE, "FeatureType": "String" },
                { "FeatureName": EVENT_TIME_FEATURE, "FeatureType": "String" },
                { "FeatureName": PAYLOAD_FEATURE, "FeatureType": "String" },
            ],
        });
        let object = body.as_object_mut().expect("a JSON object body");
        if online {
            object.insert(
                "OnlineStoreConfig".to_string(),
                json!({ "EnableOnlineStore": true }),
            );
        }
        if let Some((s3_uri, role_arn)) = offline {
            object.insert(
                "OfflineStoreConfig".to_string(),
                // `DisableGlueTableCreation`: this suite manages no Glue table, which is
                // what the recorded client asks for too.
                json!({
                    "S3StorageConfig": { "S3Uri": s3_uri },
                    "DisableGlueTableCreation": true,
                }),
            );
            object.insert("RoleArn".to_string(), json!(role_arn));
        }
        self.sagemaker(case, "CreateFeatureGroup", &body).await;
        self.describe_feature_group(case, group).await
    }

    /// `DescribeFeatureGroup` for `group`.
    async fn describe_feature_group(&self, case: &str, group: &str) -> Value {
        self.sagemaker(
            case,
            "DescribeFeatureGroup",
            &json!({ "FeatureGroupName": group }),
        )
        .await
    }
}

/// The name of the role's one inline policy.
const OFFLINE_POLICY: &str = "offline-store-write";

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
/// RFC 3986 encoder, leaving only the unreserved characters literal — so a policy
/// document's braces and quotes survive the round trip.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// One record, as a `PutRecord` body carries it.
fn record(identifier: &str, event_time: &str, payload: &str) -> Value {
    json!({
        "Record": [
            { "FeatureName": IDENTIFIER_FEATURE, "ValueAsString": identifier },
            { "FeatureName": EVENT_TIME_FEATURE, "ValueAsString": event_time },
            { "FeatureName": PAYLOAD_FEATURE, "ValueAsString": payload },
        ],
    })
}

/// The `S3StorageConfig.ResolvedOutputS3Uri` a describe reports, as the KEY it names inside
/// its bucket (the output path below the bucket).
fn resolved_key(described: &Value) -> String {
    let uri = described
        .pointer("/OfflineStoreConfig/S3StorageConfig/ResolvedOutputS3Uri")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let rest = uri.strip_prefix("s3://").unwrap_or(uri);
    rest.split_once('/')
        .map(|(_bucket, key)| key.to_string())
        .unwrap_or_default()
}

/// The `OfflineStoreStatus` a describe reports, as `(Status, BlockedReason)`.
fn offline_status(described: &Value) -> (Option<String>, Option<String>) {
    let status = described.get("OfflineStoreStatus");
    (
        status
            .and_then(|held| held.get("Status"))
            .and_then(Value::as_str)
            .map(str::to_string),
        status
            .and_then(|held| held.get("BlockedReason"))
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// The rows a Parquet object holds, rendered as text — enough to assert that the object
/// carries the record that was ingested rather than a placeholder.
fn parquet_rows(bytes: Vec<u8>) -> Vec<String> {
    let reader =
        parquet::file::reader::SerializedFileReader::new(bytes::Bytes::from(bytes))
            .expect("the written object must be a Parquet file");
    reader
        .get_row_iter(None)
        .expect("the object's rows must be readable")
        .map(|row| row.expect("each row must be readable").to_string())
        .collect()
}

/// One row of the replication table.
struct Row {
    case: &'static str,
    /// The feature group the record is ingested into.
    group: &'static str,
    /// The bucket its offline store lives in; a row's own, so its listing holds only its
    /// own objects.
    bucket: &'static str,
    /// The `S3Uri` the create names, or `None` for a group with no offline store at all.
    s3_uri: Option<&'static str>,
    /// The resource the role's inline policy grants the object write on, or `None` for a
    /// role carrying no policy at all.
    grant: Option<&'static str>,
    /// Whether the group also keeps an OnlineStore.
    online: bool,
    /// The record's event time.
    event_time: &'static str,
    /// The partition the written object's key must carry, derived from that event time.
    partition: &'static str,
    /// Whether the record must be retained in the offline store.
    replicated: bool,
    /// The `OfflineStoreStatus.Status` the describe must report, and `None` for a group
    /// with no offline store.
    status: Option<&'static str>,
}

/// RULE: a record ingested into a feature group with an `OfflineStoreConfig` is written as
/// an object under the group's resolved output path, in the partition of its own event
/// time, when the group's role is permitted the object write there — and nothing is written,
/// with the group reporting `Blocked`, when it is not.
///
/// The rows differ along what the rule generalises over: the SHAPE of the configured
/// location (a bucket with a prefix, a nested prefix, a bucket with no prefix), WHICH
/// resource the role's grant names (the whole bucket, exactly the configured prefix, another
/// bucket, no policy at all), WHICH stores the group keeps (both, offline only, online only)
/// and the record's EVENT TIME, which decides the partition.
#[tokio::test]
async fn an_ingested_record_is_replicated_into_the_offline_store_its_role_may_write() {
    let rows = [
        Row {
            case: "granted on the whole bucket",
            group: "offlinegranted",
            bucket: "features-granted",
            s3_uri: Some("s3://features-granted/offline"),
            grant: Some("arn:aws:s3:::features-granted/*"),
            online: true,
            event_time: "2026-06-01T07:08:09Z",
            partition: "year=2026/month=06/day=01/hour=07",
            replicated: true,
            status: Some("Active"),
        },
        Row {
            case: "granted on the configured prefix only, with a nested prefix",
            group: "offlinenested",
            bucket: "features-nested",
            s3_uri: Some("s3://features-nested/offline/store/"),
            grant: Some("arn:aws:s3:::features-nested/offline/store/*"),
            online: true,
            event_time: "2027-01-01T00:00:00Z",
            partition: "year=2027/month=01/day=01/hour=00",
            replicated: true,
            status: Some("Active"),
        },
        Row {
            case: "a location with no prefix at all",
            group: "offlinerootlocation",
            bucket: "features-root",
            s3_uri: Some("s3://features-root"),
            grant: Some("arn:aws:s3:::features-root/*"),
            online: true,
            event_time: "2026-02-28T23:59:59Z",
            partition: "year=2026/month=02/day=28/hour=23",
            replicated: true,
            status: Some("Active"),
        },
        Row {
            case: "a group that keeps no online store",
            group: "offlineonly",
            bucket: "features-offline-only",
            s3_uri: Some("s3://features-offline-only/offline"),
            grant: Some("arn:aws:s3:::features-offline-only/*"),
            online: false,
            event_time: "2026-06-01T07:08:09Z",
            partition: "year=2026/month=06/day=01/hour=07",
            replicated: true,
            status: Some("Active"),
        },
        Row {
            case: "a role carrying no policy at all",
            group: "offlineungranted",
            bucket: "features-ungranted",
            s3_uri: Some("s3://features-ungranted/offline"),
            grant: None,
            online: true,
            event_time: "2026-06-01T07:08:09Z",
            partition: "year=2026/month=06/day=01/hour=07",
            replicated: false,
            status: Some("Blocked"),
        },
        Row {
            case: "a role granted on another bucket",
            group: "offlineelsewhere",
            bucket: "features-elsewhere",
            s3_uri: Some("s3://features-elsewhere/offline"),
            grant: Some("arn:aws:s3:::somewhere-else/*"),
            online: true,
            event_time: "2026-06-01T07:08:09Z",
            partition: "year=2026/month=06/day=01/hour=07",
            replicated: false,
            status: Some("Blocked"),
        },
        Row {
            case: "a group with no offline store",
            group: "onlineonly",
            bucket: "features-online-only",
            s3_uri: None,
            grant: Some("arn:aws:s3:::features-online-only/*"),
            online: true,
            event_time: "2026-06-01T07:08:09Z",
            partition: "year=2026/month=06/day=01/hour=07",
            replicated: false,
            status: None,
        },
    ];

    for row in rows {
        let aws = Aws::new();
        let case = row.case;
        aws.create_bucket(row.bucket).await;
        let role_name = format!("{}-role", row.group);
        let role_arn = aws.create_role(case, &role_name).await;
        if let Some(resource) = row.grant {
            aws.grant_write(case, &role_name, resource).await;
        }
        let described = aws
            .create_feature_group(
                case,
                row.group,
                row.online,
                row.s3_uri.map(|s3_uri| (s3_uri, role_arn.as_str())),
            )
            .await;

        // The status the group reports, and the reason a blocked one is reported with.
        let (status, reason) = offline_status(&described);
        assert_eq!(
            status.as_deref(),
            row.status,
            "case {case:?}: expected OfflineStoreStatus {:?}, got {described}",
            row.status
        );
        if row.status == Some("Blocked") {
            let reason = reason.unwrap_or_default();
            for expected in [role_arn.as_str(), WRITE_ACTION] {
                assert!(
                    reason.contains(expected),
                    "case {case:?}: expected the blocked reason to name {expected:?}, got \
                     {reason:?}"
                );
            }
        } else {
            assert_eq!(
                reason, None,
                "case {case:?}: expected no BlockedReason, got {reason:?}"
            );
        }

        aws.put_record(case, row.group, record("C1", row.event_time, "first"))
            .await;

        let keys = aws.keys(row.bucket).await;
        if !row.replicated {
            assert!(
                keys.is_empty(),
                "case {case:?}: expected nothing retained in the offline store, got {keys:?}"
            );
        } else {
            assert_eq!(
                keys.len(),
                1,
                "case {case:?}: expected one replicated object, got {keys:?}"
            );
            let key = &keys[0];
            let expected_prefix = {
                let resolved = resolved_key(&described);
                if resolved.is_empty() {
                    format!("{}/", row.partition)
                } else {
                    format!("{resolved}/{}/", row.partition)
                }
            };
            assert!(
                key.starts_with(&expected_prefix) && key.ends_with(".parquet"),
                "case {case:?}: expected a Parquet object under {expected_prefix:?}, got \
                 {key:?}"
            );
            let body = aws.object(row.bucket, key).await;
            assert!(
                !body.is_empty(),
                "case {case:?}: expected a non-empty object at {key:?}"
            );
            let rows_held = parquet_rows(body);
            assert_eq!(
                rows_held.len(),
                1,
                "case {case:?}: expected the object to hold the one ingested record, got \
                 {rows_held:?}"
            );
            for expected in ["C1", row.event_time, "first"] {
                assert!(
                    rows_held[0].contains(expected),
                    "case {case:?}: expected the replicated row to carry {expected:?}, got \
                     {:?}",
                    rows_held[0]
                );
            }
        }

        // The ONLINE store is untouched by any of this: a group that keeps one still serves
        // the record it took, and a group that keeps none serves nothing.
        let (status, answered) = aws.get_record(row.group, "C1").await;
        if row.online {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the online store to serve the record, got {status} \
                 with {answered}"
            );
            let values: Vec<String> = answered["Record"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|feature| feature["ValueAsString"].as_str().unwrap_or_default().to_string())
                .collect();
            assert!(
                values.contains(&"first".to_string()),
                "case {case:?}: expected the ingested payload to read back, got {answered}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: a group with no OnlineStore has no record to serve, got \
                 {status} with {answered}"
            );
        }
    }
}

/// RULE: the status is the permission IN FORCE, so a group whose role loses the object write
/// reports `Blocked` with a `BlockedReason` and retains nothing more, and reports `Active`
/// again — retaining what is ingested after it — once the permission is restored.
#[tokio::test]
async fn revoking_and_restoring_the_roles_permission_moves_the_offline_store_status() {
    let case = "a permission revoked and restored";
    let bucket = "features-revoked";
    let group = "offlinerevoked";
    let aws = Aws::new();
    aws.create_bucket(bucket).await;
    let role_name = format!("{group}-role");
    let role_arn = aws.create_role(case, &role_name).await;
    let resource = format!("arn:aws:s3:::{bucket}/*");
    aws.grant_write(case, &role_name, &resource).await;
    let described = aws
        .create_feature_group(
            case,
            group,
            true,
            Some((&format!("s3://{bucket}/offline"), role_arn.as_str())),
        )
        .await;
    assert_eq!(
        offline_status(&described).0.as_deref(),
        Some("Active"),
        "case {case:?}: a granted role's offline store is Active, got {described}"
    );

    aws.put_record(case, group, record("C1", "2026-06-01T07:08:09Z", "first"))
        .await;
    assert_eq!(
        aws.keys(bucket).await.len(),
        1,
        "case {case:?}: the first ingestion must be retained"
    );

    // The permission goes away. Nothing else changes: the group, the role and the records
    // already written stay exactly as they are.
    aws.revoke_write(case, &role_name).await;
    let described = aws.describe_feature_group(case, group).await;
    let (status, reason) = offline_status(&described);
    assert_eq!(
        status.as_deref(),
        Some("Blocked"),
        "case {case:?}: a revoked permission blocks the offline store, got {described}"
    );
    let reason = reason.unwrap_or_default();
    for expected in [role_arn.as_str(), WRITE_ACTION] {
        assert!(
            reason.contains(expected),
            "case {case:?}: expected the blocked reason to name {expected:?}, got {reason:?}"
        );
    }
    aws.put_record(case, group, record("C2", "2026-06-02T07:08:09Z", "withheld"))
        .await;
    let keys = aws.keys(bucket).await;
    assert_eq!(
        keys.len(),
        1,
        "case {case:?}: an ingestion the role may not write retains nothing, got {keys:?}"
    );

    // And back again.
    aws.grant_write(case, &role_name, &resource).await;
    let described = aws.describe_feature_group(case, group).await;
    let (status, reason) = offline_status(&described);
    assert_eq!(
        (status.as_deref(), reason),
        (Some("Active"), None),
        "case {case:?}: a restored permission returns the offline store to Active, got \
         {described}"
    );
    aws.put_record(case, group, record("C3", "2026-06-03T07:08:09Z", "again"))
        .await;
    let keys = aws.keys(bucket).await;
    assert_eq!(
        keys.len(),
        2,
        "case {case:?}: an ingestion made while the permission is back is retained, got \
         {keys:?}"
    );
}

/// RULE: the same replication is made by the BATCH write, whose entries are ingested exactly
/// as the same records written on their own — including a historic entry, which the
/// OfflineStore takes and the OnlineStore leaves standing.
#[tokio::test]
async fn a_batch_write_replicates_every_entry_including_a_historic_one() {
    let case = "a batch of three entries, one historic";
    let bucket = "features-batched";
    let group = "offlinebatched";
    let aws = Aws::new();
    aws.create_bucket(bucket).await;
    let role_name = format!("{group}-role");
    let role_arn = aws.create_role(case, &role_name).await;
    aws.grant_write(case, &role_name, &format!("arn:aws:s3:::{bucket}/*"))
        .await;
    let described = aws
        .create_feature_group(
            case,
            group,
            true,
            Some((&format!("s3://{bucket}/offline"), role_arn.as_str())),
        )
        .await;
    let resolved = resolved_key(&described);

    // The newest record of C1 is written first, so the third entry below is HISTORIC: the
    // online store keeps the newer values and the offline store takes the older record all
    // the same.
    let entries = json!({
        "Entries": [
            { "FeatureGroupName": group, "Record": record("C1", "2026-06-01T00:00:00Z", "newer")["Record"] },
            { "FeatureGroupName": group, "Record": record("C2", "2026-07-02T13:00:00Z", "other")["Record"] },
            { "FeatureGroupName": group, "Record": record("C1", "2026-01-01T00:00:00Z", "historic")["Record"] },
        ],
    });
    let (status, answered) = aws
        .data_plane("POST", "/BatchWriteRecord", Some(entries))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from BatchWriteRecord, got {status} with {answered}"
    );
    assert_eq!(
        (
            answered["Errors"].as_array().map(Vec::len),
            answered["UnprocessedEntries"].as_array().map(Vec::len)
        ),
        (Some(0), Some(0)),
        "case {case:?}: every entry must be written, got {answered}"
    );

    let keys = aws.keys(bucket).await;
    assert_eq!(
        keys.len(),
        3,
        "case {case:?}: expected one replicated object per entry, got {keys:?}"
    );
    // Each entry lands in the partition of its OWN event time, the historic one included.
    for partition in [
        "year=2026/month=06/day=01/hour=00",
        "year=2026/month=07/day=02/hour=13",
        "year=2026/month=01/day=01/hour=00",
    ] {
        let expected_prefix = format!("{resolved}/{partition}/");
        assert!(
            keys.iter().any(|key| key.starts_with(&expected_prefix)),
            "case {case:?}: expected an object under {expected_prefix:?}, got {keys:?}"
        );
    }

    // The online store kept the NEWER record of C1: the historic entry is written only to
    // the offline store.
    let (status, answered) = aws.get_record(group, "C1").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the online store to serve C1, got {status} with {answered}"
    );
    let payload: Vec<String> = answered["Record"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|feature| feature["FeatureName"] == json!(PAYLOAD_FEATURE))
        .map(|feature| feature["ValueAsString"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        payload,
        vec!["newer".to_string()],
        "case {case:?}: the historic entry must not overwrite the online record, got \
         {answered}"
    );
}

/// The event time the record seeded before a deletion run carries.
const SEED_EVENT_TIME: &str = "2026-06-01T07:08:09Z";

/// The payload that seeded record carries, so an INGESTED row can be told from a marker.
const SEEDED_PAYLOAD: &str = "seeded";

/// The `is_deleted` column of a DELETE MARKER row, as a Parquet row renders it.
const DELETED_COLUMN_TRUE: &str = "is_deleted: true";

/// The `is_deleted` column of an ingested row; see [`DELETED_COLUMN_TRUE`].
const DELETED_COLUMN_FALSE: &str = "is_deleted: false";

/// One deletion of a run.
struct Deletion {
    /// The `DeletionMode` the request carries, as the query string spells it — empty for the
    /// defaulted mode, which is `SoftDelete`.
    mode: &'static str,
    /// The record the deletion names.
    identifier: &'static str,
    /// The `EventTime` it carries.
    event_time: &'static str,
    /// The status it must answer.
    status: u16,
    /// The partition the marker it appends must land in, and `None` for a deletion that
    /// appends none.
    partition: Option<&'static str>,
}

/// One row of the deletion-run table: a feature group, the records seeded into it and the
/// deletions then made against it, in order.
struct Run {
    case: &'static str,
    /// The feature group the deletions are made against.
    group: &'static str,
    /// The bucket its offline store lives in; a row's own, so its listing holds only its
    /// own objects.
    bucket: &'static str,
    /// Whether the group keeps an OnlineStore beside its OfflineStore.
    online: bool,
    /// The records ingested before the run, each at [`SEED_EVENT_TIME`].
    seeded: &'static [&'static str],
    /// The deletions, made in this order.
    deletions: &'static [Deletion],
}

/// RULE: a deletion of this data plane is replicated into the feature group's offline store
/// just as an ingestion is — "In both cases, Feature Store appends the deleted record marker
/// to the OfflineStore" (`DeleteRecord`) — so every deletion the service accepts APPENDS one
/// more object, under the group's resolved output path, in the partition of the deletion's own
/// `EventTime`, holding the marker of the record it names, and changes no object already in
/// the store; a deletion the service refuses appends none.
///
/// The rows differ along what the rule generalises over: the NUMBER of deletions made in a
/// run, whether their event times fall in one partition or several, the deletion MODES they
/// mix, how many distinct records they name, a refused deletion in the middle of a run, and
/// whether the feature group keeps an OnlineStore at all.
#[tokio::test]
async fn every_deletion_the_service_accepts_appends_one_more_marker() {
    let runs: [Run; 6] = [
        Run {
            case: "two deletions of one record, in different hours",
            group: "deleterising",
            bucket: "features-delete-rising",
            online: true,
            seeded: &["C1"],
            deletions: &[
                Deletion {
                    mode: "",
                    identifier: "C1",
                    event_time: "2026-06-02T08:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C1",
                    event_time: "2026-06-02T09:30:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=09"),
                },
            ],
        },
        Run {
            case: "two deletions landing in the same partition",
            group: "deletesamehour",
            bucket: "features-delete-same-hour",
            online: true,
            seeded: &["C1"],
            deletions: &[
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C1",
                    event_time: "2026-06-02T08:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C1",
                    event_time: "2026-06-02T08:45:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
            ],
        },
        Run {
            case: "a soft delete followed by a hard delete of the same record",
            group: "deletesoftthenhard",
            bucket: "features-delete-soft-then-hard",
            online: true,
            seeded: &["C1"],
            deletions: &[
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C1",
                    event_time: "2026-06-02T08:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
                Deletion {
                    mode: "&DeletionMode=HardDelete",
                    identifier: "C1",
                    event_time: "2026-06-03T09:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=03/hour=09"),
                },
            ],
        },
        Run {
            case: "deletions naming three different records",
            group: "deletethree",
            bucket: "features-delete-three",
            online: true,
            seeded: &["C1", "C2", "C3"],
            deletions: &[
                Deletion {
                    mode: "",
                    identifier: "C1",
                    event_time: "2026-06-02T08:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
                Deletion {
                    mode: "&DeletionMode=HardDelete",
                    identifier: "C2",
                    event_time: "2026-07-02T13:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=07/day=02/hour=13"),
                },
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C3",
                    event_time: "2026-12-31T23:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=12/day=31/hour=23"),
                },
            ],
        },
        Run {
            case: "a refused hard delete between two accepted deletions",
            group: "deleterefusedbetween",
            bucket: "features-delete-refused-between",
            online: true,
            seeded: &["C1"],
            deletions: &[
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C1",
                    event_time: "2026-06-02T08:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
                // "HardDelete returns EventTime: 400 ValidationException … No delete record
                // marker is written to the OfflineStore."
                Deletion {
                    mode: "&DeletionMode=HardDelete",
                    identifier: "C1",
                    event_time: "2026-01-01T00:00:00Z",
                    status: 400,
                    partition: None,
                },
                Deletion {
                    mode: "&DeletionMode=HardDelete",
                    identifier: "C1",
                    event_time: "2026-06-03T09:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=03/hour=09"),
                },
            ],
        },
        Run {
            case: "a feature group with no OnlineStore at all",
            group: "deleteofflineonly",
            bucket: "features-delete-offline-only",
            online: false,
            seeded: &["C1"],
            deletions: &[
                Deletion {
                    mode: "",
                    identifier: "C1",
                    event_time: "2026-06-02T08:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=08"),
                },
                Deletion {
                    mode: "&DeletionMode=SoftDelete",
                    identifier: "C1",
                    event_time: "2026-06-02T09:00:00Z",
                    status: 200,
                    partition: Some("year=2026/month=06/day=02/hour=09"),
                },
            ],
        },
    ];

    for run in runs {
        let case = run.case;
        let aws = Aws::new();
        aws.create_bucket(run.bucket).await;
        let role_name = format!("{}-role", run.group);
        let role_arn = aws.create_role(case, &role_name).await;
        aws.grant_write(case, &role_name, &format!("arn:aws:s3:::{}/*", run.bucket))
            .await;
        let described = aws
            .create_feature_group(
                case,
                run.group,
                run.online,
                Some((
                    &format!("s3://{}/offline", run.bucket),
                    role_arn.as_str(),
                )),
            )
            .await;
        let resolved = resolved_key(&described);

        for identifier in run.seeded {
            aws.put_record(
                case,
                run.group,
                record(identifier, SEED_EVENT_TIME, SEEDED_PAYLOAD),
            )
            .await;
        }
        let ingested = aws.keys(run.bucket).await;
        assert_eq!(
            ingested.len(),
            run.seeded.len(),
            "case {case:?}: expected one object per seeded record, got {ingested:?}"
        );
        let mut seen = ingested.clone();

        for deletion in run.deletions {
            let identifier = deletion.identifier;
            let event_time = deletion.event_time;
            let (status, answered) = aws
                .delete_record(
                    run.group,
                    &format!(
                        "RecordIdentifierValueAsString={identifier}&EventTime={event_time}{}",
                        deletion.mode
                    ),
                )
                .await;
            assert_eq!(
                status, deletion.status,
                "case {case:?}: expected HTTP {} from the deletion of {identifier} at \
                 {event_time}, got {status} with {answered}",
                deletion.status
            );

            let keys = aws.keys(run.bucket).await;
            let appended: Vec<String> = keys
                .iter()
                .filter(|key| !seen.contains(*key))
                .cloned()
                .collect();
            let Some(partition) = deletion.partition else {
                assert!(
                    appended.is_empty() && keys.len() == seen.len(),
                    "case {case:?}: expected the refused deletion of {identifier} at \
                     {event_time} to append nothing, got {keys:?}"
                );
                continue;
            };
            assert_eq!(
                appended.len(),
                1,
                "case {case:?}: expected exactly one object appended by the deletion of \
                 {identifier} at {event_time}, got {keys:?}"
            );
            let marker = &appended[0];
            let expected_prefix = format!("{resolved}/{partition}/");
            assert!(
                marker.starts_with(&expected_prefix) && marker.ends_with(".parquet"),
                "case {case:?}: expected the marker of {identifier} at {event_time} under \
                 {expected_prefix:?}, got {marker:?}"
            );
            let held = parquet_rows(aws.object(run.bucket, marker).await);
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected the marker of {identifier} to hold one row, got \
                 {held:?}"
            );
            // "The deleted record marker is a record with the same RecordIdentifer as the
            // original, but with is_deleted value set to True, EventTime set to the delete
            // input EventTime, and other feature values set to null."
            for expected in [identifier, event_time, DELETED_COLUMN_TRUE] {
                assert!(
                    held[0].contains(expected),
                    "case {case:?}: expected the marker row of {identifier} at {event_time} \
                     to carry {expected:?}, got {:?}",
                    held[0]
                );
            }
            assert!(
                !held[0].contains(SEEDED_PAYLOAD),
                "case {case:?}: expected the marker of {identifier} to carry no feature \
                 values of the record, got {:?}",
                held[0]
            );
            seen = keys;
        }

        // The store is APPEND-ONLY: every object the seeding wrote is still exactly what it
        // was written as, whatever the run then deleted.
        for key in &ingested {
            let held = parquet_rows(aws.object(run.bucket, key).await);
            assert!(
                held.first().is_some_and(|row| row
                    .contains(DELETED_COLUMN_FALSE)
                    && row.contains(SEEDED_PAYLOAD)),
                "case {case:?}: expected the ingested object {key:?} left as written, got \
                 {held:?}"
            );
        }
    }
}

/// RULE: a deletion's marker is written UNDER THE FEATURE GROUP'S OWN ROLE, exactly as an
/// ingestion is, so a group whose role has lost the object write retains no marker — and
/// reports `Blocked` — while a deletion made once the permission is back is retained again.
#[tokio::test]
async fn a_deletions_marker_is_written_only_while_the_groups_role_may_write_it() {
    let case = "a deletion made with the permission withheld, then restored";
    let bucket = "features-delete-withheld";
    let group = "offlinedeletewithheld";
    let aws = Aws::new();
    aws.create_bucket(bucket).await;
    let role_name = format!("{group}-role");
    let role_arn = aws.create_role(case, &role_name).await;
    let resource = format!("arn:aws:s3:::{bucket}/*");
    aws.grant_write(case, &role_name, &resource).await;
    let described = aws
        .create_feature_group(
            case,
            group,
            true,
            Some((&format!("s3://{bucket}/offline"), role_arn.as_str())),
        )
        .await;
    let resolved = resolved_key(&described);
    aws.put_record(case, group, record("C1", SEED_EVENT_TIME, SEEDED_PAYLOAD))
        .await;
    let ingested = aws.keys(bucket).await;
    assert_eq!(
        ingested.len(),
        1,
        "case {case:?}: the seeded ingestion must be retained, got {ingested:?}"
    );

    // The permission goes away. The deletion is still served — the replication is the
    // service's own asynchronous work — and nothing is retained for it.
    aws.revoke_write(case, &role_name).await;
    let (status, answered) = aws
        .delete_record(
            group,
            "RecordIdentifierValueAsString=C1&EventTime=2026-06-02T08:00:00Z\
             &DeletionMode=SoftDelete",
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteRecord, got {status} with {answered}"
    );
    let keys = aws.keys(bucket).await;
    assert_eq!(
        keys, ingested,
        "case {case:?}: a marker the role may not write retains nothing, got {keys:?}"
    );
    let described = aws.describe_feature_group(case, group).await;
    let (status, reason) = offline_status(&described);
    assert_eq!(
        status.as_deref(),
        Some("Blocked"),
        "case {case:?}: a revoked permission blocks the offline store, got {described}"
    );
    let reason = reason.unwrap_or_default();
    for expected in [role_arn.as_str(), WRITE_ACTION] {
        assert!(
            reason.contains(expected),
            "case {case:?}: expected the blocked reason to name {expected:?}, got {reason:?}"
        );
    }

    // And back again: the next deletion's marker is appended, in its own event time's
    // partition.
    aws.grant_write(case, &role_name, &resource).await;
    let (status, answered) = aws
        .delete_record(
            group,
            "RecordIdentifierValueAsString=C1&EventTime=2026-06-03T09:00:00Z\
             &DeletionMode=HardDelete",
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the deletion made once the permission is \
         back, got {status} with {answered}"
    );
    let keys = aws.keys(bucket).await;
    let appended: Vec<String> = keys
        .iter()
        .filter(|key| !ingested.contains(*key))
        .cloned()
        .collect();
    assert_eq!(
        appended.len(),
        1,
        "case {case:?}: expected the marker appended once the permission is back, got {keys:?}"
    );
    let expected_prefix = format!("{resolved}/year=2026/month=06/day=03/hour=09/");
    assert!(
        appended[0].starts_with(&expected_prefix) && appended[0].ends_with(".parquet"),
        "case {case:?}: expected a Parquet marker under {expected_prefix:?}, got {:?}",
        appended[0]
    );
    let held = parquet_rows(aws.object(bucket, &appended[0]).await);
    assert!(
        held.first()
            .is_some_and(|row| row.contains(DELETED_COLUMN_TRUE) && row.contains("C1")),
        "case {case:?}: expected the appended row to be the delete marker of C1, got {held:?}"
    );
}

impl Aws {
    /// `POST /FeatureGroup/<group>/Record` — one `UpdateRecord`, answering its status and
    /// body.
    async fn update_record(&self, group: &str, body: Value) -> (u16, Value) {
        self.data_plane("POST", &format!("/FeatureGroup/{group}/Record"), Some(body))
            .await
    }
}

/// The value the online record holds for `feature_name`, out of what a `GetRecord` served.
fn served_value(answered: &Value, feature_name: &str) -> String {
    answered["Record"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|feature| feature["FeatureName"] == json!(feature_name))
        .and_then(|feature| feature["ValueAsString"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// One row of the update table.
struct Update {
    case: &'static str,
    /// The feature group the record is updated in.
    group: &'static str,
    /// The bucket its offline store lives in; a row's own, so its listing holds only its
    /// own objects.
    bucket: &'static str,
    /// Whether the group's role is granted the object write on that bucket.
    granted: bool,
    /// The `TargetStores` the update names, or `None` for an update that names none at
    /// all.
    target_stores: Option<&'static [&'static str]>,
    /// The `EventTime` the update carries among its `Features`, or `None` for one that
    /// carries none — which keeps the record's stored event time.
    event_time: Option<&'static str>,
    /// The payload value the update posts.
    payload: &'static str,
    /// The status the update must answer.
    status: u16,
    /// The partition the appended object must land in, and `None` for an update that
    /// appends nothing.
    appended_partition: Option<&'static str>,
    /// The event time the MERGED record carries — the update's own when it carries one,
    /// and the stored one otherwise — which is what the appended row holds.
    merged_event_time: &'static str,
    /// The payload the ONLINE record must hold after the update.
    online_payload: &'static str,
    /// The event time the ONLINE record must hold after the update.
    online_event_time: &'static str,
}

/// RULE: an `UpdateRecord` the service accepts is made in every store the request's
/// `TargetStores` names, defaulting to all the stores the feature group has — "The target
/// stores for the record update. By default, Amazon SageMaker Feature Store updates the
/// record in all stores associated with the FeatureGroup" — so an update naming the
/// OfflineStore APPENDS one more object holding the record AS MERGED (the stored feature
/// values with the posted ones applied), in the partition of that merged record's event
/// time, while an update naming only the OnlineStore appends none and one naming only the
/// OfflineStore leaves the online record standing.
///
/// The rows differ along what the rule generalises over: WHICH stores the request names
/// (none, both, each alone), whether the update MOVES the record's event time (which
/// decides the appended object's partition), whether the service accepts the update at all,
/// and whether the group's role may write the destination.
#[tokio::test]
async fn an_accepted_update_is_replicated_into_every_store_the_request_targets() {
    let updates: [Update; 7] = [
        Update {
            case: "no TargetStores at all",
            group: "updatedefaulted",
            bucket: "features-update-defaulted",
            granted: true,
            target_stores: None,
            event_time: Some("2026-06-02T08:00:00Z"),
            payload: "updated",
            status: 200,
            appended_partition: Some("year=2026/month=06/day=02/hour=08"),
            merged_event_time: "2026-06-02T08:00:00Z",
            online_payload: "updated",
            online_event_time: "2026-06-02T08:00:00Z",
        },
        Update {
            case: "both stores named",
            group: "updatebothstores",
            bucket: "features-update-both",
            granted: true,
            target_stores: Some(&["OnlineStore", "OfflineStore"]),
            event_time: Some("2026-07-02T13:00:00Z"),
            payload: "both",
            status: 200,
            appended_partition: Some("year=2026/month=07/day=02/hour=13"),
            merged_event_time: "2026-07-02T13:00:00Z",
            online_payload: "both",
            online_event_time: "2026-07-02T13:00:00Z",
        },
        Update {
            case: "the OfflineStore alone",
            group: "updateofflineonly",
            bucket: "features-update-offline-only",
            granted: true,
            target_stores: Some(&["OfflineStore"]),
            event_time: Some("2026-06-02T08:00:00Z"),
            payload: "offline",
            status: 200,
            appended_partition: Some("year=2026/month=06/day=02/hour=08"),
            merged_event_time: "2026-06-02T08:00:00Z",
            // The online record is left exactly as the ingestion stored it.
            online_payload: SEEDED_PAYLOAD,
            online_event_time: SEED_EVENT_TIME,
        },
        Update {
            case: "the OnlineStore alone",
            group: "updateonlineonly",
            bucket: "features-update-online-only",
            granted: true,
            target_stores: Some(&["OnlineStore"]),
            event_time: Some("2026-06-02T08:00:00Z"),
            payload: "online",
            status: 200,
            appended_partition: None,
            merged_event_time: "2026-06-02T08:00:00Z",
            online_payload: "online",
            online_event_time: "2026-06-02T08:00:00Z",
        },
        Update {
            // "If you omit EventTime, the service keeps the record's existing EventTime",
            // so the merged record — and the partition its row lands in — is the stored
            // event time's.
            case: "an update carrying no EventTime",
            group: "updatenoeventtime",
            bucket: "features-update-no-event-time",
            granted: true,
            target_stores: None,
            event_time: None,
            payload: "merged",
            status: 200,
            appended_partition: Some("year=2026/month=06/day=01/hour=07"),
            merged_event_time: SEED_EVENT_TIME,
            online_payload: "merged",
            online_event_time: SEED_EVENT_TIME,
        },
        Update {
            // An update the service refuses writes nowhere: "the service rejects the update
            // with a ConflictException… The record was not modified."
            case: "an EventTime older than the record's",
            group: "updateconflicting",
            bucket: "features-update-conflicting",
            granted: true,
            target_stores: None,
            event_time: Some("2026-01-01T00:00:00Z"),
            payload: "stale",
            status: 409,
            appended_partition: None,
            merged_event_time: SEED_EVENT_TIME,
            online_payload: SEEDED_PAYLOAD,
            online_event_time: SEED_EVENT_TIME,
        },
        Update {
            // The replication is the service's own asynchronous work, so a role that may
            // not write the destination retains nothing and the update still succeeds.
            case: "a role that may not write the offline store",
            group: "updatewithheld",
            bucket: "features-update-withheld",
            granted: false,
            target_stores: None,
            event_time: Some("2026-06-02T08:00:00Z"),
            payload: "withheld",
            status: 200,
            appended_partition: None,
            merged_event_time: "2026-06-02T08:00:00Z",
            online_payload: "withheld",
            online_event_time: "2026-06-02T08:00:00Z",
        },
    ];

    for update in updates {
        let case = update.case;
        let aws = Aws::new();
        aws.create_bucket(update.bucket).await;
        let role_name = format!("{}-role", update.group);
        let role_arn = aws.create_role(case, &role_name).await;
        if update.granted {
            aws.grant_write(case, &role_name, &format!("arn:aws:s3:::{}/*", update.bucket))
                .await;
        }
        let described = aws
            .create_feature_group(
                case,
                update.group,
                true,
                Some((
                    &format!("s3://{}/offline", update.bucket),
                    role_arn.as_str(),
                )),
            )
            .await;
        let resolved = resolved_key(&described);

        // The record the update is made against, ingested first: the update is PARTIAL, so
        // the merged record is this one with the posted values applied.
        aws.put_record(case, update.group, record("C1", SEED_EVENT_TIME, SEEDED_PAYLOAD))
            .await;
        let seeded = aws.keys(update.bucket).await;
        assert_eq!(
            seeded.len(),
            usize::from(update.granted),
            "case {case:?}: expected the ingestion's own object before the update, got \
             {seeded:?}"
        );

        let mut features = vec![json!({
            "FeatureName": PAYLOAD_FEATURE,
            "ValueAsString": update.payload,
        })];
        if let Some(at) = update.event_time {
            features.push(json!({
                "FeatureName": EVENT_TIME_FEATURE,
                "ValueAsString": at,
            }));
        }
        let mut body = json!({
            "RecordIdentifierValueAsString": "C1",
            "Features": features,
        });
        if let Some(targets) = update.target_stores {
            body.as_object_mut()
                .expect("a JSON object body")
                .insert("TargetStores".to_string(), json!(targets));
        }
        let (status, answered) = aws.update_record(update.group, body).await;
        assert_eq!(
            status, update.status,
            "case {case:?}: expected HTTP {} from UpdateRecord, got {status} with {answered}",
            update.status
        );

        // The offline store is APPEND-ONLY: whatever the update wrote, the object the
        // ingestion left is still there and still holds the ingested record.
        let keys = aws.keys(update.bucket).await;
        for key in &seeded {
            assert!(
                keys.contains(key),
                "case {case:?}: expected the ingestion's object {key:?} to be left standing, \
                 got {keys:?}"
            );
            let held = parquet_rows(aws.object(update.bucket, key).await);
            assert!(
                held.first().is_some_and(|row| row.contains(SEEDED_PAYLOAD)),
                "case {case:?}: expected the ingested row to be unchanged, got {held:?}"
            );
        }
        let appended: Vec<String> = keys
            .iter()
            .filter(|key| !seeded.contains(*key))
            .cloned()
            .collect();
        match update.appended_partition {
            None => assert!(
                appended.is_empty(),
                "case {case:?}: expected nothing appended to the offline store, got \
                 {appended:?}"
            ),
            Some(partition) => {
                assert_eq!(
                    appended.len(),
                    1,
                    "case {case:?}: expected exactly one object appended by the update, got \
                     {keys:?}"
                );
                let expected_prefix = format!("{resolved}/{partition}/");
                assert!(
                    appended[0].starts_with(&expected_prefix)
                        && appended[0].ends_with(".parquet"),
                    "case {case:?}: expected a Parquet object under {expected_prefix:?}, got \
                     {:?}",
                    appended[0]
                );
                let held = parquet_rows(aws.object(update.bucket, &appended[0]).await);
                assert_eq!(
                    held.len(),
                    1,
                    "case {case:?}: expected the appended object to hold the one updated \
                     record, got {held:?}"
                );
                // The row is the record AS MERGED: the identifier and the event time the
                // merged record carries, the posted payload, and not a delete marker.
                for expected in [
                    "C1",
                    update.merged_event_time,
                    update.payload,
                    DELETED_COLUMN_FALSE,
                ] {
                    assert!(
                        held[0].contains(expected),
                        "case {case:?}: expected the appended row to carry {expected:?}, got \
                         {:?}",
                        held[0]
                    );
                }
            }
        }

        // And the ONLINE record is what the target list says it is.
        let (status, answered) = aws.get_record(update.group, "C1").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the online store to serve C1, got {status} with \
             {answered}"
        );
        let (payload, event_time) = (
            served_value(&answered, PAYLOAD_FEATURE),
            served_value(&answered, EVENT_TIME_FEATURE),
        );
        assert_eq!(
            (payload.as_str(), event_time.as_str()),
            (update.online_payload, update.online_event_time),
            "case {case:?}: expected the online record to hold ({:?}, {:?}) after the \
             update, got ({payload:?}, {event_time:?}) in {answered}",
            update.online_payload,
            update.online_event_time
        );
    }
}
