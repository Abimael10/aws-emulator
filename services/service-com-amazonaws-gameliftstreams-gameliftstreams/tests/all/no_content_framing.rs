// SPDX-License-Identifier: CC0-1.0

//! How this service FRAMES a success that carries no content.
//!
//! The rule under test is one sentence: an operation whose output structure declares no
//! member has nothing for its caller to parse, so its success answers `204 No Content`
//! with NO body and a `Content-Length` header stating the length it has.
//!
//! Why each half matters: RFC 9110 section 15.3.5 gives a 204 no room for a body at all,
//! so the empty JSON object a members-less output structure would otherwise render is
//! content on a response that declares it has none; and a client that frames a response
//! by its length, receiving neither a length nor chunked framing, must read until the
//! connection closes — on a keep-alive connection that stalls or desynchronises it, and
//! strict clients and proxies reject the message outright.
//!
//! The table below spans the class — every bodiless write this service serves, over both
//! taggable resource types, reached by all three of the methods they are bound to — and
//! not one recorded call: the framing is one property of the serializer every response of
//! this service passes through
//! (`smithy_query(query="operation", operation="TagResource,UntagResource")` reports each
//! such output shape as a structure with no members).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::application_read_projection::{
    application_in_ready_status, emulator_with_staged_content, poll_until, staged_source_uri,
    stream_group_fully_active,
};
use super::stream_workflow::{as_label, call, call_with_headers, text, REGION};

/// The status a success with no content is answered at.
const NO_CONTENT: u16 = 204;

/// The length such a response states for the body it does not carry.
const NO_CONTENT_LENGTH: &str = "0";

/// One bodiless write: a label, the request that makes it, and nothing else.
struct Write {
    case: String,
    method: &'static str,
    path: String,
    body: Option<Value>,
}

