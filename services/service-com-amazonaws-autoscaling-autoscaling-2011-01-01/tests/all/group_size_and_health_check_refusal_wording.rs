// SPDX-License-Identifier: CC0-1.0

//! The sentences Amazon EC2 Auto Scaling refuses a group's SIZES, its HEALTH CHECK TYPE and
//! its TERMINATION POLICIES with.
//!
//! RULE: an error code is not the whole answer — a client (a CDK or CloudFormation
//! wrapper, a retry policy) tells one `ValidationError` from another by the sentence that
//! comes with it, so each refusal is the fixed text the service words for that error class
//! with the request's own values interpolated, and nothing else:
//!
//! * the three sizes describe ONE group, so a desired capacity outside the bounds the
//!   request leaves the group with is refused `Desired capacity:<DesiredCapacity> must be
//!   between the specified min size:<MinSize> and max size:<MaxSize>` — the three values
//!   being the only variable parts. The window is the one THAT request establishes, so a
//!   bound the same request raises or lowers counts, and the refusal applies no field at
//!   all;
//! * `HealthCheckType` is "one or more comma-separated health check types"
//!   (`smithy_query(query="doc",shape="AutoScalingGroup",member="HealthCheckType")`), a
//!   free string the service validates itself, so the refusal is the one machine-readable
//!   statement of the set it accepts: the fixed sentence `Valid health check types are:
//!   [EC2, ELB, EBS, VPC_LATTICE].`, carrying nothing from the request;
//! * `TerminationPolicies` is likewise a list of free strings whose values the
//!   documentation gives only in prose
//!   (`smithy_query(query="doc",shape="UpdateAutoScalingGroupType",member="TerminationPolicies")`),
//!   so its refusal states the set the same way: the rejected value(s) of the request
//!   bracketed, then the fixed sentence ` is not a valid termination policy. Valid
//!   termination policies are: [` … `].` naming the policy TYPES the service publishes, in
//!   the order its catalog reports them;
//! * a capacity change asks for a capacity the group's OWN bounds admit, so one outside them
//!   is refused `New SetDesiredCapacity value <n> is above max value <MaxSize> for the
//!   AutoScalingGroup.` (or `below min value <MinSize>`) — the requested capacity and the
//!   bound it crossed being the only variable parts, with no group named.
//!
//! The rows span what each rule generalises over: which side of the range the capacity
//! misses and by how much, whether the bounds come from the create's own members or from
//! the group the update is applied to, which health check type is named — one word
//! outside the set, a word of the set in the wrong case, a comma-separated list with one
//! unknown entry — beside the comma-separated forms the service accepts; which termination
//! policies a list names: one unknown word, a known policy in the wrong case, an unknown
//! entry after a known one, two unknown entries at once, beside a policy of the documented
//! set and the Lambda function ARN that documentation ends with; and, for the capacity
//! change, the bound crossed, the distance from it, the bounds being equal and the group's
//! own name, which no refusal carries.

use crate::support::{borrowed, p, query, tag_value, tag_values, xml_unescape, REGION};

/// A launch source every row carries, so no row is refused for naming none. The bounds and
/// the health check type are read before any launch source is resolved, so the template
/// need not exist.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-wording");

/// The zone every row places its group in.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The fixed text of the bounds refusal, with the request's three sizes interpolated.
fn expected_bounds_sentence(desired: i32, min: i32, max: i32) -> String {
    format!("Desired capacity:{desired} must be between the specified min size:{min} and max size:{max}")
}

/// The fixed sentence the service states the health check types it accepts with. The set is
/// the model's own ("the valid values are EC2, EBS, ELB, and VPC_LATTICE",
/// `smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="HealthCheckType")`),
/// in the order this sentence lists them.
const EXPECTED_HEALTH_CHECK_SENTENCE: &str = "Valid health check types are: [EC2, ELB, EBS, VPC_LATTICE].";

/// The `(code, message)` of a refused awsQuery answer.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// One row of the bounds table: the sizes the create carries.
struct BoundsRow {
    case: &'static str,
    min: i32,
    max: i32,
    desired: i32,
}

