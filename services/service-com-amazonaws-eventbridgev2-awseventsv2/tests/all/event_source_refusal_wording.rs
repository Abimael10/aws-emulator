// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the refusals of a resource that is gone, of one that
//! has been revoked, and of a forwarding pattern it cannot serve — read where a
//! client reads them: off the `rpcv2Cbor` wire.
//!
//! RULE 1 — a `ResourceNotFoundException` of this service names the KIND of
//! resource that was not found, then `" not found: "`, then the identifier the
//! request supplied (the ARN for an ARN-addressed resource, the name for a bus,
//! which is addressed by name inside its ARN). It is the only thing that tells a
//! caller which of several same-shaped 404s it has, so it is the same sentence
//! whichever operation resolved the resource and whether the resource was never
//! minted or has been deleted. The rows span the resource kind (EventSource,
//! Subscriber, event bus), the operation that resolves it (a read, an update, a
//! delete, a revoke), the origin type of an EventSource (`aws.service` and
//! `aws.partner`), and the reason it is absent (never minted, or deleted).
//!
//! RULE 2 — an `InvalidStateException` raised because the addressed resource has
//! been REVOKED reads `"<ResourceKind> is revoked and cannot be updated"`, fixed
//! text naming the kind and nothing of the request. The rows span both revocable
//! kinds.
//!
//! RULE 3 — an EventSource's forwarding `Pattern` is held to its own modelled
//! contract before anything is stored: it must be a JSON document, and for an
//! `AwsServiceEventsConfiguration` it must not set any of the top-level fields
//! the forwarding rule writes itself. A request breaking either is refused `400`
//! with the `InvalidInputException` this service's model lists as a common error
//! of every operation, and changes nothing; one that breaks neither is served
//! and read back. The rows span the reserved-field catalogue the model names, the
//! position of the field (top level versus nested, which the rule does not
//! reach), the configuration variant (the partner variant reserves no field), and
//! the operation the pattern arrives on (the create and the update).
//!
//! Nothing here waits on the wall clock: a wait is for a CONDITION, polled under
//! a generous deadline. The wire harness, the input builders and the refusal
//! decoder are `modelled_input_refusal.rs`'s, reused rather than copied.

use std::time::{Duration, Instant};

use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::delete_event_source::DeleteEventSource;
use eventsv2::ops::delete_subscriber::DeleteSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::update_event_source::UpdateEventSource;
use eventsv2::ops::update_subscriber::UpdateSubscriber;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventSourceInput, DeleteEventBusInput,
    DeleteEventSourceInput, DeleteSubscriberInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeSubscriberInput,
    EventSourceConfiguration, PartnerEventsSourceConfiguration, RevokeResourceInput,
    UpdateEventSourceInput, UpdateSubscriberInput,
};

use crate::modelled_input_refusal::{
    create_subscriber, refusal, rendered, unminted_arn, Wire, INVALID_INPUT_EXCEPTION, REGION,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The error shape a read of a resource that is not there answers with, and the
/// status its `@httpError` declares.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The error shape a request the addressed resource cannot take answers with,
/// and the status its `@httpError` declares.
const INVALID_STATE_EXCEPTION: &str = "InvalidStateException";
const INVALID_STATE_STATUS: u16 = 409;

/// The status `InvalidInputException`'s own `@httpError` declares.
const INVALID_INPUT_STATUS: u16 = 400;

/// How long a poll waits for a condition the emulator owes, and how long one
/// pass waits before looking again. The deadline is far above every span this
/// service provisions in, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The top-level fields of an AWS-service EventSource's forwarding pattern that
/// the service sets itself, spelled out here rather than taken from the crate so
/// a changed catalogue fails this suite.
///
/// Source: `smithy_query(query="doc", shape="AwsServiceEventsSourceConfiguration",
/// member="Pattern")` — "Do not include source, account, or region as top-level
/// fields."
const SERVICE_SET_FIELDS: [&str; 3] = ["source", "account", "region"];

/// The id segment of an `EventSourceArn`, which the modelled pattern pins at
/// exactly `[a-z0-9]{25}`.
/// Source: `smithy_query(query="shape", shape="EventSourceArn")`.
const RESOURCE_ID_LEN: usize = 25;

/// A well-formed `EventSourceArn` of origin type `origin_type` that this service
/// never minted.
fn unminted_source_arn(origin_type: &str, name: &str, id: &str) -> String {
    assert_eq!(
        id.len(),
        RESOURCE_ID_LEN,
        "the modelled EventSourceArn pattern pins the id segment at {RESOURCE_ID_LEN} characters"
    );
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/{origin_type}/{name}/{id}")
}

/// A well-formed `SubscriberArn` that this service never minted.
/// Source: `smithy_query(query="shape", shape="SubscriberArn")`.
fn unminted_subscriber_arn(name: &str, id: &str) -> String {
    assert_eq!(
        id.len(),
        RESOURCE_ID_LEN,
        "the modelled SubscriberArn pattern pins the id segment at {RESOURCE_ID_LEN} characters"
    );
    format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/{name}/{id}")
}

/// An `AwsServiceEventsConfiguration` forwarding `aws_service`, optionally
/// filtered by `pattern`.
fn aws_service_configuration(aws_service: &str, pattern: Option<&str>) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: aws_service.to_string(),
        pattern: pattern.map(str::to_string),
        on_failure_configuration: None,
    })
}

