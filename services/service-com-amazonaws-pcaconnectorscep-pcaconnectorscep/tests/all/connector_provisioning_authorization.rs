// SPDX-License-Identifier: CC0-1.0

//! A connector comes up only if the principal that created it may have a certificate ISSUED
//! from the private CA it links.
//!
//! The RULE these cases assert, over a table rather than one recorded request: a connector
//! issues its own certificate from the private CA with the permissions of the principal that
//! created it, so when the permissions in force on that principal do not let it have a
//! certificate issued — a statement that DENIES `acm-pca:IssueCertificate` on that CA, or a
//! policy set that states permissions and could not allow that action on any CA at all — the
//! asynchronous creation lands in `FAILED` carrying the `ConnectorStatusReason` for a CA it
//! was not allowed to use, and a connector in that state serves nothing: `CreateChallenge` is
//! refused 400 `ValidationException` naming it as not active, and `ListChallengeMetadata` 400
//! `ValidationException` naming the state it is in. A policy set that could allow the issuance
//! — even scoped to another CA, since that is a grant of the action — withholds nothing, and
//! neither does a principal the account holds no policy for, since a policy record exists in
//! this emulator's IAM only where a client created it there.
//!
//! What the model says, and what it does not: `ConnectorStatusReason` declares
//! `PRIVATECA_ACCESS_DENIED` among its values and `Connector$StatusReason` is documented as
//! "Information about why connector creation failed, if status is FAILED" (smithy_query
//! shape=ConnectorStatusReason,Connector) — so the status and the reason are modelled, while
//! which authorization decides them, and the two sentences the refusals carry, are recorded
//! behaviour.
//!
//! So this suite mounts Private CA and IAM beside this service on ONE engine — what the
//! assembled emulator does — builds the CA through Private CA's own front door, attaches the
//! policy through IAM's own front door, and asks IAM's own simulation what those policies
//! decide, so no row rests on anything this suite seeded.
//!
//! Rows differ along what the rule generalises over: whether the principal states any policy
//! at all, whether the statement allows or denies, whether it covers the action (exactly, by
//! wildcard, or not at all), whether it covers the CA the connector links or another CA, and
//! whether the connector is public or restricted to a VPC endpoint.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{self, label};
use service_com_amazonaws_pcaconnectorscep_pcaconnectorscep as scep;

/// The Region every case in this suite is signed for — not the emulator default, so nothing
/// can pass by a hardcoded default.
const REGION: &str = "eu-west-2";

/// The Private CA action a connector performs with its creator's permissions, as an IAM policy
/// names it.
const ISSUE_CERTIFICATE: &str = "acm-pca:IssueCertificate";

/// The decision IAM's simulation reports for a pair the policies in force allow
/// (`PolicyEvaluationDecisionType`).
const ALLOWED: &str = "allowed";

/// The two sentences AWS refuses a connector in a state it cannot serve from with, spelled
/// here as the recorded wire text rather than read back out of the implementation.
fn not_active_message(resource_arn: &str) -> String {
    format!("Validation error detected: Resource(s) are not active: {resource_arn}.")
}

/// The second of them: a read that cannot be served from the state the record is in.
fn cannot_be_in_state_message(status: &str, resource_arn: &str) -> String {
    format!("Validation error detected: Resource cannot be in state {status}: {resource_arn}.")
}

/// An emulator holding this service, Amazon Web Services Private CA and IAM over one shared
/// engine, which is what the assembled emulator mounts.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere; these
    // cases are about these services' behaviour.
    router.set_request_authentication(false);
    router.register_service(scep::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_acmpca_acmprivateca::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// The IAM client this suite manages the calling identity's policies through: the shared
/// in-process hop every emulated service reaches IAM by.
fn iam(router: &MultiServiceRouter) -> ::aws_sdk_iam::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    ::service_common::iam_client::iam_client_through(&callback, REGION)
}

