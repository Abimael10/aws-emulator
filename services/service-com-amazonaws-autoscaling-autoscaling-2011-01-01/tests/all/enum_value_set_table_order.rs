// SPDX-License-Identifier: CC0-1.0

//! The ORDER a refusal lists an enumerated type's value set in, over the enumerated types of
//! this model at large.
//!
//! The rule under test: **a request carrying a value outside an enumerated type is refused
//! with that type's admissible values listed in the one order this service lists a value set
//! in — the values read out of a hash table keyed on each value's own text rather than out of
//! the type's declaration. The order belongs to the TYPE, so it is the same wherever that type
//! is carried: on any operation, at any depth, and whether or not the service publishes values
//! for it beyond the ones the shipped model declares.**
//!
//! Why it matters: a client that pins a refusal's wording — a test fixture, a diff against a
//! recorded answer, documentation generated from a refusal — reads the whole clause. A clause
//! that lists the values in the model's declaration order is a message no caller of the real
//! service ever receives, so every such comparison reports a difference that is not one.
//!
//! `published_enum_value_set_order.rs` beside this suite pins the one type this service
//! publishes an extra value for, and everything about the clause around the set; this one pins
//! the ORDER over the types it publishes nothing extra for, which is where the order is the
//! front door's alone. The rows differ along what the rule generalises over: WHICH enumerated
//! type is carried, WHICH operation carries it, and whether the member is a top-level one or
//! nested inside a structure the request declares.

use service_common::validation_message::hashed_enum_order;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The launch source the group below is created from — a launch TEMPLATE the account holds,
/// because no account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The group the rows that address an existing group name.
const GROUP: &str = "asg-enum-table-order";

/// One row: the request that carries a value outside an enumerated type, and the type's own
/// admissible values as the shipped model declares them.
struct Row {
    case: &'static str,
    action: &'static str,
    /// The request members beside the offending one, so the refusal is about THAT member and
    /// not about a member the operation requires.
    params: &'static [(&'static str, &'static str)],
    /// The awsQuery parameter that carries the offending value, and the value itself.
    parameter: &'static str,
    rejected: &'static str,
    /// The member as the boundary validator names it in the clause (lowerCamel, a nested
    /// member below its structure's own name).
    path: &'static str,
    /// The values the type admits, in the order the model declares them
    /// (`smithy_query(query="shape",shape="...")`).
    declared: &'static [&'static str],
}

/// `DeletionProtection`, which the model declares as `none | prevent-force-deletion |
/// prevent-all-deletion` (`smithy_query(query="shape",shape="DeletionProtection")`).
const DELETION_PROTECTION: &[&str] = &["none", "prevent-force-deletion", "prevent-all-deletion"];

/// `WarmPoolState`: `Stopped | Running | Hibernated`.
const WARM_POOL_STATE: &[&str] = &["Stopped", "Running", "Hibernated"];

/// `ScaleInProtectedInstances`: `Refresh | Ignore | Wait`.
const SCALE_IN_PROTECTED_INSTANCES: &[&str] = &["Refresh", "Ignore", "Wait"];

/// `ImpairedZoneHealthCheckBehavior`: `ReplaceUnhealthy | IgnoreUnhealthy`.
const IMPAIRED_ZONE_HEALTH_CHECK_BEHAVIOR: &[&str] = &["ReplaceUnhealthy", "IgnoreUnhealthy"];

/// The members a create needs beside the one at fault, so no clause but the enumerated one is
/// reported.
const CREATE_MEMBERS: &[(&str, &str)] = &[
    ("AutoScalingGroupName", "asg-enum-table-order-created"),
    ("MinSize", "0"),
    ("MaxSize", "1"),
    ("AvailabilityZones.member.1", "us-west-2a"),
    LAUNCH_SOURCE,
];

/// The members an update and a warm-pool put need: the group they address.
const GROUP_MEMBERS: &[(&str, &str)] = &[("AutoScalingGroupName", GROUP)];

const ROWS: &[Row] = &[
    Row {
        case: "a create's own deletion protection",
        action: "CreateAutoScalingGroup",
        params: CREATE_MEMBERS,
        parameter: "DeletionProtection",
        rejected: "prevent-every-deletion",
        path: "deletionProtection",
        declared: DELETION_PROTECTION,
    },
    Row {
        // The SAME type on another operation: the order is the type's, not the call's.
        case: "an update's deletion protection",
        action: "UpdateAutoScalingGroup",
        params: GROUP_MEMBERS,
        parameter: "DeletionProtection",
        rejected: "prevent-nothing",
        path: "deletionProtection",
        declared: DELETION_PROTECTION,
    },
    Row {
        case: "a warm pool's state",
        action: "PutWarmPool",
        params: GROUP_MEMBERS,
        parameter: "PoolState",
        rejected: "Warmed",
        path: "poolState",
        declared: WARM_POOL_STATE,
    },
    Row {
        // A member NESTED inside a structure the request declares.
        case: "an instance refresh's preference for protected instances",
        action: "StartInstanceRefresh",
        params: GROUP_MEMBERS,
        parameter: "Preferences.ScaleInProtectedInstances",
        rejected: "Replace",
        path: "preferences.scaleInProtectedInstances",
        declared: SCALE_IN_PROTECTED_INSTANCES,
    },
    Row {
        // A nested member of a create, on a type of only two values.
        case: "a create's impaired-zone health check behavior",
        action: "CreateAutoScalingGroup",
        params: CREATE_MEMBERS,
        parameter: "AvailabilityZoneImpairmentPolicy.ImpairedZoneHealthCheckBehavior",
        rejected: "ReplaceEverything",
        path: "availabilityZoneImpairmentPolicy.impairedZoneHealthCheckBehavior",
        declared: IMPAIRED_ZONE_HEALTH_CHECK_BEHAVIOR,
    },
];

