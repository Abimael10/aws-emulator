// SPDX-License-Identifier: CC0-1.0

//! The `ResourceNotFoundException` an EventSource-addressed read answers with
//! names the EventSource kind and the identifier THE REQUEST supplied, read
//! where a client reads it: off the `rpcv2Cbor` wire.
//!
//! RULE — a read of an `EventSourceArn` the store holds no source at is refused
//! `404` with `ResourceNotFoundException` whose whole `Message` is
//! `"EventSource not found: "` followed by the `EventSourceArn` member exactly
//! as submitted. The fixed text is the resource kind's; the single variable part
//! comes from the request, so it is the ARN asked about rather than any ARN the
//! store holds — whether the source was minted here and deleted, whether its
//! name has since been taken by another source, and whether the ARN names a
//! region or a partition this store never minted in at all.
//!
//! The rows span what the rule generalises over: the lifecycle that made the
//! source absent (deleted after a full create/read/list round trip, or never
//! minted), the origin type the ARN carries (`aws.service` and `aws.partner`),
//! the form and length of the name segment (plain, punctuated, one character,
//! the modelled maximum), and the region and partition segments of the ARN.
//! Each lifecycle row also checks the reads that frame the refusal: the source
//! reads back and is enumerated while it is there, and is enumerated no longer
//! once it is gone.
//!
//! The wire harness and the refusal decoder are `modelled_input_refusal.rs`'s,
//! reused rather than copied. Nothing here waits on the wall clock: the only
//! wait is the bus becoming `ACTIVE`, which the harness polls as a CONDITION.

use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::delete_event_source::DeleteEventSource;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::list_event_sources::ListEventSources;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventSourceInput, DeleteEventSourceInput,
    DescribeEventSourceInput, EventSourceConfiguration, ListEventSourcesInput,
    PartnerEventsSourceConfiguration,
};

use crate::modelled_input_refusal::{refusal, rendered, Wire, REGION};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The error shape a read of a resource that is not there answers with, and the
/// status its `@httpError` declares.
/// Source: `smithy_query(query="operation", operation="DescribeEventSource")`.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The fixed text of that refusal for the EventSource resource kind: what is
/// NOT taken from the request.
const EVENT_SOURCE_NOT_FOUND_PREFIX: &str = "EventSource not found: ";

/// The id segment of an `EventSourceArn`, which the modelled pattern pins at
/// exactly `[a-z0-9]{25}`.
/// Source: `smithy_query(query="operation", operation="DescribeEventSource")`.
const RESOURCE_ID_LEN: usize = 25;

/// The longest name segment an `EventSourceArn` admits: its pattern pins the
/// segment at `[A-Za-z0-9][\.\-_A-Za-z0-9]{0,255}`.
/// Source: `smithy_query(query="operation", operation="DescribeEventSource")`.
const ARN_NAME_MAX: usize = 256;

/// The page size a list request asks for, inside the modelled `MaxResults`
/// range of 1..100.
/// Source: `smithy_query(query="operation", operation="ListEventSources")`.
const PAGE_SIZE: i32 = 100;

/// A well-formed `EventSourceArn` in `partition` and `region`, of origin type
/// `origin_type`, that this store never minted.
fn unminted_source_arn(
    partition: &str,
    region: &str,
    origin_type: &str,
    name: &str,
    id: &str,
) -> String {
    assert_eq!(
        id.len(),
        RESOURCE_ID_LEN,
        "the modelled EventSourceArn pattern pins the id segment at {RESOURCE_ID_LEN} characters"
    );
    assert!(
        name.len() <= ARN_NAME_MAX,
        "the modelled EventSourceArn pattern admits at most {ARN_NAME_MAX} characters of name, \
         got {}",
        name.len()
    );
    format!("arn:{partition}:events:{region}:{ACCOUNT}:event-sourcev2/{origin_type}/{name}/{id}")
}

/// An `AwsServiceEventsConfiguration` forwarding `aws_service`.
fn aws_service_configuration(aws_service: &str) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: aws_service.to_string(),
        pattern: None,
        on_failure_configuration: None,
    })
}

