// SPDX-License-Identifier: CC0-1.0

//! An instance's feature toggles — `UpdateInstanceAttribute` and the
//! `DescribeInstanceAttribute` read that proves it — end to end over HTTP through the
//! same `MultiServiceRouter` the emulator serves with.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! * an update STORES the value it names for the (instance, attribute type) it names,
//!   and the describe reads that same value back, for every `InstanceAttributeType` the
//!   model declares and in both directions;
//! * a toggle nobody has set reports its DEFAULT, and reading one stores nothing: the
//!   read is repeatable and a later update still changes only the toggle it names;
//! * the `INBOUND_CALLS` and `OUTBOUND_CALLS` toggles ARE the instance's own call-
//!   direction flags, so setting one is what `DescribeInstance` then reports;
//! * an `InstanceId` that names no instance — including one a delete retired — is
//!   refused with the modelled `ResourceNotFoundException` (HTTP 404), never a 501;
//! * a `Value` that is not one of the two booleans a toggle takes is refused with HTTP
//!   400 and stores nothing.

use serde_json::{json, Value};
use service_com_amazonaws_connect_amazonconnectservice::ops::types::INSTANCE_ATTRIBUTE_TYPE_VALUES;

use super::harness::{connect_managed, Answer, Connect};

/// This service's one not-found sentence, written out here rather than read off the
/// implementation so the suite pins the text itself.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// What Connect answers a parameter whose value it does not accept with: the member is
/// named, the rejected value is not.
const INVALID_VALUE: &str = "Invalid parameter: Value";

/// What it answers a request whose `InstanceId` label carries no identifier at all with.
const INVALID_REQUEST: &str = "Invalid request";

/// The two values a feature toggle takes.
const ENABLED: &str = "true";
/// See [`ENABLED`].
const DISABLED: &str = "false";

/// The toggles a newly created instance carries ENABLED — the set AWS answers for a
/// fresh instance — with every other modelled type off until a caller turns it on. The
/// two call directions are in neither group: they follow what `CreateInstance` was given.
/// Written out here rather than read off the implementation so the suite pins the set.
const ENABLED_BY_DEFAULT: [&str; 3] = ["AUTO_RESOLVE_BEST_VOICES", "CONTACT_LENS", "EARLY_MEDIA"];

/// The `InstanceAttributeType` members that are the instance's call-direction flags.
const INBOUND_CALLS: &str = "INBOUND_CALLS";
/// See [`INBOUND_CALLS`].
const OUTBOUND_CALLS: &str = "OUTBOUND_CALLS";

/// The modelled `@length` bound of `com.amazonaws.connect#InstanceAttributeValue`
/// (`smithy_query(query="operation",operation="UpdateInstanceAttribute")` -> length min 1
/// max 100), written out here so the table states the bound rather than reading it off
/// the implementation.
const VALUE_MIN_LENGTH: usize = 1;
/// See [`VALUE_MIN_LENGTH`].
const VALUE_MAX_LENGTH: usize = 100;

impl Connect {
    /// `UpdateInstanceAttribute`: `POST /instance/{InstanceId}/attribute/{AttributeType}`.
    async fn set_attribute(
        &self,
        instance_id: &str,
        attribute_type: &str,
        body: Value,
    ) -> Answer {
        self.send_answer(
            "POST",
            &format!("/instance/{instance_id}/attribute/{attribute_type}"),
            Some(body),
        )
        .await
    }

    /// `DescribeInstanceAttribute`: `GET /instance/{InstanceId}/attribute/{AttributeType}`.
    async fn attribute(&self, instance_id: &str, attribute_type: &str) -> Answer {
        self.send_answer(
            "GET",
            &format!("/instance/{instance_id}/attribute/{attribute_type}"),
            None,
        )
        .await
    }

    /// `DescribeInstance`: `GET /instance/{InstanceId}`.
    async fn instance(&self, instance_id: &str) -> Answer {
        self.send_answer("GET", &format!("/instance/{instance_id}"), None)
            .await
    }
}

