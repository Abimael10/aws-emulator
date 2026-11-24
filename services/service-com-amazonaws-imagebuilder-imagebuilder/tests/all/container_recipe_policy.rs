// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the container recipe's resource-policy pair,
//! `PutContainerRecipePolicy` and `GetContainerRecipePolicy`.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes. The write carries its members in a JSON body; the read
//! carries the recipe's ARN in a query parameter, as its modelled `@httpQuery` puts it.
//!
//! The rule under test: a resource policy is ATTACHED to the resource the write's ARN names,
//! so the read of that resource reports the document the last successful write applied — and
//! the unset-policy document `{}` where no write has applied one. A call naming an ARN the
//! calling Region holds no such resource under reports the resource missing (HTTP 404
//! `ResourceNotFoundException`, naming the kind the ARN identifies and the ARN the request
//! carried), and a document that is not parseable JSON is refused as an invalid value of the
//! `policy` member, leaving whatever policy the resource already carried in place.
//!
//! The rows span what the rule generalises over: the SHAPE of the document applied (how many
//! statements, how it is indented, whether a principal arrives as a list or a bare string),
//! how many writes one recipe has seen, which recipe of several the write names, which Region
//! the call is signed for, and the way a document fails to be JSON.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for unless it is checking that an endpoint reaches only its own
/// Region's resources.
const REGION: &str = "us-east-1";

/// A second Region, for the cases that check regional isolation.
const OTHER_REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The document a policy read reports for a recipe no policy has been applied to.
const UNSET_POLICY: &str = "{}";

/// An inline Dockerfile template, which is all a recipe these cases attach a policy to needs.
const INLINE_TEMPLATE: &str = "FROM {{{ imagebuilder:parentImage }}}\n";

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

/// A request signed for `imagebuilder` in `region`.
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

/// Status and parsed body of one call made in `region`.
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

/// Status and parsed body of one call made in [`REGION`].
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    call_in(router, REGION, method, uri, body).await
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

/// The text one member of a response carries.
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {name} of {body}"))
        .to_string()
}

/// Assert a call succeeded, and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// Assert the answer is a refusal carrying `status`, `code` and `message`.
fn assert_refused(case: &str, status: u16, body: &Value, expected: u16, code: &str, message: &str) {
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected}, got {status} with {body}"
    );
    let actual_code = error_code(case, body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected {code:?}, got {actual_code:?} with {body}"
    );
    let actual_message = text(case, body, "message");
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// The sentence this service reports a missing required resource with.
fn missing_recipe_message(arn: &str) -> String {
    format!("The following required resource 'ContainerRecipe' cannot be found: '{arn}'.")
}

/// The ARN of the container recipe `normalized` names at `version` in `region`.
fn recipe_arn(region: &str, normalized: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:container-recipe/{normalized}/{version}")
}

/// `CreateContainerRecipe` for one recipe of `name` at `version` in `region`, under the
/// idempotency token `token`, answering the ARN it minted.
async fn create_recipe_in(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    version: &str,
    token: &str,
) -> String {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("semanticVersion".to_string(), json!(version));
    body.insert("containerType".to_string(), json!("DOCKER"));
    body.insert("parentImage".to_string(), json!("amazonlinux:latest"));
    body.insert("clientToken".to_string(), json!(token));
    body.insert(
        "targetRepository".to_string(),
        json!({ "service": "ECR", "repositoryName": "team-a/my-repo" }),
    );
    body.insert(
        "dockerfileTemplateData".to_string(),
        json!(INLINE_TEMPLATE),
    );
    let case = format!("create the container recipe {name} {version} in {region}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/CreateContainerRecipe",
        Some(&Value::Object(body)),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "containerRecipeArn")
}

/// `CreateContainerRecipe` in [`REGION`], under a token naming the recipe it creates.
async fn create_recipe(router: &MultiServiceRouter, name: &str, version: &str) -> String {
    create_recipe_in(
        router,
        REGION,
        name,
        version,
        &format!("token-{name}-{version}"),
    )
    .await
}