#[tokio::test]
async fn a_desired_capacity_outside_the_bounds_is_refused_with_the_bounds_sentence() {
    let rows = [
        BoundsRow {
            case: "one above a maximum of one",
            min: 0,
            max: 1,
            desired: 2,
        },
        BoundsRow {
            case: "below the minimum",
            min: 2,
            max: 5,
            desired: 1,
        },
        BoundsRow {
            case: "one above bounds that are equal",
            min: 3,
            max: 3,
            desired: 4,
        },
        BoundsRow {
            case: "far above a maximum of zero",
            min: 0,
            max: 0,
            desired: 9,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = crate::support::new_engine();
        let router = crate::support::router_with(engine);
        let name = format!("bounds-{index}");
        let params = [
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p(ZONE.0, ZONE.1),
            p("MinSize", row.min.to_string()),
            p("MaxSize", row.max.to_string()),
            p("DesiredCapacity", row.desired.to_string()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        let case = row.case;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the create to be refused, got {status}: {body}"
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                expected_bounds_sentence(row.desired, row.min, row.max)
            ),
            "case {case:?}: expected the bounds refusal, got {body}"
        );

        // A refused create stores nothing.
        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
        );
        assert!(
            !described.contains(name.as_str()),
            "case {case:?}: a refused create leaves no group behind, got {described}"
        );
    }
}

/// One row of the update table: the bounds the group was created with, what the update
/// carries, and the three sizes the refusal must name — the ones the update LEAVES the
/// group with, which is where a member the request omits keeps the group's own value.
struct UpdateRow {
    case: &'static str,
    created_min: i32,
    created_max: i32,
    /// `MinSize`/`MaxSize` the update carries, when it carries them.
    update_min: Option<i32>,
    update_max: Option<i32>,
    desired: i32,
    expected_min: i32,
    expected_max: i32,
}

#[tokio::test]
async fn the_update_path_states_the_bounds_it_leaves_the_group_with() {
    let rows = [
        UpdateRow {
            case: "a capacity above the group's own maximum, which the update keeps",
            created_min: 0,
            created_max: 2,
            update_min: None,
            update_max: None,
            desired: 3,
            expected_min: 0,
            expected_max: 2,
        },
        UpdateRow {
            case: "a capacity below a minimum the update itself raises",
            created_min: 0,
            created_max: 4,
            update_min: Some(3),
            update_max: None,
            desired: 1,
            expected_min: 3,
            expected_max: 4,
        },
        UpdateRow {
            case: "a capacity above a maximum the update itself lowers",
            created_min: 0,
            created_max: 6,
            update_min: None,
            update_max: Some(2),
            desired: 5,
            expected_min: 0,
            expected_max: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = crate::support::new_engine();
        let router = crate::support::router_with(engine);
        let name = format!("update-bounds-{index}");
        let case = row.case;
        let created = [
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p(ZONE.0, ZONE.1),
            p("MinSize", row.created_min.to_string()),
            p("MaxSize", row.created_max.to_string()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&created)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );

        let mut update = vec![
            p("AutoScalingGroupName", name.clone()),
            p("DesiredCapacity", row.desired.to_string()),
        ];
        if let Some(min) = row.update_min {
            update.push(p("MinSize", min.to_string()));
        }
        if let Some(max) = row.update_max {
            update.push(p("MaxSize", max.to_string()));
        }
        let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&update)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the update to be refused, got {status}: {body}"
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                expected_bounds_sentence(row.desired, row.expected_min, row.expected_max)
            ),
            "case {case:?}: expected the bounds refusal, got {body}"
        );

        // The refused update changed nothing.
        let (_, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            (
                tag_value(&described, "MinSize"),
                tag_value(&described, "MaxSize")
            ),
            (
                row.created_min.to_string(),
                row.created_max.to_string()
            ),
            "case {case:?}: a refused update leaves the group's bounds alone, got {described}"
        );
    }
}

/// One row of the combined table: the bounds the group is created with, and the three sizes
/// ONE update request carries together. The window the refusal must name is the one that
/// request establishes — `update_min`..`update_max` — whichever way each bound moved.
struct CombinedRow {
    case: &'static str,
    created_min: i32,
    created_max: i32,
    update_min: i32,
    update_max: i32,
    /// Outside `update_min`..`update_max`, so the request is refused.
    refused_desired: i32,
    /// Inside that same window, so the follow-up request is applied whole.
    accepted_desired: i32,
}

