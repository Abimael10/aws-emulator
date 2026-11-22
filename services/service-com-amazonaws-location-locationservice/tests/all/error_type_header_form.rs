// SPDX-License-Identifier: CC0-1.0

//! The FORM this service names an error shape in, in the `x-amzn-errortype` response header
//! a `restJson1` client resolves that shape from, and WHICH shape each surface names — end
//! to end over HTTP.
//!
//! RULE: every failure this service answers names its error shape in `x-amzn-errortype` as
//! the shape's BARE name — no namespace before it, no locator after it — and the body names
//! that same shape in `__type`; a success names no shape at all. Which shape is the
//! failure's own: the modelled shape for this service's name-addressed resource operations,
//! and the shared tagging frontend's own `NotFoundException` for an ARN the ARN-addressed tag
//! plane resolves to nothing, which is in no operation's modelled error set.
//!
//! The rows differ along what the rule generalises over: the error CLASS (a resource that
//! does not exist, a name already held, a member value outside its modelled pattern, a tag
//! key the service reserves), the operation answering it, the SURFACE it is answered on
//! (the name-addressed resource operations, and the ARN-addressed tag plane over each of its
//! three operations), and the LAYER the answer comes from (a handler's modelled error, the
//! handler's unmodelled one, and the boundary refusal raised before any handler runs). The
//! success half covers both success statuses this service answers: the reads' 200 and the tag
//! mutations' 204.
//!
//! Every call goes through the shared harness (`crate::harness`), so routing, restJson1
//! (de)serialization, boundary validation and the handler all run as they do in production.

use serde_json::{json, Value};

use service_com_amazonaws_location_locationservice::service::error_type_locator::header_shape_named;

use crate::harness::{call, emulator, encode, Answer, ACCOUNT, REGION};

/// The status and the wire error CODE the shared tagging frontend in front of the
/// ARN-addressed tag operations answers for an ARN that resolves to no resource. Written
/// out here rather than read off the crate, so this suite pins the wire contract itself.
const TAG_PLANE_NOT_FOUND_STATUS: u16 = 404;
const TAG_PLANE_NOT_FOUND_CODE: &str = "NotFoundException";

/// The shape `answer` names in `x-amzn-errortype`, asserted to be a BARE shape name: no
/// namespace, no locator, nothing but the name a client resolves the exception class from.
///
/// Returned rather than compared here, so a row also asserts WHICH shape it expects.
fn bare_shape(case: &str, answer: &Answer) -> String {
    let header = answer.error_type.clone().unwrap_or_else(|| {
        panic!(
            "case {case:?}: a failure must name its shape in x-amzn-errortype, got {} {:?}",
            answer.status, answer.body
        )
    });
    assert!(
        !header.is_empty() && !header.contains([':', '/', '#']),
        "case {case:?}: expected a bare shape name in x-amzn-errortype, got {header:?}"
    );
    assert_eq!(
        header_shape_named(&header),
        header,
        "case {case:?}: the whole header value must be the shape a client resolves, got \
         {header:?}"
    );
    // The body names the same shape, so a client reading either reaches one error.
    if let Some(named) = answer.json().get("__type").and_then(Value::as_str) {
        assert_eq!(
            header_shape_named(named),
            header,
            "case {case:?}: the body's __type must name the same shape as the header, got \
             {named:?}"
        );
    }
    header
}

/// The whole assertion one failure row owes: its status, and `x-amzn-errortype` naming
/// `shape` in the bare form.
fn assert_names_shape(case: &str, answer: &Answer, status: u16, shape: &str) {
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} {:?}",
        answer.status, answer.body
    );
    let named = bare_shape(case, answer);
    assert_eq!(
        named, shape,
        "case {case:?}: expected the header to name {shape:?}, got {:?}",
        answer.error_type
    );
}

