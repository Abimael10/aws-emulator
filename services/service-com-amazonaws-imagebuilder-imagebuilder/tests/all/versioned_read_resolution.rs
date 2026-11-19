// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for how a single-resource READ of a versioned resource resolves
//! the ARN it is given against the versions this service's store holds at that moment.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rule under test: a read resolves its ARN against the versions the store holds NOW.
//! An ARN naming an exact version (or one build of it) names that one record, and an ARN
//! whose trailing version nodes are the `x` wildcard names whichever stored version is
//! greatest among those it admits — "You can use the `x` wildcard in trailing version
//! positions to retrieve the latest matching version, for example `x.x.x` or `1.x.x`"
//! (`smithy_query(query="doc",shape="GetImageRecipeRequest",member="imageRecipeArn")`, and
//! the same sentence on `GetComponentRequest.componentBuildVersionArn`). So a version a
//! delete removed is reached by nothing: a read of its own ARN reports it missing — HTTP
//! 404, `x-amzn-errortype: ResourceNotFoundException` and the sentence naming the kind the
//! ARN identifies together with the ARN the REQUEST carried — a wildcard falls back to the
//! greatest version still stored, and once every version a wildcard admits is gone the
//! wildcard itself is reported missing.
//!
//! The rows span what the rule generalises over: which versioned family the read acts on
//! (an image recipe, a component build version), the form of the ARN the request carries
//! (an exact version, the record's own ARN, and a wildcard at the patch, minor and major
//! node), how many versions are still stored, which one was deleted, and why an ARN
//! resolves to nothing at all (deleted, never created, a name nothing created, another
//! Region's endpoint).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for unless a case says otherwise; the ARNs the creates
/// mint carry it.
const REGION: &str = "us-west-2";

/// A second Region, so a stored resource of one Region can be asked for through another's
/// endpoint.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, which is all a component a recipe here
/// includes needs.
const BUILD_DOCUMENT: &str = concat!(
    "name: HelloWorld\n",
    "schemaVersion: 1.0\n",
    "phases:\n",
    "  - name: build\n",
    "    steps:\n",
    "      - name: Greet\n",
    "        action: ExecuteBash\n",
    "        inputs:\n",
    "          commands:\n",
    "            - echo hello\n",
);

/// A base image in one of the forms `parentImage` admits (an AMI ID).
const PARENT_IMAGE: &str = "ami-0123456789abcdef0";

/// The semantic versions each family stores, in the order they are created: two of one
/// major version and one of the next, so a wildcard has something to choose between at
/// every node.
const VERSIONS: [&str; 3] = ["1.0.0", "1.1.0", "2.0.0"];

/// The resource-type token a missing-resource report names, as this model names that kind
/// of resource.
const IMAGE_RECIPE_KIND: &str = "ImageRecipe";

/// The resource-type token the component family carries.
const COMPONENT_KIND: &str = "Component";

/// One versioned family of this service, addressed the way its own operations address it.
struct Family {
    /// What this family is, named in every assertion its rows make.
    case: &'static str,
    /// The resource-type token its not-found report names.
    kind: &'static str,
    /// The resource prefix its ARNs carry.
    arn_prefix: &'static str,
    /// The name every version stored for this family carries.
    name: &'static str,
    /// The read operation and the member it names the resource by.
    read: &'static str,
    /// The delete operation and the member it names the resource by.
    delete: &'static str,
    /// The output member a read answers the resource in.
    member: &'static str,
}

/// The two versioned families whose single-resource read a teardown uses as the authority
/// on whether the resource still exists.
fn families() -> [Family; 2] {
    [
        Family {
            case: "an image recipe",
            kind: IMAGE_RECIPE_KIND,
            arn_prefix: "image-recipe",
            name: "resolved-recipe",
            read: "/GetImageRecipe?imageRecipeArn=",
            delete: "/DeleteImageRecipe?imageRecipeArn=",
            member: "imageRecipe",
        },
        Family {
            case: "a component build version",
            kind: COMPONENT_KIND,
            arn_prefix: "component",
            name: "resolved-component",
            read: "/GetComponent?componentBuildVersionArn=",
            delete: "/DeleteComponent?componentBuildVersionArn=",
            member: "component",
        },
    ]
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

/// A request signed for `imagebuilder` in `region` — the signal the router routes on and
/// the handlers read the Region from.
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

/// What one answer carries: its status, the `x-amzn-errortype` header a refusal is shaped
/// by, and its parsed body. An empty body reads as `Value::Null`.
struct Answer {
    /// The HTTP status.
    status: u16,
    /// The `x-amzn-errortype` header, when the answer carries one.
    error_type: Option<String>,
    /// The parsed body.
    body: Value,
}

/// Make one call through the router, signed for `region`.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let body = if raw.is_empty() {
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

/// Make one call signed for [`REGION`].
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    call_in(router, REGION, method, uri, body).await
}

