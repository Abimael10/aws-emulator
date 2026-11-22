// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the sentence every lookup of a DISTRIBUTION configuration this
//! service does not hold carries.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: which not-found sentence a refusal carries is the recorded contract
//! of the ROUTE that looked the ARN up. The READ of a distribution configuration — a call
//! addressed by that configuration's own ARN — quotes the ARN it could not resolve by itself:
//! "The resource ARN '<arn>' was not found.", the same sentence the sibling configuration
//! family's read carries, while every route that has to RESOLVE A REFERENCE to one — its
//! delete, its update and a tagging write on it — reports it as the required resource it is,
//! naming the kind its ARN identifies: "The following required resource
//! 'DistributionConfiguration' cannot be found: '<arn>'." The second test pins that boundary
//! on the infrastructure configuration.
//!
//! The rows span what the rule generalises over: which operation performs the lookup, why the
//! ARN resolves to nothing (never created, deleted after an accepted update, a name that only
//! resembles a stored one, a stored configuration reached from another Region's endpoint) and
//! the form of the configuration name the ARN carries.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for unless a case says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, so a configuration of one Region can be asked for through another's
/// endpoint.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The sentence a missing resource is reported in by a route that has to RESOLVE a reference
/// to one: the kind its ARN identifies, and the ARN the request carried.
fn kind_sentence(kind: &str, arn: &str) -> String {
    format!("The following required resource '{kind}' cannot be found: '{arn}'.")
}

/// The sentence a call addressed by a CONFIGURATION's own ARN to READ it is reported in:
/// the ARN alone.
fn arn_sentence(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
}

/// One emulator with an empty store, mounted exactly as the router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(engine),
    );
    router
}

/// A request signed for `imagebuilder` in `region` — the signal the router routes on and the
/// handlers read the Region from.
fn request(
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{region}/imagebuilder/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    builder.body(payload).expect("a well-formed test request")
}

/// Status and parsed body of one call; the error code is folded into the body as `__type`, so
/// the header and the body are read as one.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Percent-encode one path segment, as an AWS SDK encodes a NON-greedy `@httpLabel`: the `:`
/// and `/` of an ARN travel as `%3A` and `%2F`, so the whole ARN stays ONE segment.
fn encode_label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The error code a refusal carries, from `__type`.
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The `message` a refusal carries.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {body}"))
        .to_string()
}

/// The ARN a distribution configuration named `normalized` carries in `region`.
fn distribution_arn(region: &str, normalized: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:distribution-configuration/{normalized}")
}

/// The ARN an infrastructure configuration named `normalized` carries in `region`.
fn infrastructure_arn(region: &str, normalized: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:infrastructure-configuration/{normalized}")
}

/// One `distributions` entry naming `region` as its target, with the AMI settings that are
/// the least an entry may carry.
fn entry(region: &str) -> Value {
    json!({ "region": region, "amiDistributionConfiguration": {} })
}

/// `CreateDistributionConfiguration` for `name` in `region`, answering the ARN it minted.
async fn create_distribution(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("create-{region}-{name}"),
        "distributions": [entry(region)],
    });
    let case = format!("create the distribution configuration {name} in {region}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&body),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {answer}"
    );
    answer
        .get("distributionConfigurationArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the minted ARN in {answer}"))
        .to_string()
}

/// Every lookup of the distribution configuration `arn`, as (operation, method, uri, body,
/// whether that route's sentence names the KIND — false for the read, which quotes the ARN
/// alone).
fn distribution_lookups(
    region: &str,
    arn: &str,
) -> Vec<(String, &'static str, String, Option<Value>, bool)> {
    let name = arn.rsplit('/').next().unwrap_or(arn);
    vec![
        (
            "GetDistributionConfiguration".to_string(),
            "GET",
            format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
            None,
            false,
        ),
        (
            "DeleteDistributionConfiguration".to_string(),
            "DELETE",
            format!("/DeleteDistributionConfiguration?distributionConfigurationArn={arn}"),
            None,
            true,
        ),
        (
            "UpdateDistributionConfiguration".to_string(),
            "PUT",
            "/UpdateDistributionConfiguration".to_string(),
            Some(json!({
                "distributionConfigurationArn": arn,
                "clientToken": format!("update-{region}-{name}"),
                "distributions": [entry(region)],
            })),
            true,
        ),
        (
            "TagResource".to_string(),
            "POST",
            format!("/tags/{}", encode_label(arn)),
            Some(json!({ "tags": { "owner": "platform" } })),
            true,
        ),
        (
            "UntagResource".to_string(),
            "DELETE",
            format!("/tags/{}?tagKeys=owner", encode_label(arn)),
            None,
            true,
        ),
    ]
}

