// SPDX-License-Identifier: CC0-1.0

//! How a refusal names a resource the OWNING service's own API removed.
//!
//! The rule every case here holds the service to: **the resource named inside a not-found
//! handler diagnostic is rendered as the HANDLER that owns the resource type names it —
//! the type's primary-identifier object, `{"/properties/<property>":"<value>"}`, for a
//! type whose handler resolves the identifier against the type's registry schema, and the
//! identifier the request carried for every other type — whichever resource-level
//! operation asked and however the resource stopped existing, the owning service's own API
//! included.**
//!
//! `deleted_resource_refusal` holds that rule for a resource a Cloud Control DELETE
//! removed; this suite holds it where the resource was removed over the OWNING service's
//! own API, which is the other way a resource Cloud Control API read stops existing — "you
//! can use this action to return information about an existing resource in your account
//! and Amazon Web Services Region, whether those resources were provisioned using Cloud
//! Control API" (`GetResource`), so the resource a refusal is about is the owning
//! service's either way.
//!
//! `GetResource$Identifier` is where the model states the two forms a primary identifier
//! takes ("Primary identifiers can be specified as a string or JSON"); which of the two a
//! handler echoes back in its own diagnostic is that handler's business, so the rendering
//! is PER TYPE and a case about one type says nothing about another — hence the queue rows
//! beside the log group ones.
//!
//! The rows differ along what the rule generalises over: the resource type, which API the
//! resource came into being through, the shape of the identifier (a plain name, a
//! path-shaped name, a name carrying dots, a URL the owning service assigns), the
//! properties the resource carries, and which operation refuses.

use serde_json::{json, Value};

use super::deleted_resource_refusal::assert_every_refusal_names_the_resource;
use super::emulator::{message, text, Emulator};
use super::native_resource_resolution::native_groups;
use super::refusal_wording::rendered_identifier;
use super::sqs_queue_read_through::create_native;

/// The type whose resources live in the CloudWatch Logs service's own store, and whose
/// handler names the resource as the type's primary-identifier object.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The one pointer that type's registry schema holds in `primaryIdentifier`.
///
/// Catalog source: the `primaryIdentifier` list of the `AWS::Logs::LogGroup` resource
/// provider schema, whose pointers name properties of that schema.
const LOG_GROUP_POINTER: &str = "/properties/LogGroupName";

/// The control: a type whose identifier the OWNING service assigns and whose handler names
/// the resource by the identifier the request carried, so the rendering above is not a
/// global one.
const QUEUE: &str = "AWS::SQS::Queue";

#[tokio::test]
async fn a_refusal_after_an_owning_service_delete_names_the_resource_as_its_handler_does() {
    // The service that owns the type is mounted beside this one, so every row acts on a
    // group that really lives there.
    let emulator = Emulator::with_cloudwatch_logs();

    // The rows differ along the shape of the name, along the properties the group carries,
    // and along which API created the group the owning service then removed.
    let rows: [(&str, &str, Option<i64>, bool); 5] = [
        (
            "a plain name, created over the owning service's own API",
            "owning-plain",
            None,
            false,
        ),
        (
            "a group under a retention policy",
            "owning-retained",
            Some(1),
            false,
        ),
        (
            "a path-shaped name",
            "/aws-emulator/owning/nested",
            Some(3653),
            false,
        ),
        (
            "a name carrying dots",
            "owning.group.v2",
            Some(7),
            false,
        ),
        (
            "a group Cloud Control API created and the owning service removed",
            "owning-crossed",
            None,
            true,
        ),
    ];

    for (case, name, retention, through_cloud_control) in rows {
        if through_cloud_control {
            emulator
                .provision(LOG_GROUP, &json!({ "LogGroupName": name }))
                .await;
        } else {
            emulator
                .logs_ok("CreateLogGroup", json!({ "logGroupName": name }))
                .await;
        }
        if let Some(days) = retention {
            emulator
                .logs_ok(
                    "PutRetentionPolicy",
                    json!({ "logGroupName": name, "retentionInDays": days }),
                )
                .await;
        }

        // The group is really there and Cloud Control API resolves it: the refusals below
        // are about a resource the owning service's API REMOVED, not about one that never
        // existed.
        let (status, body) = emulator.get(LOG_GROUP, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created group must read back, got {status} with body {body:?}"
        );
        assert_eq!(
            text(&body["ResourceDescription"], "Identifier"),
            name,
            "case {case:?}: expected the group's own identifier, got {body:?}"
        );

        // Removed over the OWNING service's own API, and gone from its own reader.
        emulator
            .logs_ok("DeleteLogGroup", json!({ "logGroupName": name }))
            .await;
        let held = native_groups(&emulator, Some(name)).await;
        assert!(
            held.is_empty(),
            "case {case:?}: the owning service must hold the group no longer, it holds \
             {held:?}"
        );

        assert_every_refusal_names_the_resource(&emulator, LOG_GROUP, name, case).await;

        // And the identifier that diagnostic names is the type's primary-identifier object
        // itself — the JSON form the model documents an identifier may be given in — not
        // text that merely reads like one.
        let (_, refused) = emulator.get(LOG_GROUP, name).await;
        let rendered = rendered_identifier(&message(&refused), LOG_GROUP, case);
        let parsed: Value = serde_json::from_str(&rendered).unwrap_or_else(|_| {
            panic!("case {case:?}: expected the identifier as JSON, got {rendered:?}")
        });
        assert_eq!(
            parsed,
            json!({ LOG_GROUP_POINTER: name }),
            "case {case:?}: unexpected identifier object in {rendered:?}"
        );
    }
}

#[tokio::test]
async fn the_rendering_after_an_owning_service_delete_stays_per_type() {
    // The service that owns the control type is mounted beside this one, so the queue the
    // rows remove is a queue that service held.
    let emulator = Emulator::with_sqs();

    // The rows differ along the reference the request names the queue by — the url the
    // owning service assigned, and the name it also answers to — because this type's
    // handler renders neither as a schema map.
    let rows = [
        ("the url the owning service assigned", true),
        ("the name the queue also answers to", false),
    ];
    for (row, (case, by_url)) in rows.into_iter().enumerate() {
        let name = format!("owning-deleted-queue-{row}");
        let url = create_native(&emulator, &name, &json!({})).await;
        let (status, body) = emulator.get(QUEUE, &url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created queue must read back, got {status} with body {body:?}"
        );

        // Removed over the OWNING service's own API, and gone from its own reader.
        emulator
            .sqs_ok("DeleteQueue", json!({ "QueueUrl": url }))
            .await;
        let listed = emulator.sqs_ok("ListQueues", json!({})).await;
        assert!(
            !listed["QueueUrls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .any(|held| held.as_str() == Some(url.as_str())),
            "case {case:?}: the owning service must hold the queue no longer, it holds \
             {listed:?}"
        );

        let identifier = if by_url { url } else { name };
        assert_every_refusal_names_the_resource(&emulator, QUEUE, &identifier, case).await;

        // The rendering is the identifier itself, so it is NOT the schema map the log
        // group rows pin: a client reading this diagnostic sees what it was given back.
        let (_, refused) = emulator.get(QUEUE, &identifier).await;
        let rendered = rendered_identifier(&message(&refused), QUEUE, case);
        assert_eq!(
            rendered, identifier,
            "case {case:?}: expected the identifier {identifier:?}, got {rendered:?}"
        );
    }
}
