// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the OUTPUTS one entry of a distribution configuration has to
//! configure.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a `Distribution` entry "defines the settings for a specific Region" and each entry of
//!   `distributions` "defines how output images are distributed in one target Amazon Web
//!   Services Region" (`smithy_query(query="shape",shape="Distribution")`), so an entry that
//!   carries its target Region and NONE of the members that distribute an output there
//!   describes no distribution at all and is refused — by the entry's own position — while an
//!   entry carrying any one of those members is accepted;
//! * a refused create and a refused update store nothing: the name a refused create asked for
//!   is still free for the next create, and the list a refused update carried never replaces
//!   the stored one.
//!
//! The rows span what the rules generalise over: WHICH output member an entry carries (every
//! member of `Distribution` but `region`), how many entries that member holds when it is a
//! list, and the POSITION of the entry a refusal addresses.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The error this service refuses a parameter value with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The requirement clause an entry that configures no output is refused with.
const NO_OUTPUTS_REQUIREMENT: &str = "One or more outputs should be provided for each region \
                                      in a distribution configuration.";

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

/// Status and parsed body of one call; the error code is folded into the body as `__type`, so
/// the header and the body are read as one.
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

/// `CreateDistributionConfiguration` for `name` over `distributions`.
async fn create(router: &MultiServiceRouter, name: &str, distributions: &Value) -> (u16, Value) {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("create-{name}")));
    body.insert("distributions".to_string(), distributions.clone());
    call(
        router,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&Value::Object(body)),
    )
    .await
}

/// `UpdateDistributionConfiguration` replacing `arn`'s list with `distributions`.
async fn update(
    router: &MultiServiceRouter,
    arn: &str,
    token: &str,
    distributions: &Value,
) -> (u16, Value) {
    let body = json!({
        "distributionConfigurationArn": arn,
        "clientToken": token,
        "distributions": distributions.clone(),
    });
    call(
        router,
        "PUT",
        "/UpdateDistributionConfiguration",
        Some(&body),
    )
    .await
}

/// `GetDistributionConfiguration` for `arn`.
async fn get(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
        None,
    )
    .await
}

/// The ARN a distribution configuration named `normalized` carries in [`REGION`].
fn arn_of(normalized: &str) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:distribution-configuration/{normalized}")
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

/// The `message` of a refusal.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message member in {body}"))
        .to_string()
}

/// The sentence an entry at `index` that configures no output is refused with.
fn no_outputs_message(index: usize) -> String {
    format!(
        "The value supplied for parameter 'distributions[{index}]' is not valid. \
         {NO_OUTPUTS_REQUIREMENT}"
    )
}

/// The `distributions` of a stored configuration, as a read reports it.
fn stored_distributions(case: &str, body: &Value) -> Value {
    body.get("distributionConfiguration")
        .and_then(|configuration| configuration.get("distributions"))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected distributions in {body}"))
}

/// One `distributions` entry naming `region` as its target and carrying `output`: the one
/// member of the entry under test, or none at all.
fn entry(region: &str, output: Option<(&str, Value)>) -> Value {
    let mut members = Map::new();
    members.insert("region".to_string(), json!(region));
    if let Some((member, value)) = output {
        members.insert(member.to_string(), value);
    }
    Value::Object(members)
}