/// The value the describe reports for a toggle, asserting the whole modelled `Attribute`
/// so the answer carries the type it was asked about as well as its value.
async fn stored_value(
    connect: &Connect,
    case: &str,
    instance_id: &str,
    attribute_type: &str,
) -> String {
    let answer = connect.attribute(instance_id, attribute_type).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from DescribeInstanceAttribute, got {} {}",
        answer.status, answer.body
    );
    let attribute = &answer.body["Attribute"];
    assert_eq!(
        attribute["AttributeType"].as_str(),
        Some(attribute_type),
        "case {case:?}: expected the answer to carry AttributeType {attribute_type}, got \
         {attribute}"
    );
    attribute["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Value, got {attribute}"))
        .to_string()
}

/// The `Message` member of an answered error.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// A `CreateInstance` body with the call directions set as named.
fn instance_with(alias: &str, inbound: bool, outbound: bool) -> Value {
    json!({
        "IdentityManagementType": "CONNECT_MANAGED",
        "InstanceAlias": alias,
        "InboundCallsEnabled": inbound,
        "OutboundCallsEnabled": outbound,
    })
}

/// The default this suite expects for one attribute type on an instance created with
/// these call directions.
fn expected_default(attribute_type: &str, inbound: bool, outbound: bool) -> &'static str {
    let enabled = match attribute_type {
        INBOUND_CALLS => inbound,
        OUTBOUND_CALLS => outbound,
        other => ENABLED_BY_DEFAULT.contains(&other),
    };
    if enabled {
        ENABLED
    } else {
        DISABLED
    }
}

/// The hand-written sets above are pinned to the model's own value list: a name the enum
/// does not declare is a stale table, not a failing emulator.
#[test]
fn every_named_attribute_type_is_modelled() {
    for attribute_type in ENABLED_BY_DEFAULT
        .iter()
        .chain([INBOUND_CALLS, OUTBOUND_CALLS].iter())
    {
        assert!(
            INSTANCE_ATTRIBUTE_TYPE_VALUES.contains(attribute_type),
            "case {attribute_type:?}: InstanceAttributeType does not declare it"
        );
    }
}

/// RULE: an update stores the value it names and the describe reads it back — for every
/// attribute type the model declares, and in both directions, so no row can pass on a
/// value that happened to be the default.
#[tokio::test]
async fn every_modelled_toggle_round_trips_in_both_directions() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributeroundtrip"))
        .await;

    for attribute_type in INSTANCE_ATTRIBUTE_TYPE_VALUES {
        // Off, then on, then off again: the second write has to be visible too, so a
        // store that only ever recorded the first would fail here.
        for value in [DISABLED, ENABLED, DISABLED] {
            let case = format!("{attribute_type}={value}");
            let answer = connect
                .set_attribute(&instance_id, attribute_type, json!({ "Value": value }))
                .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200 from UpdateInstanceAttribute, got {} {}",
                answer.status, answer.body
            );

            let actual = stored_value(&connect, &case, &instance_id, attribute_type).await;
            assert_eq!(
                actual, value,
                "case {case:?}: expected the stored value {value:?}, got {actual:?}"
            );
        }
    }
}

/// RULE: a toggle nobody has set reports its default, and reading it stores nothing — the
/// read is repeatable, and the defaults of the call directions are the flags
/// `CreateInstance` was given rather than a fixed pair.
///
/// The rows differ along the created instance's call directions, over every modelled
/// attribute type.
#[tokio::test]
async fn an_unset_toggle_reports_its_default_and_reading_it_stores_nothing() {
    let connect = Connect::new();

    for (index, (inbound, outbound)) in [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        let (instance_id, _) = connect
            .create_instance(instance_with(&format!("attributedefaults{index}"), inbound, outbound))
            .await;

        for attribute_type in INSTANCE_ATTRIBUTE_TYPE_VALUES {
            let expected = expected_default(attribute_type, inbound, outbound);
            for read_number in ["first", "second"] {
                let case =
                    format!("inbound={inbound} outbound={outbound} {attribute_type} {read_number} read");
                let actual = stored_value(&connect, &case, &instance_id, attribute_type).await;
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the default {expected:?}, got {actual:?}"
                );
            }
        }
    }
}

