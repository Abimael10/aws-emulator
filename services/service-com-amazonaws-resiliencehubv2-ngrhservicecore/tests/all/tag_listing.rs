// SPDX-License-Identifier: CC0-1.0

//! The tag surface of this service, end to end over HTTP: `TagResource` ->
//! `ListTagsForResource` -> `UntagResource`, over every resource of this service an ARN
//! can name.
//!
//! The rules under test:
//!
//! 1. `ListTagsForResource` reports the tag map STORED on the resource its `resourceArn`
//!    names — whichever kind of resource that is — so the map it answers is the one the
//!    resource's own reader reports, and a resource carrying no tags answers the EMPTY map
//!    rather than an absent one.
//! 2. A tag write is reflected by the next listing: `TagResource` adds exactly the keys it
//!    names (a key the resource already carries taking the value supplied) and
//!    `UntagResource` removes exactly the keys it names, leaving every other key as it was;
//!    `UntagResource` is `@idempotent`, so naming a key the resource does not carry leaves
//!    the resource unchanged rather than refusing.
//! 3. A well-formed ARN naming no resource this service holds is refused
//!    `ResourceNotFoundException` (HTTP 404) by the listing and by both writes alike,
//!    identifying the resource the caller addressed.
//! 4. The listing is `@readonly`: listing twice reports the same map, and leaves what every
//!    other reader of the resource reports untouched.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers, the `@httpLabel` ARN segment, the
//! `@httpQuery` tag-key list and the HTTP status codes are exercised exactly as a client
//! would exercise them.
//!
//! The suites are TABLES: each row is one input of the class the rule covers (which kind of
//! resource the ARN names, how many tags it carries — none, one, several, the modelled
//! ceiling — the form of the keys and values, which keys a write adds, overwrites or
//! removes, and the form of the ARN that names nothing), not one recorded request.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    MAX_TAGS_PER_RESOURCE, POLICY_RESOURCE, POLICY_RESOURCE_TYPE, SERVICE_RESOURCE,
    SERVICE_RESOURCE_TYPE, SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_POLICY: &str = "/v2/create-policy";
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_SYSTEM: &str = "/v2/create-system";
const GET_POLICY: &str = "/v2/get-policy";
const GET_SERVICE: &str = "/v2/get-service";
const GET_SYSTEM: &str = "/v2/get-system";

/// The one route the three tag operations share (`POST` adds, `GET` lists, `DELETE`
/// removes), with the resource's ARN as its single `@httpLabel` segment.
///
/// SOURCE: `smithy_query(query="operation",
/// operation="TagResource,ListTagsForResource,UntagResource")` — all three are bound to
/// `/v2/tags/{resourceArn}`.
const TAGS: &str = "/v2/tags";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The IAM role `CreateService`'s `@required` `permissionModel` names.
const INVOKER_ROLE: &str = "ResilienceHubRole";

/// The status a tag write answers with: `TagResourceResponse` and `UntagResourceResponse`
/// declare no member, and this service answers such a success with no content.
const NO_CONTENT: u16 = 204;

/// `TagValue`'s modelled bounds, which a tag value of any row satisfies.
///
/// SOURCE: `smithy_query(query="shape", shape="TagValue")` — `length {min: 0, max: 256}`,
/// so the EMPTY string is a tag value.
const TAG_VALUE_MIN_LENGTH: usize = 0;
const TAG_VALUE_MAX_LENGTH: usize = 256;

/// `TagKey`'s modelled ceiling.
///
/// SOURCE: `smithy_query(query="shape", shape="TagKey")` — `length {min: 1, max: 128}`,
/// pattern `^[^\x00-\x1f\x22]+$` (every character but the C0 controls and `"`).
const TAG_KEY_MAX_LENGTH: usize = 128;

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and the Region these suites sign for. Signature verification is off
/// for these tests, so the value only has to parse.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY00000000/20260101/{REGION}\
         /resiliencehub/aws4_request, SignedHeaders=host;x-amz-date, \
         Signature=0123456789abcdef"
    )
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An ARN inside a single `@httpLabel` path segment, or a tag key inside one `@httpQuery`
/// parameter value: everything outside the unreserved set travels percent-encoded, so an
/// ARN stays ONE path segment and a key carrying a space, a `:` or a `&` stays one value.
///
/// The encoding itself is `percent_encoding`'s, as the sibling tag suites do it, rather
/// than another hand-written copy of RFC 3986.
fn encoded(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, URI_COMPONENT).to_string()
}

/// The characters that travel literally in a path segment or a query value: RFC 3986's
/// unreserved set (the alphanumerics plus `-`, `.`, `_` and `~`).
const URI_COMPONENT: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

async fn list_tags(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(router, "GET", &format!("{TAGS}/{}", encoded(arn)), None).await
}

