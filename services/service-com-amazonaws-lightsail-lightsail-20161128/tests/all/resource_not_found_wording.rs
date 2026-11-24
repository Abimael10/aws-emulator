// SPDX-License-Identifier: CC0-1.0

//! How Lightsail WORDS a not-found refusal, driven over the wire through the same
//! front door a client uses.
//!
//! The rule under test: a Lightsail refusal whose cause is that the request named
//! a resource the account does not hold is worded from WHAT THE REQUEST STATED.
//!
//! * The request named the resource by both its identifier and its kind — the
//!   input member it carried the identifier in admits one `ResourceType` and one
//!   only — so the refusal names that type and the identifier: `The
//!   <ResourceType> does not exist: <name>`.
//! * The request stated no kind, so the refusal had to SEARCH for the identifier
//!   among resource types and reports what it searched instead: `The specified
//!   resource <name> does not exist within types [<ResourceType>, ...]. Please
//!   provide a valid resource name.` That is also the sentence the alarm
//!   operations' recorded refusals carry for an alarm name.
//!
//! The envelope around both — HTTP 400, the wire error `NotFoundException`, and
//! the body `code` discriminator — is pinned by `instance_error_envelope.rs` and
//! `client_error_body_code.rs`; this suite is about the `message`, which is what a
//! teardown guard, a log scraper and a console surface read.
//!
//! So every case is a TABLE: one row per operation that addresses a resource by
//! its own identifier, across the resource types the service serves, crossed with
//! identifiers that differ in length, case, digits and separators — plus the
//! deleted-then-read path for a key pair and a disk, where the resource DID exist
//! and the refusal must read the same as for one never created.
//!
//! The HTTP harness is the one `instance_lifecycle.rs` already has, and both
//! wordings are stated once there (`does_not_exist_message`,
//! `does_not_exist_within_message`).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::instance_lifecycle::{
    does_not_exist_message, does_not_exist_within_message, error_code, Emulator,
};

/// The wire error (awsJson `__type`) of every not-found refusal: the modelled
/// shape's own name.
const NOT_FOUND: &str = "NotFoundException";

/// The body `code` a RESOURCE operation reports for a resource it cannot find.
const DOES_NOT_EXIST: &str = "DoesNotExist";

/// The body `code` an ALARM operation reports for the same condition.
const ALARM_NOT_FOUND: &str = "NotFoundException";

/// Identifiers nothing was ever created under, differing along what the rule
/// generalises over: the length, the case, the digits, the separators. Every one
/// satisfies the modelled `ResourceName` pattern `^\w[\w\-]*\w$`, so the
/// centralized constraint walk passes it through to the handler.
const MISSING_NAMES: &[&str] = &[
    "ab",
    "never-created",
    "Ghost_7",
    "sim2real-probe-9f2c242d",
    "a-name-with-many-hyphen-separated-words-and-digits-0123456789",
];

/// The same for the modelled `DomainName`, which is a DNS name rather than a
/// `ResourceName`: one apex and one deeper label set.
const MISSING_DOMAINS: &[&str] = &["absent.example", "deep.sub.absent.example"];

/// The same for the modelled `BucketName`, whose pattern
/// `^[a-z0-9][a-z0-9-]{1,52}[a-z0-9]$` and length 3..=54 admit neither upper case
/// nor underscores: the modelled minimum length, a hyphenated name and one
/// carrying digits.
const MISSING_BUCKETS: &[&str] = &["abc", "never-created-bucket", "b2-9f2c242d-probe"];

/// The same for the modelled `ContainerServiceName`, pattern
/// `^[a-z0-9]{1,2}|[a-z0-9][a-z0-9-]+[a-z0-9]$`.
const MISSING_SERVICES: &[&str] = &["cs", "never-created-service", "svc-9f2c242d"];

/// The blueprint and bundle the instance of the alarm rows is created from
/// (`src/state.rs`, `BLUEPRINT_SPECS` / `BUNDLE_SPECS`).
const BLUEPRINT_ID: &str = "amazon_linux_2023";
const BUNDLE_ID: &str = "nano_3_0";

/// The availability zone every resource of this suite is created in.
const ZONE: &str = "us-east-1a";