/// RULE (the other half of "stores what it names"): setting one toggle leaves every other
/// toggle of the same instance, and every other instance, at the value a caller would
/// have read before the call.
#[tokio::test]
async fn setting_one_toggle_leaves_every_other_one_alone() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributeisolationone"))
        .await;
    let (other_id, _) = connect
        .create_instance(connect_managed("attributeisolationtwo"))
        .await;

    for attribute_type in INSTANCE_ATTRIBUTE_TYPE_VALUES {
        // Flip this one away from its default, so the write is observable.
        let flipped = if expected_default(attribute_type, true, true) == ENABLED {
            DISABLED
        } else {
            ENABLED
        };
        let answer = connect
            .set_attribute(&instance_id, attribute_type, json!({ "Value": flipped }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {attribute_type}: expected HTTP 200, got {} {}",
            answer.status, answer.body
        );

        for other_type in INSTANCE_ATTRIBUTE_TYPE_VALUES.iter().filter(|t| *t != attribute_type) {
            let case = format!("{attribute_type} set, {other_type} read");
            let expected = expected_default(other_type, true, true);
            let actual = stored_value(&connect, &case, &instance_id, other_type).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the untouched toggle to still report \
                 {expected:?}, got {actual:?}"
            );
        }

        // And one instance's toggles are its own.
        let case = format!("{attribute_type} set on the other instance");
        let expected = expected_default(attribute_type, true, true);
        let actual = stored_value(&connect, &case, &other_id, attribute_type).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the second instance to still report {expected:?}, got \
             {actual:?}"
        );

        // Put it back, so the next row starts from the defaults again.
        let restored = expected_default(attribute_type, true, true);
        connect
            .set_attribute(&instance_id, attribute_type, json!({ "Value": restored }))
            .await;
    }
}

/// RULE: the call-direction toggles are the instance's OWN flags, so setting one is what
/// `DescribeInstance` reports — the toggle and the instance cannot disagree.
///
/// The rows are the two directions, each set both ways, with the other direction asserted
/// unchanged.
#[tokio::test]
async fn the_call_direction_toggles_are_the_instances_own_flags() {
    let connect = Connect::new();

    for (attribute_type, member, other_member) in [
        (INBOUND_CALLS, "InboundCallsEnabled", "OutboundCallsEnabled"),
        (OUTBOUND_CALLS, "OutboundCallsEnabled", "InboundCallsEnabled"),
    ] {
        let (instance_id, _) = connect
            .create_instance(instance_with(
                &format!("calldirection{}", member.to_ascii_lowercase()),
                true,
                true,
            ))
            .await;

        for (value, reported) in [(DISABLED, false), (ENABLED, true)] {
            let case = format!("{attribute_type}={value}");
            let answer = connect
                .set_attribute(&instance_id, attribute_type, json!({ "Value": value }))
                .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200, got {} {}",
                answer.status, answer.body
            );

            let described = connect.instance(&instance_id).await;
            assert_eq!(
                described.body["Instance"][member].as_bool(),
                Some(reported),
                "case {case:?}: expected DescribeInstance to report {member}={reported}, got {}",
                described.body
            );
            assert_eq!(
                described.body["Instance"][other_member].as_bool(),
                Some(true),
                "case {case:?}: expected {other_member} to stay as created, got {}",
                described.body
            );

            let actual = stored_value(&connect, &case, &instance_id, attribute_type).await;
            assert_eq!(
                actual, value,
                "case {case:?}: expected the toggle to report {value:?}, got {actual:?}"
            );
        }
    }
}

