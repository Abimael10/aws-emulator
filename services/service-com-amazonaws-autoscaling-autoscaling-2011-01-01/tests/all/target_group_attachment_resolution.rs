// SPDX-License-Identifier: CC0-1.0

//! WHICH target groups an `AttachLoadBalancerTargetGroups` may associate with a group, and
//! WHO may associate them — both decided before the group is touched.
//!
//! TWO RULES, each driven end to end over the wire this service speaks (`awsQuery`), with
//! Elastic Load Balancing v2 (which owns the target groups) and, for the second, IAM and STS
//! (which own the policies and the session) mounted on the SAME engine:
//!
//! 1. **Every `TargetGroupARNs` entry has to RESOLVE.** The association is what registers the
//!    group's instances as targets of that target group — `CreateAutoScalingGroup$TargetGroupARNs`
//!    is "the Amazon Resource Names (ARN) of the target groups to associate with the Auto
//!    Scaling group. Instances are registered as targets in a target group" — so an ARN that
//!    names no target group Elastic Load Balancing v2 holds in the Region and account the call
//!    is addressed to is an association no instance could ever be registered through. The call
//!    is refused WHOLE: HTTP 400, error code `ValidationError`, the service's own sentence, and
//!    the group's `TargetGroupARNs` unchanged. A call whose every entry resolves is served and
//!    the group reports exactly what it named.
//!
//! 2. **The caller has to be permitted to change the group.** An Auto Scaling action that is
//!    not an account-wide read is judged by IAM against the ARN of the group the request
//!    addresses, so a caller whose identity-based policies hold no `Allow` of
//!    `autoscaling:AttachLoadBalancerTargetGroups` (or whose policies `Deny` it) is refused
//!    HTTP 403 `AccessDenied` naming the principal, the action, the group's ARN and which of
//!    IAM's rules withheld the grant — and nothing is attached. The detach of the same
//!    attachments is held to the same rule.
//!
//! The rows span what each rule generalises over: for the first, HOW an entry fails to resolve
//! (a target group that was never created, one created and then deleted, a value that is no
//! ARN at all), where in the request it sits, how many entries travel with it, and which
//! resolvable kinds of target group are served (two target types, one and several entries);
//! for the second, the documents in force (none, an allow of the whole service, an allow of
//! exactly the action, an allow overridden by a deny), and the operation (the attach and the
//! detach).

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10 as elbv2_service;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam_service;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts_service;

use crate::support::{
    borrowed, create_launch_template, create_target_group, elbv2_query, emulator, form_encode,
    p, query, seed_launch_templates, seed_subnets, tag_value, tag_values, vpc_of, xml_unescape,
    REGION,
};

/// The `TargetTypeEnum` value of the target groups these rows create: an Auto Scaling group's
/// own instances are `instance` targets, which is the only target type a group may be
/// associated with at all — what a target group of any OTHER target type is answered with is
/// its own suite's subject (`target_group_target_type`), so every target group here is one
/// these rows are free to resolve.
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port the target groups of this suite listen on.
const TARGET_GROUP_PORT: i32 = 80;

/// This service's IAM action prefix and the code it refuses an unauthorized caller with,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a row pass.
const ACTION_PREFIX: &str = "autoscaling";
const ACCESS_DENIED: &str = "AccessDenied";
const VALIDATION_ERROR: &str = "ValidationError";

/// The access key id of the account's own published credential, which provisions the roles
/// below and reads the account back: a key neither STS nor IAM holds a record for.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The API versions a client sends in each service's query body.
const AUTOSCALING_API_VERSION: &str = "2011-01-01";
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The launch template every group here launches from.
const LAUNCH_TEMPLATE: &str = "lt-attachment-resolution";

/// The message this service refuses an unresolvable target group ARN with, read from the
/// crate so the rows cannot drift from the handler.
fn refusal() -> &'static str {
    autoscaling::state::elbv2_targets::UNATTACHABLE_TARGET_GROUPS_MESSAGE
}

