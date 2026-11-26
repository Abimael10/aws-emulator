// SPDX-License-Identifier: CC0-1.0

//! The AUTH POLICY on the data path: whether a request a client sends to the name a service
//! published is decided by the policy `PutAuthPolicy` attached to it.
//!
//! "The auth policy is only active when the auth type is set to AWS_IAM. … If the Auth type is
//! NONE, then, any auth policy that you provide remains inactive" (`GetAuthPolicy`'s `state`
//! member), so an ACTIVE policy is a document about REQUESTS — and the only place it can
//! decide anything is in front of the listener.
//!
//! The rules under test, each over a TABLE of documents rather than one row:
//!
//! 1. A request an ACTIVE auth policy explicitly DENIES is answered HTTP 403 with the
//!    `AccessDeniedException` sentence naming the caller, the data plane's action and the
//!    service ARN plus the request's path — whatever any `Allow` the caller's own identity
//!    policy carries says — and NOTHING behind the listener runs: the registered Lambda
//!    target is not invoked, so none of its answer reaches the client.
//! 2. A request the policies in force permit is served exactly as before: the listener's
//!    `forward` invokes the target and the client is answered from what the function
//!    returned.
//! 3. Which documents are in force is read on every request: a policy replaced by
//!    `PutAuthPolicy` decides the next request, one `DeleteAuthPolicy` removed decides
//!    nothing, and one whose resource's `authType` is no longer `AWS_IAM` is inactive. The
//!    policy on a SERVICE NETWORK the service is reached through is in force on the same
//!    rule, since a request reaches the service through that network.
//!
//! Every case drives the real wire path: an HTTP request to the service's own published name,
//! carrying the credential of a session the token service minted for a role the account
//! holds, with a real deployment package behind the listener — so a row that is served is
//! served by the function, and a row that is refused is refused before it.

use std::sync::Arc;

use crate::listeners::{a_target_function, create_listener, settled_active};
use crate::targets::{a_target_group, register_targets};
use crate::vpclattice_service_slice::{as_label, Fetched, Lattice, REGION};

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// The values `AuthType` admits (`smithy_query(query="shape",shape="AuthType")`), written out
/// here rather than read from the service.
pub(crate) const AUTH_TYPE_AWS_IAM: &str = "AWS_IAM";
pub(crate) const AUTH_TYPE_NONE: &str = "NONE";

/// The `AuthPolicyState` of a policy that decides requests, and of one that decides nothing
/// (`smithy_query(query="shape",shape="AuthPolicyState")`).
const STATE_ACTIVE: &str = "Active";
const STATE_INACTIVE: &str = "Inactive";

/// The action a client performs on a Lattice service's data plane, as the statements of an
/// auth policy name it: the data plane's own action namespace and its one operation. Written
/// out rather than read from the code under test, so no row can pass by agreeing with it.
pub(crate) const INVOKE_ACTION: &str = "vpc-lattice-svcs:Invoke";

/// The fixed clauses of the refusal AWS answers a denied request with. The variable parts —
/// the caller's ARN, the action and the resource — are the request's and the account's.
pub(crate) const DENIED_PREFIX: &str = "AccessDeniedException: User: ";
pub(crate) const DENIED_PERFORM: &str = " is not authorized to perform: ";
pub(crate) const DENIED_RESOURCE: &str = " on resource: ";
pub(crate) const DENIED_EXPLICIT_DENY: &str =
    " with an explicit deny in a service-based policy";

/// The path every row addresses, and the path the policies below scope a statement to.
pub(crate) const PROBE_PATH: &str = "/probe/42";

/// The name of the role whose session every signed row speaks for, and the name of the
/// session the token service mints for it.
const DRIVER_ROLE: &str = "lattice-auth-driver";
const DRIVER_SESSION: &str = "probe";

/// `PUT /authpolicy/{resourceIdentifier}`, whose `policy` is the document as JSON text.
pub(crate) async fn put_auth_policy(
    lattice: &Lattice,
    identifier: &str,
    policy: &serde_json::Value,
) -> u16 {
    let answer = lattice
        .call(
            "PUT",
            &format!("/authpolicy/{}", as_label(identifier)),
            Some(serde_json::json!({ "policy": policy.to_string() })),
        )
        .await;
    answer.status
}

