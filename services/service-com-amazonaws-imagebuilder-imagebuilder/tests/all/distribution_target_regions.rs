// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the target Region a distribution configuration names.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handler — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: a `distributions` entry's `region` is "the target Region" the
//! entry's settings apply in, so an entry whose value names no Region this service
//! distributes through has no target to distribute to and the create is refused — the AMI
//! distribution of THAT entry is reported as unavailable in the Region the request named, by
//! the entry's own position — while every entry naming one of those Regions is stored and
//! read back.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

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

/// A request signed for `imagebuilder` in [`REGION`] — the signal the router routes on and
/// the handlers read the Region from.
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

/// What one call answered: the status, the `x-amzn-errortype` header, and the parsed body.
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

/// Call `uri` and read back the whole answer.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let response = router.dispatch(request(method, uri, body)).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// `CreateDistributionConfiguration` for a configuration named `name` over `distributions`.
async fn create(router: &MultiServiceRouter, name: &str, distributions: Value) -> Answer {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("token-{name}")));
    body.insert("distributions".to_string(), distributions);
    call(
        router,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&Value::Object(body)),
    )
    .await
}

/// `GetDistributionConfiguration` for `arn`.
async fn get(router: &MultiServiceRouter, arn: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
        None,
    )
    .await
}

/// The error code a refusal carries, read from the `x-amzn-errortype` header and the body's
/// `__type` together: restJson1 names the shape in both, so a refusal that names it in only
/// one of them is itself a defect.
fn error_code(case: &str, answer: &Answer) -> String {
    let header = answer
        .error_type
        .as_deref()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an x-amzn-errortype header, got {:?}",
                answer.body
            )
        })
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string();
    let rendered = answer
        .body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an error shape in the body, got {:?}",
                answer.body
            )
        })
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string();
    assert_eq!(
        header, rendered,
        "case {case:?}: expected the header and the body to name one shape, got {header:?} \
         and {rendered:?}"
    );
    header
}

/// The `message` of a refusal.
fn error_message(case: &str, answer: &Answer) -> String {
    answer
        .body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a message member, got {:?}",
                answer.body
            )
        })
        .to_string()
}

/// The error code this service refuses an unreachable target Region with: the AMI settings
/// of the entry together with the Region they would apply in are a combination of parameters
/// this operation does not accept.
const EXPECTED_CODE: &str = "InvalidParameterCombinationException";

/// The sentence a refusal of the entry at `index` naming `region` carries.
fn expected_message(index: usize, region: &str) -> String {
    format!(
        "The value supplied for parameter 'distributions[{index}].amiDistributionConfiguration' \
         is not valid. AMI Distribution is not available in '{region}'."
    )
}

