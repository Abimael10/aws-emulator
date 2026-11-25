// SPDX-License-Identifier: CC0-1.0

//! Which error SHAPE each layer of this service names when it refuses a request, end to
//! end over the real router.
//!
//! restJson1 resolves the error shape a client branches on from the `x-amzn-errortype`
//! response header (and this service renders the same name in the body's `__type`). The
//! protocol admits three spellings of one shape — `Shape`, `namespace#Shape` and
//! `Shape:<namespace URI>` — and a client resolves the name out of any of them, so what
//! a refusal must get right is the SHAPE, not a spelling.
//!
//! The rule under test: the shape a refusal names is the shape of the LAYER that refused
//! the request.
//!
//! * A request naming a resource ARN of ANOTHER VENDOR names a resource this service's
//!   API cannot address at all. It is refused ahead of this service's own input
//!   validation, and the shape it names is the front door's generic bad-request shape,
//!   which no operation of this service declares.
//! * Everything this service refuses about a value of its OWN surface names the modelled
//!   shape it declares for that refusal: `ValidationException` (HTTP 400) for a value it
//!   will not accept. A resource that does not exist is the front door's again on this
//!   surface (HTTP 404), and the modelled `ResourceNotFoundException` on the operations
//!   addressed by a resource ID.
//! * A success names no shape at all: a client must not resolve an error out of a 200.
//!
//! Two further rules about the same refusals, both asserted below:
//!
//! * a not-found refusal of these three operations is the TAGGING FRONT DOOR's too — the
//!   same stack that answers the bad-request shape above — so it names that stack's generic
//!   not-found shape rather than the `ResourceNotFoundException` the model declares, while
//!   an operation addressed by a resource ID keeps the modelled shape its handler raised;
//! * the MESSAGE such a refusal carries states the GENERIC resource class, because the
//!   tagging surface addresses its subject generically.
//!
//! Every case runs on all three operations that take a `resourceArn` (`TagResource`,
//! `UntagResource` and `ListTagsForResource` all bind it to `/tags/{resourceArn}`),
//! because the rule belongs to the layer and not to one handler. The tagging BEHAVIOUR
//! those operations implement is the subject of `resource_tags.rs`; this suite asserts
//! only the identity and the wording each refusal carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// Shared with the lifecycle suite rather than written twice; `urlencode` percent-encodes
// one path segment, so an ARN carrying `:` and `/` stays the single `{resourceArn}` label
// of `/tags/{resourceArn}`.
use super::application_lifecycle::{
    call, create_application, emulator, get_application, minimal_create, started_application,
    urlencode, Answer,
};

/// The region the shared harness signs its requests for, and the region every minted ARN
/// names.
const REGION: &str = "eu-west-1";

/// The account id every emulator ARN carries (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// The front door's generic bad-request shape: the identity a resource ARN of another
/// vendor is refused with. No operation of this service declares it — the request never
/// reaches one.
const BAD_REQUEST: &str = "BadRequestException";

/// The client-input shape every operation of this service declares for a value of its own
/// surface it will not accept (`smithy_query(query="operation",…)` lists it with
/// `httpError` 400).
const VALIDATION: &str = "ValidationException";

/// The shape every one of these operations declares for a resource that does not exist
/// (`httpError` 404) — the one an operation addressed by a resource ID answers with.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The front door's generic not-found shape: the identity a resource ARN that names no
/// resource is refused with on the tagging surface. No operation of this service declares
/// it — the refusal belongs to the stack that fronts `/tags/{resourceArn}`, the same one
/// that answers [`BAD_REQUEST`] for an ARN it cannot address at all.
const FRONT_DOOR_NOT_FOUND: &str = "NotFoundException";

async fn list_tags(router: &MultiServiceRouter, resource_arn: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/tags/{}", urlencode(resource_arn)),
        None,
    )
    .await
}

async fn tag_resource(router: &MultiServiceRouter, resource_arn: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/tags/{}", urlencode(resource_arn)),
        Some(json!({ "tags": { "owner": "team-a" } })),
    )
    .await
}

