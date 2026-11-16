// SPDX-License-Identifier: CC0-1.0

//! Whether a reported load balancer carries a `SecurityGroups` member at all.
//!
//! `LoadBalancer$SecurityGroups` (`smithy_query(query="shape",shape="LoadBalancer")`) is an
//! OPTIONAL list — "the IDs of the security groups for the load balancer" — and an absent
//! list and an empty list are different answers to "does this load balancer have security
//! groups": an SDK caller tests whether the member is there, so reporting `[]` for a load
//! balancer that has none makes a group-less load balancer look group-capable.
//!
//! The rule the rows below assert is that the member carries the groups the load balancer
//! holds and is absent when it holds none, on the create's own answer and on every later
//! read of it alike.
//!
//! The rows differ along what that generalises over: the load balancer type (one of each
//! family the model names), whether the request named security groups, and how many it
//! named.

use crate::provisioning_loop::{action, call, emulator, members, p, Reply};
use smithy_server_runtime::MultiServiceRouter;

/// One row: the case, the `Type` the request names, and the security groups it names.
struct LoadBalancerRow {
    case: &'static str,
    lb_type: &'static str,
    security_groups: &'static [&'static str],
}

/// The rows: one per family the model names, crossed with whether and how many security
/// groups the request names.
///
/// The ids are of the form EC2 writes one in, which is all this service reads off the
/// value here: no EC2 answers in this suite's router, so nothing about the groups is
/// resolved and the ids are stored as named.
const ROWS: &[LoadBalancerRow] = &[
    LoadBalancerRow {
        case: "a network load balancer with no security groups",
        lb_type: "network",
        security_groups: &[],
    },
    LoadBalancerRow {
        case: "a network load balancer with one security group",
        lb_type: "network",
        security_groups: &["sg-11111111"],
    },
    LoadBalancerRow {
        case: "a network load balancer with several security groups",
        lb_type: "network",
        security_groups: &["sg-11111111", "sg-22222222"],
    },
    LoadBalancerRow {
        case: "an application load balancer with no security groups",
        lb_type: "application",
        security_groups: &[],
    },
    LoadBalancerRow {
        case: "an application load balancer with one security group",
        lb_type: "application",
        security_groups: &["sg-33333333"],
    },
    LoadBalancerRow {
        case: "a gateway load balancer, which carries none",
        lb_type: "gateway",
        security_groups: &[],
    },
];

/// Read one load balancer back by ARN.
async fn describe(router: &MultiServiceRouter, arn: &str) -> Reply {
    let mut form = action("DescribeLoadBalancers");
    form.push(p("LoadBalancerArns.member.1", arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "a read of a load balancer must be accepted, got {} body {}",
        reply.status, reply.body
    );
    reply
}

/// Whether a reply carries a `SecurityGroups` member at all, and the ids inside it.
///
/// The member is a list, so its presence is the presence of the CONTAINER element: a body
/// reporting `<SecurityGroups/>` carries the member with nothing in it, which is exactly
/// the answer this rule is about.
fn reported_security_groups(body: &str) -> (bool, Vec<String>) {
    (body.contains("<SecurityGroups"), members(body, "SecurityGroups"))
}

#[tokio::test]
async fn a_load_balancer_reports_security_groups_only_when_it_has_some() {
    let (router, _engine) = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("sg-report-{index}");
        let mut form = action("CreateLoadBalancer");
        form.push(p("Name", &name));
        form.push(p("Type", row.lb_type));
        form.push(p("Subnets.member.1", "subnet-11111111"));
        form.push(p("Subnets.member.2", "subnet-22222222"));
        for (position, group) in row.security_groups.iter().enumerate() {
            form.push(p(
                &format!("SecurityGroups.member.{}", position + 1),
                group,
            ));
        }
        let created = call(&router, &form).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected the create to be accepted, got {} body {}",
            row.case, created.status, created.body
        );
        let arn = created
            .text("LoadBalancerArn")
            .unwrap_or_else(|| panic!("case {}: no LoadBalancerArn in {}", row.case, created.body));

        let expected: Vec<String> = row
            .security_groups
            .iter()
            .map(|group| (*group).to_string())
            .collect();
        for (reader, body) in [
            ("the create's own answer", created.body.clone()),
            ("a later read", describe(&router, &arn).await.body),
        ] {
            assert_eq!(
                reported_security_groups(&body),
                (!expected.is_empty(), expected.clone()),
                "case {} ({reader}): a load balancer reports the groups it holds and no \
                 member at all when it holds none, got {body}",
                row.case
            );
        }
    }
}