/// One auth policy statement: `effect` for `principal` on `resource`, over the data plane's
/// own action.
pub(crate) fn statement(effect: &str, principal: &str, resource: &str) -> serde_json::Value {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "AWS": [principal] },
            "Action": INVOKE_ACTION,
            "Resource": resource,
        }],
    })
}

/// One IDENTITY-based statement: `effect` for the data plane's own action on `resource`.
///
/// An identity-based policy's statements name no principal — the policy is attached to one —
/// which is what an inline role policy is.
fn identity_statement(effect: &str, resource: &str) -> serde_json::Value {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": INVOKE_ACTION,
            "Resource": resource,
        }],
    })
}

/// A role of the account, holding an inline policy that ALLOWS the data plane's action on
/// every path of `service_arn`, and a session of it minted by the token service.
///
/// The role is planted through IAM's OWN state type and key rule, and the session minted
/// through the token service's OWN mint — the records `iam:CreateRole`, `iam:PutRolePolicy`
/// and an assume write — so the request below is resolved to the principal
/// `sts:GetCallerIdentity` would report for the credential it signed with, and is judged on
/// the policies in force on that role.
///
/// Answers the access key id the session signs with, the session's own ARN and the role's.
pub(crate) fn a_driver_session(
    engine: &Arc<engine::InMemoryEngine>,
    service_arn: &str,
) -> (String, String, String) {
    let account = engine::emulator_account_id();
    let role_arn = format!("arn:aws:iam::{account}:role/{DRIVER_ROLE}");
    let role_id = format!("AROA{}", "DRIVERROLE0123456".to_uppercase());
    let iam = iam_store::service_state(&**engine).expect("the engine carries an IAM store");
    {
        let mut store = iam.lock();
        store.roles.insert(
            iam_store::name_key(DRIVER_ROLE),
            service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
                path: "/".to_string(),
                role_name: DRIVER_ROLE.to_string(),
                role_id: role_id.clone(),
                arn: role_arn.clone(),
                create_date: ::aws_smithy_types::DateTime::from_secs(0),
                assume_role_policy_document: None,
                description: None,
                max_session_duration: None,
                permissions_boundary: None,
                tags: None,
                role_last_used: None,
                source_role_template: None,
            },
        );
        // The driver's own identity-based grant: the inline `Allow` the recorded case gives
        // the role. It is what makes every refusal below a refusal the AUTH POLICY produced —
        // an explicit deny there has to override this allow.
        let inline = identity_statement("Allow", &format!("{service_arn}/*"));
        store.role_inline_policies.insert(
            iam_store::name_key(DRIVER_ROLE),
            [(
                iam_store::name_key("invoke-lattice"),
                iam_store::InlinePolicy {
                    policy_name: "invoke-lattice".to_string(),
                    policy_document: inline.to_string(),
                },
            )]
            .into_iter()
            .collect(),
        );
    }
    let session_arn = ::service_common::iam_role::assumed_role_session_arn(&role_arn, DRIVER_SESSION);
    let registry =
        sts_store::service_state(&**engine).expect("the engine carries a session registry");
    let minted = sts_store::mint_role_session(
        &**engine,
        &::emulator_server::CredentialStore::default(),
        &registry,
        std::time::SystemTime::now(),
        sts_store::NewSession {
            role_arn: Some(role_arn.clone()),
            ..sts_store::NewSession::new(
                sts_store::Identity {
                    user_id: format!("{role_id}:{DRIVER_SESSION}"),
                    account: account.to_string(),
                    arn: session_arn.clone(),
                },
                sts_store::SessionKind::AssumedRole,
                3600,
            )
        },
    )
    .expect("the session is minted");
    (
        minted.credentials.access_key_id,
        session_arn,
        role_arn,
    )
}

