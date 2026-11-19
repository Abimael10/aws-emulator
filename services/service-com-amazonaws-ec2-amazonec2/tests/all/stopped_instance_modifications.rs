// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the family of modifies that reconfigure WHERE or HOW one instance
//! runs — `ModifyInstancePlacement`, `ModifyInstanceNetworkPerformanceOptions`,
//! `ModifyInstanceCapacityReservationAttributes` and the `instanceType` half of
//! `ModifyInstanceAttribute` — driven over the wire the service actually speaks: `ec2Query`,
//! through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test is one sentence: a setting an instance is given when it is PLACED on
//! hardware can only be changed while the instance is STOPPED, so an operation that writes
//! one refuses an instance in any other state — with that operation's own code and sentence,
//! and with nothing written — and from `stopped` it writes the value, which is then what
//! `DescribeInstances` reports.
//!
//! The tables below span what the rule generalises over rather than one operation: the four
//! operations of the family, the attribute each writes, the code each refusal carries
//! (`IncorrectInstanceState`, `InvalidInstanceState`, `InvalidState`) and the member a read
//! must report afterwards. Each successful row is read back through `DescribeInstances`, the
//! one reader of a stored instance, so nothing asserts a value this suite also wrote.

use ec2::state::{
    BANDWIDTH_WEIGHTING_DEFAULT, CAPACITY_RESERVATION_PREFERENCE_OPEN,
    ComAmazonawsEc2Amazonec2State as Ec2State, DEFAULT_INSTANCE_TYPE, TENANCY_DEFAULT,
    TENANCY_HOST, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{child, child_text, document};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the form EC2 issues that this store holds no image for — so an instance
/// launched from it carries the facts of the EBS-backed Linux image this crate publishes.
const AMI: &str = "ami-0123456789abcdef0";

/// A bandwidth weighting that is not the one an instance nothing configured runs with
/// ([`BANDWIDTH_WEIGHTING_DEFAULT`]), so a row that writes it proves the write rather than
/// the default.
const BANDWIDTH_WEIGHTING_VPC: &str = "vpc-1";

/// A `CapacityReservationPreference` that is not the one a launch that named none gets
/// ([`CAPACITY_RESERVATION_PREFERENCE_OPEN`]): "the instance doesn't run in a Capacity
/// Reservation even if one is available" (`CapacityReservationPreference`).
const CAPACITY_RESERVATION_PREFERENCE_NONE: &str = "none";

/// The `CapacityReservationPreference` the one preference a target may accompany is spelled
/// as: "you can only specify `CapacityReservationPreference` and
/// `CapacityReservationTarget` if the `CapacityReservationPreference` is
/// `capacity-reservations-only`" (`CapacityReservationSpecification`).
const CAPACITY_RESERVATION_PREFERENCE_ONLY: &str = "capacity-reservations-only";

/// The all-zero `cr-` id: a value shaped like a Capacity Reservation id that EC2 can never
/// have issued one under, which is the id a client reaches for to name a reservation that
/// does not exist.
const UNKNOWN_CAPACITY_RESERVATION_ID: &str = "cr-00000000000000000";

/// One `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// The fields one call carries beside the action.
type Fields = Vec<(String, String)>;

/// One row of the Capacity Reservation refusal table: the case, the specification fields the
/// call carries, and the code it must be refused with.
type SpecificationRefusal = (&'static str, Fields, &'static str);

/// Launch one instance and answer its id.
async fn launch(case: &str, router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// Look again until `instance_id` reports `wanted`, or a generous deadline passes.
///
/// The CONDITION is looked at again in a loop rather than waited out, so nothing here depends
/// on how fast the host runs; the deadline is a large multiple of the service's own
/// transition span, far enough out that a loaded host is never the reason it fails.
async fn poll_until_state(case: &str, router: &MultiServiceRouter, instance_id: &str, wanted: &str) {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let state = super::ec2_query::described_instance_state(router, REGION, instance_id).await;
        if state == wanted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the instance {instance_id} must reach {wanted:?}, last read {state:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// Stop `instance_id` and look again until it reports `stopped`.
async fn stop(case: &str, router: &MultiServiceRouter, instance_id: &str) {
    let (status, body) = call(
        router,
        REGION,
        "StopInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StopInstances must succeed, got {status} with body {body}"
    );
    poll_until_state(case, router, instance_id, "stopped").await;
}

/// The text of the member at `path` inside the one instance `DescribeInstances` reports for
/// `instance_id`, or `None` when the instance reports no such member.
///
/// Reads the TREE rather than the text, so a member is never read out of a shape that merely
/// shares its name.
async fn reported_member(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
    path: &[&str],
) -> Option<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no reported instance in {body}"));
    let (member, enclosing) = path.split_last()?;
    let mut node = instance;
    for name in enclosing {
        node = child(node, name)?;
    }
    child_text(node, member)
}

/// The Availability Zone the instance was placed in, read off the instance itself.
async fn placement_zone(case: &str, router: &MultiServiceRouter, instance_id: &str) -> String {
    reported_member(case, router, instance_id, &["placement", "availabilityZone"])
        .await
        .unwrap_or_else(|| panic!("case {case:?}: {instance_id} must report a placement zone"))
}

// --- the state table --------------------------------------------------------

/// One row of the state table: one operation of the family, the change it asks for, the
/// refusal a RUNNING instance draws, and where the value lands once the instance is stopped.
struct Row {
    case: &'static str,
    action: &'static str,
    /// The request fields beside `InstanceId`.
    fields: &'static [(&'static str, &'static str)],
    /// The code the refusal carries while the instance is running.
    code: &'static str,
    /// The whole sentence it carries, built from the instance id for the wordings that name
    /// it and fixed for the ones that do not.
    message: fn(&str) -> String,
    /// The member path a read must report the new value at.
    read: &'static [&'static str],
    /// The value that read must report after the modify, and the one it reported before.
    written: &'static str,
    held: &'static str,
}

/// The sentence `ModifyInstanceAttribute` and `ModifyInstanceCapacityReservationAttributes`
/// answer: the instance and the state the attribute can be written from.
fn not_in_the_stopped_state(instance_id: &str) -> String {
    format!("The instance '{instance_id}' is not in the 'stopped' state.")
}

/// The sentence `ModifyInstancePlacement` answers about a PLACEMENT GROUP: the attribute and
/// the state it can be changed from, naming no instance at all.
fn placement_groups_only_when_stopped(_instance_id: &str) -> String {
    "Placement groups can only be changed for instances in the 'stopped' state.".to_string()
}

/// The same sentence for the tenancy an instance runs under.
fn tenancy_only_when_stopped(_instance_id: &str) -> String {
    "Tenancy can only be changed for instances in the 'stopped' state.".to_string()
}

/// The sentence `ModifyInstanceNetworkPerformanceOptions` answers: the instance and the
/// states the operation does admit.
fn not_in_an_allowed_state(instance_id: &str) -> String {
    format!("The instance '{instance_id}' is not in an allowed state: stopped")
}

/// The family, one row per operation and attribute.
const ROWS: &[Row] = &[
    Row {
        case: "the instance type, which only a stopped instance can be given",
        action: "ModifyInstanceAttribute",
        fields: &[("InstanceType.Value", "m5.large")],
        code: "IncorrectInstanceState",
        message: not_in_the_stopped_state,
        read: &["instanceType"],
        written: "m5.large",
        held: DEFAULT_INSTANCE_TYPE,
    },
    Row {
        case: "the tenancy of the hardware the instance runs on",
        action: "ModifyInstancePlacement",
        fields: &[("Tenancy", "dedicated")],
        code: "InvalidInstanceState",
        message: tenancy_only_when_stopped,
        read: &["placement", "tenancy"],
        written: "dedicated",
        held: TENANCY_DEFAULT,
    },
    Row {
        case: "the bandwidth weighting the instance's baseline bandwidth is split by",
        action: "ModifyInstanceNetworkPerformanceOptions",
        fields: &[("BandwidthWeighting", BANDWIDTH_WEIGHTING_VPC)],
        code: "InvalidState",
        message: not_in_an_allowed_state,
        read: &["networkPerformanceOptions", "bandwidthWeighting"],
        written: BANDWIDTH_WEIGHTING_VPC,
        held: BANDWIDTH_WEIGHTING_DEFAULT,
    },
    Row {
        case: "the Capacity Reservation preference the instance runs under",
        action: "ModifyInstanceCapacityReservationAttributes",
        fields: &[(
            "CapacityReservationSpecification.CapacityReservationPreference",
            CAPACITY_RESERVATION_PREFERENCE_NONE,
        )],
        code: "IncorrectInstanceState",
        message: not_in_the_stopped_state,
        read: &[
            "capacityReservationSpecification",
            "capacityReservationPreference",
        ],
        written: CAPACITY_RESERVATION_PREFERENCE_NONE,
        held: CAPACITY_RESERVATION_PREFERENCE_OPEN,
    },
];

/// Rule: every operation of the family refuses a RUNNING instance with its own code and
/// sentence, and the setting keeps the value it had.
#[tokio::test]
async fn a_running_instance_is_refused_and_nothing_is_written() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in ROWS {
        let case = row.case;
        let instance_id = launch(case, &router).await;
        poll_until_state(case, &router, &instance_id, "running").await;

        let mut params = vec![p("InstanceId", &instance_id)];
        params.extend(row.fields.iter().map(|(key, value)| p(key, value)));
        let (status, body) = call(&router, REGION, row.action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {} on a running instance must be refused, got {status} with body \
             {body}",
            row.action
        );
        let code = error_code(&body);
        assert_eq!(
            code, row.code,
            "case {case:?}: expected {}, got {code:?} in {body}",
            row.code
        );
        let message = error_message(&body);
        let expected = (row.message)(&instance_id);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        let held = reported_member(case, &router, &instance_id, row.read).await;
        assert_eq!(
            held.as_deref(),
            Some(row.held),
            "case {case:?}: the refused modify must leave the setting at {:?}, got {held:?}",
            row.held
        );
    }
}

/// Rule: from `stopped`, every operation of the family writes the setting, and that is what a
/// later `DescribeInstances` reports.
#[tokio::test]
async fn a_stopped_instance_takes_the_setting_and_reports_it_back() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in ROWS {
        let case = row.case;
        let instance_id = launch(case, &router).await;
        stop(case, &router, &instance_id).await;

        let mut params = vec![p("InstanceId", &instance_id)];
        params.extend(row.fields.iter().map(|(key, value)| p(key, value)));
        let (status, body) = call(&router, REGION, row.action, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {} on a stopped instance must succeed, got {status} with body \
             {body}",
            row.action
        );

        let reported = reported_member(case, &router, &instance_id, row.read).await;
        assert_eq!(
            reported.as_deref(),
            Some(row.written),
            "case {case:?}: the instance must report {:?} after the modify, got {reported:?}",
            row.written
        );
    }
}

