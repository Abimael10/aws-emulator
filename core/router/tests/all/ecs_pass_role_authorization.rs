// SPDX-License-Identifier: CC0-1.0

//! Handing Amazon ECS one of the caller's IAM roles needs `iam:PassRole` on that role, and
//! Amazon ECS decides it against this account's IAM before it writes anything.
//!
//! RULE: a request that names a role for Amazon ECS to act under — the task execution role
//! and the task role a revision declares or a launch overrides
//! (`RegisterTaskDefinition$executionRoleArn`, `TaskOverride$taskRoleArn`), and the role a
//! service lets Amazon ECS reach its load balancer with (`CreateService$role`) — hands that
//! role over, which is its own IAM action granted on the role being handed over. So a
//! principal whose policies allow the Amazon ECS action but not `iam:PassRole` on that role is
//! refused the modelled `AccessDeniedException` (`@httpError 403`,
//! `smithy_query(query="operation",operation="RegisterTaskDefinition")`) naming the principal,
//! `iam:PassRole`, the ARN of the role the request named and which of IAM's rules withheld the
//! grant — and nothing it asked for is written: the family of a refused registration stays
//! absent from `ListTaskDefinitionFamilies`.
//!
//! Three services have to agree for the rule to hold, which is why this suite lives beside the
//! router: IAM holds the role and the documents in force on the principal, STS mints the
//! session a request speaks for, and Amazon ECS takes the decision and serves (or refuses) the
//! call. The role, its policy and the session are made the way a client makes them —
//! `iam:CreateRole`, `iam:PutRolePolicy` and `sts:AssumeRole` over the wire against the same
//! shared engine — so the principal a refusal blames is the one that credential speaks for.
//!
//! The rows span what the rule generalises over: which operation hands the role over, which
//! member it comes from (an execution role, a task role, both at once, a launch's overrides, a
//! service's load-balancer role, the infrastructure role of a managed Amazon EBS volume and of
//! a VPC Lattice binding, on a create, an update and a launch), how the value spells the role
//! (a full ARN and a bare name), what the principal's document says about `iam:PassRole`
//! (nothing, an explicit deny beside an allow, an allow scoped to ONE role), and which
//! principal asks (an assumed-role session and the account's own published credential, which
//! holds no IAM policies). The operations this suite does not drive hand over an
//! INFRASTRUCTURE role the same way (the capacity-provider and gateway calls, each through a
//! required nested configuration), and the gate reads those exactly as it reads the members
//! below — off the request, through the operation's own input schema.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element reader this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account this emulator serves (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The cluster the launches below address, and the container every revision declares.
const CLUSTER: &str = "passrole";
const CONTAINER: &str = "app";

/// An image in a public registry, so no row depends on what a private registry holds.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The EC2 instance id the cluster's one container instance is registered under, which
/// `StartTask` names as the capacity to place its task on.
const CAPACITY_INSTANCE_ID: &str = "i-0a1b2c3d4e5f60718";

/// The access key id of the account's own published credential, which neither STS nor IAM
/// holds a record for: the position AWS account root credentials are in.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The error code an authorization refusal carries, and the status the model pins on it.
const ACCESS_DENIED: &str = "AccessDeniedException";
const FORBIDDEN: u16 = 403;

/// The action that hands a role to a service.
const PASS_ROLE: &str = "iam:PassRole";

/// The roles the rows hand over: the one a grant names, and one no grant names.
const GRANTED_ROLE: &str = "ecsPassed";
const UNGRANTED_ROLE: &str = "ecsOther";

/// Amazon ECS, IAM and STS over ONE shared engine, as `router/src/lib.rs` mounts them, so the
/// decision Amazon ECS takes reads these very documents and this very session.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call to Amazon ECS signed with `access_key_id` in its SigV4 credential
/// scope — which is where this emulator reads which principal a request speaks for —
/// answered with its status and parsed body.
async fn ecs_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}\
                 /ecs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call made by the account's own credential that must succeed.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = ecs_as(router, ACCOUNT_KEY, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call that must succeed, answered with its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
    case: &str,
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}\
                 /{signing_name}/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected {action} to answer 200, got {status} with {text}"
    );
    text
}

/// One IAM call that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, String)], case: &str) {
    query(router, "iam", "2010-05-08", action, params, case).await;
}

/// The ARN of a role of this account under the default path, as IAM renders one.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// One role session a row calls as: the access key id its requests are signed with, and the
/// session ARN a refusal names it by.
struct Session {
    access_key_id: String,
    session_arn: String,
}