/// One request to the service's published name, signed as the session `access_key_id` names —
/// the credential scope the data plane of a Lattice service is addressed with.
pub(crate) async fn signed_fetch(
    lattice: &Lattice,
    host: &str,
    access_key_id: &str,
    region: &str,
) -> Fetched {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/vpc-lattice-svcs/\
         aws4_request, SignedHeaders=host, Signature=abc"
    );
    lattice
        .fetch(
            "GET",
            host,
            PROBE_PATH,
            "",
            &[("authorization", authorization.as_str())],
        )
        .await
}

/// Rules 1, 2 and 3, over a table of auth policy documents on a service whose `authType` is
/// `AWS_IAM`, with a real Lambda target behind its listener.
#[tokio::test]
async fn an_active_auth_policy_decides_the_request_before_the_listener_acts() {
    let (lattice, engine) = Lattice::with_lambda();
    let function_arn = a_target_function(&lattice, &engine, "lattice-auth-target").await;

    // The control plane, through this service's own API: the network, the service with IAM
    // authentication, their association, a VPC association so a client is in a position to
    // reach the service at all, the `LAMBDA` target group holding the function, and the
    // HTTP:80 listener whose default action forwards to that group.
    let network = lattice
        .create_service_network(serde_json::json!({
            "name": "auth-network",
            "authType": AUTH_TYPE_NONE,
        }))
        .await;
    assert_eq!(
        network.status, 201,
        "setup CreateServiceNetwork: expected 201, got {} / {}",
        network.status, network.body
    );
    let network_id = network.str_field("id");
    let service = lattice
        .create_service(serde_json::json!({
            "name": "auth-shop",
            "authType": AUTH_TYPE_AWS_IAM,
        }))
        .await;
    assert_eq!(
        service.status, 201,
        "setup CreateService: expected 201, got {} / {}",
        service.status, service.body
    );
    let service_id = service.str_field("id");
    let service_arn = service.str_field("arn");
    let published = format!(
        "{}.{}",
        service_com_amazonaws_vpclattice_mercurycontrolplane::state::service_label(
            "auth-shop",
            &service_id
        ),
        ::service_common::data_plane_addressing::LOOPBACK_ZONE
    );
    let associated = lattice.associate(&service_id, &network_id).await;
    assert_eq!(
        associated.status, 200,
        "setup CreateServiceNetworkServiceAssociation: expected 200, got {} / {}",
        associated.status, associated.body
    );
    let vpc = lattice
        .call(
            "POST",
            "/servicenetworkvpcassociations",
            Some(serde_json::json!({
                "serviceNetworkIdentifier": network_id,
                "vpcIdentifier": "vpc-0123456789abcdef0",
            })),
        )
        .await;
    assert_eq!(
        vpc.status, 200,
        "setup CreateServiceNetworkVpcAssociation: expected 200, got {} / {}",
        vpc.status, vpc.body
    );
    let (group_id, _group_arn) =
        a_target_group(&lattice, "auth-shop-function", "LAMBDA", None).await;
    let registered =
        register_targets(&lattice, &group_id, serde_json::json!([{ "id": function_arn }])).await;
    assert_eq!(
        registered.status, 200,
        "setup RegisterTargets: expected 200, got {} / {}",
        registered.status, registered.body
    );
    let listener = create_listener(
        &lattice,
        &service_id,
        serde_json::json!({
            "name": "http",
            "protocol": "HTTP",
            "port": 80,
            "defaultAction": {
                "forward": { "targetGroups": [{ "targetGroupIdentifier": group_id }] },
            },
        }),
    )
    .await;
    assert_eq!(
        listener.status, 201,
        "setup CreateListener: expected 201, got {} / {}",
        listener.status, listener.body
    );
    // The `ACTIVE` statuses the clock owes, waited out by looking again rather than by
    // sleeping for a span.
    settled_active(&lattice, "the service", &format!("/services/{service_id}")).await;
    settled_active(&lattice, "the target group", &format!("/targetgroups/{group_id}")).await;

    let (access_key_id, session_arn, role_arn) = a_driver_session(&engine, &service_arn);
    let whole_service = format!("{service_arn}/*");
    let addressed_resource = format!("{service_arn}{PROBE_PATH}");

    /// What one row installs, and what the plane must answer it.
    struct Row {
        case: &'static str,
        /// The auth policy to attach to the SERVICE, or `None` to remove whatever it holds.
        policy: Option<serde_json::Value>,
        /// The status the plane must answer with.
        status: u16,
        /// Set where the row must be refused by an explicit deny, which pins the whole
        /// sentence; a row that is served pins the function's own answer instead.
        explicit_deny: bool,
    }
    let rows = vec![
        Row {
            case: "an Allow naming the caller's role on the whole service",
            policy: Some(statement("Allow", &role_arn, &whole_service)),
            status: 200,
            explicit_deny: false,
        },
        Row {
            case: "the same statement at Effect=Deny",
            policy: Some(statement("Deny", &role_arn, &whole_service)),
            status: 403,
            explicit_deny: true,
        },
        Row {
            case: "a Deny naming the session rather than its role",
            policy: Some(statement("Deny", &session_arn, &whole_service)),
            status: 403,
            explicit_deny: true,
        },
        Row {
            case: "a Deny scoped to the exact path the request addressed",
            policy: Some(statement("Deny", &role_arn, &addressed_resource)),
            status: 403,
            explicit_deny: true,
        },
        Row {
            case: "a Deny naming every principal",
            policy: Some(statement("Deny", "*", &whole_service)),
            status: 403,
            explicit_deny: true,
        },
        Row {
            case: "a Deny about another path of the same service",
            policy: Some(statement(
                "Deny",
                &role_arn,
                &format!("{service_arn}/orders/*"),
            )),
            status: 200,
            explicit_deny: false,
        },
        Row {
            case: "a Deny about another principal",
            policy: Some(statement(
                "Deny",
                "arn:aws:iam::000000000000:role/someone-else",
                &whole_service,
            )),
            status: 200,
            explicit_deny: false,
        },
        Row {
            case: "an Allow naming another principal, leaving the caller to its own policy",
            policy: Some(statement(
                "Allow",
                "arn:aws:iam::000000000000:role/someone-else",
                &whole_service,
            )),
            status: 200,
            explicit_deny: false,
        },
        Row {
            case: "no auth policy at all",
            policy: None,
            status: 200,
            explicit_deny: false,
        },
    ];
    for row in rows {
        let case = row.case;
        match &row.policy {
            Some(policy) => {
                let status = put_auth_policy(&lattice, &service_id, policy).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: setup PutAuthPolicy: expected 200, got {status}"
                );
                // The document is in force because it is STORED: the read reports it back
                // active, which is the state that makes it decide anything.
                let read = lattice
                    .call("GET", &format!("/authpolicy/{}", as_label(&service_id)), None)
                    .await;
                assert_eq!(
                    read.body["policy"].as_str(),
                    Some(policy.to_string().as_str()),
                    "case {case:?}: expected GetAuthPolicy to report the stored document, got {}",
                    read.body
                );
                assert_eq!(
                    read.body["state"].as_str(),
                    Some(STATE_ACTIVE),
                    "case {case:?}: expected the policy of an AWS_IAM service to be active, got {}",
                    read.body
                );
            }
            None => {
                let removed = lattice
                    .call(
                        "DELETE",
                        &format!("/authpolicy/{}", as_label(&service_id)),
                        None,
                    )
                    .await;
                assert_eq!(
                    removed.status, 204,
                    "case {case:?}: setup DeleteAuthPolicy: expected 204, got {} / {}",
                    removed.status, removed.body
                );
            }
        }
        let answered = signed_fetch(&lattice, &published, &access_key_id, REGION).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {}, got {} with body {:?}",
            row.status, answered.status, answered.body
        );
        if row.explicit_deny {
            let expected = format!(
                "{DENIED_PREFIX}{session_arn}{DENIED_PERFORM}{INVOKE_ACTION}{DENIED_RESOURCE}\
                 {addressed_resource}{DENIED_EXPLICIT_DENY}"
            );
            assert_eq!(
                answered.body, expected,
                "case {case:?}: expected the explicit-deny sentence {expected:?}, got {:?}",
                answered.body
            );
            // Nothing behind the listener ran: none of the function's own answer reaches a
            // refused client.
            assert!(
                !answered.body.contains("raw_path"),
                "case {case:?}: a refused request must not reach the target, got {:?}",
                answered.body
            );
        } else {
            // The row was served by the TARGET: the body is the event the plane handed the
            // function, which only the function can have produced.
            let event: serde_json::Value = serde_json::from_str(&answered.body)
                .unwrap_or_else(|error| {
                    panic!(
                        "case {case:?}: expected the function's own JSON answer, got {:?} \
                         ({error})",
                        answered.body
                    )
                });
            assert_eq!(
                event["raw_path"].as_str(),
                Some(PROBE_PATH),
                "case {case:?}: expected the target to be handed the request's path, got {event}"
            );
        }
    }

    // Rule 3, over the two facts that make a stored policy decide anything: WHICH resource it
    // is attached to, and the `authType` of that resource.
    //
    // The same document that refuses this caller on the service is attached to the SERVICE
    // NETWORK the request reaches the service through instead. While that network's `authType`
    // is `NONE` the document is INACTIVE and decides nothing; once the network authenticates
    // with IAM the very same document refuses the request, because a request reaches the
    // service through its network.
    let denying = statement("Deny", &role_arn, &whole_service);
    let removed = lattice
        .call(
            "DELETE",
            &format!("/authpolicy/{}", as_label(&service_id)),
            None,
        )
        .await;
    assert_eq!(
        removed.status, 204,
        "setup DeleteAuthPolicy: expected 204, got {} / {}",
        removed.status, removed.body
    );
    let status = put_auth_policy(&lattice, &network_id, &denying).await;
    assert_eq!(
        status, 200,
        "setup PutAuthPolicy on the service network: expected 200, got {status}"
    );
    let inactive = lattice
        .call("GET", &format!("/authpolicy/{}", as_label(&network_id)), None)
        .await;
    assert_eq!(
        inactive.body["state"].as_str(),
        Some(STATE_INACTIVE),
        "case \"the network's deny while its authType is NONE\": expected the policy to be \
         inactive, got {}",
        inactive.body
    );
    let served = signed_fetch(&lattice, &published, &access_key_id, REGION).await;
    assert_eq!(
        served.status, 200,
        "case \"the network's deny while its authType is NONE\": expected the inactive policy \
         to decide nothing, got {} with body {:?}",
        served.status, served.body
    );
    let network_iam = lattice
        .call(
            "PATCH",
            &format!("/servicenetworks/{network_id}"),
            Some(serde_json::json!({ "authType": AUTH_TYPE_AWS_IAM })),
        )
        .await;
    assert_eq!(
        network_iam.status, 200,
        "setup UpdateServiceNetwork: expected 200, got {} / {}",
        network_iam.status, network_iam.body
    );
    let activated = lattice
        .call("GET", &format!("/authpolicy/{}", as_label(&network_id)), None)
        .await;
    assert_eq!(
        activated.body["state"].as_str(),
        Some(STATE_ACTIVE),
        "case \"the service network's own deny\": expected the stored policy to be active once \
         the network authenticates with IAM, got {}",
        activated.body
    );
    let refused = signed_fetch(&lattice, &published, &access_key_id, REGION).await;
    assert_eq!(
        refused.status, 403,
        "case \"the service network's own deny\": expected HTTP 403, got {} with body {:?}",
        refused.status, refused.body
    );
    assert!(
        refused.body.ends_with(DENIED_EXPLICIT_DENY),
        "case \"the service network's own deny\": expected the explicit-deny sentence, got {:?}",
        refused.body
    );
}

