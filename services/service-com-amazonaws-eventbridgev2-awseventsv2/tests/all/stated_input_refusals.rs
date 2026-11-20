// SPDX-License-Identifier: CC0-1.0

//! What this service states about a request it will not serve, read where a client reads
//! it: off the `rpcv2Cbor` wire.
//!
//! RULE 1 — a refusal states ONE CLAUSE PER VIOLATION. Every `@required` member a request
//! omits is a clause of AWS's constraint envelope, wherever that member sits — at the input
//! shape, inside a structure member, or inside an element of a list — not only the first one
//! the read reached. An absent STRUCTURE member is one clause naming that member, never one
//! per member nested under it.
//!
//! RULE 2 — a union-typed member states exactly one variant
//! (`EventSourceConfiguration`'s own documentation: "Discriminated EventSource
//! configuration. Exactly one variant must be set."), so a request that sets neither or
//! both is refused `400` with the modelled `InvalidInputException` and the arity rule as its
//! whole message.
//!
//! RULE 3 — an EventSource's `Pattern` is "A filter pattern, as a JSON string", and for the
//! AWS-service variant "Do not include source, account, or region as top-level fields"
//! (`smithy_query(query="doc", shape="AwsServiceEventsSourceConfiguration",
//! member="Pattern")`). A value outside either rule is refused `400` with
//! `InvalidInputException`; the partner variant's own `Pattern` documents no field
//! restriction, so it keeps whichever fields it names.
//!
//! RULE 4 — each refusal CLASS has its own words, which name the resource or the member the
//! refusal is about and nothing a caller supplied beyond that. The table below drives one
//! row per class this service answers, each read as a whole `Message`.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus to REPORT `ACTIVE`,
//! polled under a generous deadline by the shared harness.

use aws_smithy_cbor::Encoder;
use engine::InMemoryEngine;
use smithy_server_runtime::Operation;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::get_resource_policy::GetResourcePolicy;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::put_resource_policy::PutResourcePolicy;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::update_event_source::UpdateEventSource;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventSourceInput, DescribeEventSourceInput,
    EventSourceConfiguration, GetResourcePolicyInput, PartnerEventsSourceConfiguration,
    PutResourcePolicyInput, RevokeResourceInput, UpdateEventSourceInput,
};

use crate::modelled_input_refusal::{
    absent_clause, constraint_envelope, encoded, refusal, rendered, Wire,
    INVALID_INPUT_EXCEPTION,
};

/// The region the shared harness signs every request for.
const REGION: &str = crate::modelled_input_refusal::REGION;

/// The account the emulator mints ARNs under.
const ACCOUNT: &str = "000000000000";

/// The error shapes the rows below expect, as a refusal's `__type` carries them.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const PUBLIC_POLICY_EXCEPTION: &str = "PublicPolicyException";
const INVALID_STATE_EXCEPTION: &str = "InvalidStateException";

// ---------------------------------------------------------------------------
// Hand-built `rpcv2Cbor` bodies
// ---------------------------------------------------------------------------

