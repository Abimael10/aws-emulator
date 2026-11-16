// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals this service words itself: the ARN an operation takes,
//! the recipe `CreateComponentVersion` publishes from, the duplicate it collides with, and
//! the vendor of a tagging route's ARN.
//!
//! The rules under test:
//!
//! * an operation whose `arn` is a `ComponentVersionARN` refuses a value outside that shape
//!   with the modelled `ValidationException` (HTTP 400) and IoT Greengrass's own sentence,
//!   `Invalid Input: Component version ARN <arn> is invalid` — the offending value copied
//!   from the request and nothing else;
//! * `ListComponentVersions`, whose `arn` is a `ComponentARN`, states the same sentence with
//!   its own noun phrase (`Component ARN`), and TAKES either ARN that names one component:
//!   the component ARN `ListComponents` reports and the component-VERSION ARN
//!   `CreateComponentVersion` returns, which name the same component and therefore list the
//!   same versions, page for page;
//! * a recipe `CreateComponentVersion` will not publish is refused with
//!   `AccessDeniedException` (HTTP 403) carrying the recipe diagnosis and then the
//!   authorization sentence naming the caller: a payload that cannot be READ as a document
//!   is diagnosed at the position reading it stopped at, and a document that specifies no
//!   component name or version is diagnosed as that;
//! * re-publishing a version that exists is `ConflictException` (HTTP 409) whose sentence is
//!   chosen by whether the request carried TAGS, and which — when it did not — reports the
//!   state the stored version's own reader reports;
//! * the tagging route reads its `resourceArn`'s vendor field before it looks for any
//!   resource, so an ARN of another service is that layer's own `BadRequestException`
//!   naming the vendor it expected, while an ARN of THIS vendor that names no stored
//!   resource stays the route's not-found class.
//!
//! The rows differ along what the rules generalise over: which operation takes the ARN,
//! which part of the ARN shape the value breaks, which ARN shape a listing is addressed by,
//! how a recipe payload fails, how many tags a colliding create carries, and which vendor a
//! tagging ARN names. Every assertion drives the real `restJson1` front door and is judged
//! on the HTTP status and the JSON body.

