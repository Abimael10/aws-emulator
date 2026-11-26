// SPDX-License-Identifier: CC0-1.0

//! The AUTH POLICY on the address the control plane ANNOUNCED: whether a SigV4-signed request
//! sent to `dnsEntry.domainName` — the one address a client that has read nothing but the
//! control plane holds — is decided by the policy `PutAuthPolicy` attached to the service.
//!
//! Two suites beside this one own the halves this one joins up: `announced_address_data_plane`
//! owns that the announced address answers the service's traffic at all, and
//! `auth_policy_enforcement` owns the policy decision for a request to the service's own
//! host-style name — and this suite drives that suite's own helpers (the statement, the signed
//! request, the policy write and the sentence's fixed clauses), so the two cannot drift apart
//! about what the plane emits. What is asserted HERE is that the two meet: that the policy
//! decides the request at the address a real client was handed, which is the addressing a
//! caller inside an associated VPC uses — it resolves `dnsEntry.domainName`, signs a `GET` for
//! the data plane's own signing name and sends it there.
//!
//! THE RULE: a request this plane serves is decided, before the listener's action is
//! performed, against the auth policy ACTIVE on the service — whichever of the service's
//! addresses the client reached it at. A request an `Allow` covers is served by the `LAMBDA`
//! target the listener's `forward` selects; one an explicit `Deny` refuses is answered HTTP
//! 403 with the `AccessDeniedException` sentence naming the caller, the data plane's action
//! and the service ARN with the request's path appended, and NOTHING behind the listener runs,
//! so none of the target's answer reaches the client.
//!
//! The rows span what the rule generalises over: the effect in force (an `Allow`, the same
//! statement at `Deny`, and the `Allow` put back, so the decision follows the live document
//! rather than the first one read), the resource the statement is scoped to (every path of the
//! service, and the exact path the request addresses) and the principal it names (the caller's
//! role, its session, and every principal) — with the role's own identity-based `Allow` held
//! constant, so a refusal can only be the auth policy's.

use crate::auth_policy_enforcement::{
    a_driver_session, put_auth_policy, signed_fetch, statement, AUTH_TYPE_AWS_IAM,
    AUTH_TYPE_NONE, DENIED_EXPLICIT_DENY, DENIED_PERFORM, DENIED_PREFIX, DENIED_RESOURCE,
    INVOKE_ACTION, PROBE_PATH,
};
use crate::listeners::{a_target_function, create_listener, settled_active};
use crate::targets::{a_target_group, register_targets};
use crate::vpclattice_service_slice::{Lattice, REGION};

