// SPDX-License-Identifier: CC0-1.0

//! PCS enforces the ROLE-NAMING convention on the instance profile a compute node group
//! passes to its instances.
//!
//! RULE: `CreateComputeNodeGroup$iamInstanceProfileArn` documents the convention the service
//! enforces — "the role contained in your instance profile must have the
//! pcs:RegisterComputeNodeGroupInstance permission and the role name must start with AWSPCS
//! or must have the path /aws-pcs/" — so a create whose instance profile carries no role on
//! either half of that convention, or carries no role at all, is refused with the modelled
//! `ValidationException` (HTTP 400) and its fixed sentence, and nothing is created: no later
//! `ListComputeNodeGroups` reports a group under the requested name. A profile whose role
//! does satisfy it is accepted, and the group reads back through `GetComputeNodeGroup`
//! carrying the profile it was created with.
//!
//! The convention is about the ROLE the profile contains, not about the profile: the name
//! and the path read are the role's, taken from what IAM reports inside the profile, so a
//! profile sitting on `/aws-pcs/` whose role sits elsewhere is still refused, and a profile
//! at the default path whose role is on `/aws-pcs/` is served.
//!
//! A refusal is grounded in what IAM ANSWERED, never assumed from silence: an emulator with
//! no IAM to ask knows nothing about the profile's role and refuses nothing.
//!
//! This suite needs TWO services on one engine — PCS reaches IAM through the in-process
//! callback, so a single-service router has no IAM to ask — which is why it lives beside the
//! router rather than in either service's own crate. Every role and profile below is built
//! the way AWS builds one, through IAM's own `CreateRole`, `CreateInstanceProfile` and
//! `AddRoleToInstanceProfile`, and every ARN a create names is the one IAM answered with.
//!
//! The rows differ along what the rule generalises over: whether the role's NAME carries the
//! prefix (with and without anything after it, and in another case), whether the role's PATH
//! is the one the convention names (exactly, beneath it, one that merely begins with the same
//! text, an unrelated one), where the profile itself sits, and whether the profile contains a
//! role at all.
//!
//! Sources: `smithy_query(query="doc", shape="CreateComputeNodeGroupRequest",
//! member="iamInstanceProfileArn")` for the convention and
//! `smithy_query(query="shape", shape="ValidationException")` for the refusal's modelled
//! members, both on the pcs model.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds the form body of an awsQuery call and the reader that
// pulls one element's text out of the XML answer are the test crate's own, written once in
// `all/query_wire.rs`.
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The IAM API version an awsQuery body carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The cluster every row creates its group in.
const CLUSTER: &str = "role-convention-cluster";

/// The status a cluster has to be in before it can take a compute node group.
const ACTIVE: &str = "ACTIVE";

/// How long the cluster is given to reach [`ACTIVE`] before the wait is called a failure.
///
/// PCS reports a new cluster `CREATING` and the clock owes it `ACTIVE` a span later, so what
/// is waited for takes well under a second; this is generous slack over that span, so a
/// loaded host cannot decide the outcome, and all it is for is that a move that never comes
/// fails instead of hanging. Nothing here asserts on elapsed time.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait before LOOKING AGAIN while that condition has not yet held. Not a bound
/// on anything: the bound is [`ACTIVE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The sentence PCS refuses an instance profile it can take no role out of with. Every word
/// of it is fixed: it names neither the profile, nor the role, nor the caller.
const ROLE_REFUSED: &str =
    "AWS PCS can't access a role associated with the instance profile because the role ARN \
     is invalid. For more information, see CreateComputeNodeGroup or UpdateComputeNodeGroup \
     in the API reference.";

/// PCS and IAM over ONE shared engine, as the production router mounts them. Amazon EC2 is
/// deliberately absent: the subnets a create names are EC2's, and with no EC2 to ask that
/// resolution refuses nothing, so what decides each row below is the profile's role.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_pcs_awsparallelcomputingservice::EmulatedService::new(engine),
    );
    router
}

/// PCS alone, with no IAM on the engine to ask about an instance profile.
fn emulator_without_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_pcs_awsparallelcomputingservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One awsQuery IAM action, answered with its HTTP status and XML body.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (key, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One IAM action that must succeed, answered with its XML body.
async fn iam_ok(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> String {
    let (status, body) = iam(router, action, parameters).await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} {parameters:?} must succeed, got {status} with {body}"
    );
    body
}