/// A `PartnerEventsConfiguration`.
fn partner_configuration() -> EventSourceConfiguration {
    EventSourceConfiguration::PartnerEventsConfiguration(PartnerEventsSourceConfiguration {
        partner_event_source_arn: format!(
            "arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/acme/stream"
        ),
        pattern: None,
        partner_bus_kms_key_identifier: None,
        on_failure_configuration: None,
    })
}

/// A `CreateEventSourceInput` for `name` on `bus`, carrying `token` so a
/// re-create of a freed name is a request of its own rather than a retry.
fn create_input(
    name: &str,
    bus: &str,
    token: &str,
    description: Option<&str>,
    configuration: EventSourceConfiguration,
) -> CreateEventSourceInput {
    CreateEventSourceInput {
        name: name.to_string(),
        event_bus_arn: bus.to_string(),
        configuration,
        description: description.map(str::to_string),
        tags: None,
        client_token: Some(token.to_string()),
    }
}

/// Assert one refusal: the status, the modelled error shape and the WHOLE
/// `Message`.
fn assert_not_found(case: &str, status: u16, body: &[u8], expected_message: &str) {
    assert_eq!(
        status,
        NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {status} with {:?}",
        rendered(body)
    );
    let (code, message) = refusal(case, body);
    assert_eq!(
        code, RESOURCE_NOT_FOUND_EXCEPTION,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected {expected_message:?}, got {message:?}"
    );
}

/// Read `DescribeEventSource` at `arn` and hand back the status and body.
async fn describe(wire: &Wire, arn: &str) -> (u16, bytes::Bytes) {
    wire.call::<DescribeEventSource<InMemoryEngine>>(&DescribeEventSourceInput {
        event_source_arn: arn.to_string(),
    })
    .await
}

/// The ARNs `ListEventSources` reports for `bus`.
async fn listed_arns(wire: &Wire, case: &str, bus: &str) -> Vec<String> {
    wire.ok::<ListEventSources<InMemoryEngine>>(
        case,
        &ListEventSourcesInput {
            event_bus_arn: Some(bus.to_string()),
            name_prefix: None,
            next_token: None,
            max_results: PAGE_SIZE,
        },
    )
    .await
    .event_sources
    .unwrap_or_default()
    .into_iter()
    .filter_map(|summary| summary.event_source_arn)
    .collect()
}

/// The RULE on a source this store MINTED: the full lifecycle the client runs —
/// create, read, enumerate, delete — ends with every read of that ARN refused as
/// the EventSource it was, worded with the ARN the request carried.
#[tokio::test]
async fn a_deleted_event_source_is_refused_naming_the_arn_the_request_supplied() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "not-found-wording-bus").await;

    for (case, name, description, configuration) in [
        (
            "an aws.service source with a plain name",
            "lifecycle-one",
            None,
            aws_service_configuration("aws.ec2"),
        ),
        (
            "an aws.service source with a punctuated name and a description",
            "life.cycle_two-2",
            Some("forwards the object events"),
            aws_service_configuration("aws.s3"),
        ),
        (
            "an aws.service source whose name is a single character",
            "x",
            None,
            aws_service_configuration("aws.lambda"),
        ),
        (
            "an aws.partner source",
            "lifecycle-partner",
            None,
            partner_configuration(),
        ),
    ] {
        let arn = wire
            .ok::<CreateEventSource<InMemoryEngine>>(
                case,
                &create_input(
                    name,
                    &bus,
                    &format!("token-{name}"),
                    description,
                    configuration,
                ),
            )
            .await
            .event_source_arn
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateEventSource must report an EventSourceArn")
            });

        // While it is there it reads back and is enumerated, so what follows is
        // about the DELETE and not about a source that was never stored.
        let read = wire
            .ok::<DescribeEventSource<InMemoryEngine>>(
                case,
                &DescribeEventSourceInput {
                    event_source_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            (
                read.event_source_arn.as_deref(),
                read.name.as_deref(),
                read.event_bus_arn.as_deref(),
                read.state.as_deref(),
                read.revoked
            ),
            (
                Some(arn.as_str()),
                Some(name),
                Some(bus.as_str()),
                Some("ACTIVE"),
                None
            ),
            "case {case:?}: the created source must read back at its own ARN as an ACTIVE, \
             unrevoked source of its bus, got {read:?}"
        );
        let listed = listed_arns(&wire, case, &bus).await;
        assert_eq!(
            listed,
            vec![arn.clone()],
            "case {case:?}: the bus must enumerate exactly the source it holds, got {listed:?}"
        );

        wire.ok::<DeleteEventSource<InMemoryEngine>>(
            case,
            &DeleteEventSourceInput {
                event_source_arn: arn.clone(),
            },
        )
        .await;

        let (status, body) = describe(&wire, &arn).await;
        assert_not_found(
            case,
            status,
            &body,
            &format!("{EVENT_SOURCE_NOT_FOUND_PREFIX}{arn}"),
        );
        let listed = listed_arns(&wire, case, &bus).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: the deleted source must no longer be enumerated, got {listed:?}"
        );
    }
}