/// The whole rule, over a table of documents, at the address `GetService` announced.
#[tokio::test]
async fn an_auth_policy_decides_a_signed_request_to_the_announced_address() {
    let (lattice, engine) = Lattice::with_lambda();
    let function_arn = a_target_function(&lattice, &engine, "announced-auth-target").await;

    // The control plane, through this service's own API: the network, the service with IAM
    // authentication, their association, a VPC association so a client is in a position to
    // reach the service at all, the `LAMBDA` target group holding the function, and the
    // HTTP:80 listener whose default action forwards to that group.
    let network = lattice
        .create_service_network(serde_json::json!({
            "name": "announced-auth-network",
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
            "name": "announced-auth-shop",
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
        a_target_group(&lattice, "announced-auth-function", "LAMBDA", None).await;
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
    settled_active(&lattice, "the service", &format!("/services/{service_id}")).await;
    settled_active(&lattice, "the target group", &format!("/targetgroups/{group_id}")).await;

    // What a client that has read nothing but the control plane holds: the address
    // `GetService` announces.
    let read = lattice.get_service(&service_id).await;
    assert_eq!(
        read.status, 200,
        "setup GetService: expected 200, got {} / {}",
        read.status, read.body
    );
    let announced = read.body["dnsEntry"]["domainName"]
        .as_str()
        .unwrap_or_else(|| panic!("setup GetService: announced no domain name, got {}", read.body))
        .to_string();

    // That address resolves to this service for an in-process client too — the code of a
    // function this emulator runs, which reads the address out of the control plane and dials
    // it on the listener's port.
    let answering = ::service_common::published_data_planes::PublishedPlanes::of(&*engine)
        .expect("the engine vends the published-plane records")
        .answering_service(&format!("{announced}:80"));
    assert_eq!(
        answering.as_deref(),
        Some("vpc-lattice"),
        "expected the announced address on the listener's port to be recorded as answered by \
         this service, got {answering:?}"
    );

    let (access_key_id, session_arn, role_arn) = a_driver_session(&engine, &service_arn);
    let whole_service = format!("{service_arn}/*");
    let addressed_resource = format!("{service_arn}{PROBE_PATH}");

    /// Which principal a row's statement names.
    enum Principal {
        Role,
        Session,
        Every,
    }
    /// What one row installs, and what the plane must answer it at the announced address.
    struct Row {
        case: &'static str,
        effect: &'static str,
        principal: Principal,
        /// Whether the statement is scoped to the exact path the request addresses rather
        /// than to every path of the service.
        exact_path: bool,
        status: u16,
    }
    let rows = &[
        Row {
            case: "an Allow naming the caller's role on every path of the service",
            effect: "Allow",
            principal: Principal::Role,
            exact_path: false,
            status: 200,
        },
        Row {
            case: "the same statement at Effect=Deny",
            effect: "Deny",
            principal: Principal::Role,
            exact_path: false,
            status: 403,
        },
        Row {
            case: "a Deny naming the session rather than its role",
            effect: "Deny",
            principal: Principal::Session,
            exact_path: false,
            status: 403,
        },
        Row {
            case: "a Deny scoped to the exact path the request addresses",
            effect: "Deny",
            principal: Principal::Role,
            exact_path: true,
            status: 403,
        },
        Row {
            case: "a Deny naming every principal",
            effect: "Deny",
            principal: Principal::Every,
            exact_path: false,
            status: 403,
        },
        Row {
            case: "the Allow put back",
            effect: "Allow",
            principal: Principal::Role,
            exact_path: false,
            status: 200,
        },
    ];

    for row in rows {
        let principal = match row.principal {
            Principal::Role => role_arn.clone(),
            Principal::Session => session_arn.clone(),
            Principal::Every => "*".to_string(),
        };
        let resource = if row.exact_path {
            addressed_resource.clone()
        } else {
            whole_service.clone()
        };
        let installed = put_auth_policy(
            &lattice,
            &service_id,
            &statement(row.effect, &principal, &resource),
        )
        .await;
        assert_eq!(
            installed, 200,
            "case {:?}: PutAuthPolicy expected 200, got {installed}",
            row.case
        );

        let answered = signed_fetch(&lattice, &announced, &access_key_id, REGION).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {} from the announced address {announced:?}, got {} \
             with body {:?}",
            row.case, row.status, answered.status, answered.body
        );
        if row.status == 200 {
            // Served: the body is the TARGET's own answer, which reaches the client no other
            // way — the function computed it from the request the forward handed it.
            let echoed: serde_json::Value =
                serde_json::from_str(&answered.body).unwrap_or_else(|error| {
                    panic!(
                        "case {:?}: expected the function's own JSON answer, got {:?} ({error})",
                        row.case, answered.body
                    )
                });
            assert_eq!(
                echoed["method"].as_str(),
                Some("GET"),
                "case {:?}: the target must be handed the request the client made, got \
                 {echoed}",
                row.case
            );
            continue;
        }
        // Refused: the sentence names the caller, the action and the service ARN with the
        // request's path appended, and the rule that refused it — and carries nothing of the
        // target, which was not invoked.
        let expected = format!(
            "{DENIED_PREFIX}{session_arn}{DENIED_PERFORM}{INVOKE_ACTION}{DENIED_RESOURCE}\
             {addressed_resource}{DENIED_EXPLICIT_DENY}"
        );
        assert_eq!(
            answered.body, expected,
            "case {:?}: expected the refusal {expected:?}, got {:?}",
            row.case, answered.body
        );
    }
}
