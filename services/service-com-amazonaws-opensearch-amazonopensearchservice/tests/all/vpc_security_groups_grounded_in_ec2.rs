// SPDX-License-Identifier: CC0-1.0

//! Whether a VPC placement's security groups belong to its VPC is decided from what the
//! service that OWNS them answered.
//!
//! RULE: `VPCOptions$SecurityGroupIds` names security groups of the service that owns them,
//! and the VPC a group governs traffic in is that service's to say — so the refusal a
//! placement earns for naming a group of ANOTHER VPC than its subnets is grounded in what
//! that service ANSWERED, never in the form or the number of the identifiers a request
//! carried. An emulator with no such service mounted resolves neither side, so every call
//! that carries `VPCOptions` is served and the placement keeps exactly the groups it was
//! given: `CreateDomain` and `UpdateDomainConfig` for a domain, `CreateVpcEndpoint` and
//! `UpdateVpcEndpoint` for a service-managed endpoint. The other half of the rule — where
//! the groups and the subnets ARE resolvable, so a cross-VPC pair is refused — is driven by
//! the router's own `opensearch_vpc_security_group_membership` suite, which mounts the
//! owning service beside this one.
//!
//! The rows differ along what the rule generalises over: how many groups a request names,
//! which of the two identifier forms the owning service mints they take, whether one is
//! repeated, and whether the request names groups at all.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::VPC_ENDPOINT_STATUS_ACTIVE;

use super::domain_lifecycle::{poll_until, settled_domain, Emulator};
use super::vpc_endpoint_updates::update_endpoint;
use super::vpc_endpoints::{create_endpoint, describe_endpoints, member, minted, vpc_domain};

/// The subnet every placement below names: nothing resolves it either, so what differs
/// between the rows is only the security groups. ONE subnet, because the clusters below ask
/// for no zone awareness and a VPC-access domain names one subnet per Availability Zone it
/// is placed in.
const SUBNETS: [&str; 1] = ["subnet-0ccc0000000000001"];

/// One list of security groups a call names.
struct Row {
    case: &'static str,
    /// The domain this row's create makes, which is also what its read addresses.
    name: &'static str,
    /// The groups the request names; empty names none at all.
    groups: &'static [&'static str],
}

/// The table: the identifier forms the owning service mints a group id in (`sg-` and 8 or 17
/// hexadecimal digits), several counts of them, a repeat, and the absence of any.
const ROWS: &[Row] = &[
    Row {
        case: "one group in the long identifier form",
        name: "sg-ungrounded-one",
        groups: &["sg-0ddd0000000000001"],
    },
    Row {
        case: "one group in the short identifier form",
        name: "sg-ungrounded-short",
        groups: &["sg-0ddd0001"],
    },
    Row {
        case: "three groups, both identifier forms among them",
        name: "sg-ungrounded-three",
        groups: &["sg-0ddd0000000000002", "sg-0ddd0002", "sg-0ddd0000000000003"],
    },
    Row {
        case: "one group named twice",
        name: "sg-ungrounded-repeated",
        groups: &["sg-0ddd0000000000004", "sg-0ddd0000000000004"],
    },
    Row {
        case: "no security group at all",
        name: "sg-ungrounded-none",
        groups: &[],
    },
];

/// The `VPCOptions` a row's call carries.
fn vpc_options(groups: &[&str]) -> Value {
    let mut options = json!({ "SubnetIds": SUBNETS });
    if !groups.is_empty() {
        options["SecurityGroupIds"] = json!(groups);
    }
    options
}

