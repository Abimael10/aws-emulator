// SPDX-License-Identifier: CC0-1.0

//! How Lightsail words — and what it words ABOUT — a not-found refusal of a
//! request that addressed ONE resource by its own identifier, driven over the
//! wire through the same front door a client uses.
//!
//! Two rules are under test here.
//!
//! 1. WORDING. An operation handed both the identifier and the kind of resource
//!    it addresses — its input member admits one `ResourceType` and one only —
//!    words the refusal `The <ResourceType> does not exist: <name>`, with the
//!    identifier copied verbatim from the request. The other sentence, the one
//!    naming a bracketed list of searched types, belongs to the refusals that
//!    had to SEARCH for an identifier whose kind the request did not state, and
//!    to the alarm operations; `resource_not_found_wording.rs` holds that side
//!    of the split, and this suite asserts these refusals never carry it.
//! 2. WHICH OPERAND. An operation whose input names several resources is refused
//!    about the operand it resolves FIRST, and for `AttachDisk` that is the
//!    instance the disk would be exposed to: a request naming neither a stored
//!    disk nor a stored instance is refused about the instance.
//!
//! So the wording case is a TABLE: one row per operation that addresses a
//! resource by its own identifier and was not already covered by
//! `instance_error_envelope.rs` or `resource_not_found_wording.rs`, reporting and
//! mutating alike, crossed with identifiers that differ in length, case, digits
//! and separators — plus the created-then-deleted path, where the resource DID
//! exist and the refusal must read the same as for one never created. The
//! operand case is a table too: every combination of a present and an absent
//! disk and instance.
//!
//! The HTTP harness, and both wordings, are the ones `instance_lifecycle.rs`
//! already has; the instance and disk fixtures are `disk_attachment.rs`'s.

use serde_json::{json, Value};

use crate::disk_attachment::{create_available_disk, create_running_instance};
use crate::instance_lifecycle::{does_not_exist_message, error_code, Emulator};

/// The wire error (awsJson `__type`) of every not-found refusal: the modelled
/// shape's own name.
const NOT_FOUND: &str = "NotFoundException";

/// The body `code` a resource operation reports for a resource it cannot find.
const DOES_NOT_EXIST: &str = "DoesNotExist";

/// The fixed fragment of the OTHER not-found sentence (the searched-types one),
/// so a case can assert a resource-addressed refusal does not carry it.
const SEARCHED_TYPES_FRAGMENT: &str = "does not exist within types";

/// The availability zone every resource of this suite is created in.
const ZONE: &str = "us-east-1a";

/// Identifiers nothing was ever created under, differing along what the rule
/// generalises over: the length, the case, the digits and the separators. Every
/// one satisfies the modelled `ResourceName` pattern `^\w[\w\-]*\w$`, so the
/// centralized constraint walk passes it through to the handler. The recorded
/// name is one row, not the case.
const MISSING_NAMES: &[&str] = &[
    "ab",
    "never-created",
    "Ghost_7",
    "sim2real-probe-lightsail-instance-missing-f0ca242d",
    "a-name-with-many-hyphen-separated-words-and-digits-0123456789",
];

/// The body one row of the wording table sends for a given identifier.
type Body = fn(&str) -> Value;

/// One operation that addresses a resource by its own identifier: how its
/// request body carries that identifier, and the modelled `ResourceType` its
/// refusal must name.
///
/// SOURCE of each resource-type word: the modelled `ResourceType` enum, which is
/// also what every `Operation` record and resource ARN of that kind carries.
struct Row {
    operation: &'static str,
    body: Body,
    resource_type: &'static str,
}