use aws_smithy_types::base64;
use serde_json::{json, Value};
use service_com_amazonaws_greengrassv2_greengrassv2::state::{
    STATE_DEPLOYABLE, TAGGING_NOT_FOUND_CODE, TAGGING_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::component_versions_and_resolution::{
    component_arn, emulator, error_code, get, label, listed_versions, post, publish, recipe, text,
    version_arn, ACCOUNT, REGION,
};

/// The code the modelled 400 carries (`smithy_query(query="shape",shape="ValidationException")`:
/// client, HTTP 400).
const VALIDATION_CODE: &str = "ValidationException";

/// The code the modelled 403 carries
/// (`smithy_query(query="shape",shape="AccessDeniedException")`: client, HTTP 403).
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The code the modelled 409 carries (`smithy_query(query="shape",shape="ConflictException")`:
/// client, HTTP 409).
const CONFLICT_CODE: &str = "ConflictException";

/// The principal these requests speak for, as `sts:GetCallerIdentity` reports it for the
/// emulator's own published credential (`service_common::caller_principal`): the recipe
/// refusal names it, and names nothing else.
fn caller() -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/emulator")
}

/// Every operation whose `arn` member is a `ComponentVersionARN`
/// (`smithy_query(query="operation",operation="GetComponent,DescribeComponent,GetComponentVersionArtifact")`),
/// as (the operation, the path it binds for an `arn`).
fn component_version_routes(arn: &str) -> Vec<(&'static str, String)> {
    let label = label(arn);
    vec![
        ("GetComponent", format!("/greengrass/v2/components/{label}")),
        (
            "DescribeComponent",
            format!("/greengrass/v2/components/{label}/metadata"),
        ),
        (
            "GetComponentVersionArtifact",
            format!("/greengrass/v2/components/{label}/artifacts/install.sh"),
        ),
    ]
}

/// Values outside BOTH ARN shapes this service takes: each row breaks a different part of
/// the one grammar the model's ARN patterns state
/// (`smithy_query(query="shape",shape="ComponentVersionARN,ComponentARN")`).
fn values_outside_every_arn_shape() -> Vec<(&'static str, String)> {
    vec![
        ("no ARN at all", "not-a-valid-arn".to_string()),
        ("an empty-ish value", "x".to_string()),
        (
            "another service's ARN",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/Core"),
        ),
        (
            "another vendor's partition-global ARN",
            "arn:aws:s3:::a-bucket".to_string(),
        ),
        (
            "a Greengrass ARN of another resource type",
            format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:deployments:d-1"),
        ),
        (
            "an account field that is neither `aws` nor digits",
            format!("arn:aws:greengrass:{REGION}:my-account:components:com.example.Probe"),
        ),
        (
            "a component field with no name after it",
            format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:components:"),
        ),
        (
            "a `versions` field with no version after it",
            format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:components:com.example.Probe:versions:"),
        ),
        ("the word `arn` alone", "arn".to_string()),
    ]
}

/// RULE: every operation taking a `ComponentVersionARN` refuses a value outside that shape
/// with the modelled 400 and the sentence AWS states about it — whether or not any component
/// exists, since the value is judged before anything is looked up.
#[tokio::test]
async fn a_malformed_component_version_arn_is_refused_with_aws_sentence() {
    let router = emulator();
    // One published version, so no row below is refused because the catalog is empty.
    let (status, body) = publish(&router, &recipe("com.example.Present", "1.0.0")).await;
    assert_eq!(
        status, 201,
        "publishing the fixture version must succeed, got {status} {body}"
    );

    let mut rows = values_outside_every_arn_shape();
    // A component ARN is a well-formed ARN of this service that names a component rather
    // than one of its versions, so these operations take none of it either.
    rows.push((
        "a component ARN, which names no version",
        component_arn("com.example.Present"),
    ));

    for (case, value) in rows {
        for (operation, path) in component_version_routes(&value) {
            let (status, body) = get(&router, &path).await;
            assert_eq!(
                status, 400,
                "case {case:?} on {operation}: a malformed ARN must answer 400, got {status} \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                VALIDATION_CODE,
                "case {case:?} on {operation}: expected {VALIDATION_CODE}, got {body}"
            );
            assert_eq!(
                text(&body, "message"),
                format!("Invalid Input: Component version ARN {value} is invalid"),
                "case {case:?} on {operation}: expected AWS's invalid-ARN sentence, got {body}"
            );
        }
    }
}

/// RULE: `ListComponentVersions` refuses a value that is neither ARN shape with the same
/// sentence under its own noun phrase.
#[tokio::test]
async fn a_malformed_component_arn_is_refused_with_aws_sentence() {
    let router = emulator();
    let (status, body) = publish(&router, &recipe("com.example.Present", "1.0.0")).await;
    assert_eq!(
        status, 201,
        "publishing the fixture version must succeed, got {status} {body}"
    );

    for (case, value) in values_outside_every_arn_shape() {
        let path = format!("/greengrass/v2/components/{}/versions", label(&value));
        let (status, body) = get(&router, &path).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a malformed ARN must answer 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            VALIDATION_CODE,
            "case {case:?}: expected {VALIDATION_CODE}, got {body}"
        );
        assert_eq!(
            text(&body, "message"),
            format!("Invalid Input: Component ARN {value} is invalid"),
            "case {case:?}: expected AWS's invalid-ARN sentence for a component ARN, got {body}"
        );
    }
}

