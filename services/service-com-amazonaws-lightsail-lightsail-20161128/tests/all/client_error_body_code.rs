// SPDX-License-Identifier: CC0-1.0

//! The `code` member a Lightsail client-error body carries, driven over the wire
//! through the same front door a client uses.
//!
//! The rule under test: a Lightsail failure body carries, in its modelled `code`
//! member, the DISCRIMINATOR the refusing operation reports for that refusal
//! class — and the member is never absent, because `code` is what an SDK client
//! branches on. Which discriminator that is depends on the OPERATION that
//! refuses, not on the kind of resource it could not find:
//!
//! * the alarm operations (`PutAlarm`, `DeleteAlarm`, `TestAlarm`) report the
//!   modelled exception's own name, `NotFoundException`, for a resource they
//!   cannot find — `PutAlarm` included, whose not-found is about the instance,
//!   load balancer or database it was asked to watch;
//! * the resource operations report the sub-classification `DoesNotExist` for the
//!   same condition;
//! * an input-validation refusal reports `InvalidParams`, whichever operation
//!   answers it.
//!
//! Every one of them words the not-found message the same way — `The specified
//! resource <name> does not exist within types [<ResourceType>, ...]. Please
//! provide a valid resource name.` — and the invalid-instance-type refusal of
//! `CreateCloudFormationStack` is AWS's own sentence around the rejected value.
//!
//! So every case here is a TABLE: one row per operation that refuses, crossed
//! with identifiers and rejected values that differ in length, case, digits,
//! separators and form. The HTTP harness is the one `instance_lifecycle.rs`
//! already has.

use serde_json::{json, Value};

use crate::instance_lifecycle::{
    does_not_exist_message, does_not_exist_within_message, error_code, Emulator,
};

/// The wire error (awsJson `__type`) of a not-found refusal: the modelled shape's
/// own name, whichever discriminator the body carries.
const NOT_FOUND: &str = "NotFoundException";

/// The body `code` an ALARM operation reports for a resource it cannot find.
///
/// SOURCE: the recorded refusals of a `PutAlarm` naming a resource the account
/// does not hold and of a `DeleteAlarm` naming an alarm that was never created —
/// both carry the modelled exception's own name here.
const ALARM_NOT_FOUND_CODE: &str = "NotFoundException";

/// The body `code` a RESOURCE operation reports for the same condition.
///
/// SOURCE: the recorded refusals of `GetInstance` and `DeleteInstance` naming an
/// instance the account does not hold.
const DOES_NOT_EXIST: &str = "DoesNotExist";

/// The wire error of a refusal whose cause is that the input does not conform to
/// a validation rule of an input field.
const INVALID_INPUT: &str = "InvalidInputException";

/// The body `code` such a refusal reports, whichever operation answers it.
///
/// SOURCE: the recorded refusal of a `CreateCloudFormationStack` whose instance
/// entry named a Lightsail bundle id where an Amazon EC2 instance type belongs.
const INVALID_PARAMS: &str = "InvalidParams";

/// The resource types `PutAlarm`'s not-found message lists, in its order.
///
/// SOURCE: the recorded refusal's own sentence — "does not exist within types
/// [Instance, LoadBalancer, RelationalDatabase, Distribution, Bucket]". Written
/// out here independently of the service's own constant, so these rows pin the
/// list rather than restating whatever the code happens to hold.
const MONITORED_TYPES: &[&str] = &[
    "Instance",
    "LoadBalancer",
    "RelationalDatabase",
    "Distribution",
    "Bucket",
];

/// The modelled `ResourceType` of an alarm, which the alarm's own not-found
/// message names.
const ALARM: &str = "Alarm";