async fn tag_resource(router: &MultiServiceRouter, arn: &str, tags: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("{TAGS}/{}", encoded(arn)),
        Some(json!({"tags": tags})),
    )
    .await
}

async fn untag_resource(router: &MultiServiceRouter, arn: &str, keys: &[&str]) -> (u16, Value) {
    let query = keys
        .iter()
        .map(|key| format!("tagKeys={}", encoded(key)))
        .collect::<Vec<_>>()
        .join("&");
    call(
        router,
        "DELETE",
        &format!("{TAGS}/{}?{query}", encoded(arn)),
        None,
    )
    .await
}

/// One kind of resource this service holds and tags: how it is created, how it is read back,
/// and how a refusal names it.
#[derive(Debug, Clone, Copy)]
struct Kind {
    /// How a case names this kind.
    label: &'static str,
    create_route: &'static str,
    read_route: &'static str,
    /// The member a create and a read answer the resource under, which is also the prefix of
    /// the ARN member it carries.
    member: &'static str,
    /// The `@httpQuery` parameter and output member carrying this kind's ARN.
    arn_member: &'static str,
}

/// Every taggable resource of this store, from the ARNs
/// `ComAmazonawsResiliencehubv2NgrhservicecoreState::resource_tags` resolves: a service, a
/// system and a resilience policy.
const KINDS: [Kind; 3] = [
    Kind {
        label: "policy",
        create_route: CREATE_POLICY,
        read_route: GET_POLICY,
        member: "policy",
        arn_member: "policyArn",
    },
    Kind {
        label: "service",
        create_route: CREATE_SERVICE,
        read_route: GET_SERVICE,
        member: "service",
        arn_member: "serviceArn",
    },
    Kind {
        label: "system",
        create_route: CREATE_SYSTEM,
        read_route: GET_SYSTEM,
        member: "system",
        arn_member: "systemArn",
    },
];

/// The body that creates one resource of this kind, named `name`, carrying `tags` when any
/// are asked for.
fn create_body(kind: &Kind, name: &str, tags: Option<&Value>) -> Value {
    let mut body = match kind.label {
        "service" => json!({
            "name": name,
            "regions": [REGION],
            "permissionModel": {"invokerRoleName": INVOKER_ROLE},
        }),
        _ => json!({"name": name}),
    };
    if let Some(tags) = tags {
        body["tags"] = tags.clone();
    }
    body
}

/// Create one resource of `kind` and answer its ARN.
async fn create(
    router: &MultiServiceRouter,
    kind: &Kind,
    case: &str,
    name: &str,
    tags: Option<&Value>,
) -> String {
    let (status, body) = call(
        router,
        "POST",
        kind.create_route,
        Some(create_body(kind, name, tags)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a {}, got {status} {body:?}",
        kind.label
    );
    body[kind.member][kind.arn_member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no {} in {body:?}", kind.arn_member))
        .to_string()
}

/// The `tags` member the resource's OWN reader reports for `arn`.
async fn tags_from_reader(router: &MultiServiceRouter, kind: &Kind, case: &str, arn: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("{}?{}={}", kind.read_route, kind.arn_member, arn),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading the {} back, got {status} {body:?}",
        kind.label
    );
    body[kind.member]["tags"].clone()
}

/// The `tags` member of a successful listing.
async fn listed_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, body) = list_tags(router, arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} {body:?}"
    );
    let tags = body["tags"].clone();
    assert!(
        tags.is_object(),
        "case {case:?}: expected a tags map, got {body:?}"
    );
    tags
}