#[tokio::test]
async fn every_failure_names_its_shape_bare_and_the_body_agrees() {
    let router = emulator();

    // One tracker exists, so a miss below is a real miss rather than an empty store.
    let created = call(
        &router,
        "POST",
        "/tracking/v0/trackers",
        Some(json!({ "TrackerName": "held" })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the positive control must be created, got {} {:?}",
        created.status, created.body
    );
    let tracker_arn = created
        .json()
        .get("TrackerArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the create must report TrackerArn, got {:?}", created.body))
        .to_string();

    // ---- A resource that does not exist, named in the operation's own URI: the
    // handler's MODELLED 404.
    assert_names_shape(
        "a tracker read naming nothing stored",
        &call(&router, "GET", "/tracking/v0/trackers/absent", None).await,
        404,
        "ResourceNotFoundException",
    );

    // ---- The same class on the ARN-addressed tag plane, over each of its operations:
    // the tagging frontend's own shape, which is in no operation's modelled error set.
    let absent_arn = format!("arn:aws:geo:{REGION}:{ACCOUNT}:geofence-collection/absent");
    let encoded = encode(&absent_arn);
    assert_names_shape(
        "ListTagsForResource on an ARN naming nothing stored",
        &call(&router, "GET", &format!("/tags/{encoded}"), None).await,
        TAG_PLANE_NOT_FOUND_STATUS,
        TAG_PLANE_NOT_FOUND_CODE,
    );
    assert_names_shape(
        "TagResource on an ARN naming nothing stored",
        &call(
            &router,
            "POST",
            &format!("/tags/{encoded}"),
            Some(json!({ "Tags": { "env": "dev" } })),
        )
        .await,
        TAG_PLANE_NOT_FOUND_STATUS,
        TAG_PLANE_NOT_FOUND_CODE,
    );
    assert_names_shape(
        "UntagResource on an ARN naming nothing stored",
        &call(
            &router,
            "DELETE",
            &format!("/tags/{encoded}?tagKeys=env"),
            None,
        )
        .await,
        TAG_PLANE_NOT_FOUND_STATUS,
        TAG_PLANE_NOT_FOUND_CODE,
    );

    // ---- A name a resource of that kind already holds: the handler's modelled 409.
    assert_names_shape(
        "a create whose name is already held",
        &call(
            &router,
            "POST",
            "/tracking/v0/trackers",
            Some(json!({ "TrackerName": "held" })),
        )
        .await,
        409,
        "ConflictException",
    );

    // ---- A member value outside the pattern its model declares: the BOUNDARY refusal,
    // raised before any handler runs.
    assert_names_shape(
        "a create whose name is outside the modelled pattern",
        &call(
            &router,
            "POST",
            "/tracking/v0/trackers",
            Some(json!({ "TrackerName": "a name with spaces" })),
        )
        .await,
        400,
        "ValidationException",
    );

    // ---- A tag key the service reserves: a handler-raised refusal of the same class, on
    // the tag plane and about a resource that DOES exist.
    assert_names_shape(
        "a reserved tag key on a resource that exists",
        &call(
            &router,
            "POST",
            &format!("/tags/{}", encode(&tracker_arn)),
            Some(json!({ "Tags": { "aws:owner": "someone" } })),
        )
        .await,
        400,
        "ValidationException",
    );
}

#[tokio::test]
async fn a_success_names_no_error_shape_at_either_success_status() {
    let router = emulator();
    let created = call(
        &router,
        "POST",
        "/geofencing/v0/collections",
        Some(json!({ "CollectionName": "served" })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} {:?}",
        created.status, created.body
    );
    let arn = created
        .json()
        .get("CollectionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "the create must report CollectionArn, got {:?}",
                created.body
            )
        })
        .to_string();
    let encoded = encode(&arn);

    // Both statuses a success of this service answers: the reads' 200 and the tag
    // mutations' 204.
    let rows = [
        ("the create", created),
        (
            "TagResource",
            call(
                &router,
                "POST",
                &format!("/tags/{encoded}"),
                Some(json!({ "Tags": { "env": "dev" } })),
            )
            .await,
        ),
        (
            "ListTagsForResource",
            call(&router, "GET", &format!("/tags/{encoded}"), None).await,
        ),
        (
            "UntagResource",
            call(
                &router,
                "DELETE",
                &format!("/tags/{encoded}?tagKeys=env"),
                None,
            )
            .await,
        ),
    ];

    for (case, answer) in rows {
        assert!(
            (200..300).contains(&answer.status),
            "case {case:?}: the call must be served, got {} {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.error_type, None,
            "case {case:?}: a success names no error shape, got {:?}",
            answer.error_type
        );
    }
}