/// Every instance-addressed operation of the service, plus the key-pair reader
/// and deleter — the operations whose input member names one kind of resource
/// and one only.
fn wording_rows() -> Vec<Row> {
    let instance: Body = |name| json!({"instanceName": name});
    let port_info: Body = |name| {
        json!({
            "instanceName": name,
            "portInfo": {"fromPort": 22, "toPort": 22, "protocol": "tcp"},
        })
    };
    let port_infos: Body = |name| {
        json!({
            "instanceName": name,
            "portInfos": [{"fromPort": 80, "toPort": 80, "protocol": "tcp"}],
        })
    };
    let snapshot: Body = |name| {
        json!({
            "instanceSnapshotName": format!("snap-{name}"),
            "instanceName": name,
        })
    };
    let key_pair: Body = |name| json!({"keyPairName": name});

    vec![
        Row {
            operation: "GetInstance",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "GetInstanceState",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "RebootInstance",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "StartInstance",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "StopInstance",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "DeleteInstance",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "DeleteKnownHostKeys",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "GetInstanceAccessDetails",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "GetInstancePortStates",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "UpdateInstanceMetadataOptions",
            body: instance,
            resource_type: "Instance",
        },
        Row {
            operation: "OpenInstancePublicPorts",
            body: port_info,
            resource_type: "Instance",
        },
        Row {
            operation: "CloseInstancePublicPorts",
            body: port_info,
            resource_type: "Instance",
        },
        Row {
            operation: "PutInstancePublicPorts",
            body: port_infos,
            resource_type: "Instance",
        },
        Row {
            operation: "CreateInstanceSnapshot",
            body: snapshot,
            resource_type: "Instance",
        },
        Row {
            operation: "GetKeyPair",
            body: key_pair,
            resource_type: "KeyPair",
        },
        Row {
            operation: "DeleteKeyPair",
            body: key_pair,
            resource_type: "KeyPair",
        },
    ]
}

/// The `message` of a failure body.
fn message(body: &Value) -> String {
    body["message"]
        .as_str()
        .unwrap_or("<no message>")
        .to_string()
}

/// Assert `status`/`body` is the resource-addressed not-found refusal for
/// `resource_type` and `name`, and not the searched-types one.
fn assert_resource_addressed(case: &str, status: u16, body: &Value, resource_type: &str, name: &str) {
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
        Some(DOES_NOT_EXIST),
        "case {case}: expected the body code {DOES_NOT_EXIST}, got {:?}",
        body["code"]
    );
    let expected = does_not_exist_message(resource_type, name);
    assert_eq!(
        message(body),
        expected,
        "case {case}: expected the message {expected:?}, got {:?}",
        message(body)
    );
    assert!(
        !message(body).contains(SEARCHED_TYPES_FRAGMENT),
        "case {case}: a resource-addressed refusal must not carry the searched-types \
         sentence, got {:?}",
        message(body)
    );
}

// ---------------------------------------------------------------------------
// Rule 1: the wording
// ---------------------------------------------------------------------------

/// RULE: every operation that addresses one kind of resource by its own
/// identifier words a lookup miss "The <ResourceType> does not exist: <name>",
/// naming the kind its input member admits and the identifier the request
/// carried.
#[tokio::test]
async fn every_resource_addressed_refusal_names_its_type_and_identifier() {
    let emulator = Emulator::new();

    for row in wording_rows() {
        for name in MISSING_NAMES {
            let case = format!("{} on {name:?}", row.operation);
            let (status, body) = emulator.call(row.operation, (row.body)(name)).await;
            assert_resource_addressed(&case, status, &body, row.resource_type, name);
        }
    }

    // Every call above was refused, so nothing of any of those names was
    // stored — not by the readers and not by the mutators.
    for (listing, member) in [
        ("GetInstances", "instances"),
        ("GetKeyPairs", "keyPairs"),
        ("GetInstanceSnapshots", "instanceSnapshots"),
    ] {
        let listed = emulator.ok(listing, json!({})).await;
        assert_eq!(
            listed[member].as_array().map(Vec::len),
            Some(0),
            "case {listing} after every refusal: the store must still be empty, got {listed:?}"
        );
    }
}

