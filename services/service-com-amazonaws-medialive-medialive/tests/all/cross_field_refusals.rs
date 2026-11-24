// SPDX-License-Identifier: CC0-1.0

//! The CROSS-FIELD and CROSS-RESOURCE refusals of the channel and input writes,
//! end to end over the restJson1 wire.
//!
//! Every rule here is one no single-member model constraint can express, because
//! it relates one member of the request to another (or to a resource this
//! account holds), so each is checked by the handler that writes the members:
//!
//! * `BatchDelete` acts on resources of ONE kind per call, so exactly one of its
//!   four id lists is non-empty and a request filling two of them (or none) is
//!   refused whole, with nothing deleted;
//! * a channel destination's per-encoder `Settings` carries one entry per
//!   pipeline of the channel's class (`OutputDestination$Settings`: "Destination
//!   settings for a standard output; one destination for each redundant
//!   encoder", and `ChannelClass`: "A standard channel has two encoding
//!   pipelines and a single pipeline channel only has one");
//! * an `InputAttachments` entry may not name an input this account holds whose
//!   delete has been accepted: that input exists and is identified, it is simply
//!   in a state no channel can ingest through, so the request itself is refused
//!   and the refusal names the input and its state;
//! * a pushed-to input's `Destinations` is a per-pipeline list too — one ingest
//!   endpoint per pipeline — so it names no more destinations than the pipelines
//!   it can feed.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted
//! is the emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are tables: each recorded case (a mixed batch delete, a
//! single-pipeline channel with two destination URLs, a standard channel with
//! one, an attachment of a deleted input, a push input with three destinations)
//! is ONE row, and the other rows are what the rule generalises over — which
//! lists the request fills, which class the channel has, how many entries the
//! per-pipeline list carries, which write path stores it, and which state the
//! named input is in.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{channel_encoder_settings, input_security_group, resolved_input_body};
use crate::wire::{call, error_code, error_message, medialive};

/// An id of the shape this service mints, that nothing in the account under test
/// minted — enough to fill an id list whose resources a row does not need.
const UNHELD_ID: &str = "9999999";

/// How long a row waits for a transition the clock owes to have been
/// materialised. Generous: the test polls for the CONDITION and this is only the
/// point at which it gives up.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The modelled client error every refusal in this suite carries
/// (`smithy_query(query="operation", operation="BatchDelete")` and its siblings:
/// `BadRequestException`, httpError 400).
const BAD_REQUEST: &str = "BadRequestException";

/// `CreateChannel` with `body`.
async fn create_channel(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", "/prod/channels", Some(body)).await
}

/// `UpdateChannel` on `id` with `body`.
async fn update_channel(router: &MultiServiceRouter, id: &str, body: Value) -> (u16, Value) {
    call(router, "PUT", &format!("/prod/channels/{id}"), Some(body)).await
}

/// `DescribeChannel` on `id`.
async fn describe_channel(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/prod/channels/{id}"), None).await
}

/// Every channel this account holds, as `ListChannels` reports them.
async fn listed_channels(router: &MultiServiceRouter) -> Vec<Value> {
    let (status, body) = call(router, "GET", "/prod/channels", None).await;
    assert_eq!(status, 200, "ListChannels: got {status}: {body}");
    body["channels"].as_array().cloned().unwrap_or_default()
}

/// `CreateInput` with `body`, after the resources the body references have been
/// put in the account.
async fn create_input(router: &MultiServiceRouter, body: Value) -> (Value, u16, Value) {
    let resolved = resolved_input_body(router, body).await;
    let (status, answered) = call(router, "POST", "/prod/inputs", Some(resolved.clone())).await;
    (resolved, status, answered)
}

/// `UpdateInput` on `id` with `body`.
async fn update_input(router: &MultiServiceRouter, id: &str, body: Value) -> (u16, Value) {
    call(router, "PUT", &format!("/prod/inputs/{id}"), Some(body)).await
}