#[tokio::test]
async fn the_window_is_the_one_the_same_request_establishes() {
    let rows = [
        CombinedRow {
            case: "one above a maximum the same request raises",
            created_min: 0,
            created_max: 1,
            update_min: 0,
            update_max: 3,
            refused_desired: 4,
            accepted_desired: 3,
        },
        CombinedRow {
            case: "below a minimum the same request raises, under a maximum it also raises",
            created_min: 0,
            created_max: 1,
            update_min: 2,
            update_max: 5,
            refused_desired: 1,
            accepted_desired: 2,
        },
        CombinedRow {
            case: "above a maximum the same request lowers while raising the minimum",
            created_min: 0,
            created_max: 6,
            update_min: 2,
            update_max: 4,
            refused_desired: 5,
            accepted_desired: 3,
        },
        CombinedRow {
            case: "far above a maximum the same request raises",
            created_min: 0,
            created_max: 1,
            update_min: 0,
            update_max: 4,
            refused_desired: 9,
            accepted_desired: 0,
        },
        CombinedRow {
            case: "below equal bounds the same request sets",
            created_min: 0,
            created_max: 3,
            update_min: 3,
            update_max: 3,
            refused_desired: 0,
            accepted_desired: 3,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = crate::support::new_engine();
        let router = crate::support::router_with(engine);
        let name = format!("combined-bounds-{index}");
        let case = row.case;
        let created = [
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p(ZONE.0, ZONE.1),
            p("MinSize", row.created_min.to_string()),
            p("MaxSize", row.created_max.to_string()),
            p("DesiredCapacity", row.created_min.to_string()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&created)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );

        let refused = [
            p("AutoScalingGroupName", name.clone()),
            p("MinSize", row.update_min.to_string()),
            p("MaxSize", row.update_max.to_string()),
            p("DesiredCapacity", row.refused_desired.to_string()),
        ];
        let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&refused)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the update to be refused, got {status}: {body}"
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                expected_bounds_sentence(row.refused_desired, row.update_min, row.update_max)
            ),
            "case {case:?}: expected the bounds refusal naming the window this request \
             establishes, got {body}"
        );

        // The refusal applies NO field of that request: all three sizes are the ones the
        // create left behind.
        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
        );
        assert_eq!(
            sizes_of(&described),
            (
                row.created_min.to_string(),
                row.created_max.to_string(),
                row.created_min.to_string()
            ),
            "case {case:?}: a refused update applies no field, got {described}"
        );

        // The same window, with a capacity inside it, is applied whole.
        let accepted = [
            p("AutoScalingGroupName", name.clone()),
            p("MinSize", row.update_min.to_string()),
            p("MaxSize", row.update_max.to_string()),
            p("DesiredCapacity", row.accepted_desired.to_string()),
        ];
        let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&accepted)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the in-window update to be applied, got {status}: {body}"
        );
        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
        );
        assert_eq!(
            sizes_of(&described),
            (
                row.update_min.to_string(),
                row.update_max.to_string(),
                row.accepted_desired.to_string()
            ),
            "case {case:?}: expected the in-window sizes to be read back, got {described}"
        );
    }
}

/// The `(MinSize, MaxSize, DesiredCapacity)` a described group reports.
fn sizes_of(described: &str) -> (String, String, String) {
    (
        tag_value(described, "MinSize"),
        tag_value(described, "MaxSize"),
        tag_value(described, "DesiredCapacity"),
    )
}

/// The fixed text `SetDesiredCapacity` refuses a capacity outside the bounds the group is
/// configured with: the capacity the request asked for and the bound of the group it
/// crossed, and nothing else — the sentence ends in a period after `AutoScalingGroup` and
/// names neither the group nor any other value.
fn expected_capacity_bound_sentence(desired: i32, relation: &str, bound: i32) -> String {
    format!(
        "New SetDesiredCapacity value {desired} is {relation} value {bound} for the \
         AutoScalingGroup."
    )
}

