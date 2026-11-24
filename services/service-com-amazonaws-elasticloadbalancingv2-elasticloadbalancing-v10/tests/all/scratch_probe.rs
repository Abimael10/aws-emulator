// SPDX-License-Identifier: CC0-1.0

//! One probe of the whole create path this fix settles: what `CreateTargetGroup` ANSWERS
//! and what a later `DescribeTargetGroups` reports are the same record, member for member.
//!
//! The two suites beside this one table the rules themselves — the members a `lambda`
//! target group has (`target_group_lambda_network_members`) and the protocol version and
//! health-check defaults a target group takes
//! (`target_group_protocol_version_and_health_check_defaults`). This one asserts the
//! invariant both rest on: the create reports the record it STORED, so no member can be
//! right in the answer and wrong in the store.

use crate::provisioning_loop::{action, call, emulator, p};
use crate::target_group_health_check_schedule::describe_by_name;

/// The members the create settles and a read projects, as the element names the awsQuery
/// response carries.
const SETTLED_MEMBERS: &[&str] = &[
    "Protocol",
    "Port",
    "VpcId",
    "ProtocolVersion",
    "HealthCheckProtocol",
    "HealthCheckPort",
    "HealthCheckPath",
    "HealthCheckEnabled",
    "HealthCheckIntervalSeconds",
    "HealthCheckTimeoutSeconds",
    "HealthyThresholdCount",
    "UnhealthyThresholdCount",
    "TargetType",
    "IpAddressType",
];

#[tokio::test]
async fn the_create_answers_the_target_group_a_later_read_reports() {
    let (router, _engine) = emulator();

    // (the case, the members the create carries beyond `Name`)
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "a target group taking every default",
            &[("Protocol", "HTTPS"), ("Port", "443"), ("VpcId", "vpc-1")],
        ),
        (
            "a target group stating its health check and carrying an empty protocol version",
            &[
                ("Protocol", "HTTPS"),
                ("Port", "443"),
                ("VpcId", "vpc-1"),
                ("ProtocolVersion", ""),
                ("HealthCheckProtocol", "HTTPS"),
                ("HealthCheckTimeoutSeconds", "5"),
            ],
        ),
        ("a target group whose target is invoked", &[("TargetType", "lambda")]),
    ];

    for (index, (case, fields)) in cases.iter().enumerate() {
        let name = format!("settled-{index}");
        let mut form = action("CreateTargetGroup");
        form.push(p("Name", &name));
        for (key, value) in fields.iter() {
            form.push(p(key, value));
        }
        let created = call(&router, &form).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected the create to be served, got {} {}",
            created.status, created.body
        );

        let described = describe_by_name(&router, &name).await;
        assert_eq!(
            described.status, 200,
            "case {case}: expected the stored target group to be reported, got {} {}",
            described.status, described.body
        );
        let answered: Vec<(&str, Option<String>)> = SETTLED_MEMBERS
            .iter()
            .map(|member| (*member, created.text(member)))
            .collect();
        let reported: Vec<(&str, Option<String>)> = SETTLED_MEMBERS
            .iter()
            .map(|member| (*member, described.text(member)))
            .collect();
        assert_eq!(
            answered, reported,
            "case {case}: the create must answer the record it stored, got {} and {}",
            created.body, described.body
        );
    }
}
