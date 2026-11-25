// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the pipeline ENDPOINT operations — `CreatePipelineEndpoint` and
//! `ListPipelineEndpoints` — driven the way a client drives them: one HTTP request per call
//! through the same `MultiServiceRouter` the emulator boots.
//!
//! The rule under test is the one every create of a child resource follows: once the
//! request satisfies the constraints its own members declare, the PARENT the request names
//! has to resolve in this service's stored state — an ARN no stored pipeline carries names
//! no pipeline to create an endpoint for, so the call is refused with the operation's
//! modelled 404 and records nothing — and a create whose parent resolves stores the child,
//! answers from the stored record and is reported by the listing of that child kind. Which
//! leg answers is decided in that order: a malformed member is refused before the lookup
//! ever happens.
//!
//! The rows of each table differ along what the rule generalises over: which segment of the
//! ARN names no pipeline, whether the pipeline never existed or was deleted, how many
//! subnets and security groups the VPC configuration carries (including both published
//! bounds and one past the upper one), the id forms the two legal lengths allow, and how
//! many endpoints one pipeline carries.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A region that is NOT the emulator default, so nothing under test can depend on a
/// default-region code path.
const REGION: &str = "eu-west-2";

/// The canonical emulator account id every ARN carries.
const ACCOUNT: &str = "000000000000";

/// How many reads a polling case will take before it gives up.
///
/// Generous: the transition an endpoint create arms is due the instant the call answers, so
/// a case that needs more reads than this is not slow, it is broken. Counting READS keeps
/// the cases independent of how fast the host runs — nothing here sleeps or reads a clock.
const READ_BUDGET: usize = 8;

/// A subnet id of the SHORTEST legal form: `SubnetId` is `@length(15, 24)` with pattern
/// `^subnet-\w{8}(\w{9})?$` (smithy_query: shape SubnetId), so `subnet-` plus 8 characters
/// sits on the lower bound.
const SHORT_SUBNET: &str = "subnet-0a1b2c3d";

/// A subnet id of the LONGEST legal form: `subnet-` plus 17 characters, the shape's upper
/// `@length` bound.
const LONG_SUBNET: &str = "subnet-0a1b2c3d4e5f6a7b8";

/// A security group id of the shortest legal form: `SecurityGroupId` is `@length(11, 20)`
/// with pattern `^sg-\w{8}(\w{9})?$` (smithy_query: shape SecurityGroupId).
const SHORT_GROUP: &str = "sg-0a1b2c3d";

/// A security group id of the longest legal form: `sg-` plus 17 characters.
const LONG_GROUP: &str = "sg-0a1b2c3d4e5f6a7b8";

/// The most subnets (and security groups) one endpoint's VPC configuration may name:
/// `SubnetIds` and `SecurityGroupIds` are both `@length(1, 12)` (smithy_query: shapes
/// SubnetIds / SecurityGroupIds).
const MAX_VPC_IDS: usize = 12;

/// What one call answered: the HTTP status, the modelled error code (empty on success) and
/// the parsed body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    /// The error message the response carries, under either casing a protocol may render
    /// the `message` member with.
    fn message(&self) -> String {
        for key in ["message", "Message"] {
            if let Some(text) = self.body.get(key).and_then(Value::as_str) {
                return text.to_string();
            }
        }
        panic!("expected an error message in {:?}", self.body)
    }

    /// The `PipelineEndpoints` list of a listing answer.
    fn endpoints(&self, case: &str) -> Vec<Value> {
        self.body["PipelineEndpoints"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a `PipelineEndpoints` list in {:?}",
                    self.body
                )
            })
    }

    /// The endpoint ids a listing answer reports, in the order it reported them.
    fn endpoint_ids(&self, case: &str) -> Vec<String> {
        self.endpoints(case)
            .iter()
            .map(|endpoint| {
                endpoint["EndpointId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an endpoint with no EndpointId in {endpoint:?}")
                    })
                    .to_string()
            })
            .collect()
    }

    /// The one endpoint of a listing answer carrying `endpoint_id`, or `None`.
    fn endpoint_with(&self, case: &str, endpoint_id: &str) -> Option<Value> {
        self.endpoints(case)
            .into_iter()
            .find(|endpoint| endpoint["EndpointId"].as_str() == Some(endpoint_id))
    }
}