/// Create an application and wait until its content is `READY`, answering `(id, arn)`.
async fn ready_application(router: &MultiServiceRouter, description: &str) -> (String, String) {
    let (status, created) = call(
        router,
        "POST",
        "/applications",
        Some(json!({
            "Description": description,
            "RuntimeEnvironment": {"Type": "PROTON", "Version": "20250516"},
            "ExecutablePath": "game.exe",
            "ApplicationSourceUri": staged_source_uri(),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {description:?}: expected CreateApplication 201, got {status} {created}"
    );
    let id = text(&created, "/Id", description);
    application_in_ready_status(router, &id, description).await;
    (id, text(&created, "/Arn", description))
}

/// Create a stream group over `locations` whose default application is `application`,
/// and wait until it and every location it holds are `ACTIVE`.
async fn active_group(
    router: &MultiServiceRouter,
    description: &str,
    application: &str,
    locations: &[&str],
) -> (String, String) {
    let configurations: Vec<Value> = locations
        .iter()
        .map(|location| json!({"LocationName": location, "AlwaysOnCapacity": 1}))
        .collect();
    let (status, created) = call(
        router,
        "POST",
        "/streamgroups",
        Some(json!({
            "Description": description,
            "StreamClass": "gen5n_high",
            "DefaultApplicationIdentifier": application,
            "LocationConfigurations": configurations,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {description:?}: expected CreateStreamGroup 201, got {status} {created}"
    );
    let id = text(&created, "/Id", description);
    stream_group_fully_active(router, &id, description).await;
    (id, text(&created, "/Arn", description))
}

/// Start one stream session in `group` and wait until it has left `ACTIVATING`, which is
/// the one status `TerminateStreamSession` refuses ("You can terminate a stream session
/// in any status except ACTIVATING").
async fn terminable_session(
    router: &MultiServiceRouter,
    group: &str,
    application: &str,
    case: &str,
) -> String {
    let (status, session) = call(
        router,
        "POST",
        &format!("/streamgroups/{group}/streamsessions"),
        Some(json!({
            "Protocol": "WebRTC",
            "SignalRequest": "offer",
            "ApplicationIdentifier": application,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected StartStreamSession 201, got {status} {session}"
    );
    let id = text(&session, "/Arn", case)
        .rsplit('/')
        .next()
        .expect("a stream session ARN ends in the session id")
        .to_string();
    poll_until(
        router,
        &format!("/streamgroups/{group}/streamsessions/{id}"),
        case,
        |body| body["Status"] != json!("ACTIVATING"),
    )
    .await;
    id
}

/// Create one stream URL in `group` for `application`.
async fn stream_url(
    router: &MultiServiceRouter,
    group: &str,
    application: &str,
    case: &str,
) -> String {
    let (status, created) = call(
        router,
        "POST",
        &format!("/streamgroups/{group}/streamurls"),
        Some(json!({
            "ApplicationIdentifier": application,
            "Protocol": "WebRTC",
            "Locations": [REGION],
            "UrlExpiresAfterMinutes": 60,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected CreateStreamUrl 201, got {status} {created}"
    );
    text(&created, "/StreamUrlId", case)
}

/// Every bodiless write this service serves, each on a resource of its own so no row
/// depends on another having run.
async fn bodiless_writes(router: &MultiServiceRouter) -> Vec<Write> {
    let mut writes: Vec<Write> = Vec::new();

    // The two tagging writes, on each resource type this API tags: "You can assign tags
    // to the following Amazon GameLift Streams resource types: Application StreamGroup"
    // (`TagResource`'s documentation).
    for kind in ["application", "streamgroup"] {
        let (application_id, application_arn) =
            ready_application(router, &format!("framing-tags-{kind}")).await;
        let arn = if kind == "application" {
            application_arn
        } else {
            active_group(
                router,
                &format!("framing-tags-group-{kind}"),
                &application_id,
                &[REGION],
            )
            .await
            .1
        };
        writes.push(Write {
            case: format!("TagResource on a {kind}"),
            method: "POST",
            path: format!("/tags/{}", as_label(&arn)),
            body: Some(json!({"Tags": {"env": "dev"}})),
        });
        writes.push(Write {
            case: format!("UntagResource on a {kind}"),
            method: "DELETE",
            path: format!("/tags/{}?tagKeys=env", as_label(&arn)),
            body: None,
        });
    }

    // A location released from a group that keeps serving its other one.
    let (application_id, _) = ready_application(router, "framing-locations").await;
    let (group_id, _) = active_group(
        router,
        "framing-locations-group",
        &application_id,
        &[REGION, "us-east-1"],
    )
    .await;
    writes.push(Write {
        case: "RemoveStreamGroupLocations".to_string(),
        method: "DELETE",
        path: format!("/streamgroups/{group_id}/locations?locations=us-east-1"),
        body: None,
    });

    // A stream session terminated, and a stream URL revoked.
    let (application_id, application_arn) = ready_application(router, "framing-sessions").await;
    let (group_id, _) = active_group(
        router,
        "framing-sessions-group",
        &application_id,
        &[REGION],
    )
    .await;
    let session_id =
        terminable_session(router, &group_id, &application_arn, "framing-sessions").await;
    writes.push(Write {
        case: "TerminateStreamSession".to_string(),
        method: "DELETE",
        path: format!("/streamgroups/{group_id}/streamsessions/{session_id}"),
        body: None,
    });
    let url_id = stream_url(router, &group_id, &application_arn, "framing-urls").await;
    writes.push(Write {
        case: "RevokeStreamUrl".to_string(),
        method: "POST",
        path: format!("/streamgroups/{group_id}/streamurls/{url_id}/revoke"),
        body: None,
    });

    // The two deletes, each of a resource nothing else in this table names.
    let (deleted_application, _) = ready_application(router, "framing-delete-application").await;
    writes.push(Write {
        case: "DeleteApplication".to_string(),
        method: "DELETE",
        path: format!("/applications/{deleted_application}"),
        body: None,
    });
    let (application_id, _) = ready_application(router, "framing-delete-group-app").await;
    let (deleted_group, _) = active_group(
        router,
        "framing-delete-group",
        &application_id,
        &[REGION],
    )
    .await;
    writes.push(Write {
        case: "DeleteStreamGroup".to_string(),
        method: "DELETE",
        path: format!("/streamgroups/{deleted_group}"),
        body: None,
    });

    writes
}

/// Every bodiless write answers 204 with no body and states the length it has.
#[tokio::test]
async fn a_success_that_carries_no_content_states_the_length_it_has() {
    let router = emulator_with_staged_content().await;
    for write in bodiless_writes(&router).await {
        let case = &write.case;
        let (status, headers, body) =
            call_with_headers(&router, write.method, &write.path, write.body.clone()).await;
        assert_eq!(
            status, NO_CONTENT,
            "case {case:?}: expected {NO_CONTENT}, got {status} {body}"
        );
        assert_eq!(
            body,
            Value::Null,
            "case {case:?}: a no-content answer carries no body, got {body}"
        );
        assert_eq!(
            headers.get("content-length").map(String::as_str),
            Some(NO_CONTENT_LENGTH),
            "case {case:?}: expected content-length {NO_CONTENT_LENGTH}, got {:?}",
            headers.get("content-length")
        );
    }
}

/// A success that DOES carry content is untouched by that framing: it keeps its body,
/// and nothing here frames it as empty.
#[tokio::test]
async fn a_success_that_carries_content_keeps_its_body() {
    let router = emulator_with_staged_content().await;
    let (application_id, application_arn) = ready_application(&router, "framing-content").await;
    let (group_id, _) = active_group(
        &router,
        "framing-content-group",
        &application_id,
        &[REGION],
    )
    .await;

    // One read, one create and one write whose output has members: each must answer with
    // the body its own members render.
    let rows: Vec<(&str, &str, String, Option<Value>, u16)> = vec![
        (
            "GetApplication",
            "GET",
            format!("/applications/{application_id}"),
            None,
            200,
        ),
        (
            "ListTagsForResource",
            "GET",
            format!("/tags/{}", as_label(&application_arn)),
            None,
            200,
        ),
        (
            "AddStreamGroupLocations",
            "POST",
            format!("/streamgroups/{group_id}/locations"),
            Some(json!({
                "LocationConfigurations": [{"LocationName": "us-east-1", "AlwaysOnCapacity": 1}],
            })),
            200,
        ),
    ];
    for (case, method, path, body, expected) in rows {
        let (status, _headers, answered) =
            call_with_headers(&router, method, &path, body).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected}, got {status} {answered}"
        );
        assert!(
            answered.is_object(),
            "case {case:?}: an answer that carries content keeps its body, got {answered}"
        );
    }
}