/// The RULE's variable part comes from the REQUEST, not from the store: once the
/// deleted source's name has been taken by a new source, a read of the retired
/// ARN still names that retired ARN, while the new one resolves.
#[tokio::test]
async fn the_refusal_names_the_retired_arn_even_once_its_name_is_taken_again() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "reused-name-bus").await;
    let case = "a name taken again after its source was deleted";
    let name = "reused-source-name";

    let retired = wire
        .ok::<CreateEventSource<InMemoryEngine>>(
            case,
            &create_input(
                name,
                &bus,
                "token-first",
                None,
                aws_service_configuration("aws.ec2"),
            ),
        )
        .await
        .event_source_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource must report an EventSourceArn"));

    wire.ok::<DeleteEventSource<InMemoryEngine>>(
        case,
        &DeleteEventSourceInput {
            event_source_arn: retired.clone(),
        },
    )
    .await;

    let live = wire
        .ok::<CreateEventSource<InMemoryEngine>>(
            case,
            &create_input(
                name,
                &bus,
                "token-second",
                None,
                aws_service_configuration("aws.ec2"),
            ),
        )
        .await
        .event_source_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource must report an EventSourceArn"));
    assert_ne!(
        live, retired,
        "case {case:?}: a source minted after the delete must carry an ARN of its own, got {live:?}"
    );

    let (status, body) = describe(&wire, &retired).await;
    assert_not_found(
        case,
        status,
        &body,
        &format!("{EVENT_SOURCE_NOT_FOUND_PREFIX}{retired}"),
    );

    let read = wire
        .ok::<DescribeEventSource<InMemoryEngine>>(
            case,
            &DescribeEventSourceInput {
                event_source_arn: live.clone(),
            },
        )
        .await;
    assert_eq!(
        read.event_source_arn.as_deref(),
        Some(live.as_str()),
        "case {case:?}: the source that took the name must still read back, got {read:?}"
    );
}

/// The RULE on an ARN this store never minted: the refusal echoes the submitted
/// `EventSourceArn` character for character, whatever its partition, region,
/// origin type and name segment are.
#[tokio::test]
async fn an_unminted_event_source_arn_is_echoed_into_the_refusal_as_submitted() {
    let wire = Wire::new();

    let longest_name = format!("n{}", "a".repeat(ARN_NAME_MAX - 1));
    for (case, arn) in [
        (
            "an aws.service ARN of the signed region",
            unminted_source_arn(
                "aws",
                REGION,
                "aws.service",
                "absent-service-source",
                "aaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
        ),
        (
            "an aws.partner ARN of the signed region",
            unminted_source_arn(
                "aws",
                REGION,
                "aws.partner",
                "absent-partner-source",
                "bbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ),
        (
            "an ARN of another region",
            unminted_source_arn(
                "aws",
                "eu-west-3",
                "aws.service",
                "absent-elsewhere",
                "ccccccccccccccccccccccccc",
            ),
        ),
        (
            "an ARN of another partition",
            unminted_source_arn(
                "aws-us-gov",
                "us-gov-west-1",
                "aws.service",
                "absent-in-gov",
                "ddddddddddddddddddddddddd",
            ),
        ),
        (
            "an ARN carrying the longest name segment the pattern admits",
            unminted_source_arn(
                "aws",
                REGION,
                "aws.service",
                &longest_name,
                "eeeeeeeeeeeeeeeeeeeeeeeee",
            ),
        ),
    ] {
        let (status, body) = describe(&wire, &arn).await;
        assert_not_found(
            case,
            status,
            &body,
            &format!("{EVENT_SOURCE_NOT_FOUND_PREFIX}{arn}"),
        );
    }
}