/// Create the IAM role `role_name` carrying `document` as its inline policy, assume it, and
/// answer the session the assume minted — each step through that service's OWN API.
async fn assumed_role(
    router: &MultiServiceRouter,
    role_name: &str,
    document: &str,
    case: &str,
) -> Session {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", document.to_string()),
        ],
        case,
    )
    .await;
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn(role_name)),
            ("RoleSessionName", "ecs-session".to_string()),
        ],
        case,
    )
    .await;
    Session {
        access_key_id: element(&assumed, "AccessKeyId"),
        session_arn: element(&assumed, "Arn"),
    }
}

/// Create the two roles the rows hand over, through IAM's own API: what decides a pass is the
/// CALLER's policy, so the passed roles carry none of their own.
async fn passable_roles(router: &MultiServiceRouter, case: &str) {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    for name in [GRANTED_ROLE, UNGRANTED_ROLE] {
        iam(
            router,
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust.clone()),
            ],
            case,
        )
        .await;
    }
}

/// The error code a refusal reports, as the short shape name its `__type` carries.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence IAM refuses a request with when nothing grants `iam:PassRole` on `role`.
fn default_deny(principal: &str, role: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {PASS_ROLE} on resource: {role} \
         because no identity-based policy allows the {PASS_ROLE} action"
    )
}

/// The sentence IAM refuses a request with when an explicit `Deny` covers `iam:PassRole` on
/// `role`.
fn explicit_deny(principal: &str, role: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {PASS_ROLE} on resource: {role} \
         with an explicit deny in an identity-based policy"
    )
}

/// A document granting every Amazon ECS action, with `extra` statements beside it.
fn ecs_policy(extra: Vec<Value>) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": "ecs:*",
        "Resource": "*",
    })];
    statements.extend(extra);
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// A revision body for `family`, with the role members `roles` sets.
fn registration(family: &str, roles: &[(&str, String)]) -> Value {
    let mut body = json!({
        "family": family,
        "containerDefinitions": [{ "name": CONTAINER, "image": IMAGE, "memory": 128 }],
    });
    for (member, value) in roles {
        body[*member] = json!(value);
    }
    body
}

/// One row of the refusal table: the call a principal makes, and the role the refusal must
/// name.
struct Refused {
    case: &'static str,
    operation: &'static str,
    body: Value,
    /// The ARN of the role the refusal names — the FIRST role the request hands over.
    role: String,
}

