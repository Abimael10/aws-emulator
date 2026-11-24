// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the requirement the `distributions` member carries as a WHOLE:
//! "Each entry defines how output images are distributed in one target Amazon Web Services
//! Region. A Region can appear at most once in the list."
//! (`smithy_query(query="operation",operation="CreateDistributionConfiguration")`).
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a value of `distributions` that breaks a requirement of the member it sits on is refused
//!   as that VALUE: HTTP 400, `InvalidParameterValueException` and the one sentence this
//!   service states a refused parameter in, naming the path of the offending member and the
//!   requirement its value had to satisfy. The list's own requirement is one of them, so a
//!   list naming a target Region twice names the LIST, while a requirement one entry broke
//!   names that entry at its own position;
//! * a value admissible on its own that cannot be combined with another member of the request
//!   keeps the COMBINATION class (`InvalidParameterCombinationException`): a target Region
//!   this service cannot distribute through refuses the entry's AMI settings together with the
//!   Region they would apply in;
//! * `CreateDistributionConfiguration` and `UpdateDistributionConfiguration` read their lists
//!   by the same rule, in the same class and with the same sentence, and a refused request
//!   stores nothing — a refused create leaves its name free, and a refused update leaves the
//!   stored list exactly as it was.
//!
//! The rows span what the rules generalise over: how MANY entries the list holds, how many
//! times a target is named, the DISTANCE between the entries that repeat it, the settings each
//! repeated entry carries, the POSITION of the entry that repeats one, and which requirement
//! of `distributions` the value broke.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The class this service refuses a parameter VALUE with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The class this service refuses a COMBINATION of parameters with — "two mutually exclusive
/// parameters, or a parameter without its required companion parameter"
/// (`smithy_query(query="shape",shape="InvalidParameterCombinationException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombinationException";

/// The sentence a list naming one target Region more than once is refused with: the member
/// whose value was refused, and the requirement that value had to satisfy.
const TARGET_NAMED_TWICE_MESSAGE: &str = "The value supplied for parameter 'distributions' is \
                                          not valid. A region may appear in a distribution \
                                          configuration at most once.";

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
    let body = json!({
        "name": name,
        "clientToken": format!("create-{name}"),
        "distributions": distributions.clone(),
    });
    call(
        router,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&body),
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

/// The `distributions` of a stored configuration, as a read reports it.
fn stored_distributions(case: &str, body: &Value) -> Value {
    body.get("distributionConfiguration")
        .and_then(|configuration| configuration.get("distributions"))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected distributions in {body}"))
}

/// One `distributions` entry naming `region` as its target and distributing an AMI there, with
/// the description its AMI settings carry when it carries one — so two entries for one target
/// can differ in their settings.
fn entry(region: &str, description: Option<&str>) -> Value {
    let mut settings = Map::new();
    if let Some(description) = description {
        settings.insert("description".to_string(), json!(description));
    }
    json!({ "region": region, "amiDistributionConfiguration": Value::Object(settings) })
}

/// A list naming each of `regions` once, distributing an AMI to each.
fn entries(regions: &[&str]) -> Value {
    Value::Array(regions.iter().map(|region| entry(region, None)).collect())
}

/// Assert that `answer` is the refusal `code` carrying `message`.
fn assert_refusal(case: &str, status: u16, answer: &Value, code: &str, message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {answer}"
    );
    let actual = error_code(case, answer);
    assert_eq!(
        actual, code,
        "case {case:?}: expected the error code {code:?}, got {actual:?}"
    );
    let actual = error_message(case, answer);
    assert_eq!(
        actual, message,
        "case {case:?}: expected the message {message:?}, got {actual:?}"
    );
}

