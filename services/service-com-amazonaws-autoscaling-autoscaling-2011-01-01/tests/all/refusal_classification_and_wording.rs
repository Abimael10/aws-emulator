// SPDX-License-Identifier: CC0-1.0

//! How Amazon EC2 Auto Scaling CLASSIFIES and WORDS the refusals of this surface, end to end
//! over the awsQuery wire.
//!
//! Every case here answers HTTP 400; what each table pins is the error `Code` the refusal is
//! classified under, the fixed text AWS words that class in, and — for the create of a launch
//! configuration — which of two checks runs first. The rules:
//!
//! * a request that MOVES named instances of a group and is refused by the group's own sizes
//!   states those three sizes and asks the caller to update them, naming the move and how many
//!   instances the request named — one template for `AttachInstances` and `EnterStandby`;
//! * a tag write that asks for more tags than a resource may carry is the modelled
//!   `LimitExceededFault`, whichever operation writes the tags and whichever way the quota is
//!   crossed;
//! * a tag naming a resource type this service does not tag is refused naming the type that
//!   was asked for and the one type that is valid;
//! * a constraint-violation clause quotes the value the request submitted IN FULL, however
//!   long it was, with no elision and no byte count of the emulator's own;
//! * `CreateLaunchConfiguration` validates the request's own launch source before it reports
//!   that the account cannot create launch configurations at all;
//! * a paginated read refuses a `NextToken` it did not issue in the present tense, and the
//!   attachment reads that page on the pair refuse one sent without the page size for that
//!   pair before they read the token at all.
//!
//! Each table spans what its rule generalises over — the sizes and the counts, the way the
//! quota is crossed, the resource type asked for, the member and the length of the value, the
//! members a create states, the token sent — rather than the one request a recording made.
//! The lifecycle-action refusal that names the instance together with the hook it was looked
//! for under is tabled where that surface is driven
//! (`lifecycle_hook_execution::a_callback_naming_no_held_action_says_which_half_matched_nothing`).
//!
//! Nothing here waits for a span: every wait looks AGAIN until the condition the clock owes
//! holds, or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, ec2_query, emulator, p, query, seed_subnets, tag_value, tag_values, xml_unescape,
};

/// How long a case here waits for a condition the clock owes before calling it a failure.
///
/// A BACKSTOP, not a wait: every helper below looks again until the condition holds, so this
/// only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The launch template every group here is created from — one the harness puts in the
/// account's own Amazon EC2 store.
const TEMPLATE: &str = "lt-1";

/// The AMI a standalone instance of this suite is launched from.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The one resource type a tag of this service may name, from the model's own documentation
/// for `Tag$ResourceType`: "The only supported value is auto-scaling-group."
const GROUP_RESOURCE_TYPE: &str = "auto-scaling-group";

/// The wire code and the unescaped message of one refusal.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// The sentence a move the group's sizes do not admit is refused with, as AWS words it: the
/// group, the three sizes it holds, the move that was asked for and how many instances it
/// named, then the remedy.
fn group_sizes_refusal(
    group: &str,
    min_size: i32,
    max_size: i32,
    desired: i32,
    clause: &str,
    instances: usize,
) -> String {
    format!(
        "AutoScalingGroup {group} has min-size={min_size}, max-size={max_size}, and \
         desired-size={desired}. {clause} {instances} instance, please update the \
         AutoScalingGroup sizes appropriately."
    )
}

/// Create one Auto Scaling group over the wire with the three sizes a row states, placed in
/// `zone`, and assert the create succeeded.
async fn create_group(
    router: &MultiServiceRouter,
    group: &str,
    zone: &str,
    min_size: i32,
    max_size: i32,
    desired: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", min_size.to_string()),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("AvailabilityZones.member.1", zone),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {group:?}: expected HTTP 200, got {status} with {body:?}"
    );
}