/// `PutContainerRecipePolicy` applying `document` to `arn`, in `region`.
async fn put_policy_in(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    document: &str,
) -> (u16, Value) {
    let body = json!({ "containerRecipeArn": arn, "policy": document });
    call_in(
        router,
        region,
        "PUT",
        "/PutContainerRecipePolicy",
        Some(&body),
    )
    .await
}

/// `PutContainerRecipePolicy` in [`REGION`].
async fn put_policy(router: &MultiServiceRouter, arn: &str, document: &str) -> (u16, Value) {
    put_policy_in(router, REGION, arn, document).await
}

/// `GetContainerRecipePolicy` for `arn`, in `region`.
async fn get_policy_in(router: &MultiServiceRouter, region: &str, arn: &str) -> (u16, Value) {
    call_in(
        router,
        region,
        "GET",
        &format!("/GetContainerRecipePolicy?containerRecipeArn={arn}"),
        None,
    )
    .await
}

/// `GetContainerRecipePolicy` in [`REGION`].
async fn get_policy(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    get_policy_in(router, REGION, arn).await
}

/// The policy document a read in [`REGION`] reports for `arn`.
async fn reported_policy(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let (status, answer) = get_policy(router, arn).await;
    let answer = assert_ok(case, status, answer);
    text(case, &answer, "policy")
}

/// A share policy whose `Action` list carries `action` and whose principal is `principal`,
/// written compactly — the form a read reports.
fn share_policy(action: &str, principal: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{principal}"}},"Action":["{action}"],"Resource":"*"}}]}}"#
    )
}

#[tokio::test]
async fn a_policy_is_attached_to_the_container_recipe_its_arn_names() {
    let router = emulator();
    let arn = create_recipe(&router, "Policy Recipe", "1.0.0").await;
    assert_eq!(
        arn,
        recipe_arn(REGION, "policy-recipe", "1.0.0"),
        "expected the create to mint the recipe's ARN, got {arn:?}"
    );

    // Before any write, the read reports the unset-policy document rather than failing.
    let before = reported_policy(&router, "before any write", &arn).await;
    assert_eq!(
        before, UNSET_POLICY,
        "case \"before any write\": expected {UNSET_POLICY:?}, got {before:?}"
    );

    // (case, the document a write applies, what a read then reports for it)
    let writes: &[(&str, String, String)] = &[
        // A one-statement share policy, as a caller writes one.
        (
            "a share policy",
            share_policy("imagebuilder:GetContainerRecipe", "111111111111"),
            share_policy("imagebuilder:GetContainerRecipe", "111111111111"),
        ),
        // A second write REPLACES the first rather than merging into it.
        (
            "a replacement policy",
            share_policy("imagebuilder:GetContainerRecipe", "222222222222"),
            share_policy("imagebuilder:GetContainerRecipe", "222222222222"),
        ),
        // A document a caller indented is stored without the whitespace between its JSON
        // tokens, which is the form a read reports.
        (
            "an indented document",
            "{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": []\n}".to_string(),
            r#"{"Version":"2012-10-17","Statement":[]}"#.to_string(),
        ),
        // A principal given as a one-element list says what the bare string says, and is
        // reported as that string.
        (
            "a one-element principal list",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["333333333333"]},"Action":["imagebuilder:GetContainerRecipe"],"Resource":"*"}]}"#.to_string(),
            share_policy("imagebuilder:GetContainerRecipe", "333333333333"),
        ),
        // Two statements stay two statements, in the order the document carries them.
        (
            "two statements",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"444444444444"},"Action":["imagebuilder:GetContainerRecipe"],"Resource":"*"},{"Effect":"Allow","Principal":{"AWS":"555555555555"},"Action":["imagebuilder:ListContainerRecipes"],"Resource":"*"}]}"#.to_string(),
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"444444444444"},"Action":["imagebuilder:GetContainerRecipe"],"Resource":"*"},{"Effect":"Allow","Principal":{"AWS":"555555555555"},"Action":["imagebuilder:ListContainerRecipes"],"Resource":"*"}]}"#.to_string(),
        ),
        // The unset-policy document written explicitly is still a document.
        (
            "the empty document",
            UNSET_POLICY.to_string(),
            UNSET_POLICY.to_string(),
        ),
    ];

    for (case, document, reported) in writes {
        let (status, answer) = put_policy(&router, &arn, document).await;
        let answer = assert_ok(case, status, answer);
        // The write names the recipe it wrote to, read back off the stored record.
        let named = text(case, &answer, "containerRecipeArn");
        assert_eq!(
            named, arn,
            "case {case:?}: expected the write to name {arn:?}, got {named:?}"
        );
        // Every successful call identifies itself.
        let request_id = text(case, &answer, "requestId");
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected a request id, got {request_id:?}"
        );

        let actual = reported_policy(&router, case, &arn).await;
        assert_eq!(
            actual, *reported,
            "case {case:?}: expected {reported:?}, got {actual:?}"
        );

        // Putting back the document the read reported stores the same document again.
        let (status, answer) = put_policy(&router, &arn, &actual).await;
        assert_ok(case, status, answer);
        let again = reported_policy(&router, case, &arn).await;
        assert_eq!(
            again, actual,
            "case {case:?}: expected re-applying the reported document to change nothing, got \
             {again:?}"
        );
    }
}

