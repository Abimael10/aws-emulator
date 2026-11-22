// SPDX-License-Identifier: CC0-1.0

//! The PARTITION a launch places an instance in, driven over the wire the service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a member a launch carries is a member the instance it mints REPORTS, so
//! `Placement.PartitionNumber` — "the number of the partition that the instance is in. Valid
//! only if the placement group strategy is set to `partition`" — is kept on the instance and
//! answered by `DescribeInstances`, both as the member itself and through the filter that
//! selects on it. A number that names no partition of the group the launch places the instance
//! in refuses the launch instead, so nothing is accepted and then dropped.
//!
//! The rows span what that generalises over: how many partitions the group was created with
//! (the documented default, a count in between, and the published maximum), WHICH partition of
//! it the launch asks for (the first, the last and one in the middle) and how many instances one
//! launch places there; and, for the refusals, each bound of the admitted range plus one past it
//! and every placement that has no partition at all to number.

use ec2::state::{
    DEFAULT_PLACEMENT_GROUP_PARTITION_COUNT, MAX_PLACEMENT_GROUP_PARTITION_COUNT,
    MIN_PLACEMENT_GROUP_PARTITION_COUNT,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, filters};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for: the emulator's own default Region, whose default
/// VPC and subnets a fresh account already holds.
const REGION: &str = "us-east-1";

/// The image every launch names.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type every launch names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The code EC2 answers for a parameter value it cannot serve.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// One `action` call signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// The same call, requiring HTTP 200, and the body it answered.
async fn invoke_ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One placement group of `strategy`, with `extra` form fields, as the name it was created
/// under.
async fn placement_group(
    case: &str,
    router: &MultiServiceRouter,
    group_name: &str,
    strategy: &str,
    extra: &[Param],
) -> String {
    let mut params = vec![
        param("GroupName", group_name),
        param("Strategy", strategy),
    ];
    params.extend_from_slice(extra);
    invoke_ok(case, router, "CreatePlacementGroup", &params).await;
    group_name.to_string()
}

/// One `RunInstances` of `count` instances placed by `placement`, as its status and body.
async fn launch(
    router: &MultiServiceRouter,
    count: i32,
    placement: &[Param],
) -> (u16, String) {
    let mut params = vec![
        param("ImageId", AMI),
        param("InstanceType", INSTANCE_TYPE),
        param("MinCount", &count.to_string()),
        param("MaxCount", &count.to_string()),
    ];
    params.extend_from_slice(placement);
    invoke(router, "RunInstances", &params).await
}