/// RULE: the wording is about what the store holds NOW, not about whether the
/// resource ever existed — a resource created through the API and then deleted is
/// refused with the same sentence as a name nothing was ever created under.
#[tokio::test]
async fn a_deleted_resource_is_refused_with_the_same_resource_addressed_wording() {
    let emulator = Emulator::new();
    // Names differing along what the rule generalises over, each created and
    // then deleted through the API — no case reaches into the store.
    for name in ["ab", "short-lived-instance", "Probe_Instance-2026"] {
        emulator
            .ok(
                "CreateInstances",
                json!({
                    "instanceNames": [name],
                    "availabilityZone": ZONE,
                    "blueprintId": "amazon_linux_2023",
                    "bundleId": "nano_3_0",
                }),
            )
            .await;
        // It reads back while the account holds it, so the refusals below are
        // about the delete and not about an operation that never served it.
        let read = emulator.ok("GetInstance", json!({"instanceName": name})).await;
        assert_eq!(
            read["instance"]["name"].as_str(),
            Some(name),
            "case reading {name:?} back: expected the stored instance, got {read:?}"
        );
        emulator
            .ok("DeleteInstance", json!({"instanceName": name}))
            .await;

        for row in wording_rows()
            .into_iter()
            .filter(|row| row.resource_type == "Instance")
        {
            let case = format!("{} after deleting {name:?}", row.operation);
            let (status, body) = emulator.call(row.operation, (row.body)(name)).await;
            assert_resource_addressed(&case, status, &body, "Instance", name);
        }
    }

    // The same for a key pair, the other kind this family's refusals name: the
    // teardown read a client runs after deleting one.
    for name in ["kp", "round-trip-key", "Probe_Key-2026"] {
        emulator
            .ok("CreateKeyPair", json!({"keyPairName": name}))
            .await;
        emulator
            .ok("DeleteKeyPair", json!({"keyPairName": name}))
            .await;
        for operation in ["GetKeyPair", "DeleteKeyPair"] {
            let case = format!("{operation} after deleting {name:?}");
            let (status, body) = emulator
                .call(operation, json!({"keyPairName": name}))
                .await;
            assert_resource_addressed(&case, status, &body, "KeyPair", name);
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: which operand a two-resource refusal is about
// ---------------------------------------------------------------------------

/// One `AttachDisk` whose operands do not both resolve: the names it sends, and
/// the operand its refusal must be about.
struct Operands {
    case: &'static str,
    disk_name: &'static str,
    instance_name: &'static str,
    /// The resource type the refusal names, and the identifier it carries.
    refused_type: &'static str,
    refused_name: &'static str,
}

/// RULE: `AttachDisk` resolves the instance operand before the disk, so a
/// request whose instance the account does not hold is refused about that
/// instance whether or not the disk resolves, and the disk is reported only when
/// the instance did resolve.
#[tokio::test]
async fn attach_disk_is_refused_about_the_operand_it_resolves_first() {
    let emulator = Emulator::new();
    create_running_instance(&emulator, "host-instance", ZONE).await;
    create_available_disk(&emulator, "free-disk", ZONE, 8).await;

    let rows: &[Operands] = &[
        Operands {
            case: "neither operand resolves",
            disk_name: "ghost-disk",
            instance_name: "ghost-instance",
            refused_type: "Instance",
            refused_name: "ghost-instance",
        },
        Operands {
            case: "neither operand resolves, other names",
            disk_name: "sim2real-probe-lightsail-disk-missing-f0ca242d",
            instance_name: "sim2real-probe-lightsail-instance-missing-f0ca242d",
            refused_type: "Instance",
            refused_name: "sim2real-probe-lightsail-instance-missing-f0ca242d",
        },
        Operands {
            case: "the disk resolves, the instance does not",
            disk_name: "free-disk",
            instance_name: "ghost-instance",
            refused_type: "Instance",
            refused_name: "ghost-instance",
        },
        Operands {
            case: "the instance resolves, the disk does not",
            disk_name: "ghost-disk",
            instance_name: "host-instance",
            refused_type: "Disk",
            refused_name: "ghost-disk",
        },
    ];

    for row in rows {
        let case = row.case;
        let (status, body) = emulator
            .call(
                "AttachDisk",
                json!({
                    "diskName": row.disk_name,
                    "instanceName": row.instance_name,
                    "diskPath": "/dev/xvdf",
                }),
            )
            .await;
        assert_resource_addressed(case, status, &body, row.refused_type, row.refused_name);
    }

    // None of the refusals attached anything: the stored disk is still free and
    // the stored instance still exposes only its system disk.
    let disk = emulator.ok("GetDisk", json!({"diskName": "free-disk"})).await["disk"].clone();
    assert_eq!(
        disk["isAttached"].as_bool(),
        Some(false),
        "case after the refusals: expected the disk to stay unattached, got {disk:?}"
    );
    assert_eq!(
        disk["state"].as_str(),
        Some("available"),
        "case after the refusals: expected the disk to stay available, got {disk:?}"
    );
    let instance = emulator
        .ok("GetInstance", json!({"instanceName": "host-instance"}))
        .await["instance"]
        .clone();
    assert_eq!(
        instance["hardware"]["disks"].as_array().map(Vec::len),
        Some(1),
        "case after the refusals: expected only the system disk, got {:?}",
        instance["hardware"]["disks"]
    );
}