// --- the placement group, host and partition rules --------------------------

/// Create a placement group with `strategy` and answer its name and id.
async fn create_placement_group(
    case: &str,
    router: &MultiServiceRouter,
    group_name: &str,
    extra: &[(&str, &str)],
) -> String {
    let mut params = vec![p("GroupName", group_name)];
    params.extend(extra.iter().map(|(key, value)| p(key, value)));
    let (status, body) = call(router, REGION, "CreatePlacementGroup", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePlacementGroup must succeed, got {status} with body {body}"
    );
    element(&body, "groupId")
        .unwrap_or_else(|| panic!("case {case:?}: CreatePlacementGroup reported no id: {body}"))
}

/// Rule: a placement is written as the request names it — a group by NAME or by the id that
/// names the same group, a partition of a partition group, and the EMPTY name that takes the
/// instance out of its group — and a placement naming a resource the account does not hold is
/// refused with that resource's own code.
#[tokio::test]
async fn a_placement_names_a_group_by_name_or_by_id_and_can_be_cleared() {
    let case = "placement group round trip";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    stop(case, &router, &instance_id).await;

    let spread_id = create_placement_group(case, &router, "spread-group", &[("Strategy", "spread")])
        .await;
    let partition_group = "partition-group";
    create_placement_group(
        case,
        &router,
        partition_group,
        &[("Strategy", "partition"), ("PartitionCount", "3")],
    )
    .await;

    // By NAME.
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[p("InstanceId", &instance_id), p("GroupName", "spread-group")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: naming the group by name must succeed, got {status} with body {body}"
    );
    let reported = reported_member(case, &router, &instance_id, &["placement", "groupName"]).await;
    assert_eq!(
        reported.as_deref(),
        Some("spread-group"),
        "case {case:?}: the instance must report the group it was put in, got {reported:?}"
    );
    let reported_id = reported_member(case, &router, &instance_id, &["placement", "groupId"]).await;
    assert_eq!(
        reported_id.as_deref(),
        Some(spread_id.as_str()),
        "case {case:?}: the reported group id must be the group's own, got {reported_id:?}"
    );

    // By ID: the same group named the other way, moving the instance into the partition
    // group, with a partition of it in the same call.
    let partition_id = {
        let (status, body) = call(
            &router,
            REGION,
            "DescribePlacementGroups",
            &[p("GroupName.1", partition_group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePlacementGroups must succeed, got {status} with body {body}"
        );
        element(&body, "groupId").unwrap_or_else(|| {
            panic!("case {case:?}: the described group reported no id: {body}")
        })
    };
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[
            p("InstanceId", &instance_id),
            p("GroupId", &partition_id),
            p("PartitionNumber", "2"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: naming the group by id must succeed, got {status} with body {body}"
    );
    let reported = reported_member(case, &router, &instance_id, &["placement", "groupName"]).await;
    assert_eq!(
        reported.as_deref(),
        Some(partition_group),
        "case {case:?}: the instance must report the group the id named, got {reported:?}"
    );
    let partition = reported_member(
        case,
        &router,
        &instance_id,
        &["placement", "partitionNumber"],
    )
    .await;
    assert_eq!(
        partition.as_deref(),
        Some("2"),
        "case {case:?}: the instance must report the partition it was placed in, got \
         {partition:?}"
    );

    // The EMPTY name: "to remove an instance from a placement group, specify an empty
    // string" — and a partition is a partition OF a group, so it goes with it.
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[p("InstanceId", &instance_id), p("GroupName", "")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the empty group name must succeed, got {status} with body {body}"
    );
    let reported = reported_member(case, &router, &instance_id, &["placement", "groupName"]).await;
    assert_eq!(
        reported.as_deref(),
        Some(""),
        "case {case:?}: an instance in no group reports the empty name, got {reported:?}"
    );
    let partition = reported_member(
        case,
        &router,
        &instance_id,
        &["placement", "partitionNumber"],
    )
    .await;
    assert_eq!(
        partition, None,
        "case {case:?}: an instance in no group is in no partition, got {partition:?}"
    );
}

/// Rule: the refusal names WHAT cannot be changed, so a request about a PLACEMENT GROUP and
/// one about the tenancy answer different sentences under the one code.
#[tokio::test]
async fn the_refusal_names_the_attribute_the_request_asked_to_change() {
    let case = "the placement sentence";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    poll_until_state(case, &router, &instance_id, "running").await;
    create_placement_group(case, &router, "running-group", &[("Strategy", "cluster")]).await;

    for (label, fields, expected) in [
        (
            "a placement group",
            vec![p("GroupName", "running-group")],
            placement_groups_only_when_stopped(&instance_id),
        ),
        (
            "the tenancy",
            vec![p("Tenancy", "dedicated")],
            tenancy_only_when_stopped(&instance_id),
        ),
    ] {
        let mut params = vec![p("InstanceId", &instance_id)];
        params.extend(fields);
        let (status, body) = call(&router, REGION, "ModifyInstancePlacement", &params).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "InvalidInstanceState",
            "case {label:?}: expected InvalidInstanceState, got {code:?} in {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {label:?}: expected message {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(&instance_id),
            "case {label:?}: this sentence names the attribute rather than the instance, got \
             {message:?}"
        );
    }
}

/// Rule: the caller-chosen bytes the store accounts for move only when the call actually
/// moves them — a placement that names the host resource group's ARN charges it and gives
/// back the one it replaced, and a placement that names any other attribute leaves the
/// accounting exactly where it was.
///
/// The store's byte budget is the emulator's own bound on caller-supplied data it retains, so
/// a call that credited back bytes it never released would let that bound drift.
#[tokio::test]
async fn the_stored_byte_accounting_follows_what_the_call_actually_writes() {
    let case = "stored byte accounting";
    let (router, engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    stop(case, &router, &instance_id).await;
    let arn = "arn:aws:resource-groups:us-west-2:111122223333:group/dedicated-hosts";

    let stored_bytes = || engine.state::<Ec2State>().lock().stored_bytes();

    // Naming the ARN charges exactly its length.
    let before = stored_bytes();
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[
            p("InstanceId", &instance_id),
            p("Tenancy", TENANCY_HOST),
            p("HostResourceGroupArn", arn),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: naming the host resource group must succeed, got {status} with body \
         {body}"
    );
    let charged = stored_bytes();
    assert_eq!(
        charged,
        before + arn.len(),
        "case {case:?}: the ARN's own bytes must be charged once, was {before}"
    );

    // A placement that names ANOTHER attribute leaves the ARN resident, so it gives nothing
    // back.
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[p("InstanceId", &instance_id), p("Affinity", "host")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: naming the affinity must succeed, got {status} with body {body}"
    );
    let after = stored_bytes();
    assert_eq!(
        after, charged,
        "case {case:?}: a call that writes no ARN must not move the accounting, was {charged}"
    );
    let reported = reported_member(
        case,
        &router,
        &instance_id,
        &["placement", "hostResourceGroupArn"],
    )
    .await;
    assert_eq!(
        reported.as_deref(),
        Some(arn),
        "case {case:?}: the ARN must still be the one the instance holds, got {reported:?}"
    );

    // Replacing it with a LONGER one charges the difference and nothing else.
    let longer = "arn:aws:resource-groups:us-west-2:111122223333:group/dedicated-hosts-two";
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[
            p("InstanceId", &instance_id),
            p("HostResourceGroupArn", longer),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: replacing the ARN must succeed, got {status} with body {body}"
    );
    let replaced = stored_bytes();
    assert_eq!(
        replaced,
        before + longer.len(),
        "case {case:?}: only the ARN the instance now holds may be charged, was {after}"
    );
}

/// One refusal row of the placement table: the fields beside the instance id, and the status
/// and code the request draws.
struct PlacementRefusal {
    case: &'static str,
    fields: &'static [(&'static str, &'static str)],
    code: &'static str,
}

/// Rule: every resource a placement names is resolved against the store it lives in, and a
/// request that names none of them, names a group twice, or asks for a change the instance's
/// tenancy does not admit, is refused rather than written.
#[tokio::test]
async fn a_placement_that_cannot_be_carried_out_is_refused() {
    let case = "placement refusals";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    stop(case, &router, &instance_id).await;
    create_placement_group(case, &router, "cluster-group", &[("Strategy", "cluster")]).await;

    let rows: &[PlacementRefusal] = &[
        PlacementRefusal {
            case: "no attribute at all",
            fields: &[],
            code: "MissingParameter",
        },
        PlacementRefusal {
            case: "a placement group the account does not hold",
            fields: &[("GroupName", "no-such-group")],
            code: "InvalidPlacementGroup.Unknown",
        },
        PlacementRefusal {
            case: "a placement group id the account does not hold",
            fields: &[("GroupId", "pg-00000000000000000")],
            code: "InvalidPlacementGroup.Unknown",
        },
        PlacementRefusal {
            case: "a group named twice, by name and by id",
            fields: &[("GroupName", "cluster-group"), ("GroupId", "pg-0123456789abcdef0")],
            code: "InvalidParameterCombination",
        },
        PlacementRefusal {
            case: "a Dedicated Host the account does not hold",
            fields: &[("HostId", "h-0123456789abcdef0")],
            code: "InvalidHostID.NotFound",
        },
        PlacementRefusal {
            case: "a partition of a group whose strategy is not partition",
            fields: &[("GroupName", "cluster-group"), ("PartitionNumber", "1")],
            code: "InvalidParameterValue",
        },
        PlacementRefusal {
            case: "a host resource group without the tenancy it requires",
            fields: &[(
                "HostResourceGroupArn",
                "arn:aws:resource-groups:us-west-2:111122223333:group/hosts",
            )],
            code: "InvalidParameterCombination",
        },
    ];

    for row in rows {
        let mut params = vec![p("InstanceId", &instance_id)];
        params.extend(row.fields.iter().map(|(key, value)| p(key, value)));
        let (status, body) = call(&router, REGION, "ModifyInstancePlacement", &params).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with body {body}",
            row.case
        );
        let code = error_code(&body);
        assert_eq!(
            code, row.code,
            "case {:?}: expected {}, got {code:?} in {body}",
            row.case, row.code
        );
    }
}

/// Rule: "you can't change the tenancy from host to dedicated or default" — an instance on a
/// host keeps that tenancy, and the attempt answers `InvalidRequest`.
#[tokio::test]
async fn a_tenancy_of_host_cannot_be_changed_away() {
    let case = "tenancy host to default";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    stop(case, &router, &instance_id).await;

    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstancePlacement",
        &[p("InstanceId", &instance_id), p("Tenancy", TENANCY_HOST)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: taking the host tenancy must succeed, got {status} with body {body}"
    );

    for away in [TENANCY_DEFAULT, "dedicated"] {
        let (status, body) = call(
            &router,
            REGION,
            "ModifyInstancePlacement",
            &[p("InstanceId", &instance_id), p("Tenancy", away)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: moving to {away} must be refused, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "InvalidRequest",
            "case {case:?}: expected InvalidRequest moving to {away}, got {code:?} in {body}"
        );
        let reported = reported_member(case, &router, &instance_id, &["placement", "tenancy"]).await;
        assert_eq!(
            reported.as_deref(),
            Some(TENANCY_HOST),
            "case {case:?}: the tenancy must still be {TENANCY_HOST}, got {reported:?}"
        );
    }
}

// --- the bandwidth weighting a request may ask for --------------------------

/// Rule: the weighting a request names is resolved against the catalog of its own modelled
/// enum, so every published word is accepted and written, and a word outside it is refused
/// `InvalidParameterValue` in the sentence EC2 words for that setting — whichever side of
/// dispatch refuses it.
#[tokio::test]
async fn every_published_bandwidth_weighting_is_written_and_an_unpublished_one_is_refused() {
    let case = "bandwidth weightings";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    stop(case, &router, &instance_id).await;

    // The catalog the model publishes
    // (`smithy_query(query="shape",shape="InstanceBandwidthWeighting")`): every member, so no
    // row is the specification.
    for weighting in ec2::ops::types::INSTANCE_BANDWIDTH_WEIGHTING_VALUES {
        let (status, body) = call(
            &router,
            REGION,
            "ModifyInstanceNetworkPerformanceOptions",
            &[
                p("InstanceId", &instance_id),
                p("BandwidthWeighting", weighting),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {weighting} must be accepted, got {status} with body {body}"
        );
        let answered = element(&body, "bandwidthWeighting");
        assert_eq!(
            answered.as_deref(),
            Some(*weighting),
            "case {case:?}: the answer must carry the weighting it wrote, got {answered:?}"
        );
        let reported = reported_member(
            case,
            &router,
            &instance_id,
            &["networkPerformanceOptions", "bandwidthWeighting"],
        )
        .await;
        assert_eq!(
            reported.as_deref(),
            Some(*weighting),
            "case {case:?}: the instance must report {weighting}, got {reported:?}"
        );
    }

    let unpublished = "not-a-real-weighting";
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstanceNetworkPerformanceOptions",
        &[
            p("InstanceId", &instance_id),
            p("BandwidthWeighting", unpublished),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: {unpublished} must be refused, got {status} with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, "InvalidParameterValue",
        "case {case:?}: expected InvalidParameterValue, got {code:?} in {body}"
    );
    let message = error_message(&body);
    let expected = format!(
        "\"{unpublished}\" is not a valid value for bandwidth weighting. Specify a valid \
         value and try again."
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected message {expected:?}, got {message:?}"
    );
}

// --- the Capacity Reservation an instance runs in ---------------------------

/// Rule: an instance pointed AT a Capacity Reservation runs in it — the instance reports the
/// reservation and the reservation has that much less capacity available — and a preference
/// that says it runs in no particular reservation takes it back out again.
#[tokio::test]
async fn targeting_a_capacity_reservation_moves_the_reserved_capacity() {
    let case = "capacity reservation targeting";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    let zone = placement_zone(case, &router, &instance_id).await;
    stop(case, &router, &instance_id).await;

    let (status, body) = call(
        &router,
        REGION,
        "CreateCapacityReservation",
        &[
            p("InstanceType", DEFAULT_INSTANCE_TYPE),
            p("InstancePlatform", "Linux/UNIX"),
            p("AvailabilityZone", &zone),
            p("InstanceCount", "2"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateCapacityReservation must succeed, got {status} with body {body}"
    );
    let reservation_id = element(&body, "capacityReservationId").unwrap_or_else(|| {
        panic!("case {case:?}: the create reported no reservation id: {body}")
    });

    // What the reservation has available before anything runs in it, read off its own report.
    let available = |body: &str| {
        element(body, "availableInstanceCount")
            .and_then(|count| count.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no availableInstanceCount in {body}"))
    };
    let (status, body) = call(
        &router,
        REGION,
        "DescribeCapacityReservations",
        &[p("CapacityReservationId.1", &reservation_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeCapacityReservations must succeed, got {status} with body {body}"
    );
    let before = available(&body);

    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstanceCapacityReservationAttributes",
        &[
            p("InstanceId", &instance_id),
            p(
                "CapacityReservationSpecification.CapacityReservationTarget.CapacityReservationId",
                &reservation_id,
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: targeting the reservation must succeed, got {status} with body {body}"
    );

    let reported = reported_member(case, &router, &instance_id, &["capacityReservationId"]).await;
    assert_eq!(
        reported.as_deref(),
        Some(reservation_id.as_str()),
        "case {case:?}: the instance must report the reservation it runs in, got {reported:?}"
    );
    let preference = reported_member(
        case,
        &router,
        &instance_id,
        &[
            "capacityReservationSpecification",
            "capacityReservationPreference",
        ],
    )
    .await;
    assert_eq!(
        preference.as_deref(),
        Some(CAPACITY_RESERVATION_PREFERENCE_ONLY),
        "case {case:?}: an instance that targets one reservation runs only in a reservation, \
         got {preference:?}"
    );

    let (status, body) = call(
        &router,
        REGION,
        "DescribeCapacityReservations",
        &[p("CapacityReservationId.1", &reservation_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeCapacityReservations must succeed, got {status} with body {body}"
    );
    let taken = available(&body);
    assert_eq!(
        taken,
        before - 1,
        "case {case:?}: the reservation must have one instance's capacity less available \
         than the {before} it started with, got {taken}"
    );

    // Taking the instance back out gives the capacity back.
    let (status, body) = call(
        &router,
        REGION,
        "ModifyInstanceCapacityReservationAttributes",
        &[
            p("InstanceId", &instance_id),
            p(
                "CapacityReservationSpecification.CapacityReservationPreference",
                CAPACITY_RESERVATION_PREFERENCE_NONE,
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setting the preference to none must succeed, got {status} with body \
         {body}"
    );
    let reported = reported_member(case, &router, &instance_id, &["capacityReservationId"]).await;
    assert_eq!(
        reported, None,
        "case {case:?}: an instance that runs in no reservation reports none, got {reported:?}"
    );
    let (status, body) = call(
        &router,
        REGION,
        "DescribeCapacityReservations",
        &[p("CapacityReservationId.1", &reservation_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeCapacityReservations must succeed, got {status} with body {body}"
    );
    let given_back = available(&body);
    assert_eq!(
        given_back, before,
        "case {case:?}: the reservation must have its {before} back, got {given_back}"
    );
}

/// Rule: a Capacity Reservation specification that cannot be carried out is refused rather
/// than written — a target the account holds no reservation for, a preference that may not
/// accompany a target, and a specification that names neither member.
#[tokio::test]
async fn a_capacity_reservation_specification_that_cannot_hold_is_refused() {
    let case = "capacity reservation refusals";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    stop(case, &router, &instance_id).await;

    let target = |id: &str| {
        p(
            "CapacityReservationSpecification.CapacityReservationTarget.CapacityReservationId",
            id,
        )
    };
    let rows: Vec<SpecificationRefusal> = vec![
        (
            "a reservation id the account holds none under",
            vec![target(UNKNOWN_CAPACITY_RESERVATION_ID)],
            "InvalidCapacityReservationId.NotFound",
        ),
        (
            "a value that is no reservation id at all",
            vec![target("reservation-7")],
            "InvalidCapacityReservationId.Malformed",
        ),
        (
            "a target beside a preference that may not accompany one",
            vec![
                target(UNKNOWN_CAPACITY_RESERVATION_ID),
                p(
                    "CapacityReservationSpecification.CapacityReservationPreference",
                    CAPACITY_RESERVATION_PREFERENCE_OPEN,
                ),
            ],
            "InvalidParameterCombination",
        ),
        (
            "a specification that names neither member",
            Vec::new(),
            "MissingParameter",
        ),
    ];

    for (label, fields, expected) in rows {
        let mut params = vec![p("InstanceId", &instance_id)];
        params.extend(fields);
        let (status, body) = call(
            &router,
            REGION,
            "ModifyInstanceCapacityReservationAttributes",
            &params,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, expected,
            "case {label:?}: expected {expected}, got {code:?} in {body}"
        );
        let preference = reported_member(
            label,
            &router,
            &instance_id,
            &[
                "capacityReservationSpecification",
                "capacityReservationPreference",
            ],
        )
        .await;
        assert_eq!(
            preference.as_deref(),
            Some(CAPACITY_RESERVATION_PREFERENCE_OPEN),
            "case {label:?}: the refused call must leave the preference as it was, got \
             {preference:?}"
        );
    }
}