async fn untag_resource(router: &MultiServiceRouter, resource_arn: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/tags/{}?tagKeys=owner", urlencode(resource_arn)),
        None,
    )
    .await
}

/// The same `resourceArn` sent to each of the three operations that bind it.
async fn answers(router: &MultiServiceRouter, resource_arn: &str) -> Vec<(&'static str, Answer)> {
    vec![
        ("ListTagsForResource", list_tags(router, resource_arn).await),
        ("TagResource", tag_resource(router, resource_arn).await),
        ("UntagResource", untag_resource(router, resource_arn).await),
    ]
}

/// The rule: every refusal carries the error-type identity of the layer that refused the
/// request, and a success carries none.
///
/// The rows span the refusal classes a request carrying a resource ARN can take, each on
/// all three operations:
///
/// * ARNs of other vendors — several vendors, two partitions, with and without a region,
///   over and under `ResourceArn`'s `length 60..1024` — all refused by the vendor check,
///   so the class is asserted over the ARN shapes it generalises over rather than one;
/// * values of this service's own surface the member does not admit: this vendor with a
///   resource type `ResourceArn`'s pattern does not name, a value that is not an ARN at
///   all, and one under the published length minimum;
/// * an ARN that satisfies the member and names no stored resource.
#[tokio::test]
async fn every_refusal_names_the_error_shape_of_the_layer_that_refused_it() {
    let router = emulator();
    let application_id = started_application(&router, "tok-identity").await;
    // The ARN the service minted and STORED, read back through the resource's own
    // report rather than spelled out here.
    let stored = get_application(&router, &application_id).await;
    let stored_application_arn = stored
        .at("/application/arn")
        .as_str()
        .unwrap_or_default()
        .to_string();

    // (case, resource ARN, expected status, expected resolved shape)
    let mut rows: Vec<(String, String, u16, &str)> = Vec::new();

    // Another vendor: the front door's refusal, whatever the ARN's other segments say.
    for (case, arn) in [
        (
            "an S3 bucket ARN (no region, no account)",
            "arn:aws:s3:::emr-serverless-artifacts-bucket-000000000000000".to_string(),
        ),
        (
            "an IAM role ARN",
            "arn:aws:iam::000000000000:role/EMRServerlessExecutionRoleForTests".to_string(),
        ),
        (
            // `emr` is a different vendor from `emr-serverless`, prefix or not.
            "an EMR cluster ARN",
            format!("arn:aws:emr:{REGION}:{ACCOUNT_ID}:cluster/j-000000000000000000001"),
        ),
        (
            "a Lambda ARN in another partition",
            "arn:aws-cn:lambda:cn-north-1:000000000000:function:emr-serverless-trigger".to_string(),
        ),
        (
            // Under `ResourceArn`'s length minimum: the vendor is still readable, and
            // the vendor check is ahead of the member's own constraints.
            "an S3 ARN under the member's minimum length",
            "arn:aws:s3:::b".to_string(),
        ),
    ] {
        rows.push((case.to_string(), arn, 400, BAD_REQUEST));
    }

    // This service's own surface: the member's constraints, refused with the modelled
    // client-input shape.
    for (case, arn) in [
        (
            "this vendor with a resource type the member's pattern does not name",
            format!("arn:aws:emr-serverless:{REGION}:{ACCOUNT_ID}:/clusters/0000000000000001"),
        ),
        (
            "a value that is not an ARN at all",
            "this-is-not-an-arn-but-it-is-long-enough-to-pass-the-length-check".to_string(),
        ),
        (
            // The region segment is `.+`, so a one-character region keeps the ARN well
            // formed while taking it under the published `length 60..1024` minimum.
            "this vendor under the member's minimum length",
            "arn:aws:emr-serverless:x:000000000000:/applications/a".to_string(),
        ),
    ] {
        rows.push((case.to_string(), arn, 400, VALIDATION));
    }

    // The member is satisfied and the resource does not exist: the front door's own 404.
    rows.push((
        "an ARN naming no stored resource".to_string(),
        format!(
            "arn:aws:emr-serverless:{REGION}:{ACCOUNT_ID}:/applications/\
             00fakeidnonexistent0000000000000"
        ),
        404,
        FRONT_DOOR_NOT_FOUND,
    ));

    for (case, resource_arn, status, shape) in rows {
        for (operation, answer) in answers(&router, &resource_arn).await {
            let case = format!("{operation}({case})");
            assert_eq!(
                answer.status, status,
                "case {case:?}: expected {status}, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.resolved_error_shape(),
                Some(shape),
                "case {case:?}: expected an identity resolving to {shape}, got {:?}",
                answer.error_type
            );
            // The body names the same shape, so a client reading either one branches the
            // same way.
            assert_eq!(
                answer.error_code(),
                shape,
                "case {case:?}: expected the body to name {shape}, got {:?}",
                answer.body
            );
        }
    }

    // A served request names no error shape at all.
    let listed = list_tags(&router, &stored_application_arn).await;
    assert_eq!(
        listed.status, 200,
        "case 'a stored application': expected 200, got {} with {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.resolved_error_shape(),
        None,
        "case 'a stored application': a success carries no error-type identity, got {:?}",
        listed.error_type
    );
    assert_eq!(
        listed.at("/tags"),
        Value::Object(serde_json::Map::new()),
        "case 'a stored application': expected an empty tag map, got {:?}",
        listed.body
    );
}

