// SPDX-License-Identifier: CC0-1.0

//! HOW Amazon Data Lifecycle Manager refuses a request whose `ResourceArn` names
//! no resource, on the wire.
//!
//! One rule, asserted over every operation that addresses a resource by ARN and
//! every reason an ARN can resolve to nothing, rather than the one request an
//! issue cited:
//!
//! > An operation that addresses its resource by an ARN answers an ARN that names
//! > no resource with HTTP 404 under the error type `NotFoundException`, stating
//! > `Resource was not found` in the lowercase `message` member that class
//! > declares and carrying none of the members a modelled error of this service
//! > brings. The prose is fixed: no part of the request appears in it. That class
//! > is NOT the `ResourceNotFoundException` the `PolicyId`-addressed operations
//! > answer, so a client branching on the error type tells the two resolutions
//! > apart.
//!
//! The rows differ along the operation (`ListTagsForResource`, `TagResource`,
//! `UntagResource` — the three bound to `/tags/{ResourceArn}`), the HTTP method,
//! the body, and WHY the ARN resolves to nothing: the id was never minted, the
//! policy was created and deleted, it lives in another region, the account or the
//! partition is another one, the id differs from a live one only in case, and the
//! calling region holds no policy at all. The last test is the contrast: the same
//! missing policy addressed by `PolicyId` keeps the modelled class.
//!
//! Every row goes through the router, so each case exercises routing, the
//! member-constraint walk, the handler, the persisted state and serialization —
//! the same path a client takes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dlm_dlm_20180112::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region most rows are signed for, and a second one for the rows about an
/// ARN whose policy lives elsewhere.
const REGION: &str = "us-east-1";
const OTHER_REGION: &str = "ap-southeast-2";

/// The refusal the ARN-addressed lookup answers, its status and its whole prose.
/// Spelled out here rather than read from the crate so a wrong constant cannot
/// make this test pass.
const NOT_FOUND_EXCEPTION: &str = "NotFoundException";
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The modelled not-found error the `PolicyId`-addressed operations answer, its
/// `@httpError` status and the `Code` that class reports
/// (`smithy_query(query="shape", shape="ResourceNotFoundException")`:
/// `Message`, `Code`, `ResourceType`, `ResourceIds`).
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
const RESOURCE_NOT_FOUND_STATUS: u16 = 404;
const RESOURCE_NOT_FOUND_CODE: &str = "ResourceNotFound";

/// The status a served `TagResource` answers (a body-less write), as against the
/// 200 of the other two tag operations.
const TAG_RESOURCE_STATUS: u16 = 204;

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the status, the `x-amzn-errortype` header and the
/// JSON body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    /// The error shape this answer names, as a restJson1 client resolves it: the
    /// `x-amzn-errortype` header, falling back to the body's `__type`, with any
    /// `namespace#` prefix stripped.
    fn error_shape(&self) -> &str {
        let named = if self.error_type.is_empty() {
            self.body["__type"].as_str().unwrap_or_default()
        } else {
            self.error_type.as_str()
        };
        named.rsplit('#').next().unwrap_or_default()
    }
}

/// A SigV4 credential scope naming this service in `region` — what makes a call a
/// call to that region's endpoint.
fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/dlm/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("dlm.{region}.amazonaws.com"))
        .header("authorization", signed_for(region));
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// `/tags/{ResourceArn}` for `arn`. The label is a single path segment, so the
/// ARN's `/` is percent-encoded.
fn tags_uri(arn: &str) -> String {
    format!("/tags/{}", arn.replace('/', "%2F"))
}

/// The role every policy here names: one this account may pass.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/AWSDataLifecycleManagerDefaultRole",
        engine::emulator_account_id()
    )
}