#[tokio::test]
async fn each_container_recipe_carries_its_own_policy() {
    let router = emulator();
    // Two recipes of one name at different versions, and a third of another name: a policy
    // reaches exactly the recipe its ARN names.
    let first = create_recipe(&router, "shared-recipe", "1.0.0").await;
    let second = create_recipe(&router, "shared-recipe", "2.0.0").await;
    let third = create_recipe(&router, "other-recipe", "1.0.0").await;

    let document = share_policy("imagebuilder:GetContainerRecipe", "111111111111");
    let (status, answer) = put_policy(&router, &second, &document).await;
    assert_ok("the recipe written to", status, answer);

    for (case, arn, expected) in [
        ("the recipe written to", &second, document.as_str()),
        ("the earlier version of the same name", &first, UNSET_POLICY),
        ("a recipe of another name", &third, UNSET_POLICY),
    ] {
        let actual = reported_policy(&router, case, arn).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // Deleting the recipe releases the policy with it: the read reports the recipe missing,
    // and a recipe created again under the same ARN carries no policy at all.
    let (status, answer) = call(
        &router,
        "DELETE",
        &format!("/DeleteContainerRecipe?containerRecipeArn={second}"),
        None,
    )
    .await;
    assert_ok("delete the recipe", status, answer);
    let (status, answer) = get_policy(&router, &second).await;
    assert_refused(
        "a deleted recipe",
        status,
        &answer,
        404,
        "ResourceNotFoundException",
        &missing_recipe_message(&second),
    );

    // The same name and version again, under a token of its own, so what answers is a new
    // create rather than the receipt of the first one.
    let recreated = create_recipe_in(
        &router,
        REGION,
        "shared-recipe",
        "2.0.0",
        "token-shared-recipe-2.0.0-again",
    )
    .await;
    assert_eq!(
        recreated, second,
        "expected the recipe created again to carry the same ARN, got {recreated:?}"
    );
    let actual = reported_policy(&router, "a recipe created after the delete", &recreated).await;
    assert_eq!(
        actual, UNSET_POLICY,
        "case \"a recipe created after the delete\": expected {UNSET_POLICY:?}, got {actual:?}"
    );
}

#[tokio::test]
async fn a_policy_call_naming_no_stored_container_recipe_reports_it_missing() {
    let router = emulator();
    let arn = create_recipe(&router, "kept-recipe", "1.0.0").await;

    // The write that establishes what the refusals below must leave alone.
    let stored = share_policy("imagebuilder:GetContainerRecipe", "111111111111");
    let (status, answer) = put_policy(&router, &arn, &stored).await;
    assert_ok("the policy to keep", status, answer);

    // (case, an ARN of a container recipe this Region holds nothing under)
    let absent: &[(&str, String)] = &[
        (
            "a name nothing was stored under",
            recipe_arn(REGION, "absent-recipe", "1.0.0"),
        ),
        (
            "a version nothing was stored under",
            recipe_arn(REGION, "kept-recipe", "9.0.0"),
        ),
        (
            "a recipe of another Region",
            recipe_arn(OTHER_REGION, "kept-recipe", "1.0.0"),
        ),
    ];

    for (case, missing) in absent {
        let expected = missing_recipe_message(missing);
        let (status, answer) = put_policy(&router, missing, &stored).await;
        assert_refused(
            case,
            status,
            &answer,
            404,
            "ResourceNotFoundException",
            &expected,
        );
        let (status, answer) = get_policy(&router, missing).await;
        assert_refused(
            &format!("{case}, read"),
            status,
            &answer,
            404,
            "ResourceNotFoundException",
            &expected,
        );
    }

    // A regional endpoint reaches only its own Region's recipes, so the same ARN that works
    // here is missing there — and the policy this Region holds survives the refusal.
    let (status, answer) = put_policy_in(&router, OTHER_REGION, &arn, &stored).await;
    assert_refused(
        "the owning Region's ARN, called in another Region",
        status,
        &answer,
        404,
        "ResourceNotFoundException",
        &missing_recipe_message(&arn),
    );
    let (status, answer) = get_policy_in(&router, OTHER_REGION, &arn).await;
    assert_refused(
        "the owning Region's ARN, read in another Region",
        status,
        &answer,
        404,
        "ResourceNotFoundException",
        &missing_recipe_message(&arn),
    );

    let surviving = reported_policy(&router, "after the refusals", &arn).await;
    assert_eq!(
        surviving, stored,
        "case \"after the refusals\": expected the stored policy to survive, got {surviving:?}"
    );
}

#[tokio::test]
async fn a_policy_document_that_is_not_json_is_refused() {
    let router = emulator();
    let arn = create_recipe(&router, "guarded-recipe", "1.0.0").await;
    let stored = share_policy("imagebuilder:GetContainerRecipe", "111111111111");
    let (status, answer) = put_policy(&router, &arn, &stored).await;
    assert_ok("the policy to keep", status, answer);

    let malformed = "The value supplied for parameter 'policy' is not valid. Policy is malformed \
                     and cannot be parsed";
    // The ways a document fails to be JSON, over an ARN that names a stored recipe and one
    // that names nothing: the value is refused either way, and nothing is written.
    for (document_case, document) in [
        ("an unterminated object", "{this is not valid json"),
        ("a bare word", "not json at all"),
        ("an unterminated array", "[1, 2"),
        ("a lone closing brace", "}"),
        ("a trailing comma", r#"{"Version":"2012-10-17",}"#),
    ] {
        for (target_case, target) in [
            ("a stored recipe", arn.clone()),
            (
                "a recipe nothing was stored under",
                recipe_arn(REGION, "absent-recipe", "1.0.0"),
            ),
        ] {
            let case = format!("{document_case}, {target_case}");
            let (status, answer) = put_policy(&router, &target, document).await;
            assert_refused(
                &case,
                status,
                &answer,
                400,
                "InvalidParameterValueException",
                malformed,
            );
        }
        let actual = reported_policy(&router, document_case, &arn).await;
        assert_eq!(
            actual, stored,
            "case {document_case:?}: expected the stored policy to survive the refusal, got \
             {actual:?}"
        );
    }
}
