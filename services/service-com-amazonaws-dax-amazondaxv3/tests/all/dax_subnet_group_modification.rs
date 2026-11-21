// SPDX-License-Identifier: CC0-1.0

//! Modifying a stored subnet group: what `UpdateSubnetGroup` changes, and what it leaves
//! exactly as it was.
//!
//! The rule every suite below drives is the one a modify follows on this service: ONLY
//! the members a request carries are applied to the stored group, each subnet list it
//! carries REPLACES the group's membership and is resolved against the subnets the
//! ACCOUNT has — as the service that owns subnets reports them — and a request naming a
//! group the store does not hold, or a membership that cannot be resolved, changes
//! nothing at all.
//!
//! So DAX is mounted BESIDE Amazon EC2 on one engine, through the shared wire harness in
//! `dax_account_network`, and every assertion is made against what EC2 holds and what
//! `DescribeSubnetGroups` reads back, never against a value written out here.

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::dax_account_network::{
    account_subnets, call, ec2, emulator, error_code, error_message, AccountSubnet,
    OTHER_REGION, REGION,
};

/// The status this service's `awsJson1_1` wire puts on every modelled client fault.
const CLIENT_FAULT_STATUS: u16 = 400;

/// The description every group below is created with, so a modification that must leave
/// the description alone has something to leave alone.
const AS_CREATED: &str = "as created";

/// A VPC of the client's OWN with one subnet in it, both minted through EC2: the other
/// way a subnet comes to be the account's, and a subnet in a DIFFERENT VPC from the
/// default one.
async fn client_subnet(router: &MultiServiceRouter, zone: &str) -> AccountSubnet {
    let client = ec2(router, REGION);
    let vpc = client
        .create_vpc()
        .cidr_block("10.77.0.0/16")
        .send()
        .await
        .expect("CreateVpc must mint a VPC")
        .vpc
        .as_ref()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc must report the VPC it minted")
        .to_string();
    let subnet = client
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block("10.77.1.0/24")
        .availability_zone(zone)
        .send()
        .await
        .expect("CreateSubnet must mint a subnet")
        .subnet
        .expect("CreateSubnet must report the subnet it minted");
    AccountSubnet {
        id: subnet
            .subnet_id()
            .expect("a minted subnet carries an id")
            .to_string(),
        zone: subnet.availability_zone().unwrap_or_default().to_string(),
        vpc,
    }
}