/// The principal every request to this emulator speaks for — the identity IAM holds among the
/// account's users and `sts:GetCallerIdentity` reports — read from the shared definition rather
/// than respelled here.
fn caller_arn() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The name IAM holds that principal under.
fn caller_name() -> &'static str {
    ::service_common::caller_principal::AMBIENT_USER_NAME
}

/// Attach one inline policy to the calling identity through IAM's own `PutUserPolicy`.
async fn put_caller_policy(router: &MultiServiceRouter, name: &str, document: &Value) {
    iam(router)
        .put_user_policy()
        .user_name(caller_name())
        .policy_name(name)
        .policy_document(document.to_string())
        .send()
        .await
        .expect("IAM must attach the inline policy to the calling identity");
}

/// What IAM's OWN simulation says the policies in force on the calling identity decide for
/// `action` on `resource` — asked, never assumed, so each row rests on IAM's answer.
async fn simulated_decision(router: &MultiServiceRouter, action: &str, resource: &str) -> String {
    iam(router)
        .simulate_principal_policy()
        .policy_source_arn(caller_arn())
        .action_names(action)
        .resource_arns(resource)
        .send()
        .await
        .expect("IAM must simulate the calling identity's policies")
        .evaluation_results()
        .first()
        .map(|result| result.eval_decision().as_str().to_string())
        .expect("IAM must report one evaluation result")
}

/// Create a connector linking `ca_arn`, make the transition its creation owes due, and answer
/// the settled `Connector` body.
async fn settled_connector(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
    ca_arn: &str,
    vpc_endpoint_id: Option<&str>,
) -> (String, harness::Answer) {
    let mut request = json!({ "CertificateAuthorityArn": ca_arn });
    if let Some(vpce) = vpc_endpoint_id {
        request["VpcEndpointId"] = json!(vpce);
    }
    let created = harness::call(router, REGION, "POST", "/connectors", Some(request)).await;
    assert_eq!(
        (created.status, created.error_type.as_str()),
        (202, ""),
        "case {case:?}: expected 202 from CreateConnector, got {} / {:?}",
        created.status,
        created.body
    );
    let arn = created.text("ConnectorArn");
    // The transition the clock owes is made due through the service's own state, so the next
    // read settles it: no case waits for the transition span.
    harness::release_pending(engine, &arn);
    let read = harness::call(
        router,
        REGION,
        "GET",
        &format!("/connectors/{}", label(&arn)),
        None,
    )
    .await;
    let status = read.connector("Status").as_str().unwrap_or_default();
    assert!(
        status == scep::state::STATUS_ACTIVE || status == scep::state::STATUS_FAILED,
        "case {case:?}: a connector whose transition is due reports a terminal status, got \
         {status:?} / {:?}",
        read.body
    );
    (arn, read)
}

/// One row: the policy the calling identity carries, and whether it withholds the issuance
/// the connector's creation needs.
#[derive(Debug)]
struct Row {
    name: &'static str,
    /// The statement the inline policy carries, or `None` where the principal is left
    /// carrying no policy at all.
    statement: Option<Statement>,
    /// Whether the connector's create restricts it to a VPC endpoint, so the rule is shown not
    /// to be about the rest of the request.
    vpc_endpoint_id: Option<&'static str>,
    /// Whether the creation must land in `FAILED` with the private-CA permission reason.
    withheld: bool,
}

/// The one statement a row's policy states.
#[derive(Debug, Clone, Copy)]
struct Statement {
    /// The statement's `Effect`.
    effect: &'static str,
    /// The `Action` element, as the policy writes it.
    action: &'static str,
    /// Whether the `Resource` element names the CA the connector links (`false` names another
    /// CA of the same account).
    covers_the_ca: bool,
}

