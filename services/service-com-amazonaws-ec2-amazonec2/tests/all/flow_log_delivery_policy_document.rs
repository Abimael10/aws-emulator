// SPDX-License-Identifier: CC0-1.0

//! The DOCUMENT a flow log's delivery grants are written in, as the destination bucket reports
//! it back.
//!
//! The grants themselves — their principal, actions, resources and conditions — are the
//! subject of `flow_log_s3_delivery_grants`. This suite is about the document around them, and
//! the one rule it follows: the log delivery NAMES the document it writes and NUMBERS the
//! statements it writes in it, and changes nothing else about the document the bucket carries.
//! So:
//!
//! 1. THE DOCUMENT CARRIES THE LOG-DELIVERY GRANT DOCUMENT'S NAME, and each statement the
//!    delivery writes carries its base statement id followed by its position among the
//!    statements of that kind — one write grant per delivered location, numbered from one, and
//!    the single bucket-ACL check as the first of its kind.
//! 2. A DOCUMENT OF THE OWNER'S KEEPS WHAT THE OWNER WROTE: its own name, and every statement
//!    of its own with the statement id it was written with.
//! 3. THE BUCKET REPORTS THE DOCUMENT IT CARRIES: a document put afterwards reads back through
//!    `GetBucketPolicy` byte for byte, whatever name and statement ids it carries.
//!
//! EC2 and Amazon S3 are mounted on ONE shared engine, exactly as the assembled emulator
//! mounts them, so the policy is read back from the state Amazon S3 serves `GetBucketPolicy`
//! from.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements};

/// The Region this suite signs for, and the Region an Amazon S3 `CreateBucket` with no
/// `CreateBucketConfiguration` places a bucket in.
const REGION: &str = "us-east-1";

/// The name the log-delivery grant document carries, written out here so this suite states the
/// rule independently of the implementation's own spelling.
const DELIVERY_DOCUMENT_ID: &str = "AWSLogDeliveryWrite20150319";

/// The base statement id of the grant of the object write, which each write statement is
/// numbered from.
const WRITE_SID: &str = "AWSLogDeliveryWrite";

/// The base statement id of the grant of the bucket-ACL read.
const ACL_CHECK_SID: &str = "AWSLogDeliveryAclCheck";

/// The service principal the delivery acts as — the identity the written statements grant.
const LOG_DELIVERY_PRINCIPAL: &str = "delivery.logs.amazonaws.com";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// EC2 and Amazon S3 over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's own concern and is exercised elsewhere;
        // these cases are about these services' behaviour.
        router.set_request_authentication(false);
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// The SigV4 `Authorization` header of a request signed for `service`, which is how the
    /// router picks the service a request belongs to.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTEC2/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=00"
        )
    }

    /// One VPC of the account, through EC2's own `CreateVpc`.
    async fn given_vpc(&self, cidr_block: &str) -> String {
        let (status, body) = call(
            &self.router,
            REGION,
            "CreateVpc",
            &[p("CidrBlock", cidr_block)],
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateVpc({cidr_block}) must answer 200, got {status} with body {body}"
        );
        element(&body, "vpcId")
            .unwrap_or_else(|| panic!("CreateVpc({cidr_block}) reported no vpcId in {body}"))
    }

    /// One bucket, through Amazon S3's own (REST) wire, path style.
    async fn given_bucket(&self, name: &str) {
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("/{name}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::new())
            .expect("the test request is well formed");
        let (status, payload) = self.dispatch(request).await;
        assert_eq!(
            status, 200,
            "CreateBucket({name}) must succeed, got {status} with {payload}"
        );
    }

    /// Attach `document` to `name` through Amazon S3's own `PutBucketPolicy`, exactly as the
    /// bytes given.
    async fn put_bucket_policy(&self, case: &str, name: &str, document: &str) {
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("/{name}?policy"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::from(document.to_string()))
            .expect("the test request is well formed");
        let (status, payload) = self.dispatch(request).await;
        assert_eq!(
            status, 204,
            "case {case:?}: PutBucketPolicy({name}) must succeed, got {status} with {payload}"
        );
    }

    /// The policy `name` carries right now, as the BYTES `GetBucketPolicy` answered, beside the
    /// status — a read that says nothing about whether the body is JSON.
    async fn bucket_policy_body(&self, name: &str) -> (u16, String) {
        let request = http::Request::builder()
            .method("GET")
            .uri(format!("/{name}?policy"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::new())
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }

    /// The document `name` must be carrying, parsed — a read that insists on 200 and on JSON.
    async fn policy_of(&self, case: &str, name: &str) -> Value {
        let (status, body) = self.bucket_policy_body(name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the bucket {name} must carry a policy, got {status} with {body}"
        );
        serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("case {case:?}: GetBucketPolicy must answer a JSON document ({error}): {body}")
        })
    }

    /// One `CreateFlowLogs` to an Amazon S3 destination that must succeed.
    async fn given_s3_flow_log(&self, case: &str, vpc_id: &str, log_destination: &str) {
        let params = vec![
            p("ResourceType", "VPC"),
            p("ResourceId.1", vpc_id),
            p("TrafficType", "ALL"),
            p("LogDestinationType", "s3"),
            p("LogDestination", log_destination),
        ];
        let (status, body) = call(&self.router, REGION, "CreateFlowLogs", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateFlowLogs must answer 200, got {status} with body {body}"
        );
        let ids = elements(&body, "item");
        assert_eq!(
            ids.len(),
            1,
            "case {case:?}: CreateFlowLogs must report exactly one flow log id, got {ids:?} \
             in {body}"
        );
    }

    async fn dispatch(&self, request: http::Request<Bytes>) -> (u16, String) {
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }
}