fn new_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(engine),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Answer {
    let payload = match body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://osis.{REGION}.amazonaws.com{path}"))
        // The credential scope is how a handler learns the region its ARNs name.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/osis/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// The ARN of a pipeline in the signed region, as `CreatePipeline` mints it.
fn arn_of(pipeline_name: &str) -> String {
    format!("arn:aws:osis:{REGION}:{ACCOUNT}:pipeline/{pipeline_name}")
}

/// `base` with its last character replaced by the hex digit of `index`, which keeps the
/// id's length and charset (`\w`) and makes every id of a list distinct.
fn distinct_id(base: &str, index: usize) -> String {
    let digit = char::from_digit(index as u32 % 16, 16).expect("a hex digit");
    let mut id = base.to_string();
    id.truncate(base.len() - 1);
    id.push(digit);
    id
}

/// A `PipelineEndpointVpcOptions` carrying `subnets` subnet ids and `groups` security group
/// ids, built from the id forms above so every element satisfies its own constraints.
fn vpc_options(subnets: usize, groups: usize) -> Value {
    // Each id differs in its last character, so a list names distinct subnets and groups
    // while every element keeps a legal form; the two forms alternate, so a list spans
    // both published lengths.
    let subnet_ids: Vec<String> = (0..subnets)
        .map(|index| {
            let base = if index % 2 == 0 { SHORT_SUBNET } else { LONG_SUBNET };
            distinct_id(base, index)
        })
        .collect();
    let security_group_ids: Vec<String> = (0..groups)
        .map(|index| {
            let base = if index % 2 == 0 { SHORT_GROUP } else { LONG_GROUP };
            distinct_id(base, index)
        })
        .collect();
    json!({ "SubnetIds": subnet_ids, "SecurityGroupIds": security_group_ids })
}

async fn create_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        "/2022-01-01/osis/createPipeline",
        Some(json!({
            "PipelineName": name,
            "MinUnits": 1,
            "MaxUnits": 2,
            "PipelineConfigurationBody":
                "version: \"2\"\nendpoint-pipeline:\n  source:\n    http:\n",
        })),
    )
    .await
}

async fn get_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/getPipeline/{name}"),
        None,
    )
    .await
}

async fn delete_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/2022-01-01/osis/deletePipeline/{name}"),
        None,
    )
    .await
}

/// `CreatePipelineEndpoint` is `POST /2022-01-01/osis/createPipelineEndpoint`, with both
/// members in the body.
async fn create_endpoint(router: &MultiServiceRouter, arn: &str, options: Value) -> Answer {
    call(
        router,
        "POST",
        "/2022-01-01/osis/createPipelineEndpoint",
        Some(json!({ "PipelineArn": arn, "VpcOptions": options })),
    )
    .await
}

/// `ListPipelineEndpoints` is `GET /2022-01-01/osis/listPipelineEndpoints`, with
/// `maxResults` and `nextToken` on the query.
async fn list_endpoints(router: &MultiServiceRouter, query: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/listPipelineEndpoints{query}"),
        None,
    )
    .await
}

async fn list_pipelines(router: &MultiServiceRouter, query: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/listPipelines{query}"),
        None,
    )
    .await
}

