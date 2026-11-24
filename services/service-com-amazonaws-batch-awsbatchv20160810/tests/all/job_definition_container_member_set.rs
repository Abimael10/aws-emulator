// SPDX-License-Identifier: CC0-1.0

//! The member set a read of a registered job definition reports for its container
//! properties.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire document
//! a `batch` SDK client sees. The rule under test: a read reports the FIXED member set AWS
//! Batch writes for a `ContainerProperties` object and the `NetworkConfiguration` it nests —
//! the interface-configuration list present and EMPTY where nothing holds an entry, and the
//! two deprecated resource reservations (`vcpus`, `memory`) present as an explicit NULL where
//! the registration supplied neither — instead of pruning a member that holds nothing. A
//! member the read writes unconditionally is also one a registration can hand back, so the
//! document a read reports round-trips.
//!
//! A client that reads a revision back to diff it against desired state therefore never sees
//! one of these members move between a value and absent, and one that iterates the
//! interface-configuration list needs no null guard. The member set does not heal or decay
//! either: every row is read TWICE and both reads must agree.

use serde_json::{json, Value};

use crate::job_lifecycle::Batch;

/// An image every row below registers, so no row turns on which image it names.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux";

/// The execution role a job on Fargate resources requires.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/execution";

/// The two members `ContainerProperties` reports whatever the registration supplied. Source:
/// the model's own shape (`smithy_query(query="shape", shape="ContainerProperties")`) — the
/// two `@deprecated` integer members, both documented as "deprecated, use
/// `resourceRequirements` instead".
const DEPRECATED_RESERVATIONS: &[&str] = &["vcpus", "memory"];

/// RULE: a read reports a container's `networkConfiguration` with every member that structure
/// carries — including the interface-configuration list, which comes back as the empty list
/// where nothing holds an entry and as the entries it holds otherwise. A container that named
/// no network configuration at all reports none, because an absent structure is not an empty
/// one.
///
/// The rows differ along the `assignPublicIp` the registration names (each value the model
/// declares, and none at all), along how many interface configurations it supplies (none, one,
/// two) and along whether it names a network configuration in the first place.
#[tokio::test]
async fn a_described_container_reports_its_network_configuration_in_full() {
    let batch = Batch::new();
    // (case, the networkConfiguration the registration supplies, the interfaceConfigurations
    // the read must report — None for "no network configuration is reported at all")
    let rows: Vec<(&str, Option<Value>, Option<Value>)> = vec![
        (
            "no network configuration at all",
            None,
            None,
        ),
        (
            "a public IP assignment turned on",
            Some(json!({"assignPublicIp": "ENABLED"})),
            Some(json!([])),
        ),
        (
            "a public IP assignment turned off",
            Some(json!({"assignPublicIp": "DISABLED"})),
            Some(json!([])),
        ),
        (
            "an empty network configuration",
            Some(json!({})),
            Some(json!([])),
        ),
        (
            "an interface-configuration list supplied already empty",
            Some(json!({"assignPublicIp": "ENABLED", "interfaceConfigurations": []})),
            Some(json!([])),
        ),
        (
            "one interface configuration supplied",
            Some(json!({
                "assignPublicIp": "ENABLED",
                "interfaceConfigurations": [{"interfaceName": "eth0"}],
            })),
            Some(json!([{"interfaceName": "eth0"}])),
        ),
        (
            "two interface configurations supplied",
            Some(json!({
                "assignPublicIp": "DISABLED",
                "interfaceConfigurations": [
                    {"interfaceName": "eth0"},
                    {"interfaceName": "eth1"},
                ],
            })),
            Some(json!([{"interfaceName": "eth0"}, {"interfaceName": "eth1"}])),
        ),
    ];

    for (index, (case, supplied, expected)) in rows.iter().enumerate() {
        let name = format!("jd-network-{index}");
        let mut container = fargate_container();
        if let Some(configuration) = supplied.as_ref() {
            container["networkConfiguration"] = configuration.clone();
        }
        let described = register_and_describe(
            &batch,
            case,
            json!({
                "jobDefinitionName": &name,
                "type": "container",
                "platformCapabilities": ["FARGATE"],
                "containerProperties": container,
            }),
            &name,
        )
        .await;
        let reported = &described["containerProperties"]["networkConfiguration"];
        let Some(expected) = expected else {
            assert!(
                reported.is_null(),
                "case {case:?}: a container that named no network configuration must report \
                 none, got {reported}"
            );
            continue;
        };
        let actual = reported.get("interfaceConfigurations");
        assert_eq!(
            actual,
            Some(expected),
            "case {case:?}: expected interfaceConfigurations {expected}, got {actual:?} \
             (the reported network configuration is {reported})"
        );
        let stated = supplied
            .as_ref()
            .and_then(|configuration| configuration.get("assignPublicIp").cloned());
        assert_eq!(
            reported.get("assignPublicIp").cloned(),
            stated,
            "case {case:?}: expected assignPublicIp {stated:?}, got {reported}"
        );
    }
}