/// A `PartnerEventsConfiguration`, optionally filtered by `pattern`.
fn partner_configuration(pattern: Option<&str>) -> EventSourceConfiguration {
    EventSourceConfiguration::PartnerEventsConfiguration(PartnerEventsSourceConfiguration {
        partner_event_source_arn: format!(
            "arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/acme/stream"
        ),
        pattern: pattern.map(str::to_string),
        partner_bus_kms_key_identifier: None,
        on_failure_configuration: None,
    })
}

/// A `CreateEventSourceInput` carrying `configuration`.
fn create_with(
    name: &str,
    event_bus_arn: &str,
    configuration: EventSourceConfiguration,
) -> CreateEventSourceInput {
    CreateEventSourceInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        configuration,
        description: None,
        tags: None,
        client_token: Some(format!("token-for-{name}")),
    }
}

/// The `Pattern` of a described source's configuration, whichever variant it is.
fn pattern_of(configuration: Option<&EventSourceConfiguration>) -> Option<String> {
    match configuration? {
        EventSourceConfiguration::AwsServiceEventsConfiguration(configuration) => {
            configuration.pattern.clone()
        }
        EventSourceConfiguration::PartnerEventsConfiguration(configuration) => {
            configuration.pattern.clone()
        }
    }
}

/// Assert one refusal: the status, the modelled error shape and the whole
/// `Message`.
fn assert_refusal(
    case: &str,
    status: u16,
    body: &[u8],
    expected_status: u16,
    expected_code: &str,
    expected_message: &str,
) {
    assert_eq!(
        status,
        expected_status,
        "case {case:?}: expected HTTP {expected_status}, got {status} with {:?}",
        rendered(body)
    );
    let (code, message) = refusal(case, body);
    assert_eq!(
        code, expected_code,
        "case {case:?}: expected {expected_code}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected {expected_message:?}, got {message:?}"
    );
}

/// Create a source on `bus` and hand back its minted ARN.
async fn minted_source(
    wire: &Wire,
    case: &str,
    name: &str,
    bus: &str,
    configuration: EventSourceConfiguration,
) -> String {
    wire.ok::<CreateEventSource<InMemoryEngine>>(case, &create_with(name, bus, configuration))
        .await
        .event_source_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource must report an EventSourceArn"))
}

