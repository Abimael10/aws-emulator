// SPDX-License-Identifier: CC0-1.0

//! Which `CreateTargetGroup` and `ModifyTargetGroup` members a target group may carry for
//! the kind of target it holds, over the wire.
//!
//! RULE: a target group whose targets are INVOKED rather than connected to (`TargetType`
//! `lambda`) has no network identity, so no member that states where or how traffic reaches
//! its targets applies to it — the model says so member by member ("if the target is a
//! Lambda function, this parameter does not apply" on `Protocol`, `Port` and `VpcId`, and
//! the two health-check members that address a target over the network). A write that
//! names one anyway is refused: HTTP 400, awsQuery code `ValidationError`, and the fixed
//! sentence "<member> cannot be specified for target groups with target type
//! '<targetType>'", whose variable parts are the offending member as the service names it
//! ([`stated_member_name`]) and the target type the request asked for. Nothing is stored
//! for a refused call, and every other target type HAS those members, so the same request
//! is served for them.
//!
//! The rows span what the rule generalises over: every member of the set, several values of
//! each (so no row depends on the one value a recording carries), the member on its own and
//! beside others, the create that mints a target group and the modify that changes one,
//! every `TargetTypeEnum` value, and the member arriving with nothing in it — which states
//! no value at all and so is not refused.
//!
//! The wire harness (`emulator`, `call`, `action`, `p`) is `provisioning_loop`'s; nothing
//! here restates it.

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::stated_member_name;
use smithy_server_runtime::MultiServiceRouter;

use crate::provisioning_loop::{action, call, emulator, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;
use crate::target_group_health_check_schedule::describe_by_name;

/// The code this API answers an input it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The code `DescribeTargetGroups` reports a selection nothing holds under.
const TARGET_GROUP_NOT_FOUND: &str = "TargetGroupNotFound";

/// The target type whose targets are invoked rather than connected to.
const LAMBDA: &str = "lambda";

/// One row: the member the request names and the value it carries. Several values per
/// member, because the rule is about the MEMBER rather than about what it carries.
const NETWORK_MEMBER_VALUES: &[(&str, &str)] = &[
    ("Port", "80"),
    ("Port", "1"),
    ("Port", "65535"),
    ("Protocol", "HTTP"),
    ("Protocol", "TCP"),
    ("VpcId", "vpc-12345678"),
    ("VpcId", "vpc-0a1b2c3d4e5f67890"),
    ("HealthCheckProtocol", "HTTP"),
    ("HealthCheckProtocol", "HTTPS"),
    ("HealthCheckPort", "traffic-port"),
    ("HealthCheckPort", "8080"),
];

/// The status, code and message of a refusal, as a caller reads them.
fn refusal(reply: &Reply) -> (u16, String, String) {
    (
        reply.status,
        reply.error_code(),
        unescaped(&reply.text("Message").unwrap_or_default()),
    )
}

/// The sentence the rule states for `member` on a target group of `target_type`.
fn expected_message(member: &str, target_type: &str) -> String {
    format!(
        "{} cannot be specified for target groups with target type '{target_type}'",
        stated_member_name(member)
    )
}

/// Create a target group named `name` carrying `fields` beside `TargetType`.
async fn create(
    router: &MultiServiceRouter,
    name: &str,
    target_type: &str,
    fields: &[(&str, &str)],
) -> Reply {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("TargetType", target_type));
    for (key, value) in fields {
        form.push(p(key, value));
    }
    call(router, &form).await
}

#[tokio::test]
async fn a_lambda_create_naming_a_network_member_is_refused_and_stores_nothing() {
    let (router, _engine) = emulator();

    for (index, (member, value)) in NETWORK_MEMBER_VALUES.iter().enumerate() {
        let case = format!("{member}={value} on a lambda target group");
        let name = format!("lambda-member-{index}");
        let reply = create(&router, &name, LAMBDA, &[(member, value)]).await;
        let expected = expected_message(member, LAMBDA);
        assert_eq!(
            refusal(&reply),
            (400, VALIDATION_ERROR.to_string(), expected.clone()),
            "case {case}: expected 400 {VALIDATION_ERROR} {expected:?}, got {} {}",
            reply.status,
            reply.body
        );

        // A refused create stores nothing: the name it asked for is held by nothing.
        let described = describe_by_name(&router, &name).await;
        assert_eq!(
            (described.status, described.error_code()),
            (400, TARGET_GROUP_NOT_FOUND.to_string()),
            "case {case}: a refused create must store nothing, got {} {}",
            described.status,
            described.body
        );
    }
}

#[tokio::test]
async fn a_lambda_create_naming_no_network_member_is_served() {
    // The control for every row above: the same create with the member left out is served,
    // and the target group it stores reports none of the members the rule is about.
    let (router, _engine) = emulator();

    let reply = create(&router, "lambda-member-none", LAMBDA, &[]).await;
    assert_eq!(
        reply.status, 200,
        "case a lambda create naming no network member: expected 200, got {} {}",
        reply.status, reply.body
    );

    let described = describe_by_name(&router, "lambda-member-none").await;
    assert_eq!(
        described.status, 200,
        "case the stored lambda target group: expected 200, got {} {}",
        described.status, described.body
    );
    for member in ["Protocol", "Port", "VpcId", "HealthCheckProtocol", "HealthCheckPort"] {
        assert_eq!(
            described.text(member),
            None,
            "case the stored lambda target group: a target that is invoked reports no \
             {member}, got {}",
            described.body
        );
    }
}