/// Every member of `Distribution` that configures an output of a build in the entry's target
/// Region — the shape's members but `region`
/// (`smithy_query(query="shape",shape="Distribution")`) — each with a value the model admits.
///
/// Every enumerated value is the model's own: `targetRepository.service` is the one value of
/// `ContainerRepositoryService` (`ECR`), `diskImageFormat` one of `DiskImageFormat` (`VMDK`,
/// `RAW`, `VHD`) and `dataType` one of `SsmParameterDataType` (`text`, `aws:ec2:image`).
fn output_members() -> Vec<(&'static str, Value)> {
    vec![
        ("amiDistributionConfiguration", json!({})),
        (
            "amiDistributionConfiguration",
            json!({ "name": "golden-{{ imagebuilder:buildDate }}" }),
        ),
        (
            "containerDistributionConfiguration",
            json!({
                "containerTags": ["latest"],
                "targetRepository": {"service": "ECR", "repositoryName": "team/app"},
            }),
        ),
        (
            "licenseConfigurationArns",
            json!([format!(
                "arn:aws:license-manager:{REGION}:{ACCOUNT}:license-configuration:\
                 lic-00000000000000000000000000000001"
            )]),
        ),
        (
            "launchTemplateConfigurations",
            json!([{ "launchTemplateId": "lt-00000000000000001", "setDefaultVersion": true }]),
        ),
        (
            "s3ExportConfiguration",
            json!({
                "roleName": "vmimport",
                "diskImageFormat": "VMDK",
                "s3Bucket": "exports",
            }),
        ),
        (
            "fastLaunchConfigurations",
            json!([{ "enabled": true, "maxParallelLaunches": 6 }]),
        ),
        (
            "ssmParameterConfigurations",
            json!([{ "parameterName": "/images/golden", "dataType": "aws:ec2:image" }]),
        ),
    ]
}

/// RULE: an entry carrying any one of the members that distribute an output to its target is
/// accepted, whichever member it is, and the stored configuration reports that entry back.
#[tokio::test]
async fn an_entry_carrying_one_output_is_stored() {
    let router = emulator();

    for (index, (member, value)) in output_members().into_iter().enumerate() {
        let case = format!("an entry carrying {member} ({index})");
        let name = format!("one-output-{index}");
        let distributions = json!([entry(REGION, Some((member, value)))]);
        let (status, created) = create(&router, &name, &distributions).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {created}"
        );

        let arn = arn_of(&name);
        let (status, read) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the stored configuration to read back, got {status} with \
             {read}"
        );
        let stored = stored_distributions(&case, &read);
        assert_eq!(
            stored, distributions,
            "case {case:?}: expected the stored distributions {distributions}, got {stored}"
        );
    }
}

/// RULE: an entry that configures NO output is refused as that entry's own value, at the
/// entry's position — and the refused create stores nothing, so the name it asked for is
/// still free for a create that does configure an output.
#[tokio::test]
async fn an_entry_without_outputs_refuses_the_create_and_stores_nothing() {
    // (case, the entries the request carried, the position refused)
    let rows: Vec<(&str, Value, usize)> = vec![
        (
            "one entry naming only its target",
            json!([entry(REGION, None)]),
            0,
        ),
        (
            "the second entry, after one carrying AMI settings",
            json!([
                entry(REGION, Some(("amiDistributionConfiguration", json!({})))),
                entry("eu-west-1", None),
            ]),
            1,
        ),
        (
            "the third entry, after two carrying different outputs",
            json!([
                entry(
                    REGION,
                    Some((
                        "s3ExportConfiguration",
                        json!({
                            "roleName": "vmimport",
                            "diskImageFormat": "RAW",
                            "s3Bucket": "exports",
                        }),
                    )),
                ),
                entry(
                    "eu-west-1",
                    Some((
                        "licenseConfigurationArns",
                        json!([format!(
                            "arn:aws:license-manager:eu-west-1:{ACCOUNT}:license-configuration:\
                             lic-00000000000000000000000000000002"
                        )]),
                    )),
                ),
                entry("ap-south-1", None),
            ]),
            2,
        ),
        // `ssmParameterConfigurations` is the one output member whose list the model bounds
        // at no minimum length (`smithy_query(query="shape",shape="Distribution")`), so a
        // list holding nothing reaches this rule rather than the modelled-constraint walk —
        // and a member holding nothing configures no output.
        (
            "an entry whose SSM parameter list holds nothing",
            json!([entry(
                REGION,
                Some(("ssmParameterConfigurations", json!([])))
            )]),
            0,
        ),
        (
            "an entry with no output whose target names no Region either",
            json!([entry("us-nowhere-1", None)]),
            0,
        ),
    ];

    for (index, (case, distributions, position)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("no-outputs-{index}");
        let (status, answer) = create(&router, &name, &distributions).await;

        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answer}"
        );
        let code = error_code(case, &answer);
        assert_eq!(
            code, INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the error code {INVALID_PARAMETER_VALUE:?}, got {code:?}"
        );
        let message = error_message(case, &answer);
        let expected = no_outputs_message(position);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // The refused create stored nothing, so the ARN its name would have carried names no
        // configuration ...
        let arn = arn_of(&name);
        let (status, read) = get(&router, &arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused create to have stored nothing, got {status} \
             with {read}"
        );

        // ... and the name is still free: a create of the SAME name carrying an output
        // succeeds and answers the ARN and the client token, rather than being refused as a
        // duplicate of the create AWS never accepted.
        let valid = json!([entry(
            REGION,
            Some(("amiDistributionConfiguration", json!({})))
        )]);
        let (status, created) = create(&router, &name, &valid).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the name to still be free, got {status} with {created}"
        );
        let answered_arn = created
            .get("distributionConfigurationArn")
            .and_then(Value::as_str);
        assert_eq!(
            answered_arn,
            Some(arn.as_str()),
            "case {case:?}: expected the create to answer the ARN {arn:?}, got \
             {answered_arn:?}"
        );
        let token = created.get("clientToken").and_then(Value::as_str);
        assert_eq!(
            token,
            Some(format!("create-{name}").as_str()),
            "case {case:?}: expected the create to echo its client token, got {token:?}"
        );
    }
}