/// Create a pipeline and read it until the `CREATING` -> `ACTIVE` transition the clock owes
/// it has been materialised; answer with its ARN and the ingestion endpoint it publishes.
///
/// The budget is a number of READS, not a duration: nothing here sleeps, so a slow host
/// makes the case slower, never different.
async fn active_pipeline(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = create_pipeline(router, name).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreatePipeline, got {} / {:?}",
        created.status, created.body
    );
    for _ in 0..READ_BUDGET {
        let read = get_pipeline(router, name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetPipeline while polling, got {} / {:?}",
            read.status, read.body
        );
        if read.body["Pipeline"]["Status"].as_str() == Some("ACTIVE") {
            let urls = read.body["Pipeline"]["IngestEndpointUrls"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let published = urls
                .first()
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected an advertised ingest endpoint, got {urls:?}"
                    )
                })
                .to_string();
            return (arn_of(name), published);
        }
    }
    panic!("case {case:?}: pipeline {name:?} did not reach ACTIVE within {READ_BUDGET} reads")
}

/// Read the endpoint listing until the endpoint `endpoint_id` reports `want`, the way a
/// client polls a status, and answer with the endpoint as the listing reports it.
async fn poll_endpoint_status(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_id: &str,
    want: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..READ_BUDGET {
        let listed = list_endpoints(router, "").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200 from ListPipelineEndpoints, got {} / {:?}",
            listed.status, listed.body
        );
        if let Some(endpoint) = listed.endpoint_with(case, endpoint_id) {
            if endpoint["Status"].as_str() == Some(want) {
                return endpoint;
            }
            last = endpoint;
        }
    }
    panic!(
        "case {case:?}: endpoint {endpoint_id:?} never reported {want:?} within \
         {READ_BUDGET} reads, last seen {last:?}"
    )
}

/// RULE: a create whose parent ARN resolves to no stored pipeline is refused 404
/// `ResourceNotFoundException`, with the service's fixed wording, and records nothing —
/// whichever segment of the ARN is the one no pipeline carries, and whatever the VPC
/// configuration asks for.
#[tokio::test]
async fn an_endpoint_create_refuses_every_arn_no_pipeline_carries() {
    let router = new_router();
    // One real pipeline exists throughout, so each refusal below is about the ARN the
    // request carried and not about an empty account.
    let (real_arn, _) = active_pipeline(&router, "the one real pipeline", "endpoint-host").await;

    // A pipeline that existed and was deleted: its ARN is well-formed and was live, and
    // names nothing now.
    let (gone_arn, _) = active_pipeline(&router, "a pipeline to delete", "endpoint-gone").await;
    let deleted = delete_pipeline(&router, "endpoint-gone").await;
    assert_eq!(
        deleted.status, 200,
        "expected 200 from DeletePipeline, got {} / {:?}",
        deleted.status, deleted.body
    );

    // (case, the ARN the create names, the VPC configuration it asks for)
    let cases: Vec<(&str, String, Value)> = vec![
        (
            "another pipeline name",
            arn_of("endpoint-ghost"),
            vpc_options(1, 1),
        ),
        (
            "another Region",
            format!("arn:aws:osis:us-east-1:{ACCOUNT}:pipeline/endpoint-host"),
            vpc_options(2, 1),
        ),
        (
            "another account",
            format!("arn:aws:osis:{REGION}:210987654321:pipeline/endpoint-host"),
            vpc_options(1, 2),
        ),
        (
            "another partition",
            format!("arn:aws-cn:osis:{REGION}:{ACCOUNT}:pipeline/endpoint-host"),
            vpc_options(1, 1),
        ),
        (
            "the real name with a suffix",
            format!("{real_arn}-extra"),
            vpc_options(1, 1),
        ),
        (
            "a deleted pipeline's own ARN",
            gone_arn,
            vpc_options(MAX_VPC_IDS, MAX_VPC_IDS),
        ),
    ];

    for (case, arn, options) in &cases {
        let refused = create_endpoint(&router, arn, options.clone()).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404 for {arn:?}, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException for {arn:?}, got {:?} / {:?}",
            refused.error_type, refused.body
        );
        let message = refused.message();
        assert_eq!(
            message, "The pipeline could not be found",
            "case {case:?}: expected the service's not-found wording, got {message:?}"
        );
    }

    // Nothing was recorded by any refused call: the account holds no endpoint at all.
    let listed = list_endpoints(&router, "").await;
    assert_eq!(
        listed.status, 200,
        "expected 200 from ListPipelineEndpoints, got {} / {:?}",
        listed.status, listed.body
    );
    let ids = listed.endpoint_ids("after the refusals");
    assert!(
        ids.is_empty(),
        "expected no endpoint recorded by a refused create, got {ids:?}"
    );
}