/// The `placement` member `name` that `DescribeInstances` reports for `instance_id`, or `None`
/// when the instance reports none.
async fn reported_placement(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
    name: &str,
) -> Option<String> {
    let body = invoke_ok(
        case,
        router,
        "DescribeInstances",
        &super::ec2_query::indexed("InstanceId", &[instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "instanceId").as_deref() == Some(instance_id)
                && child(*node, "placement").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance {instance_id} in {body}"));
    child(instance, "placement").and_then(|placement| child_text(placement, name))
}

/// One row of the launch table: a partition group, the partition of it a launch asks for and
/// how many instances that launch places.
struct Row {
    case: &'static str,
    group_name: &'static str,
    /// The group's `PartitionCount`, or `None` to leave the create's own default.
    partitions: Option<i32>,
    partition_number: i32,
    instances: i32,
}

/// An instance launched into a partition of a partition placement group reports that partition.
#[tokio::test]
async fn a_launch_into_a_partition_reports_the_partition_it_asked_for() {
    let rows = [
        Row {
            case: "the first partition of a group created with the default count",
            group_name: "defaulted-partitions",
            partitions: None,
            partition_number: MIN_PLACEMENT_GROUP_PARTITION_COUNT,
            instances: 1,
        },
        Row {
            case: "the last partition of a group created with the default count",
            group_name: "defaulted-partitions-last",
            partitions: None,
            partition_number: DEFAULT_PLACEMENT_GROUP_PARTITION_COUNT,
            instances: 2,
        },
        Row {
            case: "a partition in the middle of a group of five",
            group_name: "five-partitions",
            partitions: Some(5),
            partition_number: 3,
            instances: 1,
        },
        Row {
            case: "the last partition of a group at the published maximum",
            group_name: "seven-partitions",
            partitions: Some(MAX_PLACEMENT_GROUP_PARTITION_COUNT),
            partition_number: MAX_PLACEMENT_GROUP_PARTITION_COUNT,
            instances: 3,
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    for row in &rows {
        let Row {
            case,
            group_name,
            partitions,
            partition_number,
            instances,
        } = row;
        let extra: Vec<Param> = partitions
            .map(|count| vec![param("PartitionCount", &count.to_string())])
            .unwrap_or_default();
        let group = placement_group(case, &router, group_name, "partition", &extra).await;

        let (status, body) = launch(
            &router,
            *instances,
            &[
                param("Placement.GroupName", &group),
                param("Placement.PartitionNumber", &partition_number.to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the launch must succeed, got {status} with body {body}"
        );
        let launched = elements(&body, "instanceId");
        assert_eq!(
            launched.len(),
            usize::try_from(*instances).unwrap_or_default(),
            "case {case:?}: expected {instances} instances, got {launched:?}"
        );

        // EVERY instance of the launch is in the partition the launch named, and in the group
        // that partition belongs to — read back through the describe rather than from the
        // launch's own answer.
        for instance_id in &launched {
            let reported = reported_placement(case, &router, instance_id, "partitionNumber").await;
            assert_eq!(
                reported.as_deref(),
                Some(partition_number.to_string().as_str()),
                "case {case:?}: expected {instance_id} in partition {partition_number}, got \
                 {reported:?}"
            );
            let reported_group =
                reported_placement(case, &router, instance_id, "groupName").await;
            assert_eq!(
                reported_group.as_deref(),
                Some(group.as_str()),
                "case {case:?}: expected {instance_id} in the group {group}, got \
                 {reported_group:?}"
            );
        }

        // The member is reported, so the filter that selects on it finds exactly those
        // instances: the same fact read the other way round.
        let body = invoke_ok(
            case,
            &router,
            "DescribeInstances",
            &filters(&[
                ("placement-group-name", &[group.as_str()]),
                (
                    "placement-partition-number",
                    &[partition_number.to_string().as_str()],
                ),
            ]),
        )
        .await;
        let mut selected = elements(&body, "instanceId");
        selected.sort();
        let mut expected = launched.clone();
        expected.sort();
        assert_eq!(
            selected, expected,
            "case {case:?}: the partition filter must select the instances of that partition"
        );
    }
}

/// A partition number that names no partition of the group the instance would be in refuses the
/// launch, and nothing is launched.
#[tokio::test]
async fn a_partition_no_group_of_the_launch_has_refuses_it() {
    let case = "a partition that does not exist";
    let (router, _engine) = super::ec2_query::emulator();
    let partitioned = placement_group(
        case,
        &router,
        "three-partitions",
        "partition",
        &[param("PartitionCount", "3")],
    )
    .await;
    let clustered = placement_group(case, &router, "a-cluster", "cluster", &[]).await;
    let spread = placement_group(case, &router, "a-spread", "spread", &[]).await;

    // (the case, the placement the launch asks for)
    let rows: Vec<(String, Vec<Param>)> = vec![
        (
            "one below the first partition".to_string(),
            vec![
                param("Placement.GroupName", &partitioned),
                param(
                    "Placement.PartitionNumber",
                    &(MIN_PLACEMENT_GROUP_PARTITION_COUNT - 1).to_string(),
                ),
            ],
        ),
        (
            "one above the group's own count".to_string(),
            vec![
                param("Placement.GroupName", &partitioned),
                param("Placement.PartitionNumber", "4"),
            ],
        ),
        (
            "one above the published maximum any group could have".to_string(),
            vec![
                param("Placement.GroupName", &partitioned),
                param(
                    "Placement.PartitionNumber",
                    &(MAX_PLACEMENT_GROUP_PARTITION_COUNT + 1).to_string(),
                ),
            ],
        ),
        (
            "a partition of a cluster group, which has none".to_string(),
            vec![
                param("Placement.GroupName", &clustered),
                param("Placement.PartitionNumber", "1"),
            ],
        ),
        (
            "a partition of a spread group, which has none".to_string(),
            vec![
                param("Placement.GroupName", &spread),
                param("Placement.PartitionNumber", "1"),
            ],
        ),
        (
            "a partition with no placement group at all".to_string(),
            vec![param("Placement.PartitionNumber", "1")],
        ),
    ];

    for (name, placement) in &rows {
        let before = elements(&invoke_ok(name, &router, "DescribeInstances", &[]).await, "instanceId");
        let (status, body) = launch(&router, 1, placement).await;
        assert_eq!(
            status, 400,
            "case {name:?}: the launch must be refused, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_PARAMETER_VALUE,
            "case {name:?}: expected the code {INVALID_PARAMETER_VALUE:?}, got {code:?} in {body}"
        );
        let after = elements(&invoke_ok(name, &router, "DescribeInstances", &[]).await, "instanceId");
        assert_eq!(
            after, before,
            "case {name:?}: a refused launch must mint no instance"
        );
    }

    // The control: the same launch into a partition the group DOES have succeeds, so the
    // refusals are about the number rather than about the member.
    let (status, body) = launch(
        &router,
        1,
        &[
            param("Placement.GroupName", &partitioned),
            param("Placement.PartitionNumber", "3"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a partition the group has must be launchable into, got {status} with \
         body {body}"
    );
    let instance_id = element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: the launch must report its instance: {body}"));
    assert_eq!(
        reported_placement(case, &router, &instance_id, "partitionNumber")
            .await
            .as_deref(),
        Some("3"),
        "case {case:?}: the instance must report the partition the launch asked for"
    );
}