/// Launch one standalone Amazon EC2 instance into `subnet_id` over Amazon EC2's own wire and
/// answer its id, once Amazon EC2 reports it `running`.
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
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) =
            ec2_query(router, "DescribeInstances", &[p("InstanceId.1", &instance_id)]).await;
        let state = if status == 200 {
            tag_value(&tag_value(&body, "instanceState"), "name")
        } else {
            String::new()
        };
        if state == "running" {
            return instance_id;
        }
        assert!(
            Instant::now() < deadline,
            "expected Amazon EC2 to report {instance_id} running, got {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until `group` reports exactly `wanted` instances in `state`, answering their
/// ids.
async fn instances_in_state(
    router: &MultiServiceRouter,
    group: &str,
    state: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeAutoScalingGroups for {group} expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let matching: Vec<String> = ids
            .iter()
            .zip(states.iter())
            .filter(|(_, held)| held.as_str() == state)
            .map(|(id, _)| id.clone())
            .collect();
        if matching.len() == wanted {
            return matching;
        }
        assert!(
            Instant::now() < deadline,
            "group {group} never reported {wanted} instance(s) in {state}; last read {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Which move a row of the capacity table asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Move {
    /// `AttachInstances`: the row's instances are the account's own, attached into the group.
    Attach,
    /// `EnterStandby` with the decrement: the row's instances are the group's own, parked.
    Standby,
}

impl Move {
    /// The clause AWS names this move in.
    fn clause(self) -> &'static str {
        match self {
            Self::Attach => "To attach",
            Self::Standby => "To place into standby",
        }
    }
}

/// One row of the capacity table: the sizes the group is created with, the move asked for and
/// how many instances it names.
struct CapacityRow {
    case: &'static str,
    group: &'static str,
    min_size: i32,
    max_size: i32,
    desired: i32,
    move_kind: Move,
    instances: usize,
}

/// A move the group's own sizes do not admit is refused with ONE sentence: the sizes the
/// group holds now, the move that was asked for, how many instances it named, and the remedy
/// — the same template for the two operations that answer it.
///
/// The rows differ along the move (an attach past `MaxSize`, a park below `MinSize`), the
/// three sizes the group is configured with, and how many instances one call names, so
/// nothing about the sentence can be keyed to one request's shape.
#[tokio::test]
async fn a_move_the_groups_sizes_do_not_admit_states_the_sizes_and_the_move() {
    let rows = [
        CapacityRow {
            case: "an attach of one instance past the maximum size",
            group: "asg-bound-attach-one",
            min_size: 0,
            max_size: 1,
            desired: 1,
            move_kind: Move::Attach,
            instances: 1,
        },
        CapacityRow {
            case: "an attach of two instances past the maximum size",
            group: "asg-bound-attach-two",
            min_size: 1,
            max_size: 2,
            desired: 1,
            move_kind: Move::Attach,
            instances: 2,
        },
        CapacityRow {
            case: "a park of one instance below the minimum size",
            group: "asg-bound-standby-one",
            min_size: 1,
            max_size: 3,
            desired: 1,
            move_kind: Move::Standby,
            instances: 1,
        },
        CapacityRow {
            case: "a park of two instances below the minimum size",
            group: "asg-bound-standby-two",
            min_size: 2,
            max_size: 3,
            desired: 2,
            move_kind: Move::Standby,
            instances: 2,
        },
    ];

    for row in &rows {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let subnet = &subnets[0];
        create_group(
            &router,
            row.group,
            &subnet.zone,
            row.min_size,
            row.max_size,
            row.desired,
        )
        .await;

        let (status, body) = match row.move_kind {
            Move::Attach => {
                let mut params = vec![p("AutoScalingGroupName", row.group)];
                for index in 1..=row.instances {
                    let instance = run_instance(&router, &subnet.id).await;
                    params.push(p(&format!("InstanceIds.member.{index}"), instance));
                }
                query(&router, "AttachInstances", &borrowed(&params)).await
            }
            Move::Standby => {
                let held = instances_in_state(
                    &router,
                    row.group,
                    "InService",
                    usize::try_from(row.desired).expect("a non-negative desired capacity"),
                )
                .await;
                let mut params = vec![
                    p("AutoScalingGroupName", row.group),
                    p("ShouldDecrementDesiredCapacity", "true"),
                ];
                for (index, instance) in held.iter().take(row.instances).enumerate() {
                    params.push(p(
                        &format!("InstanceIds.member.{}", index + 1),
                        instance.clone(),
                    ));
                }
                query(&router, "EnterStandby", &borrowed(&params)).await
            }
        };

        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body:?}",
            row.case
        );
        let expected = group_sizes_refusal(
            row.group,
            row.min_size,
            row.max_size,
            row.desired,
            row.move_kind.clause(),
            row.instances,
        );
        assert_eq!(
            refusal(&body),
            ("ValidationError".to_string(), expected.clone()),
            "case {:?}: expected the sizes sentence {expected:?}, got {body}",
            row.case
        );
    }
}

