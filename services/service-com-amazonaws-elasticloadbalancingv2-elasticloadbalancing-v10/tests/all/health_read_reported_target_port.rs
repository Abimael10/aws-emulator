// SPDX-License-Identifier: CC0-1.0

//! The port a health read reports for EACH target one request names, over the wire.
//!
//! RULE: `DescribeTargetHealth` "describes the health of the specified targets or all of your
//! targets" and answers one `TargetHealthDescription` per target the request named, so the rule
//! that decides a row's `Target$Port` — the port the request named for that target, else the
//! target group's own `TargetGroup$Port` ("the port on which the targets receive traffic", which
//! a registration may override) — is read per ROW, from the one target that row is about. A
//! request naming several targets at once is therefore answered target by target, in the order
//! it named them, each row carrying the port traffic to ITS target goes to and the health-check
//! port the group's configuration gives that same target, whether or not the group holds a
//! registration for it.
//!
//! The row-wise form of that rule over ONE target, and the group ports it defaults from, are
//! `crate::target_id_forms_and_reported_ports`'s tables; this suite reads the rule where one
//! request carries several targets, and reads a health row through that sibling's harness
//! ([`reported_for`], [`target_group_on`], [`ReportedTarget`]), so the two suites cannot drift
//! on how a row is read.
//!
//! The targets this read names differ along what the rule generalises over: a target that is
//! registered and one that never was, an id-only target and one named on a port of its own, and
//! the same id named twice on two different ports — which are two targets, not one.

use crate::provisioning_loop::emulator;
use crate::target_id_forms_and_reported_ports::{
    one_target, reported_for, target_group_on, ReportedTarget,
};

/// The health state a target of a target group no load balancer forwards to is under: nothing
/// is checking it, whether it is registered (`Target.NotInUse`) or was never registered
/// (`Target.NotRegistered`).
const UNUSED: &str = "unused";
const NOT_IN_USE: &str = "Target.NotInUse";
const NOT_REGISTERED: &str = "Target.NotRegistered";

/// One target the read names: the id, the port it names beside it, and the row that target must
/// be answered with.
struct NamedTarget {
    case: &'static str,
    id: &'static str,
    names_port: Option<&'static str>,
    /// Whether the target group holds a registration of `id` on its own port before the read.
    registered: bool,
    reports_port: &'static str,
    reason: &'static str,
}

/// The group's own port and health-check port, which every row below defaults from: a traffic
/// port no default picks and a health-check port of the group's own, so a row that reported one
/// in place of the other, or either in place of a default, fails this suite.
const GROUP_PORT: &str = "44300";
const GROUP_HEALTH_CHECK_PORT: &str = "4000";

#[tokio::test]
async fn a_read_of_several_targets_answers_each_of_them_in_the_order_named() {
    let (router, _engine) = emulator();
    let case = "several targets in one read";
    let arn = target_group_on(
        &router,
        case,
        "health-read-many",
        "instance",
        Some(GROUP_PORT),
        Some(GROUP_HEALTH_CHECK_PORT),
    )
    .await;

    let named: &[NamedTarget] = &[
        NamedTarget {
            case: "an id that was never registered, named by id alone",
            id: "i-00000000000000088",
            names_port: None,
            registered: false,
            reports_port: GROUP_PORT,
            reason: NOT_REGISTERED,
        },
        NamedTarget {
            case: "a registered target, named by id alone",
            id: "i-00000000000000077",
            names_port: None,
            registered: true,
            reports_port: GROUP_PORT,
            reason: NOT_IN_USE,
        },
        NamedTarget {
            case: "the unregistered id again, on a port of its own",
            id: "i-00000000000000088",
            names_port: Some("44301"),
            registered: false,
            reports_port: "44301",
            reason: NOT_REGISTERED,
        },
        NamedTarget {
            case: "another unregistered id, named by id alone",
            id: "i-00000000000000099",
            names_port: None,
            registered: false,
            reports_port: GROUP_PORT,
            reason: NOT_REGISTERED,
        },
    ];

    for target in named.iter().filter(|target| target.registered) {
        let reply = one_target(&router, "RegisterTargets", &arn, target.id, None).await;
        assert_eq!(
            reply.status, 200,
            "case {}: the registration must be accepted, got {} body {}",
            target.case, reply.status, reply.body
        );
    }

    let request: Vec<(String, Option<&str>)> = named
        .iter()
        .map(|target| (target.id.to_string(), target.names_port))
        .collect();
    let reported = reported_for(&router, &arn, &request).await;
    let expected: Vec<ReportedTarget> = named
        .iter()
        .map(|target| ReportedTarget {
            id: Some(target.id.to_string()),
            port: Some(target.reports_port.to_string()),
            health_check_port: Some(GROUP_HEALTH_CHECK_PORT.to_string()),
            state: Some(UNUSED.to_string()),
            reason: Some(target.reason.to_string()),
        })
        .collect();
    assert_eq!(
        reported.len(),
        named.len(),
        "case {case}: expected one row per named target ({}), got {reported:?}",
        named.len()
    );
    for (index, target) in named.iter().enumerate() {
        assert_eq!(
            reported[index], expected[index],
            "case {case}, target {} ({}): expected {:?}, got {:?}",
            index + 1,
            target.case,
            expected[index],
            reported[index]
        );
    }
}