/// `DescribeInput` on `id`.
async fn describe_input(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/prod/inputs/{id}"), None).await
}

/// Every input this account holds, as `ListInputs` reports them.
async fn listed_inputs(router: &MultiServiceRouter) -> Vec<Value> {
    let (status, body) = call(router, "GET", "/prod/inputs", None).await;
    assert_eq!(status, 200, "ListInputs: got {status}: {body}");
    body["inputs"].as_array().cloned().unwrap_or_default()
}

/// `DescribeInputSecurityGroup` on `id`.
async fn describe_group(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/prod/inputSecurityGroups/{id}"),
        None,
    )
    .await
}

/// One input a channel can attach, answered as its minted id: a `URL_PULL`
/// input, which needs nothing in the account but the redundant pair of source
/// URLs every pulled-from input needs.
async fn pullable_input(router: &MultiServiceRouter, name: &str) -> String {
    let (_, status, created) = create_input(
        router,
        json!({ "name": name, "type": "URL_PULL" }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: expected CreateInput to answer 201, got {status}: {created}"
    );
    created["input"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateInput reported no id: {created}"))
        .to_string()
}

/// The input `id`, read until it reports `state`, or a panic once
/// [`POLL_DEADLINE`] has passed: a transition the clock owes is materialised by
/// the next read, so a row that needs the settled state waits for THAT
/// condition rather than for a span of time.
async fn input_in_state(router: &MultiServiceRouter, id: &str, state: &str) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = describe_input(router, id).await;
        assert_eq!(
            status, 200,
            "DescribeInput {id}: expected 200, got {status}: {body}"
        );
        if body["state"] == json!(state) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "expected input {id} to reach {state}, it reported {:?}",
            body["state"]
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `State` the input `id` reports right now.
async fn input_state(router: &MultiServiceRouter, id: &str) -> String {
    let (status, body) = describe_input(router, id).await;
    assert_eq!(
        status, 200,
        "DescribeInput {id}: expected 200, got {status}: {body}"
    );
    body["state"].as_str().unwrap_or_default().to_string()
}

/// The ids one `BatchDelete` row builds its request from: one input and one
/// input security group this account holds, so a refused call can be seen to
/// have deleted neither.
struct Held {
    input: String,
    group: String,
}

/// One row of the batch-delete table: the case, the request body built from the
/// resources the account holds, and whether the request's SHAPE is one the
/// operation serves.
type BatchRow = (&'static str, fn(&Held) -> Value, bool);

/// Rule: `BatchDelete` deletes resources of ONE kind per call, so exactly one of
/// its four id lists must be non-empty; a request that fills two of them, or
/// none, is refused whole with the modelled `BadRequestException` (400) and the
/// fixed sentence that states the requirement, and nothing is deleted.
///
/// The rows span which lists the request fills: each list on its own, one list
/// beside an empty one (an empty list names no resource, so it is the same
/// request as one leaving the member out), two lists of different kinds — the
/// recorded case names an input and an input security group — every list at
/// once, no list at all, and only empty ones.
#[tokio::test]
async fn a_batch_delete_names_exactly_one_kind_of_resource() {
    let table: [BatchRow; 10] = [
        (
            "inputs only",
            |held| json!({ "inputIds": [held.input] }),
            true,
        ),
        (
            "input security groups only",
            |held| json!({ "inputSecurityGroupIds": [held.group] }),
            true,
        ),
        (
            "channels only",
            |_| json!({ "channelIds": [UNHELD_ID] }),
            true,
        ),
        (
            "multiplexes only",
            |_| json!({ "multiplexIds": [UNHELD_ID] }),
            true,
        ),
        (
            "one list beside an empty one",
            |held| json!({ "inputIds": [held.input], "channelIds": [] }),
            true,
        ),
        (
            "inputs and input security groups",
            |held| {
                json!({
                    "inputIds": [held.input],
                    "inputSecurityGroupIds": [held.group],
                })
            },
            false,
        ),
        (
            "channels and multiplexes",
            |_| json!({ "channelIds": [UNHELD_ID], "multiplexIds": [UNHELD_ID] }),
            false,
        ),
        (
            "every list at once",
            |held| {
                json!({
                    "channelIds": [UNHELD_ID],
                    "inputIds": [held.input],
                    "inputSecurityGroupIds": [held.group],
                    "multiplexIds": [UNHELD_ID],
                })
            },
            false,
        ),
        ("no list at all", |_| json!({}), false),
        (
            "only empty lists",
            |_| json!({ "inputIds": [], "inputSecurityGroupIds": [] }),
            false,
        ),
    ];

    for (case, build, is_served) in table {
        // An account of its own per row, so what one row deleted cannot change
        // what the next one sees.
        let router = medialive();
        let held = Held {
            input: pullable_input(&router, "batched").await,
            group: input_security_group(&router).await,
        };

        let (status, body) = call(&router, "POST", "/prod/batch/delete", Some(build(&held))).await;

        if is_served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a one-kind call to be answered, got {status}: {body}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the request shape to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(status, &body),
            BAD_REQUEST,
            "case {case:?}: expected the modelled client error, got {body}"
        );
        assert_eq!(
            error_message(&body),
            "Exactly one non-empty list of channel, multiplex, input, or input security group \
             IDs is required",
            "case {case:?}: expected the fixed sentence of this refusal class, got {:?}",
            error_message(&body)
        );

        // Nothing was deleted: the input is still the detached input it was and
        // the group is still idle, each read through its own reader.
        assert_eq!(
            input_state(&router, &held.input).await,
            "DETACHED",
            "case {case:?}: expected the refused call to leave input {} untouched",
            held.input
        );
        let (status, group) = describe_group(&router, &held.group).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInputSecurityGroup: got {status}: {group}"
        );
        assert_eq!(
            group["state"],
            json!("IDLE"),
            "case {case:?}: expected the refused call to leave group {} untouched, got {:?}",
            held.group,
            group["state"]
        );
    }
}

/// The destination a channel row declares its per-encoder settings on.
const DESTINATION_ID: &str = "cross-field-destination";

/// `count` per-encoder destination settings, each naming an output URL of its
/// own.
fn settings(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "url": format!("s3://example-bucket/stream{index}/index") }))
            .collect(),
    )
}