/// How a row of the tag-quota table crosses the quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crossing {
    /// One `CreateOrUpdateTags` call stating more tags than a group may carry.
    OneCall,
    /// One more key for a group that already carries the whole quota.
    OneMoreKey,
    /// A `CreateAutoScalingGroup` carrying more tags than a group may carry.
    OnCreate,
}

/// A tag write that asks for more tags than a group may carry is the MODELLED limit fault,
/// under the wholly fixed sentence AWS words that quota refusal in — whichever operation
/// writes the tags, and whichever way the quota is crossed. Nothing of the refused call is
/// stored.
#[tokio::test]
async fn a_tag_write_past_the_quota_is_refused_as_the_modelled_limit() {
    // The quota a group's tags are held to, published as 50 tags per resource, which is the
    // same number one call may state.
    const QUOTA: usize = 50;

    for (case, crossing) in [
        ("one call stating one tag too many", Crossing::OneCall),
        ("one key past a group already at the quota", Crossing::OneMoreKey),
        ("a create carrying one tag too many", Crossing::OnCreate),
    ] {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let group = "asg-tag-quota";
        let created = crossing != Crossing::OnCreate;
        if created {
            create_group(&router, group, &subnets[0].zone, 0, 0, 0).await;
        }

        // The tags this row's refused call carries, and the call that carries them.
        let tags = |count: usize, first: usize| -> Vec<(String, String)> {
            (0..count)
                .flat_map(|index| {
                    let member = index + 1;
                    vec![
                        p(&format!("Tags.member.{member}.ResourceId"), group),
                        p(
                            &format!("Tags.member.{member}.ResourceType"),
                            GROUP_RESOURCE_TYPE,
                        ),
                        p(
                            &format!("Tags.member.{member}.Key"),
                            format!("key-{}", first + index),
                        ),
                        p(&format!("Tags.member.{member}.Value"), "v"),
                    ]
                })
                .collect()
        };

        let (status, body) = match crossing {
            Crossing::OneCall => {
                let params = tags(QUOTA + 1, 0);
                query(&router, "CreateOrUpdateTags", &borrowed(&params)).await
            }
            Crossing::OneMoreKey => {
                let filling = tags(QUOTA, 0);
                let (status, body) =
                    query(&router, "CreateOrUpdateTags", &borrowed(&filling)).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: filling the group to the quota expected 200, got {status}: \
                     {body}"
                );
                let one_more = tags(1, QUOTA);
                query(&router, "CreateOrUpdateTags", &borrowed(&one_more)).await
            }
            Crossing::OnCreate => {
                let mut params = vec![
                    p("AutoScalingGroupName", group),
                    p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
                    p("MinSize", "0"),
                    p("MaxSize", "0"),
                    p("AvailabilityZones.member.1", subnets[0].zone.clone()),
                ];
                params.extend(tags(QUOTA + 1, 0));
                query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await
            }
        };

        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            refusal(&body),
            (
                "LimitExceeded".to_string(),
                "You tried to create more tags than allowed".to_string()
            ),
            "case {case:?}: expected the modelled quota refusal, got {body}"
        );

        // Nothing of the refused call is stored: the group carries the tags it carried
        // before, and a create the quota refused left no group at all.
        let (status, body) = query(
            &router,
            "DescribeTags",
            &[
                ("Filters.member.1.Name", "auto-scaling-group"),
                ("Filters.member.1.Values.member.1", group),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTags expected 200, got {status}: {body}"
        );
        let held = tag_values(&body, "Key").len();
        let expected_held = match crossing {
            Crossing::OneMoreKey => QUOTA,
            _ => 0,
        };
        assert_eq!(
            held, expected_held,
            "case {case:?}: expected the group to carry {expected_held} tags, got {held} in \
             {body}"
        );
        if !created {
            let (status, body) = query(
                &router,
                "DescribeAutoScalingGroups",
                &[("AutoScalingGroupNames.member.1", group)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
            );
            assert!(
                tag_values(&body, "AutoScalingGroupName").is_empty(),
                "case {case:?}: a refused create stores no group, got {body}"
            );
        }
    }
}