/// RULE: a `distributions` entry whose `region` names no Region this service distributes
/// through names no target, so the create is refused with that entry's AMI distribution
/// reported unavailable in the Region the request named — and nothing is stored.
///
/// The rows differ along what the rule generalises over: the FORM of the value that names no
/// Region of this partition (case, a separator, a label past the length a Region field
/// admits, a bare word, a well-formed label of a Region that does not exist, a Region of
/// another partition), and the POSITION of the entry that carries it.
#[tokio::test]
async fn an_entry_naming_no_region_refuses_the_create() {
    // (case, the targets the request named, the position refused, the value refused)
    let rows: Vec<(&str, Vec<&str>, usize, &str)> = vec![
        (
            "the only target, in upper case",
            vec!["US-EAST-2"],
            0,
            "US-EAST-2",
        ),
        (
            "the only target, in mixed case",
            vec!["Eu-West-1"],
            0,
            "Eu-West-1",
        ),
        (
            "the second target, after a Region of the partition",
            vec!["us-west-2", "AP-SOUTHEAST-3"],
            1,
            "AP-SOUTHEAST-3",
        ),
        (
            "the third target, after two Regions of the partition",
            vec!["us-west-2", "eu-central-1", "US_WEST_1"],
            2,
            "US_WEST_1",
        ),
        (
            "a target carrying a separator an endpoint authority would re-point on",
            vec!["us-west-2/elsewhere"],
            0,
            "us-west-2/elsewhere",
        ),
        (
            "a target longer than the Region field admits",
            vec!["us-west-2-us-west-2-us"],
            0,
            "us-west-2-us-west-2-us",
        ),
        (
            "a target that is a word rather than a Region",
            vec!["Oregon"],
            0,
            "Oregon",
        ),
        (
            "a well-formed label of a Region that does not exist",
            vec!["us-nowhere-1"],
            0,
            "us-nowhere-1",
        ),
        (
            "a Region code with its number changed",
            vec!["us-west-2", "eu-west-9"],
            1,
            "eu-west-9",
        ),
        (
            // The catalog holds the other partitions' Regions, and a distribution of this
            // one reaches none of them.
            "a GovCloud Region",
            vec!["us-gov-west-1"],
            0,
            "us-gov-west-1",
        ),
        (
            "a China Region, after a Region of this partition",
            vec!["us-west-2", "cn-north-1"],
            1,
            "cn-north-1",
        ),
        (
            "two targets naming no Region, the first of them refused",
            vec!["US-EAST-2", "EU-WEST-1"],
            0,
            "US-EAST-2",
        ),
    ];

    for (index, (case, targets, position, refused)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("unreachable-target-{index}");
        let distributions = Value::Array(
            targets
                .iter()
                .map(|region| json!({ "region": region, "amiDistributionConfiguration": {} }))
                .collect(),
        );
        let answer = create(&router, &name, distributions).await;

        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        let code = error_code(case, &answer);
        assert_eq!(
            code, EXPECTED_CODE,
            "case {case:?}: expected the error code {EXPECTED_CODE:?}, got {code:?}"
        );
        let message = error_message(case, &answer);
        let expected = expected_message(position, refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // A refused create stores nothing: the ARN its name would have carried resolves to
        // no configuration.
        let arn = format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:distribution-configuration/{name}");
        let read = get(&router, &arn).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the refused create to have stored nothing, got HTTP {} \
             with {:?}",
            read.status, read.body
        );
    }
}

/// RULE: every entry naming a Region this service distributes through is a target, so the
/// create succeeds and the stored configuration reports those targets back.
///
/// The rows differ along how many targets one configuration names and which Regions they
/// are, including the Region the request itself was signed for and Regions no fixture of this
/// service names.
#[tokio::test]
async fn every_entry_naming_a_region_of_the_partition_is_stored() {
    // (case, the targets the request named)
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("one target, the signing Region itself", vec!["us-west-2"]),
        ("one target elsewhere", vec!["eu-central-1"]),
        (
            "three targets across three areas",
            vec!["us-east-1", "ap-southeast-3", "eu-west-1"],
        ),
        // Regions read straight off this tree's one Region catalog
        // (`emulator_telemetry::policy::KNOWN_REGIONS`, which
        // `crate::state::distribution_target` reads a target against), which no fixture of
        // this service names: a target is decided by that catalog, not by one recording.
        (
            "three targets no fixture of this service names",
            vec!["af-south-1", "me-central-1", "mx-central-1"],
        ),
    ];

    for (index, (case, targets)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("reachable-target-{index}");
        let distributions = Value::Array(
            targets
                .iter()
                .map(|region| json!({ "region": region, "amiDistributionConfiguration": {} }))
                .collect(),
        );
        let answer = create(&router, &name, distributions).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );

        let arn = answer
            .body
            .get("distributionConfigurationArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN in {:?}", answer.body))
            .to_string();
        let read = get(&router, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the stored configuration to read back, got HTTP {} with \
             {:?}",
            read.status, read.body
        );
        let stored: Vec<String> = read
            .body
            .get("distributionConfiguration")
            .and_then(|configuration| configuration.get("distributions"))
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected distributions in {:?}", read.body))
            .iter()
            .map(|entry| {
                entry
                    .get("region")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            stored, targets,
            "case {case:?}: expected the stored targets {targets:?}, got {stored:?}"
        );
    }
}
