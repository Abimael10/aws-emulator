// SPDX-License-Identifier: CC0-1.0

//! How a refusal names a resource a Cloud Control DELETE has already removed.
//!
//! The rule every case here holds the service to: **the resource named inside a
//! not-found handler diagnostic is rendered as the HANDLER that owns the resource type
//! names it — the type's primary-identifier object, `{"/properties/<property>":
//! "<value>"}`, for a type whose handler resolves the identifier against the type's
//! registry schema, and the identifier the request carried for every other type — whichever
//! resource-level operation asked (`GetResource` and `UpdateResource` inline, the
//! asynchronous `DeleteResource` on the request record it mints) and however the resource
//! stopped existing.**
//!
//! `GetResource$Identifier` is where the model states the two forms a primary identifier
//! takes ("Primary identifiers can be specified as a string or JSON"); which of the two a
//! handler echoes back in its own diagnostic is that handler's business, so the rendering
//! is PER TYPE and a case about one type says nothing about another.
//!
//! The rows differ along what the rule generalises over: the resource type, where its
//! resources live (another service's store, this service's own), the shape of the
//! identifier (a plain name, a path-shaped name, a name carrying dots, a URL the owning
//! service assigns), and which operation refuses.

use serde_json::{json, Value};

use super::emulator::{code, message, progress, text, Emulator, STATUS_FAILED};
use super::refusal_wording::{assert_handler_not_found, handler_rendered_not_found, inert_patch};

/// The type whose resources live in the CloudWatch Logs service's own store, and whose
/// handler names the resource as the type's primary-identifier object.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The control for a type whose resources live in this service's own store and whose
/// handler names the resource by the identifier the request carried.
const STREAM: &str = "AWS::Kinesis::Stream";

/// A type whose identifier the OWNING service assigns and whose handler also names the
/// resource by the identifier the request carried, so the rendering above is not a global
/// one.
const QUEUE: &str = "AWS::SQS::Queue";

/// Every resource-level refusal of a resource that is not there, checked against the
/// diagnostic its owning handler states.
///
/// The two SYNCHRONOUS operations answer it inline, inside the handler envelope that
/// carries the handler error code and the token of the invocation that failed; the
/// asynchronous delete is ADMITTED and reports the same verdict on the request record it
/// minted, where the diagnostic and the handler error code each have a member of their
/// own.
async fn assert_every_refusal_names_the_resource(
    emulator: &Emulator,
    type_name: &str,
    identifier: &str,
    case: &str,
) {
    let detail = handler_rendered_not_found(type_name, identifier);

    let answers = [
        ("GetResource", emulator.get(type_name, identifier).await),
        (
            "UpdateResource",
            emulator
                .update(type_name, identifier, &inert_patch(), None)
                .await,
        ),
    ];
    for (operation, (status, body)) in answers {
        let case = format!("{case} ({operation})");
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            code(&body)
        );
        let request_token = assert_handler_not_found(&message(&body), type_name, &detail, &case);
        assert!(
            !request_token.is_empty(),
            "case {case:?}: the refusal must name the handler invocation that failed, got \
             {:?}",
            message(&body)
        );
    }

    let case = format!("{case} (DeleteResource)");
    let (status, body) = emulator.delete(type_name, identifier).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body:?}"
    );
    let failed = emulator
        .terminal(&text(&progress(&body), "RequestToken"))
        .await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "case {case:?}: the delete of an absent resource must fail, got {failed:?}"
    );
    assert_eq!(
        text(&failed, "ErrorCode"),
        "NotFound",
        "case {case:?}: expected the NotFound handler error code, got {failed:?}"
    );
    let stated = text(&failed, "StatusMessage");
    assert_eq!(
        stated, detail,
        "case {case:?}: expected the handler diagnostic {detail:?}, got {stated:?}"
    );
}