/// A tag naming a resource type this service does not tag is refused naming the type that was
/// asked for and the one type that is valid — the same sentence from both writers of a
/// group's tags.
///
/// The rows differ along the value sent: a near miss of the valid type, another resource of
/// this service, a type of another service, and a differently cased spelling of the valid one
/// (the documented value is one literal, so a case variant is not it).
#[tokio::test]
async fn an_unsupported_resource_type_names_the_type_and_the_valid_set() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let group = "asg-resource-type";
    create_group(&router, group, &subnets[0].zone, 0, 0, 0).await;

    for resource_type in [
        "autoscaling-group",
        "auto-scaling-instance",
        "ec2-instance",
        "Auto-Scaling-Group",
    ] {
        for operation in ["CreateOrUpdateTags", "DeleteTags"] {
            let params = [
                ("Tags.member.1.ResourceId", group),
                ("Tags.member.1.ResourceType", resource_type),
                ("Tags.member.1.Key", "env"),
                ("Tags.member.1.Value", "prod"),
            ];
            let (status, body) = query(&router, operation, &params).await;
            let case = format!("{operation} with ResourceType {resource_type:?}");
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {body:?}"
            );
            let expected = format!(
                "Invalid resourceType: {resource_type}. Valid resource types are: \
                 {GROUP_RESOURCE_TYPE} "
            );
            assert_eq!(
                refusal(&body),
                ("ValidationError".to_string(), expected.clone()),
                "case {case:?}: expected {expected:?}, got {body}"
            );
        }
    }
}

/// One row of the echo table: the member a value is sent in, the path the clause names it at,
/// the maximum that member declares, and how long the value is.
struct EchoRow {
    case: &'static str,
    /// The operation the value is sent to.
    operation: &'static str,
    /// The member the over-long value is sent in, as the wire spells the parameter.
    parameter: &'static str,
    /// The path the clause states the violated member at.
    path: &'static str,
    /// The `@length` maximum that member declares, from the model.
    maximum: usize,
    /// How long the value this row sends is.
    length: usize,
}

/// A constraint-violation clause quotes the value the request submitted IN FULL: the whole
/// value between the quotes, the member path and the maximum from the violated constraint,
/// and no elision or byte count anywhere in the sentence.
///
/// The rows differ along the member (a tag's value and its key, a group's name), the maximum
/// that member declares (256, 128, 255 — each read off the model's own `@length`), and how
/// far past it the value is: one character past, far past, and — for the key — past its own
/// maximum while still shorter than the ceiling the framework's own walk elides at, so a row
/// cannot pass by elision never having applied.
#[tokio::test]
async fn a_constraint_clause_quotes_the_whole_submitted_value() {
    let rows = [
        // `Tag$Value` -> `TagValue`, `@length` 0..256
        // (`smithy_query(query="shape",shape="TagValue")`).
        EchoRow {
            case: "a tag value one character past its maximum",
            operation: "CreateOrUpdateTags",
            parameter: "Tags.member.1.Value",
            path: "tags.1.member.value",
            maximum: 256,
            length: 257,
        },
        EchoRow {
            case: "a tag value far past its maximum",
            operation: "CreateOrUpdateTags",
            parameter: "Tags.member.1.Value",
            path: "tags.1.member.value",
            maximum: 256,
            length: 1024,
        },
        // `Tag$Key` -> `TagKey`, `@length` 1..128.
        EchoRow {
            case: "a tag key past its maximum but inside the framework's own echo ceiling",
            operation: "CreateOrUpdateTags",
            parameter: "Tags.member.1.Key",
            path: "tags.1.member.key",
            maximum: 128,
            length: 129,
        },
        EchoRow {
            case: "a tag key far past its maximum",
            operation: "CreateOrUpdateTags",
            parameter: "Tags.member.1.Key",
            path: "tags.1.member.key",
            maximum: 128,
            length: 600,
        },
        // `CreateLaunchConfigurationType`'s and `CreateAutoScalingGroupType`'s names are
        // `XmlStringMaxLen255`, `@length` 1..255
        // (`smithy_query(query="shape",shape="XmlStringMaxLen255")`).
        EchoRow {
            case: "a group name one character past its maximum",
            operation: "CreateAutoScalingGroup",
            parameter: "AutoScalingGroupName",
            path: "autoScalingGroupName",
            maximum: 255,
            length: 256,
        },
        EchoRow {
            case: "a group name far past its maximum",
            operation: "CreateAutoScalingGroup",
            parameter: "AutoScalingGroupName",
            path: "autoScalingGroupName",
            maximum: 255,
            length: 1601,
        },
    ];

    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let group = "asg-echoed-values";
    create_group(&router, group, &subnets[0].zone, 0, 0, 0).await;

    for row in &rows {
        // A value of the row's length, in characters every one of these members' patterns
        // admits, so the length is the only constraint it breaks.
        let value = "v".repeat(row.length);
        let mut params = vec![p(row.parameter, value.clone())];
        if row.operation == "CreateOrUpdateTags" {
            params.push(p("Tags.member.1.ResourceId", group));
            params.push(p("Tags.member.1.ResourceType", GROUP_RESOURCE_TYPE));
            if row.parameter != "Tags.member.1.Key" {
                params.push(p("Tags.member.1.Key", "env"));
            }
            if row.parameter != "Tags.member.1.Value" {
                params.push(p("Tags.member.1.Value", "prod"));
            }
        } else {
            params.push(p("LaunchTemplate.LaunchTemplateName", TEMPLATE));
            params.push(p("MinSize", "0"));
            params.push(p("MaxSize", "0"));
            params.push(p("AvailabilityZones.member.1", subnets[0].zone.clone()));
        }

        let (status, body) = query(&router, row.operation, &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body:?}",
            row.case
        );
        let (code, message) = refusal(&body);
        let expected = format!(
            "1 validation error detected: Value '{value}' at '{}' failed to satisfy constraint: \
             Member must have length less than or equal to {}",
            row.path, row.maximum
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("ValidationError", expected.as_str()),
            "case {:?}: expected the clause to quote the whole value, got {message:?}",
            row.case
        );
        assert!(
            !message.contains("bytes]") && !message.contains("..."),
            "case {:?}: the clause must carry no elision of its own, got {message:?}",
            row.case
        );
    }
}