/// The security groups a reported `VPCOptions` carries, in the order it reports them.
fn groups_in(vpc_options: &Value) -> Vec<String> {
    vpc_options["SecurityGroupIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// The same, for the `VPCOptions` of a reported `DomainStatus`.
fn reported_groups(status: &Value) -> Vec<String> {
    groups_in(&status["VPCOptions"])
}

#[tokio::test]
async fn a_domain_keeps_the_security_groups_nothing_could_resolve() {
    let emulator = Emulator::new();

    for row in ROWS {
        let case = row.case;
        let created = emulator
            .create(json!({
                "DomainName": row.name,
                "EngineVersion": "OpenSearch_2.11",
                "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
                "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
                "VPCOptions": vpc_options(row.groups),
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: a placement nothing could resolve grounds no refusal, got {} with \
             {:?}",
            created.status, created.body
        );

        // Read the domain back, so what is asserted is the placement the service STORED
        // rather than only what the create echoed.
        let read = emulator.describe(row.name).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the domain to be readable, got {} with {:?}",
            read.status, read.body
        );
        let expected: Vec<String> = row.groups.iter().map(|id| (*id).to_string()).collect();
        for (source, status) in [
            ("the create", &created.body["DomainStatus"]),
            ("DescribeDomain", &read.body["DomainStatus"]),
        ] {
            assert_eq!(
                reported_groups(status),
                expected,
                "case {case}: {source} must report the groups the request named, got {status:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_change_keeps_the_security_groups_nothing_could_resolve() {
    let emulator = Emulator::new();

    let name = "sg-ungrounded-changed";
    let created = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "VPCOptions": vpc_options(ROWS[0].groups),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateDomain must be served, got {} with {:?}",
        created.status, created.body
    );

    // The change half of the rule, over the rows that differ in what the groups look like.
    // The domain is idle before each change, which is the condition — not a stretch of time
    // — a client waits for.
    for row in &ROWS[1..] {
        let case = row.case;
        settled_domain(&emulator, name).await;
        let changed = emulator
            .update_config(name, json!({ "VPCOptions": vpc_options(row.groups) }))
            .await;
        assert_eq!(
            changed.status, 200,
            "case {case}: a change nothing could resolve grounds no refusal, got {} with {:?}",
            changed.status, changed.body
        );
        let read = emulator.describe(name).await;
        let expected: Vec<String> = row.groups.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            reported_groups(&read.body["DomainStatus"]),
            expected,
            "case {case}: the domain must carry the groups the change named, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn an_endpoint_keeps_the_security_groups_nothing_could_resolve() {
    let emulator = Emulator::new();
    let arn = vpc_domain(&emulator, "sg-ungrounded-endpoint").await;

    // One endpoint, created with the first row's groups, then moved through the rest: every
    // call carries `VpcOptions`, and none of its identifiers resolves anywhere.
    let case = ROWS[0].case;
    let created = create_endpoint(
        &emulator,
        json!({ "DomainArn": arn, "VpcOptions": vpc_options(ROWS[0].groups) }),
    )
    .await;
    let endpoint = minted(&created, case);
    let id = member(&endpoint, "VpcEndpointId", case);
    assert_eq!(
        groups_in(&endpoint["VpcOptions"]),
        vec![ROWS[0].groups[0].to_string()],
        "case {case}: the endpoint must carry the groups the create named, got {endpoint:?}"
    );

    for row in ROWS[1..].iter().filter(|row| !row.groups.is_empty()) {
        let case = row.case;
        // The endpoint is ACTIVE again before the next change, which is the condition a
        // client polls for.
        poll_until(case, || async {
            let read = describe_endpoints(&emulator, &[id.as_str()]).await;
            read.body["VpcEndpoints"][0]["Status"] == json!(VPC_ENDPOINT_STATUS_ACTIVE)
        })
        .await;

        let changed = update_endpoint(&emulator, &id, &vpc_options(row.groups)).await;
        assert_eq!(
            changed.status, 200,
            "case {case}: a change nothing could resolve grounds no refusal, got {} with {:?}",
            changed.status, changed.body
        );
        let read = describe_endpoints(&emulator, &[id.as_str()]).await;
        let reported = read.body["VpcEndpoints"][0]["VpcOptions"].clone();
        let expected: Vec<String> = row.groups.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            groups_in(&reported),
            expected,
            "case {case}: the endpoint must carry the groups the change named, got {reported:?}"
        );
    }
}
