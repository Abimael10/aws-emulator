// SPDX-License-Identifier: CC0-1.0

//! The REGION every ARN this service mints carries, end to end over HTTP.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rule under test: a value this service mints into an answered member satisfies the
//! constraint of the shape that member targets, so the one caller-chosen field of a
//! minted ARN — the region of the request's SigV4 credential scope, which no boundary
//! validator sees — names the ARN only when `ARN`'s `^[a-zA-Z0-9:*/-]+$` and its
//! `@length(max: 2048)` admit it in that field, and otherwise the service's own default
//! region names it, exactly as for a request carrying no credential scope at all.
//!
//! The rows below differ along what the rule generalises over: which character the signed
//! region is spelled with, and how long it is — and every one of them is checked on all
//! three ARNs this service mints (a change set's, an entity's and an assessment's) through
//! every operation that reports one.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region a minted ARN names when the signed one cannot stand in it. Spelled out
/// here rather than taken from the crate so a changed default fails this test instead of
/// following it.
const FALLBACK_REGION: &str = "us-east-1";

/// The entity type the change sets below create, which is also the type segment of the
/// entity ARNs they mint.
const ENTITY_TYPE: &str = "AmiProduct";

/// How long the region field of a minted ARN may be: `ARN`'s `@length(max: 2048)`
/// (`smithy_query(query="shape", shape="ARN")`) less the longest ARN this service mints
/// around it — `arn:aws:` (8) + `aws-marketplace` (15) + the two `:` around the region
/// (2) + the account id (12) + `:` (1) + `AWSMarketplace` (14) + `/` (1) + an entity type
/// at `EntityType`'s `@length(max: 255)` + `/` (1) + an entity id at `EntityId`'s
/// `@length(max: 255)`, which is 564 characters.
const REGION_LIMIT: usize = 2048 - 564;

/// `ARN`'s `^[a-zA-Z0-9:*/-]+$` with its `@length(min: 1, max: 2048)`, spelled out here
/// rather than taken from the crate's own predicate so a wrong implementation cannot make
/// this test pass.
fn is_arn(value: &str) -> bool {
    (1..=2048).contains(&value.chars().count())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '*' | '/' | '-'))
}

/// An `Authorization` header signed for `region`, naming this service's signing name
/// (`aws-marketplace`), which is what routes the request here.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/aws-marketplace/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// An ARN of this catalog as the service publishes it, built here from the parts rather
/// than through the crate's own builder.
fn spelled_out_arn(region: &str, resource: &str) -> String {
    format!("arn:aws:aws-marketplace:{region}:{ACCOUNT}:{CATALOG}/{resource}")
}

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The string at `pointer`, or the empty string.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The array at `pointer`, or an empty one when the member is absent.
    fn list(&self, pointer: &str) -> Vec<Value> {
        self.body
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}

async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "catalog.marketplace.us-east-1.amazonaws.com")
        .header("authorization", authorization(region));
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// The body of a change set that creates one AMI product WITH a delivery option, so that
/// applying it mints an entity ARN and assessing that option mints an assessment ARN.
fn create_ami_product() -> Value {
    json!({
        "Catalog": CATALOG,
        "ChangeSet": [{
            "ChangeType": "AddDeliveryOptions",
            "Entity": {"Type": format!("{ENTITY_TYPE}@1.0")},
            "Details": json!({
                "Version": {"VersionTitle": "1.0", "ReleaseNotes": "first release"},
                "DeliveryOptions": [{"Details": {"AmiDeliveryOptionDetails": {
                    "AmiSource": {
                        "AmiId": "ami-0abcdef1234567890",
                        "AccessRoleArn": "arn:aws:iam::123456789012:role/MarketplaceScan",
                        "UserName": "ec2-user",
                        "OperatingSystemName": "AMAZONLINUX",
                        "OperatingSystemVersion": "2023",
                    },
                    "RecommendedInstanceType": "t3.medium",
                    "SecurityGroups": [{
                        "IpProtocol": "tcp",
                        "IpRanges": ["10.0.0.0/16"],
                        "FromPort": 22,
                        "ToPort": 22,
                    }],
                    "UsageInstructions": "Connect over SSH and run /opt/product/start.",
                }}}],
            }).to_string(),
        }],
    })
}