/// RULE: a read reports the two deprecated resource reservations whatever the registration
/// supplied — the value it supplied, or an explicit null where it supplied none — so the key
/// is present on every container a read reports and a client can tell "the revision reserves
/// nothing here" from "this read dropped the member".
///
/// The rows differ along WHICH of the two the registration supplies (neither, one, the other,
/// both) and along the VALUE it supplies (one vCPU, several, the smallest documented memory
/// reservation of 4 MiB, a large one). Each row is asserted in BOTH places a job definition
/// carries container properties: the top-level `containerProperties` and the `container` of a
/// multi-node range, which is the same shape and so owes the same document.
#[tokio::test]
async fn a_described_container_reports_the_deprecated_reservations_it_holds() {
    let batch = Batch::new();
    // (case, the reservations the registration supplies)
    let rows: Vec<(&str, Value)> = vec![
        ("neither reservation", json!({})),
        ("one vCPU only", json!({"vcpus": 1})),
        ("several vCPUs only", json!({"vcpus": 8})),
        // "You must specify at least 4 MiB of memory for a job using this parameter"
        // (`ContainerProperties$memory`).
        ("the smallest memory reservation only", json!({"memory": 4})),
        ("both reservations", json!({"vcpus": 2, "memory": 4096})),
        ("a large memory reservation", json!({"vcpus": 4, "memory": 65536})),
    ];

    for (index, (case, supplied)) in rows.iter().enumerate() {
        let mut container = json!({"image": IMAGE});
        for (member, value) in supplied.as_object().cloned().unwrap_or_default() {
            container[member] = value;
        }

        // 1. The top-level containerProperties.
        let name = format!("jd-reservations-container-{index}");
        let described = register_and_describe(
            &batch,
            case,
            json!({
                "jobDefinitionName": &name,
                "type": "container",
                "containerProperties": container.clone(),
            }),
            &name,
        )
        .await;
        assert_reservations(
            case,
            "containerProperties",
            supplied,
            &described["containerProperties"],
        );

        // 2. The container of a multi-node range.
        let node_name = format!("jd-reservations-node-{index}");
        let described = register_and_describe(
            &batch,
            case,
            json!({
                "jobDefinitionName": &node_name,
                "type": "multinode",
                "nodeProperties": {
                    "numNodes": 2,
                    "mainNode": 0,
                    "nodeRangeProperties": [
                        {"targetNodes": "0:1", "container": container.clone()},
                    ],
                },
            }),
            &node_name,
        )
        .await;
        assert_reservations(
            case,
            "a node range's container",
            supplied,
            &described["nodeProperties"]["nodeRangeProperties"][0]["container"],
        );
    }
}

/// RULE: the members this read reports unconditionally are READABLE again, so a client that
/// copies a described container verbatim into a fresh `RegisterJobDefinition` is accepted and
/// the revision it registers holds exactly what the copied document said — an explicit null is
/// read as "no value", not as a type error.
///
/// The rows differ along which of the two reservations the first revision held, and therefore
/// along which of the copied keys is an explicit null and which carries a number.
#[tokio::test]
async fn a_described_container_can_be_registered_again_verbatim() {
    let batch = Batch::new();
    // (case, the reservations the first registration supplies)
    let rows: Vec<(&str, Value)> = vec![
        ("neither reservation", json!({})),
        ("one vCPU only", json!({"vcpus": 1})),
        ("both reservations", json!({"vcpus": 2, "memory": 4096})),
    ];

    for (index, (case, supplied)) in rows.iter().enumerate() {
        let mut container = json!({"image": IMAGE});
        for (member, value) in supplied.as_object().cloned().unwrap_or_default() {
            container[member] = value;
        }
        container["networkConfiguration"] = json!({"assignPublicIp": "DISABLED"});
        let name = format!("jd-roundtrip-{index}");
        let described = register_and_describe(
            &batch,
            case,
            json!({
                "jobDefinitionName": &name,
                "type": "container",
                "containerProperties": container,
            }),
            &name,
        )
        .await;

        // The copy a client makes: the described container, keys and nulls as reported.
        let copied = described["containerProperties"].clone();
        assert!(
            copied.get("vcpus").is_some() && copied.get("memory").is_some(),
            "case {case:?}: the copied container must carry both reservation keys, got {copied}"
        );
        let copy_name = format!("{name}-copy");
        let again = register_and_describe(
            &batch,
            case,
            json!({
                "jobDefinitionName": &copy_name,
                "type": "container",
                "containerProperties": copied.clone(),
            }),
            &copy_name,
        )
        .await;
        assert_reservations(
            case,
            "the re-registered container",
            supplied,
            &again["containerProperties"],
        );
        assert_eq!(
            again["containerProperties"], copied,
            "case {case:?}: re-registering a described container must hold the same document, \
             got {} from {copied}",
            again["containerProperties"]
        );
    }
}

