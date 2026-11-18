// SPDX-License-Identifier: CC0-1.0

//! How the MSK configuration family refuses an `Arn` it resolves to no
//! configuration, driven end to end over the wire through the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`).
//!
//! The RULE this suite asserts, over a table rather than one recorded request:
//! every operation that names a configuration by `Arn` refuses an `Arn` of ITS OWN
//! region that resolves to no configuration with HTTP 400
//! `BadRequestException`, naming `Arn` as the parameter at fault and carrying
//! the service's ONE fixed sentence for that condition — a sentence with no
//! variable parts, so no value from the request or the endpoint appears in it.
//! The rows span both dimensions the rule generalises over: which operation
//! answers (the two readers, the revision listing, the update, the delete) and
//! what makes the `Arn` resolve to nothing (never minted, deleted, another
//! account, another service namespace, another resource kind, or not an ARN at
//! all).
//!
//! The REGION segment is a gate of its own, ahead of that lookup, and the second
//! suite below asserts it: an ARN names a resource in one Region and a regional
//! endpoint serves only its own Region's resources, so a well-formed configuration
//! ARN of ANOTHER Region names a resource this endpoint does not hold — HTTP 404
//! `NotFoundException` — while a region segment that is no Region at all is an
//! input the caller mistyped — HTTP 400 `BadRequestException` with the generic
//! parameter sentence. Three answers, three gates, and the one this endpoint's own
//! region reaches is the lookup above.

use serde_json::{json, Value};

// The router harness, the signing region, the account id, the URI encoder and
// the configuration family's shared refusal assertion every suite of this crate
// shares — the assertion lives there so this suite and the deletion suite cannot
// drift apart on what that refusal is.
use super::kafka_harness::{
    assert_configuration_arn_resolves_to_nothing, assert_configuration_state_refusal, pct,
    reported_server_properties, sleep_past_configuration_deletion, ConfigurationRequest, Emulator,
    Reply, ACCOUNT, REGION,
};

/// Every operation that takes a configuration `Arn`, as
/// `(name, method, uri suffix, body)` — the whole family the rule covers, each
/// bound as the model binds it under `/v1/configurations/{Arn}`.
fn operations() -> Vec<(&'static str, &'static str, &'static str, Option<Value>)> {
    vec![
        ("DescribeConfiguration", "GET", "", None),
        ("ListConfigurationRevisions", "GET", "/revisions", None),
        ("DescribeConfigurationRevision", "GET", "/revisions/1", None),
        // The same reader with a revision number that is illegal in itself: the
        // configuration is resolved first, so an `Arn` that names nothing is
        // what the refusal reports even when another member is wrong too.
        (
            "DescribeConfigurationRevision/illegal-revision",
            "GET",
            "/revisions/0",
            None,
        ),
        (
            "UpdateConfiguration",
            "PUT",
            "",
            Some(json!({
                "serverProperties": ::aws_smithy_types::base64::encode(b"num.partitions=7\n"),
            })),
        ),
        ("DeleteConfiguration", "DELETE", "", None),
    ]
}

/// Call `operation` on `arn` at `region`'s endpoint.
async fn call(
    emulator: &Emulator,
    region: &str,
    operation: &(&str, &str, &str, Option<Value>),
    arn: &str,
) -> Reply {
    let (_, method, suffix, body) = operation;
    emulator
        .call_in(
            region,
            method,
            &format!("/v1/configurations/{}{suffix}", pct(arn)),
            body.clone(),
        )
        .await
}

/// RULE: every operation of the configuration family refuses an `Arn` that
/// resolves to no configuration of the endpoint's region with HTTP 400
/// `BadRequestException` and the service's one fixed sentence for that
/// condition, naming `Arn` as the parameter at fault — however the `Arn` came to
/// resolve to nothing, and whichever operation answers.
#[tokio::test]
async fn every_configuration_operation_refuses_an_unresolvable_arn_alike() {
    let emulator = Emulator::new();

    // One configuration this endpoint holds (the control) and one this endpoint
    // held until a delete took it. A configuration of ANOTHER region is not a row
    // here: it is the region gate's, which the suite below asserts.
    let held = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("held-here"))
        .await;
    let deleted = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("to-delete"))
        .await;
    let gone = emulator
        .call_in(
            REGION,
            "DELETE",
            &format!("/v1/configurations/{}", pct(&deleted)),
            None,
        )
        .await;
    assert_eq!(
        gone.status, 200,
        "expected the delete that sets the already-deleted row up to succeed, got {} {}",
        gone.status, gone.body
    );
    // The delete is asynchronous, so the configuration resolves until the clock
    // owes its removal: waited out here, because the row below is about an ARN
    // that resolves to NOTHING, not one whose delete is still in flight.
    sleep_past_configuration_deletion();

    // Each way an `Arn` can resolve to no configuration of this endpoint.
    let rows: Vec<(&str, String)> = vec![
        (
            "never-minted",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/never-minted/\
                 11111111-2222-4333-8444-555555555555-1"
            ),
        ),
        ("already-deleted", deleted.clone()),
        ("another-account", held.replace(ACCOUNT, "123456789012")),
        ("another-service-namespace", held.replace(":kafka:", ":kafka-cluster:")),
        (
            "another-resource-kind",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a-cluster/\
                 11111111-2222-4333-8444-555555555555-2"
            ),
        ),
        ("not-an-arn", "not-an-arn".to_string()),
        ("truncated-arn", "arn:aws:kafka".to_string()),
    ];

    for (case, arn) in &rows {
        for operation in operations() {
            let reply = call(&emulator, REGION, &operation, arn).await;
            assert_configuration_arn_resolves_to_nothing(
                &reply,
                &format!("{case}/{}", operation.0),
                arn,
                REGION,
            );
        }
    }

    // Nothing above touched the configuration this endpoint does hold: it is
    // still readable and still on its first revision.
    let described = emulator.describe_configuration(&held).await;
    assert_eq!(
        (described.status, described.at("/latestRevision/revision").as_i64()),
        (200, Some(1)),
        "case control: the held configuration must be unchanged, got {} {}",
        described.status,
        described.body
    );
}

