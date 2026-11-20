// SPDX-License-Identifier: CC0-1.0

//! `AttachInstances` brings the account's own Amazon EC2 instances into an Auto Scaling
//! group.
//!
//! The rule under test: an attach is TWO facts about the group and nothing else — each
//! instance the request names becomes an instance OF the group, reported `Pending` until it
//! is in service, and the group's desired capacity rises by the number attached, so the
//! group launches no replacement for any of them ("Amazon EC2 Auto Scaling increases the
//! desired capacity of the group by the number of instances being attached",
//! `smithy_query(query="doc",shape="AttachInstances")`). The instances are the account's
//! OWN Amazon EC2 instances, so an attach may only take one Amazon EC2 holds, running, that
//! no Auto Scaling group holds already and that is in one of the group's own Availability
//! Zones — and a request naming an id that fails any of those, or one whose capacity would
//! pass the group's `MaxSize` ("if the number of instances being attached plus the desired
//! capacity of the group exceeds the maximum size of the group, the operation fails"), is
//! refused WHOLE: nothing is attached and the group's capacity does not move.
//!
//! The first table spans what the rule generalises over for a successful attach: how many
//! instances one call takes, an id named twice, a group that already holds an instance of
//! its own, and both ways a group states where its instances go. The second spans the
//! refusals, one row per reason an attach cannot be made, each asserting the WHOLE message
//! and that the group is left exactly as it was; it also carries a row the service must
//! ACCEPT, so no row can pass by refusing everything.
//!
//! Every row is driven over the wire — awsQuery for Auto Scaling, ec2Query for Amazon EC2 —
//! against the services on ONE engine, so the instances these assertions resolve against
//! are the account's own. Nothing here waits for a span: every wait looks AGAIN until the
//! condition the clock owes holds, or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, p, query, seed_subnets, tag_value,
    tag_values, xml_unescape, SeededSubnet,
};

/// How long a case here waits for a condition the clock owes before calling it a failure.
///
/// A BACKSTOP, not a wait: every helper below looks again until the condition holds, so
/// this only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The AMI a standalone instance of this suite is launched from. An AMI id is all a launch
/// has to name, and this is one of the issued form.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The `LifecycleState` an instance of a group reaches once it is in service, and the
/// transient one it is reported in on the way there.
const IN_SERVICE: &str = "InService";
const PENDING: &str = "Pending";

/// The whole message an attach naming a group this service does not hold answers with: the
/// fixed prefix, then the detail clause THIS operation reports, which labels the resource
/// and names the group the request asked for.
///
/// THE ONE COPY for this suite; `attach_instances_absent_group.rs` drives the same sentence
/// over a table of names.
pub(crate) fn absent_group_message(name: &str) -> String {
    format!("AutoScalingGroup name not found - AutoScalingGroup: {name} not found")
}

/// Where a group in this suite places its instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// `AvailabilityZones`, naming the zone the seeded subnet is in.
    Zone,
    /// `VPCZoneIdentifier`, naming that subnet.
    Subnet,
}

/// The status, the wire code and the unescaped message of one refusal.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// Create one Auto Scaling group over the wire, from a launch template the account holds,
/// and assert the create succeeded.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    max_size: i32,
    desired_capacity: i32,
    placement: Placement,
    subnet: &SeededSubnet,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired_capacity.to_string()),
    ];
    match placement {
        Placement::Zone => params.push(p("AvailabilityZones.member.1", subnet.zone.clone())),
        Placement::Subnet => params.push(p("VPCZoneIdentifier", subnet.id.clone())),
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected HTTP 200, got {status} with {body:?}"
    );
}

/// Launch one standalone Amazon EC2 instance into `subnet_id` over Amazon EC2's own wire
/// and answer its id, once Amazon EC2 reports it `running`.
///
/// An instance Amazon EC2 has just launched is `pending` and reaches `running` on EC2's own
/// clock, so this looks again until EC2 reports it up rather than waiting for a span.
async fn run_instance(router: &MultiServiceRouter, subnet_id: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "RunInstances",
        &[
            p("ImageId", IMAGE_ID),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", subnet_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "RunInstances expected HTTP 200, got {status} with {body:?}"
    );
    let instance_id = tag_value(&body, "instanceId");
    assert!(
        !instance_id.is_empty(),
        "RunInstances must report its instance, got {body}"
    );
    wait_for_ec2_state(router, &instance_id, "running").await;
    instance_id
}

/// What Amazon EC2 reports one instance's state as, read from a `DescribeInstances` that
/// named exactly that instance. `""` when Amazon EC2 resolves the id to nothing.
async fn ec2_instance_state(router: &MultiServiceRouter, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    if status != 200 {
        return String::new();
    }
    tag_value(&tag_value(&body, "instanceState"), "name")
}