/// A row carrying one statement.
fn row(
    name: &'static str,
    effect: &'static str,
    action: &'static str,
    covers_the_ca: bool,
    withheld: bool,
) -> Row {
    Row {
        name,
        statement: Some(Statement {
            effect,
            action,
            covers_the_ca,
        }),
        vpc_endpoint_id: None,
        withheld,
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // A principal the account holds no policy for: nothing is stated about what it
            // may do, so nothing is withheld.
            name: "a principal carrying no policy at all",
            statement: None,
            vpc_endpoint_id: None,
            withheld: false,
        },
        row(
            "an allow of exactly this action on this CA",
            "Allow",
            ISSUE_CERTIFICATE,
            true,
            false,
        ),
        row(
            "an allow of every action of that service on this CA",
            "Allow",
            "acm-pca:*",
            true,
            false,
        ),
        row(
            // A grant of the action scoped to ANOTHER CA is still a grant of the action, and
            // the weaker question a gate in front of a resource-scoped action asks is whether
            // the policy set could allow it at all.
            "an allow of this action on another CA",
            "Allow",
            ISSUE_CERTIFICATE,
            false,
            false,
        ),
        row(
            "a deny of exactly this action on this CA",
            "Deny",
            ISSUE_CERTIFICATE,
            true,
            true,
        ),
        row(
            "a deny of every action of that service on this CA",
            "Deny",
            "acm-pca:*",
            true,
            true,
        ),
        row(
            // Stated permissions that could not allow the issuance anywhere: every request
            // for it falls to the default deny.
            "an allow of another action of that service on this CA",
            "Allow",
            "acm-pca:DeleteCertificateAuthority",
            true,
            true,
        ),
        row(
            "a deny of this action on another CA, allowing nothing",
            "Deny",
            ISSUE_CERTIFICATE,
            false,
            true,
        ),
        Row {
            name: "an allow of this action on this CA, connector restricted to a VPC endpoint",
            statement: Some(Statement {
                effect: "Allow",
                action: ISSUE_CERTIFICATE,
                covers_the_ca: true,
            }),
            vpc_endpoint_id: Some("vpce-0123abcd"),
            withheld: false,
        },
        Row {
            name: "a deny of this action on this CA, connector restricted to a VPC endpoint",
            statement: Some(Statement {
                effect: "Deny",
                action: ISSUE_CERTIFICATE,
                covers_the_ca: true,
            }),
            vpc_endpoint_id: Some("vpce-00112233445566778"),
            withheld: true,
        },
    ]
}

/// The whole rule, row by row: what the calling identity's policies decide is what the
/// creation lands in.
#[tokio::test]
async fn a_connector_fails_when_its_creator_may_not_issue_from_the_private_ca() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.name;
        let (router, engine) = emulator();
        let (ca_arn, _held) =
            harness::active_private_ca(&router, &engine, REGION, "scep-authorization-linked").await;
        let (other_ca, _other_held) =
            harness::active_private_ca(&router, &engine, REGION, "scep-authorization-other").await;

        if let Some(statement) = row.statement {
            let resource = if statement.covers_the_ca {
                ca_arn.clone()
            } else {
                other_ca.clone()
            };
            put_caller_policy(
                &router,
                &format!("scep-issuance-{index}"),
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": statement.effect,
                        "Action": statement.action,
                        "Resource": resource,
                    }],
                }),
            )
            .await;
        }

        // What IAM itself decides for the pair the connector's creation needs: a creation this
        // service withholds must never be one IAM reports as allowed.
        let decision = simulated_decision(&router, ISSUE_CERTIFICATE, &ca_arn).await;
        if row.withheld {
            assert_ne!(
                decision, ALLOWED,
                "case {case:?}: IAM reported the issuance {decision:?}, so it must not be \
                 withheld"
            );
        }

        let (arn, settled) = settled_connector(
            &router,
            &engine,
            case,
            &ca_arn,
            row.vpc_endpoint_id,
        )
        .await;
        let expected = if row.withheld {
            (
                json!(scep::state::STATUS_FAILED),
                json!(scep::state::REASON_PRIVATECA_ACCESS_DENIED),
            )
        } else {
            (json!(scep::state::STATUS_ACTIVE), Value::Null)
        };
        assert_eq!(
            (
                settled.connector("Status").clone(),
                settled.connector("StatusReason").clone()
            ),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            settled.body
        );
        assert!(
            scep::ops::types::shared_types::CONNECTOR_STATUS_REASON_VALUES
                .contains(&scep::state::REASON_PRIVATECA_ACCESS_DENIED),
            "case {case:?}: the reason must be a declared ConnectorStatusReason, got {:?}",
            scep::ops::types::shared_types::CONNECTOR_STATUS_REASON_VALUES
        );

        // The listing projects the same record as the single read.
        let listed = harness::call(&router, REGION, "GET", "/connectors", None).await;
        let summary = listed.body["Connectors"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["Arn"] == json!(arn))
            .unwrap_or_else(|| panic!("case {case:?}: no ConnectorSummary for {arn:?}"))
            .clone();
        assert_eq!(
            (&summary["Status"], &summary["StatusReason"]),
            (
                settled.connector("Status"),
                settled.connector("StatusReason")
            ),
            "case {case:?}: ListConnectors must report what GetConnector does, got {summary:?}"
        );
    }
}