/// Create one Auto Scaling group with no capacity, attached to `attached`.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet_id: &str, attached: &[String]) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", subnet_id),
    ];
    for (index, arn) in attached.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Post one `AttachLoadBalancerTargetGroups` and answer its status, wire error code and
/// message.
async fn attach(
    router: &MultiServiceRouter,
    name: &str,
    arns: &[String],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The target groups the group reports under its own `TargetGroupARNs`, in the order it holds
/// them — the implemented reader of the member every row writes through.
async fn attached_target_groups(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {name:?} must succeed, got {status} with {body:?}"
    );
    let Some((_, rest)) = body.split_once("<TargetGroupARNs>") else {
        return Vec::new();
    };
    let inner = rest
        .split_once("</TargetGroupARNs>")
        .map_or(rest, |(inner, _)| inner);
    tag_values(inner, "member")
}

/// How ONE entry of a row's request fails to resolve, or that it resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// A target group Elastic Load Balancing v2 holds: created over its own wire.
    Held(usize),
    /// A well-formed target group ARN of this Region and account that was never created.
    NeverCreated,
    /// The ARN of a target group that WAS created and has since been deleted.
    Deleted,
    /// A value that is no ARN at all, which the AWS CLI sends verbatim.
    Malformed,
}

/// RULE 1: an attach naming an entry that resolves to no target group of this Region and
/// account is refused whole — HTTP 400, `ValidationError`, nothing attached — and one whose
/// every entry resolves is served and reported.
#[tokio::test]
async fn an_attach_resolves_every_target_group_it_names() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);

    // Three target groups the account really holds, and one created and then DELETED, whose
    // ARN is well formed and resolves to nothing.
    let mut held: Vec<String> = Vec::new();
    for index in 0..3 {
        held.push(
            create_target_group(
                &router,
                &format!("tg-resolve-{index}"),
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }
    let deleted = create_target_group(
        &router,
        "tg-resolve-gone",
        TARGET_GROUP_PORT,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;
    let (status, body) = elbv2_query(
        &router,
        "DeleteTargetGroup",
        &[p("TargetGroupArn", deleted.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "the target group to be deleted must delete, got {status} with {body:?}"
    );
    // A well-formed target group ARN of this Region and account that nothing was created
    // under: the same form `CreateTargetGroup` mints, built through the emulator's own ARN
    // writer rather than written out.
    let never_created = engine::emulator_arn(
        "elasticloadbalancing",
        REGION,
        "targetgroup/tg-resolve-absent/0123456789abcdef",
    );
    let value_of = |entry: Entry| match entry {
        Entry::Held(index) => held[index].clone(),
        Entry::NeverCreated => never_created.clone(),
        Entry::Deleted => deleted.clone(),
        Entry::Malformed => "my-target-group".to_string(),
    };

    // (case, the entries the attach names in request order, whether it must be served)
    let rows: Vec<(&str, Vec<Entry>, bool)> = vec![
        (
            "one target group the account holds",
            vec![Entry::Held(0)],
            true,
        ),
        (
            "two target groups the account holds",
            vec![Entry::Held(1), Entry::Held(2)],
            true,
        ),
        (
            "a target group that was never created, alone",
            vec![Entry::NeverCreated],
            false,
        ),
        (
            "the ARN of a target group that has been deleted, alone",
            vec![Entry::Deleted],
            false,
        ),
        (
            "a value that is no ARN at all, alone",
            vec![Entry::Malformed],
            false,
        ),
        (
            "an unresolvable entry FIRST, before one that resolves",
            vec![Entry::NeverCreated, Entry::Held(0)],
            false,
        ),
        (
            "an unresolvable entry LAST, after one that resolves",
            vec![Entry::Held(0), Entry::Deleted],
            false,
        ),
        (
            "an unresolvable entry BETWEEN two that resolve",
            vec![Entry::Held(0), Entry::NeverCreated, Entry::Held(1)],
            false,
        ),
        (
            "two unresolvable entries among three that resolve",
            vec![
                Entry::Held(0),
                Entry::NeverCreated,
                Entry::Held(1),
                Entry::Deleted,
                Entry::Held(2),
            ],
            false,
        ),
    ];

    for (row, (case, entries, served)) in rows.into_iter().enumerate() {
        let name = format!("asg-resolve-{row}");
        create_group(&router, &name, &subnets[0].id, &[]).await;
        let requested: Vec<String> = entries.iter().copied().map(value_of).collect();
        let (status, code, message) = attach(&router, &name, &requested).await;
        let attached = attached_target_groups(&router, &name).await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the attach to be served, got {status} with {code:?} \
                 {message:?}"
            );
            assert_eq!(
                attached, requested,
                "case {case:?}: expected the group to hold {requested:?}, got {attached:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected the {VALIDATION_ERROR} code, got {code:?} {message:?}"
        );
        assert_eq!(
            message,
            refusal(),
            "case {case:?}: expected the message {:?}, got {message:?}",
            refusal()
        );
        assert!(
            attached.is_empty(),
            "case {case:?}: the refusal must attach nothing, got {attached:?}"
        );
    }

    // The same refusal leaves an attachment the group ALREADY held exactly as it was: a
    // refused call is not a detach.
    let name = "asg-resolve-standing";
    create_group(&router, name, &subnets[0].id, std::slice::from_ref(&held[0])).await;
    let (status, code, message) = attach(
        &router,
        name,
        &[held[1].clone(), never_created.clone()],
    )
    .await;
    assert_eq!(
        status, 400,
        "the mixed attach must be refused, got {status} with {code:?} {message:?}"
    );
    let attached = attached_target_groups(&router, name).await;
    assert_eq!(
        attached,
        vec![held[0].clone()],
        "expected the group to still hold exactly its standing attachment, got {attached:?}"
    );
}

/// Post one `AttachTrafficSources` naming one traffic source, and answer its status, wire
/// error code and message.
async fn attach_traffic_source(
    router: &MultiServiceRouter,
    name: &str,
    identifier: &str,
    source_type: Option<&str>,
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("TrafficSources.member.1.Identifier", identifier),
    ];
    if let Some(source_type) = source_type {
        params.push(p("TrafficSources.member.1.Type", source_type));
    }
    let (status, body) = query(router, "AttachTrafficSources", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The `(Identifier, Type)` pairs the group reports under its own `TrafficSources`, in the
/// order it holds them.
async fn attached_traffic_sources(
    router: &MultiServiceRouter,
    name: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {name:?} must succeed, got {status} with {body:?}"
    );
    let Some((_, rest)) = body.split_once("<TrafficSources>") else {
        return Vec::new();
    };
    let inner = rest
        .split_once("</TrafficSources>")
        .map_or(rest, |(inner, _)| inner);
    tag_values(inner, "Identifier")
        .into_iter()
        .zip(tag_values(inner, "Type"))
        .collect()
}

/// The `(Identifier, Type, State)` triples the group reports through
/// `DescribeTrafficSources`, in the order it holds them — the reader whose whole subject the
/// traffic-source attachments are.
async fn described_traffic_sources(
    router: &MultiServiceRouter,
    name: &str,
    source_type: Option<&str>,
) -> Vec<(String, String, String)> {
    let mut params = vec![p("AutoScalingGroupName", name)];
    if let Some(source_type) = source_type {
        params.push(p("TrafficSourceType", source_type));
    }
    let (status, body) = query(router, "DescribeTrafficSources", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "DescribeTrafficSources for {name:?} must succeed, got {status} with {body:?}"
    );
    let identifiers = tag_values(&body, "Identifier");
    let types = tag_values(&body, "Type");
    let states = tag_values(&body, "State");
    assert_eq!(
        (identifiers.len(), types.len()),
        (states.len(), states.len()),
        "every reported traffic source must carry its identifier, its type and its state, got \
         {body:?}"
    );
    identifiers
        .into_iter()
        .zip(types)
        .zip(states)
        .map(|((identifier, source_type), state)| (identifier, source_type, state))
        .collect()
}

/// RULE 1 again, through the member that carries the same association: an `elbv2` traffic
/// source names "the ARN of an Application Load Balancer, Gateway Load Balancer, or Network
/// Load Balancer target group", so it is resolved exactly as `TargetGroupARNs` is — refused
/// whole when it resolves to nothing, and attached and reported when it does.
#[tokio::test]
async fn a_traffic_source_attach_resolves_the_target_group_it_names() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);

    let held = create_target_group(
        &router,
        "tg-traffic-held",
        TARGET_GROUP_PORT,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;
    let never_created = engine::emulator_arn(
        "elasticloadbalancing",
        REGION,
        "targetgroup/tg-traffic-absent/0123456789abcdef",
    );

    // (case, the identifier, the Type the request states for it, whether it must be served)
    let rows: Vec<(&str, String, Option<&str>, bool)> = vec![
        (
            "a target group the account holds, typed elbv2",
            held.clone(),
            Some("elbv2"),
            true,
        ),
        (
            "the same target group with the type left out, which its ARN states",
            held.clone(),
            None,
            true,
        ),
        (
            "a target group that was never created, typed elbv2",
            never_created.clone(),
            Some("elbv2"),
            false,
        ),
        (
            "a target group that was never created, with the type left out",
            never_created.clone(),
            None,
            false,
        ),
        (
            "a value that is no ARN at all, typed elbv2",
            "my-target-group".to_string(),
            Some("elbv2"),
            false,
        ),
    ];

    for (row, (case, identifier, source_type, served)) in rows.into_iter().enumerate() {
        let name = format!("asg-traffic-{row}");
        create_group(&router, &name, &subnets[0].id, &[]).await;
        let (status, code, message) =
            attach_traffic_source(&router, &name, &identifier, source_type).await;
        let attached = attached_traffic_sources(&router, &name).await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the attach to be served, got {status} with {code:?} \
                 {message:?}"
            );
            // The association is reported back with the type that identifier is of,
            // whichever way the request stated it: an ARN of an Elastic Load Balancing
            // target group is an `elbv2` traffic source.
            assert_eq!(
                attached,
                vec![(identifier.clone(), "elbv2".to_string())],
                "case {case:?}: expected the group to report the traffic source it attached, \
                 got {attached:?}"
            );
            // And through the reader whose own subject those attachments are: the state is
            // the one the attachment of a group holding no instance is in, every one of its
            // registrations (none) being in place.
            let described = described_traffic_sources(&router, &name, None).await;
            assert_eq!(
                described,
                vec![(identifier.clone(), "elbv2".to_string(), "Added".to_string())],
                "case {case:?}: expected DescribeTrafficSources to report the attachment, got \
                 {described:?}"
            );
            // "If you provide a traffic source type, then the results only include that
            // traffic source type": the one type the group holds, and another it does not.
            let selected = described_traffic_sources(&router, &name, Some("elbv2")).await;
            assert_eq!(
                selected, described,
                "case {case:?}: expected the elbv2 selection to report the same attachment, \
                 got {selected:?}"
            );
            let other = described_traffic_sources(&router, &name, Some("vpc-lattice")).await;
            assert!(
                other.is_empty(),
                "case {case:?}: expected no traffic source of another type, got {other:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected the {VALIDATION_ERROR} code, got {code:?} {message:?}"
        );
        assert_eq!(
            message,
            refusal(),
            "case {case:?}: expected the message {:?}, got {message:?}",
            refusal()
        );
        assert!(
            attached.is_empty(),
            "case {case:?}: the refusal must attach nothing, got {attached:?}"
        );
        let described = described_traffic_sources(&router, &name, None).await;
        assert!(
            described.is_empty(),
            "case {case:?}: the reader of the attachments must report none either, got \
             {described:?}"
        );
    }

    // Additive, and each traffic source held ONCE: attaching the same one again leaves the
    // attachments exactly as the first call did.
    let name = "asg-traffic-additive";
    create_group(&router, name, &subnets[0].id, &[]).await;
    for attempt in 0..2 {
        let (status, code, message) =
            attach_traffic_source(&router, name, &held, Some("elbv2")).await;
        assert_eq!(
            status, 200,
            "attempt {attempt}: the attach must be served, got {status} with {code:?} \
             {message:?}"
        );
    }
    let attached = attached_traffic_sources(&router, name).await;
    assert_eq!(
        attached,
        vec![(held.clone(), "elbv2".to_string())],
        "expected the group to hold the one traffic source once, got {attached:?}"
    );
}

/// The emulator the authorization rows drive: Auto Scaling beside the services that own what
/// the decision and the attachment are made of — IAM's policies, STS's session, Elastic Load
/// Balancing v2's target groups and Amazon EC2's subnets — on ONE shared engine.
fn authorized_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(elbv2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One query-protocol call to `service`, presented as signed by the access key id `akid` —
/// the only place the credential appears on the wire, and so what the service reads the
/// caller's identity from.
async fn call_as(
    router: &MultiServiceRouter,
    service: &str,
    api_version: &str,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={api_version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = if service == "iam" {
        "iam.amazonaws.com".to_string()
    } else {
        format!("{service}.{REGION}.amazonaws.com")
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={akid}/20260101/{REGION}/{service}/aws4_request, \
                 SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Auto Scaling call signed by `akid`.
async fn autoscaling_as(
    router: &MultiServiceRouter,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    call_as(
        router,
        "autoscaling",
        AUTOSCALING_API_VERSION,
        akid,
        action,
        params,
    )
    .await
}

/// A one-statement identity-based document: `effect` on `action` over every resource.
fn statement(effect: &str, action: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"*"}}]}}"#
    )
}