/// RULE: the same requirement holds on an UPDATE's replacement list, and a refused update
/// leaves the stored list exactly as it was.
#[tokio::test]
async fn an_entry_without_outputs_refuses_the_update_and_changes_nothing() {
    // (case, the replacement list, the position refused)
    let rows: Vec<(&str, Value, usize)> = vec![
        (
            "a replacement entry naming only its target",
            json!([entry(REGION, None)]),
            0,
        ),
        (
            "a second replacement entry carrying no output",
            json!([
                entry(
                    REGION,
                    Some((
                        "amiDistributionConfiguration",
                        json!({ "name": "{{ imagebuilder:buildDate }}-app" })
                    ))
                ),
                entry("eu-central-1", None),
            ]),
            1,
        ),
        (
            "a replacement entry whose SSM parameter list holds nothing",
            json!([entry(
                REGION,
                Some(("ssmParameterConfigurations", json!([])))
            )]),
            0,
        ),
    ];

    for (index, (case, replacement, position)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("refused-update-{index}");
        let stored = json!([entry(
            REGION,
            Some(("amiDistributionConfiguration", json!({})))
        )]);
        let (status, created) = create(&router, &name, &stored).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to succeed, got {status} with {created}"
        );
        let arn = arn_of(&name);

        let (status, answer) = update(&router, &arn, "update-token", &replacement).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answer}"
        );
        let code = error_code(case, &answer);
        assert_eq!(
            code, INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the error code {INVALID_PARAMETER_VALUE:?}, got {code:?}"
        );
        let message = error_message(case, &answer);
        let expected = no_outputs_message(position);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        let (status, read) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the configuration to still read back, got {status} with \
             {read}"
        );
        let after = stored_distributions(case, &read);
        assert_eq!(
            after, stored,
            "case {case:?}: expected the refused update to have changed nothing, got {after}"
        );
        // A refused update filed no receipt either, so its token is still free for a request
        // the service accepts.
        let accepted = json!([
            entry(
                REGION,
                Some(("amiDistributionConfiguration", json!({})))
            ),
            entry(
                "eu-central-1",
                Some(("amiDistributionConfiguration", json!({})))
            ),
        ]);
        let (status, answer) = update(&router, &arn, "update-token", &accepted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the token to still be free, got {status} with {answer}"
        );
    }
}