/// A connector whose creation was withheld serves nothing: the two calls that need it answer
/// the sentences this service refuses a connector in that state with, while the same two calls
/// on a connector that came up are served.
#[tokio::test]
async fn a_failed_connector_is_refused_by_the_calls_that_need_it() {
    // (the case, whether the creator may issue from the CA)
    for (case, authorized) in [
        ("a connector whose creation was withheld", false),
        ("a connector that came up", true),
    ] {
        let (router, engine) = emulator();
        let (ca_arn, _held) =
            harness::active_private_ca(&router, &engine, REGION, "scep-failed-connector").await;
        if !authorized {
            put_caller_policy(
                &router,
                "scep-issuance-withheld",
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Action": ISSUE_CERTIFICATE,
                        "Resource": ca_arn,
                    }],
                }),
            )
            .await;
        }

        let (arn, settled) = settled_connector(&router, &engine, case, &ca_arn, None).await;
        let expected_status = if authorized {
            scep::state::STATUS_ACTIVE
        } else {
            scep::state::STATUS_FAILED
        };
        assert_eq!(
            settled.connector("Status"),
            &json!(expected_status),
            "case {case:?}: expected {expected_status}, got {:?}",
            settled.body
        );

        let challenge = harness::call(
            &router,
            REGION,
            "POST",
            "/challenges",
            Some(json!({ "ConnectorArn": arn })),
        )
        .await;
        let metadata = harness::call(
            &router,
            REGION,
            "GET",
            &format!("/challengeMetadata?ConnectorArn={}", label(&arn)),
            None,
        )
        .await;

        if authorized {
            assert_eq!(
                (challenge.status, challenge.error_type.as_str()),
                (202, ""),
                "case {case:?}: an ACTIVE connector mints a challenge, got {} / {:?}",
                challenge.status,
                challenge.body
            );
            assert_eq!(
                metadata.status, 200,
                "case {case:?}: an ACTIVE connector lists its challenge metadata, got {} / {:?}",
                metadata.status, metadata.body
            );
            continue;
        }

        assert_eq!(
            (challenge.status, challenge.error_type.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a FAILED connector cannot mint a challenge, got {} / {:?}",
            challenge.status,
            challenge.body
        );
        assert_eq!(
            challenge.message(),
            not_active_message(&arn),
            "case {case:?}: expected the not-active sentence, got {:?}",
            challenge.message()
        );
        assert_eq!(
            (metadata.status, metadata.error_type.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a FAILED connector has no challenge metadata to list, got {} / {:?}",
            metadata.status,
            metadata.body
        );
        assert_eq!(
            metadata.message(),
            cannot_be_in_state_message(scep::state::STATUS_FAILED, &arn),
            "case {case:?}: expected the sentence naming the state the connector is in, got {:?}",
            metadata.message()
        );
    }
}