/// One item of a hand-built request body.
///
/// A body is built by hand where the generated input TYPE cannot express the request under
/// test: a structure member present but empty, or a union carrying two variants, neither of
/// which a Rust enum or a non-`Option` field can hold.
enum Item {
    Text(String),
    Map(Vec<(&'static str, Item)>),
    List(Vec<Item>),
}

/// `item` written as the CBOR it stands for.
fn write(encoder: &mut Encoder, item: &Item) {
    match item {
        Item::Text(text) => {
            encoder.str(text);
        }
        Item::Map(entries) => {
            encoder.begin_map();
            for (name, value) in entries {
                encoder.str(name);
                write(encoder, value);
            }
            encoder.end();
        }
        Item::List(elements) => {
            encoder.begin_array();
            for element in elements {
                write(encoder, element);
            }
            encoder.end();
        }
    }
}

/// `members` as the body of a request: a CBOR map keyed by the member names the model
/// declares, which is how `rpcv2Cbor` names a structure's members.
fn body(members: Vec<(&'static str, Item)>) -> Vec<u8> {
    let mut encoder = Encoder::new(Vec::new());
    write(&mut encoder, &Item::Map(members));
    encoder.into_writer()
}

/// A text member.
fn text(value: &str) -> Item {
    Item::Text(value.to_string())
}

/// A `PutEvents` body naming `event_bus_arn` and carrying one entry per element of
/// `entries`, each stating only the members its element lists.
fn put_events_body(event_bus_arn: &str, entries: Vec<Vec<(&'static str, Item)>>) -> Vec<u8> {
    body(vec![
        ("EventBusArn", text(event_bus_arn)),
        (
            "Entries",
            Item::List(entries.into_iter().map(Item::Map).collect()),
        ),
    ])
}

// ---------------------------------------------------------------------------
// The constraint envelope
// ---------------------------------------------------------------------------

/// The whole `Message` the envelope carries for the members at `paths`, all absent.
///
/// The clause and the envelope are the sibling suite's
/// ([`crate::modelled_input_refusal`]), so the two cannot come to state AWS's envelope
/// differently.
fn absences(paths: &[&str]) -> String {
    let clauses: Vec<String> = paths.iter().map(|path| absent_clause(path)).collect();
    constraint_envelope(&clauses)
}

/// The whole `Message` of a refusal of a union-typed member set other than exactly once.
fn one_variant_required(member: &str) -> String {
    format!("Exactly one {member} sub-field must be provided")
}

// ---------------------------------------------------------------------------
// Configurations and sources
// ---------------------------------------------------------------------------

/// An `AwsServiceEventsConfiguration` forwarding `aws_service`'s events under `pattern`.
fn aws_service_configuration(aws_service: &str, pattern: Option<&str>) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: aws_service.to_string(),
        pattern: pattern.map(str::to_string),
        on_failure_configuration: None,
    })
}

/// A `PartnerEventsConfiguration` under `pattern`.
fn partner_configuration(pattern: Option<&str>) -> EventSourceConfiguration {
    EventSourceConfiguration::PartnerEventsConfiguration(PartnerEventsSourceConfiguration {
        partner_event_source_arn: format!(
            "arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/vendor/stream"
        ),
        pattern: pattern.map(str::to_string),
        partner_bus_kms_key_identifier: None,
        on_failure_configuration: None,
    })
}

/// A `CreateEventSourceInput` on `event_bus_arn` carrying `configuration`.
fn create_source(
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

/// An EventSource on `bus`, created with `configuration`, answering its ARN.
async fn source_on(
    wire: &Wire,
    case: &str,
    name: &str,
    bus: &str,
    configuration: EventSourceConfiguration,
) -> String {
    wire.ok::<CreateEventSource<InMemoryEngine>>(case, &create_source(name, bus, configuration))
        .await
        .event_source_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource must report an ARN"))
}

// ---------------------------------------------------------------------------
// RULE 1: one clause per omitted member
// ---------------------------------------------------------------------------

/// RULE 1 over the positions a required member can sit at, and over how many of them one
/// request can omit: the input shape itself, a structure member, one element of a list, and
/// several elements of one list.
#[tokio::test]
async fn every_required_member_a_request_omits_is_one_clause_of_the_envelope() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "absence-bus").await;

