// SPDX-License-Identifier: CC0-1.0

//! The members a health check carries only when its probe speaks HTTP semantics, over
//! the wire, on both operations that write a health check.
//!
//! `HealthCheckPath` ("[HTTP/HTTPS health checks] The destination for health checks on
//! the targets") and `Matcher` ("[HTTP/HTTPS health checks] The HTTP or gRPC codes to use
//! when checking for a successful response from a target") belong to an HTTP-side probe
//! alone (`smithy_query(query="operation",operation="CreateTargetGroup")`). The model
//! ties neither to `HealthCheckProtocol` — each is an optional member of its own — so the
//! handler is the only gate.
//!
//! The rule these tables pin: a write naming either member for a probe that will not
//! speak HTTP or HTTPS is refused HTTP 400 `InvalidConfigurationRequest` and stores
//! nothing, and the protocol it is judged on is the EFFECTIVE one — what the request
//! names, else the default the member documents for that target group on a create ("For
//! Application Load Balancers, the default is HTTP. For Network Load Balancers and
//! Gateway Load Balancers, the default is TCP"), else what the group already stores on a
//! modify. A `lambda` target group reports no probe protocol at all and its check is an
//! HTTP one, so it carries both members.
//!
//! The rows differ along what the rule generalises over: the target group's own protocol
//! (every family `Protocol` names), whether the request states the probe protocol or
//! leaves it to the default, which of the two members it names (either, both), and
//! whether the resulting probe is an HTTP one at all.
//!
//! The wire harness (`emulator`, `call`, `action`, `p`) is `provisioning_loop`'s and the
//! target-group helpers are `target_group_health_check_port`'s; nothing here restates
//! either.