/// Poll `DescribeEventSource` on `arn` until it stops answering 200, and hand
/// back the status and body of the refusal. Waits for the CONDITION, never for a
/// duration.
async fn until_gone(wire: &Wire, case: &str, arn: &str) -> (u16, bytes::Bytes) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = wire
            .call::<DescribeEventSource<InMemoryEngine>>(&DescribeEventSourceInput {
                event_source_arn: arn.to_string(),
            })
            .await;
        if status != 200 {
            return (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {arn} must stop resolving once it is deleted, but the read still \
             answers 200 with {:?}",
            rendered(&body)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE 1, on an EventSource that WAS there: a delete removes it, so every later
/// read of its ARN is refused as the EventSource it was — whichever operation
/// resolves it, and whichever origin type the source had.
#[tokio::test]
async fn a_deleted_event_source_is_refused_as_not_found_by_every_operation_that_resolves_it() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "deleted-source-bus").await;

    for (case, configuration) in [
        (
            "an aws.service source with no pattern",
            aws_service_configuration("aws.ec2", None),
        ),
        (
            "an aws.service source carrying a pattern",
            aws_service_configuration("aws.s3", Some(r#"{"detail-type": ["Object Created"]}"#)),
        ),
        (
            "an aws.partner source",
            partner_configuration(Some(r#"{"detail-type": ["Order Placed"]}"#)),
        ),
    ] {
        let name = format!("gone-{}", case.len());
        let arn = minted_source(&wire, case, &name, &bus, configuration).await;

        // It reads back while it is there, so what follows is about the DELETE
        // and not about a source that was never stored.
        let read = wire
            .ok::<DescribeEventSource<InMemoryEngine>>(
                case,
                &DescribeEventSourceInput {
                    event_source_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            read.event_source_arn.as_deref(),
            Some(arn.as_str()),
            "case {case:?}: the created source must read back at its own ARN, got {read:?}"
        );

        wire.ok::<DeleteEventSource<InMemoryEngine>>(
            case,
            &DeleteEventSourceInput {
                event_source_arn: arn.clone(),
            },
        )
        .await;

        let expected = format!("EventSource not found: {arn}");
        let (status, body) = until_gone(&wire, case, &arn).await;
        assert_refusal(
            case,
            status,
            &body,
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            &expected,
        );

        // Every other operation that resolves the same ARN words it the same
        // way: the refusal belongs to the resource, not to one handler.
        let (status, body) = wire
            .call::<UpdateEventSource<InMemoryEngine>>(&UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                description: Some("after the delete".to_string()),
                ..Default::default()
            })
            .await;
        assert_refusal(
            &format!("{case} (UpdateEventSource)"),
            status,
            &body,
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            &expected,
        );

        let (status, body) = wire
            .call::<DeleteEventSource<InMemoryEngine>>(&DeleteEventSourceInput {
                event_source_arn: arn.clone(),
            })
            .await;
        assert_refusal(
            &format!("{case} (DeleteEventSource again)"),
            status,
            &body,
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            &expected,
        );

        let (status, body) = wire
            .call::<RevokeResource<InMemoryEngine>>(&RevokeResourceInput { arn: arn.clone() })
            .await;
        assert_refusal(
            &format!("{case} (RevokeResource)"),
            status,
            &body,
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            &expected,
        );
    }
}

/// RULE 1, across the resource KINDS: each 404 names the kind of resource that
/// was not found and the identifier the request supplied, so two absent
/// resources of different kinds are told apart by the message alone.
#[tokio::test]
async fn every_not_found_refusal_names_its_resource_kind_and_the_identifier_requested() {
    let wire = Wire::new();

    let absent_service_source =
        unminted_source_arn("aws.service", "no-such-source", "aaaaaaaaaaaaaaaaaaaaaaaaa");
    let absent_partner_source =
        unminted_source_arn("aws.partner", "no-such-partner", "bbbbbbbbbbbbbbbbbbbbbbbbb");
    let absent_subscriber = unminted_subscriber_arn("no-such-sub", "ccccccccccccccccccccccccc");

    // The EventSource reads, one per origin type: the kind is the resource's,
    // not the origin's.
    for (case, arn) in [
        ("an aws.service EventSource", absent_service_source.as_str()),
        ("an aws.partner EventSource", absent_partner_source.as_str()),
    ] {
        let (status, body) = wire
            .call::<DescribeEventSource<InMemoryEngine>>(&DescribeEventSourceInput {
                event_source_arn: arn.to_string(),
            })
            .await;
        assert_refusal(
            case,
            status,
            &body,
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            &format!("EventSource not found: {arn}"),
        );
    }

    // The subscriber reads, through three operations that resolve one.
    let expected_subscriber = format!("Subscriber not found: {absent_subscriber}");
    let (status, body) = wire
        .call::<DescribeSubscriber<InMemoryEngine>>(&DescribeSubscriberInput {
            subscriber_arn: absent_subscriber.clone(),
        })
        .await;
    assert_refusal(
        "DescribeSubscriber on a subscriber that was never minted",
        status,
        &body,
        NOT_FOUND_STATUS,
        RESOURCE_NOT_FOUND_EXCEPTION,
        &expected_subscriber,
    );
    let (status, body) = wire
        .call::<UpdateSubscriber<InMemoryEngine>>(&UpdateSubscriberInput {
            subscriber_arn: absent_subscriber.clone(),
            description: Some("nothing to update".to_string()),
            ..Default::default()
        })
        .await;
    assert_refusal(
        "UpdateSubscriber on a subscriber that was never minted",
        status,
        &body,
        NOT_FOUND_STATUS,
        RESOURCE_NOT_FOUND_EXCEPTION,
        &expected_subscriber,
    );
    let (status, body) = wire
        .call::<DeleteSubscriber<InMemoryEngine>>(&DeleteSubscriberInput {
            subscriber_arn: absent_subscriber.clone(),
        })
        .await;
    assert_refusal(
        "DeleteSubscriber on a subscriber that was never minted",
        status,
        &body,
        NOT_FOUND_STATUS,
        RESOURCE_NOT_FOUND_EXCEPTION,
        &expected_subscriber,
    );

    // The bus, which is addressed by the NAME its ARN carries, so that is the
    // identifier its own refusal reports. Read on a bus this service DID mint
    // and has since removed: an ARN whose identifier it never handed out
    // addresses no resource of this service at all, and takes the fixed text of
    // the class instead — the row below it.
    let retired_name = "retired-wording-bus";
    let retired = wire.active_bus("the retired bus", retired_name).await;
    wire.ok::<DeleteEventBus<InMemoryEngine>>(
        "the retired bus",
        &DeleteEventBusInput {
            event_bus_arn: retired.clone(),
        },
    )
    .await;
    let case = "DescribeEventBus on a bus this service minted and removed";
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = wire
            .call::<DescribeEventBus<InMemoryEngine>>(&DescribeEventBusInput {
                event_bus_arn: retired.clone(),
            })
            .await;
        if status != 200 {
            assert_refusal(
                case,
                status,
                &body,
                NOT_FOUND_STATUS,
                RESOURCE_NOT_FOUND_EXCEPTION,
                &format!("Event bus not found: {retired_name}"),
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted bus must stop resolving, but the read still answers 200 \
             with {:?}",
            rendered(&body)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // The boundary of the rule: an ARN carrying an identifier this service never
    // handed out names no resource of this service, so there is no resource kind
    // to name and the refusal is the fixed text of the class.
    let never_minted = unminted_arn(REGION, "no-such-bus", "ddddddddddddddddddddddddd");
    let (status, body) = wire
        .call::<DescribeEventBus<InMemoryEngine>>(&DescribeEventBusInput {
            event_bus_arn: never_minted.clone(),
        })
        .await;
    assert_refusal(
        "DescribeEventBus on an identifier this service never minted",
        status,
        &body,
        NOT_FOUND_STATUS,
        RESOURCE_NOT_FOUND_EXCEPTION,
        "Resource does not exist",
    );
}

/// RULE 2: a revoked resource refuses an update with the fixed sentence naming
/// the KIND that is revoked — the same shape of sentence for both revocable
/// kinds, and naming nothing of the request.
#[tokio::test]
async fn a_revoked_resource_refuses_an_update_naming_the_kind_that_is_revoked() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "revoked-wording-bus").await;

    let source = minted_source(
        &wire,
        "a revoked EventSource",
        "revoked-source",
        &bus,
        aws_service_configuration("aws.ec2", None),
    )
    .await;
    let subscriber = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(
            "a revoked subscriber",
            &create_subscriber("revoked-sub", &bus, Some("token-revoked-sub")),
        )
        .await
        .subscriber_arn
        .expect("CreateSubscriber must report a SubscriberArn");

    for arn in [source.as_str(), subscriber.as_str()] {
        wire.ok::<RevokeResource<InMemoryEngine>>(
            "the revoke",
            &RevokeResourceInput {
                arn: arn.to_string(),
            },
        )
        .await;
    }

    let (status, body) = wire
        .call::<UpdateEventSource<InMemoryEngine>>(&UpdateEventSourceInput {
            event_source_arn: source.clone(),
            description: Some("a change the source cannot take".to_string()),
            ..Default::default()
        })
        .await;
    assert_refusal(
        "UpdateEventSource on a revoked EventSource",
        status,
        &body,
        INVALID_STATE_STATUS,
        INVALID_STATE_EXCEPTION,
        "EventSource is revoked and cannot be updated",
    );

    let (status, body) = wire
        .call::<UpdateSubscriber<InMemoryEngine>>(&UpdateSubscriberInput {
            subscriber_arn: subscriber.clone(),
            description: Some("a change the subscriber cannot take".to_string()),
            ..Default::default()
        })
        .await;
    assert_refusal(
        "UpdateSubscriber on a revoked subscriber",
        status,
        &body,
        INVALID_STATE_STATUS,
        INVALID_STATE_EXCEPTION,
        "Subscriber is revoked and cannot be updated",
    );
}

/// RULE 3, on the CREATE: a forwarding pattern that is not JSON, or that sets a
/// field the forwarding rule writes itself, is refused and mints nothing; one
/// that breaks neither rule is served and reads back as submitted.
#[tokio::test]
async fn a_create_holds_the_forwarding_pattern_to_its_modelled_contract() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "create-pattern-bus").await;

    // Every reserved field of the catalogue the member's doc names, including
    // the two the issue's own instance never cited.
    for field in SERVICE_SET_FIELDS {
        let case = format!("a create whose Pattern sets '{field}'");
        let pattern = format!(r#"{{"{field}": ["aws.ec2"], "detail-type": ["x"]}}"#);
        let (status, body) = wire
            .call::<CreateEventSource<InMemoryEngine>>(&create_with(
                "reserved-field-source",
                &bus,
                aws_service_configuration("aws.ec2", Some(&pattern)),
            ))
            .await;
        assert_refusal(
            &case,
            status,
            &body,
            INVALID_INPUT_STATUS,
            INVALID_INPUT_EXCEPTION,
            &format!(
                "Pattern must not set the key '{field}'; the forwarding rule sets it from the \
                 requested AwsService and the source account and region"
            ),
        );
    }

    // A text that is not a JSON document at all.
    for (case, pattern) in [
        ("a create whose Pattern is not JSON", "{not json"),
        ("a create whose Pattern is bare text", "detail-type"),
    ] {
        let (status, body) = wire
            .call::<CreateEventSource<InMemoryEngine>>(&create_with(
                "unreadable-pattern-source",
                &bus,
                aws_service_configuration("aws.ec2", Some(pattern)),
            ))
            .await;
        assert_refusal(
            case,
            status,
            &body,
            INVALID_INPUT_STATUS,
            INVALID_INPUT_EXCEPTION,
            "Pattern is not valid JSON",
        );
    }

    // The refused creates minted nothing: the names they asked for are free.
    for name in ["reserved-field-source", "unreadable-pattern-source"] {
        let arn = minted_source(
            &wire,
            "a refused create mints nothing",
            name,
            &bus,
            aws_service_configuration("aws.ec2", Some(r#"{"detail-type": ["x"]}"#)),
        )
        .await;
        assert!(
            arn.contains(name),
            "case \"a refused create mints nothing\": the minted ARN must carry the name {name:?}, \
             got {arn:?}"
        );
    }

    // What the rule does NOT reach, each served and read back: no pattern at
    // all, an empty pattern (the member's `@length` admits one), a reserved name
    // nested rather than top level, and the partner variant, whose own `Pattern`
    // reserves no field.
    for (case, name, configuration, expected_pattern) in [
        (
            "no pattern at all",
            "served-none",
            aws_service_configuration("aws.ec2", None),
            None,
        ),
        (
            "an empty pattern",
            "served-empty",
            aws_service_configuration("aws.ec2", Some("")),
            Some(""),
        ),
        (
            "a reserved name nested rather than top level",
            "served-nested",
            aws_service_configuration("aws.ec2", Some(r#"{"detail": {"source": ["x"]}}"#)),
            Some(r#"{"detail": {"source": ["x"]}}"#),
        ),
        (
            "the partner variant, which reserves no field",
            "served-partner",
            partner_configuration(Some(r#"{"source": ["acme"]}"#)),
            Some(r#"{"source": ["acme"]}"#),
        ),
    ] {
        let arn = minted_source(&wire, case, name, &bus, configuration).await;
        let read = wire
            .ok::<DescribeEventSource<InMemoryEngine>>(
                case,
                &DescribeEventSourceInput {
                    event_source_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            pattern_of(read.configuration.as_ref()).as_deref(),
            expected_pattern,
            "case {case:?}: the stored Pattern must be the one the create carried, got {:?}",
            read.configuration
        );
    }
}

/// RULE 3, on the UPDATE: the same contract, and a refused update changes
/// nothing — the source keeps the configuration and the description it had.
#[tokio::test]
async fn a_refused_update_leaves_the_stored_event_source_exactly_as_it_was() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "update-pattern-bus").await;

    let accepted = r#"{"detail-type": ["Object Created"]}"#;
    let arn = minted_source(
        &wire,
        "the subject",
        "update-pattern-source",
        &bus,
        aws_service_configuration("aws.s3", Some(accepted)),
    )
    .await;
    wire.ok::<UpdateEventSource<InMemoryEngine>>(
        "the first description",
        &UpdateEventSourceInput {
            event_source_arn: arn.clone(),
            description: Some("first description".to_string()),
            ..Default::default()
        },
    )
    .await;

    let mut refused: Vec<(String, String)> = SERVICE_SET_FIELDS
        .iter()
        .map(|field| {
            (
                format!("an update whose Pattern sets '{field}'"),
                format!(
                    "Pattern must not set the key '{field}'; the forwarding rule sets it from \
                     the requested AwsService and the source account and region"
                ),
            )
        })
        .collect();
    refused.push((
        "an update whose Pattern is not JSON".to_string(),
        "Pattern is not valid JSON".to_string(),
    ));

    for (index, (case, expected)) in refused.iter().enumerate() {
        let pattern = match SERVICE_SET_FIELDS.get(index) {
            Some(field) => format!(r#"{{"{field}": ["aws.s3"]}}"#),
            None => "{not json".to_string(),
        };
        let (status, body) = wire
            .call::<UpdateEventSource<InMemoryEngine>>(&UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                configuration: Some(aws_service_configuration("aws.s3", Some(&pattern))),
                description: Some("a description the refusal must not store".to_string()),
            })
            .await;
        assert_refusal(
            case,
            status,
            &body,
            INVALID_INPUT_STATUS,
            INVALID_INPUT_EXCEPTION,
            expected,
        );

        // Read back after EVERY refusal: the last accepted configuration and
        // description are what the source still reports.
        let read = wire
            .ok::<DescribeEventSource<InMemoryEngine>>(
                case,
                &DescribeEventSourceInput {
                    event_source_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            pattern_of(read.configuration.as_ref()).as_deref(),
            Some(accepted),
            "case {case:?}: the refused update must leave the stored Pattern as it was, got {:?}",
            read.configuration
        );
        assert_eq!(
            read.description.as_deref(),
            Some("first description"),
            "case {case:?}: the refused update must leave the stored Description as it was, got \
             {:?}",
            read.description
        );
    }

    // An update the contract accepts does reach the source, so the refusals
    // above are about the pattern and not about updates being inert.
    let next = r#"{"detail-type": ["Object Deleted"]}"#;
    wire.ok::<UpdateEventSource<InMemoryEngine>>(
        "an accepted update",
        &UpdateEventSourceInput {
            event_source_arn: arn.clone(),
            configuration: Some(aws_service_configuration("aws.s3", Some(next))),
            ..Default::default()
        },
    )
    .await;
    let read = wire
        .ok::<DescribeEventSource<InMemoryEngine>>(
            "an accepted update",
            &DescribeEventSourceInput {
                event_source_arn: arn.clone(),
            },
        )
        .await;
    assert_eq!(
        pattern_of(read.configuration.as_ref()).as_deref(),
        Some(next),
        "case \"an accepted update\": the stored Pattern must be the one the update carried, got \
         {:?}",
        read.configuration
    );
}