/// RULE: there is nothing to set and nothing to report on an instance this account does
/// not hold, whichever toggle is named — including an identifier that named one until
/// `DeleteInstance` retired it.
///
/// The rows differ along WHY the instance is absent, with a live instance standing beside
/// them so no row can pass merely because the store is empty.
#[tokio::test]
async fn an_absent_instance_sets_and_reports_nothing() {
    let connect = Connect::new();
    let (live_id, _) = connect
        .create_instance(connect_managed("attributeneighbour"))
        .await;
    let (retired_id, _) = connect
        .create_instance(connect_managed("attributeretired"))
        .await;
    let deleted = connect
        .send_answer("DELETE", &format!("/instance/{retired_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "DeleteInstance of a live instance: got {} {}",
        deleted.status, deleted.body
    );

    let rows: Vec<(&str, String)> = vec![
        (
            "an identifier of the minted form that holds nothing",
            "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30".to_string(),
        ),
        ("a name, not an identifier", "support".to_string()),
        ("an identifier a delete retired", retired_id.clone()),
    ];

    for (case, instance_id) in rows {
        assert_ne!(
            instance_id, live_id,
            "case {case:?}: the row has to name an instance the store does not hold"
        );
        for attribute_type in INSTANCE_ATTRIBUTE_TYPE_VALUES {
            let answers = [
                (
                    "UpdateInstanceAttribute",
                    connect
                        .set_attribute(&instance_id, attribute_type, json!({ "Value": ENABLED }))
                        .await,
                ),
                (
                    "DescribeInstanceAttribute",
                    connect.attribute(&instance_id, attribute_type).await,
                ),
            ];
            for (operation, answer) in answers {
                assert_eq!(
                    answer.status, 404,
                    "case {case:?}/{attribute_type}/{operation}: expected HTTP 404, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code, "ResourceNotFoundException",
                    "case {case:?}/{attribute_type}/{operation}: expected \
                     ResourceNotFoundException, got {:?} {}",
                    answer.error_code, answer.body
                );
                let actual = message_of(&answer.body);
                assert_eq!(
                    actual, RESOURCE_NOT_FOUND,
                    "case {case:?}/{attribute_type}/{operation}: expected message \
                     {RESOURCE_NOT_FOUND:?}, got {actual:?}"
                );
            }
        }
    }

    // The live instance is untouched by every row above: its toggles still read.
    let actual = stored_value(&connect, "the live instance", &live_id, INBOUND_CALLS).await;
    assert_eq!(
        actual, ENABLED,
        "the live instance's toggles are still served, got {actual:?}"
    );
}

/// RULE: a `Value` that is not one of the two booleans a toggle takes is refused with
/// HTTP 400 and stores nothing.
///
/// The rows differ along the FORM of the value: the booleans in the wrong case, other
/// spellings of a boolean, text, digits, values with whitespace around them, the modelled
/// length maximum, and one past each end of the modelled `@length` bound (which the
/// request boundary refuses, so those rows assert the status only).
#[tokio::test]
async fn a_value_that_is_not_a_toggle_is_refused_and_stores_nothing() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributebadvalue"))
        .await;
    let attribute_type = "CONTACTFLOW_LOGS";

    // The value every refusal below has to leave stored.
    let stored = ENABLED;
    let first = connect
        .set_attribute(&instance_id, attribute_type, json!({ "Value": stored }))
        .await;
    assert_eq!(
        first.status, 200,
        "expected the toggle to be set first, got {} {}",
        first.status, first.body
    );

    // (case, the value sent, whether this operation's own modelled refusal answers it)
    let rows: Vec<(&str, String, bool)> = vec![
        ("the enabled value in title case", "True".to_string(), true),
        ("the enabled value in upper case", "TRUE".to_string(), true),
        ("the disabled value in upper case", "FALSE".to_string(), true),
        ("another spelling of enabled", "yes".to_string(), true),
        ("enabled as a digit", "1".to_string(), true),
        ("a word, not a boolean", "enabled".to_string(), true),
        ("the enabled value with a trailing space", "true ".to_string(), true),
        ("the disabled value with a leading space", " false".to_string(), true),
        ("both values at once", "truefalse".to_string(), true),
        (
            "the modelled maximum length, still not a boolean",
            "t".repeat(VALUE_MAX_LENGTH),
            true,
        ),
        // Outside the modelled bound, so the request boundary refuses it before the
        // handler runs: the status is the assertion, not this operation's own wording.
        (
            "one below the modelled minimum length",
            "t".repeat(VALUE_MIN_LENGTH - 1),
            false,
        ),
        (
            "one past the modelled maximum length",
            "t".repeat(VALUE_MAX_LENGTH + 1),
            false,
        ),
    ];

    for (case, value, modelled) in rows {
        let answer = connect
            .set_attribute(&instance_id, attribute_type, json!({ "Value": value }))
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        if modelled {
            assert_eq!(
                answer.error_code, "InvalidParameterException",
                "case {case:?}: expected InvalidParameterException, got {:?} {}",
                answer.error_code, answer.body
            );
            let actual = message_of(&answer.body);
            assert_eq!(
                actual, INVALID_VALUE,
                "case {case:?}: expected message {INVALID_VALUE:?}, got {actual:?}"
            );
            assert!(
                !answer.body.to_string().contains(value.trim()),
                "case {case:?}: the refusal must not quote the rejected value, got {}",
                answer.body
            );
        }

        let actual = stored_value(&connect, case, &instance_id, attribute_type).await;
        assert_eq!(
            actual, stored,
            "case {case:?}: expected the refusal to leave {stored:?} stored, got {actual:?}"
        );
    }
}