/// Every operation that addresses ONE kind of resource by its own identifier,
/// with the input member carrying it, the modelled `ResourceType` its refusal must
/// name, and the identifiers to try.
///
/// SOURCE of each resource type word: the modelled `ResourceType` enum, which is
/// also what every `Operation` record and resource ARN of that kind carries.
type Row = (&'static str, &'static str, &'static str, &'static [&'static str]);
const RESOURCE_ADDRESSED: &[Row] = &[
    // The two kinds whose recorded refusals pin the wording.
    ("GetKeyPair", "keyPairName", "KeyPair", MISSING_NAMES),
    ("DeleteKeyPair", "keyPairName", "KeyPair", MISSING_NAMES),
    ("GetDisk", "diskName", "Disk", MISSING_NAMES),
    ("DeleteDisk", "diskName", "Disk", MISSING_NAMES),
    // The rest of the resource surface, where the same rule holds.
    ("GetInstance", "instanceName", "Instance", MISSING_NAMES),
    ("GetInstanceState", "instanceName", "Instance", MISSING_NAMES),
    ("RebootInstance", "instanceName", "Instance", MISSING_NAMES),
    ("DeleteInstance", "instanceName", "Instance", MISSING_NAMES),
    ("GetStaticIp", "staticIpName", "StaticIp", MISSING_NAMES),
    ("ReleaseStaticIp", "staticIpName", "StaticIp", MISSING_NAMES),
    (
        "GetDiskSnapshot",
        "diskSnapshotName",
        "DiskSnapshot",
        MISSING_NAMES,
    ),
    (
        "DeleteDiskSnapshot",
        "diskSnapshotName",
        "DiskSnapshot",
        MISSING_NAMES,
    ),
    (
        "GetInstanceSnapshot",
        "instanceSnapshotName",
        "InstanceSnapshot",
        MISSING_NAMES,
    ),
    (
        "DeleteInstanceSnapshot",
        "instanceSnapshotName",
        "InstanceSnapshot",
        MISSING_NAMES,
    ),
    (
        "GetRelationalDatabase",
        "relationalDatabaseName",
        "RelationalDatabase",
        MISSING_NAMES,
    ),
    (
        "GetRelationalDatabaseSnapshot",
        "relationalDatabaseSnapshotName",
        "RelationalDatabaseSnapshot",
        MISSING_NAMES,
    ),
    (
        "GetLoadBalancer",
        "loadBalancerName",
        "LoadBalancer",
        MISSING_NAMES,
    ),
    (
        "DeleteLoadBalancer",
        "loadBalancerName",
        "LoadBalancer",
        MISSING_NAMES,
    ),
    (
        "GetDistributions",
        "distributionName",
        "Distribution",
        MISSING_NAMES,
    ),
    (
        "DeleteDistribution",
        "distributionName",
        "Distribution",
        MISSING_NAMES,
    ),
    (
        "DeleteCertificate",
        "certificateName",
        "Certificate",
        MISSING_NAMES,
    ),
    ("GetDomain", "domainName", "Domain", MISSING_DOMAINS),
    ("DeleteDomain", "domainName", "Domain", MISSING_DOMAINS),
    ("GetBuckets", "bucketName", "Bucket", MISSING_BUCKETS),
    (
        "GetContainerServices",
        "serviceName",
        "ContainerService",
        MISSING_SERVICES,
    ),
];

/// The `message` of a failure body.
fn message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// Assert `status`/`body` is a not-found refusal carrying `code` and exactly
/// `expected_message`.
fn assert_not_found(case: &str, status: u16, body: &Value, code: &str, expected_message: &str) {
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} {body:?}"
    );
    assert_eq!(
        error_code(body),
        NOT_FOUND,
        "case {case}: expected the wire error {NOT_FOUND}, got {:?}",
        body["__type"]
    );
    assert_eq!(
        body["code"].as_str(),
        Some(code),
        "case {case}: expected the body code {code}, got {:?}",
        body["code"]
    );
    assert_eq!(
        message(body),
        expected_message,
        "case {case}: expected the message {expected_message:?}, got {:?}",
        message(body)
    );
}