/// One row of the capacity-bound table: the bounds the group is created with, the capacity
/// the change asks for, and either the bound it crosses (`relation`, `bound`) or `None` for a
/// capacity the group can serve.
struct CapacityBoundRow {
    case: &'static str,
    /// The group's own name, which the refusal must NOT carry whatever it is.
    group: &'static str,
    min: i32,
    max: i32,
    desired: i32,
    crossed: Option<(&'static str, i32)>,
}

/// RULE: `SetDesiredCapacity` refuses a capacity outside the group's own bounds with the
/// fixed sentence for the bound it crossed, carrying the requested capacity and that bound
/// and nothing else; the group keeps the capacity it had.
#[tokio::test]
async fn a_capacity_outside_the_groups_bounds_is_refused_with_the_bound_sentence() {
    let rows = [
        CapacityBoundRow {
            case: "one above a maximum of one",
            group: "capacity-bound",
            min: 0,
            max: 1,
            desired: 2,
            crossed: Some(("above max", 1)),
        },
        CapacityBoundRow {
            case: "far above a maximum of zero",
            group: "a-group-whose-name-is-considerably-longer-than-the-others",
            min: 0,
            max: 0,
            desired: 9,
            crossed: Some(("above max", 0)),
        },
        CapacityBoundRow {
            case: "below a minimum of two",
            group: "bound2",
            min: 2,
            max: 5,
            desired: 1,
            crossed: Some(("below min", 2)),
        },
        CapacityBoundRow {
            case: "below bounds that are equal",
            group: "capacity.bound-3",
            min: 3,
            max: 3,
            desired: 0,
            crossed: Some(("below min", 3)),
        },
        CapacityBoundRow {
            case: "the maximum itself, which is inside the bounds",
            group: "capacity-at-max",
            min: 0,
            max: 2,
            desired: 2,
            crossed: None,
        },
        CapacityBoundRow {
            case: "the minimum itself, which is inside the bounds",
            group: "capacity-at-min",
            min: 1,
            max: 3,
            desired: 1,
            crossed: None,
        },
    ];

    for row in &rows {
        let engine = crate::support::new_engine();
        let router = crate::support::router_with(engine);
        let case = row.case;
        // The group starts at its own minimum, so a refused change is visible as the
        // capacity it left behind.
        let params = [
            p("AutoScalingGroupName", row.group),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p(ZONE.0, ZONE.1),
            p("MinSize", row.min.to_string()),
            p("MaxSize", row.max.to_string()),
            p("DesiredCapacity", row.min.to_string()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the group must be created, got {status}: {body}"
        );

        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", row.group),
                ("DesiredCapacity", row.desired.to_string().as_str()),
            ],
        )
        .await;
        let expected_capacity = match row.crossed {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a capacity inside the bounds must be served, got {status}: \
                     {body}"
                );
                row.desired
            }
            Some((relation, bound)) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the capacity to be refused, got {status}: {body}"
                );
                assert_eq!(
                    refusal(&body),
                    (
                        "ValidationError".to_string(),
                        expected_capacity_bound_sentence(row.desired, relation, bound)
                    ),
                    "case {case:?}: expected the capacity-bound sentence, got {body}"
                );
                assert!(
                    !xml_unescape(&tag_value(&body, "Message")).contains(row.group),
                    "case {case:?}: the sentence names no group, got {body}"
                );
                row.min
            }
        };

        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", row.group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
        );
        assert_eq!(
            tag_value(&described, "DesiredCapacity"),
            expected_capacity.to_string(),
            "case {case:?}: expected the capacity the answered change left, got {described}"
        );
    }
}

/// One row of the health check table: the value named, and whether the service accepts it.
struct HealthCheckRow {
    case: &'static str,
    value: &'static str,
    accepted: bool,
}