use crate::provisioning_loop::{action, call, emulator, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;
use crate::target_group_health_check_port::{create_group, describe, health_check_of};
use smithy_server_runtime::MultiServiceRouter;

/// The modelled refusal for a combination of members no target group can hold
/// (`CreateTargetGroup`/`ModifyTargetGroup` errors include
/// `InvalidConfigurationRequestException`, awsQueryError
/// `InvalidConfigurationRequest`/400).
const INVALID_CONFIGURATION_REQUEST: &str = "InvalidConfigurationRequest";

/// One target group the rule is asked of: the case name, the fields of the create besides
/// `Name`, and the probe protocol the stored health check ends up speaking — empty when
/// the group reports no probe protocol at all, which is a `lambda` one's.
type GroupRow = (&'static str, &'static [(&'static str, &'static str)], &'static str);

/// The target groups the table runs over: one per protocol family, with the probe
/// protocol stated and left to the default, plus the shapes whose probe IS an HTTP one.
const GROUP_ROWS: &[GroupRow] = &[
    // Probes that do not speak HTTP, so neither member may be named.
    (
        "a TCP group with a TCP probe",
        &[
            ("Protocol", "TCP"),
            ("Port", "44300"),
            ("VpcId", "vpc-1"),
            ("HealthCheckProtocol", "TCP"),
        ],
        "TCP",
    ),
    (
        "a TCP group leaving the probe protocol to the default",
        &[("Protocol", "TCP"), ("Port", "1234"), ("VpcId", "vpc-1")],
        "TCP",
    ),
    (
        "a TLS group leaving the probe protocol to the default",
        &[("Protocol", "TLS"), ("Port", "443"), ("VpcId", "vpc-1")],
        "TCP",
    ),
    (
        "a UDP group with a TCP probe",
        &[
            ("Protocol", "UDP"),
            ("Port", "53"),
            ("VpcId", "vpc-1"),
            ("HealthCheckProtocol", "TCP"),
        ],
        "TCP",
    ),
    (
        "a TCP_UDP group leaving the probe protocol to the default",
        &[("Protocol", "TCP_UDP"), ("Port", "1234"), ("VpcId", "vpc-1")],
        "TCP",
    ),
    (
        "a GENEVE group leaving the probe protocol to the default",
        &[("Protocol", "GENEVE"), ("Port", "6081"), ("VpcId", "vpc-1")],
        "TCP",
    ),
    // Probes that DO speak HTTP, so both members belong to them.
    (
        "an HTTP group leaving the probe protocol to the default",
        &[("Protocol", "HTTP"), ("Port", "80"), ("VpcId", "vpc-1")],
        "HTTP",
    ),
    (
        "an HTTPS group with an HTTPS probe",
        &[
            ("Protocol", "HTTPS"),
            ("Port", "443"),
            ("VpcId", "vpc-1"),
            ("HealthCheckProtocol", "HTTPS"),
        ],
        "HTTPS",
    ),
    (
        "a TCP group with an HTTP probe",
        &[
            ("Protocol", "TCP"),
            ("Port", "1234"),
            ("VpcId", "vpc-1"),
            ("HealthCheckProtocol", "HTTP"),
        ],
        "HTTP",
    ),
    (
        "a TLS group with an HTTPS probe",
        &[
            ("Protocol", "TLS"),
            ("Port", "443"),
            ("VpcId", "vpc-1"),
            ("HealthCheckProtocol", "HTTPS"),
        ],
        "HTTPS",
    ),
    (
        // A Lambda function is invoked rather than connected to, so its check reports no
        // probe protocol and is an HTTP one.
        "a lambda group",
        &[("TargetType", "lambda")],
        "",
    ),
];

/// One combination of the two HTTP-only members: the case name, the fields that name
/// them, and the plural noun the refusal states — the path when both are named, because
/// one rule refuses both and reports the path first.
type MemberRow = (&'static str, &'static [(&'static str, &'static str)], &'static str);

/// Which of the two members the request names: either on its own, and both together.
const MEMBER_ROWS: &[MemberRow] = &[
    (
        "a path",
        &[("HealthCheckPath", "/sync-health-check")],
        "paths",
    ),
    ("a matcher", &[("Matcher.HttpCode", "200-399")], "matchers"),
    (
        "both members",
        &[
            ("HealthCheckPath", "/ping"),
            ("Matcher.HttpCode", "200-399"),
        ],
        "paths",
    ),
];

/// Whether a probe that speaks `protocol` carries the HTTP-only members. A group that
/// reports no probe protocol is a `lambda` one, whose check is an HTTP one all the same.
fn probe_is_http(protocol: &str) -> bool {
    matches!(protocol, "" | "HTTP" | "HTTPS")
}

/// The sentence a write naming `member` on a probe speaking `protocol` is refused with.
fn expected_message(member: &str, protocol: &str) -> String {
    format!("Health check {member} are not supported for {protocol} health checks")
}

/// The element one awsQuery form member is reported under (`Matcher.HttpCode` is carried
/// inside `<Matcher>`).
fn element_of(member: &str) -> &str {
    member.rsplit('.').next().expect("a member names an element")
}

/// The fields of one row, as the awsQuery form carries them.
fn form_fields(fields: &[(&str, &str)]) -> Vec<(String, String)> {
    fields.iter().map(|(key, value)| p(key, value)).collect()
}

/// What `DescribeTargetGroups` answers for a target group selected by NAME, whatever the
/// reply is — used to prove a refused create stored nothing at all.
async fn describe_by_name(router: &MultiServiceRouter, name: &str) -> Reply {
    let mut form = action("DescribeTargetGroups");
    form.push(p("Names.member.1", name));
    call(router, &form).await
}

#[tokio::test]
async fn a_create_takes_an_http_only_health_check_member_only_on_an_http_probe() {
    let (router, _engine) = emulator();

    for (group_index, (group_case, group_fields, probe)) in GROUP_ROWS.iter().enumerate() {
        for (member_index, (member_case, member_fields, noun)) in MEMBER_ROWS.iter().enumerate() {
            let case = format!("{group_case} with {member_case}");
            let name = format!("http-only-create-{group_index}-{member_index}");
            let mut form = action("CreateTargetGroup");
            form.push(p("Name", &name));
            form.extend(form_fields(group_fields));
            form.extend(form_fields(member_fields));
            let reply = call(&router, &form).await;

            if probe_is_http(probe) {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: a member of an HTTP probe must be accepted, got {} body {}",
                    reply.status, reply.body
                );
                let arn = reply
                    .text("TargetGroupArn")
                    .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn in {}", reply.body));
                // Accepted means STORED: every member the request named reads back.
                let described = describe(&router, &arn).await;
                for (member, value) in member_fields.iter() {
                    assert_eq!(
                        described.text(element_of(member)).as_deref(),
                        Some(*value),
                        "case {case:?}: expected {member} to read back as {value}, got {}",
                        described.body
                    );
                }
                continue;
            }

            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, INVALID_CONFIGURATION_REQUEST),
                "case {case:?}: expected 400 {INVALID_CONFIGURATION_REQUEST}, got {} body {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                unescaped(&reply.text("Message").unwrap_or_default()),
                expected_message(noun, probe),
                "case {case:?}: unexpected refusal message in {}",
                reply.body
            );
            // A refused create stores no target group, so that name is still free.
            let read = describe_by_name(&router, &name).await;
            assert_eq!(
                (read.status, read.error_code().as_str()),
                (400, "TargetGroupNotFound"),
                "case {case:?}: a refused create must store no target group, got {}",
                read.body
            );
        }
    }
}