/// RULE: a member that fails its own modelled constraint is refused 400
/// `ValidationException` BEFORE the parent is looked up, so a request that is both
/// malformed and names no pipeline is answered on the malformed member; a request that
/// satisfies every constraint reaches the lookup.
#[tokio::test]
async fn a_malformed_request_is_refused_before_the_pipeline_lookup() {
    let router = new_router();
    let ghost = arn_of("endpoint-nowhere");

    // (case, the VPC configuration, the status and code expected)
    let too_many: Vec<String> = (0..=MAX_VPC_IDS)
        .map(|index| format!("subnet-0a1b2c3d4e5f6a7b{digit}", digit = index % 10))
        .collect();
    let cases: Vec<(&str, Value, u16, &str)> = vec![
        (
            "a subnet id that is not one",
            json!({ "SubnetIds": ["not-a-valid-subnet-id"], "SecurityGroupIds": [SHORT_GROUP] }),
            400,
            "ValidationException",
        ),
        (
            "a security group id that is not one",
            json!({ "SubnetIds": [SHORT_SUBNET], "SecurityGroupIds": ["not-a-valid-sg-id"] }),
            400,
            "ValidationException",
        ),
        (
            "both lists malformed",
            json!({
                "SubnetIds": ["not-a-valid-subnet-id"],
                "SecurityGroupIds": ["not-a-valid-sg-id"],
            }),
            400,
            "ValidationException",
        ),
        (
            "one subnet past the published maximum",
            json!({ "SubnetIds": too_many, "SecurityGroupIds": [SHORT_GROUP] }),
            400,
            "ValidationException",
        ),
        (
            "an empty subnet list, below the published minimum",
            json!({ "SubnetIds": [], "SecurityGroupIds": [SHORT_GROUP] }),
            400,
            "ValidationException",
        ),
        (
            "every element legal, at the published maximum",
            vpc_options(MAX_VPC_IDS, MAX_VPC_IDS),
            404,
            "ResourceNotFoundException",
        ),
        (
            "every element legal, at the published minimum",
            vpc_options(1, 1),
            404,
            "ResourceNotFoundException",
        ),
    ];

    for (case, options, status, code) in &cases {
        let answer = create_endpoint(&router, &ghost, options.clone()).await;
        assert_eq!(
            answer.status, *status,
            "case {case:?}: expected HTTP {status}, got {} / {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_type, *code,
            "case {case:?}: expected {code}, got {:?} / {:?}",
            answer.error_type, answer.body
        );
    }

    let listed = list_endpoints(&router, "").await;
    let ids = listed.endpoint_ids("after the malformed creates");
    assert!(
        ids.is_empty(),
        "expected no endpoint recorded by a refused create, got {ids:?}"
    );
}