#[tokio::test]
async fn an_unsupported_health_check_type_is_refused_with_the_fixed_sentence() {
    let rows = [
        HealthCheckRow {
            case: "a word outside the set",
            value: "HTTP",
            accepted: false,
        },
        HealthCheckRow {
            case: "a word of the set in the wrong case",
            value: "ec2",
            accepted: false,
        },
        HealthCheckRow {
            case: "a comma-separated list with one unknown entry",
            value: "EC2,HTTP",
            accepted: false,
        },
        HealthCheckRow {
            case: "an entry of the set the issue never cited",
            value: "VPC_LATTICE",
            accepted: true,
        },
        HealthCheckRow {
            case: "the comma-separated form the model documents",
            value: "EC2,ELB",
            accepted: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = crate::support::new_engine();
        let router = crate::support::router_with(engine);
        let name = format!("health-{index}");
        let case = row.case;
        let params = [
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p(ZONE.0, ZONE.1),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("HealthCheckType", row.value),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        if row.accepted {
            assert_eq!(
                status, 200,
                "case {case:?} ({value}): expected the create to be accepted, got {status}: {body}",
                value = row.value
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?} ({value}): expected the create to be refused, got {status}: {body}",
            value = row.value
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                EXPECTED_HEALTH_CHECK_SENTENCE.to_string()
            ),
            "case {case:?}: expected the fixed health check sentence, got {body}"
        );
        assert!(
            !xml_unescape(&tag_value(&body, "Message")).contains(row.value),
            "case {case:?}: the sentence carries nothing from the request, got {body}"
        );

        // The same sentence on the update path, for a group that already exists.
        let base = format!("{name}-base");
        let (status, body) = query(
            &router,
            "CreateAutoScalingGroup",
            &borrowed(&[
                p("AutoScalingGroupName", base.clone()),
                p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
                p(ZONE.0, ZONE.1),
                p("MinSize", "0"),
                p("MaxSize", "1"),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the base group must be created, got {status}: {body}"
        );
        let (status, body) = query(
            &router,
            "UpdateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", base.as_str()),
                ("HealthCheckType", row.value),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the update to be refused, got {status}: {body}"
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                EXPECTED_HEALTH_CHECK_SENTENCE.to_string()
            ),
            "case {case:?}: expected the fixed health check sentence on the update, got {body}"
        );
    }
}

/// The fixed text of the termination-policy refusal, with the rejected value(s) bracketed
/// ahead of it. Spelled out here rather than imported, as every sentence in this suite is:
/// the valid-value list names the policy TYPES the service publishes — the Lambda-ARN form
/// named by its type `Lambda` — in the order that catalog reports them.
fn expected_termination_policy_sentence(rejected: &[String]) -> String {
    format!(
        "[{}] is not a valid termination policy. Valid termination policies are: \
         [AllocationStrategy, ClosestToNextInstanceHour, Default, Lambda, NewestInstance, \
         OldestInstance, OldestLaunchConfiguration, OldestLaunchTemplate].",
        rejected.join(", ")
    )
}

/// The custom-policy form the member's documentation ends with:
/// "arn:aws:lambda:region:account-id:function:my-function:my-alias", written for the Region
/// and the account this emulator answers for rather than for any one recorded function.
fn lambda_policy() -> String {
    format!(
        "arn:aws:lambda:{REGION}:{}:function:scale-in-chooser:live",
        engine::emulator_account_id()
    )
}

/// One row of the termination-policy table: the policies the request lists, in order, and
/// the entries of that list the service rejects — empty when the whole list is accepted.
struct TerminationPolicyRow {
    case: &'static str,
    policies: Vec<String>,
    rejected: Vec<String>,
}

impl TerminationPolicyRow {
    /// A row whose whole list the service accepts.
    fn accepted(case: &'static str, policies: &[&str]) -> Self {
        Self {
            case,
            policies: policies.iter().map(|policy| (*policy).to_string()).collect(),
            rejected: Vec::new(),
        }
    }

    /// A row the service refuses, naming the entries it rejects in request order.
    fn refused(case: &'static str, policies: &[&str], rejected: &[&str]) -> Self {
        Self {
            case,
            policies: policies.iter().map(|policy| (*policy).to_string()).collect(),
            rejected: rejected.iter().map(|value| (*value).to_string()).collect(),
        }
    }
}

/// The `TerminationPolicies.member.N` parameters a request names `policies` under.
fn termination_policy_params(policies: &[String]) -> Vec<(String, String)> {
    policies
        .iter()
        .enumerate()
        .map(|(index, policy)| {
            p(
                &format!("TerminationPolicies.member.{}", index + 1),
                policy.clone(),
            )
        })
        .collect()
}

/// The termination policies a described group reports, in the order it holds them.
fn described_policies(described: &str) -> Vec<String> {
    tag_values(&tag_value(described, "TerminationPolicies"), "member")
}

#[tokio::test]
async fn an_unknown_termination_policy_is_refused_with_the_bracketed_sentence() {
    let rows = [
        TerminationPolicyRow::accepted(
            "a policy of the documented set the issue never cited",
            &["AllocationStrategy"],
        ),
        TerminationPolicyRow::accepted(
            "two documented policies, applied in the order they are listed",
            &["OldestLaunchTemplate", "Default"],
        ),
        TerminationPolicyRow::refused(
            "a word outside the documented set",
            &["NotARealPolicy"],
            &["NotARealPolicy"],
        ),
        TerminationPolicyRow::refused(
            "a documented policy in the wrong case",
            &["oldestinstance"],
            &["oldestinstance"],
        ),
        TerminationPolicyRow::refused(
            "an unknown entry after a known one",
            &["OldestInstance", "NotARealPolicy"],
            &["NotARealPolicy"],
        ),
        TerminationPolicyRow::refused(
            "two unknown entries at once, named in request order",
            &["NeverHeardOfIt", "OldestInstance", "NotARealPolicy"],
            &["NeverHeardOfIt", "NotARealPolicy"],
        ),
    ];
    // The Lambda function ARN the documentation ends with, which is accepted in place of the
    // `Lambda` type the sentence lists.
    let lambda = lambda_policy();
    let rows = rows.into_iter().chain(std::iter::once(
        TerminationPolicyRow::accepted("the Lambda function ARN form", &[lambda.as_str()]),
    ));

    for (index, row) in rows.enumerate() {
        let engine = crate::support::new_engine();
        let router = crate::support::router_with(engine);
        let case = row.case;
        let name = format!("termination-policy-{index}");
        let mut params = vec![
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p(ZONE.0, ZONE.1),
            p("MinSize", "0"),
            p("MaxSize", "1"),
        ];
        params.extend(termination_policy_params(&row.policies));
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;

        if row.rejected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be accepted, got {status}: {body}"
            );
            let (status, described) = query(
                &router,
                "DescribeAutoScalingGroups",
                &[("AutoScalingGroupNames.member.1", name.as_str())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
            );
            assert_eq!(
                described_policies(&described),
                row.policies,
                "case {case:?}: expected the accepted policies to be read back in order, got \
                 {described}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the create to be refused, got {status}: {body}"
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                expected_termination_policy_sentence(&row.rejected)
            ),
            "case {case:?}: expected the bracketed termination-policy sentence, got {body}"
        );
        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
        );
        assert!(
            !described.contains(name.as_str()),
            "case {case:?}: a refused create leaves no group behind, got {described}"
        );

        // The same sentence on the update path, where the refused request must leave the
        // policies the group already holds exactly as they were.
        let base = format!("{name}-base");
        let (status, body) = query(
            &router,
            "CreateAutoScalingGroup",
            &borrowed(&[
                p("AutoScalingGroupName", base.clone()),
                p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
                p(ZONE.0, ZONE.1),
                p("MinSize", "0"),
                p("MaxSize", "1"),
                p("TerminationPolicies.member.1", "OldestInstance"),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the base group must be created, got {status}: {body}"
        );
        let mut update = vec![p("AutoScalingGroupName", base.clone())];
        update.extend(termination_policy_params(&row.policies));
        let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&update)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the update to be refused, got {status}: {body}"
        );
        assert_eq!(
            refusal(&body),
            (
                "ValidationError".to_string(),
                expected_termination_policy_sentence(&row.rejected)
            ),
            "case {case:?}: expected the same sentence on the update path, got {body}"
        );
        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", base.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {described}"
        );
        assert_eq!(
            described_policies(&described),
            vec!["OldestInstance".to_string()],
            "case {case:?}: a refused update leaves the group's policies alone, got {described}"
        );
    }
}