    // (case, operation, the request body, the whole Message expected)
    let rows: Vec<(&str, &str, Vec<u8>, String)> = vec![
        (
            // The recorded class: a batch whose entries each break a different
            // requirement. One clause for each, so the member path says WHICH entry.
            "two entries of a batch each omitting a different member",
            PutEvents::<InMemoryEngine>::NAME,
            put_events_body(
                &bus,
                vec![
                    vec![("DetailType", text("Demo"))],
                    vec![("Source", text("demo.app"))],
                    vec![("Source", text("demo.app")), ("DetailType", text("Demo"))],
                ],
            ),
            absences(&["entries.1.member.source", "entries.2.member.detailType"]),
        ),
        (
            "one entry omitting both of its required members",
            PutEvents::<InMemoryEngine>::NAME,
            put_events_body(&bus, vec![vec![("Detail", text("{}"))]]),
            absences(&["entries.1.member.source", "entries.1.member.detailType"]),
        ),
        (
            "two entries omitting both of their required members",
            PutEvents::<InMemoryEngine>::NAME,
            put_events_body(&bus, vec![vec![], vec![]]),
            absences(&[
                "entries.1.member.source",
                "entries.1.member.detailType",
                "entries.2.member.source",
                "entries.2.member.detailType",
            ]),
        ),
        (
            // The single-absence case is unchanged: one violation is one clause.
            "a batch of well-formed entries addressed at nothing",
            PutEvents::<InMemoryEngine>::NAME,
            body(vec![(
                "Entries",
                Item::List(vec![Item::Map(vec![
                    ("Source", text("demo.app")),
                    ("DetailType", text("Demo")),
                ])]),
            )]),
            absences(&["eventBusArn"]),
        ),
        (
            "a structure member present but stating neither of its required members",
            CreateSubscriber::<InMemoryEngine>::NAME,
            body(vec![
                ("Name", text("absence-subscriber")),
                ("EventBusArn", text(&bus)),
                ("InvokeConfiguration", Item::Map(vec![])),
                ("ClientToken", text("absence-token")),
            ]),
            absences(&[
                "invokeConfiguration.roleArn",
                "invokeConfiguration.targetArn",
            ]),
        ),
        (
            // An absent STRUCTURE member is ONE clause: the members nested under it are
            // not reported, because the request never opened it.
            "every required member of the input shape omitted",
            CreateSubscriber::<InMemoryEngine>::NAME,
            body(vec![("ClientToken", text("absence-token"))]),
            absences(&["name", "eventBusArn", "invokeConfiguration"]),
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, answered) = wire.send(operation, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must answer 400, got {status} with {:?}",
            rendered(&answered)
        );
        let (code, message) = refusal(case, &answered);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: {operation} must answer {INVALID_INPUT_EXCEPTION}, got {code:?} \
             with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: {operation} must state {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a union states exactly one variant
// ---------------------------------------------------------------------------

/// RULE 2 over the two ways a request can break the arity — no variant and both variants —
/// on both operations that take the union, with the request that states exactly one as the
/// other half of the rule.
#[tokio::test]
async fn a_configuration_union_states_exactly_one_variant() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "union-bus").await;
    let source = source_on(
        &wire,
        "the source under test",
        "union.source",
        &bus,
        aws_service_configuration("aws.s3", None),
    )
    .await;

    let both = || {
        Item::Map(vec![
            (
                "AwsServiceEventsConfiguration",
                Item::Map(vec![("AwsService", text("aws.s3"))]),
            ),
            (
                "PartnerEventsConfiguration",
                Item::Map(vec![(
                    "PartnerEventSourceArn",
                    text(&format!(
                        "arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/vendor/stream"
                    )),
                )]),
            ),
        ])
    };

    // (case, operation, the request body)
    let refused: Vec<(&str, &str, Vec<u8>)> = vec![
        (
            "an update whose configuration states no variant",
            UpdateEventSource::<InMemoryEngine>::NAME,
            body(vec![
                ("EventSourceArn", text(&source)),
                ("Configuration", Item::Map(vec![])),
            ]),
        ),
        (
            "an update whose configuration states both variants",
            UpdateEventSource::<InMemoryEngine>::NAME,
            body(vec![
                ("EventSourceArn", text(&source)),
                ("Configuration", both()),
            ]),
        ),
        (
            "a create whose configuration states no variant",
            CreateEventSource::<InMemoryEngine>::NAME,
            body(vec![
                ("Name", text("union.create")),
                ("EventBusArn", text(&bus)),
                ("ClientToken", text("union-create-token")),
                ("Configuration", Item::Map(vec![])),
            ]),
        ),
        (
            "a create whose configuration states both variants",
            CreateEventSource::<InMemoryEngine>::NAME,
            body(vec![
                ("Name", text("union.create.two")),
                ("EventBusArn", text(&bus)),
                ("ClientToken", text("union-create-token-two")),
                ("Configuration", both()),
            ]),
        ),
    ];

    let expected = one_variant_required("Configuration");
    for (case, operation, request) in refused {
        let (status, answered) = wire.send(operation, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must answer 400, got {status} with {:?}",
            rendered(&answered)
        );
        let (code, message) = refusal(case, &answered);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: {operation} must answer {INVALID_INPUT_EXCEPTION}, got {code:?} \
             with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: {operation} must state {expected:?}, got {message:?}"
        );
    }