/// Assert `arn` is reported missing by every lookup of a distribution configuration, each in
/// the sentence that route carries.
async fn assert_every_lookup_reports_it_missing(
    router: &MultiServiceRouter,
    row: &str,
    region: &'static str,
    arn: &str,
) {
    for (operation, method, uri, body, names_the_kind) in distribution_lookups(region, arn) {
        let case = format!("{operation} on {row}");
        let (status, answer) = call_in(router, region, method, &uri, body.as_ref()).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {answer}"
        );
        let code = error_code(&case, &answer);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
        );
        let expected = if names_the_kind {
            kind_sentence("DistributionConfiguration", arn)
        } else {
            arn_sentence(arn)
        };
        let actual = error_message(&case, &answer);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a distribution configuration the calling Region holds nothing under is reported
/// missing by every lookup of it, whatever the reason it resolves to nothing — by the ARN
/// alone where the call READS it, and as the required resource it is where the call has to
/// resolve a reference to it.
#[tokio::test]
async fn every_lookup_of_a_missing_distribution_configuration_reports_it_missing() {
    let router = emulator();

    // One stored configuration per name form the modelled pattern admits, each read back, so
    // the rows below report what is absent rather than refusing everything.
    let mut stored = Vec::new();
    for name in ["kept-one", "kept_two_2", "k33"] {
        let arn = create_distribution(&router, REGION, name).await;
        let case = format!("the configuration {name} read back after its create");
        let (status, read) = call_in(
            &router,
            REGION,
            "GET",
            &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {read}"
        );
        stored.push(arn);
    }

    // One configuration that WAS stored, updated and then deleted, so a row can look up an
    // ARN that lived through the operation this suite is about.
    let dead = create_distribution(&router, REGION, "updated-then-deleted").await;
    let (status, updated) = call_in(
        &router,
        REGION,
        "PUT",
        "/UpdateDistributionConfiguration",
        Some(&json!({
            "distributionConfigurationArn": dead,
            "clientToken": "update-before-the-delete",
            "distributions": [entry(REGION), entry(OTHER_REGION)],
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the update before the delete\": expected HTTP 200, got {status} with {updated}"
    );
    let (status, cleared) = call_in(
        &router,
        REGION,
        "DELETE",
        &format!("/DeleteDistributionConfiguration?distributionConfigurationArn={dead}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the delete that clears it\": expected HTTP 200, got {status} with {cleared}"
    );

    // (row, the Region the request is signed for, the ARN it names)
    let rows: Vec<(&str, &'static str, String)> = vec![
        ("a configuration that was updated and then deleted", REGION, dead),
        (
            "a name nothing created",
            REGION,
            distribution_arn(REGION, "absent-one"),
        ),
        (
            "a one-token name nothing created",
            REGION,
            distribution_arn(REGION, "absent_two_2"),
        ),
        (
            "a name that only resembles a stored one",
            REGION,
            distribution_arn(REGION, "kept-one-2"),
        ),
        (
            "a stored configuration asked for through another Region's endpoint",
            OTHER_REGION,
            stored[1].clone(),
        ),
        (
            "a name of another Region nothing created",
            REGION,
            distribution_arn(OTHER_REGION, "k33"),
        ),
    ];

    for (row, region, arn) in rows {
        assert_every_lookup_reports_it_missing(&router, row, region, &arn).await;
    }

    // The configurations the rows did not name are still there, untouched by any of them.
    for arn in &stored[1..] {
        let case = format!("the configuration {arn} left standing");
        let (status, read) = call_in(
            &router,
            REGION,
            "GET",
            &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {read}"
        );
    }
}

/// RULE's boundary: the INFRASTRUCTURE configuration is reported by its ARN alone for its read
/// and for its delete — where the distribution configuration's DELETE names the kind — while a
/// tagging write on either names the kind, so the sentence follows the route rather than the
/// family.
#[tokio::test]
async fn the_infrastructure_configuration_is_still_reported_by_its_arn_alone() {
    let router = emulator();

    // (case, method, the request target)
    let absent = infrastructure_arn(REGION, "absent-infra");
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a read of an infrastructure configuration nothing created",
            "GET",
            format!("/GetInfrastructureConfiguration?infrastructureConfigurationArn={absent}"),
        ),
        (
            "a delete of an infrastructure configuration nothing created",
            "DELETE",
            format!("/DeleteInfrastructureConfiguration?infrastructureConfigurationArn={absent}"),
        ),
    ];

    for (case, method, uri) in rows {
        let (status, answer) = call_in(&router, REGION, method, &uri, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {answer}"
        );
        let expected = arn_sentence(&absent);
        let actual = error_message(case, &answer);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );
    }

    // A tagging write on that same ARN names the kind, so the ARN-only sentence belongs to
    // the two operations above rather than to every call that mentions the configuration.
    let case = "a tagging write naming an infrastructure configuration nothing created";
    let (status, answer) = call_in(
        &router,
        REGION,
        "POST",
        &format!("/tags/{}", encode_label(&absent)),
        Some(&json!({ "tags": { "owner": "platform" } })),
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {answer}"
    );
    let expected = kind_sentence("InfrastructureConfiguration", &absent);
    let actual = error_message(case, &answer);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );
}