/// Read a change set until the clock has carried it to a terminal status, and answer that
/// description. The wait is for the CONDITION, bounded by a deadline generous enough for a
/// loaded machine.
async fn poll_until_terminal(router: &MultiServiceRouter, region: &str, id: &str) -> Answer {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let described = call(
            router,
            region,
            "GET",
            &format!("/DescribeChangeSet?catalog={CATALOG}&changeSetId={id}"),
            None,
        )
        .await;
        let status = described.text("/Status");
        if ["SUCCEEDED", "FAILED", "CANCELLED"].contains(&status.as_str()) {
            return described;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "change set {id} must reach a terminal status, last reported {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// One reported ARN: it is the ARN the service publishes for that resource in the region
/// the rule says names it, and it satisfies the `ARN` shape that carries it.
fn check_arn(case: &str, member: &str, actual: &str, expected: &str) {
    assert_eq!(
        actual, expected,
        "case {case:?}: {member} must be {expected:?}, got {actual:?}"
    );
    assert!(
        is_arn(actual),
        "case {case:?}: {member} must satisfy ARN's ^[a-zA-Z0-9:*/-]+$ within 2048 \
         characters, got {actual:?}"
    );
}

/// Every operation that reports a minted ARN reports one inside `ARN`, naming the signed
/// region when that region can stand in an ARN and the default region when it cannot.
#[tokio::test]
async fn every_minted_arn_names_a_region_its_shape_admits() {
    // (case, the region the request is signed for, the region its ARNs must name)
    let rows: Vec<(String, String, String)> = vec![
        (
            "a published region name".to_string(),
            "eu-west-1".to_string(),
            "eu-west-1".to_string(),
        ),
        (
            // A published region the issue did not cite, with a two-digit-free spelling.
            "another published region name".to_string(),
            "ap-southeast-7".to_string(),
            "ap-southeast-7".to_string(),
        ),
        (
            "a region spelled with the uppercase letters ARN admits".to_string(),
            "US-EAST-1".to_string(),
            "US-EAST-1".to_string(),
        ),
        (
            "a region spelled with an underscore".to_string(),
            "us_east_1".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            "a region spelled with dots".to_string(),
            "us.east.1".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            "a region spelled with plus signs".to_string(),
            "us+east+1".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            "a region spelled with equals signs, which ResourceARN admits".to_string(),
            "us=east=1".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            "a region carrying the colon that separates an ARN's fields".to_string(),
            "us:east:1".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            "a region carrying the asterisk of an ARN wildcard".to_string(),
            "us-east-*".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            // A character outside ARN's class that the credential scope's own syntax
            // carries unchanged, like the four above.
            "a region spelled with tildes".to_string(),
            "us~east~1".to_string(),
            FALLBACK_REGION.to_string(),
        ),
        (
            "a region of one character".to_string(),
            "a".to_string(),
            "a".to_string(),
        ),
        (
            "a region exactly as long as the ARN's region field allows".to_string(),
            "a".repeat(REGION_LIMIT),
            "a".repeat(REGION_LIMIT),
        ),
        (
            "a region one character past what the ARN's region field allows".to_string(),
            "a".repeat(REGION_LIMIT + 1),
            FALLBACK_REGION.to_string(),
        ),
    ];

    for (case, signed, expected) in rows {
        let case = case.as_str();
        let region = signed.as_str();
        let router = emulator();

        let started = call(
            &router,
            region,
            "POST",
            "/StartChangeSet",
            Some(create_ami_product()),
        )
        .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: StartChangeSet must answer 200, got {} with {:?}",
            started.status, started.body
        );
        let change_set_id = started.text("/ChangeSetId");
        let change_set_arn = spelled_out_arn(&expected, &format!("ChangeSet/{change_set_id}"));
        check_arn(
            case,
            "StartChangeSetResponse$ChangeSetArn",
            &started.text("/ChangeSetArn"),
            &change_set_arn,
        );

        let described = poll_until_terminal(&router, region, &change_set_id).await;
        assert_eq!(
            described.text("/Status"),
            "SUCCEEDED",
            "case {case:?}: the change set must succeed before its entity is read, got {:?}",
            described.body
        );
        check_arn(
            case,
            "DescribeChangeSetResponse$ChangeSetArn",
            &described.text("/ChangeSetArn"),
            &change_set_arn,
        );

        let listed = call(
            &router,
            region,
            "POST",
            "/ListChangeSets",
            Some(json!({"Catalog": CATALOG})),
        )
        .await;
        let summary = listed
            .list("/ChangeSetSummaryList")
            .into_iter()
            .find(|item| item["ChangeSetId"] == json!(change_set_id))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: ListChangeSets must report change set {change_set_id}, \
                     got {:?}",
                    listed.body
                )
            });
        check_arn(
            case,
            "ChangeSetSummaryListItem$ChangeSetArn",
            summary["ChangeSetArn"].as_str().unwrap_or_default(),
            &change_set_arn,
        );

        let entity_id = summary["EntityIdList"][0]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !entity_id.is_empty(),
            "case {case:?}: the succeeded change set must name the entity it created, got {:?}",
            listed.body
        );
        let entity_arn = spelled_out_arn(&expected, &format!("{ENTITY_TYPE}/{entity_id}"));

        let entity = call(
            &router,
            region,
            "GET",
            &format!("/DescribeEntity?catalog={CATALOG}&entityId={entity_id}"),
            None,
        )
        .await;
        check_arn(
            case,
            "DescribeEntityResponse$EntityArn",
            &entity.text("/EntityArn"),
            &entity_arn,
        );

        let batch = call(
            &router,
            region,
            "POST",
            "/BatchDescribeEntities",
            Some(json!({
                "EntityRequestList": [{"Catalog": CATALOG, "EntityId": entity_id}],
            })),
        )
        .await;
        check_arn(
            case,
            "EntityDetail$EntityArn",
            &batch.text(&format!("/EntityDetails/{entity_id}/EntityArn")),
            &entity_arn,
        );

        let entities = call(
            &router,
            region,
            "POST",
            "/ListEntities",
            Some(json!({"Catalog": CATALOG, "EntityType": ENTITY_TYPE})),
        )
        .await;
        let entity_summary = entities
            .list("/EntitySummaryList")
            .into_iter()
            .find(|item| item["EntityId"] == json!(entity_id))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: ListEntities must report entity {entity_id}, got {:?}",
                    entities.body
                )
            });
        check_arn(
            case,
            "EntitySummary$EntityArn",
            entity_summary["EntityArn"].as_str().unwrap_or_default(),
            &entity_arn,
        );

        let assessments = call(
            &router,
            region,
            "POST",
            "/ListAssessments",
            Some(json!({
                "Catalog": CATALOG,
                "MaxResults": 100,
                "AssessmentTargetFilter": {"ChangeSetId": change_set_id},
            })),
        )
        .await;
        let assessment = assessments
            .list("/AssessmentSummaryList")
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the delivery option the change set submitted must be \
                     assessed, got {:?}",
                    assessments.body
                )
            });
        let assessment_id = assessment["AssessmentId"].as_str().unwrap_or_default();
        let assessment_arn = spelled_out_arn(&expected, &format!("Assessment/{assessment_id}"));
        check_arn(
            case,
            "AssessmentSummary$AssessmentArn",
            assessment["AssessmentArn"].as_str().unwrap_or_default(),
            &assessment_arn,
        );

        let detailed = call(
            &router,
            region,
            "POST",
            "/DescribeAssessment",
            Some(json!({"Catalog": CATALOG, "AssessmentIdentifier": assessment_id})),
        )
        .await;
        check_arn(
            case,
            "DescribeAssessmentResponse$AssessmentArn",
            &detailed.text("/AssessmentArn"),
            &assessment_arn,
        );
    }
}

