// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests decomposing the not-found refusal a DISTRIBUTION configuration read
//! carries into its fixed text and its one variable part.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what a
//! signed AWS SDK request observes.
//!
//! The rule under test: a read addressing one of this service's ARNs that the calling Region
//! resolves to nothing is refused with the not-found error the model defines
//! (`ResourceNotFoundException`, HTTP 404;
//! `smithy_query(query="shape",shape="ResourceNotFoundException")`), and its message is the
//! required-resource template whose ONLY variable parts are the KIND that ARN identifies and the
//! identifier as the request carried it: "The following required resource '<kind>' cannot be
//! found: '<identifier>'." Nothing else of the request — the Region it was signed for, the client
//! token, the number of target Regions the configuration had — reaches the sentence, and the
//! refusal is unchanged however many times it is asked for, because a read writes nothing.
//!
//! The rows span what the rule generalises over: the form of the configuration name the ARN
//! carries (one character, hyphens, underscores and digits, a long name), the Region and the
//! account field of the ARN (the modelled `DistributionConfigurationArn` pattern
//! `^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws):distribution-configuration/[a-z0-9-_]+$`
//! admits a 12-digit account or the literal `aws`;
//! `smithy_query(query="shape",shape="DistributionConfigurationArn")`), and why the ARN resolves
//! to nothing — never created, or created with distributions in several Regions, read back and
//! then deleted.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) a minted ARN carries.
const ACCOUNT: &str = "000000000000";

/// The fixed text that opens the required-resource sentence, up to the kind it names. Spelled out
/// here rather than taken from the crate, so a reworded refusal fails.
const TEMPLATE_HEAD: &str = "The following required resource '";

/// The fixed text between the kind and the identifier.
const TEMPLATE_MIDDLE: &str = "' cannot be found: '";

/// The fixed text that closes the sentence after the identifier.
const TEMPLATE_TAIL: &str = "'.";

/// The kind token a distribution-configuration ARN identifies: the model's own shape name for
/// that resource (`smithy_query(query="shape",shape="DistributionConfiguration")`).
const KIND: &str = "DistributionConfiguration";

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

/// A request signed for `imagebuilder` in [`REGION`] — the signal the router routes on and the
/// handlers read the Region from.
fn request(method: &str, uri: &str, body: Option<&Value>) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{REGION}/imagebuilder/\
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

/// Status and parsed body of one call; the error code travels in `x-amzn-errortype`, so it is
/// folded into the body as `__type` and the header and the body are read as one.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(method, uri, body)).await;
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

/// `GET /GetDistributionConfiguration` for `arn`.
async fn read(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
        None,
    )
    .await
}

/// Assert the read of `arn` is refused as a missing required resource, with the kind and the ARN
/// as the sentence's only variable parts, and answer the message it carried.
async fn assert_reported_missing(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let (status, answer) = read(router, arn).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {answer}"
    );
    let code = error_code(case, &answer);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
    );
    let actual = error_message(case, &answer);
    let expected = format!("{TEMPLATE_HEAD}{KIND}{TEMPLATE_MIDDLE}{arn}{TEMPLATE_TAIL}");
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );

    // The same sentence, read as template plus variable parts: the fixed text is where it
    // belongs and the stretch between the kind and the close is the ARN the request carried,
    // character for character.
    let opened = actual.strip_prefix(TEMPLATE_HEAD).unwrap_or_else(|| {
        panic!("case {case:?}: expected the message to open with {TEMPLATE_HEAD:?}, got {actual:?}")
    });
    let closed = opened.strip_suffix(TEMPLATE_TAIL).unwrap_or_else(|| {
        panic!("case {case:?}: expected the message to close with {TEMPLATE_TAIL:?}, got {actual:?}")
    });
    let (kind, identifier) = closed.split_once(TEMPLATE_MIDDLE).unwrap_or_else(|| {
        panic!("case {case:?}: expected {TEMPLATE_MIDDLE:?} in the message, got {actual:?}")
    });
    assert_eq!(
        kind, KIND,
        "case {case:?}: expected the kind {KIND:?}, got {kind:?} in {actual:?}"
    );
    assert_eq!(
        identifier, arn,
        "case {case:?}: expected the identifier {arn:?}, got {identifier:?} in {actual:?}"
    );
    actual
}

/// The ARN a distribution configuration named `name` carries in `region` under `account`.
fn distribution_arn(region: &str, account: &str, name: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{account}:distribution-configuration/{name}")
}