/// RULE: reading the explicit null back as "no value" widens nothing else — a reservation
/// carrying anything but a whole number or that null is still refused, and the revision is not
/// stored.
///
/// The rows differ along the FORM of the value: the null the read itself writes and a whole
/// number in each of the two JSON spellings of one (accepted), against a string, a fractional
/// number, a boolean, a list and an object (refused).
#[tokio::test]
async fn a_reservation_outside_a_whole_number_or_the_null_a_read_writes_is_refused() {
    let batch = Batch::new();
    // (case, the value `vcpus` carries, the reservation the read must report — None for
    // "the registration is refused and stores nothing")
    let rows: Vec<(&str, Value, Option<Value>)> = vec![
        ("the null a read writes", Value::Null, Some(Value::Null)),
        ("a whole number", json!(2), Some(json!(2))),
        ("a whole number spelled as a float", json!(2.0), Some(json!(2))),
        ("a number as a string", json!("2"), None),
        ("a fractional number", json!(2.5), None),
        ("a boolean", json!(true), None),
        ("a list", json!([2]), None),
        ("an object", json!({"value": 2}), None),
    ];

    for (index, (case, carried, expected)) in rows.iter().enumerate() {
        let name = format!("jd-reservation-form-{index}");
        let request = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "containerProperties": {"image": IMAGE, "vcpus": carried},
        });
        let (status, body) = batch.call("/v1/registerjobdefinition", request).await;
        let Some(expected) = expected else {
            assert!(
                (400..500).contains(&status),
                "case {case:?}: expected the registration to be refused, got {status} {body}"
            );
            let (read_status, read_body) = batch
                .call(
                    "/v1/describejobdefinitions",
                    json!({"jobDefinitionName": &name}),
                )
                .await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: expected the read to answer 200, got {read_status} {read_body}"
            );
            assert_eq!(
                read_body["jobDefinitions"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: a refused registration must store nothing, got {read_body}"
            );
            continue;
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected the registration to be accepted, got {status} {body}"
        );
        let described = describe(&batch, case, &name).await;
        let actual = described["containerProperties"].get("vcpus");
        assert_eq!(
            actual,
            Some(expected),
            "case {case:?}: expected vcpus {expected}, got {actual:?} (the described \
             container is {})",
            described["containerProperties"]
        );
    }
}

/// Both deprecated reservations are present on `container`: as the value the registration
/// supplied, or as an explicit null where it supplied none.
fn assert_reservations(case: &str, place: &str, supplied: &Value, container: &Value) {
    for member in DEPRECATED_RESERVATIONS {
        let expected = supplied.get(member).cloned().unwrap_or(Value::Null);
        let actual = container.get(*member);
        assert_eq!(
            actual,
            Some(&expected),
            "case {case:?} in {place}: expected {member} {expected}, got {actual:?} \
             (the reported container is {container})"
        );
    }
}

/// A Fargate container: the image, the execution role Fargate requires and a supported
/// VCPU/MEMORY reservation — the deprecated `vcpus`/`memory` members are not applicable
/// there, so a Fargate row supplies neither.
fn fargate_container() -> Value {
    json!({
        "image": IMAGE,
        "executionRoleArn": EXECUTION_ROLE,
        "resourceRequirements": [
            {"type": "VCPU", "value": "0.25"},
            {"type": "MEMORY", "value": "512"},
        ],
    })
}

/// Register `request`, then read the revision back TWICE: the member set a read reports is
/// also the one a later read reports, so the gap this suite pins cannot be an artifact of the
/// registering call.
async fn register_and_describe(batch: &Batch, case: &str, request: Value, name: &str) -> Value {
    let (status, body) = batch.call("/v1/registerjobdefinition", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the registration to be accepted, got {status} {body}"
    );
    let first = describe(batch, case, name).await;
    let again = describe(batch, case, name).await;
    assert_eq!(
        first, again,
        "case {case:?}: a later read must report the same document, got {first} then {again}"
    );
    first
}

/// The one revision registered under `name`.
async fn describe(batch: &Batch, case: &str, name: &str) -> Value {
    let (status, body) = batch
        .call(
            "/v1/describejobdefinitions",
            json!({"jobDefinitionName": name}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the describe to answer 200, got {status} {body}"
    );
    let definitions = body["jobDefinitions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        definitions.len(),
        1,
        "case {case:?}: expected exactly one described revision, got {body}"
    );
    definitions[0].clone()
}