/// `tags` as a JSON object.
fn tag_object(tags: &BTreeMap<String, String>) -> Value {
    let mut object = Map::new();
    for (key, value) in tags {
        object.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(object)
}

/// `count` distinct tags, for a row that cares how MANY tags a resource carries rather than
/// what they say.
fn many_tags(count: usize) -> BTreeMap<String, String> {
    (0..count)
        .map(|index| (format!("key-{index}"), format!("value-{index}")))
        .collect()
}

/// Assert that the listing, and the resource's own reader, both report `expected` for `arn`,
/// and that listing again reports the same map — the listing reports stored state and
/// changes none.
async fn assert_tags(
    router: &MultiServiceRouter,
    kind: &Kind,
    case: &str,
    arn: &str,
    expected: &BTreeMap<String, String>,
) {
    let expected = tag_object(expected);
    let listed = listed_tags(router, case, arn).await;
    assert_eq!(
        listed, expected,
        "case {case:?}: expected the listing to report {expected:?}, got {listed:?}"
    );
    let projected = tags_from_reader(router, kind, case, arn).await;
    assert_eq!(
        projected, expected,
        "case {case:?}: expected the {}'s own reader to report {expected:?}, got {projected:?}",
        kind.label
    );
    let again = listed_tags(router, case, arn).await;
    assert_eq!(
        again, expected,
        "case {case:?}: expected a second listing to report {expected:?}, got {again:?}"
    );
}

/// One row of the tag-content table: the tags a resource is created with.
fn content_rows() -> Vec<(String, BTreeMap<String, String>)> {
    let mut rows: Vec<(String, BTreeMap<String, String>)> = vec![
        ("no tags at all".to_string(), BTreeMap::new()),
        (
            "a single tag".to_string(),
            [("env".to_string(), "prod".to_string())].into_iter().collect(),
        ),
        ("several tags".to_string(), many_tags(5)),
        (
            format!("as many tags as TagMap admits ({MAX_TAGS_PER_RESOURCE})"),
            many_tags(MAX_TAGS_PER_RESOURCE),
        ),
        (
            "a key and a value at their modelled ceilings".to_string(),
            [(
                "k".repeat(TAG_KEY_MAX_LENGTH),
                "v".repeat(TAG_VALUE_MAX_LENGTH),
            )]
            .into_iter()
            .collect(),
        ),
        (
            "a value at TagValue's modelled floor, the empty string".to_string(),
            [("blank".to_string(), "v".repeat(TAG_VALUE_MIN_LENGTH))]
                .into_iter()
                .collect(),
        ),
    ];
    // The characters `TagKey`/`TagValue`'s pattern admits beyond the alphanumerics: spaces,
    // punctuation the URI reserves, and non-ASCII text.
    rows.push((
        "keys and values carrying spaces, URI-reserved punctuation and non-ASCII text"
            .to_string(),
        [
            ("team name".to_string(), "platform & ops".to_string()),
            ("cost:centre".to_string(), "a/b+c=d".to_string()),
            ("propriétaire".to_string(), "équipe ☃".to_string()),
        ]
        .into_iter()
        .collect(),
    ));
    rows
}

#[tokio::test]
async fn every_taggable_resource_lists_the_tags_it_carries() {
    // Rows differ along WHICH resource an ARN names (policy, service, system) and what tags
    // it carries: none at all, one, several, the modelled ceiling, keys and values at their
    // modelled bounds, and keys and values carrying every class of character the model
    // admits.
    let router = emulator();
    for kind in KINDS {
        for (index, (row, tags)) in content_rows().into_iter().enumerate() {
            let case = format!("a {} with {row}", kind.label);
            let name = format!("Listed-{}-{index}", kind.label);
            let requested = tag_object(&tags);
            let created_with = if tags.is_empty() {
                None
            } else {
                Some(&requested)
            };
            let arn = create(&router, &kind, &case, &name, created_with).await;
            assert_tags(&router, &kind, &case, &arn, &tags).await;
        }
    }
}

#[tokio::test]
async fn an_added_tag_and_a_removed_tag_are_both_reflected_in_the_next_listing() {
    // One sequence per resource kind. The steps differ along what the write does: add a new
    // key, overwrite a key the resource already carries, remove one key of several, remove a
    // key the resource does not carry at all (which changes nothing), remove every remaining
    // key, and tag a resource whose map is empty again. One step's key carries a space and a
    // `:`, so the key list travels as an encoded query value.
    #[derive(Debug)]
    enum Step {
        Add(Value),
        Remove(Vec<&'static str>),
    }

    let spaced_key = "team name:ops";
    let router = emulator();
    for kind in KINDS {
        let case = format!("a {} tagged and untagged in turn", kind.label);
        let initial: BTreeMap<String, String> = [
            ("keep".to_string(), "first".to_string()),
            ("drop".to_string(), "second".to_string()),
        ]
        .into_iter()
        .collect();
        let arn = create(
            &router,
            &kind,
            &case,
            &format!("Retagged-{}", kind.label),
            Some(&tag_object(&initial)),
        )
        .await;
        assert_tags(&router, &kind, &case, &arn, &initial).await;

        let steps: Vec<(&str, Step, BTreeMap<String, String>)> = vec![
            (
                "a key the resource does not carry yet",
                Step::Add(json!({"added": "third"})),
                [("keep", "first"), ("drop", "second"), ("added", "third")]
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            (
                "a key the resource already carries, with a new value",
                Step::Add(json!({"keep": "changed"})),
                [("keep", "changed"), ("drop", "second"), ("added", "third")]
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            (
                "a key whose characters the URI reserves",
                Step::Add(json!({spaced_key: "on call"})),
                [
                    ("keep", "changed"),
                    ("drop", "second"),
                    ("added", "third"),
                    (spaced_key, "on call"),
                ]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ),
            (
                "removing one key of several",
                Step::Remove(vec!["drop"]),
                [("keep", "changed"), ("added", "third"), (spaced_key, "on call")]
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            (
                "removing the same key again, which the resource no longer carries",
                Step::Remove(vec!["drop"]),
                [("keep", "changed"), ("added", "third"), (spaced_key, "on call")]
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            (
                "removing a key the resource never carried",
                Step::Remove(vec!["never-set"]),
                [("keep", "changed"), ("added", "third"), (spaced_key, "on call")]
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            (
                "removing the URI-reserved key by its encoded name",
                Step::Remove(vec![spaced_key]),
                [("keep", "changed"), ("added", "third")]
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            (
                "removing every remaining key at once",
                Step::Remove(vec!["keep", "added"]),
                BTreeMap::new(),
            ),
            (
                "tagging a resource whose map is empty again",
                Step::Add(json!({"after": "empty"})),
                [("after".to_string(), "empty".to_string())]
                    .into_iter()
                    .collect(),
            ),
        ];

        for (step_case, step, expected) in steps {
            let step_case = format!("{case}: {step_case}");
            let (status, body) = match &step {
                Step::Add(tags) => tag_resource(&router, &arn, tags).await,
                Step::Remove(keys) => untag_resource(&router, &arn, keys).await,
            };
            assert_eq!(
                status, NO_CONTENT,
                "case {step_case:?}: expected HTTP {NO_CONTENT} from the write, got {status} \
                 {body:?}"
            );
            assert!(
                body.is_null(),
                "case {step_case:?}: a no-content answer carries no body, got {body:?}"
            );
            assert_tags(&router, &kind, &step_case, &arn, &expected).await;
        }
    }
}

#[tokio::test]
async fn an_arn_naming_no_stored_resource_is_refused_by_every_tag_operation() {
    // Rows differ along the form of the ARN: an id of each taggable kind this emulator never
    // minted, another Region's, another account's, and a resource segment naming a kind this
    // store does not hold at all. Each satisfies `Arn`'s modelled pattern, so each reaches a
    // handler. Expected `resourceType` / `resourceId`: the kind the ARN's resource segment
    // names, and the identifier the resource itself would report — a system's own `systemId`,
    // which is the segment without its `system/` qualifier, and the whole segment for a kind
    // that carries no id of its own.
    let router = emulator();
    // A live resource of each kind, so the refusals are about the ARN rather than an empty
    // store.
    for kind in KINDS {
        create(
            &router,
            &kind,
            "a live resource of each kind",
            &format!("Live-{}", kind.label),
            None,
        )
        .await;
    }

    let rows: Vec<(String, &str, String)> = vec![
        (
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{POLICY_RESOURCE}/missing"),
            POLICY_RESOURCE_TYPE,
            format!("{POLICY_RESOURCE}/missing"),
        ),
        (
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SERVICE_RESOURCE}/missing"),
            SERVICE_RESOURCE_TYPE,
            format!("{SERVICE_RESOURCE}/missing"),
        ),
        (
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/missing"),
            SYSTEM_RESOURCE_TYPE,
            "missing".to_string(),
        ),
        (
            format!("arn:aws:resiliencehub:eu-central-1:{ACCOUNT}:{SYSTEM_RESOURCE}/elsewhere"),
            SYSTEM_RESOURCE_TYPE,
            "elsewhere".to_string(),
        ),
        (
            format!("arn:aws:resiliencehub:{REGION}:210987654321:{POLICY_RESOURCE}/other-account"),
            POLICY_RESOURCE_TYPE,
            format!("{POLICY_RESOURCE}/other-account"),
        ),
        (
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:app/not-a-tagged-resource"),
            "app",
            "app/not-a-tagged-resource".to_string(),
        ),
    ];

    for (arn, resource_type, resource_id) in rows {
        // The listing and both writes resolve the same ARNs, so each refuses the same way.
        let answers = [
            ("ListTagsForResource", list_tags(&router, &arn).await),
            (
                "TagResource",
                tag_resource(&router, &arn, &json!({"env": "prod"})).await,
            ),
            ("UntagResource", untag_resource(&router, &arn, &["env"]).await),
        ];
        for (operation, (status, body)) in answers {
            let case = format!("{operation} for {arn:?}");
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404 for an ARN naming no stored resource, got \
                 {status} {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {body:?}"
            );
            assert_eq!(
                body["resourceType"].as_str(),
                Some(resource_type),
                "case {case:?}: expected resourceType {resource_type:?}, got {:?}",
                body["resourceType"]
            );
            assert_eq!(
                body["resourceId"].as_str(),
                Some(resource_id.as_str()),
                "case {case:?}: expected resourceId {resource_id:?}, got {:?}",
                body["resourceId"]
            );
            let expected = format!("{resource_type} not found: {resource_id}");
            assert_eq!(
                body["message"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the refusal {expected:?}, got {:?}",
                body["message"]
            );
        }
    }
}