// --- the IDENTITY and the WORDING a tagging not-found refusal carries ---

/// AWS's wording for the not-found refusal of the TAGGING surface: the fixed phrase that
/// refusal carries when the `resourceArn` a tag operation was given names no resource. It
/// interpolates nothing — no ARN, no resource kind, no id — so the same sentence comes back
/// for every row below. Written out here so what the suite asserts is the wire text and
/// not whatever the crate computes.
const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The phrases the operations that address ONE resource class BY ITS OWN ID keep: there
/// the request named the class, so the refusal names it too. They are the control on the
/// rule above — a different sentence from the generic one, on the same error shape.
const APPLICATION_NOT_FOUND_MESSAGE: &str = "Application does not exist";
/// The same, for a job run addressed by its id inside an application that exists.
const JOB_RUN_NOT_FOUND_MESSAGE: &str = "Job Run with given ID doesn't exist";

/// An id of the modelled form (`^[0-9a-z]+$`, `length 1..64`) that names no resource of
/// this emulator.
const UNKNOWN_ID: &str = "00fakeidnonexistent0000000000000";

/// The `message` member of an answer's body, or the empty string when it carries none.
fn message(answer: &Answer) -> &str {
    answer.body["message"].as_str().unwrap_or_default()
}

/// An application this emulator STORES: its id and the ARN its own create reported.
///
/// The create answers both, so no ARN is spelled out here; and nothing waits for a
/// clock-owed transition, because a tag operation addresses the resource rather than the
/// status it is in.
async fn stored_application(router: &MultiServiceRouter, token: &str) -> (String, String) {
    let created = create_application(router, minimal_create(token)).await;
    assert_eq!(
        created.status, 200,
        "case {token:?}: CreateApplication expected 200, got {} with {:?}",
        created.status, created.body
    );
    let id = created
        .at("/applicationId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let arn = created.at("/arn").as_str().unwrap_or_default().to_string();
    assert!(
        !id.is_empty() && !arn.is_empty(),
        "case {token:?}: the create must report the id and the ARN it stored, got {:?}",
        created.body
    );
    (id, arn)
}

/// The rule: a tag operation refused because its `resourceArn` names no resource is the
/// TAGGING FRONT DOOR's refusal — it names that stack's generic not-found shape, in the
/// header a client resolves the shape from and in the body alike, and states the GENERIC
/// resource class, because the tagging surface addresses an application, a job run or a
/// session through one member. The operations that address ONE resource class by its own id
/// keep the modelled shape their handler raised and that class's own phrase.
///
/// The rows differ along what the rule generalises over: the KIND of resource the ARN
/// addresses (application, job run, session), WHICH part of it resolves to nothing (the
/// application itself, or a child of one that exists), and the account and the region the
/// ARN names — each on all three operations that bind `resourceArn`.
#[tokio::test]
async fn a_tag_not_found_is_the_front_doors_and_states_the_generic_resource_class() {
    let router = emulator();
    // A resource this emulator DOES hold, so each row below is refused for the ARN it
    // carries rather than for an empty store.
    let (application_id, application_arn) = stored_application(&router, "tok-tag-notfound").await;
    let unknown_application_arn = application_arn.replace(&application_id, UNKNOWN_ID);

    let rows = [
        (
            "an application that does not exist",
            unknown_application_arn.clone(),
        ),
        (
            "a job run of an application that does exist",
            format!("{application_arn}/jobruns/{UNKNOWN_ID}"),
        ),
        (
            "a session of an application that does exist",
            format!("{application_arn}/sessions/{UNKNOWN_ID}"),
        ),
        (
            "a job run of an application that does not exist",
            format!("{unknown_application_arn}/jobruns/{UNKNOWN_ID}"),
        ),
        (
            "another account's application",
            application_arn.replace(ACCOUNT_ID, "123456789012"),
        ),
        (
            "another region's application",
            application_arn.replace(REGION, "us-east-1"),
        ),
    ];

    for (case, resource_arn) in rows {
        for (operation, answer) in answers(&router, &resource_arn).await {
            let case = format!("{operation}({case})");
            assert_eq!(
                answer.status, 404,
                "case {case:?}: expected 404, got {} with {:?}",
                answer.status, answer.body
            );
            // Both places a restJson1 failure names a shape name the front door's, so a
            // client reading either one branches the same way.
            assert_eq!(
                answer.resolved_error_shape(),
                Some(FRONT_DOOR_NOT_FOUND),
                "case {case:?}: expected an identity resolving to {FRONT_DOOR_NOT_FOUND}, got \
                 {:?}",
                answer.error_type
            );
            assert_eq!(
                answer.error_code(),
                FRONT_DOOR_NOT_FOUND,
                "case {case:?}: expected the body to name {FRONT_DOOR_NOT_FOUND}, got {:?}",
                answer.body
            );
            assert_eq!(
                message(&answer),
                RESOURCE_NOT_FOUND_MESSAGE,
                "case {case:?}: expected the fixed phrase {RESOURCE_NOT_FOUND_MESSAGE:?}, got \
                 {:?}",
                message(&answer)
            );
            // Nothing of the request reaches the refusal: neither the ARN it carried nor
            // the id inside it occurs in the answer.
            let rendered = answer.body.to_string();
            assert!(
                !rendered.contains(&resource_arn) && !rendered.contains(UNKNOWN_ID),
                "case {case:?}: the request must not be echoed, got {rendered:?}"
            );
        }
    }

    // The control: an operation that addresses ONE resource class by its id keeps the
    // modelled shape its own handler raised and states that class, so neither the identity
    // nor the phrase above belongs to this service's every 404.
    for (case, uri, expected) in [
        (
            "GetApplication(an id that names none)",
            format!("/applications/{UNKNOWN_ID}"),
            APPLICATION_NOT_FOUND_MESSAGE,
        ),
        (
            "GetJobRun(an id that names none)",
            format!("/applications/{application_id}/jobruns/{UNKNOWN_ID}"),
            JOB_RUN_NOT_FOUND_MESSAGE,
        ),
    ] {
        let answer = call(&router, "GET", &uri, None).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.resolved_error_shape(),
            Some(NOT_FOUND),
            "case {case:?}: expected an identity resolving to {NOT_FOUND}, got {:?}",
            answer.error_type
        );
        assert_eq!(
            answer.error_code(),
            NOT_FOUND,
            "case {case:?}: expected the body to name {NOT_FOUND}, got {:?}",
            answer.body
        );
        assert_eq!(
            message(&answer),
            expected,
            "case {case:?}: expected the class phrase {expected:?}, got {:?}",
            message(&answer)
        );
    }

    // ... and the resource the store DOES hold is still served, so the refusals above are
    // the ARN's classification and not a dead path.
    let listed = list_tags(&router, &application_arn).await;
    assert_eq!(
        listed.status, 200,
        "case 'the stored application': expected 200, got {} with {:?}",
        listed.status, listed.body
    );
}