/// Walk every page of `path` and return the versions reported, in order.
async fn walk(router: &MultiServiceRouter, case: &str, path: &str, page_size: usize) -> Vec<String> {
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut uri = format!("{path}?maxResults={page_size}");
        if let Some(cursor) = &token {
            uri.push_str(&format!("&nextToken={cursor}"));
        }
        let (status, body) = get(router, &uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing {uri:?} must answer 200, got {status} {body}"
        );
        seen.extend(listed_versions(&body));
        pages += 1;
        assert!(
            pages <= 8,
            "case {case:?}: paging {path:?} must terminate, got {seen:?}"
        );
        match body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    seen
}

/// RULE: either ARN that names one component lists that component's versions, page for
/// page — so the ARN a create returned is an address a listing takes.
#[tokio::test]
async fn either_arn_shape_lists_the_same_component_versions() {
    let router = emulator();
    let published = ["1.0.0", "1.0.1", "1.0.2"];
    for version in published {
        let (status, body) = publish(&router, &recipe("com.example.Paged", version)).await;
        assert_eq!(
            status, 201,
            "publishing {version} must succeed, got {status} {body}"
        );
    }
    // Greater versions first, which is the order the operation documents.
    let expected: Vec<String> = published.iter().rev().map(|v| (*v).to_string()).collect();

    // The addresses one component can be listed by: its own ARN, and the ARN of each of its
    // versions — every one of which `CreateComponentVersion` handed a client back.
    let mut addresses = vec![("the component ARN", component_arn("com.example.Paged"))];
    for version in published {
        addresses.push((
            "a component-version ARN",
            version_arn("com.example.Paged", version),
        ));
    }

    for (kind, arn) in addresses {
        let case = format!("{kind} {arn}");
        let path = format!("/greengrass/v2/components/{}/versions", label(&arn));
        // One entry per page is the walk the paginated flow makes; the whole collection in
        // one page is the same report in one answer.
        for page_size in [1, 3, 10] {
            let seen = walk(&router, &case, &path, page_size).await;
            assert_eq!(
                seen, expected,
                "case {case:?} in pages of {page_size}: expected every version exactly once, \
                 greatest first, got {seen:?}"
            );
        }
    }
}

/// `CreateComponentVersion` with `payload` as the raw `inlineRecipe` bytes.
async fn publish_bytes(router: &MultiServiceRouter, payload: &[u8]) -> (u16, Value) {
    post(
        router,
        "/greengrass/v2/createComponentVersion",
        &json!({ "inlineRecipe": base64::encode(payload) }),
    )
    .await
}