#[tokio::test]
async fn a_member_carrying_nothing_states_no_value_and_is_not_refused() {
    // An awsQuery request can carry a member with nothing in it, which asks for nothing: a
    // lambda create carrying such a member is served, exactly as one leaving it out is.
    //
    // `VpcId` is the member this is asked of: it is a bare string in the model, so the
    // empty form reaches this rule. The others carry a value out of a published set or
    // range (`Protocol` and `HealthCheckProtocol` are `ProtocolEnum`, `Port` is an integer,
    // `HealthCheckPort` is a port or the literal default), so nothing empty is a value of
    // theirs at all.
    let (router, _engine) = emulator();

    let reply = create(&router, "lambda-empty-vpc", LAMBDA, &[("VpcId", "")]).await;
    assert_eq!(
        reply.status, 200,
        "case an empty VpcId on a lambda target group: a member carrying nothing states \
         nothing, so the create must be served, got {} {}",
        reply.status, reply.body
    );
    let described = describe_by_name(&router, "lambda-empty-vpc").await;
    assert_eq!(
        (described.status, described.text("VpcId")),
        (200, None),
        "case an empty VpcId on a lambda target group: nothing was stated, so nothing is \
         stored, got {} {}",
        described.status,
        described.body
    );
}

#[tokio::test]
async fn the_refusal_names_the_first_member_of_the_request_that_does_not_apply() {
    // Several members at once are one refusal, about the first of them the service judges:
    // the port a target would receive traffic on.
    let (router, _engine) = emulator();

    let reply = create(
        &router,
        "lambda-member-many",
        LAMBDA,
        &[
            ("Port", "443"),
            ("Protocol", "HTTPS"),
            ("VpcId", "vpc-12345678"),
        ],
    )
    .await;
    let expected = expected_message("Port", LAMBDA);
    assert_eq!(
        refusal(&reply),
        (400, VALIDATION_ERROR.to_string(), expected.clone()),
        "case three network members at once: expected 400 {VALIDATION_ERROR} {expected:?}, \
         got {} {}",
        reply.status,
        reply.body
    );
}

#[tokio::test]
async fn a_modify_cannot_give_a_lambda_target_group_a_member_it_does_not_have() {
    // The verdict is on the MEMBER rather than on when it is named, so a stored lambda
    // target group can no more be GIVEN a health-check protocol or port than it could be
    // created with one — and a refused modify leaves the stored check exactly as it was.
    let (router, _engine) = emulator();

    let created = create(&router, "lambda-modify", LAMBDA, &[]).await;
    assert_eq!(
        created.status, 200,
        "case the lambda target group under test: expected 200, got {} {}",
        created.status, created.body
    );
    let arn = created
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("no TargetGroupArn in {}", created.body));

    for (member, value) in [
        ("HealthCheckProtocol", "HTTP"),
        ("HealthCheckProtocol", "HTTPS"),
        ("HealthCheckPort", "traffic-port"),
        ("HealthCheckPort", "8080"),
    ] {
        let case = format!("a modify naming {member}={value} on a lambda target group");
        let mut form = action("ModifyTargetGroup");
        form.push(p("TargetGroupArn", &arn));
        form.push(p(member, value));
        let reply = call(&router, &form).await;
        let expected = expected_message(member, LAMBDA);
        assert_eq!(
            refusal(&reply),
            (400, VALIDATION_ERROR.to_string(), expected.clone()),
            "case {case}: expected 400 {VALIDATION_ERROR} {expected:?}, got {} {}",
            reply.status,
            reply.body
        );

        let described = describe_by_name(&router, "lambda-modify").await;
        assert_eq!(
            (described.status, described.text(member)),
            (200, None),
            "case {case}: a refused modify must store nothing for the member, got {}",
            described.body
        );
    }

    // A member such a group DOES have is still writable, so the refusals above are about
    // the member and not about the group being a Lambda one.
    let mut form = action("ModifyTargetGroup");
    form.push(p("TargetGroupArn", &arn));
    form.push(p("HealthCheckPath", "/ping"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case a modify naming a member a lambda target group has: expected 200, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        describe_by_name(&router, "lambda-modify")
            .await
            .text("HealthCheckPath")
            .as_deref(),
        Some("/ping"),
        "case a modify naming a member a lambda target group has: the written path must \
         read back"
    );
}

#[tokio::test]
async fn every_other_target_type_takes_the_network_members() {
    // The members belong to every target type whose targets are reached over the network,
    // so the same create is served for those — and the values it names are what the stored
    // target group then reports.
    let (router, _engine) = emulator();

    // (the target type, the protocol its targets are reached over, the port, the health
    // check port) — `alb` is reached over TCP alone ("register a single Application Load
    // Balancer as a target"), the host target types over HTTP here.
    let served: &[(&str, &str, &str)] = &[
        ("instance", "HTTP", "80"),
        ("ip", "HTTP", "8080"),
        ("alb", "TCP", "443"),
    ];

    for (index, (target_type, protocol, port)) in served.iter().enumerate() {
        let case = format!("a {target_type} target group over {protocol}");
        let name = format!("member-ok-{index}");
        let reply = create(
            &router,
            &name,
            target_type,
            &[
                ("Protocol", protocol),
                ("Port", port),
                ("VpcId", "vpc-12345678"),
                ("HealthCheckPort", "traffic-port"),
            ],
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected the create to be served, got {} {}",
            reply.status, reply.body
        );

        let described = describe_by_name(&router, &name).await;
        assert_eq!(
            (
                described.text("Protocol").as_deref(),
                described.text("Port").as_deref(),
                described.text("VpcId").as_deref(),
                described.text("HealthCheckPort").as_deref(),
            ),
            (
                Some(*protocol),
                Some(*port),
                Some("vpc-12345678"),
                Some("traffic-port")
            ),
            "case {case}: expected the stored members to read back, got {}",
            described.body
        );
    }
}