/// The rule: a not-found refusal names the shape of the LAYER that refused the request, and
/// that layer is decided by HOW the request addresses its resource — by a resource ARN (the
/// tagging front door's generic shape) or by a resource ID (the modelled shape the
/// operation declares). Everything else about the refusal — the status, the sentence, every
/// other member — is the one its own handler stated.
///
/// The rows differ along the operation and the resource class, over both layers, so the
/// restatement is pinned to the surface it belongs to rather than to one handler or to
/// every 404 this service answers.
#[tokio::test]
async fn a_not_found_names_the_shape_of_the_surface_that_addressed_the_resource() {
    let router = emulator();
    let (application_id, application_arn) = stored_application(&router, "tok-tag-identity").await;
    let unknown_application_arn = application_arn.replace(&application_id, UNKNOWN_ID);
    let tags_of = |arn: &str| format!("/tags/{}", urlencode(arn));

    // (case, method, uri, body, expected shape)
    let rows: Vec<(&str, &str, String, Option<Value>, &str)> = vec![
        (
            "ListTagsForResource on an ARN that names no resource",
            "GET",
            tags_of(&unknown_application_arn),
            None,
            FRONT_DOOR_NOT_FOUND,
        ),
        (
            "TagResource on an ARN that names no resource",
            "POST",
            tags_of(&unknown_application_arn),
            Some(json!({ "tags": { "owner": "team-a" } })),
            FRONT_DOOR_NOT_FOUND,
        ),
        (
            "UntagResource on an ARN that names no resource",
            "DELETE",
            format!("{}?tagKeys=owner", tags_of(&unknown_application_arn)),
            None,
            FRONT_DOOR_NOT_FOUND,
        ),
        (
            "ListTagsForResource on a job run ARN that names no resource",
            "GET",
            tags_of(&format!("{application_arn}/jobruns/{UNKNOWN_ID}")),
            None,
            FRONT_DOOR_NOT_FOUND,
        ),
        (
            "GetApplication on an id that names none",
            "GET",
            format!("/applications/{UNKNOWN_ID}"),
            None,
            NOT_FOUND,
        ),
        (
            "StartApplication on an id that names none",
            "POST",
            format!("/applications/{UNKNOWN_ID}/start"),
            None,
            NOT_FOUND,
        ),
        (
            "DeleteApplication on an id that names none",
            "DELETE",
            format!("/applications/{UNKNOWN_ID}"),
            None,
            NOT_FOUND,
        ),
        (
            "GetJobRun on a job run id that names none",
            "GET",
            format!("/applications/{application_id}/jobruns/{UNKNOWN_ID}"),
            None,
            NOT_FOUND,
        ),
        (
            "CancelJobRun on a job run id that names none",
            "DELETE",
            format!("/applications/{application_id}/jobruns/{UNKNOWN_ID}"),
            None,
            NOT_FOUND,
        ),
    ];

    for (case, method, uri, body, shape) in rows {
        let answer = call(&router, method, &uri, body).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.resolved_error_shape(),
            Some(shape),
            "case {case:?}: expected an identity resolving to {shape}, got {:?}",
            answer.error_type
        );
        assert_eq!(
            answer.error_code(),
            shape,
            "case {case:?}: expected the body to name {shape}, got {:?}",
            answer.body
        );
        // Whichever layer the shape belongs to, the refusal still states a sentence: the
        // restatement moves the NAME and nothing else.
        assert!(
            !message(&answer).is_empty(),
            "case {case:?}: expected the refusal to state a message, got {:?}",
            answer.body
        );
    }

    // A success is answered as the renderer wrote it: it names no shape, in either place.
    let listed = list_tags(&router, &application_arn).await;
    assert_eq!(
        listed.status, 200,
        "case 'the stored application': expected 200, got {} with {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.error_type, None,
        "case 'the stored application': a success names no error shape, got {:?}",
        listed.error_type
    );
    assert!(
        listed.at("/__type").is_null(),
        "case 'the stored application': a success names no shape in its body either, got {:?}",
        listed.body
    );
}