/// One `CreateChannel` body: the encoder settings a channel configuration has to
/// declare, the class the row names (none at all when `class` is `None`) and the
/// destinations it carries.
fn channel_body(class: Option<&str>, destinations: Value) -> Value {
    let mut body = json!({
        "name": "cross-field-channel",
        "encoderSettings": channel_encoder_settings(),
        "destinations": destinations,
    });
    if let Some(class) = class {
        body["channelClass"] = json!(class);
    }
    body
}

/// One row of the destination-settings table.
struct DestinationRow {
    case: &'static str,
    /// The `ChannelClass` the request names, or `None` to leave the member out
    /// (the service's own default class then applies).
    class: Option<&'static str>,
    /// The number of per-encoder `Settings` the destination declares, or `None`
    /// to leave the member out.
    declared: Option<usize>,
    /// The settings count the refusal states, or `None` when the configuration
    /// is one the service stores.
    refused_with: Option<usize>,
}

/// Rule: a channel destination's per-encoder `Settings` carries exactly one
/// entry per pipeline of the channel's class — the member is "Destination
/// settings for a standard output; one destination for each redundant encoder"
/// and `ChannelClass` gives a standard channel two encoding pipelines and a
/// single-pipeline channel one — so a write declaring any other number is
/// refused with the modelled `BadRequestException` (400), naming the count the
/// class requires and the destination it was declared on, and no channel is
/// created.
///
/// The rows span both directions of the mismatch (too many settings for a
/// single-pipeline channel — the recorded case — and too few for a standard
/// one), both classes of the `ChannelClass` enum plus the request that names no
/// class, the counts around each class's requirement, and the destination that
/// declares no standard output at all (an empty list, or no member), which the
/// per-encoder count says nothing about.
#[tokio::test]
async fn a_channel_destination_declares_one_setting_per_pipeline() {
    let table: [DestinationRow; 11] = [
        DestinationRow {
            case: "single pipeline, one setting",
            class: Some("SINGLE_PIPELINE"),
            declared: Some(1),
            refused_with: None,
        },
        DestinationRow {
            case: "single pipeline, two settings",
            class: Some("SINGLE_PIPELINE"),
            declared: Some(2),
            refused_with: Some(1),
        },
        DestinationRow {
            case: "single pipeline, three settings",
            class: Some("SINGLE_PIPELINE"),
            declared: Some(3),
            refused_with: Some(1),
        },
        DestinationRow {
            case: "standard, two settings",
            class: Some("STANDARD"),
            declared: Some(2),
            refused_with: None,
        },
        DestinationRow {
            case: "standard, one setting",
            class: Some("STANDARD"),
            declared: Some(1),
            refused_with: Some(2),
        },
        DestinationRow {
            case: "standard, three settings",
            class: Some("STANDARD"),
            declared: Some(3),
            refused_with: Some(2),
        },
        DestinationRow {
            case: "no class named, one setting",
            class: None,
            declared: Some(1),
            refused_with: Some(2),
        },
        DestinationRow {
            case: "no class named, two settings",
            class: None,
            declared: Some(2),
            refused_with: None,
        },
        DestinationRow {
            case: "single pipeline, no settings member",
            class: Some("SINGLE_PIPELINE"),
            declared: None,
            refused_with: None,
        },
        DestinationRow {
            case: "standard, no settings member",
            class: Some("STANDARD"),
            declared: None,
            refused_with: None,
        },
        DestinationRow {
            case: "standard, an empty settings list",
            class: Some("STANDARD"),
            declared: Some(0),
            refused_with: None,
        },
    ];

    for row in table {
        let case = row.case;
        let router = medialive();
        let destination = match row.declared {
            Some(count) => json!({ "id": DESTINATION_ID, "settings": settings(count) }),
            None => json!({ "id": DESTINATION_ID }),
        };
        let (status, body) =
            create_channel(&router, channel_body(row.class, json!([destination]))).await;

        match row.refused_with {
            Some(required) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the mismatch to be refused, got {status}: {body}"
                );
                assert_eq!(
                    error_code(status, &body),
                    BAD_REQUEST,
                    "case {case:?}: expected the modelled client error, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    format!(
                        "Exactly {required} destinations settings required for destination \
                         {DESTINATION_ID}"
                    ),
                    "case {case:?}: expected the refusal to state the required count and the \
                     destination, got {:?}",
                    error_message(&body)
                );
                assert!(
                    listed_channels(&router).await.is_empty(),
                    "case {case:?}: expected no channel to have been created, got {:?}",
                    listed_channels(&router).await
                );
            }
            None => {
                assert_eq!(
                    status, 201,
                    "case {case:?}: expected the configuration to be stored, got {status}: {body}"
                );
                assert_eq!(
                    body["channel"]["destinations"][0]["settings"]
                        .as_array()
                        .map(Vec::len),
                    Some(row.declared.unwrap_or(0)),
                    "case {case:?}: expected the declared settings to be stored, got {:?}",
                    body["channel"]["destinations"][0]
                );
            }
        }
    }
}