/// What one operation of the family must answer against a configuration whose
/// delete is in flight: a call that CHANGES the configuration is refused on the
/// state it is in, a reader that resolves a reference it holds answers it, and a
/// reader naming a revision the configuration never issued is still the
/// unresolvable-reference refusal (the reference, not the state, is what fails).
enum InFlight {
    StateRefusal,
    Served,
    ReferenceRefusal,
}

/// RULE: the two refusals of the configuration family discriminate a
/// state-machine refusal from a lookup miss. While a delete is in flight the
/// configuration is still a resource this endpoint holds: every reader resolves
/// it `DELETING` and every call that would CHANGE it is refused on its state.
/// Only once the clock has completed the removal does the SAME call become the
/// unresolvable-reference refusal — and it answers that same terminal refusal
/// however many times it is polled, whichever operation polls it.
#[tokio::test]
async fn a_delete_in_flight_refuses_on_state_and_only_then_on_the_lookup() {
    let emulator = Emulator::new();
    let arn = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("polled"))
        .await;

    // Readable before the delete, so what follows is a change the delete made
    // rather than a configuration that was never there.
    let before = emulator.describe_configuration(&arn).await;
    assert_eq!(
        before.status, 200,
        "case before-delete: the configuration must be readable, got {} {}",
        before.status, before.body
    );

    let deleted = emulator
        .call_in(
            REGION,
            "DELETE",
            &format!("/v1/configurations/{}", pct(&arn)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case delete: expected DeleteConfiguration to succeed, got {} {}",
        deleted.status, deleted.body
    );

    // A poll loop drives every operation of the family again and again while the
    // delete is in flight: the readers resolve it, and the calls that would
    // change it are refused on the state it is in — never reported as an ARN
    // that names nothing.
    for attempt in 1..=3 {
        for operation in operations() {
            let expected = match operation.0 {
                "UpdateConfiguration" | "DeleteConfiguration" => InFlight::StateRefusal,
                "DescribeConfigurationRevision/illegal-revision" => InFlight::ReferenceRefusal,
                _ => InFlight::Served,
            };
            let case = format!("in-flight attempt {attempt}/{}", operation.0);
            let reply = call(&emulator, REGION, &operation, &arn).await;
            match expected {
                InFlight::StateRefusal => assert_configuration_state_refusal(&reply, &case),
                InFlight::ReferenceRefusal => {
                    assert_configuration_arn_resolves_to_nothing(&reply, &case, &arn, REGION)
                }
                InFlight::Served => assert_eq!(
                    reply.status, 200,
                    "case {case}: a reader must still resolve a configuration whose delete \
                     is in flight, got {} {}",
                    reply.status, reply.body
                ),
            }
        }
    }

    sleep_past_configuration_deletion();

    // The removal has come due, so the same calls are now the one terminal
    // refusal — every attempt, from every operation, and no attempt changes the
    // answer.
    for attempt in 1..=3 {
        for operation in operations() {
            let reply = call(&emulator, REGION, &operation, &arn).await;
            assert_configuration_arn_resolves_to_nothing(
                &reply,
                &format!("poll attempt {attempt}/{}", operation.0),
                &arn,
                REGION,
            );
        }
    }
}

/// What this endpoint answers a configuration ARN of ANOTHER Region: the fixed
/// not-found sentence of the service, with the U+2019 apostrophe AWS writes it
/// with. Spelled out here rather than read from the service's own constants, so
/// this suite pins the wording instead of the code under test.
const RESOURCE_DOES_NOT_EXIST: &str = "The requested resource doesn\u{2019}t exist.";

/// And what it answers a region segment that is no Region at all: the generic
/// parameter sentence, likewise spelled out.
const PARAMETERS_NOT_VALID: &str = "One or more of the parameters are not valid.";

/// Other Regions AWS publishes, as the region segment of an ARN this endpoint is
/// handed. The rows differ along what the rule generalises over: a Region of this
/// endpoint's own partition and of another, one whose code has more hyphenated
/// words than this endpoint's, and one the issue never cited.
///
/// SOURCE: the published Region list
/// (`service_com_amazonaws_kafka_kafka::state::configuration_arn::AWS_REGIONS`,
/// which re-exports the tree's one catalog), of which these are entries.
const OTHER_REGIONS: &[&str] = &[
    "us-east-2",
    "eu-central-1",
    "ap-southeast-4",
    "us-gov-west-1",
];