/// One row of the launch-configuration table: the members the create states, and what the
/// refusal must be.
struct LaunchSourceRow {
    case: &'static str,
    params: &'static [(&'static str, &'static str)],
    code: &'static str,
    message: &'static str,
}

/// The sentence a create stating no launch source is refused with.
const NO_LAUNCH_SOURCE: &str = "Valid requests must contain either the InstanceID parameter or \
                                both the ImageId and InstanceType parameters.";

/// The sentence a well-formed create is refused with, because no launch configuration can be
/// created in this account.
const CREATION_UNAVAILABLE: &str = "The Launch Configuration creation operation is not \
                                    available in your account. Use launch templates to create \
                                    configuration templates for your Auto Scaling groups.";

/// `CreateLaunchConfiguration` validates the request's OWN launch source first: a request
/// naming neither an instance to copy nor both halves of the image-and-type source is refused
/// for its parameters, and only a well-formed one reaches the account-level refusal.
///
/// The rows span the argument combinations the rule is about: each half of the image-and-type
/// source on its own, neither half, both, the instance-to-copy source, and a request carrying
/// more than one source.
#[tokio::test]
async fn the_launch_source_is_validated_before_the_account_refusal() {
    let rows = [
        LaunchSourceRow {
            case: "the required member alone",
            params: &[("LaunchConfigurationName", "lc-none")],
            code: "ValidationError",
            message: NO_LAUNCH_SOURCE,
        },
        LaunchSourceRow {
            case: "an image with no instance type",
            params: &[
                ("LaunchConfigurationName", "lc-image-only"),
                ("ImageId", "ami-0123456789abcdef0"),
            ],
            code: "ValidationError",
            message: NO_LAUNCH_SOURCE,
        },
        LaunchSourceRow {
            case: "an instance type with no image",
            params: &[
                ("LaunchConfigurationName", "lc-type-only"),
                ("InstanceType", "t3.micro"),
            ],
            code: "ValidationError",
            message: NO_LAUNCH_SOURCE,
        },
        LaunchSourceRow {
            case: "both halves of the image-and-type source",
            params: &[
                ("LaunchConfigurationName", "lc-image-and-type"),
                ("ImageId", "ami-0123456789abcdef0"),
                ("InstanceType", "t3.micro"),
            ],
            code: "UnsupportedOperation",
            message: CREATION_UNAVAILABLE,
        },
        LaunchSourceRow {
            case: "the instance-to-copy source",
            params: &[
                ("LaunchConfigurationName", "lc-from-instance"),
                ("InstanceId", "i-0123456789abcdef0"),
            ],
            code: "UnsupportedOperation",
            message: CREATION_UNAVAILABLE,
        },
        LaunchSourceRow {
            case: "an instance to copy beside an image",
            params: &[
                ("LaunchConfigurationName", "lc-instance-and-image"),
                ("InstanceId", "i-0123456789abcdef0"),
                ("ImageId", "ami-0123456789abcdef0"),
            ],
            code: "UnsupportedOperation",
            message: CREATION_UNAVAILABLE,
        },
    ];

    let (router, _engine) = emulator();
    for row in &rows {
        let (status, body) = query(&router, "CreateLaunchConfiguration", row.params).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body:?}",
            row.case
        );
        assert_eq!(
            refusal(&body),
            (row.code.to_string(), row.message.to_string()),
            "case {:?}: expected {} {:?}, got {body}",
            row.case,
            row.code,
            row.message
        );
    }

    // Not one of them stored anything, whichever refusal answered it.
    let (status, body) = query(&router, "DescribeLaunchConfigurations", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeLaunchConfigurations after the refusals: got {status}: {body}"
    );
    assert!(
        tag_values(&body, "LaunchConfigurationName").is_empty(),
        "a refused create must store nothing, got {body}"
    );
}