#[tokio::test]
async fn a_call_handing_ecs_a_role_the_caller_may_not_pass_is_refused_and_writes_nothing() {
    let router = emulator();
    let case = "the account's own records";
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    ecs(
        &router,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": json!({
                "accountId": ACCOUNT,
                "instanceId": CAPACITY_INSTANCE_ID,
                "region": REGION,
                "instanceType": "m5.large",
                "availabilityZone": format!("{REGION}a"),
            }).to_string(),
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
        case,
    )
    .await;
    passable_roles(&router, case).await;

    // A principal that may perform every Amazon ECS action and nothing else: the position a
    // client relying on `iam:PassRole` as its privilege-escalation boundary is in.
    let case = "a principal with every ECS action and no iam:PassRole";
    let session = assumed_role(&router, "ecs-only", &ecs_policy(Vec::new()), case).await;
    let granted = role_arn(GRANTED_ROLE);
    let other = role_arn(UNGRANTED_ROLE);

    let rows = vec![
        Refused {
            case: "RegisterTaskDefinition handing over an execution role",
            operation: "RegisterTaskDefinition",
            body: registration("refusedexec", &[("executionRoleArn", granted.clone())]),
            role: granted.clone(),
        },
        Refused {
            case: "RegisterTaskDefinition handing over a task role",
            operation: "RegisterTaskDefinition",
            body: registration("refusedtask", &[("taskRoleArn", other.clone())]),
            role: other.clone(),
        },
        Refused {
            case: "RegisterTaskDefinition handing over both, refused on the execution role",
            operation: "RegisterTaskDefinition",
            body: registration(
                "refusedboth",
                &[
                    ("executionRoleArn", granted.clone()),
                    ("taskRoleArn", other.clone()),
                ],
            ),
            role: granted.clone(),
        },
        Refused {
            case: "RunTask whose overrides hand over an execution role",
            operation: "RunTask",
            body: json!({
                "cluster": CLUSTER,
                "taskDefinition": "servedplain",
                "overrides": { "executionRoleArn": granted.clone() },
            }),
            role: granted.clone(),
        },
        Refused {
            case: "StartTask whose overrides hand over a task role",
            operation: "StartTask",
            body: json!({
                "cluster": CLUSTER,
                "taskDefinition": "servedplain",
                "containerInstances": [CAPACITY_INSTANCE_ID],
                "overrides": { "taskRoleArn": other.clone() },
            }),
            role: other.clone(),
        },
        Refused {
            case: "RegisterDaemonTaskDefinition handing over an execution role",
            operation: "RegisterDaemonTaskDefinition",
            body: json!({
                "family": "refuseddaemon",
                "containerDefinitions": [{ "image": IMAGE, "cpu": 128 }],
                "executionRoleArn": granted.clone(),
            }),
            role: granted.clone(),
        },
        Refused {
            // The member takes "the name or full Amazon Resource Name (ARN) of the IAM role",
            // and IAM judges the pass on the role's ARN either way.
            case: "CreateService handing over its load-balancer role by bare name",
            operation: "CreateService",
            body: json!({
                "cluster": CLUSTER,
                "serviceName": "refusedservice",
                "taskDefinition": "servedplain",
                "role": GRANTED_ROLE,
            }),
            role: granted.clone(),
        },
        Refused {
            // The INFRASTRUCTURE role Amazon ECS creates the service's Amazon EBS volume
            // with, which the request hands over just as plainly as the two above.
            case: "CreateService handing over its managed EBS volume's role",
            operation: "CreateService",
            body: json!({
                "cluster": CLUSTER,
                "serviceName": "refusedvolumeservice",
                "taskDefinition": "servedplain",
                "volumeConfigurations": [{
                    "name": "data",
                    "managedEBSVolume": { "roleArn": other.clone() },
                }],
            }),
            role: other.clone(),
        },
        Refused {
            // The INFRASTRUCTURE role a service binds its VPC Lattice target group with.
            case: "CreateService handing over its VPC Lattice role",
            operation: "CreateService",
            body: json!({
                "cluster": CLUSTER,
                "serviceName": "refusedlatticeservice",
                "taskDefinition": "servedplain",
                "vpcLatticeConfigurations": [{
                    "roleArn": granted.clone(),
                    "targetGroupArn": format!(
                        "arn:aws:vpc-lattice:{REGION}:{ACCOUNT}:targetgroup/tg-0123456789abcdef0"
                    ),
                    "portName": "web",
                }],
            }),
            role: granted.clone(),
        },
        Refused {
            // The same member on the UPDATE, which names no load-balancer role of its own.
            case: "UpdateService handing over its managed EBS volume's role",
            operation: "UpdateService",
            body: json!({
                "cluster": CLUSTER,
                "service": "refusedupdateservice",
                "volumeConfigurations": [{
                    "name": "data",
                    "managedEBSVolume": { "roleArn": other.clone() },
                }],
            }),
            role: other.clone(),
        },
        Refused {
            // And on a LAUNCH, whose volume configuration carries the same role.
            case: "RunTask handing over its managed EBS volume's role",
            operation: "RunTask",
            body: json!({
                "cluster": CLUSTER,
                "taskDefinition": "servedplain",
                "volumeConfigurations": [{
                    "name": "data",
                    "managedEBSVolume": { "roleArn": granted.clone() },
                }],
            }),
            role: granted.clone(),
        },
    ];

    for row in &rows {
        let row_case = row.case;
        let (status, answered) = ecs_as(
            &router,
            &session.access_key_id,
            row.operation,
            row.body.clone(),
        )
        .await;
        assert_eq!(
            status, FORBIDDEN,
            "case {row_case:?}: expected {} to answer {FORBIDDEN}, got {status} with {answered}",
            row.operation
        );
        assert_eq!(
            error_code(&answered),
            ACCESS_DENIED,
            "case {row_case:?}: expected an {ACCESS_DENIED}, got {answered}"
        );
        assert_eq!(
            answered["message"].as_str().unwrap_or_default(),
            default_deny(&session.session_arn, &row.role),
            "case {row_case:?}: expected IAM's wording for a missing {PASS_ROLE} grant, got \
             {answered}"
        );
    }

    // The same principal's registration that hands over NO role is served: there is nothing
    // to pass, so there is nothing to be authorized for.
    let case = "the same principal registering a revision that names no role";
    let (status, served) = ecs_as(
        &router,
        &session.access_key_id,
        "RegisterTaskDefinition",
        registration("servedplain", &[]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a registration naming no role to be served, got {status} \
         with {served}"
    );

    // Nothing the refused calls asked for was written: no family of a refused registration
    // and no service of a refused create.
    let case = "what the refused calls wrote";
    let families = ecs(&router, "ListTaskDefinitionFamilies", json!({}), case).await;
    let listed: Vec<String> = families["families"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|family| family.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        listed,
        vec!["servedplain".to_string()],
        "case {case:?}: expected only the served family, got {listed:?}"
    );
    let services = ecs(
        &router,
        "ListServices",
        json!({ "cluster": CLUSTER }),
        case,
    )
    .await;
    assert!(
        services["serviceArns"]
            .as_array()
            .is_none_or(|arns| arns.is_empty()),
        "case {case:?}: a refused create must write nothing, got {services}"
    );
}