#[tokio::test]
async fn a_refusal_after_a_cloud_control_delete_names_the_resource_as_its_handler_does() {
    // The service that owns one of the types is mounted beside this one, so the rows about
    // its type act on resources that really live there.
    let emulator = Emulator::with_cloudwatch_logs();

    // The rows differ along the type, along where its resources live, and along the shape
    // of the identifier the caller chose.
    let rows: [(&str, &str, Value); 4] = [
        (
            "a plain name of a type whose resources live in the owning service's store",
            LOG_GROUP,
            json!({ "LogGroupName": "deleted-group", "RetentionInDays": 1 }),
        ),
        (
            "a path-shaped name of that type",
            LOG_GROUP,
            json!({ "LogGroupName": "/aws-emulator/deleted/group" }),
        ),
        (
            "a name carrying dots of that type",
            LOG_GROUP,
            json!({ "LogGroupName": "deleted.group.v2", "RetentionInDays": 7 }),
        ),
        (
            "a name of a type whose resources live in this service's own store",
            STREAM,
            json!({ "StreamName": "deleted-stream", "ShardCount": 1 }),
        ),
    ];

    for (case, type_name, desired) in rows {
        // Created through Cloud Control API, and really there: the refusals below are
        // about a resource a DELETE removed, not about one that never existed.
        let identifier = emulator.provision(type_name, &desired).await;
        let (status, body) = emulator.get(type_name, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created resource must read back, got {status} with body \
             {body:?}"
        );

        // Removed through Cloud Control API, which settles as a SUCCESS.
        let deleted = emulator.delete_and_settle(type_name, &identifier).await;
        assert_eq!(
            text(&deleted, "Identifier"),
            identifier,
            "case {case:?}: the delete must name the resource it removed, got {deleted:?}"
        );

        assert_every_refusal_names_the_resource(&emulator, type_name, &identifier, case).await;
    }
}

#[tokio::test]
async fn the_rendering_is_per_type_so_a_type_named_by_its_identifier_keeps_that_form() {
    // The service that owns the type is mounted beside this one, so the queue the rows
    // delete is a queue that service held.
    let emulator = Emulator::with_sqs();

    // The rows differ along the reference the request names the queue by — the URL the
    // owning service assigned, and the name it also answers to — because neither is
    // rendered as a schema map for this type.
    let rows = [
        ("the url the owning service assigned", true),
        ("the name the queue also answers to", false),
    ];
    for (row, (case, by_url)) in rows.into_iter().enumerate() {
        let name = format!("deleted-queue-{row}");
        let url = emulator
            .provision(QUEUE, &json!({ "QueueName": name }))
            .await;
        emulator.delete_and_settle(QUEUE, &url).await;

        let identifier = if by_url { url } else { name };
        assert_every_refusal_names_the_resource(&emulator, QUEUE, &identifier, case).await;
    }
}

#[tokio::test]
async fn a_rendered_identifier_is_the_json_object_the_schemas_pointer_names() {
    // The rendering a case above pins as text is also the JSON form the model documents an
    // identifier may be given in, so it reads back as one object of the schema's pointer
    // and the value the request carried.
    let emulator = Emulator::with_cloudwatch_logs();
    let name = "rendered-as-json";
    emulator
        .provision(LOG_GROUP, &json!({ "LogGroupName": name }))
        .await;
    emulator.delete_and_settle(LOG_GROUP, name).await;

    let (status, body) = emulator.get(LOG_GROUP, name).await;
    assert_eq!(
        status, 400,
        "the deleted group: expected HTTP 400, got {status} with body {body:?}"
    );
    let stated = message(&body);
    let opening = format!("Resource of type '{LOG_GROUP}' with identifier '");
    let rendered = stated
        .split_once(&opening)
        .and_then(|(_, rest)| rest.split_once("' was not found."))
        .map(|(identifier, _)| identifier.to_string())
        .unwrap_or_else(|| panic!("expected a not-found diagnostic, got {stated:?}"));
    let parsed: Value = serde_json::from_str(&rendered)
        .unwrap_or_else(|_| panic!("expected the identifier as JSON, got {rendered:?}"));
    assert_eq!(
        parsed,
        json!({ "/properties/LogGroupName": name }),
        "unexpected identifier object in {stated:?}"
    );
}