/// The statements of a policy document, in the order it holds them.
fn statements(document: &Value) -> Vec<Value> {
    document["Statement"].as_array().cloned().unwrap_or_default()
}

/// The `Sid` of every statement of `document` that ALLOWS the log delivery service `action`,
/// in document order.
fn granted_statement_ids(document: &Value, action: &str) -> Vec<String> {
    statements(document)
        .into_iter()
        .filter(|statement| {
            statement["Effect"] == json!("Allow")
                && statement["Principal"]["Service"] == json!(LOG_DELIVERY_PRINCIPAL)
                && statement["Action"] == json!(action)
        })
        .map(|statement| statement["Sid"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `["<base>1", "<base>2", …]` — the statement ids `count` statements of one kind carry.
fn numbered(base: &str, count: usize) -> Vec<String> {
    (1..=count).map(|at| format!("{base}{at}")).collect()
}

/// RULE 1: the document the delivery writes carries the grant document's name, and each
/// statement it writes carries its base statement id numbered by its position among the
/// statements of that kind.
///
/// The steps differ in how many locations the bucket delivers to, which is what the write
/// grants are numbered over: one flow log, then a second one under a subfolder, then a third
/// delivering where the first does (which adds no location and so no number).
#[tokio::test]
async fn the_delivery_names_the_document_and_numbers_the_statements_it_writes() {
    let emulator = Emulator::new();
    let bucket = "flow-logs-named-document";
    emulator.given_bucket(bucket).await;
    let vpc = emulator.given_vpc("10.6.0.0/16").await;
    let second_vpc = emulator.given_vpc("10.7.0.0/16").await;

    // (case, the monitored VPC, the destination ARN, the write grants the bucket must then
    // carry)
    let steps: Vec<(&str, &str, String, usize)> = vec![
        (
            "the first flow log, delivering to the bucket itself",
            &vpc,
            format!("arn:aws:s3:::{bucket}"),
            1,
        ),
        (
            "a second flow log, delivering under a subfolder",
            &second_vpc,
            format!("arn:aws:s3:::{bucket}/vpc/"),
            2,
        ),
        (
            "a third flow log, delivering where the first one does",
            &second_vpc,
            format!("arn:aws:s3:::{bucket}"),
            2,
        ),
    ];

    for (case, monitored, log_destination, writes) in steps {
        emulator
            .given_s3_flow_log(case, monitored, &log_destination)
            .await;
        let document = emulator.policy_of(case, bucket).await;
        assert_eq!(
            document["Id"],
            json!(DELIVERY_DOCUMENT_ID),
            "case {case:?}: the document the delivery wrote must carry its name, got {document}"
        );
        assert_eq!(
            granted_statement_ids(&document, "s3:PutObject"),
            numbered(WRITE_SID, writes),
            "case {case:?}: the {writes} write grants must be numbered from one, \
             got {document}"
        );
        assert_eq!(
            granted_statement_ids(&document, "s3:GetBucketAcl"),
            numbered(ACL_CHECK_SID, 1),
            "case {case:?}: the one ACL-check grant must be the first of its kind, \
             got {document}"
        );
    }
}

/// RULE 2: a document of the owner's keeps what the owner wrote — its own name, and its own
/// statements with the statement ids they were written with.
///
/// The rows differ in what the owner's document names: a name of its own, no name at all, and
/// statement ids that collide with the ones the delivery writes (a document whose own
/// statement is already called `AWSLogDeliveryWrite1`).
#[tokio::test]
async fn the_owners_own_name_and_statement_ids_survive_the_delivery_write() {
    // (case, the bucket, the owner's own document, the name the policy must carry afterwards)
    let cases: Vec<(&str, &str, Value, String)> = vec![
        (
            "a document the owner named",
            "flow-logs-owner-named",
            json!({
                "Version": "2012-10-17",
                "Id": "the-owners-own-policy",
                "Statement": [{
                    "Sid": "DenyInsecureTransport",
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "s3:*",
                    "Resource": "arn:aws:s3:::flow-logs-owner-named/*",
                    "Condition": { "Bool": { "aws:SecureTransport": "false" } },
                }],
            }),
            "the-owners-own-policy".to_string(),
        ),
        (
            "a document carrying no name at all",
            "flow-logs-owner-unnamed",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "AllowTheAuditor",
                    "Effect": "Allow",
                    "Principal": { "AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id()) },
                    "Action": "s3:GetBucketAcl",
                    "Resource": "arn:aws:s3:::flow-logs-owner-unnamed",
                }],
            }),
            DELIVERY_DOCUMENT_ID.to_string(),
        ),
        (
            "a document whose own statement id collides with a written one",
            "flow-logs-owner-collides",
            json!({
                "Version": "2012-10-17",
                "Id": "AWSLogDeliveryWrite20150319",
                "Statement": [{
                    "Sid": "AWSLogDeliveryWrite1",
                    "Effect": "Deny",
                    "Principal": { "Service": LOG_DELIVERY_PRINCIPAL },
                    "Action": "s3:PutObject",
                    "Resource": "arn:aws:s3:::flow-logs-owner-collides/quarantine/*",
                }],
            }),
            DELIVERY_DOCUMENT_ID.to_string(),
        ),
    ];

    for (case, bucket, owners, named) in cases {
        let emulator = Emulator::new();
        emulator.given_bucket(bucket).await;
        emulator
            .put_bucket_policy(case, bucket, &owners.to_string())
            .await;
        let vpc = emulator.given_vpc("10.8.0.0/16").await;
        emulator
            .given_s3_flow_log(case, &vpc, &format!("arn:aws:s3:::{bucket}"))
            .await;

        let document = emulator.policy_of(case, bucket).await;
        assert_eq!(
            document["Id"],
            json!(named),
            "case {case:?}: the document must be named {named}, got {document}"
        );
        let owners_own = statements(&owners);
        let kept: Vec<Value> = statements(&document)
            .into_iter()
            .filter(|statement| owners_own.contains(statement))
            .collect();
        assert_eq!(
            kept, owners_own,
            "case {case:?}: the owner's own statements must be left exactly as written, \
             got {document}"
        );
        assert_eq!(
            granted_statement_ids(&document, "s3:PutObject"),
            numbered(WRITE_SID, 1),
            "case {case:?}: the write grant must be the first of its kind, got {document}"
        );
    }
}