/// RULE: a create whose parent resolves stores the endpoint, answers from the stored record
/// and is reported by the listing — one record per create, each with its own id and the VPC
/// configuration it was asked for.
#[tokio::test]
async fn an_endpoint_created_for_a_pipeline_is_recorded_and_listed() {
    let router = new_router();

    // (case, pipeline name, how many subnets and security groups the create asks for)
    let cases: Vec<(&str, &str, usize, usize)> = vec![
        ("one subnet and one group", "endpoint-smallest", 1, 1),
        ("a few of each", "endpoint-spread", 3, 2),
        (
            "both lists at the published maximum",
            "endpoint-widest",
            MAX_VPC_IDS,
            MAX_VPC_IDS,
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, name, subnets, groups) in &cases {
        let (arn, published) = active_pipeline(&router, case, name).await;
        let options = vpc_options(*subnets, *groups);
        let created = create_endpoint(&router, &arn, options.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreatePipelineEndpoint, got {} / {:?}",
            created.status, created.body
        );
        let endpoint_id = created.body["EndpointId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an EndpointId in {:?}", created.body))
            .to_string();
        // `PipelineEndpointId` is `@length(3, 512)` with pattern `^[a-zA-Z0-9][a-zA-Z0-9-_]+$`
        // (smithy_query: shape PipelineEndpointId).
        assert!(
            (3..=512).contains(&endpoint_id.len())
                && endpoint_id
                    .starts_with(|first: char| first.is_ascii_alphanumeric())
                && endpoint_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "case {case:?}: endpoint id {endpoint_id:?} is outside what PipelineEndpointId admits"
        );
        assert!(
            minted.insert(endpoint_id.clone()),
            "case {case:?}: endpoint id {endpoint_id:?} was already minted for another endpoint"
        );
        assert_eq!(
            created.body["PipelineArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: expected the create to answer the pipeline's own ARN, got {:?}",
            created.body["PipelineArn"]
        );
        assert_eq!(
            created.body["Status"].as_str(),
            Some("CREATING"),
            "case {case:?}: expected a new endpoint to report CREATING, got {:?}",
            created.body["Status"]
        );

        // Read back through the listing: the endpoint the create minted settles into
        // ACTIVE and carries what the create was asked for.
        let listed = poll_endpoint_status(&router, case, &endpoint_id, "ACTIVE").await;
        assert_eq!(
            listed["PipelineArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: expected the listing to report the pipeline's ARN, got {listed:?}"
        );
        assert_eq!(
            listed["VpcOptions"], options,
            "case {case:?}: expected the VPC configuration the create asked for, got {listed:?}"
        );
        assert_eq!(
            listed["IngestEndpointUrl"].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected the endpoint to publish the pipeline's ingestion \
             endpoint {published:?}, got {listed:?}"
        );
    }

    // A second endpoint for a pipeline that already has one is its own resource, with its
    // own id: the account then reports both.
    let case = "a second endpoint for the same pipeline";
    let arn = arn_of("endpoint-smallest");
    let second = create_endpoint(&router, &arn, vpc_options(2, 2)).await;
    assert_eq!(
        second.status, 200,
        "case {case:?}: expected 200 from CreatePipelineEndpoint, got {} / {:?}",
        second.status, second.body
    );
    let second_id = second.body["EndpointId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EndpointId in {:?}", second.body))
        .to_string();
    assert!(
        minted.insert(second_id.clone()),
        "case {case:?}: expected a distinct id for a second endpoint, got {second_id:?} again"
    );

    let listed = list_endpoints(&router, "").await;
    let reported: BTreeSet<String> = listed.endpoint_ids(case).into_iter().collect();
    assert_eq!(
        reported, minted,
        "case {case:?}: expected the listing to report exactly the endpoints created"
    );
}

/// RULE: the endpoint listing pages over the whole account with the cursor it mints, and a
/// token it did not mint names no position — including one another listing of this service
/// minted.
#[tokio::test]
async fn the_endpoint_listing_pages_the_account_and_refuses_a_foreign_token() {
    let router = new_router();
    let mut expected: BTreeSet<String> = BTreeSet::new();
    for (case, name, endpoints) in [
        ("the first pipeline", "endpoint-paged-one", 2usize),
        ("the second pipeline", "endpoint-paged-two", 1usize),
    ] {
        let (arn, _) = active_pipeline(&router, case, name).await;
        for index in 0..endpoints {
            let created = create_endpoint(&router, &arn, vpc_options(index + 1, 1)).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected 200 from CreatePipelineEndpoint, got {} / {:?}",
                created.status, created.body
            );
            expected.insert(
                created.body["EndpointId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected an EndpointId in {:?}", created.body)
                    })
                    .to_string(),
            );
        }
    }

    // One endpoint per page: every record is reported exactly once over the walk, and the
    // last page carries no token.
    let mut walked: Vec<String> = Vec::new();
    let mut query = "?maxResults=1".to_string();
    for _ in 0..=expected.len() {
        let page = list_endpoints(&router, &query).await;
        assert_eq!(
            page.status, 200,
            "expected 200 from a paged ListPipelineEndpoints, got {} / {:?}",
            page.status, page.body
        );
        let ids = page.endpoint_ids("one endpoint per page");
        assert!(
            ids.len() <= 1,
            "expected at most one endpoint per page, got {ids:?}"
        );
        walked.extend(ids);
        match page.body["NextToken"].as_str() {
            Some(token) => query = format!("?maxResults=1&nextToken={token}"),
            None => break,
        }
    }
    let unique: BTreeSet<String> = walked.iter().cloned().collect();
    assert_eq!(
        walked.len(),
        unique.len(),
        "expected each endpoint reported once over the walk, got {walked:?}"
    );
    assert_eq!(
        unique, expected,
        "expected the walk to report every endpoint of the account, got {walked:?}"
    );

    // A token no endpoint listing minted names no position in it. The pipeline listing's
    // own token is such a token: the two listings page different resources.
    let pipelines = list_pipelines(&router, "?maxResults=1").await;
    let foreign = pipelines.body["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a NextToken from ListPipelines, got {:?}", pipelines.body))
        .to_string();
    for (case, token) in [
        ("a token the pipeline listing minted", foreign.as_str()),
        ("a token no listing minted", "6e6f742d612d746f6b656e"),
        ("a token that is not even hex", "not-a-token"),
    ] {
        let refused = list_endpoints(&router, &format!("?nextToken={token}")).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 for {token:?}, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ValidationException",
            "case {case:?}: expected ValidationException for {token:?}, got {:?} / {:?}",
            refused.error_type, refused.body
        );
    }
}