/// Identifiers nothing was ever created under, differing along what the rule
/// generalises over: the length, the case, the digits, the separators. Every one
/// satisfies the modelled `ResourceName` pattern `^\w[\w\-]*\w$`, so the
/// centralized walk passes it through to the handler.
const MISSING_NAMES: &[&str] = &[
    "ab",
    "never-created",
    "Probe_7",
    "sim2real-probe-alarm-missing-f0ca242d",
    "a-name-with-many-hyphen-separated-words-and-digits-0123456789",
];

/// The blueprint and bundle the monitored instance of these cases is created
/// from: the monitored resource's own configuration is not what any case is
/// about.
const INSTANCE_BLUEPRINT: &str = "amazon_linux_2023";
const INSTANCE_BUNDLE: &str = "micro_3_0";

/// A `PutAlarm` body watching `monitored` under the alarm name `alarm`, valid in
/// every other respect so the only thing a refusal can be about is the name.
fn put_alarm_body(alarm: &str, monitored: &str) -> Value {
    json!({
        "alarmName": alarm,
        "monitoredResourceName": monitored,
        "metricName": "CPUUtilization",
        "comparisonOperator": "GreaterThanThreshold",
        "threshold": 80.0,
        "evaluationPeriods": 1,
    })
}

/// Assert `status`/`body` is a not-found refusal carrying `code` and `message`.
fn assert_not_found(case: &str, status: u16, body: &Value, code: &str, message: &str) {
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} {body:?}"
    );
    assert_eq!(
        error_code(body),
        NOT_FOUND,
        "case {case}: expected the wire error {NOT_FOUND}, got {body:?}"
    );
    assert_eq!(
        body["code"].as_str(),
        Some(code),
        "case {case}: expected the body code {code}, got {body:?}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {body:?}"
    );
}

/// Assert `status`/`body` is an input-validation refusal carrying
/// [`INVALID_PARAMS`].
fn assert_invalid_params(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} {body:?}"
    );
    assert_eq!(
        error_code(body),
        INVALID_INPUT,
        "case {case}: expected the wire error {INVALID_INPUT}, got {body:?}"
    );
    assert_eq!(
        body["code"].as_str(),
        Some(INVALID_PARAMS),
        "case {case}: expected the body code {INVALID_PARAMS}, got {body:?}"
    );
}

// ---------------------------------------------------------------------------
// The alarm operations
// ---------------------------------------------------------------------------

/// RULE: every alarm operation answers a resource it cannot find with the
/// modelled exception's own name in the body's `code`, beside the service's
/// not-found message naming the identifier the request carried and the types it
/// was looked for among.
#[tokio::test]
async fn the_alarm_operations_report_the_exception_name_as_the_not_found_code() {
    let emulator = Emulator::new();
    // One resource an alarm can watch and one stored alarm, so each refusal below
    // is about the name the request carried rather than about an empty store.
    emulator
        .ok(
            "CreateInstances",
            json!({
                "instanceNames": ["box"],
                "availabilityZone": "us-east-1a",
                "blueprintId": INSTANCE_BLUEPRINT,
                "bundleId": INSTANCE_BUNDLE,
            }),
        )
        .await;
    emulator.ok("PutAlarm", put_alarm_body("cpu", "box")).await;

    // (operation, the body built from the missing identifier, the types the
    // operation's message lists)
    type Body = fn(&str) -> Value;
    let paths: &[(&str, Body, &[&str])] = &[
        (
            "PutAlarm",
            |name| put_alarm_body("probe", name),
            MONITORED_TYPES,
        ),
        ("DeleteAlarm", |name| json!({"alarmName": name}), &[ALARM]),
        (
            "TestAlarm",
            |name| json!({"alarmName": name, "state": "ALARM"}),
            &[ALARM],
        ),
    ];

    for (operation, body, types) in paths {
        for name in MISSING_NAMES {
            let case = format!("{operation} on {name:?}");
            let (status, answered) = emulator.call(operation, body(name)).await;
            assert_not_found(
                &case,
                status,
                &answered,
                ALARM_NOT_FOUND_CODE,
                &does_not_exist_within_message(types, name),
            );
        }
    }

    // The same operations serve the names that ARE stored, so the refusals above
    // were about the identifier and not about the operation.
    emulator
        .ok(
            "TestAlarm",
            json!({"alarmName": "cpu", "state": "INSUFFICIENT_DATA"}),
        )
        .await;
    emulator.ok("PutAlarm", put_alarm_body("net", "box")).await;
    emulator
        .ok("DeleteAlarm", json!({"alarmName": "cpu"}))
        .await;
}

