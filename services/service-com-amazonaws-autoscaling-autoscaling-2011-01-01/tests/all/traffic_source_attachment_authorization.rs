// SPDX-License-Identifier: CC0-1.0

//! WHO may change the traffic sources an Auto Scaling group holds — the decision
//! `AttachTrafficSources` and `DetachTrafficSources` take before they read or write anything.
//!
//! The rule under test: an Auto Scaling action that is not an account-wide read is judged by
//! IAM against the ARN of the group the request addresses, so a caller whose identity-based
//! policies hold no `Allow` of the action it asked for — or whose policies `Deny` it — is
//! refused HTTP 403 `AccessDenied` naming the principal, the action, the group's own ARN and
//! which of IAM's rules withheld the grant, and the group's attachments are left exactly as
//! they were: a refused detach detaches NOTHING, as a refused attach attaches nothing. A
//! caller the policies in force permit is served and the group reports exactly the change the
//! call asked for.
//!
//! The rows span what that rule generalises over: the documents in force (none at all, an
//! allow of the whole service, an allow of exactly these two actions, an allow overridden by a
//! deny of the detach, and an allow of the attach alone — so the decision is per ACTION and
//! not per operation family), and, for each, the kind of traffic source the group holds
//! (`elbv2`, `elb`, `vpc-lattice` — the catalog of `TrafficSourceIdentifier$Type`), since
//! which resource the association names changes nothing about who may remove it. Every row
//! reads the group back through `DescribeTrafficSources` as the account's own credential, so
//! no row can pass on a refusal that still changed the group.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts_service;

use crate::support::{
    borrowed, create_target_group, emulator_with_iam, form_encode, list_members, p, query,
    seed_subnets, tag_value, vpc_of, xml_unescape, SeededSubnet, REGION,
};

/// The launch template every group here launches from, one the shared harness puts in the
/// account's own Amazon EC2 store.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// This service's IAM action prefix and the code it refuses an unauthorized caller with,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a row pass.
const ACTION_PREFIX: &str = "autoscaling";
const ACCESS_DENIED: &str = "AccessDenied";

/// The access key id the account's own published credential presents: a key neither STS nor
/// IAM holds a record for, which is the position AWS account root credentials are in.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The API versions a client sends in each service's query body.
const AUTOSCALING_API_VERSION: &str = "2011-01-01";
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The three `TrafficSourceIdentifier$Type` values an association can be held under, from the
/// model's own catalog for the member
/// (`smithy_query(query="doc",shape="TrafficSourceIdentifier",member="Type")`): `elb` for the
/// name of a Classic Load Balancer, `elbv2` for an Elastic Load Balancing target group ARN and
/// `vpc-lattice` for a VPC Lattice target group ARN.
const TYPE_ELB: &str = "elb";
const TYPE_ELBV2: &str = "elbv2";
const TYPE_VPC_LATTICE: &str = "vpc-lattice";

/// Every kind a row holds its one traffic source under.
const KINDS: [&str; 3] = [TYPE_ELBV2, TYPE_ELB, TYPE_VPC_LATTICE];