#[tokio::test]
async fn a_modify_takes_an_http_only_health_check_member_only_on_an_http_probe() {
    let (router, _engine) = emulator();

    for (group_index, (group_case, group_fields, probe)) in GROUP_ROWS.iter().enumerate() {
        for (member_index, (member_case, member_fields, noun)) in MEMBER_ROWS.iter().enumerate() {
            let case = format!("{group_case} with {member_case}");
            // The group is created WITHOUT either member, so what the modify is judged on
            // is the probe the target group already stores.
            let name = format!("http-only-modify-{group_index}-{member_index}");
            let arn = create_group(&router, &case, &name, &form_fields(group_fields)).await;
            let before = health_check_of(&describe(&router, &arn).await);

            let mut form = action("ModifyTargetGroup");
            form.push(p("TargetGroupArn", &arn));
            form.extend(form_fields(member_fields));
            let reply = call(&router, &form).await;
            let described = describe(&router, &arn).await;

            if probe_is_http(probe) {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: a member of an HTTP probe must be accepted, got {} body {}",
                    reply.status, reply.body
                );
                for (member, value) in member_fields.iter() {
                    assert_eq!(
                        described.text(element_of(member)).as_deref(),
                        Some(*value),
                        "case {case:?}: expected {member} to read back as {value}, got {}",
                        described.body
                    );
                }
                continue;
            }

            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, INVALID_CONFIGURATION_REQUEST),
                "case {case:?}: expected 400 {INVALID_CONFIGURATION_REQUEST}, got {} body {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                unescaped(&reply.text("Message").unwrap_or_default()),
                expected_message(noun, probe),
                "case {case:?}: unexpected refusal message in {}",
                reply.body
            );
            assert_eq!(
                health_check_of(&described),
                before,
                "case {case:?}: a refused modify must change nothing, got {}",
                described.body
            );
        }
    }
}

#[tokio::test]
async fn a_probe_that_becomes_an_http_one_carries_the_members_the_same_request_names() {
    // The other side of the rule: the members are refused for the PROBE the write would
    // leave behind, not for the target group's own protocol — so the very same TCP target
    // group takes both of them in a request that also turns its probe into an HTTP one,
    // and takes neither once the probe is a TCP one again.
    let (router, _engine) = emulator();

    let arn = create_group(
        &router,
        "a TCP group whose probe becomes an HTTP one",
        "http-only-becomes-http",
        &form_fields(&[("Protocol", "TCP"), ("Port", "1234"), ("VpcId", "vpc-1")]),
    )
    .await;

    let mut form = action("ModifyTargetGroup");
    form.push(p("TargetGroupArn", &arn));
    form.push(p("HealthCheckProtocol", "HTTP"));
    form.push(p("HealthCheckPath", "/healthz"));
    form.push(p("Matcher.HttpCode", "200-299"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case the probe becoming an HTTP one: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let described = describe(&router, &arn).await;
    assert_eq!(
        (
            described.text("HealthCheckPath").as_deref(),
            described.text("HttpCode").as_deref()
        ),
        (Some("/healthz"), Some("200-299")),
        "case the probe becoming an HTTP one: expected both members stored, got {}",
        described.body
    );

    // And back: with the probe a TCP one again, the same members are refused.
    let mut form = action("ModifyTargetGroup");
    form.push(p("TargetGroupArn", &arn));
    form.push(p("HealthCheckProtocol", "TCP"));
    form.push(p("HealthCheckPath", "/healthz"));
    let reply = call(&router, &form).await;
    assert_eq!(
        (
            reply.status,
            reply.error_code(),
            unescaped(&reply.text("Message").unwrap_or_default())
        ),
        (
            400,
            INVALID_CONFIGURATION_REQUEST.to_string(),
            expected_message("paths", "TCP")
        ),
        "case the probe becoming a TCP one again: unexpected reply {}",
        reply.body
    );
}