/// RULE: an endpoint exists only while the pipeline it was created for does, so deleting
/// that pipeline takes its endpoints with it and leaves every other pipeline's alone.
#[tokio::test]
async fn deleting_a_pipeline_takes_its_endpoints_with_it() {
    let router = new_router();
    let case = "two pipelines, one deleted";
    let (doomed, _) = active_pipeline(&router, case, "endpoint-doomed").await;
    let (kept, _) = active_pipeline(&router, case, "endpoint-kept").await;

    let mut doomed_ids: BTreeSet<String> = BTreeSet::new();
    for index in 0..2 {
        let created = create_endpoint(&router, &doomed, vpc_options(index + 1, 1)).await;
        doomed_ids.insert(
            created.body["EndpointId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected an EndpointId in {:?}", created.body)
                })
                .to_string(),
        );
    }
    let kept_endpoint = create_endpoint(&router, &kept, vpc_options(1, 1)).await;
    let kept_id = kept_endpoint.body["EndpointId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an EndpointId in {:?}",
                kept_endpoint.body
            )
        })
        .to_string();

    let deleted = delete_pipeline(&router, "endpoint-doomed").await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected 200 from DeletePipeline, got {} / {:?}",
        deleted.status, deleted.body
    );

    let listed = list_endpoints(&router, "").await;
    let reported: BTreeSet<String> = listed.endpoint_ids(case).into_iter().collect();
    assert_eq!(
        reported,
        BTreeSet::from([kept_id.clone()]),
        "case {case:?}: expected only the surviving pipeline's endpoint {kept_id:?} to be \
         reported, with {doomed_ids:?} gone"
    );
}