/// RULE: the discriminator depends on the operation that refuses, so the resource
/// operations keep reporting their own sub-classification for exactly the same
/// condition — a name the account holds nothing under.
#[tokio::test]
async fn the_resource_operations_report_their_own_not_found_code() {
    let emulator = Emulator::new();

    // (operation, the member carrying the identifier, the resource type its
    // message names, the identifiers to try)
    let paths: &[(&str, &str, &str, &[&str])] = &[
        ("GetInstance", "instanceName", "Instance", MISSING_NAMES),
        ("DeleteInstance", "instanceName", "Instance", MISSING_NAMES),
        ("GetDisk", "diskName", "Disk", MISSING_NAMES),
        ("GetKeyPair", "keyPairName", "KeyPair", MISSING_NAMES),
        (
            "GetDomain",
            "domainName",
            "Domain",
            &["absent.example", "deep.sub.absent.example"],
        ),
    ];

    for (operation, member, resource_type, names) in paths {
        for name in *names {
            let case = format!("{operation} on {name:?}");
            let (status, body) = emulator.call(operation, json!({*member: name})).await;
            assert_not_found(
                &case,
                status,
                &body,
                DOES_NOT_EXIST,
                &does_not_exist_message(resource_type, name),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

/// RULE: a refusal whose cause is that the input does not conform to a validation
/// rule carries `InvalidParams` in the body's `code`, whichever operation answers
/// it and whichever rule the input broke.
#[tokio::test]
async fn every_input_validation_refusal_reports_the_invalid_params_code() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateInstances",
            json!({
                "instanceNames": ["box"],
                "availabilityZone": "us-east-1a",
                "blueprintId": INSTANCE_BLUEPRINT,
                "bundleId": INSTANCE_BUNDLE,
            }),
        )
        .await;

    let one_entry = json!({
        "sourceName": "export-record",
        "instanceType": "t2.micro",
        "portInfoSource": "DEFAULT",
        "availabilityZone": "us-east-1a",
    });
    // (case, operation, body) — one row per rule the request can break, across
    // the operations that enforce them: a count, a range, a dependent value, a
    // required member of a set, a form, and a page token.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "no instance entry at all",
            "CreateCloudFormationStack",
            json!({"instances": []}),
        ),
        (
            "two instance entries in one call",
            "CreateCloudFormationStack",
            json!({"instances": [one_entry.clone(), one_entry.clone()]}),
        ),
        (
            "a Lightsail bundle id where an Amazon EC2 instance type belongs",
            "CreateCloudFormationStack",
            json!({"instances": [{
                "sourceName": "export-record",
                "instanceType": "nano_2_0",
                "portInfoSource": "DEFAULT",
                "availabilityZone": "us-east-1a",
            }]}),
        ),
        (
            "a zone of no region this service serves",
            "CreateCloudFormationStack",
            json!({"instances": [{
                "sourceName": "export-record",
                "instanceType": "t2.micro",
                "portInfoSource": "DEFAULT",
                "availabilityZone": "mars-central-1a",
            }]}),
        ),
        (
            "an evaluation window below the documented minimum",
            "PutAlarm",
            {
                let mut body = put_alarm_body("probe", "box");
                body["evaluationPeriods"] = json!(0);
                body
            },
        ),
        (
            "an evaluation window above the documented maximum",
            "PutAlarm",
            {
                let mut body = put_alarm_body("probe", "box");
                body["evaluationPeriods"] = json!(289);
                body
            },
        ),
        (
            "an M out of N datapoint count above the N",
            "PutAlarm",
            {
                let mut body = put_alarm_body("probe", "box");
                body["evaluationPeriods"] = json!(2);
                body["datapointsToAlarm"] = json!(3);
                body
            },
        ),
        (
            "notification triggers that do not name the ALARM state",
            "PutAlarm",
            {
                let mut body = put_alarm_body("probe", "box");
                body["notificationTriggers"] = json!(["OK", "INSUFFICIENT_DATA"]);
                body
            },
        ),
        (
            "a metric the monitored resource's type does not publish",
            "PutAlarm",
            {
                let mut body = put_alarm_body("probe", "box");
                body["metricName"] = json!("DatabaseConnections");
                body
            },
        ),
        (
            "a page token naming no alarm to continue from",
            "GetAlarms",
            json!({"pageToken": "not-a-page-token"}),
        ),
        (
            "a page token naming no instance to continue from",
            "GetInstances",
            json!({"pageToken": "not-a-page-token"}),
        ),
    ];

    for (case, operation, body) in rows {
        let (status, answered) = emulator.call(operation, body).await;
        assert_invalid_params(&format!("{operation}: {case}"), status, &answered);
    }
}