/// RULE (the instance label): an `InstanceId` that carries no identifier at all
/// addresses nothing, so both operations refuse it as the malformed request it is — the
/// modelled `InvalidRequestException` (HTTP 400) — ahead of any lookup, rather than as a
/// resource that happens to be missing.
///
/// The rows are the three ways a label present in the URI carries no identifier.
#[tokio::test]
async fn a_label_carrying_no_identifier_is_refused_as_a_malformed_request() {
    let connect = Connect::new();
    // A live instance stands beside the refused labels, so no row can pass merely
    // because the store is empty.
    connect
        .create_instance(connect_managed("attributeblanklabel"))
        .await;

    for (case, sent) in [
        ("one space", "%20"),
        ("several spaces", "%20%20%20"),
        ("a tab", "%09"),
    ] {
        let answers = [
            (
                "UpdateInstanceAttribute",
                connect
                    .set_attribute(sent, INBOUND_CALLS, json!({ "Value": ENABLED }))
                    .await,
            ),
            (
                "DescribeInstanceAttribute",
                connect.attribute(sent, INBOUND_CALLS).await,
            ),
        ];
        for (operation, answer) in answers {
            assert_eq!(
                answer.status, 400,
                "case {case:?}/{operation}: expected HTTP 400, got {} {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code, "InvalidRequestException",
                "case {case:?}/{operation}: expected InvalidRequestException, got {:?} {}",
                answer.error_code, answer.body
            );
            let actual = message_of(&answer.body);
            assert_eq!(
                actual, INVALID_REQUEST,
                "case {case:?}/{operation}: expected message {INVALID_REQUEST:?}, got \
                 {actual:?}"
            );
        }
    }
}

/// RULE (idempotency): setting a toggle to a value it already holds is the same state,
/// whether or not the caller sends the `ClientToken` the model gives the request.
#[tokio::test]
async fn a_repeated_update_carrying_a_client_token_stores_the_same_value() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributeidempotent"))
        .await;
    let attribute_type = "MESSAGE_STREAMING";

    for body in [
        json!({ "Value": ENABLED, "ClientToken": "a-token-the-caller-chose" }),
        json!({ "Value": ENABLED, "ClientToken": "a-token-the-caller-chose" }),
        json!({ "Value": ENABLED }),
    ] {
        let case = body.to_string();
        let answer = connect
            .set_attribute(&instance_id, attribute_type, body.clone())
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} {}",
            answer.status, answer.body
        );
        let actual = stored_value(&connect, &case, &instance_id, attribute_type).await;
        assert_eq!(
            actual, ENABLED,
            "case {case:?}: expected the stored value {ENABLED:?}, got {actual:?}"
        );
    }
}