/// Create one subnet group over `subnet_ids` with [`AS_CREATED`] as its description, and
/// return the `SubnetGroup` the create answered with.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet_ids: &[String]) -> Value {
    let (status, body) = call(
        router,
        "CreateSubnetGroup",
        json!({
            "SubnetGroupName": name,
            "Description": AS_CREATED,
            "SubnetIds": subnet_ids,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateSubnetGroup over {subnet_ids:?} must succeed, got {status} {body:?}"
    );
    body["SubnetGroup"].clone()
}

/// The `SubnetGroup` `DescribeSubnetGroups` reads back for `name`.
async fn read_group(router: &MultiServiceRouter, name: &str) -> Value {
    let (status, body) = call(
        router,
        "DescribeSubnetGroups",
        json!({ "SubnetGroupNames": [name] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: DescribeSubnetGroups must report the stored group, got {status} {body:?}"
    );
    body["SubnetGroups"][0].clone()
}

/// The subnet identifiers a group reports, in the order it reports them.
fn reported_ids(group: &Value) -> Vec<String> {
    group["Subnets"]
        .as_array()
        .map(|subnets| {
            subnets
                .iter()
                .map(|subnet| {
                    subnet["SubnetIdentifier"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What one modification carries and what the group must report once it is applied.
struct ModifyCase {
    case: &'static str,
    /// The name the request addresses the group by, which need not be the stored casing.
    addressed_as: String,
    description: Option<&'static str>,
    subnet_ids: Option<Vec<String>>,
    expected_description: &'static str,
    expected_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Rule: a modification applies ONLY the members the request carries, leaving every other
// value of the stored group as it was, and answers the group as it now stands; a
// `SubnetIds` it carries REPLACES the membership, with each subnet's VPC and Availability
// Zone coming from the service that owns subnets.
// Source: `UpdateSubnetGroup` ("Modifies an existing subnet group") with `Description`
// and `SubnetIds` optional and `SubnetGroupName` required
// (smithy_query(query="operation",operation="UpdateSubnetGroup")), and
// `SubnetGroup$VpcId` / `Subnet$SubnetAvailabilityZone`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_the_members_a_modification_carries_are_applied() {
    let router = emulator();
    let held = account_subnets(&router, REGION).await;
    assert!(
        held.len() >= 3,
        "the account's default VPC must hold a subnet per zone, got {held:?}"
    );
    let ids: Vec<String> = held.iter().map(|subnet| subnet.id.clone()).collect();

    // A group NO case touches: a modification changes the group it names and nothing else.
    let sentinel = create_group(&router, "sentinel", &ids[..2]).await;

    // Rows differ along what the rule generalises over: which members the request carries
    // (one, the other, both, neither), the size and order of a replacement list, whether
    // it repeats a subnet, whether the description is emptied, and the casing the group is
    // addressed by.
    let cases = vec![
        ModifyCase {
            case: "description-only",
            addressed_as: "description-only".to_string(),
            description: Some("a new description"),
            subnet_ids: None,
            expected_description: "a new description",
            expected_ids: ids[..2].to_vec(),
        },
        ModifyCase {
            case: "subnets-only",
            addressed_as: "subnets-only".to_string(),
            description: None,
            subnet_ids: Some(vec![ids[2].clone()]),
            expected_description: AS_CREATED,
            expected_ids: vec![ids[2].clone()],
        },
        ModifyCase {
            case: "both-members",
            addressed_as: "both-members".to_string(),
            description: Some("both changed"),
            subnet_ids: Some(ids.iter().rev().cloned().collect()),
            expected_description: "both changed",
            expected_ids: ids.iter().rev().cloned().collect(),
        },
        ModifyCase {
            case: "neither-member",
            addressed_as: "neither-member".to_string(),
            description: None,
            subnet_ids: None,
            expected_description: AS_CREATED,
            expected_ids: ids[..2].to_vec(),
        },
        ModifyCase {
            case: "emptied-description",
            addressed_as: "emptied-description".to_string(),
            description: Some(""),
            subnet_ids: None,
            expected_description: "",
            expected_ids: ids[..2].to_vec(),
        },
        ModifyCase {
            case: "repeated-subnet",
            addressed_as: "repeated-subnet".to_string(),
            description: None,
            subnet_ids: Some(vec![ids[1].clone(), ids[1].clone()]),
            expected_description: AS_CREATED,
            expected_ids: vec![ids[1].clone(), ids[1].clone()],
        },
        ModifyCase {
            case: "every-subnet",
            addressed_as: "every-subnet".to_string(),
            description: Some("widened"),
            subnet_ids: Some(ids.clone()),
            expected_description: "widened",
            expected_ids: ids.clone(),
        },
        ModifyCase {
            // "This value is stored as a lowercase string"
            // (`CreateSubnetGroup$SubnetGroupName`), so the stored group is the one any
            // casing of its name addresses.
            case: "addressed-in-another-casing",
            addressed_as: "Addressed-In-Another-Casing".to_string(),
            description: Some("found whatever the casing"),
            subnet_ids: Some(vec![ids[0].clone()]),
            expected_description: "found whatever the casing",
            expected_ids: vec![ids[0].clone()],
        },
    ];

    for case in cases {
        let created = create_group(&router, case.case, &ids[..2]).await;
        let mut request = json!({ "SubnetGroupName": case.addressed_as });
        if let Some(description) = case.description {
            request["Description"] = json!(description);
        }
        if let Some(subnet_ids) = case.subnet_ids.clone() {
            request["SubnetIds"] = json!(subnet_ids);
        }

        let (status, body) = call(&router, "UpdateSubnetGroup", request).await;
        let label = case.case;
        assert_eq!(
            status, 200,
            "case {label:?}: a modification of a stored group succeeds, got {status} {body:?}"
        );
        let modified = body["SubnetGroup"].clone();

        assert_eq!(
            modified["SubnetGroupName"].as_str(),
            Some(label),
            "case {label:?}: the group keeps the name it is stored under, got {modified:?}"
        );
        assert_eq!(
            modified["Description"].as_str(),
            Some(case.expected_description),
            "case {label:?}: expected the description {:?}, got {modified:?}",
            case.expected_description
        );
        assert_eq!(
            reported_ids(&modified),
            case.expected_ids,
            "case {label:?}: expected the subnets {:?}, got {modified:?}",
            case.expected_ids
        );

        // Every subnet reports the zone, and the group the VPC, that the OWNER of subnets
        // holds for it — not anything derived from the identifier text.
        let expected_vpc = held
            .iter()
            .find(|subnet| subnet.id == case.expected_ids[0])
            .map(|subnet| subnet.vpc.clone());
        assert_eq!(
            modified["VpcId"].as_str().map(str::to_string),
            expected_vpc,
            "case {label:?}: expected the VPC EC2 holds these subnets in ({expected_vpc:?}), \
             got {modified:?}"
        );
        for subnet in modified["Subnets"].as_array().cloned().unwrap_or_default() {
            let id = subnet["SubnetIdentifier"].as_str().unwrap_or_default();
            let expected_zone = held
                .iter()
                .find(|known| known.id == id)
                .map(|known| known.zone.clone());
            assert_eq!(
                subnet["SubnetAvailabilityZone"].as_str().map(str::to_string),
                expected_zone,
                "case {label:?}: subnet {id} must report the zone EC2 holds it in \
                 ({expected_zone:?}), got {subnet:?}"
            );
        }

        // Round trip: the modification was PERSISTED, so the read reports exactly what the
        // modification answered — and a case that carried neither member reads back as it
        // was created.
        let read = read_group(&router, case.case).await;
        assert_eq!(
            read, modified,
            "case {label:?}: the stored group must read back as the modification answered, \
             got {read:?}"
        );
        if case.description.is_none() && case.subnet_ids.is_none() {
            assert_eq!(
                read, created,
                "case {label:?}: a modification carrying no member changes nothing, \
                 got {read:?}"
            );
        }

        // The group no case named is untouched by any of them.
        let untouched = read_group(&router, "sentinel").await;
        assert_eq!(
            untouched, sentinel,
            "case {label:?}: only the group a modification names may change, \
             got {untouched:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a modification naming a subnet group this service does not hold is refused with
// the modelled `SubnetGroupNotFoundFault`, naming the group as the service holds names,
// and mints nothing.
// Source: `UpdateSubnetGroup`'s `SubnetGroupNotFoundFault` ("The requested subnet group
// name does not refer to an existing subnet group"); the client-fault status is this
// service's JSON protocol, see `dax_client_fault_wire_status`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_modification_of_a_group_the_store_does_not_hold_is_refused() {
    let router = emulator();
    let ids = account_subnets(&router, REGION).await;
    let subnet = ids[0].id.clone();
    // One group that DOES exist, and a cluster and a parameter group whose names are not
    // subnet group names: the groups this service holds are the only ones a modify finds.
    create_group(&router, "stored", std::slice::from_ref(&subnet)).await;
    let (status, body) = call(
        &router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": "a-parameter-group" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateParameterGroup must succeed, got {status} {body:?}"
    );

    // Rows differ in the shape of the name that resolves to nothing — its length, its
    // casing, its hyphens, a name that belongs to ANOTHER resource family, and the
    // default parameter group's name — and in whether the request also carries subnets.
    let cases: [(&str, bool); 7] = [
        ("ghost", false),
        ("g", false),
        ("MixedCaseGhost", false),
        ("ghost-with-hyphens", true),
        ("a-parameter-group", false),
        (state::DEFAULT_PARAMETER_GROUP, false),
        ("storedx", true),
    ];

    for (name, with_subnets) in cases {
        let mut request = json!({
            "SubnetGroupName": name,
            "Description": "never applied",
        });
        if with_subnets {
            request["SubnetIds"] = json!([subnet.clone()]);
        }
        let (status, body) = call(&router, "UpdateSubnetGroup", request).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: expected {CLIENT_FAULT_STATUS}, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "SubnetGroupNotFoundFault",
            "case {name:?}: expected SubnetGroupNotFoundFault, got {body:?}"
        );
        assert_eq!(
            error_message(&body),
            format!("Subnet group {} not found.", name.to_lowercase()),
            "case {name:?}: the fault must name the group that resolves to nothing, \
             got {body:?}"
        );

        // A refused modification mints nothing: the group still does not exist.
        let (status, read) = call(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": [name] }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: a refused modification leaves nothing behind, got {status} {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "SubnetGroupNotFoundFault",
            "case {name:?}: expected SubnetGroupNotFoundFault, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a membership a modification carries is held to every rule a create's is held to —
// each identifier resolves against the subnets the ACCOUNT has (400
// `InvalidParameterValueException` otherwise), the list names at least one subnet and no
// more than a group may hold (`InvalidSubnet` / `SubnetQuotaExceededFault`), and every
// subnet of it belongs to ONE VPC (`InvalidSubnet`) — and a refused modification leaves
// the stored group exactly as it was.
// Source: `UpdateSubnetGroup`'s modelled errors, `CreateCluster$SubnetGroupName` ("All of
// the subnets that you specify in a subnet group must exist in the same VPC") and the
// create path those same rules are asserted on in `dax_account_network`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_membership_that_cannot_be_resolved_leaves_the_group_as_it_was() {
    let router = emulator();
    let held = account_subnets(&router, REGION).await;
    let ids: Vec<String> = held.iter().map(|subnet| subnet.id.clone()).collect();
    let elsewhere: Vec<String> = account_subnets(&router, OTHER_REGION)
        .await
        .into_iter()
        .map(|subnet| subnet.id)
        .collect();
    assert!(
        !elsewhere.is_empty() && !elsewhere.iter().any(|id| ids.contains(id)),
        "another Region's subnets are other subnets, got {elsewhere:?} beside {ids:?}"
    );
    // A subnet in a VPC of the client's own, and one the account HAD and deleted.
    let own = client_subnet(&router, &held[0].zone).await;
    let gone = client_subnet(&router, &held[1].zone).await;
    ec2(&router, REGION)
        .delete_subnet()
        .subnet_id(&gone.id)
        .send()
        .await
        .expect("DeleteSubnet must remove the subnet it was given");

    // One subnet more than a group may hold. The identifiers are fabricated on purpose:
    // the quota is checked before anything is resolved, so the list never reaches EC2.
    let over_quota: Vec<String> = (0..=state::MAX_SUBNETS_PER_GROUP)
        .map(|index| format!("subnet-{index:017x}"))
        .collect();
    let quota_message = format!(
        "SubnetIds names {} subnets, more than the {} allowed in a subnet group",
        over_quota.len(),
        state::MAX_SUBNETS_PER_GROUP
    );
    let unresolved_message = |unresolved: &[&str]| {
        format!(
            "Some input subnets in :[{}] are invalid.",
            unresolved.join(", ")
        )
    };

    // Rows differ in WHY the membership cannot be stored: an identifier nothing holds (in
    // either id form, and one that is not an id at all), a subnet of another Region, a
    // deleted subnet, a bad identifier among good ones, an empty list, a list spanning two
    // VPCs, and a list longer than a group may hold.
    let cases: Vec<(&str, Vec<String>, &str, String)> = vec![
        (
            "unheld-long-form",
            vec!["subnet-0000000000deadbee".to_string()],
            "InvalidParameterValueException",
            unresolved_message(&["subnet-0000000000deadbee"]),
        ),
        (
            "unheld-short-form",
            vec!["subnet-0a1b2c3d".to_string()],
            "InvalidParameterValueException",
            unresolved_message(&["subnet-0a1b2c3d"]),
        ),
        (
            "not-a-subnet-id",
            vec!["potato".to_string()],
            "InvalidParameterValueException",
            unresolved_message(&["potato"]),
        ),
        (
            "another-regions-subnet",
            vec![elsewhere[0].clone()],
            "InvalidParameterValueException",
            unresolved_message(&[elsewhere[0].as_str()]),
        ),
        (
            "a-deleted-subnet",
            vec![gone.id.clone()],
            "InvalidParameterValueException",
            unresolved_message(&[gone.id.as_str()]),
        ),
        (
            "one-bad-among-good",
            vec![
                ids[0].clone(),
                "subnet-0000000000deadbee".to_string(),
                ids[1].clone(),
            ],
            "InvalidParameterValueException",
            unresolved_message(&["subnet-0000000000deadbee"]),
        ),
        (
            "an-empty-list",
            Vec::new(),
            "InvalidSubnet",
            "SubnetIds must name at least one subnet".to_string(),
        ),
        (
            "two-vpcs",
            vec![ids[0].clone(), own.id.clone()],
            "InvalidSubnet",
            "All of the subnets that you specify in a subnet group must exist in the same VPC."
                .to_string(),
        ),
        (
            "over-quota",
            over_quota.clone(),
            "SubnetQuotaExceededFault",
            quota_message.clone(),
        ),
    ];

    for (case, subnet_ids, expected_code, expected_message) in cases {
        let created = create_group(&router, case, &ids[..2]).await;
        let (status, body) = call(
            &router,
            "UpdateSubnetGroup",
            json!({
                "SubnetGroupName": case,
                "Description": "never applied",
                "SubnetIds": subnet_ids,
            }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?}: {subnet_ids:?} is no membership a group can hold, \
             got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body:?}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {body:?}"
        );

        // NOTHING was applied — not the subnets, and not the description the same request
        // carried.
        let read = read_group(&router, case).await;
        assert_eq!(
            read, created,
            "case {case:?}: a refused modification leaves the group as it was, got {read:?}"
        );
    }
}