/// RULE: a recipe-level refusal is the 403 `AccessDeniedException` whose message is the
/// recipe diagnosis followed by the authorization sentence naming the caller. A payload that
/// cannot be read is diagnosed at the position reading it stopped at; a document that was
/// read but specifies no component name or version is diagnosed as that.
#[tokio::test]
async fn a_recipe_refusal_is_the_403_aws_answers() {
    let router = emulator();
    let authorization = format!("User: {} is not authorized to perform: null", caller());
    let identity = "Invalid Input: The component recipe doesn't specify a component name or \
                    version. Add ComponentName and ComponentVersion parameters to the recipe \
                    and try again.";

    // (case, the payload, the diagnosis it is refused with)
    let rows: Vec<(&str, Vec<u8>, String)> = vec![
        (
            "neither JSON nor YAML",
            b"]] not json nor yaml".to_vec(),
            "Invalid recipe format; line: 1, column: 1.".to_string(),
        ),
        (
            "no document at all",
            Vec::new(),
            "Invalid recipe format; line: 1, column: 1.".to_string(),
        ),
        (
            "bytes that are not UTF-8, on the second line",
            b"ComponentName: a\n\xff".to_vec(),
            "Invalid recipe format; line: 2, column: 1.".to_string(),
        ),
        (
            "two documents where a recipe is one",
            b"ComponentName: a\n---\nComponentVersion: 1.0.0\n".to_vec(),
            "Invalid recipe format; line: 1, column: 1.".to_string(),
        ),
        (
            "a document that is a sequence",
            b"[1, 2, 3]".to_vec(),
            identity.to_string(),
        ),
        (
            "a document that is a scalar",
            b"\"just a string\"".to_vec(),
            identity.to_string(),
        ),
        (
            "a mapping with no ComponentName",
            br#"{"RecipeFormatVersion":"2020-01-25","ComponentVersion":"1.0.0"}"#.to_vec(),
            identity.to_string(),
        ),
        (
            "a mapping with no ComponentVersion",
            br#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.X"}"#.to_vec(),
            identity.to_string(),
        ),
        (
            "an empty mapping",
            b"{}".to_vec(),
            identity.to_string(),
        ),
    ];

    for (case, payload, diagnosis) in rows {
        let (status, body) = publish_bytes(&router, &payload).await;
        assert_eq!(
            status, 403,
            "case {case:?}: a refused recipe must answer 403, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            ACCESS_DENIED_CODE,
            "case {case:?}: expected {ACCESS_DENIED_CODE}, got {body}"
        );
        assert_eq!(
            text(&body, "message"),
            format!("{diagnosis} {authorization}"),
            "case {case:?}: expected the diagnosis and then the authorization sentence, got \
             {body}"
        );
    }

    // Nothing was published by any of those requests: the catalog is still empty, so a
    // refused create wrote nothing.
    let (status, body) = get(&router, "/greengrass/v2/components").await;
    assert_eq!(
        (status, body.get("components")),
        (200, Some(&json!([]))),
        "case \"after every refusal\": a refused create must publish nothing, got {status} {body}"
    );
}

/// `CreateComponentVersion` for `recipe` carrying `tags`.
async fn publish_tagged(
    router: &MultiServiceRouter,
    document: &str,
    tags: &[(&str, &str)],
) -> (u16, Value) {
    let tags: serde_json::Map<String, Value> = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), json!(value)))
        .collect();
    post(
        router,
        "/greengrass/v2/createComponentVersion",
        &json!({
            "inlineRecipe": base64::encode(document.as_bytes()),
            "tags": tags,
        }),
    )
    .await
}