/// Poll `GetDisk` until it reports `want`.
///
/// The transition is owed by the clock, not by the poll: it arrives without
/// anyone asking more than once.
async fn wait_for_disk_state(emulator: &Emulator, name: &str, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let disk = emulator.ok("GetDisk", json!({"diskName": name})).await["disk"].clone();
        if disk["state"].as_str() == Some(want) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case waiting for disk state {want:?} on {name}: still {:?}",
            disk["state"]
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ---------------------------------------------------------------------------
// The resource-addressed wording
// ---------------------------------------------------------------------------

/// RULE: an operation handed both the identifier and the kind of resource it
/// addresses words its not-found refusal "The <ResourceType> does not exist:
/// <name>", with the type the operation addresses and the identifier copied
/// verbatim from the request.
#[tokio::test]
async fn every_resource_addressed_refusal_names_the_type_and_the_identifier() {
    let emulator = Emulator::new();

    for (operation, member, resource_type, names) in RESOURCE_ADDRESSED {
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

/// RULE: the wording is about what the request named, not about whether the
/// resource ever existed — a key pair that was created and then deleted is
/// refused with the same sentence as a name nothing was ever created under, on
/// the reader and on the second delete alike.
#[tokio::test]
async fn a_deleted_key_pair_is_refused_with_the_resource_addressed_wording() {
    let emulator = Emulator::new();
    // Names that differ along what the rule generalises over, each created and
    // then deleted through the API — no case reaches into the store.
    for name in ["kp", "round-trip-key", "Probe_Key-2026"] {
        let created = emulator
            .ok("CreateKeyPair", json!({"keyPairName": name}))
            .await;
        assert_eq!(
            created["keyPair"]["name"].as_str(),
            Some(name),
            "case creating {name:?}: expected the created key pair to carry the name, got {created:?}"
        );
        // It reads back while it is held, so the refusals below are about the
        // deletion and not about an operation that never served this name.
        let read = emulator.ok("GetKeyPair", json!({"keyPairName": name})).await;
        assert_eq!(
            read["keyPair"]["name"].as_str(),
            Some(name),
            "case reading {name:?} back: expected the stored key pair, got {read:?}"
        );

        emulator
            .ok("DeleteKeyPair", json!({"keyPairName": name}))
            .await;

        let expected = does_not_exist_message("KeyPair", name);
        for operation in ["GetKeyPair", "DeleteKeyPair"] {
            let case = format!("{operation} after deleting {name:?}");
            let (status, body) = emulator
                .call(operation, json!({"keyPairName": name}))
                .await;
            assert_not_found(&case, status, &body, DOES_NOT_EXIST, &expected);
        }
    }
}

/// RULE: the same for a disk — the normal shape of idempotent cleanup is deleting
/// one twice, and the second delete is refused "The Disk does not exist:
/// <diskName>".
#[tokio::test]
async fn a_deleted_disk_is_refused_with_the_resource_addressed_wording() {
    let emulator = Emulator::new();
    // Sizes differ so each row is a disk of its own rather than a renamed copy of
    // one; the smallest published volume is enough for every case.
    for (name, size_in_gb) in [("d1", 8), ("round-trip-disk", 16), ("Probe_Disk-2026", 32)] {
        emulator
            .ok(
                "CreateDisk",
                json!({"diskName": name, "availabilityZone": ZONE, "sizeInGb": size_in_gb}),
            )
            .await;
        wait_for_disk_state(&emulator, name, "available").await;
        emulator.ok("DeleteDisk", json!({"diskName": name})).await;

        let expected = does_not_exist_message("Disk", name);
        for operation in ["GetDisk", "DeleteDisk"] {
            let case = format!("{operation} after deleting {name:?}");
            let (status, body) = emulator.call(operation, json!({"diskName": name})).await;
            assert_not_found(&case, status, &body, DOES_NOT_EXIST, &expected);
        }
    }
}

// ---------------------------------------------------------------------------
// The searched-types wording
// ---------------------------------------------------------------------------

/// The resource types `PutAlarm`'s refusal reports `monitoredResourceName` was
/// looked for among, in the order its message lists them.
///
/// SOURCE: the recorded refusal's own sentence — "does not exist within types
/// [Instance, LoadBalancer, RelationalDatabase, Distribution, Bucket]". Written
/// out here independently of the service's own constant, so this row pins the list
/// rather than restating whatever the code happens to hold.
const MONITORED_TYPES: &[&str] = &[
    "Instance",
    "LoadBalancer",
    "RelationalDatabase",
    "Distribution",
    "Bucket",
];

/// RULE: the OTHER wording stays where it belongs — a refusal whose request
/// stated no resource kind, and the alarm operations addressing an alarm, report
/// the identifier and the types searched instead of naming one type.
#[tokio::test]
async fn the_searched_types_wording_stays_with_the_refusals_that_searched() {
    let emulator = Emulator::new();
    // One monitorable resource and one stored alarm, so each refusal below is
    // about the name the request carried rather than about an empty store.
    emulator
        .ok(
            "CreateInstances",
            json!({
                "instanceNames": ["box"],
                "availabilityZone": ZONE,
                "blueprintId": BLUEPRINT_ID,
                "bundleId": BUNDLE_ID,
            }),
        )
        .await;
    emulator
        .ok(
            "PutAlarm",
            json!({
                "alarmName": "cpu",
                "monitoredResourceName": "box",
                "metricName": "CPUUtilization",
                "comparisonOperator": "GreaterThanThreshold",
                "threshold": 80.0,
                "evaluationPeriods": 1,
            }),
        )
        .await;

    type Body = fn(&str) -> Value;
    // (operation, the body built from the missing identifier, the types its
    // message lists)
    let paths: &[(&str, Body, &[&str])] = &[
        (
            "PutAlarm",
            |name| {
                json!({
                    "alarmName": "probe",
                    "monitoredResourceName": name,
                    "metricName": "CPUUtilization",
                    "comparisonOperator": "GreaterThanThreshold",
                    "threshold": 80.0,
                    "evaluationPeriods": 1,
                })
            },
            MONITORED_TYPES,
        ),
        ("DeleteAlarm", |name| json!({"alarmName": name}), &["Alarm"]),
        (
            "TestAlarm",
            |name| json!({"alarmName": name, "state": "ALARM"}),
            &["Alarm"],
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
                ALARM_NOT_FOUND,
                &does_not_exist_within_message(types, name),
            );
        }
    }

    // The two wordings are different sentences, so a client matching one never
    // matches the other.
    let addressed = does_not_exist_message("Alarm", "cpu");
    let searched = does_not_exist_within_message(&["Alarm"], "cpu");
    assert_ne!(
        addressed, searched,
        "case the two wordings: expected distinct sentences, got {addressed:?} for both"
    );
}
