// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the output AMI name a distribution configuration names.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handler — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: `amiDistributionConfiguration.name` is "the name of the output AMI.
//! The name must include the `{{ imagebuilder:buildDate }}` dynamic tag so that each build
//! produces a uniquely named AMI"
//! (`smithy_query(query="doc",shape="AmiDistributionConfiguration",member="name")`), so a
//! name the tag is missing from is refused as that member's own value — naming the member at
//! the position of the entry that carried it — while a name carrying the tag, and an entry
//! naming no AMI name at all, are stored and read back.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The error code this service refuses the value of one parameter with: the modelled
/// `InvalidParameterValueException`, "The value that you provided for the specified parameter
/// is invalid."
const EXPECTED_CODE: &str = "InvalidParameterValueException";

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

/// A request signed for `imagebuilder` in [`REGION`].
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

/// Status, error code and parsed body of one call. The code is read from the
/// `x-amzn-errortype` header, which restJson1 names the error shape in.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Option<String>, Value) {
    let response = router.dispatch(request(method, uri, body)).await;
    let status = response.status().as_u16();
    let code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .rsplit(['#', '/'])
                .next()
                .unwrap_or(value)
                .to_string()
        });
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, code, parsed)
}

/// `CreateDistributionConfiguration` for a configuration named `name` over `distributions`.
async fn create(
    router: &MultiServiceRouter,
    name: &str,
    distributions: Value,
) -> (u16, Option<String>, Value) {
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
async fn get(router: &MultiServiceRouter, arn: &str) -> (u16, Option<String>, Value) {
    call(
        router,
        "GET",
        &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
        None,
    )
    .await
}

/// One distribution entry for `region` whose AMI settings name `ami_name`.
fn entry(region: &str, ami_name: &str) -> Value {
    json!({
        "region": region,
        "amiDistributionConfiguration": { "name": ami_name },
    })
}

/// The `message` of a refusal.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message member in {body}"))
        .to_string()
}

/// One row of the refusal table: the case, the entries as (target, AMI name), and the
/// zero-based position of the entry a refusal addresses.
type Row<'row> = (&'row str, Vec<(&'row str, &'row str)>, usize);

/// RULE: an output AMI name that does not include the build-date tag is refused as that
/// member's value, at the position of the entry that carried it, and nothing is stored.
///
/// The rows differ along what the rule generalises over: the FORM of the name that does not
/// name each build (a bare name, a name carrying another dynamic tag, a name carrying a
/// partial tag) and the POSITION of the entry that carried it.
#[tokio::test]
async fn an_ami_name_without_the_build_date_tag_refuses_the_create() {
    // (case, the entries as (target, AMI name), the position refused)
    let rows: Vec<Row<'_>> = vec![
        ("a bare name", vec![("us-west-2", "golden")], 0),
        (
            "a name carrying another dynamic tag",
            vec![("us-west-2", "golden-{{ imagebuilder:buildVersion }}")],
            0,
        ),
        (
            "a name carrying the tag's text outside its braces",
            vec![("us-west-2", "golden-imagebuilder:buildDate")],
            0,
        ),
        (
            "the second entry's name, after one that names each build",
            vec![
                ("us-west-2", "first-{{ imagebuilder:buildDate }}"),
                ("eu-west-1", "second"),
            ],
            1,
        ),
        (
            "the third entry's name, after two that name each build",
            vec![
                ("us-west-2", "{{ imagebuilder:buildDate }}-one"),
                ("eu-west-1", "{{imagebuilder:buildDate}}-two"),
                ("ap-southeast-3", "three"),
            ],
            2,
        ),
    ];

    for (index, (case, entries, position)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("unnamed-build-{index}");
        let distributions = Value::Array(
            entries
                .iter()
                .map(|(region, ami_name)| entry(region, ami_name))
                .collect(),
        );
        let (status, code, body) = create(&router, &name, distributions).await;

        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            code.as_deref(),
            Some(EXPECTED_CODE),
            "case {case:?}: expected the error code {EXPECTED_CODE:?}, got {code:?}"
        );
        let expected = format!(
            "The value supplied for parameter \
             'distributions[{position}].amiDistributionConfiguration.name' is not valid. The \
             EC2 AMI name must contain '{{{{ imagebuilder:buildDate }}}}' to ensure that the \
             name will be unique."
        );
        let message = error_message(case, &body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // A refused create stores nothing: the ARN its name would have carried resolves to
        // no configuration.
        let arn =
            format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:distribution-configuration/{name}");
        let (read_status, _, read_body) = get(&router, &arn).await;
        assert_eq!(
            read_status, 404,
            "case {case:?}: expected the refused create to have stored nothing, got HTTP \
             {read_status} with {read_body}"
        );
    }
}

/// RULE: a name that includes the build-date tag names each build, whatever else it carries
/// and however the tag is spaced — and AMI settings naming no output AMI name at all leave
/// the naming to Image Builder, so both are stored and read back.
#[tokio::test]
async fn a_name_that_names_each_build_is_stored() {
    // (case, the AMI name the entry named, or none at all)
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("AMI settings naming no AMI name at all", None),
        (
            "the tag as the documentation writes it",
            Some("golden-{{ imagebuilder:buildDate }}"),
        ),
        (
            "the tag written compactly",
            Some("golden-{{imagebuilder:buildDate}}"),
        ),
        ("the tag alone", Some("{{ imagebuilder:buildDate }}")),
        (
            "the tag ahead of the rest of the name",
            Some("{{ imagebuilder:buildDate }}-golden"),
        ),
    ];

    for (index, (case, ami_name)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("named-build-{index}");
        let distributions = match ami_name {
            Some(ami_name) => json!([entry("us-west-2", ami_name)]),
            None => json!([{ "region": "us-west-2", "amiDistributionConfiguration": {} }]),
        };
        let (status, code, body) = create(&router, &name, distributions).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} ({code:?}) with {body}"
        );

        let arn = body
            .get("distributionConfigurationArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN in {body}"))
            .to_string();
        let (read_status, _, read_body) = get(&router, &arn).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected the stored configuration to read back, got HTTP \
             {read_status} with {read_body}"
        );
        let stored = read_body
            .get("distributionConfiguration")
            .and_then(|configuration| configuration.get("distributions"))
            .and_then(Value::as_array)
            .and_then(|entries| entries.first())
            .and_then(|entry| entry.get("amiDistributionConfiguration"))
            .and_then(|settings| settings.get("name"))
            .and_then(Value::as_str);
        assert_eq!(
            stored, ami_name,
            "case {case:?}: expected the stored AMI name {ami_name:?}, got {stored:?}"
        );
    }
}