/// Every list that names one target Region more than once, as (case, the list).
///
/// The rows differ along what the rule generalises over: the NUMBER of entries, how many times
/// a target is repeated, the distance between the entries that repeat it, the position of the
/// repeating entry, and the settings each of them carries.
fn lists_naming_a_target_twice() -> Vec<(&'static str, Value)> {
    vec![
        (
            "two entries for one target, adjacent",
            entries(&[REGION, REGION]),
        ),
        (
            "two entries for one target, with another between them",
            entries(&["eu-west-1", "us-east-1", "eu-west-1"]),
        ),
        (
            "two entries for one target, five entries apart",
            entries(&[
                "ap-south-1",
                "us-east-1",
                "eu-west-1",
                "eu-central-1",
                "sa-east-1",
                "ap-south-1",
            ]),
        ),
        (
            "three entries for one target",
            entries(&["us-east-2", "us-east-2", "us-east-2"]),
        ),
        (
            "the last two of four entries naming one target",
            entries(&["us-east-1", "eu-west-2", "ap-northeast-1", "eu-west-2"]),
        ),
        (
            "two entries for one target carrying different settings",
            json!([
                entry("ca-central-1", Some("the first setting")),
                entry("ca-central-1", Some("the second setting")),
            ]),
        ),
        (
            "two entries for one target, one of them carrying settings",
            json!([entry("eu-north-1", Some("only here")), entry("eu-north-1", None)]),
        ),
    ]
}

/// RULE: a create whose `distributions` names a target Region more than once is refused as
/// that member's VALUE, with the sentence naming the member and the requirement — and the
/// refused create stores nothing, so the name it asked for is still free.
#[tokio::test]
async fn a_create_naming_a_target_twice_is_refused_as_the_lists_value() {
    for (index, (case, distributions)) in lists_naming_a_target_twice().into_iter().enumerate() {
        let router = emulator();
        let name = format!("twice-create-{index}");
        let (status, answer) = create(&router, &name, &distributions).await;
        assert_refusal(
            case,
            status,
            &answer,
            INVALID_PARAMETER_VALUE,
            TARGET_NAMED_TWICE_MESSAGE,
        );

        // Nothing was stored, so the ARN the name would have carried names no configuration ...
        let arn = arn_of(&name);
        let (status, read) = get(&router, &arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused create to have stored nothing, got {status} \
             with {read}"
        );

        // ... and the name is still free: the same name over a list naming each target once is
        // accepted and reads back as that list.
        let accepted = entries(&[REGION, "eu-west-1"]);
        let (status, created) = create(&router, &name, &accepted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the name to still be free, got {status} with {created}"
        );
        let (status, read) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the accepted create to read back, got {status} with {read}"
        );
        let stored = stored_distributions(case, &read);
        assert_eq!(
            stored, accepted,
            "case {case:?}: expected the stored list {accepted}, got {stored}"
        );
    }
}

/// RULE: the same requirement holds on an UPDATE's replacement list, in the same class and
/// with the same sentence, and the refused update leaves the stored list exactly as it was —
/// after which a valid replacement is stored whole.
#[tokio::test]
async fn an_update_naming_a_target_twice_is_refused_and_changes_nothing() {
    for (index, (case, replacement)) in lists_naming_a_target_twice().into_iter().enumerate() {
        let router = emulator();
        let name = format!("twice-update-{index}");
        let before = entries(&[REGION]);
        let (status, created) = create(&router, &name, &before).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to succeed, got {status} with {created}"
        );
        let arn = arn_of(&name);

        let (status, answer) = update(&router, &arn, "token-twice", &replacement).await;
        assert_refusal(
            case,
            status,
            &answer,
            INVALID_PARAMETER_VALUE,
            TARGET_NAMED_TWICE_MESSAGE,
        );

        let (status, read) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the configuration to still read back, got {status} with \
             {read}"
        );
        let after = stored_distributions(case, &read);
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused update to have changed nothing, got {after}"
        );

        // The refused update filed no receipt either, so the token is still free for a
        // replacement the service accepts — and that one replaces the whole list.
        let accepted = json!([
            entry(REGION, Some("replaced")),
            entry("ap-southeast-2", None),
        ]);
        let (status, answer) = update(&router, &arn, "token-twice", &accepted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the accepted replacement, got {status} with {answer}"
        );
        let (_, read) = get(&router, &arn).await;
        let after = stored_distributions(case, &read);
        assert_eq!(
            after, accepted,
            "case {case:?}: expected the replacement {accepted} to be stored whole, got {after}"
        );
    }
}