/// The port the target groups of this suite listen on.
const TARGET_GROUP_PORT: i32 = 80;

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID"), the only
/// target type a group may be associated with.
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The emulator these rows drive: the shared four-service router — Auto Scaling beside Amazon
/// EC2, Elastic Load Balancing v2 and IAM on ONE engine — with STS mounted beside them, since
/// the caller a decision is taken for is the session STS minted the request's key for.
fn authorized_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = emulator_with_iam();
    router.register_service(sts_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One query-protocol call to `service`, presented as signed by the access key id `akid` — the
/// only place the credential appears on the wire, and so what the service reads the caller's
/// identity from.
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
    // IAM is a global service, so its endpoint carries no Region.
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
/// session's access key id and the ARN `sts:GetCallerIdentity` reports for it, which a refusal
/// must name.
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
        &[("RoleName", role_name), ("AssumeRolePolicyDocument", &trust)],
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

/// What a row expects of one call it drives.
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
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// Create one group with no capacity, so it owes no registration and nothing is launched.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet: &SeededSubnet) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", subnet.id.clone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// The ARN IAM judges an action on the group `name` against: the group's OWN, as the service
/// that minted it reports it under `AutoScalingGroupARN` — read back rather than written out,
/// since the id segment of it is this service's to mint.
async fn group_arn_of(router: &MultiServiceRouter, name: &str) -> String {
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

/// The identifier of a traffic source of `kind` this suite can attach: an `elbv2` association
/// names a target group the account HOLDS, while a Classic Load Balancer's name and a VPC
/// Lattice target group ARN name resources this emulator holds no store for.
async fn source_of(router: &MultiServiceRouter, vpc_id: &str, kind: &str, tag: &str) -> String {
    match kind {
        TYPE_ELBV2 => {
            create_target_group(
                router,
                &format!("tg-{tag}"),
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                vpc_id,
            )
            .await
        }
        TYPE_ELB => format!("classic-{tag}"),
        TYPE_VPC_LATTICE => {
            engine::emulator_arn("vpc-lattice", REGION, &format!("targetgroup/tg-{tag}"))
        }
        other => unreachable!("this suite holds no traffic source of kind {other:?}"),
    }
}

/// The `TrafficSources` parameters naming one traffic source under its kind.
fn source_params(name: &str, identifier: &str, kind: &str) -> Vec<(String, String)> {
    vec![
        p("AutoScalingGroupName", name),
        p("TrafficSources.member.1.Identifier", identifier),
        p("TrafficSources.member.1.Type", kind),
    ]
}

/// The identifiers the group reports through `DescribeTrafficSources`, read as the account's
/// own credential — the implemented reader of the very collection these calls change.
async fn reported_identifiers(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeTrafficSources",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the traffic sources of {name:?} expected 200, got {status} with \
         {body:?}"
    );
    list_members(&body, "TrafficSources")
        .into_iter()
        .map(|member| tag_value(member, "Identifier"))
        .collect()
}

/// The caller one row drives its calls as: the session's access key id, the principal that
/// key speaks for, and the ARN the decision on this group is taken against.
#[derive(Debug, Clone)]
struct Caller {
    akid: String,
    principal: String,
    resource: String,
}

/// Drive one Auto Scaling call as the session and judge it against what the row expects.
async fn judged(
    router: &MultiServiceRouter,
    case: &str,
    caller: &Caller,
    operation: &str,
    params: &[(String, String)],
    expected: Expect,
) {
    let (status, body) = autoscaling_as(router, &caller.akid, operation, &borrowed(params)).await;
    if expected == Expect::Served {
        assert_eq!(
            status, 200,
            "case {case:?} / {operation}: the permitted call must be served, got {status} {body}"
        );
        return;
    }
    assert_eq!(
        status, 403,
        "case {case:?} / {operation}: the unauthorized call must be refused, got {status} {body}"
    );
    assert_eq!(
        tag_value(&body, "Code"),
        ACCESS_DENIED,
        "case {case:?} / {operation}: expected {ACCESS_DENIED}, got {body}"
    );
    let action = format!("{ACTION_PREFIX}:{operation}");
    let expected_message = refusal_message(&caller.principal, &action, &caller.resource, expected);
    let reported = xml_unescape(&tag_value(&body, "Message"));
    assert_eq!(
        reported, expected_message,
        "case {case:?} / {operation}: expected {expected_message:?}, got {reported:?}"
    );
}

/// RULE: the attach and the detach of a traffic source are authorized against the group's own
/// ARN before the group is touched, so a caller the policies in force do not permit attaches
/// and detaches NOTHING and is refused 403 `AccessDenied`; a caller they permit is served and
/// the group reports exactly what the call changed.
#[tokio::test]
async fn a_traffic_source_detach_is_authorized_against_the_groups_arn() {
    let (router, engine) = authorized_emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;

    /// One row: the case, the role's name, its inline policies, and what the attach and the
    /// detach of one traffic source are each answered with.
    type Row = (
        &'static str,
        &'static str,
        Vec<(&'static str, String)>,
        Expect,
        Expect,
    );
    let rows: Vec<Row> = vec![
        (
            "a role carrying no policy at all",
            "traffic-bare-role",
            vec![],
            Expect::NoAllow,
            Expect::NoAllow,
        ),
        (
            "a role allowed the whole service",
            "traffic-service-role",
            vec![("allow-all", statement("Allow", "autoscaling:*"))],
            Expect::Served,
            Expect::Served,
        ),
        (
            "a role allowed exactly these two actions",
            "traffic-action-role",
            vec![
                (
                    "allow-attach",
                    statement("Allow", "autoscaling:AttachTrafficSources"),
                ),
                (
                    "allow-detach",
                    statement("Allow", "autoscaling:DetachTrafficSources"),
                ),
            ],
            Expect::Served,
            Expect::Served,
        ),
        (
            "a role whose allow is overridden by a deny of the detach",
            "traffic-denied-role",
            vec![
                ("allow-all", statement("Allow", "autoscaling:*")),
                (
                    "deny-detach",
                    statement("Deny", "autoscaling:DetachTrafficSources"),
                ),
            ],
            Expect::Served,
            Expect::ExplicitDeny,
        ),
        (
            "a role allowed the attach alone",
            "traffic-attach-only-role",
            vec![(
                "allow-attach",
                statement("Allow", "autoscaling:AttachTrafficSources"),
            )],
            Expect::Served,
            Expect::NoAllow,
        ),
    ];

    for (row, (case, role_name, inline, on_attach, on_detach)) in rows.iter().enumerate() {
        let (akid, principal) = session_of(&router, case, role_name, inline).await;
        for (kind_index, kind) in KINDS.iter().enumerate() {
            let case = format!("{case} detaching an {kind} traffic source");
            let tag = format!("ts-auth-{row}-{kind_index}");
            let identifier = source_of(&router, &vpc_id, kind, &tag).await;
            let name = format!("asg-{tag}");
            create_group(&router, &name, &subnets[0]).await;
            let caller = Caller {
                akid: akid.clone(),
                principal: principal.clone(),
                resource: group_arn_of(&router, &name).await,
            };
            let params = source_params(&name, &identifier, kind);

            // The attach is held to the same rule as the detach, and is judged first.
            judged(
                &router,
                &case,
                &caller,
                "AttachTrafficSources",
                &params,
                *on_attach,
            )
            .await;
            let after_attach = reported_identifiers(&router, &case, &name).await;
            let expected_after_attach: Vec<String> = if *on_attach == Expect::Served {
                vec![identifier.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(
                after_attach, expected_after_attach,
                "case {case:?}: after the attach the group must hold \
                 {expected_after_attach:?}, got {after_attach:?}"
            );

            // Whatever the attach was answered, the detach is driven against a group that
            // HOLDS the traffic source: the account's own credential seeds the attachment the
            // refused attach did not make, so what a refused detach leaves in place is an
            // attachment there was something to remove.
            if *on_attach != Expect::Served {
                let (status, body) =
                    query(&router, "AttachTrafficSources", &borrowed(&params)).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the account's own attach must be served, got {status} {body}"
                );
            }
            let held = reported_identifiers(&router, &case, &name).await;
            assert_eq!(
                held,
                vec![identifier.clone()],
                "case {case:?}: the detach must be driven against a group holding \
                 {identifier:?}, got {held:?}"
            );

            judged(
                &router,
                &case,
                &caller,
                "DetachTrafficSources",
                &params,
                *on_detach,
            )
            .await;
            let after_detach = reported_identifiers(&router, &case, &name).await;
            let expected_after_detach: Vec<String> = if *on_detach == Expect::Served {
                Vec::new()
            } else {
                vec![identifier.clone()]
            };
            assert_eq!(
                after_detach, expected_after_detach,
                "case {case:?}: after the detach the group must hold \
                 {expected_after_detach:?}, got {after_detach:?}"
            );
        }
    }
}