#[tokio::test]
async fn the_grant_in_force_decides_which_role_the_caller_may_hand_over() {
    let router = emulator();
    let case = "the account's own records";
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    passable_roles(&router, case).await;
    let granted = role_arn(GRANTED_ROLE);
    let other = role_arn(UNGRANTED_ROLE);

    // A principal granted `iam:PassRole` on ONE role: the decision is about the role the
    // request hands over and not about the operation alone.
    let case = "a grant scoped to one role";
    let scoped = assumed_role(
        &router,
        "ecs-scoped-pass",
        &ecs_policy(vec![json!({
            "Effect": "Allow",
            "Action": PASS_ROLE,
            "Resource": granted.clone(),
        })]),
        case,
    )
    .await;
    let (status, served) = ecs_as(
        &router,
        &scoped.access_key_id,
        "RegisterTaskDefinition",
        registration("scopedgranted", &[("executionRoleArn", granted.clone())]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the granted role to be passed, got {status} with {served}"
    );
    assert_eq!(
        served["taskDefinition"]["executionRoleArn"]
            .as_str()
            .unwrap_or_default(),
        granted,
        "case {case:?}: expected the registered revision to carry the role, got {served}"
    );
    let (status, refused) = ecs_as(
        &router,
        &scoped.access_key_id,
        "RegisterTaskDefinition",
        registration("scopedrefused", &[("executionRoleArn", other.clone())]),
    )
    .await;
    assert_eq!(
        status, FORBIDDEN,
        "case {case:?}: expected the role the grant does not name to be refused, got {status} \
         with {refused}"
    );
    assert_eq!(
        refused["message"].as_str().unwrap_or_default(),
        default_deny(&scoped.session_arn, &other),
        "case {case:?}: expected IAM's default-deny wording, got {refused}"
    );

    // An explicit `Deny` about one role wins over an allow of everything, and reaches only
    // the role it names.
    let case = "an explicit deny beside an allow of every pass";
    let denied = assumed_role(
        &router,
        "ecs-denied-pass",
        &ecs_policy(vec![
            json!({ "Effect": "Allow", "Action": PASS_ROLE, "Resource": "*" }),
            json!({ "Effect": "Deny", "Action": PASS_ROLE, "Resource": other.clone() }),
        ]),
        case,
    )
    .await;
    let (status, refused) = ecs_as(
        &router,
        &denied.access_key_id,
        "RegisterTaskDefinition",
        registration("denyrefused", &[("taskRoleArn", other.clone())]),
    )
    .await;
    assert_eq!(
        status, FORBIDDEN,
        "case {case:?}: expected the denied role to be refused, got {status} with {refused}"
    );
    assert_eq!(
        refused["message"].as_str().unwrap_or_default(),
        explicit_deny(&denied.session_arn, &other),
        "case {case:?}: expected IAM's explicit-deny wording, got {refused}"
    );
    let (status, served) = ecs_as(
        &router,
        &denied.access_key_id,
        "RegisterTaskDefinition",
        registration("denyserved", &[("taskRoleArn", granted.clone())]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the role the deny does not name to be passed, got {status} \
         with {served}"
    );

    // The account's own published credential holds no IAM policies — the position AWS account
    // root credentials are in — so its pass is served.
    let case = "the account's own published credential";
    let (status, served) = ecs_as(
        &router,
        ACCOUNT_KEY,
        "RegisterTaskDefinition",
        registration("accountpass", &[("executionRoleArn", granted.clone())]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the account's own credential to be served, got {status} with \
         {served}"
    );

    // Exactly the registrations that were served are the families the account holds.
    let case = "the families the served registrations wrote";
    let families = ecs(&router, "ListTaskDefinitionFamilies", json!({}), case).await;
    let mut listed: Vec<String> = families["families"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|family| family.as_str().unwrap_or_default().to_string())
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        vec![
            "accountpass".to_string(),
            "denyserved".to_string(),
            "scopedgranted".to_string(),
        ],
        "case {case:?}: expected only the served families, got {listed:?}"
    );
}
