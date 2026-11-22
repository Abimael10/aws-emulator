// SPDX-License-Identifier: CC0-1.0

//! The SIGN of a group's sizes, and the HEADROOM its `MaxSize` leaves it.
//!
//! Three rules, each taken over a table rather than over one request:
//!
//! * a bound is a COUNT of instances, so a `MinSize` below zero is refused for being
//!   negative — `Min bound, <MinSize>, must be non-negative` — by every operation that
//!   writes a group's bounds (`CreateAutoScalingGroup`, `UpdateAutoScalingGroup` and the
//!   scheduled action that sets them later), the bound the request handed over being the
//!   only variable part;
//! * a capacity is likewise a count, so `SetDesiredCapacity` asked for a negative one
//!   refuses it for its SIGN rather than against either bound of the group — `New
//!   SetDesiredCapacity value <n> is negative.` — ahead of the bound comparison, so the
//!   caller is told the value is not a capacity instead of being told to lower the group's
//!   minimum; and a refused change launches nothing, so the group holds exactly the
//!   instances the capacity it kept asks for;
//! * the per-group instance ceiling bounds the instances a request asks a group to RUN,
//!   not the bounds it may later grow within: `MinSize`/`MaxSize` are stored verbatim and
//!   hold no instance, so a group whose maximum is a large headroom value is created,
//!   updated and read back with that maximum unchanged, while the capacity it is actually
//!   driven to is held to the ceiling.
//!
//! The rows span what those rules generalise over: how far below zero a bound or a
//! capacity is (one, several, and the bottom of the modelled integer range), which bound
//! of the group a negative capacity would otherwise have crossed, which operation writes
//! the sizes, and how large a maximum is — on the ceiling, one past it, far past it and at
//! the top of the modelled range — beside the capacities on and past the ceiling.
//!
//! Each row is driven over the awsQuery wire through one router over one engine, the way
//! the emulator serves it, and what a refused request left behind is read back through
//! `DescribeAutoScalingGroups`.

use crate::support::{
    borrowed, negative_min_size_message, new_engine, p, query, router_with, tag_value, tag_values,
    xml_unescape,
};

/// The launch source every group here is created from — a launch TEMPLATE the harness put
/// in the account's Amazon EC2 store, because no account can create a launch configuration.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The zone every group here places its instances in.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The most instances this emulator holds ONE group to. Written out here rather than taken
/// from the crate, so the assertions below do not take the handler's own word for it: it is
/// a bound on this process (the Smithy model puts no `@range` on `AutoScalingGroupMaxSize`
/// or `AutoScalingGroupDesiredCapacity`), and it bounds instances rather than bounds.
const MAX_GROUP_CAPACITY: i32 = 1_000;

/// The top of the modelled integer range a size travels as.
const TOP_OF_THE_RANGE: i32 = i32::MAX;

/// The whole message a request asking a group to run more instances than the ceiling is
/// refused with: the capacity it asked for, and the ceiling.
fn group_size_past_ceiling_message(size: i32) -> String {
    format!(
        "The group size {size} exceeds the limit of {MAX_GROUP_CAPACITY} instances per Auto \
         Scaling group"
    )
}

/// The whole message a capacity change past the ceiling is refused with: the same two
/// values, worded around the operation's own name.
fn capacity_past_ceiling_message(size: i32) -> String {
    format!(
        "New SetDesiredCapacity value {size} exceeds the limit of {MAX_GROUP_CAPACITY} instances \
         per Auto Scaling group"
    )
}

/// The whole message `SetDesiredCapacity` refuses a negative capacity with.
fn negative_capacity_message(value: i32) -> String {
    format!("New SetDesiredCapacity value {value} is negative.")
}

/// The whole message `SetDesiredCapacity` refuses a capacity below the group's minimum
/// with, which a negative capacity must NOT be answered with.
fn below_min_message(value: i32, min_size: i32) -> String {
    format!("New SetDesiredCapacity value {value} is below min value {min_size} for the AutoScalingGroup.")
}

