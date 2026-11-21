// SPDX-License-Identifier: CC0-1.0

//! `LaunchInstances` launches instances INTO an Auto Scaling group.
//!
//! "Launches a specified number of instances in an Auto Scaling group. Returns instance IDs
//! and other details if launch is successful or error details if launch is unsuccessful"
//! (`smithy_query(query="operation",operation="LaunchInstances")`): the instances it answers
//! are instances OF THE GROUP, so the group reports them, the account's own Amazon EC2
//! state resolves every id, they run what the group's launch source says and sit in the
//! placement the request named — and the group's desired capacity rises by the number
//! launched, because a group holds the instances its capacity says it holds.
//!
//! The table below spans what that rule generalises over: how the GROUP is placed (by
//! subnet or by Availability Zone alone), how the REQUEST names the placement (a zone, a
//! zone id, a subnet, or nothing at all for a group with one zone), how much capacity is
//! asked for (including more than the hundred instances one launch places), and which
//! `RetryStrategy` it carries. Beside it: the launch a repeat of one client token is, the
//! refusals a request outside the group's own configuration is answered with, and the error
//! a launch that placed nothing reports.
//!
//! Three services are mounted on ONE engine and driven over their own wire protocols, the
//! way the emulator serves them, so the ids these assertions follow are the ids a client is
//! given.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, p, query, seed_subnets, tag_value,
    tag_values, xml_unescape, REGION,
};

/// The launch template every group here launches from, created over Amazon EC2's own wire
/// by the harness: its instances run `t3.micro` from `ami-0123456789abcdef0`.
const TEMPLATE: &str = "lt-launch";

/// The instance type that template carries, which is what every launched instance must run
/// and what every collection must report.
const TEMPLATE_INSTANCE_TYPE: &str = "t3.micro";

/// One `InstanceCollection` of an answer: the placement and instance type a set of launched
/// instances share, and their ids.
#[derive(Debug, Clone)]
struct Collection {
    instance_type: String,
    subnet_id: String,
    availability_zone: String,
    availability_zone_id: String,
    instance_ids: Vec<String>,
}

/// The collections an answer reports, in the order it reports them.
///
/// One collection per `InstanceIds` list, which is the member every collection carries: the
/// chunk before each closing `InstanceIds` tag holds that collection's own placement.
fn collections(body: &str) -> Vec<Collection> {
    let instances = tag_value(body, "Instances");
    instances
        .split("</InstanceIds>")
        .filter(|chunk| chunk.contains("<InstanceIds"))
        .map(|chunk| {
            let ids = chunk.split("<InstanceIds").nth(1).unwrap_or_default();
            Collection {
                instance_type: tag_value(chunk, "InstanceType"),
                subnet_id: tag_value(chunk, "SubnetId"),
                availability_zone: tag_value(chunk, "AvailabilityZone"),
                availability_zone_id: tag_value(chunk, "AvailabilityZoneId"),
                instance_ids: tag_values(ids, "member"),
            }
        })
        .collect()
}

/// Every instance id an answer reports, across its collections, sorted.
fn launched_ids(body: &str) -> Vec<String> {
    let mut ids: Vec<String> = collections(body)
        .into_iter()
        .flat_map(|collection| collection.instance_ids)
        .collect();
    ids.sort();
    ids
}

/// The Availability Zones of [`REGION`], from Amazon EC2's own catalog for it — never
/// written out here, so a zone name and its id are the pair that service holds.
fn zones() -> Vec<(String, String)> {
    ec2::availability_zones_in_region(REGION)
        .into_iter()
        .map(|zone| (zone.zone_name, zone.zone_id))
        .collect()
}

/// The instance ids the group itself reports, sorted.
async fn group_instance_ids(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    let mut ids = tag_values(&body, "InstanceId");
    ids.sort();
    ids
}