/// Rule, from the second write path: `UpdateChannel` stores the destinations its
/// request names under the same per-pipeline rule, against the class the channel
/// HAS, and a refused update leaves the stored destinations exactly as they
/// were.
#[tokio::test]
async fn an_update_declares_one_destination_setting_per_pipeline() {
    let router = medialive();
    let (status, created) = create_channel(
        &router,
        channel_body(
            Some("STANDARD"),
            json!([{ "id": DESTINATION_ID, "settings": settings(2) }]),
        ),
    )
    .await;
    assert_eq!(status, 201, "setup: CreateChannel: got {status}: {created}");
    let id = created["channel"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let stored = created["channel"]["destinations"].clone();

    // An update is accepted from a channel that has stopped, which is the state
    // the create's transient one settles into — waited for as a condition.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, described) = describe_channel(&router, &id).await;
        assert_eq!(status, 200, "DescribeChannel: got {status}: {described}");
        if described["state"] == json!("IDLE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected channel {id} to settle to IDLE, it reported {:?}",
            described["state"]
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // (case, the destinations the update names, the count its refusal states)
    let table: [(&str, Value, Option<usize>); 3] = [
        (
            "one setting on a standard channel",
            json!([{ "id": DESTINATION_ID, "settings": settings(1) }]),
            Some(2),
        ),
        (
            "three settings on a standard channel",
            json!([{ "id": DESTINATION_ID, "settings": settings(3) }]),
            Some(2),
        ),
        (
            "two settings on a standard channel",
            json!([{ "id": DESTINATION_ID, "settings": settings(2) }]),
            None,
        ),
    ];

    for (case, destinations, refused_with) in table {
        let (status, body) =
            update_channel(&router, &id, json!({ "destinations": destinations.clone() })).await;
        match refused_with {
            Some(required) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the update to be refused, got {status}: {body}"
                );
                assert_eq!(
                    error_message(&body),
                    format!(
                        "Exactly {required} destinations settings required for destination \
                         {DESTINATION_ID}"
                    ),
                    "case {case:?}: expected the same sentence the create answers, got {:?}",
                    error_message(&body)
                );
                let (status, after) = describe_channel(&router, &id).await;
                assert_eq!(status, 200, "case {case:?}: DescribeChannel: got {status}");
                assert_eq!(
                    after["destinations"], stored,
                    "case {case:?}: expected the refused update to write nothing, got {:?}",
                    after["destinations"]
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the matching count to be stored, got {status}: {body}"
            ),
        }
    }
}

/// Rule: an `InputAttachments` entry may not name an input this account holds
/// whose delete has been accepted — that input exists and is identified, it is
/// simply in a state no channel can ingest through — so the write is refused
/// with the modelled `BadRequestException` (400), naming the input the request
/// carried and the state it is in, and no channel is created.
///
/// The rows span the half of the deletion lifecycle the input stands in (the
/// transient state the delete starts, and the terminal one the clock settles it
/// into — the recorded case), and whether the request asked to be performed or
/// only checked (`DryRun` asks for the create to be CHECKED, so it earns every
/// refusal the real create would).
#[tokio::test]
async fn an_attachment_of_a_deleted_input_is_refused_naming_its_state() {
    // (case, whether to wait for the terminal state, whether the request is a
    //  dry run)
    let table: [(&str, bool, bool); 4] = [
        ("delete accepted, performed", false, false),
        ("delete accepted, checked only", false, true),
        ("delete settled, performed", true, false),
        ("delete settled, checked only", true, true),
    ];

    for (case, settled, dry_run) in table {
        let router = medialive();
        let input = pullable_input(&router, "doomed").await;
        let (status, deleted) = call(&router, "DELETE", &format!("/prod/inputs/{input}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteInput: got {status}: {deleted}"
        );

        // The state the refusal must name is the one the input's OWN reader
        // reports, so the row never assumes which half of the lifecycle the
        // clock has reached.
        let state = if settled {
            input_in_state(&router, &input, "DELETED").await;
            "DELETED".to_string()
        } else {
            input_state(&router, &input).await
        };

        let (status, body) = create_channel(
            &router,
            json!({
                "name": "attaches-a-deleted-input",
                "dryRun": dry_run,
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": [
                    { "inputAttachmentName": "doomed", "inputId": input },
                ],
            }),
        )
        .await;

        assert_eq!(
            status, 400,
            "case {case:?}: expected the attachment to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(status, &body),
            BAD_REQUEST,
            "case {case:?}: expected the modelled client error, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Input {input} is {state}"),
            "case {case:?}: expected the refusal to name the input and its state, got {:?}",
            error_message(&body)
        );
        assert!(
            listed_channels(&router).await.is_empty(),
            "case {case:?}: expected no channel to have been created, got {:?}",
            listed_channels(&router).await
        );
    }
}