/// One PCS operation over its awsJson1_0 wire, answered with its HTTP status and JSON body.
async fn pcs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("pcs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("pcs"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("AWSParallelComputingService.{operation}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.body().clone();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The awsJson1_0 error code, without the namespace the envelope may prefix.
fn error_code(payload: &Value) -> String {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The error message, whichever casing the envelope carries it under.
fn error_message(payload: &Value) -> String {
    payload
        .get("Message")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An EC2 trust policy, so each role below is one an instance could assume.
fn ec2_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "ec2.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A `CreateComputeNodeGroup` body carrying every `@required` member.
fn create_body(cluster: &str, group: &str, profile_arn: &str) -> Value {
    json!({
        "clusterIdentifier": cluster,
        "computeNodeGroupName": group,
        "subnetIds": ["subnet-0123456789abcdef0"],
        "customLaunchTemplate": {"id": "lt-0123456789abcdef0", "version": "1"},
        "iamInstanceProfileArn": profile_arn,
        "scalingConfiguration": {"minInstanceCount": 0, "maxInstanceCount": 4},
        "instanceConfigs": [{"instanceType": "c6i.large"}],
    })
}

/// Create the cluster and wait until it reports [`ACTIVE`], looking again until it does or
/// [`ACTIVE_DEADLINE`] passes.
///
/// The CALLER waits for the condition, as a caller of the real service would; the service
/// never sleeps for it, and nothing between the reads nudges it.
async fn active_cluster(router: &MultiServiceRouter) {
    let (status, created) = pcs(
        router,
        "CreateCluster",
        &json!({
            "clusterName": CLUSTER,
            "scheduler": {"type": "SLURM", "version": "24.11"},
            "size": "SMALL",
            "networking": {"subnetIds": ["subnet-0123456789abcdef0"]},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateCluster must succeed, got {status} with {created}"
    );
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    loop {
        let (status, read) = pcs(
            router,
            "GetCluster",
            &json!({"clusterIdentifier": CLUSTER}),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetCluster must answer 200 while the cluster provisions, got {status} \
             with {read}"
        );
        if read["cluster"]["status"].as_str() == Some(ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the clock owed {CLUSTER} its {ACTIVE} status and it never came; last read \
             {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every compute node group `ListComputeNodeGroups` reports for the cluster, by name.
async fn group_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = pcs(
        router,
        "ListComputeNodeGroups",
        &json!({"clusterIdentifier": CLUSTER, "maxResults": 100}),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListComputeNodeGroups must answer 200, got {status} with {listed}"
    );
    listed["computeNodeGroups"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One row: the profile this account holds, and whether PCS may take a role out of it.
struct Case {
    case: &'static str,
    /// The role the profile contains, as `(role name, the path IAM holds it under)`, or
    /// `None` for a profile that contains no role at all.
    role: Option<(&'static str, &'static str)>,
    /// The path the instance profile ITSELF sits on, which the convention says nothing about.
    profile_path: &'static str,
    /// Whether the create is served.
    served: bool,
}

/// The rows, spanning the forms a role's name and path can take against the convention.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "a role name carrying the prefix, at the default path",
            role: Some(("AWSPCS-nodes", "/")),
            profile_path: "/",
            served: true,
        },
        Case {
            case: "a role named exactly the prefix",
            role: Some(("AWSPCS", "/")),
            profile_path: "/",
            served: true,
        },
        Case {
            case: "a role name that carries the prefix in another case",
            role: Some(("awspcs-lowercase", "/")),
            profile_path: "/",
            served: false,
        },
        Case {
            // The recorded case: a role on neither half of the convention.
            case: "a role name on neither half of the convention",
            role: Some(("myteam-badrole", "/")),
            profile_path: "/",
            served: false,
        },
        Case {
            case: "a role on the /aws-pcs/ path, named outside the prefix",
            role: Some(("data-nodes", "/aws-pcs/")),
            profile_path: "/",
            served: true,
        },
        Case {
            case: "a role beneath the /aws-pcs/ path",
            role: Some(("team-nodes", "/aws-pcs/team/")),
            profile_path: "/",
            served: true,
        },
        Case {
            case: "a role on a path that merely begins with the same text",
            role: Some(("extra-nodes", "/aws-pcs-extra/")),
            profile_path: "/",
            served: false,
        },
        Case {
            case: "a role on an unrelated path",
            role: Some(("other-nodes", "/pcs/")),
            profile_path: "/",
            served: false,
        },
        Case {
            // The convention is about the ROLE, so the profile's own path does not satisfy it.
            case: "the profile on the /aws-pcs/ path, its role on neither half",
            role: Some(("pathed-profile-role", "/")),
            profile_path: "/aws-pcs/",
            served: false,
        },
        Case {
            case: "a profile that contains no role at all",
            role: None,
            profile_path: "/",
            served: false,
        },
    ]
}

/// Hold one row's instance profile in IAM, built the way AWS builds one, and answer the ARN
/// IAM minted for it.
async fn held_profile(router: &MultiServiceRouter, profile: &str, row: &Case) -> String {
    let created = iam_ok(
        router,
        "CreateInstanceProfile",
        &[
            ("InstanceProfileName", profile),
            ("Path", row.profile_path),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    if let Some((role, path)) = row.role {
        iam_ok(
            router,
            "CreateRole",
            &[
                ("RoleName", role),
                ("Path", path),
                ("AssumeRolePolicyDocument", &ec2_trust_policy()),
            ],
        )
        .await;
        iam_ok(
            router,
            "AddRoleToInstanceProfile",
            &[("InstanceProfileName", profile), ("RoleName", role)],
        )
        .await;
    }
    arn
}

#[tokio::test]
async fn a_compute_node_group_is_created_only_for_a_role_pcs_may_take_out_of_the_profile() {
    let router = emulator();
    active_cluster(&router).await;

    let mut expected_groups: Vec<String> = Vec::new();
    for (index, row) in cases().iter().enumerate() {
        let profile = format!("role-convention-{index}");
        let group = format!("group-{index}");
        let profile_arn = held_profile(&router, &profile, row).await;

        let (status, answered) =
            pcs(&router, "CreateComputeNodeGroup", &create_body(CLUSTER, &group, &profile_arn))
                .await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {:?}: expected the create to be served, got {status} with {answered}",
                row.case
            );
            assert_eq!(
                answered["computeNodeGroup"]["iamInstanceProfileArn"],
                json!(profile_arn),
                "case {:?}: expected the profile to persist, got {answered}",
                row.case
            );
            // Round trip: the stored record is what a later reader answers from.
            let (status, read) = pcs(
                &router,
                "GetComputeNodeGroup",
                &json!({
                    "clusterIdentifier": CLUSTER,
                    "computeNodeGroupIdentifier": &group,
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: GetComputeNodeGroup must answer 200, got {status} with {read}",
                row.case
            );
            assert_eq!(
                read["computeNodeGroup"]["iamInstanceProfileArn"],
                json!(profile_arn),
                "case {:?}: expected the stored profile on the read back, got {read}",
                row.case
            );
            expected_groups.push(group.clone());
        } else {
            assert_eq!(
                status, 400,
                "case {:?}: expected the create to be refused, got {status} with {answered}",
                row.case
            );
            assert_eq!(
                error_code(&answered),
                "ValidationException",
                "case {:?}: got {answered}",
                row.case
            );
            assert_eq!(
                error_message(&answered),
                ROLE_REFUSED,
                "case {:?}: got {answered}",
                row.case
            );
            // The sentence is fixed text: it echoes nothing the caller sent.
            for supplied in [profile_arn.as_str(), group.as_str()] {
                assert!(
                    !error_message(&answered).contains(supplied),
                    "case {:?}: the sentence must echo no caller-supplied value, got {answered}",
                    row.case
                );
            }
        }

        // The effect-absence half: only the served rows left a group behind.
        let mut held = group_names(&router).await;
        held.sort();
        let mut expected = expected_groups.clone();
        expected.sort();
        assert_eq!(
            held, expected,
            "case {:?}: expected only the served creates to have persisted, got {held:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_emulator_with_no_iam_to_ask_refuses_nothing() {
    // Nothing is known about the profile's role here, and a refusal is grounded in what IAM
    // answered rather than assumed from silence.
    let router = emulator_without_iam();
    active_cluster(&router).await;
    let profile_arn = format!(
        "arn:aws:iam::{}:instance-profile/myteam-badrole",
        engine::EMULATOR_ACCOUNT_ID
    );

    let (status, served) = pcs(
        &router,
        "CreateComputeNodeGroup",
        &create_body(CLUSTER, "no-iam-peer", &profile_arn),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"no IAM mounted\": expected the create to be served, got {status} with {served}"
    );
    assert_eq!(
        served["computeNodeGroup"]["iamInstanceProfileArn"],
        json!(profile_arn),
        "case \"no IAM mounted\": expected the profile to persist, got {served}"
    );
}