/// A well-formed `CreateLifecyclePolicy` request for a custom EBS snapshot
/// policy, as a client sends it.
fn create_request(description: &str) -> Value {
    json!({
        "ExecutionRoleArn": role_arn(),
        "Description": description,
        "State": "ENABLED",
        "PolicyDetails": {
            "PolicyType": "EBS_SNAPSHOT_MANAGEMENT",
            "ResourceTypes": ["VOLUME"],
            "TargetTags": [{"Key": "backup", "Value": "daily"}],
            "Schedules": [{
                "Name": "Daily",
                "CreateRule": {"Interval": 24, "IntervalUnit": "HOURS"},
                "RetainRule": {"Count": 7},
            }],
        },
    })
}

/// Create one policy in `region`, answering its id and the `PolicyArn` a read of
/// it reports.
async fn created_policy(
    router: &MultiServiceRouter,
    region: &str,
    description: &str,
) -> (String, String) {
    let created = call_in(
        router,
        region,
        "POST",
        "/policies",
        Some(create_request(description)),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the subject policy {description:?} must be created, got {} with {:?}",
        created.status, created.body
    );
    let id = created.body["PolicyId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "a create must report a PolicyId, got {:?}",
                created.body
            )
        })
        .to_string();
    let read = call_in(router, region, "GET", &format!("/policies/{id}"), None).await;
    let arn = read.body["Policy"]["PolicyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("a policy must report its ARN, got {:?}", read.body))
        .to_string();
    (id, arn)
}

/// The three operations `/tags/{ResourceArn}` binds, as `(label, method, uri,
/// body)` for one ARN.
fn tag_operations(arn: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        ("ListTagsForResource", "GET", tags_uri(arn), None),
        (
            "TagResource",
            "POST",
            tags_uri(arn),
            Some(json!({"Tags": {"written": "by the refused call"}})),
        ),
        (
            "UntagResource",
            "DELETE",
            format!("{}?tagKeys=team", tags_uri(arn)),
            None,
        ),
    ]
}

/// Assert `answer` is the ARN-addressed lookup's refusal: its status and error
/// type, its fixed prose in the lowercase member, and none of the members a
/// modelled error of this service would have brought with it.
fn assert_arn_not_found(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_shape(),
        NOT_FOUND_EXCEPTION,
        "case {case:?}: expected the error type {NOT_FOUND_EXCEPTION}, got {:?} with {:?}",
        answer.error_type,
        answer.body
    );
    assert_eq!(
        answer.body["message"],
        json!(NOT_FOUND_MESSAGE),
        "case {case:?}: expected the message {NOT_FOUND_MESSAGE:?}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["Message"],
        Value::Null,
        "case {case:?}: this class states its reason in the lowercase member only, got {:?}",
        answer.body
    );
    for member in ["Code", "ResourceType", "ResourceIds"] {
        assert_eq!(
            answer.body[member],
            Value::Null,
            "case {case:?}: this class declares no {member} member, got {:?}",
            answer.body
        );
    }
}