/// Look again until Amazon EC2 reports `instance_id` in `expected`.
async fn wait_for_ec2_state(router: &MultiServiceRouter, instance_id: &str, expected: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = ec2_instance_state(router, instance_id).await;
        if state == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected Amazon EC2 to report {instance_id} {expected}, got {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The tags Amazon EC2 holds on one instance, as `key=value` pairs.
async fn ec2_instance_tags(router: &MultiServiceRouter, instance_id: &str) -> Vec<String> {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "DescribeInstances for {instance_id} expected HTTP 200, got {status} with {body:?}"
    );
    let keys = tag_values(&body, "key");
    let values = tag_values(&body, "value");
    keys.into_iter()
        .zip(values)
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// What `DescribeAutoScalingGroups` answers for one group.
async fn describe(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} expected HTTP 200, got {status} with {body:?}"
    );
    body
}

/// Look again until the group reports exactly `wanted` instances, every one of them in
/// service, and answer their ids with the body they were read from.
async fn settled_instances(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> (Vec<String>, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        if ids.len() == wanted && states.iter().all(|state| state == IN_SERVICE) {
            return (ids, body);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: group {name} never reported {wanted} instance(s) in service; last \
             read {ids:?} in {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Attach the instances `ids` names to `name` over the wire.
async fn attach(router: &MultiServiceRouter, name: &str, ids: &[String]) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, id) in ids.iter().enumerate() {
        params.push(p(&format!("InstanceIds.member.{}", index + 1), id.clone()));
    }
    query(router, "AttachInstances", &borrowed(&params)).await
}

/// The `Description` of every scaling activity the group has recorded.
async fn activity_descriptions(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeScalingActivities for {name} expected HTTP 200, got {status} with {body:?}"
    );
    tag_values(&body, "Description")
}

/// One row of the successful-attach table.
struct AttachRow {
    case: &'static str,
    /// The desired capacity the group is created with, which is how many instances of its
    /// own it launches before anything is attached.
    launched: usize,
    /// How many standalone instances the case attaches.
    attaching: usize,
    /// Whether the request names the first of them TWICE.
    duplicate: bool,
    /// Where the group places its instances.
    placement: Placement,
}

/// A successful attach gives the group the instances it named and raises its desired
/// capacity by how many it took, launching no replacement for any of them.
#[tokio::test]
async fn attaching_instances_gives_the_group_the_instances_and_raises_its_capacity() {
    let rows = [
        AttachRow {
            case: "one instance into an empty group",
            launched: 0,
            attaching: 1,
            duplicate: false,
            placement: Placement::Zone,
        },
        AttachRow {
            case: "two instances in one call",
            launched: 0,
            attaching: 2,
            duplicate: false,
            placement: Placement::Zone,
        },
        AttachRow {
            case: "the same instance named twice in one call",
            launched: 0,
            attaching: 1,
            duplicate: true,
            placement: Placement::Zone,
        },
        AttachRow {
            case: "one instance into a group that already launched one of its own",
            launched: 1,
            attaching: 1,
            duplicate: false,
            placement: Placement::Zone,
        },
        AttachRow {
            case: "one instance into a group placed by subnet",
            launched: 0,
            attaching: 1,
            duplicate: false,
            placement: Placement::Subnet,
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let subnet = subnets.first().expect("one seeded subnet").clone();
        create_launch_template(&router, "lt-attach").await;
        let name = "asg-attach";
        create_group(
            &router,
            name,
            "lt-attach",
            4,
            i32::try_from(row.launched).expect("a small capacity"),
            row.placement,
            &subnet,
        )
        .await;
        // The group's OWN instances first, so a case that attaches beside them is read
        // against a group that has already settled.
        let (own, _) = settled_instances(&router, case, name, row.launched).await;

        let mut attaching: Vec<String> = Vec::new();
        for _ in 0..row.attaching {
            attaching.push(run_instance(&router, &subnet.id).await);
        }
        let mut named = attaching.clone();
        if row.duplicate {
            named.push(
                attaching
                    .first()
                    .expect("a case that repeats an id attaches at least one")
                    .clone(),
            );
        }

        let (status, body) = attach(&router, name, &named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: AttachInstances expected HTTP 200, got {status} with {body:?}"
        );

        let expected_instances = row.launched + row.attaching;
        let (reported, settled) = settled_instances(&router, case, name, expected_instances).await;
        for instance_id in &attaching {
            assert!(
                reported.contains(instance_id),
                "case {case:?}: the group must report the attached instance {instance_id}, got \
                 {reported:?}"
            );
        }
        for instance_id in &own {
            assert!(
                reported.contains(instance_id),
                "case {case:?}: the group must still report its own instance {instance_id}, got \
                 {reported:?}"
            );
        }
        let capacity = tag_value(&settled, "DesiredCapacity");
        assert_eq!(
            capacity,
            expected_instances.to_string(),
            "case {case:?}: the desired capacity must rise by the {} instance(s) attached, got \
             {capacity:?} in {settled}",
            row.attaching
        );

        // Each attached instance is the account's own Amazon EC2 instance still: the attach
        // neither replaced nor terminated it, and Amazon EC2 now reports the group it is
        // part of where a client reads an instance's tags.
        let group_tag = format!(
            "{}={name}",
            autoscaling::state::ec2_instances::GROUP_NAME_TAG
        );
        for instance_id in &attaching {
            let state = ec2_instance_state(&router, instance_id).await;
            assert_eq!(
                state, "running",
                "case {case:?}: Amazon EC2 must still report {instance_id} running, got {state:?}"
            );
            let tags = ec2_instance_tags(&router, instance_id).await;
            assert!(
                tags.contains(&group_tag),
                "case {case:?}: the attached instance {instance_id} must carry {group_tag:?}, got \
                 {tags:?}"
            );
        }

        // One scaling activity per instance attached, naming that instance.
        let descriptions = activity_descriptions(&router, name).await;
        for instance_id in &attaching {
            let expected = format!("Attaching EC2 instance: {instance_id}");
            assert_eq!(
                descriptions
                    .iter()
                    .filter(|description| *description == &expected)
                    .count(),
                1,
                "case {case:?}: exactly one activity must report {expected:?}, got \
                 {descriptions:?}"
            );
        }
    }
}

/// An attached instance is reported in the transient state every instance coming into
/// service is reported in before it settles.
#[tokio::test]
async fn an_attached_instance_is_reported_pending_before_it_is_in_service() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets.first().expect("one seeded subnet").clone();
    create_launch_template(&router, "lt-attach-pending").await;
    let name = "asg-attach-pending";
    create_group(
        &router,
        name,
        "lt-attach-pending",
        4,
        0,
        Placement::Zone,
        &subnet,
    )
    .await;
    let instance_id = run_instance(&router, &subnet.id).await;

    let (status, body) = attach(&router, name, std::slice::from_ref(&instance_id)).await;
    assert_eq!(
        status, 200,
        "AttachInstances expected HTTP 200, got {status} with {body:?}"
    );
    // Read straight after the call: the instance is still on its way into service, which is
    // the state a caller acting immediately has to be able to see.
    let reported = tag_value(&describe(&router, name).await, "LifecycleState");
    assert_eq!(
        reported, PENDING,
        "an instance just attached must be reported {PENDING}, got {reported:?}"
    );
    // And it reaches service on the clock, with no further call from anybody.
    let (settled, _) = settled_instances(&router, "the attached instance settles", name, 1).await;
    assert_eq!(
        settled,
        vec![instance_id.clone()],
        "the group must report the attached instance in service, got {settled:?}"
    );
}

/// One row of the refusal table: the case, the group it names, the instance ids it names,
/// and the whole message the service answers.
struct RefusalRow {
    case: &'static str,
    group: String,
    ids: Vec<String>,
    message: String,
}

/// Every reason an attach cannot be made is refused with the sentence for that reason, and
/// the group is left exactly as it was.
#[tokio::test]
async fn an_attach_that_cannot_be_made_is_refused_whole() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let home = subnets.first().expect("two seeded subnets").clone();
    let elsewhere = subnets.get(1).expect("two seeded subnets").clone();
    assert_ne!(
        home.zone, elsewhere.zone,
        "the two seeded subnets must be in different Availability Zones"
    );
    create_launch_template(&router, "lt-attach-refusal").await;

    // The group under test: room for two instances, holding none of its own.
    let name = "asg-attach-refusal";
    create_group(
        &router,
        name,
        "lt-attach-refusal",
        2,
        0,
        Placement::Zone,
        &home,
    )
    .await;
    // A second group holding an instance of its own, and a third already AT its maximum.
    let other = "asg-attach-other";
    create_group(
        &router,
        other,
        "lt-attach-refusal",
        1,
        1,
        Placement::Zone,
        &home,
    )
    .await;
    let full = "asg-attach-full";
    create_group(
        &router,
        full,
        "lt-attach-refusal",
        1,
        1,
        Placement::Zone,
        &home,
    )
    .await;
    let (held_by_other, _) = settled_instances(&router, "the other group settles", other, 1).await;
    let held_by_other = held_by_other.first().expect("one instance").clone();
    settled_instances(&router, "the full group settles", full, 1).await;

    let attached = run_instance(&router, &home.id).await;
    let spare = run_instance(&router, &home.id).await;
    let abroad = run_instance(&router, &elsewhere.id).await;
    let stopped = run_instance(&router, &home.id).await;
    let (status, body) = ec2_query(
        &router,
        "StopInstances",
        &[p("InstanceId.1", stopped.clone())],
    )
    .await;    assert_eq!(
        status, 200,
        "StopInstances expected HTTP 200, got {status} with {body:?}"
    );
    wait_for_ec2_state(&router, &stopped, "stopped").await;

    // The row the service must ACCEPT, so no refusal below can pass by refusing
    // everything — and what the group holds for the rest of this case.
    let (status, body) = attach(&router, name, std::slice::from_ref(&attached)).await;
    assert_eq!(
        status, 200,
        "attaching a running instance of the group's own zone expected HTTP 200, got {status} \
         with {body:?}"
    );
    settled_instances(&router, "the accepted attach settles", name, 1).await;

    // An id the whole account holds nothing for, of the issued form a client submits.
    let unknown = "i-00000000000000099".to_string();
    let rows = vec![
        RefusalRow {
            case: "a group that does not exist",
            group: "asg-attach-absent".to_string(),
            ids: vec![spare.clone()],
            message: absent_group_message("asg-attach-absent"),
        },
        RefusalRow {
            case: "an instance id Amazon EC2 holds nothing for",
            group: name.to_string(),
            ids: vec![unknown.clone()],
            message: format!("The instance ID '{unknown}' does not exist"),
        },
        RefusalRow {
            case: "an instance that is not running",
            group: name.to_string(),
            ids: vec![stopped.clone()],
            message: format!(
                "Instance {stopped} is in stopped state, not in running state, and cannot be \
                 attached."
            ),
        },
        RefusalRow {
            case: "an instance the group already holds",
            group: name.to_string(),
            ids: vec![attached.clone()],
            message: format!("Instance {attached} is already part of Auto Scaling group {name}."),
        },
        RefusalRow {
            case: "an instance another group holds",
            group: name.to_string(),
            ids: vec![held_by_other.clone()],
            message: format!(
                "Instance {held_by_other} is already part of Auto Scaling group {other}."
            ),
        },
        RefusalRow {
            case: "an instance in a zone the group is not configured for",
            group: name.to_string(),
            ids: vec![abroad.clone()],
            message: format!(
                "Instance {abroad} is in Availability Zone {}, which is not one of the \
                 Availability Zones of Auto Scaling group {name}.",
                elsewhere.zone
            ),
        },
        RefusalRow {
            case: "an attachable instance named beside an unknown one",
            group: name.to_string(),
            ids: vec![spare.clone(), unknown.clone()],
            message: format!("The instance ID '{unknown}' does not exist"),
        },
        RefusalRow {
            case: "an attach that would pass the group's maximum size",
            group: full.to_string(),
            ids: vec![spare.clone()],
            message: format!(
                "AutoScalingGroup {full} has min-size=0, max-size=1, desired-size=1. New \
                 desired-size=2 must be between min-size and max-size."
            ),
        },
        RefusalRow {
            case: "more instance ids than one call may name",
            group: name.to_string(),
            // One past the documented 20 a call may name, so the bound itself is tested.
            ids: (0..21).map(|index| format!("i-000000000000000{index:02}")).collect(),
            message: "1 validation error detected: Value with 21 items at 'instanceIds' failed \
                      to satisfy constraint: Member must have length less than or equal to 20"
                .to_string(),
        },
    ];

    for row in rows {
        let case = row.case;
        let (status, body) = attach(&router, &row.group, &row.ids).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected a ValidationError, got {body:?}"
        );
        assert_eq!(
            message, row.message,
            "case {case:?}: expected the whole message {:?}, got {message:?}",
            row.message
        );
    }

    // Nothing any refusal named was attached, and the capacity never moved: the group still
    // holds the one instance the accepted row gave it.
    let (reported, body) = settled_instances(&router, "the group is unchanged", name, 1).await;
    assert_eq!(
        reported,
        vec![attached.clone()],
        "the refused calls must leave the group holding only {attached}, got {reported:?}"
    );
    let capacity = tag_value(&body, "DesiredCapacity");
    assert_eq!(
        capacity, "1",
        "the refused calls must leave the desired capacity at 1, got {capacity:?} in {body}"
    );
    // Each instance a refusal named is still the standalone instance it was.
    for instance_id in [&spare, &abroad] {
        let state = ec2_instance_state(&router, instance_id).await;
        assert_eq!(
            state, "running",
            "a refused attach must leave {instance_id} running, got {state:?}"
        );
    }
}