/// RULE: re-publishing a version that exists is the modelled 409, and its sentence is chosen
/// by whether the request carried tags — the plain collision reporting the state the stored
/// version's own reader reports, the tagged one reporting that it cannot be created again
/// with tags.
#[tokio::test]
async fn a_duplicate_version_conflicts_in_the_wording_aws_uses() {
    let router = emulator();
    let name = "com.example.Twice";
    let version = "1.0.0";
    let document = recipe(name, version);
    let (status, body) = publish(&router, &document).await;
    assert_eq!(
        status, 201,
        "publishing the first version must succeed, got {status} {body}"
    );

    // The state the stored version's own reader reports, which is what the plain collision
    // has to name: a published version's recipe validation is done, so it is deployable.
    let (status, described) = get(
        &router,
        &format!(
            "/greengrass/v2/components/{}/metadata",
            label(&version_arn(name, version))
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "the stored version must be readable, got {status} {described}"
    );
    let state = text(&described["status"], "componentState");
    assert_eq!(
        state, STATE_DEPLOYABLE,
        "case \"the stored version\": a published version settles deployable, got {described}"
    );

    let account = ACCOUNT;
    let plain = format!("Component [{name} : {version}] for account [{account}] already exists with state: [{state}]");
    let tagged = format!(
        "Component [{name} : {version}] for account [{account}] already exists and can't be \
         created again with tags."
    );

    // (case, the tags the colliding request carries, the sentence it is refused with)
    type ConflictRow<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let rows: &[ConflictRow] = &[
        ("no tags at all", &[], &plain),
        ("one tag", &[("env", "test")], &tagged),
        (
            "several tags",
            &[("env", "test"), ("owner", "platform")],
            &tagged,
        ),
    ];

    for (case, tags, expected) in rows {
        let (status, body) = if tags.is_empty() {
            publish(&router, &document).await
        } else {
            publish_tagged(&router, &document, tags).await
        };
        assert_eq!(
            status, 409,
            "case {case:?}: a duplicate version must answer 409, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            CONFLICT_CODE,
            "case {case:?}: expected {CONFLICT_CODE}, got {body}"
        );
        assert_eq!(
            &text(&body, "message"),
            *expected,
            "case {case:?}: expected the sentence AWS states for this collision, got {body}"
        );
        assert_eq!(
            text(&body, "resourceId"),
            version_arn(name, version),
            "case {case:?}: the conflict must name the stored version, got {body}"
        );
    }
}

/// The three operations the tagging route binds, as (the operation, its method).
const TAGGING_ROUTE: &[(&str, &str)] = &[
    ("TagResource", "POST"),
    ("ListTagsForResource", "GET"),
    ("UntagResource", "DELETE"),
];

/// One tagging request for `arn`, by method: the body and query each operation needs.
async fn tagging_call(
    router: &MultiServiceRouter,
    method: &str,
    arn: &str,
) -> (u16, Value) {
    let label = label(arn);
    match method {
        "POST" => post(router, &format!("/tags/{label}"), &json!({ "tags": { "env": "x" } })).await,
        "GET" => get(router, &format!("/tags/{label}")).await,
        _ => {
            crate::component_versions_and_resolution::call(
                router,
                "DELETE",
                &format!("/tags/{label}?tagKeys=env"),
                Vec::new(),
            )
            .await
        }
    }
}

/// RULE: the tagging route refuses an ARN of another vendor with that layer's own
/// `BadRequestException` and the sentence naming the vendor it expected, on every operation
/// it serves; an ARN of THIS vendor that names no stored resource keeps the route's
/// not-found class.
#[tokio::test]
async fn the_tagging_route_refuses_another_vendors_arn_as_a_bad_request() {
    let router = emulator();
    let vendor_sentence = "Invalid input resource arn: expected vendor greengrass";

    for (case, arn) in [
        ("an Amazon S3 bucket", "arn:aws:s3:::a-bucket".to_string()),
        (
            "an AWS IoT thing",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/Core"),
        ),
        (
            "an AWS Lambda function in another partition",
            "arn:aws-cn:lambda:cn-north-1:000000000000:function:f".to_string(),
        ),
        (
            "the legacy Greengrass vendor spelled as another service",
            format!("arn:aws:greengrassv2:{REGION}:{ACCOUNT}:components:com.example.X"),
        ),
    ] {
        for (operation, method) in TAGGING_ROUTE {
            let (status, body) = tagging_call(&router, method, &arn).await;
            assert_eq!(
                status, 400,
                "case {case:?} on {operation}: another vendor's ARN must answer 400, got \
                 {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                "BadRequestException",
                "case {case:?} on {operation}: expected the tagging layer's bad-request class, \
                 got {body}"
            );
            assert_eq!(
                text(&body, "message"),
                vendor_sentence,
                "case {case:?} on {operation}: expected the vendor sentence, got {body}"
            );
        }
    }

    // THIS vendor's ARN, inside the shape the route takes, naming nothing: the resource is
    // looked for and not found, which is a different refusal.
    let absent = version_arn("com.example.Absent", "1.0.0");
    for (operation, method) in TAGGING_ROUTE {
        let (status, body) = tagging_call(&router, method, &absent).await;
        assert_eq!(
            status, 404,
            "case \"a greengrass ARN naming nothing\" on {operation}: expected 404, got \
             {status} {body}"
        );
        assert_eq!(
            (error_code(&body), text(&body, "message")),
            (
                TAGGING_NOT_FOUND_CODE.to_string(),
                TAGGING_NOT_FOUND_MESSAGE.to_string()
            ),
            "case \"a greengrass ARN naming nothing\" on {operation}: expected the route's \
             not-found class, got {body}"
        );
    }
}