/// RULE — every operation that addresses a resource by ARN refuses an ARN that
/// names no resource with the same class, status and fixed prose, whatever made
/// the ARN resolve to nothing.
#[tokio::test]
async fn an_arn_that_names_no_resource_is_a_not_found_exception() {
    let router = emulator();
    let account = engine::emulator_account_id();

    // A live policy in the calling region, so each refusal is about resolution
    // rather than about an empty store, and one in another region.
    let (live_id, live_arn) = created_policy(&router, REGION, "the live subject").await;
    let (_, elsewhere_arn) = created_policy(&router, OTHER_REGION, "the other region").await;

    // One created in the calling region and then deleted: its ARN was once real.
    let (deleted_id, deleted_arn) = created_policy(&router, REGION, "the deleted subject").await;
    let gone = call_in(
        &router,
        REGION,
        "DELETE",
        &format!("/policies/{deleted_id}"),
        None,
    )
    .await;
    assert_eq!(
        gone.status, 200,
        "the subject policy must delete, got {} with {:?}",
        gone.status, gone.body
    );

    let rows: Vec<(&str, String)> = vec![
        (
            // The id counter mints ids from one upward, so the top of the
            // sixteen-hex-digit space is an id this store has not minted.
            "an id that was never minted",
            format!("arn:aws:dlm:{REGION}:{account}:policy/policy-ffffffffffffffff"),
        ),
        ("a policy that was created and deleted", deleted_arn),
        ("a policy that lives in another region", elsewhere_arn),
        (
            "another account's policy",
            format!("arn:aws:dlm:{REGION}:999999999999:policy/{live_id}"),
        ),
        (
            "another partition's policy",
            format!("arn:aws-cn:dlm:{REGION}:{account}:policy/{live_id}"),
        ),
        (
            "the live id in upper case",
            format!(
                "arn:aws:dlm:{REGION}:{account}:policy/{}",
                live_id.to_uppercase()
            ),
        ),
        (
            // An ARN whose id and region are a live policy's, called in a region
            // whose store this test leaves empty.
            "a region that holds no policy at all",
            format!("arn:aws:dlm:eu-north-1:{account}:policy/{live_id}"),
        ),
    ];

    // The prose is fixed, so every row of every operation must answer the same
    // body; the first answer is the reference the rest are compared against.
    let mut reference: Option<Value> = None;
    for (row, arn) in &rows {
        let region = if row == &"a region that holds no policy at all" {
            "eu-north-1"
        } else {
            REGION
        };
        for (operation, method, uri, body) in tag_operations(arn) {
            let answer = call_in(&router, region, method, &uri, body).await;
            let case = format!("{row}, {operation} with {arn}");
            assert_arn_not_found(&case, &answer);
            match &reference {
                None => reference = Some(answer.body.clone()),
                Some(first) => assert_eq!(
                    &answer.body, first,
                    "case {case:?}: the prose is fixed, so every refusal must answer the same \
                     body as {first:?}, got {:?}",
                    answer.body
                ),
            }
        }
    }

    // And no refused call reached the live policy: it is still served, with the
    // tags it already held.
    let listed = call_in(&router, REGION, "GET", &tags_uri(&live_arn), None).await;
    assert_eq!(
        listed.status, 200,
        "a refused ARN must leave the live policy readable, got {} with {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.body["Tags"],
        json!({}),
        "a refused TagResource must store no tag on the live policy, got {:?}",
        listed.body
    );
}

/// The other half of the rule — the class belongs to the LOOKUP, not to the
/// resource: the same policy that does not exist, addressed by its `PolicyId`,
/// keeps the modelled `ResourceNotFoundException` with every member that shape
/// declares, so the two codes stay distinguishable.
///
/// The rows differ along the operation and the method, and the id is a policy
/// that was created and then deleted, so one resource is addressed both ways.
#[tokio::test]
async fn the_same_missing_policy_keeps_the_modelled_class_when_addressed_by_id() {
    let router = emulator();
    let (id, arn) = created_policy(&router, REGION, "addressed both ways").await;
    let deleted = call_in(&router, REGION, "DELETE", &format!("/policies/{id}"), None).await;
    assert_eq!(
        deleted.status, 200,
        "the subject policy must delete, got {} with {:?}",
        deleted.status, deleted.body
    );

    let by_id: Vec<(&str, &str, String, Option<Value>)> = vec![
        ("GetLifecyclePolicy", "GET", format!("/policies/{id}"), None),
        (
            "DeleteLifecyclePolicy",
            "DELETE",
            format!("/policies/{id}"),
            None,
        ),
        (
            "UpdateLifecyclePolicy",
            "PATCH",
            format!("/policies/{id}"),
            Some(json!({"State": "DISABLED"})),
        ),
        (
            "GetLifecyclePolicies by policyIds",
            "GET",
            format!("/policies?policyIds={id}"),
            None,
        ),
    ];
    for (operation, method, uri, body) in by_id {
        let answer = call_in(&router, REGION, method, &uri, body).await;
        let case = format!("{operation} with the deleted id {id}");
        assert_eq!(
            answer.status, RESOURCE_NOT_FOUND_STATUS,
            "case {case:?}: expected HTTP {RESOURCE_NOT_FOUND_STATUS}, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_shape(),
            RESOURCE_NOT_FOUND_EXCEPTION,
            "case {case:?}: expected the modelled {RESOURCE_NOT_FOUND_EXCEPTION}, got {:?} \
             with {:?}",
            answer.error_type,
            answer.body
        );
        assert_eq!(
            answer.body["Code"],
            json!(RESOURCE_NOT_FOUND_CODE),
            "case {case:?}: the modelled class reports its Code, got {:?}",
            answer.body
        );
        assert!(
            answer.body["Message"]
                .as_str()
                .is_some_and(|message| message.contains(&id)),
            "case {case:?}: the modelled refusal names the id asked for, got {:?}",
            answer.body
        );
    }

    // The very same resource, addressed by the ARN it was issued, is the other
    // class — the one difference a client branches on.
    for (operation, method, uri, body) in tag_operations(&arn) {
        let answer = call_in(&router, REGION, method, &uri, body).await;
        let case = format!("{operation} with the deleted policy's ARN {arn}");
        assert_arn_not_found(&case, &answer);
        assert_ne!(
            answer.error_shape(),
            RESOURCE_NOT_FOUND_EXCEPTION,
            "case {case:?}: the ARN-addressed lookup must not answer the modelled class, \
             got {:?}",
            answer.body
        );
    }
}