    // The other half: exactly one variant is served, and a configuration omitted
    // altogether has no arity to break.
    for (case, input) in [
        (
            "an update stating exactly one variant",
            UpdateEventSourceInput {
                event_source_arn: source.clone(),
                configuration: Some(aws_service_configuration("aws.s3", None)),
                ..Default::default()
            },
        ),
        (
            "an update stating no configuration at all",
            UpdateEventSourceInput {
                event_source_arn: source.clone(),
                description: Some("no configuration here".to_string()),
                ..Default::default()
            },
        ),
    ] {
        let (status, answered) = wire.call::<UpdateEventSource<InMemoryEngine>>(&input).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: UpdateEventSource must serve it, got {status} with {:?}",
            rendered(&answered)
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 3: the pattern an EventSource filters on
// ---------------------------------------------------------------------------

/// RULE 3 over each field the forwarding rule sets itself, over a value that is not JSON at
/// all, and over the patterns both rules admit — on the create and on the update, so the
/// rule is the member's and not one operation's.
#[tokio::test]
async fn an_event_source_pattern_is_json_and_leaves_the_forwarded_fields_alone() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "pattern-bus").await;

    // The fields the member's own documentation names, each tested on its own row —
    // including `account` and `region`, which no recorded case cites.
    // Source: `smithy_query(query="doc", shape="AwsServiceEventsSourceConfiguration",
    // member="Pattern")`: "Do not include source, account, or region as top-level fields".
    let forwarded: [(&str, &str); 3] = [
        ("source", r#"{"source":["aws.s3"]}"#),
        ("account", r#"{"account":["000000000000"]}"#),
        ("region", r#"{"region":["us-west-2"]}"#),
    ];

    let mut rows: Vec<(String, Option<String>, String)> = Vec::new();
    for (field, pattern) in forwarded {
        rows.push((
            format!("an aws-service pattern setting {field:?}"),
            Some(pattern.to_string()),
            format!(
                "Pattern must not set the key '{field}'; the forwarding rule sets it from the \
                 requested AwsService and the source account and region"
            ),
        ));
    }
    rows.push((
        "an aws-service pattern setting a forwarded field beside another".to_string(),
        Some(r#"{"detail-type":["Object Created"],"region":["us-west-2"]}"#.to_string()),
        "Pattern must not set the key 'region'; the forwarding rule sets it from the requested \
         AwsService and the source account and region"
            .to_string(),
    ));
    for (case, pattern) in [
        ("a pattern that is not JSON", "not json at all"),
        ("a pattern that is a bare word", "detail-type"),
        ("a pattern whose JSON is cut short", r#"{"detail-type":["#),
    ] {
        rows.push((
            case.to_string(),
            Some(pattern.to_string()),
            "Pattern is not valid JSON".to_string(),
        ));
    }
    // The other half of each rule: a pattern naming none of the forwarded fields, a
    // configuration carrying no pattern, and the empty pattern the member's own `@length`
    // minimum of 0 admits, are all served.
    for (case, pattern) in [
        (
            "a pattern naming none of the forwarded fields",
            Some(r#"{"detail-type":["Object Created"]}"#),
        ),
        ("no pattern at all", None),
        ("the empty pattern", Some("")),
    ] {
        rows.push((case.to_string(), pattern.map(str::to_string), String::new()));
    }

    for (index, (case, pattern, expected)) in rows.iter().enumerate() {
        let name = format!("pattern.source{index}");
        let input = create_source(
            &name,
            &bus,
            aws_service_configuration("aws.s3", pattern.as_deref()),
        );
        let (status, answered) = wire.call::<CreateEventSource<InMemoryEngine>>(&input).await;
        if expected.is_empty() {
            assert_eq!(
                status,
                200,
                "case {case:?}: CreateEventSource must serve it, got {status} with {:?}",
                rendered(&answered)
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: CreateEventSource must answer 400, got {status} with {:?}",
            rendered(&answered)
        );
        let (code, message) = refusal(case, &answered);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: expected {INVALID_INPUT_EXCEPTION}, got {code:?} with {message:?}"
        );
        assert_eq!(
            &message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The same rule on the UPDATE of a source that already exists, and the partner
    // variant, whose own `Pattern` documents no field restriction.
    let source = source_on(
        &wire,
        "the source under test",
        "pattern.updatable",
        &bus,
        aws_service_configuration("aws.s3", None),
    )
    .await;
    let (status, answered) = wire
        .call::<UpdateEventSource<InMemoryEngine>>(&UpdateEventSourceInput {
            event_source_arn: source.clone(),
            configuration: Some(aws_service_configuration(
                "aws.s3",
                Some(r#"{"account":["000000000000"]}"#),
            )),
            ..Default::default()
        })
        .await;
    assert_eq!(
        status,
        400,
        "case \"an update setting a forwarded field\": expected 400, got {status} with {:?}",
        rendered(&answered)
    );
    let (_, message) = refusal("an update setting a forwarded field", &answered);
    assert_eq!(
        message,
        "Pattern must not set the key 'account'; the forwarding rule sets it from the requested \
         AwsService and the source account and region",
        "case \"an update setting a forwarded field\": the update states the member's rule"
    );

    let (status, answered) = wire
        .call::<CreateEventSource<InMemoryEngine>>(&create_source(
            "pattern.partner",
            &bus,
            partner_configuration(Some(r#"{"source":["aws.partner/vendor/stream"]}"#)),
        ))
        .await;
    assert_eq!(
        status,
        200,
        "case \"a partner pattern naming source\": the partner variant states no such \
         restriction, got {status} with {:?}",
        rendered(&answered)
    );
}

// ---------------------------------------------------------------------------
// RULE 4: each refusal class states its own words
// ---------------------------------------------------------------------------

/// RULE 4 over the classes this service refuses with, one row each: the whole `Message`,
/// the error shape and the status.
#[tokio::test]
async fn each_refusal_class_states_its_own_words() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "wording-bus").await;
    let source = source_on(
        &wire,
        "the source under test",
        "wording.source",
        &bus,
        aws_service_configuration("aws.s3", None),
    )
    .await;

    // A source this service minted and then revoked, which takes no further change.
    let revoked = source_on(
        &wire,
        "the revoked source",
        "wording.revoked",
        &bus,
        aws_service_configuration("aws.ec2", None),
    )
    .await;
    let (status, answered) = wire
        .call::<RevokeResource<InMemoryEngine>>(&RevokeResourceInput {
            arn: revoked.clone(),
        })
        .await;
    assert_eq!(
        status,
        200,
        "the revoke must succeed, got {status} with {:?}",
        rendered(&answered)
    );

    // An EventSource ARN of the modelled form that this service never minted.
    let unminted = format!(
        "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.service/absent.source/\
         abcdefghijklmnopqrstuvwxy"
    );

    // (case, operation, body, status, error shape, the whole Message)
    let rows: Vec<Row> = vec![
        (
            "a read of a policy the bus does not hold",
            GetResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<GetResourcePolicy<InMemoryEngine>>(&GetResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: None,
            }),
            404,
            RESOURCE_NOT_FOUND_EXCEPTION,
            "No 'default' resource policy is attached to this event bus".to_string(),
        ),
        (
            // The policy NAME is the one variable part, so the row that names the other
            // modelled policy reads it back.
            "a read of the other named policy the bus does not hold",
            GetResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<GetResourcePolicy<InMemoryEngine>>(&GetResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: Some("AWS_RAM".to_string()),
            }),
            404,
            RESOURCE_NOT_FOUND_EXCEPTION,
            "No 'AWS_RAM' resource policy is attached to this event bus".to_string(),
        ),
        (
            "a write naming a policy this service does not attach",
            PutResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<PutResourcePolicy<InMemoryEngine>>(&PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: Some("extra".to_string()),
                policy_document: allowing_account_policy(),
                expected_revision_id: None,
            }),
            400,
            INVALID_INPUT_EXCEPTION,
            "PolicyName 'extra' is not a valid resource policy name. Valid values are \
             'default' and 'AWS_RAM'."
                .to_string(),
        ),
        (
            "a write of the policy the other party owns",
            PutResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<PutResourcePolicy<InMemoryEngine>>(&PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: Some("AWS_RAM".to_string()),
                policy_document: allowing_account_policy(),
                expected_revision_id: None,
            }),
            403,
            ACCESS_DENIED_EXCEPTION,
            "Only AWS Resource Access Manager can modify the AWS_RAM resource policy".to_string(),
        ),
        (
            "a write whose document is not JSON",
            PutResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<PutResourcePolicy<InMemoryEngine>>(&PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: None,
                policy_document: "not a policy".to_string(),
                expected_revision_id: None,
            }),
            400,
            INVALID_INPUT_EXCEPTION,
            "The resource policy is invalid".to_string(),
        ),
        (
            "a write whose document states no statement",
            PutResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<PutResourcePolicy<InMemoryEngine>>(&PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: None,
                policy_document: r#"{"Version":"2012-10-17","Statement":[]}"#.to_string(),
                expected_revision_id: None,
            }),
            400,
            INVALID_INPUT_EXCEPTION,
            "The resource policy is invalid".to_string(),
        ),
        (
            "a write that would open the bus to every caller",
            PutResourcePolicy::<InMemoryEngine>::NAME,
            encoded::<PutResourcePolicy<InMemoryEngine>>(&PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: None,
                policy_document: wildcard_principal_policy(),
                expected_revision_id: None,
            }),
            400,
            PUBLIC_POLICY_EXCEPTION,
            "The resource policy could grant public access to the event bus, so it was not \
             attached. Replace any wildcard principal with specific principals, or add a \
             condition that limits the callers to specific AWS accounts."
                .to_string(),
        ),
        (
            "a read of an event source this service never minted",
            DescribeEventSource::<InMemoryEngine>::NAME,
            encoded::<DescribeEventSource<InMemoryEngine>>(&DescribeEventSourceInput {
                event_source_arn: unminted.clone(),
            }),
            404,
            RESOURCE_NOT_FOUND_EXCEPTION,
            format!("EventSource not found: {unminted}"),
        ),
        (
            "an update of a revoked event source",
            UpdateEventSource::<InMemoryEngine>::NAME,
            encoded::<UpdateEventSource<InMemoryEngine>>(&UpdateEventSourceInput {
                event_source_arn: revoked.clone(),
                description: Some("a change it cannot take".to_string()),
                ..Default::default()
            }),
            409,
            INVALID_STATE_EXCEPTION,
            "EventSource is revoked and cannot be updated".to_string(),
        ),
        (
            "an update changing the configuration variant",
            UpdateEventSource::<InMemoryEngine>::NAME,
            encoded::<UpdateEventSource<InMemoryEngine>>(&UpdateEventSourceInput {
                event_source_arn: source.clone(),
                configuration: Some(partner_configuration(None)),
                ..Default::default()
            }),
            400,
            INVALID_INPUT_EXCEPTION,
            "EventSource type is immutable: the configuration variant must remain \
             AWS_SERVICE_EVENTS"
                .to_string(),
        ),
    ];

    for (case, operation, request, expected_status, expected_code, expected_message) in rows {
        let (status, answered) = wire.send(operation, request).await;
        assert_eq!(
            status,
            expected_status,
            "case {case:?}: {operation} must answer {expected_status}, got {status} with {:?}",
            rendered(&answered)
        );
        let (code, message) = refusal(case, &answered);
        assert_eq!(
            code, expected_code,
            "case {case:?}: {operation} must answer {expected_code}, got {code:?} with \
             {message:?}"
        );
        assert_eq!(
            message, expected_message,
            "case {case:?}: {operation} must state {expected_message:?}, got {message:?}"
        );
    }
}

/// One row of the refusal-class table: the case, the operation, the request body, and the
/// status, error shape and whole `Message` that class is answered with.
type Row = (
    &'static str,
    &'static str,
    Vec<u8>,
    u16,
    &'static str,
    String,
);

/// A resource policy granting one named account, which opens the bus to nobody else.
fn allowing_account_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"one","Effect":"Allow",
           "Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},
           "Action":"events:PutEvents","Resource":"*"}}]}}"#
    )
}

/// A resource policy whose principal is a wildcard and which no condition narrows, which is
/// what `PublicPolicyException` describes.
fn wildcard_principal_policy() -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Sid":"open","Effect":"Allow",
       "Principal":"*","Action":"events:PutEvents","Resource":"*"}]}"#
        .to_string()
}