/// Create the group the rows that address one name, and assert the create succeeded.
async fn create_group(router: &MultiServiceRouter) {
    let params = vec![
        p("AutoScalingGroupName", GROUP),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", "us-west-2a"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating the group {GROUP:?}: expected HTTP 200, got {status} with {body:?}"
    );
}

/// The values a clause lists between its brackets, in the order it lists them.
fn stated_values(case: &str, message: &str) -> Vec<String> {
    message
        .split_once("enum value set: [")
        .unwrap_or_else(|| {
            panic!("case {case:?}: the message must state a value set, got {message:?}")
        })
        .1
        .trim_end_matches(']')
        .split(", ")
        .map(ToString::to_string)
        .collect()
}

#[tokio::test]
async fn every_enumerated_types_value_set_is_listed_in_the_services_table_order() {
    let router = router_with(new_engine());
    create_group(&router).await;

    for row in ROWS {
        let case = row.case;
        let mut params: Vec<(String, String)> = row
            .params
            .iter()
            .map(|(key, value)| p(key, *value))
            .collect();
        params.push(p(row.parameter, row.rejected));
        let (status, body) = query(&router, row.action, &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the set must be refused, got {status} with {body:?}"
        );
        assert_eq!(
            (
                tag_value(&body, "Type").as_str(),
                tag_value(&body, "Code").as_str()
            ),
            ("Sender", "ValidationError"),
            "case {case:?}: expected a Sender ValidationError, got {body:?}"
        );

        // The WHOLE message, so a row cannot pass by refusing for another reason: the
        // framework's own clause, with the value set in the order this service lists one in.
        let message = xml_unescape(&tag_value(&body, "Message"));
        let derived: Vec<String> = hashed_enum_order(row.declared)
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        let expected = format!(
            "1 validation error detected: Value '{}' at '{}' failed to satisfy constraint: \
             Member must satisfy enum value set: [{}]",
            row.rejected,
            row.path,
            derived.join(", ")
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );

        // The MEMBERS of the stated set are the values the type admits and nothing else...
        let stated = stated_values(case, &message);
        let mut stated_members = stated.clone();
        stated_members.sort_unstable();
        let mut admitted: Vec<String> =
            row.declared.iter().map(|value| (*value).to_string()).collect();
        admitted.sort_unstable();
        assert_eq!(
            stated_members, admitted,
            "case {case:?}: expected the type's own values, got {message:?}"
        );

        // ...and for a type whose table order is not its declaration order, the clause lists
        // NEITHER the model's order nor a sort of the set — so a regression to the framework's
        // own rendering of the values fails here.
        let model_order: Vec<String> =
            row.declared.iter().map(|value| (*value).to_string()).collect();
        if derived != model_order {
            assert_ne!(
                stated, model_order,
                "case {case:?}: the clause must not list the model's declaration order"
            );
        }
        if derived != admitted {
            assert_ne!(
                stated, admitted,
                "case {case:?}: the clause must not list the set sorted"
            );
        }
    }
}

/// Every value each of those types DOES admit is accepted, so the refusals above cannot pass
/// by refusing every value of the member.
#[tokio::test]
async fn every_declared_value_of_those_types_is_accepted() {
    let router = router_with(new_engine());
    create_group(&router).await;

    for value in DELETION_PROTECTION {
        let params = vec![p("AutoScalingGroupName", GROUP), p("DeletionProtection", *value)];
        let (status, body) =
            query(&router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {value:?}: the update must be accepted, got {status} with {body:?}"
        );
        let (status, read) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", GROUP)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {value:?}: the read back must succeed, got {status} with {read:?}"
        );
        assert_eq!(
            tag_value(&read, "DeletionProtection").as_str(),
            *value,
            "case {value:?}: the group must carry the protection it was updated with, got \
             {read:?}"
        );
    }

    // ...and every state the pool's own enum admits, on the same group: deletion protection
    // governs the DELETE of a group and nothing a warm-pool put asks for, so the group is left
    // carrying whatever the loop above put on it.
    for value in WARM_POOL_STATE {
        let params = vec![p("AutoScalingGroupName", GROUP), p("PoolState", *value)];
        let (status, body) = query(&router, "PutWarmPool", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {value:?}: the warm-pool put must be accepted, got {status} with {body:?}"
        );
        let (status, read) = query(
            &router,
            "DescribeWarmPool",
            &[("AutoScalingGroupName", GROUP)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {value:?}: the read back must succeed, got {status} with {read:?}"
        );
        assert_eq!(
            tag_value(&read, "PoolState").as_str(),
            *value,
            "case {value:?}: the pool must carry the state it was put with, got {read:?}"
        );
    }
}