/// The class is about an ARN that names NOTHING, so an ARN that names a policy is
/// served: each operation reaches the policy its ARN names, and only an ARN that
/// resolves to nothing is refused.
///
/// The rows differ along the operation and the tag set the policy holds when it
/// is called, so the served path is exercised before and after a write.
#[tokio::test]
async fn an_arn_that_names_a_resource_is_served() {
    let router = emulator();
    let (_, arn) = created_policy(&router, REGION, "the served subject").await;

    let listed = call_in(&router, REGION, "GET", &tags_uri(&arn), None).await;
    assert_eq!(
        listed.status, 200,
        "case {:?}: expected HTTP 200 from ListTagsForResource, got {} with {:?}",
        "a resolvable ARN before any tag is written", listed.status, listed.body
    );
    assert_eq!(
        listed.body["Tags"],
        json!({}),
        "case {:?}: a policy created with no tags reports none, got {:?}",
        "a resolvable ARN before any tag is written",
        listed.body
    );

    let tagged = call_in(
        &router,
        REGION,
        "POST",
        &tags_uri(&arn),
        Some(json!({"Tags": {"team": "backups"}})),
    )
    .await;
    assert_eq!(
        tagged.status, TAG_RESOURCE_STATUS,
        "case {:?}: expected HTTP {TAG_RESOURCE_STATUS} from TagResource, got {} with {:?}",
        "a resolvable ARN", tagged.status, tagged.body
    );
    let after_tag = call_in(&router, REGION, "GET", &tags_uri(&arn), None).await;
    assert_eq!(
        after_tag.body["Tags"],
        json!({"team": "backups"}),
        "case {:?}: the tag written through this ARN must be readable through it, got {:?}",
        "a resolvable ARN after a write",
        after_tag.body
    );

    let untagged = call_in(
        &router,
        REGION,
        "DELETE",
        &format!("{}?tagKeys=team", tags_uri(&arn)),
        None,
    )
    .await;
    assert_eq!(
        untagged.status, 200,
        "case {:?}: expected HTTP 200 from UntagResource, got {} with {:?}",
        "a resolvable ARN", untagged.status, untagged.body
    );
    assert_eq!(
        call_in(&router, REGION, "GET", &tags_uri(&arn), None)
            .await
            .body["Tags"],
        json!({}),
        "case {:?}: the removed key must be gone",
        "a resolvable ARN after a removal"
    );
}