/// Create the role `role_name` with `inline` policies on it, then assume it: answers the
/// session's access key id and the ARN `sts:GetCallerIdentity` reports for it, which a
/// refusal must name.
async fn session_of(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &[(&str, String)],
) -> (String, String) {
    let account = engine::emulator_account_id();
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    let (status, created) = call_as(
        router,
        "iam",
        IAM_API_VERSION,
        ACCOUNT_KEY_ID,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be created, got {status} {created}"
    );
    for (policy_name, document) in inline {
        let (status, put) = call_as(
            router,
            "iam",
            IAM_API_VERSION,
            ACCOUNT_KEY_ID,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", policy_name),
                ("PolicyDocument", document),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the inline policy {policy_name:?} must be stored, got {status} {put}"
        );
    }
    let role_arn = format!("arn:aws:iam::{account}:role/{role_name}");
    let session_name = format!("{role_name}-session");
    let (status, assumed) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", &session_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be assumable, got {status} {assumed}"
    );
    let akid = tag_value(&assumed, "AccessKeyId");
    let arn = tag_value(&assumed, "Arn");
    assert!(
        !akid.is_empty() && !arn.is_empty(),
        "case {case:?}: the session must report its key and its ARN, got {assumed}"
    );
    (akid, arn)
}

/// What a row expects of the call it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: the policies in force allow the action.
    Served,
    /// HTTP 403 `AccessDenied` because a statement about the request says `Deny`.
    ExplicitDeny,
    /// HTTP 403 `AccessDenied` because no statement about the request allows it.
    NoAllow,
}