/// One `distributions` entry naming `region` as its target, with the AMI settings that are the
/// least an entry may carry.
fn entry(region: &str) -> Value {
    json!({ "region": region, "amiDistributionConfiguration": {} })
}

/// RULE: every read that resolves a distribution-configuration ARN to nothing reports it in the
/// required-resource sentence whose only variable parts are the kind and that ARN — whatever form
/// the ARN's name, Region and account field take.
#[tokio::test]
async fn the_missing_distribution_configuration_sentence_varies_only_in_the_arn() {
    let router = emulator();

    // (case, the ARN the read names). The name forms span the modelled pattern's character
    // class, and the account field spans both of its alternatives.
    let rows: Vec<(&str, String)> = vec![
        (
            "a one-character name nothing created",
            distribution_arn(REGION, ACCOUNT, "k"),
        ),
        (
            "a hyphenated name nothing created",
            distribution_arn(REGION, ACCOUNT, "absent-distribution-configuration"),
        ),
        (
            "a name of underscores and digits nothing created",
            distribution_arn(REGION, ACCOUNT, "absent_2_config_7"),
        ),
        (
            "a long name nothing created",
            distribution_arn(
                REGION,
                ACCOUNT,
                "absent-distribution-configuration-with-a-much-longer-generated-name-0123456789",
            ),
        ),
        (
            "a name of another Region nothing created",
            distribution_arn("eu-west-1", ACCOUNT, "absent-elsewhere"),
        ),
        (
            "the account field's service-owned alternative",
            distribution_arn(REGION, "aws", "absent-service-owned"),
        ),
    ];

    let mut sentences = Vec::new();
    for (case, arn) in &rows {
        sentences.push(assert_reported_missing(&router, case, arn).await);
    }

    // Two rows differ in the ARN alone, so their sentences differ in the ARN alone: strip each
    // ARN out and what is left is one and the same fixed text.
    for (index, (case, arn)) in rows.iter().enumerate() {
        let skeleton = sentences[index].replace(arn.as_str(), "<arn>");
        let expected = format!("{TEMPLATE_HEAD}{KIND}{TEMPLATE_MIDDLE}<arn>{TEMPLATE_TAIL}");
        assert_eq!(
            skeleton, expected,
            "case {case:?}: expected the fixed text {expected:?}, got {skeleton:?}"
        );
    }
}

/// RULE, over the lifecycle the sentence is reached through: a configuration created with
/// distributions in several Regions reads back while it is stored and, once deleted, is reported
/// missing in that same sentence — repeatably, because the read writes nothing.
#[tokio::test]
async fn a_deleted_multi_region_distribution_configuration_is_reported_missing() {
    let router = emulator();

    // (case, the target Regions the configuration distributes to) — the count the sentence must
    // be independent of.
    let rows: Vec<(&str, &[&str])> = vec![
        ("one target Region", &["us-west-2"]),
        ("two target Regions", &["us-west-2", "eu-west-1"]),
        (
            "three target Regions",
            &["us-west-2", "eu-west-1", "ap-southeast-2"],
        ),
    ];

    for (case, regions) in rows {
        let name = format!("multi-{}", regions.len());
        let (status, created) = call(
            &router,
            "PUT",
            "/CreateDistributionConfiguration",
            Some(&json!({
                "name": name,
                "clientToken": format!("create-{name}"),
                "distributions": regions.iter().map(|region| entry(region)).collect::<Vec<_>>(),
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the create, got {status} with {created}"
        );
        let arn = created
            .get("distributionConfigurationArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected the minted ARN in {created}"))
            .to_string();

        // Stored, so the read answers the configuration rather than the refusal.
        let (status, read_back) = read(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read back, got {status} with {read_back}"
        );
        let stored_regions = read_back
            .get("distributionConfiguration")
            .and_then(|configuration| configuration.get("distributions"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_else(|| panic!("case {case:?}: expected distributions in {read_back}"));
        assert_eq!(
            stored_regions,
            regions.len(),
            "case {case:?}: expected {} distributions, got {stored_regions}",
            regions.len()
        );

        let (status, deleted) = call(
            &router,
            "DELETE",
            &format!("/DeleteDistributionConfiguration?distributionConfigurationArn={arn}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the delete, got {status} with {deleted}"
        );

        // Asked for twice: a read changes nothing, so the second answer is the first.
        let first = assert_reported_missing(&router, &format!("{case}, read once"), &arn).await;
        let second = assert_reported_missing(&router, &format!("{case}, read again"), &arn).await;
        assert_eq!(
            second, first,
            "case {case:?}: expected the repeated read to carry {first:?}, got {second:?}"
        );
    }
}