/// The text one member of an answer carries.
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} in {body}"))
        .to_string()
}

/// The error class a refusal carries, from `__type` (restJson1 renders the shape there,
/// optionally namespace-qualified).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The sentence a missing resource of a versioned family is reported in: the kind its ARN
/// identifies, and the ARN the request carried.
fn missing_message(kind: &str, arn: &str) -> String {
    format!("The following required resource '{kind}' cannot be found: '{arn}'.")
}

/// The ARN the resource `name` carries at `version` in `region`, under `prefix` — a
/// version ARN, which carries no build node.
fn version_arn(region: &str, prefix: &str, name: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:{prefix}/{name}/{version}")
}

/// `CreateComponent` for one build version of `name` at `version`, answering the build
/// version ARN it minted.
async fn create_component(router: &MultiServiceRouter, name: &str, version: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "platform": "Linux",
        "clientToken": format!("component-{name}-{version}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name} at {version}");
    let answer = call(router, "PUT", "/CreateComponent", Some(&body)).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {}",
        answer.status, answer.body
    );
    text(&case, &answer.body, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` at `version` over `component`, answering the ARN it
/// minted.
async fn create_image_recipe(
    router: &MultiServiceRouter,
    name: &str,
    version: &str,
    component: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "parentImage": PARENT_IMAGE,
        "clientToken": format!("recipe-{name}-{version}"),
        "components": [{ "componentArn": component }],
    });
    let case = format!("create the image recipe {name} at {version}");
    let answer = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {}",
        answer.status, answer.body
    );
    text(&case, &answer.body, "imageRecipeArn")
}

/// Store one version of `family`, answering the record's OWN ARN — which for a component
/// names the build the create minted, and for a recipe is the version ARN itself.
async fn store_version(
    router: &MultiServiceRouter,
    family: &Family,
    version: &str,
    base_component: &str,
) -> String {
    if family.kind == COMPONENT_KIND {
        create_component(router, family.name, version).await
    } else {
        create_image_recipe(router, family.name, version, base_component).await
    }
}

/// Assert a read of `reference` resolves to the record `expected` names.
async fn assert_resolves(
    router: &MultiServiceRouter,
    family: &Family,
    row: &str,
    reference: &str,
    expected: &str,
) {
    let case = format!("{row} of {}", family.case);
    let answer = call(router, "GET", &format!("{}{reference}", family.read), None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 for {reference}, got {} with {}",
        answer.status, answer.body
    );
    let resource = answer
        .body
        .get(family.member)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {} in {}", family.member, answer.body));
    let actual = resource.get("arn").and_then(Value::as_str);
    assert_eq!(
        actual,
        Some(expected),
        "case {case:?}: expected the read of {reference} to resolve to {expected:?}, got \
         {actual:?}"
    );
}

/// Assert a read of `reference` through `region`'s endpoint reports it missing, in the
/// modelled not-found error: the status, the shaped error type on the header and in the
/// body, and the sentence naming the kind together with the ARN the request carried.
async fn assert_missing_in(
    router: &MultiServiceRouter,
    family: &Family,
    row: &str,
    region: &str,
    reference: &str,
) {
    let case = format!("{row} of {}", family.case);
    let answer = call_in(
        router,
        region,
        "GET",
        &format!("{}{reference}", family.read),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected HTTP 404 for {reference}, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected the ResourceNotFoundException error type on the header, got \
         {:?}",
        answer.error_type
    );
    let code = error_code(&case, &answer.body);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} with {}",
        answer.body
    );
    let expected = missing_message(family.kind, reference);
    let actual = text(&case, &answer.body, "message");
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );
}

/// Assert a read of `reference` through [`REGION`]'s endpoint reports it missing.
async fn assert_missing(
    router: &MultiServiceRouter,
    family: &Family,
    row: &str,
    reference: &str,
) {
    assert_missing_in(router, family, row, REGION, reference).await;
}

/// Delete one version of `family` by the record's own ARN, and assert the delete was
/// accepted. What the delete ECHOES is `resource_delete_lifecycle.rs`'s rule, so the rows
/// here read what the store holds afterwards rather than re-asserting the echo.
async fn delete_version(router: &MultiServiceRouter, family: &Family, arn: &str) {
    let case = format!("delete {} {arn}", family.case);
    let answer = call(
        router,
        "DELETE",
        &format!("{}{arn}", family.delete),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {}",
        answer.status, answer.body
    );
}

/// RULE: a read resolves its ARN against the versions the store holds NOW — so a wildcard
/// answers the greatest version still stored, a version a delete removed is reached by
/// nothing, and a wildcard whose every matching version is gone is reported missing too.
#[tokio::test]
async fn a_read_resolves_a_version_arn_against_the_versions_still_stored() {
    let router = emulator();
    // The component the recipes of this suite include. It is a family of its own, so the
    // rows that delete component versions never touch it.
    let base = create_component(&router, "resolution-base", "1.0.0").await;

    for family in families() {
        let mut stored = Vec::new();
        for version in VERSIONS {
            stored.push(store_version(&router, &family, version, &base).await);
        }
        let arn_at = |version: &str| version_arn(REGION, family.arn_prefix, family.name, version);

        // (row, the ARN form the request carries, the version it resolves to) with every
        // version stored: a wildcard answers the greatest version it admits, and an exact
        // version ARN answers that version whether or not it is the greatest.
        let resolved: [(&str, String, usize); 6] = [
            ("the full wildcard", arn_at("x.x.x"), 2),
            ("the first major's wildcard", arn_at("1.x.x"), 1),
            ("the first minor's wildcard", arn_at("1.0.x"), 0),
            ("the second minor's wildcard", arn_at("1.1.x"), 1),
            ("the exact greatest version", arn_at(VERSIONS[2]), 2),
            ("the record's own ARN", stored[0].clone(), 0),
        ];
        for (row, reference, version) in &resolved {
            assert_resolves(&router, &family, row, reference, &stored[*version]).await;
        }

        // The greatest version goes: its own ARN is reached by nothing, a wildcard that
        // admitted only it is reported missing, and the wildcards that admit an older
        // version fall back to the greatest one left.
        delete_version(&router, &family, &stored[2]).await;
        assert_missing(&router, &family, "the deleted greatest version", &stored[2]).await;
        assert_missing(
            &router,
            &family,
            "the wildcard of the emptied major version",
            &arn_at("2.x.x"),
        )
        .await;
        assert_resolves(
            &router,
            &family,
            "the full wildcard after the greatest version went",
            &arn_at("x.x.x"),
            &stored[1],
        )
        .await;
        assert_resolves(
            &router,
            &family,
            "the first major's wildcard after the greatest version went",
            &arn_at("1.x.x"),
            &stored[1],
        )
        .await;

        // The middle version goes next: the full wildcard falls back again, to the only
        // version left.
        delete_version(&router, &family, &stored[1]).await;
        assert_missing(&router, &family, "the deleted middle version", &stored[1]).await;
        assert_missing(
            &router,
            &family,
            "the wildcard of the emptied minor version",
            &arn_at("1.1.x"),
        )
        .await;
        assert_resolves(
            &router,
            &family,
            "the full wildcard with one version left",
            &arn_at("x.x.x"),
            &stored[0],
        )
        .await;

        // The last version goes: every ARN form the family admits is now reached by
        // nothing.
        delete_version(&router, &family, &stored[0]).await;
        for (row, reference) in [
            ("the last version's own ARN", stored[0].clone()),
            ("the full wildcard with nothing left", arn_at("x.x.x")),
            ("the major wildcard with nothing left", arn_at("1.x.x")),
            ("the minor wildcard with nothing left", arn_at("1.0.x")),
            ("the exact version with nothing left", arn_at(VERSIONS[0])),
        ] {
            assert_missing(&router, &family, row, &reference).await;
        }
    }
}

/// RULE, on the resources a teardown asks about: whatever the reason an ARN resolves to
/// nothing — deleted, never created, a name nothing created, or a stored resource asked
/// for through another Region's endpoint — the read reports it in the one modelled
/// not-found error, naming the ARN the request carried; and reporting it leaves the
/// resources that ARE stored readable.
#[tokio::test]
async fn every_read_an_arn_resolves_to_nothing_for_reports_the_modelled_not_found_error() {
    let router = emulator();
    let base = create_component(&router, "survivor-base", "1.0.0").await;

    for family in families() {
        // One version that is deleted, and one that stands through every row below.
        let deleted = store_version(&router, &family, VERSIONS[0], &base).await;
        let kept = store_version(&router, &family, VERSIONS[1], &base).await;
        delete_version(&router, &family, &deleted).await;

        // (row, the Region the request is signed for, the ARN it names)
        let rows: [(&str, &str, String); 5] = [
            ("the deleted version", REGION, deleted.clone()),
            (
                "a version nothing created",
                REGION,
                version_arn(REGION, family.arn_prefix, family.name, "9.9.9"),
            ),
            (
                "a name nothing created",
                REGION,
                version_arn(REGION, family.arn_prefix, "absent-resource", VERSIONS[0]),
            ),
            ("a stored version through another Region's endpoint", OTHER_REGION, kept.clone()),
            (
                "a name of another Region nothing created",
                REGION,
                version_arn(OTHER_REGION, family.arn_prefix, family.name, VERSIONS[1]),
            ),
        ];
        for (row, region, reference) in &rows {
            assert_missing_in(&router, &family, row, region, reference).await;
        }

        // A repeat read answers exactly the same: a read reports the store, it does not
        // change it.
        assert_missing(&router, &family, "the deleted version read again", &deleted).await;

        // The version the rows did not delete is still there, untouched by any of them.
        assert_resolves(
            &router,
            &family,
            "the version left standing",
            &kept,
            &kept,
        )
        .await;
    }
}