/// The `DesiredCapacity` the group reports.
async fn group_desired_capacity(router: &MultiServiceRouter, name: &str) -> String {
    let (_, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    tag_value(&body, "DesiredCapacity")
}

/// Where Amazon EC2 says one instance is: its `(availability zone, subnet id)`, or `None`
/// when EC2 resolves the id to no instance of the account.
async fn ec2_placement(
    router: &MultiServiceRouter,
    instance_id: &str,
) -> Option<(String, String, String)> {
    let (status, body) = ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    if status != 200 {
        return None;
    }
    Some((
        tag_value(&tag_value(&body, "placement"), "availabilityZone"),
        tag_value(&body, "subnetId"),
        tag_value(&body, "instanceType"),
    ))
}

/// How the GROUP is placed.
#[derive(Debug, Clone, Copy)]
enum GroupPlacement {
    /// `VPCZoneIdentifier` naming this many seeded subnets.
    Subnets(usize),
    /// `AvailabilityZones` naming this many of the Region's zones.
    Zones(usize),
}

/// How the REQUEST names where to launch.
#[derive(Debug, Clone, Copy)]
enum Names {
    /// `AvailabilityZones`, naming the zone at this index of the group's own.
    Zone(usize),
    /// `AvailabilityZoneIds`, naming the id of that same zone.
    ZoneId(usize),
    /// `SubnetIds`, naming the seeded subnet at this index.
    Subnet(usize),
    /// Nothing: the group's one zone is where the instances go.
    Nothing,
}

/// One row of the launch table.
struct Row {
    case: &'static str,
    group: GroupPlacement,
    names: Names,
    /// `RequestedCapacity`.
    capacity: i32,
    /// The group's `MaxSize`.
    max_size: i32,
    /// `RetryStrategy`, when the request carries one.
    retry: Option<&'static str>,
    /// How many instances the launch must place.
    expected: usize,
}

/// A launch places the instances it was asked for, in the group and in the placement the
/// request named, and the group's capacity and the account's Amazon EC2 state both report
/// them.
#[tokio::test]
async fn a_launch_request_launches_instances_into_the_group() {
    let rows = [
        Row {
            case: "a group of one subnet, the request naming that subnet",
            group: GroupPlacement::Subnets(1),
            names: Names::Subnet(0),
            capacity: 1,
            max_size: 3,
            retry: None,
            expected: 1,
        },
        Row {
            case: "a group of two subnets, the request naming the zone of the second",
            group: GroupPlacement::Subnets(2),
            names: Names::Zone(1),
            capacity: 2,
            max_size: 4,
            retry: Some("none"),
            expected: 2,
        },
        Row {
            case: "a group of two zones, the request naming the id of the second",
            group: GroupPlacement::Zones(2),
            names: Names::ZoneId(1),
            capacity: 3,
            max_size: 3,
            retry: Some("retry-with-group-configuration"),
            expected: 3,
        },
        Row {
            case: "a group of one zone, the request naming no placement",
            group: GroupPlacement::Zones(1),
            names: Names::Nothing,
            capacity: 2,
            max_size: 2,
            retry: None,
            expected: 2,
        },
        Row {
            case: "a capacity above the hundred instances one launch places",
            group: GroupPlacement::Subnets(1),
            names: Names::Subnet(0),
            capacity: 150,
            max_size: 200,
            retry: None,
            // "the actual instance count is limited to 100 instances per launch"
            // (`LaunchInstancesRequest$RequestedCapacity`).
            expected: 100,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let (router, engine) = emulator();
        let catalog = zones();
        let subnets = match row.group {
            GroupPlacement::Subnets(count) => seed_subnets(&engine, count),
            GroupPlacement::Zones(_) => Vec::new(),
        };
        create_launch_template(&router, TEMPLATE).await;

        let name = format!("asg-launch-{index}");
        let mut params: Vec<(String, String)> = vec![
            p("AutoScalingGroupName", name.clone()),
            p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            p("MinSize", "0"),
            p("MaxSize", row.max_size.to_string()),
        ];
        match row.group {
            GroupPlacement::Subnets(_) => {
                let identifier = subnets
                    .iter()
                    .map(|subnet| subnet.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                params.push(p("VPCZoneIdentifier", identifier));
            }
            GroupPlacement::Zones(count) => {
                for (position, (zone_name, _)) in catalog.iter().enumerate().take(count) {
                    params.push(p(
                        &format!("AvailabilityZones.member.{}", position + 1),
                        zone_name.clone(),
                    ));
                }
            }
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        // Where the launch must place, and how the request says so.
        let (expected_zone, expected_subnet, mut launch_params) = match row.names {
            Names::Subnet(position) => {
                let subnet = &subnets[position];
                (
                    subnet.zone.clone(),
                    Some(subnet.id.clone()),
                    vec![p("SubnetIds.member.1", subnet.id.clone())],
                )
            }
            Names::Zone(position) => match row.group {
                GroupPlacement::Subnets(_) => {
                    let subnet = &subnets[position];
                    (
                        subnet.zone.clone(),
                        Some(subnet.id.clone()),
                        vec![p("AvailabilityZones.member.1", subnet.zone.clone())],
                    )
                }
                GroupPlacement::Zones(_) => (
                    catalog[position].0.clone(),
                    None,
                    vec![p("AvailabilityZones.member.1", catalog[position].0.clone())],
                ),
            },
            Names::ZoneId(position) => (
                catalog[position].0.clone(),
                None,
                vec![p("AvailabilityZoneIds.member.1", catalog[position].1.clone())],
            ),
            Names::Nothing => match row.group {
                GroupPlacement::Subnets(_) => (
                    subnets[0].zone.clone(),
                    Some(subnets[0].id.clone()),
                    Vec::new(),
                ),
                GroupPlacement::Zones(_) => (catalog[0].0.clone(), None, Vec::new()),
            },
        };
        let token = format!("token-{index}");
        launch_params.push(p("AutoScalingGroupName", name.clone()));
        launch_params.push(p("RequestedCapacity", row.capacity.to_string()));
        launch_params.push(p("ClientToken", token.clone()));
        if let Some(retry) = row.retry {
            launch_params.push(p("RetryStrategy", retry));
        }

        let (status, body) = query(&router, "LaunchInstances", &borrowed(&launch_params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: LaunchInstances expected 200, got {status}: {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "AutoScalingGroupName"),
            name,
            "case {:?}: the answer names the group the instances were launched into, got {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "ClientToken"),
            token,
            "case {:?}: the answer carries the request's own idempotency token, got {body}",
            row.case
        );
        assert!(
            !body.contains("<ErrorCode>"),
            "case {:?}: a launch that placed everything reports no error, got {body}",
            row.case
        );

        let reported = collections(&body);
        let ids = launched_ids(&body);
        assert_eq!(
            ids.len(),
            row.expected,
            "case {:?}: expected {} launched instances, got {ids:?} in {body}",
            row.case,
            row.expected
        );
        for collection in &reported {
            assert_eq!(
                collection.instance_type, TEMPLATE_INSTANCE_TYPE,
                "case {:?}: the instances run the launch source's instance type, got \
                 {collection:?}",
                row.case
            );
            assert_eq!(
                collection.availability_zone, expected_zone,
                "case {:?}: expected the zone {expected_zone:?}, got {collection:?}",
                row.case
            );
            let zone_id = catalog
                .iter()
                .find(|(zone_name, _)| *zone_name == expected_zone)
                .map(|(_, zone_id)| zone_id.clone())
                .expect("the zone is one of the Region's own");
            assert_eq!(
                collection.availability_zone_id, zone_id,
                "case {:?}: the zone's two spellings are one placement, got {collection:?}",
                row.case
            );
            match expected_subnet.as_deref() {
                Some(subnet_id) => assert_eq!(
                    collection.subnet_id, subnet_id,
                    "case {:?}: expected the subnet {subnet_id:?}, got {collection:?}",
                    row.case
                ),
                None => assert!(
                    collection.subnet_id.is_empty(),
                    "case {:?}: a group that named no subnet places no instance in one, got \
                     {collection:?}",
                    row.case
                ),
            }
        }

        // The group reports exactly those instances, and its desired capacity rose by them.
        assert_eq!(
            group_instance_ids(&router, &name).await,
            ids,
            "case {:?}: the group reports the instances the launch placed",
            row.case
        );
        assert_eq!(
            group_desired_capacity(&router, &name).await,
            row.expected.to_string(),
            "case {:?}: the group's desired capacity rises by the instances launched",
            row.case
        );

        // Every id resolves in the account's own Amazon EC2 state, placed where the launch
        // said and running what the group's launch source says.
        for instance_id in &ids {
            let placement = ec2_placement(&router, instance_id).await.unwrap_or_else(|| {
                panic!(
                    "case {:?}: the launch answered {instance_id}, which Amazon EC2 resolves \
                     to no instance",
                    row.case
                )
            });
            assert_eq!(
                placement.0, expected_zone,
                "case {:?}: Amazon EC2 places {instance_id} in the zone the launch named, got \
                 {placement:?}",
                row.case
            );
            assert_eq!(
                placement.1,
                expected_subnet.clone().unwrap_or_default(),
                "case {:?}: Amazon EC2 places {instance_id} in the subnet the launch named, got \
                 {placement:?}",
                row.case
            );
            assert_eq!(
                placement.2, TEMPLATE_INSTANCE_TYPE,
                "case {:?}: Amazon EC2 launched {instance_id} as the launch source's type, got \
                 {placement:?}",
                row.case
            );
        }

        // A repeat of the SAME token is the same launch: it answers what the first answered
        // and places nothing more.
        let (status, repeat) = query(&router, "LaunchInstances", &borrowed(&launch_params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: a repeat of one client token expected 200, got {status}: {repeat}",
            row.case
        );
        assert_eq!(
            launched_ids(&repeat),
            ids,
            "case {:?}: a repeat of one client token answers the launch it already made",
            row.case
        );
        assert_eq!(
            group_instance_ids(&router, &name).await,
            ids,
            "case {:?}: a repeat of one client token launches nothing more",
            row.case
        );

        // The same token carrying OTHER parameters is a second request under one token.
        let mut mismatched = launch_params.clone();
        for parameter in &mut mismatched {
            if parameter.0 == "RequestedCapacity" {
                parameter.1 = (row.capacity + 1).to_string();
            }
        }
        let (status, body) = query(&router, "LaunchInstances", &borrowed(&mismatched)).await;
        assert_eq!(
            status, 400,
            "case {:?}: a token reused for other parameters expected 400, got {status}: {body}",
            row.case
        );
        assert!(
            body.contains("<Code>IdempotentParameterMismatch</Code>"),
            "case {:?}: expected the IdempotentParameterMismatch query error code, got {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "Message"),
            "The parameters in the current request do not match the parameters from a previous \
             request with the same client token",
            "case {:?}: expected the fixed one-token-two-requests sentence, got {body}",
            row.case
        );
    }
}

/// The launch reports one collection PER PLACEMENT: two subnets of one zone are two
/// placements, so the instances one launch spread across them are reported apart.
#[tokio::test]
async fn instances_are_reported_per_placement() {
    let (router, engine) = emulator();
    // Two subnets in ONE zone: the harness places the i-th subnet in the i-th zone of the
    // Region and wraps, so asking for one more than the zone count puts two in one zone.
    let zone_count = zones().len();
    let subnets = seed_subnets(&engine, zone_count + 1);
    create_launch_template(&router, TEMPLATE).await;
    let shared_zone = subnets[0].zone.clone();
    let paired: Vec<String> = subnets
        .iter()
        .filter(|subnet| subnet.zone == shared_zone)
        .map(|subnet| subnet.id.clone())
        .collect();
    assert_eq!(
        paired.len(),
        2,
        "the harness must have seeded two subnets in {shared_zone:?}, got {subnets:?}"
    );

    let identifier = paired.join(",");
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "asg-two-subnets"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "4"),
            ("VPCZoneIdentifier", identifier.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");

    let (status, body) = query(
        &router,
        "LaunchInstances",
        &[
            ("AutoScalingGroupName", "asg-two-subnets"),
            ("RequestedCapacity", "2"),
            ("ClientToken", "two-subnets"),
            ("SubnetIds.member.1", paired[0].as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "LaunchInstances: got {status}: {body}");
    let reported = collections(&body);
    assert_eq!(
        reported.len(),
        1,
        "a launch naming ONE subnet places into that subnet alone, got {reported:?} in {body}"
    );
    assert_eq!(
        reported[0].subnet_id, paired[0],
        "the collection reports the subnet the launch named, got {reported:?}"
    );

    // Naming the ZONE the two subnets share places into both of them, so the launch is
    // reported as two collections — one per subnet.
    let (status, body) = query(
        &router,
        "LaunchInstances",
        &[
            ("AutoScalingGroupName", "asg-two-subnets"),
            ("RequestedCapacity", "2"),
            ("ClientToken", "two-subnets-zone"),
            ("AvailabilityZones.member.1", shared_zone.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "LaunchInstances by zone: got {status}: {body}");
    let mut reported = collections(&body);
    reported.sort_by(|left, right| left.subnet_id.cmp(&right.subnet_id));
    assert_eq!(
        reported.len(),
        2,
        "two subnets of one zone are two placements, got {reported:?} in {body}"
    );
    let mut expected = paired.clone();
    expected.sort();
    assert_eq!(
        reported
            .iter()
            .map(|collection| collection.subnet_id.clone())
            .collect::<Vec<String>>(),
        expected,
        "each placement is reported with its own subnet, got {reported:?}"
    );
    for collection in &reported {
        assert_eq!(
            collection.availability_zone, shared_zone,
            "both placements are in the zone the launch named, got {collection:?}"
        );
        assert_eq!(
            collection.instance_ids.len(),
            1,
            "each of the two placements took one instance, got {collection:?}"
        );
    }
}

/// One row of the refusal table: what the request says, and the message the service answers.
struct Refusal {
    case: &'static str,
    /// The parameters the launch carries beyond the group name, capacity and token.
    params: Vec<(String, String)>,
    /// The group the request names, when it is not the created one.
    group: Option<&'static str>,
    capacity: i32,
    expected: String,
}

/// A launch request outside what the group is configured for is REFUSED, with the sentence
/// the rule it broke states — and the group is left exactly as it was.
#[tokio::test]
async fn a_launch_outside_the_groups_configuration_is_refused() {
    let (router, engine) = emulator();
    let catalog = zones();
    let subnets = seed_subnets(&engine, 2);
    create_launch_template(&router, TEMPLATE).await;

    // A group of ONE of the seeded subnets, so the other subnet and every other zone are
    // outside its configuration.
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "asg-refused"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");
    // And a group of TWO zones, which is the one a request has to state a placement for.
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "asg-multi-zone"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("AvailabilityZones.member.1", catalog[0].0.as_str()),
            ("AvailabilityZones.member.2", catalog[1].0.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");

    let cases = vec![
        Refusal {
            case: "a group the account does not hold",
            params: Vec::new(),
            group: Some("asg-absent"),
            capacity: 1,
            expected: "AutoScalingGroup not found - No such Auto Scaling group: asg-absent"
                .to_string(),
        },
        Refusal {
            case: "both spellings of one placement",
            params: vec![
                p("AvailabilityZones.member.1", subnets[0].zone.clone()),
                p("AvailabilityZoneIds.member.1", catalog[0].1.clone()),
            ],
            group: None,
            capacity: 1,
            expected: "You cannot specify both AvailabilityZones and AvailabilityZoneIds in the \
                       same request"
                .to_string(),
        },
        Refusal {
            case: "a zone of no Region",
            params: vec![p("AvailabilityZones.member.1", "zz-nowhere-9z")],
            group: None,
            capacity: 1,
            expected: "You must use a valid fully-formed launch template. Invalid availability \
                       zone: [zz-nowhere-9z]"
                .to_string(),
        },
        Refusal {
            case: "a subnet the account does not hold",
            params: vec![p("SubnetIds.member.1", "subnet-00000000000000000")],
            group: None,
            capacity: 1,
            expected: "The subnet ID 'subnet-00000000000000000' does not exist".to_string(),
        },
        Refusal {
            case: "a zone the group is not configured with",
            params: vec![p("AvailabilityZones.member.1", subnets[1].zone.clone())],
            group: None,
            capacity: 1,
            expected: "The specified Availability Zones must match or be included in the \
                       Availability Zone configuration of the Auto Scaling group"
                .to_string(),
        },
        Refusal {
            case: "a subnet the group is not configured with",
            params: vec![p("SubnetIds.member.1", subnets[1].id.clone())],
            group: None,
            capacity: 1,
            expected: "The specified subnets must be included in the subnet configuration of the \
                       Auto Scaling group"
                .to_string(),
        },
        Refusal {
            case: "a capacity above the group's max size",
            params: vec![p("SubnetIds.member.1", subnets[0].id.clone())],
            group: None,
            capacity: 3,
            expected: "Launching 3 instance(s) would take the AutoScalingGroup: asg-refused \
                       above its max size: 2"
                .to_string(),
        },
        Refusal {
            case: "a retry strategy outside the model's own set",
            params: vec![p("RetryStrategy", "retry-forever")],
            group: None,
            capacity: 1,
            expected: "1 validation error detected: Value 'retry-forever' at 'retryStrategy' \
                       failed to satisfy constraint: Member must satisfy enum value set: \
                       [retry-with-group-configuration, none]"
                .to_string(),
        },
        Refusal {
            case: "no placement for a group with several zones",
            params: Vec::new(),
            group: Some("asg-multi-zone"),
            capacity: 1,
            expected: "Either AvailabilityZones or SubnetIds must be specified for Auto Scaling \
                       groups with multiple Availability Zone configurations"
                .to_string(),
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = case.group.unwrap_or("asg-refused");
        let mut params = case.params.clone();
        params.push(p("AutoScalingGroupName", name));
        params.push(p("RequestedCapacity", case.capacity.to_string()));
        params.push(p("ClientToken", format!("refused-{index}")));
        let (status, body) = query(&router, "LaunchInstances", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<Code>ValidationError</Code>"),
            "case {:?}: expected the ValidationError query error code, got {body}",
            case.case
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            case.expected,
            "case {:?}: expected the sentence the broken rule states, got {body}",
            case.case
        );
        // A refused launch placed nothing, in either group.
        for group in ["asg-refused", "asg-multi-zone"] {
            assert!(
                group_instance_ids(&router, group).await.is_empty(),
                "case {:?}: a refused launch leaves {group} without instances",
                case.case
            );
            assert_eq!(
                group_desired_capacity(&router, group).await,
                "0",
                "case {:?}: a refused launch leaves {group}'s capacity alone",
                case.case
            );
        }
    }
}

/// A launch that could place NOTHING reports the error of the placement it was asked for —
/// never an instance that does not exist.
///
/// "If you suspend `Launch`, your Auto Scaling group does not add instances"
/// (`SuspendProcesses`' own documentation), so a group that has suspended it places none:
/// the answer carries the error, the group gains no instance and no capacity, and the failed
/// scaling activity the launch recorded states the same reason.
#[tokio::test]
async fn a_launch_that_placed_nothing_reports_why() {
    const EXPECTED: &str = "The Launch process of the Auto Scaling group is suspended, so no \
                            instance was added. Launching EC2 instance failed.";

    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_launch_template(&router, TEMPLATE).await;
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "asg-suspended"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "4"),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");
    let (status, body) = query(
        &router,
        "SuspendProcesses",
        &[
            ("AutoScalingGroupName", "asg-suspended"),
            ("ScalingProcesses.member.1", "Launch"),
        ],
    )
    .await;
    assert_eq!(status, 200, "SuspendProcesses: got {status}: {body}");

    let (status, body) = query(
        &router,
        "LaunchInstances",
        &[
            ("AutoScalingGroupName", "asg-suspended"),
            ("RequestedCapacity", "2"),
            ("ClientToken", "suspended-launch"),
            ("SubnetIds.member.1", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "LaunchInstances: got {status}: {body}");
    assert!(
        collections(&body).is_empty(),
        "a launch that placed nothing reports no instance, got {body}"
    );
    assert_eq!(
        tag_value(&body, "ErrorCode"),
        "ValidationError",
        "the error names what stopped the launch, got {body}"
    );
    assert_eq!(
        tag_value(&body, "ErrorMessage"),
        EXPECTED,
        "the error states why nothing was placed, got {body}"
    );
    assert_eq!(
        tag_value(&body, "SubnetId"),
        subnets[0].id,
        "the error is about the placement the launch was asked for, got {body}"
    );
    assert_eq!(
        tag_value(&body, "InstanceType"),
        TEMPLATE_INSTANCE_TYPE,
        "the error names the type the group would have launched, got {body}"
    );
    assert!(
        group_instance_ids(&router, "asg-suspended").await.is_empty(),
        "a launch that placed nothing leaves the group without instances"
    );
    assert_eq!(
        group_desired_capacity(&router, "asg-suspended").await,
        "0",
        "a launch that placed nothing raises the group's capacity by nothing"
    );

    // The activity log states the same reason the answer did.
    let (status, body) = query(
        &router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", "asg-suspended")],
    )
    .await;
    assert_eq!(status, 200, "DescribeScalingActivities: got {status}: {body}");
    assert_eq!(
        tag_values(&body, "StatusMessage"),
        vec![EXPECTED.to_string(), EXPECTED.to_string()],
        "each unit of capacity nothing was placed for recorded its own failed activity, got \
         {body}"
    );
}