/// Rule, from the second write path: `UpdateChannel` refuses an attachment it
/// would ADD that names an input whose delete has been accepted, with the same
/// sentence, and leaves the channel's attachments as they were.
#[tokio::test]
async fn an_update_that_attaches_a_deleted_input_is_refused() {
    let router = medialive();
    let (status, created) = create_channel(
        &router,
        json!({
            "name": "attaches-later",
            "encoderSettings": channel_encoder_settings(),
        }),
    )
    .await;
    assert_eq!(status, 201, "setup: CreateChannel: got {status}: {created}");
    let id = created["channel"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    // The attachments the created channel INGESTS through, which a refused update
    // must leave exactly as they are (a channel that attaches nothing is refused,
    // so the create carries one — `crate::channel_create_structure`).
    let attached = created["channel"]["inputAttachments"].clone();

    let input = pullable_input(&router, "doomed").await;
    let (status, deleted) = call(&router, "DELETE", &format!("/prod/inputs/{input}"), None).await;
    assert_eq!(status, 200, "setup: DeleteInput: got {status}: {deleted}");
    input_in_state(&router, &input, "DELETED").await;

    // The update is accepted from a channel that has stopped, which is the state
    // the create's transient one settles into.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, described) = describe_channel(&router, &id).await;
        assert_eq!(status, 200, "DescribeChannel: got {status}: {described}");
        if described["state"] == json!("IDLE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected channel {id} to settle to IDLE, it reported {:?}",
            described["state"]
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let (status, body) = update_channel(
        &router,
        &id,
        json!({
            "inputAttachments": [{ "inputAttachmentName": "doomed", "inputId": input }],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected the update to be refused, got {status}: {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Input {input} is DELETED"),
        "expected the same sentence the create answers, got {:?}",
        error_message(&body)
    );
    let (status, after) = describe_channel(&router, &id).await;
    assert_eq!(status, 200, "DescribeChannel: got {status}: {after}");
    assert_eq!(
        after["inputAttachments"], attached,
        "expected the refused update to write nothing, got {:?}",
        after["inputAttachments"]
    );
}

/// One row of the push-destination table.
struct PushRow {
    case: &'static str,
    /// The `InputType` the request names.
    input_type: &'static str,
    /// The destinations the request declares.
    destinations: usize,
    /// Whether the count is one the service stores.
    is_served: bool,
}

/// `count` ingest destinations, each naming a stream location of its own.
fn push_destinations(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "streamName": format!("live/stream{index}") }))
            .collect(),
    )
}

/// Rule: a pushed-to input's `Destinations` is a per-pipeline list — one ingest
/// endpoint per pipeline, and MediaLive runs at most two — so a write declaring
/// more is refused with the modelled `BadRequestException` (400) and the fixed
/// sentence of that class; no input is created and the security group the
/// request named takes no reference.
///
/// The rows span the pushed-to `InputType`s (`RTMP_PUSH`, `RTP_PUSH`,
/// `UDP_PUSH`), the counts at and above the bound — the recorded case is the
/// `RTMP_PUSH` input declaring three — and a pulled-from type, whose ingest is
/// its `Sources` rather than a per-pipeline destination list.
#[tokio::test]
async fn a_push_input_declares_one_destination_per_pipeline() {
    let table: [PushRow; 7] = [
        PushRow {
            case: "rtmp push, one destination",
            input_type: "RTMP_PUSH",
            destinations: 1,
            is_served: true,
        },
        PushRow {
            case: "rtmp push, two destinations",
            input_type: "RTMP_PUSH",
            destinations: 2,
            is_served: true,
        },
        PushRow {
            case: "rtmp push, three destinations",
            input_type: "RTMP_PUSH",
            destinations: 3,
            is_served: false,
        },
        PushRow {
            case: "rtp push, three destinations",
            input_type: "RTP_PUSH",
            destinations: 3,
            is_served: false,
        },
        PushRow {
            case: "udp push, four destinations",
            input_type: "UDP_PUSH",
            destinations: 4,
            is_served: false,
        },
        PushRow {
            case: "udp push, two destinations",
            input_type: "UDP_PUSH",
            destinations: 2,
            is_served: true,
        },
        PushRow {
            case: "url pull, three destinations",
            input_type: "URL_PULL",
            destinations: 3,
            is_served: true,
        },
    ];

    for row in table {
        let case = row.case;
        let router = medialive();
        let (requested, status, body) = create_input(
            &router,
            json!({
                "name": "pushed-to",
                "type": row.input_type,
                "destinations": push_destinations(row.destinations),
            }),
        )
        .await;

        if row.is_served {
            assert_eq!(
                status, 201,
                "case {case:?}: expected the input to be created, got {status}: {body}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the count to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(status, &body),
            BAD_REQUEST,
            "case {case:?}: expected the modelled client error, got {body}"
        );
        assert_eq!(
            error_message(&body),
            "At least one destination and at most two destinations required",
            "case {case:?}: expected the fixed sentence of this refusal class, got {:?}",
            error_message(&body)
        );
        assert!(
            listed_inputs(&router).await.is_empty(),
            "case {case:?}: expected no input to have been created, got {:?}",
            listed_inputs(&router).await
        );

        // The group the refused request named took no reference: its `Inputs` is
        // "the list of inputs currently using this Input Security Group".
        let group = requested["inputSecurityGroups"][0]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the row named no security group"))
            .to_string();
        let (status, described) = describe_group(&router, &group).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInputSecurityGroup: got {status}: {described}"
        );
        assert_eq!(
            described["inputs"],
            json!([]),
            "case {case:?}: expected the group to hold no input, got {:?}",
            described["inputs"]
        );
    }
}

/// Rule, from the second write path: `UpdateInput` rewrites a pushed-to input's
/// destinations under the same per-pipeline bound, and a refused update leaves
/// the stored destinations exactly as they were.
#[tokio::test]
async fn an_update_declares_one_push_destination_per_pipeline() {
    let router = medialive();
    let (_, status, created) = create_input(
        &router,
        json!({
            "name": "grows",
            "type": "RTMP_PUSH",
            "destinations": push_destinations(2),
        }),
    )
    .await;
    assert_eq!(status, 201, "setup: CreateInput: got {status}: {created}");
    let id = created["input"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let stored = created["input"]["destinations"].clone();

    let (status, body) =
        update_input(&router, &id, json!({ "destinations": push_destinations(3) })).await;
    assert_eq!(
        status, 400,
        "expected the third destination to be refused, got {status}: {body}"
    );
    assert_eq!(
        error_message(&body),
        "At least one destination and at most two destinations required",
        "expected the same sentence the create answers, got {:?}",
        error_message(&body)
    );
    let (status, after) = describe_input(&router, &id).await;
    assert_eq!(status, 200, "DescribeInput: got {status}: {after}");
    assert_eq!(
        after["destinations"], stored,
        "expected the refused update to write nothing, got {:?}",
        after["destinations"]
    );
}