/// RULE 3: the bucket reports the document it carries — one put after the create reads back
/// through `GetBucketPolicy` byte for byte, whatever name and statement ids it carries.
///
/// The rows differ in the FORM of the document put: named and unnamed, statement ids with and
/// without a trailing index, a single statement object rather than a list, and key order and
/// whitespace a re-serialization would not reproduce.
#[tokio::test]
async fn a_document_put_over_the_grants_reads_back_byte_for_byte() {
    let bucket = "flow-logs-round-trip";

    // (case, the document exactly as the caller writes it)
    let documents: Vec<(&str, String)> = vec![
        (
            "a named document with indexed statement ids",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Id\":\"{DELIVERY_DOCUMENT_ID}\",\
                 \"Statement\":[{{\"Sid\":\"{WRITE_SID}1\",\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Service\":\"{LOG_DELIVERY_PRINCIPAL}\"}},\
                 \"Action\":\"s3:PutObject\",\
                 \"Resource\":\"arn:aws:s3:::{bucket}/AWSLogs/*\"}},\
                 {{\"Sid\":\"{ACL_CHECK_SID}1\",\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Service\":\"{LOG_DELIVERY_PRINCIPAL}\"}},\
                 \"Action\":\"s3:GetBucketAcl\",\
                 \"Resource\":\"arn:aws:s3:::{bucket}\"}}]}}"
            ),
        ),
        (
            "a document with no name and an unindexed statement id",
            format!(
                "{{\"Statement\":[{{\"Sid\":\"{WRITE_SID}\",\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Service\":\"{LOG_DELIVERY_PRINCIPAL}\"}},\
                 \"Action\":\"s3:PutObject\",\
                 \"Resource\":\"arn:aws:s3:::{bucket}/*\"}}],\
                 \"Version\":\"2012-10-17\"}}"
            ),
        ),
        (
            "a single statement object, with its name last and indented",
            format!(
                "{{\n  \"Statement\": {{\n    \"Sid\": \"OwnersOwn42\",\n    \
                 \"Effect\": \"Deny\",\n    \"Principal\": \"*\",\n    \
                 \"Action\": \"s3:DeleteBucket\",\n    \
                 \"Resource\": \"arn:aws:s3:::{bucket}\"\n  }},\n  \
                 \"Id\": \"a name of the owner's own\"\n}}"
            ),
        ),
    ];

    for (case, document) in documents {
        let emulator = Emulator::new();
        emulator.given_bucket(bucket).await;
        let vpc = emulator.given_vpc("10.9.0.0/16").await;
        emulator
            .given_s3_flow_log(case, &vpc, &format!("arn:aws:s3:::{bucket}"))
            .await;
        emulator.put_bucket_policy(case, bucket, &document).await;

        let (status, body) = emulator.bucket_policy_body(bucket).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetBucketPolicy must answer 200, got {status} with {body}"
        );
        assert_eq!(
            body, document,
            "case {case:?}: GetBucketPolicy must report the document as it was put, got {body}"
        );
    }
}