/// RULE: `CreateCloudFormationStack` refuses an `instanceType` that is not an
/// Amazon EC2 instance type with AWS's own sentence around the rejected value —
/// fixed text, the value taken from the request — and lets every well-formed type
/// through to the resolution of the export snapshot record the entry names.
#[tokio::test]
async fn the_invalid_instance_type_refusal_is_awss_sentence_around_the_rejected_value() {
    let emulator = Emulator::new();

    // Values that are not an Amazon EC2 instance type name, differing along what
    // the rule generalises over: a Lightsail bundle id (two of them, the shapes
    // a plan id takes), a missing dot, upper case, an empty part on either side,
    // a third part, and a hyphen where no family boundary is.
    let rejected = [
        "nano_2_0",
        "micro_3_0",
        "t2micro",
        "T2.MICRO",
        "t2.",
        ".micro",
        "t2.micro.large",
        "c7i.large-x",
        "-c7i.large",
    ];
    for instance_type in rejected {
        let case = format!("instanceType {instance_type:?}");
        let (status, body) = emulator
            .call(
                "CreateCloudFormationStack",
                json!({"instances": [{
                    "sourceName": "export-record",
                    "instanceType": instance_type,
                    "portInfoSource": "DEFAULT",
                    "availabilityZone": "us-east-1a",
                }]}),
            )
            .await;
        assert_invalid_params(&case, status, &body);
        assert_eq!(
            body["message"].as_str(),
            Some(
                format!(
                    "Sorry, the input instance type {instance_type} is invalid. See the \
                     Lightsail API documentation for examples of valid instance types."
                )
                .as_str()
            ),
            "case {case}: expected AWS's invalid-instance-type sentence, got {body:?}"
        );
    }

    // A well-formed Amazon EC2 instance type is not what the refusal is about, so
    // the call gets past the type check and is refused for the source the entry
    // names — with the resource operations' own not-found discriminator.
    for instance_type in ["t2.micro", "c7i-flex.large", "mac2-m2pro.metal"] {
        let case = format!("instanceType {instance_type:?}");
        let (status, body) = emulator
            .call(
                "CreateCloudFormationStack",
                json!({"instances": [{
                    "sourceName": "no-such-export-record",
                    "instanceType": instance_type,
                    "portInfoSource": "DEFAULT",
                    "availabilityZone": "us-east-1a",
                }]}),
            )
            .await;
        assert_not_found(
            &case,
            status,
            &body,
            DOES_NOT_EXIST,
            &does_not_exist_message("ExportSnapshotRecord", "no-such-export-record"),
        );
    }
}