/// RULE: which refusal CLASS carries the sentence follows what is wrong with the value, not
/// which requirement it broke: a value breaking a requirement of the member it sits on is
/// refused as that value, while a value admissible on its own that cannot be combined with
/// another member of the request is refused as that combination. Both operations answer the
/// same class and the same sentence for one list.
#[tokio::test]
async fn the_refusal_class_and_sentence_are_the_same_on_both_paths() {
    // (case, the list, the class, the sentence)
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "a list naming one target twice",
            entries(&["us-east-1", "us-east-1"]),
            INVALID_PARAMETER_VALUE,
            TARGET_NAMED_TWICE_MESSAGE.to_string(),
        ),
        (
            "an entry that distributes nothing to its target",
            json!([{ "region": "us-east-1" }]),
            INVALID_PARAMETER_VALUE,
            "The value supplied for parameter 'distributions[0]' is not valid. One or more \
             outputs should be provided for each region in a distribution configuration."
                .to_string(),
        ),
        (
            "the second entry, naming its output AMI by no build",
            json!([
                entry("us-east-1", None),
                { "region": "eu-west-1", "amiDistributionConfiguration": {"name": "golden"} },
            ]),
            INVALID_PARAMETER_VALUE,
            "The value supplied for parameter \
             'distributions[1].amiDistributionConfiguration.name' is not valid. The EC2 AMI \
             name must contain '{{ imagebuilder:buildDate }}' to ensure that the name will be \
             unique."
                .to_string(),
        ),
        (
            "an entry naming a target this service cannot distribute through",
            entries(&["us-nowhere-1"]),
            INVALID_PARAMETER_COMBINATION,
            "The value supplied for parameter \
             'distributions[0].amiDistributionConfiguration' is not valid. AMI Distribution is \
             not available in 'us-nowhere-1'."
                .to_string(),
        ),
        (
            "a target this service cannot reach, named twice, which is read as the entry it is",
            entries(&["us-nowhere-1", "us-nowhere-1"]),
            INVALID_PARAMETER_COMBINATION,
            "The value supplied for parameter \
             'distributions[0].amiDistributionConfiguration' is not valid. AMI Distribution is \
             not available in 'us-nowhere-1'."
                .to_string(),
        ),
        (
            "a target named twice whose second entry distributes nothing",
            json!([entry("us-east-1", None), { "region": "us-east-1" }]),
            INVALID_PARAMETER_VALUE,
            "The value supplied for parameter 'distributions[1]' is not valid. One or more \
             outputs should be provided for each region in a distribution configuration."
                .to_string(),
        ),
    ];

    for (index, (case, distributions, code, message)) in rows.into_iter().enumerate() {
        let router = emulator();

        let created = format!("class-create-{index}");
        let (status, answer) = create(&router, &created, &distributions).await;
        assert_refusal(
            &format!("{case} on the create path"),
            status,
            &answer,
            code,
            &message,
        );

        // The same list reaches the update path through a configuration the service accepted,
        // and is refused in the same class with the same sentence.
        let name = format!("class-update-{index}");
        let (status, answer) = create(&router, &name, &entries(&[REGION])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to succeed, got {status} with {answer}"
        );
        let arn = arn_of(&name);
        let (status, answer) = update(&router, &arn, "token-class", &distributions).await;
        assert_refusal(
            &format!("{case} on the update path"),
            status,
            &answer,
            code,
            &message,
        );
    }
}