/// Region segments that name no Region at all: Region-SHAPED text AWS publishes no
/// Region for, a number past the Regions a prefix has, and text that is not even
/// shaped like one.
const UNREADABLE_REGIONS: &[&str] = &["xx-fake-1", "us-west-9", "not-a-region"];

/// RULE: the REGION segment of a configuration `Arn` is part of resolving it, so
/// the whole configuration family answers an ARN of another Region HTTP 404
/// `NotFoundException` with the service's fixed not-found sentence — on the reads,
/// on the revision reads and on the writes alike — rather than resolving it to this
/// endpoint's own configuration or reporting it as a malformed reference.
#[tokio::test]
async fn a_configuration_arn_of_another_region_is_not_found_here() {
    let emulator = Emulator::new();

    let held = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("arnscope"))
        .await;

    for elsewhere in OTHER_REGIONS {
        // Exactly one segment changed: everything else about the ARN is the one
        // this endpoint minted, so the only reason it names nothing here is the
        // Region it belongs to.
        let carried = held.replace(REGION, elsewhere);
        for operation in operations() {
            let case = format!("{elsewhere}/{}", operation.0);
            let reply = call(&emulator, REGION, &operation, &carried).await;
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (404, "NotFoundException", RESOURCE_DOES_NOT_EXIST),
                "case {case}: expected the not-found refusal, got {} {}",
                reply.status,
                reply.body
            );
        }
    }

    // The configuration this endpoint holds is untouched by every call above: it
    // is still ACTIVE, still on revision 1, and that revision still carries the
    // body it was created with.
    let described = emulator.describe_configuration(&held).await;
    assert_eq!(
        (
            described.status,
            described.at("/state").as_str(),
            described.at("/latestRevision/revision").as_i64()
        ),
        (200, Some("ACTIVE"), Some(1)),
        "case control: the held configuration must be unchanged, got {} {}",
        described.status,
        described.body
    );
    let revision = emulator.describe_configuration_revision(&held, 1).await;
    let stored = reported_server_properties(&revision, "control");
    let stored = String::from_utf8(stored).expect("the stored body is the text it was created with");
    assert!(
        !stored.contains("num.partitions"),
        "case control: a refused cross-region update must append nothing, got {stored:?}"
    );

    // And at the endpoint of the Region the ARN belongs to, the SAME value is that
    // endpoint's own reference to resolve: it holds no configuration under it, so
    // the answer there is the family's own-region lookup refusal rather than this
    // endpoint's not-found. The two answers are what make the region part of the
    // identity rather than part of the syntax.
    let carried = held.replace(REGION, OTHER_REGIONS[0]);
    let there = emulator
        .call_in(
            OTHER_REGIONS[0],
            "GET",
            &format!("/v1/configurations/{}", pct(&carried)),
            None,
        )
        .await;
    assert_configuration_arn_resolves_to_nothing(
        &there,
        "the other region's own endpoint",
        &carried,
        OTHER_REGIONS[0],
    );
    let minted_there = emulator
        .create_configuration_row_ok_in(
            OTHER_REGIONS[0],
            &ConfigurationRequest::named("arnscope-there"),
        )
        .await;
    let served = emulator
        .call_in(
            OTHER_REGIONS[0],
            "GET",
            &format!("/v1/configurations/{}", pct(&minted_there)),
            None,
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the other region's own configuration\": it must be served there, got {} {}",
        served.status, served.body
    );
}

/// RULE (the earlier gate): a region segment that is no Region at all is an input
/// the caller can only have mistyped, so the family answers HTTP 400
/// `BadRequestException` with the generic parameter sentence, naming `arn` — a
/// different answer from the 404 an ARN of a real other Region gets.
#[tokio::test]
async fn a_region_segment_that_names_no_region_is_a_parameter_refusal() {
    let emulator = Emulator::new();

    let held = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("arnscope-bad"))
        .await;

    for unreadable in UNREADABLE_REGIONS {
        let carried = held.replace(REGION, unreadable);
        for operation in operations() {
            let case = format!("{unreadable}/{}", operation.0);
            let reply = call(&emulator, REGION, &operation, &carried).await;
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (400, "BadRequestException", PARAMETERS_NOT_VALID),
                "case {case}: expected the parameter refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.at("/invalidParameter").as_str(),
                Some("arn"),
                "case {case}: expected the refusal to name arn, got {}",
                reply.body
            );
            assert!(
                !reply.error_message().contains(unreadable),
                "case {case}: the refusal must not echo the submitted value, got {:?}",
                reply.error_message()
            );
        }
    }

    // The control: this endpoint's own ARN still resolves, so the refusals above
    // rest on the region segment alone.
    let described = emulator.describe_configuration(&held).await;
    assert_eq!(
        described.status, 200,
        "case control: the held configuration must still be served, got {} {}",
        described.status, described.body
    );
}