/// The sentence this service refuses `principal` with for `action` on `resource`, with the
/// clause the rule the decision was taken under states.
fn refusal_message(principal: &str, action: &str, resource: &str, expected: Expect) -> String {
    let reason = match expected {
        Expect::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Expect::NoAllow => format!("because no identity-based policy allows the {action} action"),
        Expect::Served => unreachable!("a served call carries no refusal"),
    };
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// The ARN IAM judges an action on the group `name` against: the group's OWN, as the service
/// that minted it reports it under `AutoScalingGroupARN` — read back rather than written out,
/// since the id segment of it is this service's to mint.
async fn group_resource(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {name:?} must succeed, got {status} with {body:?}"
    );
    let arn = tag_value(&body, "AutoScalingGroupARN");
    assert!(
        !arn.is_empty(),
        "the group {name:?} must report its own ARN, got {body:?}"
    );
    arn
}

/// RULE 2: the attach and the detach of a target group are authorized against the group's own
/// ARN before they touch it, so a caller the policies in force do not permit attaches and
/// detaches NOTHING and is refused 403 `AccessDenied`; a caller they permit is served.
#[tokio::test]
async fn a_target_group_attachment_is_authorized_against_the_groups_arn() {
    let (router, engine) = authorized_emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-unused-attachment-template").await;

    /// One row: the case, the role's name, its inline policies, and what the calls answer.
    type Row = (&'static str, &'static str, Vec<(&'static str, String)>, Expect);
    let rows: Vec<Row> = vec![
        (
            "a role carrying no policy at all",
            "attach-bare-role",
            vec![],
            Expect::NoAllow,
        ),
        (
            "a role allowed the whole service",
            "attach-service-role",
            vec![("allow-all", statement("Allow", "autoscaling:*"))],
            Expect::Served,
        ),
        (
            "a role allowed exactly these two actions",
            "attach-action-role",
            vec![
                (
                    "allow-attach",
                    statement("Allow", "autoscaling:AttachLoadBalancerTargetGroups"),
                ),
                (
                    "allow-detach",
                    statement("Allow", "autoscaling:DetachLoadBalancerTargetGroups"),
                ),
            ],
            Expect::Served,
        ),
        (
            "a role whose allow is overridden by a deny of the attachment actions",
            "attach-denied-role",
            vec![
                ("allow-all", statement("Allow", "autoscaling:*")),
                (
                    "deny-attach",
                    statement("Deny", "autoscaling:*LoadBalancerTargetGroups"),
                ),
            ],
            Expect::ExplicitDeny,
        ),
    ];

    for (row, (case, role_name, inline, expected)) in rows.iter().enumerate() {
        let (akid, principal) = session_of(&router, case, role_name, inline).await;
        let name = format!("asg-attachment-{row}");
        // The group and the target group both exist and belong to the account: the only
        // thing a row varies is what the caller is permitted to do.
        let target_group = create_target_group(
            &router,
            &format!("tg-auth-{row}"),
            TARGET_GROUP_PORT,
            TARGET_TYPE_INSTANCE,
            &vpc_id,
        )
        .await;
        create_group(&router, &name, &subnets[0].id, &[]).await;
        // The ARN the decision is taken on is the group's own, which it has as soon as it
        // exists.
        let resource = group_resource(&router, &name).await;

        for (operation, before) in [
            ("AttachLoadBalancerTargetGroups", Vec::new()),
            (
                "DetachLoadBalancerTargetGroups",
                if *expected == Expect::Served {
                    vec![target_group.clone()]
                } else {
                    Vec::new()
                },
            ),
        ] {
            let (status, body) = autoscaling_as(
                &router,
                &akid,
                operation,
                &[
                    ("AutoScalingGroupName", name.as_str()),
                    ("TargetGroupARNs.member.1", target_group.as_str()),
                ],
            )
            .await;
            if *expected == Expect::Served {
                assert_eq!(
                    status, 200,
                    "case {case:?} / {operation}: the permitted call must be served, got \
                     {status} {body}"
                );
            } else {
                assert_eq!(
                    status, 403,
                    "case {case:?} / {operation}: the unauthorized call must be refused, got \
                     {status} {body}"
                );
                assert_eq!(
                    tag_value(&body, "Code"),
                    ACCESS_DENIED,
                    "case {case:?} / {operation}: expected {ACCESS_DENIED}, got {body}"
                );
                let action = format!("{ACTION_PREFIX}:{operation}");
                assert_eq!(
                    xml_unescape(&tag_value(&body, "Message")),
                    refusal_message(&principal, &action, &resource, *expected),
                    "case {case:?} / {operation}: expected IAM's refusal wording, got {body}"
                );
            }
            // What the group holds is read back as the account's own credential: a refused
            // call attached and detached nothing, and a served one did exactly its own work.
            let attached = attached_target_groups(&router, &name).await;
            let expected_after: Vec<String> = match (*expected == Expect::Served, operation) {
                (true, "AttachLoadBalancerTargetGroups") => vec![target_group.clone()],
                (true, _) => Vec::new(),
                (false, _) => before.clone(),
            };
            assert_eq!(
                attached, expected_after,
                "case {case:?} / {operation}: expected the group to hold {expected_after:?}, \
                 got {attached:?}"
            );
        }
    }
}