/// `CancelChangeSet` reports the ARN the record holds, so a change set started in a
/// region no ARN can name is cancelled as the ARN the start answered.
///
/// The cancel is issued while the change set is still cancellable; the status it reaches
/// afterwards is the lifecycle's own test, not this one's.
#[tokio::test]
async fn cancel_reports_the_arn_the_record_holds() {
    for (case, signed, expected) in [
        ("a published region name", "eu-west-1", "eu-west-1"),
        (
            "a region spelled with an underscore",
            "us_east_1",
            FALLBACK_REGION,
        ),
    ] {
        let router = emulator();
        let started = call(
            &router,
            signed,
            "POST",
            "/StartChangeSet",
            Some(create_ami_product()),
        )
        .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: StartChangeSet must answer 200, got {} with {:?}",
            started.status, started.body
        );
        let change_set_id = started.text("/ChangeSetId");
        let cancelled = call(
            &router,
            signed,
            "PATCH",
            &format!("/CancelChangeSet?catalog={CATALOG}&changeSetId={change_set_id}"),
            None,
        )
        .await;
        assert_eq!(
            cancelled.status, 200,
            "case {case:?}: CancelChangeSet must answer 200, got {} with {:?}",
            cancelled.status, cancelled.body
        );
        check_arn(
            case,
            "CancelChangeSetResponse$ChangeSetArn",
            &cancelled.text("/ChangeSetArn"),
            &spelled_out_arn(expected, &format!("ChangeSet/{change_set_id}")),
        );
    }
}