/// RULE: the policy decides the principal the EMULATOR authenticated, so a request that
/// presents no credential at all speaks for nobody — an auth policy whose grants all name a
/// principal admits it nowhere.
///
/// The rows differ along the document in force: a grant naming the driver's role (which is
/// not this caller), and a grant naming every principal, which is the one element an
/// unauthenticated caller is named by.
#[tokio::test]
async fn a_request_presenting_no_credential_speaks_for_nobody() {
    let (lattice, engine) = Lattice::with_lambda();
    let function_arn = a_target_function(&lattice, &engine, "lattice-anonymous-target").await;
    let network = lattice
        .create_service_network(serde_json::json!({ "name": "anon-network" }))
        .await;
    assert_eq!(network.status, 201, "setup CreateServiceNetwork: got {}", network.body);
    let network_id = network.str_field("id");
    let service = lattice
        .create_service(serde_json::json!({
            "name": "anon-shop",
            "authType": AUTH_TYPE_AWS_IAM,
        }))
        .await;
    assert_eq!(service.status, 201, "setup CreateService: got {}", service.body);
    let service_id = service.str_field("id");
    let service_arn = service.str_field("arn");
    let published = format!(
        "{}.{}",
        service_com_amazonaws_vpclattice_mercurycontrolplane::state::service_label(
            "anon-shop",
            &service_id
        ),
        ::service_common::data_plane_addressing::LOOPBACK_ZONE
    );
    assert_eq!(
        lattice.associate(&service_id, &network_id).await.status,
        200,
        "setup CreateServiceNetworkServiceAssociation"
    );
    let vpc = lattice
        .call(
            "POST",
            "/servicenetworkvpcassociations",
            Some(serde_json::json!({
                "serviceNetworkIdentifier": network_id,
                "vpcIdentifier": "vpc-0123456789abcdef0",
            })),
        )
        .await;
    assert_eq!(vpc.status, 200, "setup CreateServiceNetworkVpcAssociation: got {}", vpc.body);
    let (group_id, _group_arn) = a_target_group(&lattice, "anon-function", "LAMBDA", None).await;
    assert_eq!(
        register_targets(&lattice, &group_id, serde_json::json!([{ "id": function_arn }]))
            .await
            .status,
        200,
        "setup RegisterTargets"
    );
    let listener = create_listener(
        &lattice,
        &service_id,
        serde_json::json!({
            "name": "http",
            "protocol": "HTTP",
            "port": 80,
            "defaultAction": {
                "forward": { "targetGroups": [{ "targetGroupIdentifier": group_id }] },
            },
        }),
    )
    .await;
    assert_eq!(listener.status, 201, "setup CreateListener: got {}", listener.body);
    settled_active(&lattice, "the service", &format!("/services/{service_id}")).await;
    settled_active(&lattice, "the target group", &format!("/targetgroups/{group_id}")).await;

    let role_arn = format!(
        "arn:aws:iam::{}:role/{DRIVER_ROLE}",
        engine::emulator_account_id()
    );
    let whole_service = format!("{service_arn}/*");
    // (case, the document in force, the status an unsigned request is answered)
    let rows: Vec<(&str, serde_json::Value, u16)> = vec![
        (
            "a grant naming a role, which no unauthenticated caller is",
            statement("Allow", &role_arn, &whole_service),
            403,
        ),
        (
            "a grant naming every principal",
            statement("Allow", "*", &whole_service),
            200,
        ),
    ];
    for (case, policy, status) in rows {
        let put = put_auth_policy(&lattice, &service_id, &policy).await;
        assert_eq!(put, 200, "case {case:?}: setup PutAuthPolicy: expected 200, got {put}");
        let answered = lattice.fetch("GET", &published, PROBE_PATH, "", &[]).await;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            answered.status, answered.body
        );
        if status == 403 {
            // The whole sentence: the caller nothing authenticated speaks for nobody, the
            // action is the data plane's own and the resource the service ARN plus the path,
            // and the clause names the only document that was consulted.
            let expected = format!(
                "{DENIED_PREFIX}anonymous{DENIED_PERFORM}{INVOKE_ACTION}{DENIED_RESOURCE}\
                 {service_arn}{PROBE_PATH} because no service-based policy allows the \
                 {INVOKE_ACTION} action"
            );
            assert_eq!(
                answered.body, expected,
                "case {case:?}: expected the refusal {expected:?}, got {:?}",
                answered.body
            );
            assert!(
                !answered.body.contains("raw_path"),
                "case {case:?}: a refused request must not reach the target, got {:?}",
                answered.body
            );
        }
    }
}