/// The `(code, message)` of a refused awsQuery answer.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// Create one group and answer the call's status and body.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    sizes: &[(&str, String)],
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p(ZONE.0, ZONE.1),
    ];
    for (member, value) in sizes {
        params.push(p(member, value.clone()));
    }
    query(router, "CreateAutoScalingGroup", &borrowed(&params)).await
}

/// The `(MinSize, MaxSize, DesiredCapacity)` a described group reports, as the digits on
/// the wire.
async fn described_sizes(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> (String, String, String) {
    let (_, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    (
        tag_value(&body, "MinSize"),
        tag_value(&body, "MaxSize"),
        tag_value(&body, "DesiredCapacity"),
    )
}

/// RULE: a `MinSize` below zero is refused for being negative, by every operation that
/// writes a group's bounds, and a minimum of zero — the bound itself — is accepted.
#[tokio::test]
async fn a_negative_minimum_bound_is_refused_as_a_non_negative_bound() {
    // (case, the MinSize the request states, whether the service accepts it)
    let rows: [(&str, i32, bool); 5] = [
        ("one below zero", -1, false),
        ("several below zero", -7, false),
        ("far below zero", -1_000_000, false),
        ("the bottom of the modelled integer range", i32::MIN, false),
        ("zero, the bound itself", 0, true),
    ];

    for (index, (case, min_size, accepted)) in rows.iter().enumerate() {
        let engine = new_engine();
        let router = router_with(engine);
        let name = format!("negative-min-{index}");

        // The CREATE path.
        let (status, body) = create_group(
            &router,
            &name,
            &[("MinSize", min_size.to_string()), ("MaxSize", "3".to_string())],
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be served, got {status}: {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the create to be refused, got {status}: {body}"
            );
            assert_eq!(
                refusal(&body),
                (
                    "ValidationError".to_string(),
                    negative_min_size_message(min_size)
                ),
                "case {case:?}: expected the non-negative-bound refusal, got {body}"
            );
            let (_, described) = query(
                &router,
                "DescribeAutoScalingGroups",
                &[("AutoScalingGroupNames.member.1", name.as_str())],
            )
            .await;
            assert!(
                tag_values(&described, "AutoScalingGroupName").is_empty(),
                "case {case:?}: a refused create must leave no group behind, got {described}"
            );
        }

        // The UPDATE path, on a group whose bounds the refused request must leave alone.
        let base = format!("{name}-base");
        let (status, body) = create_group(
            &router,
            &base,
            &[
                ("MinSize", "1".to_string()),
                ("MaxSize", "3".to_string()),
                ("DesiredCapacity", "1".to_string()),
            ],
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
                ("MinSize", min_size.to_string().as_str()),
            ],
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served, got {status}: {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the update to be refused, got {status}: {body}"
            );
            assert_eq!(
                refusal(&body),
                (
                    "ValidationError".to_string(),
                    negative_min_size_message(min_size)
                ),
                "case {case:?}: expected the same sentence on the update path, got {body}"
            );
            let sizes = described_sizes(&router, &base).await;
            assert_eq!(
                sizes,
                (
                    "1".to_string(),
                    "3".to_string(),
                    "1".to_string()
                ),
                "case {case:?}: a refused update must leave the group's sizes alone, got {sizes:?}"
            );
        }

        // The SCHEDULED ACTION that would write those same bounds later.
        let (status, body) = query(
            &router,
            "PutScheduledUpdateGroupAction",
            &[
                ("AutoScalingGroupName", base.as_str()),
                ("ScheduledActionName", "later"),
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MinSize", min_size.to_string().as_str()),
            ],
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the scheduled action to be served, got {status}: {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the scheduled action to be refused, got {status}: {body}"
            );
            assert_eq!(
                refusal(&body),
                (
                    "ValidationError".to_string(),
                    negative_min_size_message(min_size)
                ),
                "case {case:?}: expected the same sentence from the scheduled action, got {body}"
            );
        }
    }
}

/// One row of the negative-capacity table: the bounds the group is configured with, and the
/// capacity the change asks for.
struct SignRow {
    case: &'static str,
    min: i32,
    max: i32,
    asked_for: i32,
    /// `None` for a capacity the service serves.
    expected: Option<String>,
}

/// RULE: `SetDesiredCapacity` refuses a negative capacity for its SIGN, ahead of either
/// bound of the group, and a capacity of zero is weighed against the group's minimum like
/// any other non-negative one.
#[tokio::test]
async fn a_negative_capacity_change_is_refused_for_its_sign_not_against_a_bound() {
    let rows = [
        SignRow {
            case: "one below zero, on a group whose minimum is zero",
            min: 0,
            max: 3,
            asked_for: -1,
            expected: Some(negative_capacity_message(-1)),
        },
        SignRow {
            case: "one below zero, on a group whose minimum is above zero",
            min: 1,
            max: 3,
            asked_for: -1,
            expected: Some(negative_capacity_message(-1)),
        },
        SignRow {
            case: "several below zero",
            min: 0,
            max: 3,
            asked_for: -9,
            expected: Some(negative_capacity_message(-9)),
        },
        SignRow {
            case: "the bottom of the modelled integer range",
            min: 0,
            max: 3,
            asked_for: i32::MIN,
            expected: Some(negative_capacity_message(i32::MIN)),
        },
        SignRow {
            case: "zero, below a minimum of one: the bound refusal, not the sign one",
            min: 1,
            max: 3,
            asked_for: 0,
            expected: Some(below_min_message(0, 1)),
        },
        SignRow {
            case: "zero, which a minimum of zero admits",
            min: 0,
            max: 3,
            asked_for: 0,
            expected: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = new_engine();
        let router = router_with(engine);
        let case = row.case;
        let name = format!("capacity-sign-{index}");
        // The group starts at its own minimum, so a refused change is visible as the
        // capacity it left behind.
        let (status, body) = create_group(
            &router,
            &name,
            &[
                ("MinSize", row.min.to_string()),
                ("MaxSize", row.max.to_string()),
                ("DesiredCapacity", row.min.to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the group must be created, got {status}: {body}"
        );

        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", name.as_str()),
                ("DesiredCapacity", row.asked_for.to_string().as_str()),
            ],
        )
        .await;
        match row.expected.as_deref() {
            Some(expected) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the change to be refused, got {status}: {body}"
                );
                assert_eq!(
                    refusal(&body),
                    ("ValidationError".to_string(), expected.to_string()),
                    "case {case:?}: expected that sentence, got {body}"
                );
                assert!(
                    !xml_unescape(&tag_value(&body, "Message")).contains(name.as_str()),
                    "case {case:?}: the sentence names no group, got {body}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the change to be served, got {status}: {body}"
            ),
        }

        let (_, _, desired) = described_sizes(&router, &name).await;
        let expected_capacity = if row.expected.is_some() {
            row.min
        } else {
            row.asked_for
        };
        assert_eq!(
            desired,
            expected_capacity.to_string(),
            "case {case:?}: expected the group to hold {expected_capacity}, got {desired:?}"
        );
        // A refused change launches NOTHING: the group holds one instance per unit of the
        // capacity it kept, and no more.
        let (_, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        let instances = tag_values(&described, "InstanceId").len();
        assert_eq!(
            instances,
            usize::try_from(expected_capacity).expect("a capacity this row states is a count"),
            "case {case:?}: expected the group to hold {expected_capacity} instance(s), got \
             {instances} in {described}"
        );
    }
}

/// RULE: a `MaxSize` is headroom rather than instances, so a large one is accepted and read
/// back verbatim — by the create, by the update, and by the scheduled action that writes it
/// later.
#[tokio::test]
async fn large_headroom_bounds_are_accepted_and_round_trip_verbatim() {
    // (case, the MaxSize the request states)
    let rows: [(&str, i32); 4] = [
        ("a maximum on the ceiling", MAX_GROUP_CAPACITY),
        ("a maximum one past the ceiling", MAX_GROUP_CAPACITY + 1),
        ("a maximum far past the ceiling", 999_999_999),
        ("a maximum at the top of the modelled range", TOP_OF_THE_RANGE),
    ];

    for (index, (case, max_size)) in rows.iter().enumerate() {
        let engine = new_engine();
        let router = router_with(engine);
        let name = format!("headroom-{index}");

        // The CREATE path: a group asked to run nothing is created whatever headroom its
        // maximum leaves it.
        let (status, body) = create_group(
            &router,
            &name,
            &[
                ("MinSize", "0".to_string()),
                ("MaxSize", max_size.to_string()),
                ("DesiredCapacity", "0".to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status}: {body}"
        );
        let sizes = described_sizes(&router, &name).await;
        assert_eq!(
            sizes,
            (
                "0".to_string(),
                max_size.to_string(),
                "0".to_string()
            ),
            "case {case:?}: expected the maximum to round-trip verbatim, got {sizes:?}"
        );

        // The UPDATE path: the same headroom raised onto a group that already exists.
        let base = format!("{name}-base");
        let (status, body) = create_group(
            &router,
            &base,
            &[
                ("MinSize", "0".to_string()),
                ("MaxSize", "1".to_string()),
                ("DesiredCapacity", "0".to_string()),
            ],
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
                ("MaxSize", max_size.to_string().as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be served, got {status}: {body}"
        );
        let sizes = described_sizes(&router, &base).await;
        assert_eq!(
            sizes,
            (
                "0".to_string(),
                max_size.to_string(),
                "0".to_string()
            ),
            "case {case:?}: expected the raised maximum to round-trip verbatim, got {sizes:?}"
        );

        // The SCHEDULED ACTION path: the same headroom written for later.
        let (status, body) = query(
            &router,
            "PutScheduledUpdateGroupAction",
            &[
                ("AutoScalingGroupName", base.as_str()),
                ("ScheduledActionName", "headroom-later"),
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MaxSize", max_size.to_string().as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the scheduled action to be served, got {status}: {body}"
        );
        let (_, described) = query(
            &router,
            "DescribeScheduledActions",
            &[("AutoScalingGroupName", base.as_str())],
        )
        .await;
        assert_eq!(
            tag_value(&described, "MaxSize"),
            max_size.to_string(),
            "case {case:?}: expected the stored action to report the maximum verbatim, got \
             {described}"
        );
    }
}

/// One row of the ceiling table: the three sizes a create states, and the capacity the
/// refusal names (`None` for a request the service serves).
struct CeilingRow {
    case: &'static str,
    min: i32,
    max: i32,
    desired: Option<i32>,
    refused_capacity: Option<i32>,
}

/// RULE: what the per-group instance ceiling holds is the capacity a request asks the group
/// to RUN — the desired capacity it states, else the one its bounds leave the group at —
/// and the refusal names that capacity.
#[tokio::test]
async fn the_ceiling_holds_the_capacity_a_request_asks_the_group_to_run() {
    let rows = [
        CeilingRow {
            case: "a capacity one past the ceiling, inside a maximum that admits it",
            min: 0,
            max: TOP_OF_THE_RANGE,
            desired: Some(MAX_GROUP_CAPACITY + 1),
            refused_capacity: Some(MAX_GROUP_CAPACITY + 1),
        },
        CeilingRow {
            case: "a minimum one past the ceiling, which the capacity follows",
            min: MAX_GROUP_CAPACITY + 1,
            max: MAX_GROUP_CAPACITY + 1,
            desired: None,
            refused_capacity: Some(MAX_GROUP_CAPACITY + 1),
        },
        CeilingRow {
            case: "every size at the top of the modelled range",
            min: TOP_OF_THE_RANGE,
            max: TOP_OF_THE_RANGE,
            desired: Some(TOP_OF_THE_RANGE),
            refused_capacity: Some(TOP_OF_THE_RANGE),
        },
        CeilingRow {
            case: "a capacity on the ceiling, which the group may hold",
            min: 0,
            max: MAX_GROUP_CAPACITY,
            desired: Some(MAX_GROUP_CAPACITY),
            refused_capacity: None,
        },
        CeilingRow {
            case: "a capacity well inside the ceiling, under a maximum far past it",
            min: 0,
            max: 999_999_999,
            desired: Some(2),
            refused_capacity: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = new_engine();
        let router = router_with(engine);
        let case = row.case;
        let name = format!("ceiling-{index}");
        let mut sizes = vec![
            ("MinSize", row.min.to_string()),
            ("MaxSize", row.max.to_string()),
        ];
        if let Some(desired) = row.desired {
            sizes.push(("DesiredCapacity", desired.to_string()));
        }
        let (status, body) = create_group(&router, &name, &sizes).await;
        match row.refused_capacity {
            Some(capacity) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the create to be refused, got {status}: {body}"
                );
                assert_eq!(
                    refusal(&body),
                    (
                        "LimitExceeded".to_string(),
                        group_size_past_ceiling_message(capacity)
                    ),
                    "case {case:?}: expected the ceiling refusal naming the capacity, got {body}"
                );
                let (_, described) = query(
                    &router,
                    "DescribeAutoScalingGroups",
                    &[("AutoScalingGroupNames.member.1", name.as_str())],
                )
                .await;
                assert!(
                    tag_values(&described, "AutoScalingGroupName").is_empty(),
                    "case {case:?}: a refused create must leave no group behind, got {described}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to be served, got {status}: {body}"
                );
                let sizes = described_sizes(&router, &name).await;
                assert_eq!(
                    sizes,
                    (
                        row.min.to_string(),
                        row.max.to_string(),
                        row.desired.unwrap_or(row.min).to_string()
                    ),
                    "case {case:?}: expected the sizes to round-trip, got {sizes:?}"
                );
            }
        }
    }
}

/// RULE, on the stored group: the ceiling still holds every capacity a group with large
/// headroom is driven to, and a capacity inside it is applied and read back.
#[tokio::test]
async fn a_group_with_large_headroom_is_still_held_to_the_ceiling_when_it_runs() {
    let engine = new_engine();
    let router = router_with(engine);
    let name = "headroom-runs";
    let (status, body) = create_group(
        &router,
        name,
        &[
            ("MinSize", "0".to_string()),
            ("MaxSize", 999_999_999.to_string()),
            ("DesiredCapacity", "0".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group with headroom must be created, got {status}: {body}"
    );

    // Its own maximum admits this capacity; the ceiling on what this process stores does
    // not, and the refusal names the capacity asked for.
    let past = MAX_GROUP_CAPACITY + 1;
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", name),
            ("DesiredCapacity", past.to_string().as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "a capacity past the ceiling must be refused, got {status}: {body}"
    );
    assert_eq!(
        refusal(&body),
        (
            "ValidationError".to_string(),
            capacity_past_ceiling_message(past)
        ),
        "expected the capacity-ceiling refusal, got {body}"
    );
    let (_, _, desired) = described_sizes(&router, name).await;
    assert_eq!(
        desired, "0",
        "a refused capacity change must leave the group where it was, got {desired:?}"
    );

    // A capacity inside the ceiling is applied, so the headroom group is a working group
    // rather than one that can never be driven.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", name), ("DesiredCapacity", "2")],
    )
    .await;
    assert_eq!(
        status, 200,
        "a capacity inside the ceiling must be served, got {status}: {body}"
    );
    let sizes = described_sizes(&router, name).await;
    assert_eq!(
        sizes,
        (
            "0".to_string(),
            999_999_999.to_string(),
            "2".to_string()
        ),
        "expected the applied capacity beside the stored headroom, got {sizes:?}"
    );
}