/// A `NextToken` this service did not issue is refused in the PRESENT tense: the sentence is
/// about the token the caller is holding now, and the token it sent is its only variable part.
///
/// The rows differ along the token sent: a value of no form this service issues, the marker
/// with nothing behind it, the marker with text that is no offset, the first page's own
/// offset (which is never issued as a token) and a bare number.
#[tokio::test]
async fn an_unusable_next_token_is_refused_in_the_present_tense() {
    let (router, _engine) = emulator();
    for token in [
        "sim2real-not-a-real-next-token-xyz123",
        "not-a-token",
        "next-",
        "next-abc",
        "next-0",
        "0",
    ] {
        let (status, body) = query(
            &router,
            "DescribeAutoScalingInstances",
            &[("NextToken", token)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let expected = format!("The token '{token}' is invalid");
        assert_eq!(
            refusal(&body),
            ("InvalidNextToken".to_string(), expected.clone()),
            "case {token:?}: expected {expected:?}, got {body}"
        );
    }
}

/// `DescribeLoadBalancers` and `DescribeTrafficSources` page against the page size the
/// request states, so a resumption token sent with no `MaxRecords` is refused for that PAIR
/// — before the token itself is read — while a token sent beside a page size is refused for
/// the token's own value, and the sibling target-group read answers the token's value either
/// way.
///
/// The rows differ along the read, whether the request states the page size, and the token
/// sent, so neither the precedence nor which read holds it can be keyed to one request.
#[tokio::test]
async fn a_resumption_token_is_refused_for_the_members_the_read_pages_on() {
    // The sentence the attachment reads that page on the pair refuse a token sent with no
    // page size with.
    const WITHOUT_PAGE_SIZE: &str = "Cannot specify NextToken without MaxRecords";

    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let group = "asg-attachment-paging";
    create_group(&router, group, &subnets[0].zone, 0, 0, 0).await;

    for (read, token, page_size, expected) in [
        (
            "DescribeLoadBalancers",
            "sim2real-not-a-real-next-token-xyz123",
            None,
            WITHOUT_PAGE_SIZE.to_string(),
        ),
        ("DescribeLoadBalancers", "not-a-token", None, WITHOUT_PAGE_SIZE.to_string()),
        (
            "DescribeLoadBalancers",
            "not-a-token",
            Some("1"),
            "NextToken: not-a-token".to_string(),
        ),
        (
            "DescribeLoadBalancers",
            "next-0",
            Some("100"),
            "NextToken: next-0".to_string(),
        ),
        // The sibling target-group read answers the token's own value whether or not the page
        // size is stated: that refusal is that read's own recorded fact.
        (
            "DescribeLoadBalancerTargetGroups",
            "not-a-token",
            None,
            "NextToken: not-a-token".to_string(),
        ),
        // `DescribeTrafficSources` pages on the pair the same way `DescribeLoadBalancers`
        // does, and answers the token's own value once the page size is stated.
        (
            "DescribeTrafficSources",
            "not-a-token",
            None,
            WITHOUT_PAGE_SIZE.to_string(),
        ),
        (
            "DescribeTrafficSources",
            "not-a-token",
            Some("50"),
            "NextToken: not-a-token".to_string(),
        ),
    ] {
        let mut params = vec![
            p("AutoScalingGroupName", group),
            p("NextToken", token),
        ];
        if let Some(page_size) = page_size {
            params.push(p("MaxRecords", page_size));
        }
        let (status, body) = query(&router, read, &borrowed(&params)).await;
        let case = format!("{read} with the token {token:?} and page size {page_size:?}");
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            refusal(&body),
            ("ValidationError".to_string(), expected.clone()),
            "case {case:?}: expected {expected:?}, got {body}"
        );
    }
}
