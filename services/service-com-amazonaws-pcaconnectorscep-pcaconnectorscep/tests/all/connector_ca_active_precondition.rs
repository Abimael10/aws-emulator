// SPDX-License-Identifier: CC0-1.0

//! The private CA a create links has to be IN SERVICE before a connector is admitted.
//!
//! The RULE these cases assert, over a table rather than one recorded request: a referenced
//! resource whose owning service does not report it in its active state is a PRECONDITION of
//! the call, so `CreateConnector` refuses such a `CertificateAuthorityArn` synchronously —
//! HTTP 400 `ValidationException` with the sentence this service words a not-active reference
//! with ("Validation error detected: Resource(s) are not active: <arn>.") — creates no
//! connector at all, and admits the very same request once Private CA reports that CA
//! `ACTIVE`. A connector cannot issue its own certificate from a CA that is not in service,
//! and no amount of waiting changes that, so the outcome is not something the creation can
//! land in.
//!
//! What the model says, and what it does not: `CreateConnector` models
//! `ValidationException` (errorKind=client, httpError=400, required `Message`, optional
//! `Reason`) and `CertificateAuthorityArn` is `@required` with a `@pattern` that constrains
//! ARN SYNTAX only (smithy_query operation=CreateConnector) — so the precondition itself, and
//! the sentence it is refused with, are recorded behaviour rather than anything the model
//! states.
//!
//! So this suite mounts Private CA beside this service on ONE engine — what the assembled
//! emulator does — and builds every CA through that service's own front door, asking it for
//! the status each row rests on rather than assuming it.
//!
//! Rows differ along what the rule generalises over: which status of Private CA's own
//! catalogue the CA is in (still being created, waiting for its certificate, in service,
//! disabled again, deleted), how it got there, the Region the CA and the connector live in,
//! and whether the request also restricts the connector to a VPC endpoint.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{self, label};
use service_com_amazonaws_pcaconnectorscep_pcaconnectorscep as scep;

use service_com_amazonaws_acmpca_acmprivateca::state as pca;

/// The Region most cases are signed for — not the emulator default, so nothing can pass by a
/// hardcoded default.
const REGION: &str = "eu-west-2";

/// A second Region this emulator also serves, so the rule is not one Region's.
const OTHER_REGION: &str = "ap-northeast-1";

/// The sentence AWS refuses a not-active reference with, spelled here as the recorded wire
/// text rather than read back out of the implementation: the fixed words, then the ARN the
/// request named, then a terminating `.`.
fn not_active_message(resource_arn: &str) -> String {
    format!("Validation error detected: Resource(s) are not active: {resource_arn}.")
}

/// An emulator holding this service AND Amazon Web Services Private CA over one shared
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
    (router, engine)
}

/// One `CreateConnector` call for `ca_arn`, signed for `region`, optionally restricted to a
/// VPC endpoint.
async fn create_connector(
    router: &MultiServiceRouter,
    region: &str,
    ca_arn: &str,
    vpc_endpoint_id: Option<&str>,
) -> harness::Answer {
    let mut request = json!({ "CertificateAuthorityArn": ca_arn });
    if let Some(vpce) = vpc_endpoint_id {
        request["VpcEndpointId"] = json!(vpce);
    }
    harness::call(router, region, "POST", "/connectors", Some(request)).await
}

/// How many connectors of `region` name `ca_arn` — read through `ListConnectors`, so a
/// "nothing was created" claim rests on what this service reports rather than on its store.
async fn connectors_for(router: &MultiServiceRouter, region: &str, ca_arn: &str) -> usize {
    let listed = harness::call(router, region, "GET", "/connectors", None).await;
    listed.body["Connectors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["CertificateAuthorityArn"] == json!(ca_arn))
        .count()
}

/// Assert one row's create was refused for the precondition: the modelled 400, the sentence,
/// and no connector left behind for that CA.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    ca_arn: &str,
    vpc_endpoint_id: Option<&str>,
) {
    let refused = create_connector(router, region, ca_arn, vpc_endpoint_id).await;
    assert_eq!(
        (refused.status, refused.error_type.as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException for {ca_arn:?}, got {} / {:?}",
        refused.status,
        refused.body
    );
    assert_eq!(
        refused.message(),
        not_active_message(ca_arn),
        "case {case:?}: expected the not-active sentence, got {:?}",
        refused.message()
    );
    // The implementation's own builder must word that same sentence, so every operation this
    // service refuses a not-active reference with states it identically.
    assert_eq!(
        scep::state::resources_not_active_message(ca_arn),
        not_active_message(ca_arn),
        "case {case:?}: the service's builder must word AWS's sentence"
    );
    assert_eq!(
        connectors_for(router, region, ca_arn).await,
        0,
        "case {case:?}: the refused create must leave no connector naming {ca_arn:?}"
    );
}

/// Every status Private CA reports a CA in other than `ACTIVE` refuses the create, and the
/// `ACTIVE` one is admitted — whatever else the request carries.
#[tokio::test]
async fn a_create_is_refused_unless_private_ca_reports_the_ca_active() {
    let (router, engine) = emulator();

    // The CAs, each built through Private CA's own API and left in a different status: one
    // still being created (its CREATING -> PENDING_CERTIFICATE move not yet made due), one
    // waiting for its certificate, one in service, one put in service and disabled again, and
    // one deleted inside its restoration period.
    let creating = harness::created_private_ca(&router, REGION, "scep-precondition-creating").await;
    let pending =
        harness::pending_certificate_private_ca(&router, &engine, REGION, "scep-precondition-pending")
            .await;
    let (active, _held) =
        harness::active_private_ca(&router, &engine, REGION, "scep-precondition-active").await;
    let (disabled, _disabled_held) =
        harness::active_private_ca(&router, &engine, REGION, "scep-precondition-disabled").await;
    harness::private_ca_client(&router, REGION)
        .update_certificate_authority()
        .certificate_authority_arn(&disabled)
        .status(::aws_sdk_acmpca::types::CertificateAuthorityStatus::Disabled)
        .send()
        .await
        .expect("Private CA must disable the certificate authority");
    let deleted =
        harness::pending_certificate_private_ca(&router, &engine, REGION, "scep-precondition-deleted")
            .await;
    harness::private_ca_client(&router, REGION)
        .delete_certificate_authority()
        .certificate_authority_arn(&deleted)
        .send()
        .await
        .expect("Private CA must delete the certificate authority");

    // The rows: the CA, whether the create must be admitted, and a VPC endpoint id on some of
    // them, so the precondition is shown to be about the CA and not about the rest of the
    // request.
    let rows: Vec<(&str, &String, bool, Option<&str>)> = vec![
        ("a CA still being created", &creating, false, None),
        ("a CA waiting for its certificate", &pending, false, None),
        ("a CA in service", &active, true, None),
        ("a CA its account disabled again", &disabled, false, None),
        (
            "a CA inside the restoration period of its delete",
            &deleted,
            false,
            None,
        ),
        (
            "a CA waiting for its certificate, restricted to a VPC endpoint",
            &pending,
            false,
            Some("vpce-0123abcd"),
        ),
        (
            "a CA in service, restricted to a VPC endpoint",
            &active,
            true,
            Some("vpce-00112233445566778"),
        ),
    ];

    for (case, ca_arn, admitted, vpc_endpoint_id) in rows {
        // What Private CA itself reports: the rule is about that answer, so the row's
        // expectation is checked against it rather than against how this suite built the CA.
        let reported = harness::reported_private_ca_status(&router, REGION, ca_arn).await;
        assert_eq!(
            reported == pca::STATUS_ACTIVE,
            admitted,
            "case {case:?}: a create is admitted exactly when Private CA reports {:?}; it \
             reports {reported:?}",
            pca::STATUS_ACTIVE
        );

        if !admitted {
            assert_refused(&router, case, REGION, ca_arn, vpc_endpoint_id).await;
            continue;
        }

        let created = create_connector(&router, REGION, ca_arn, vpc_endpoint_id).await;
        assert_eq!(
            (created.status, created.error_type.as_str()),
            (202, ""),
            "case {case:?}: a CA in service must be admitted, got {} / {:?}",
            created.status,
            created.body
        );
        let arn = created.text("ConnectorArn");
        harness::release_pending(&engine, &arn);
        let read = harness::call(
            &router,
            REGION,
            "GET",
            &format!("/connectors/{}", label(&arn)),
            None,
        )
        .await;
        assert_eq!(
            read.connector("CertificateAuthorityArn"),
            &json!(ca_arn),
            "case {case:?}: the connector must carry the CA its create named, got {:?}",
            read.body
        );
    }
}

/// The refusal is about the CA's status at the instant of the call, not about the request: the
/// identical call is refused while the CA is waiting for its certificate and succeeds once
/// that same CA is in service — in either Region this suite uses.
#[tokio::test]
async fn the_same_request_succeeds_once_that_ca_reaches_active() {
    for (case, region) in [
        ("the Region most cases use", REGION),
        ("a second Region", OTHER_REGION),
    ] {
        let (router, engine) = emulator();
        let ca_arn =
            harness::pending_certificate_private_ca(&router, &engine, region, "scep-waited-for")
                .await;
        assert_eq!(
            harness::reported_private_ca_status(&router, region, &ca_arn).await,
            pca::STATUS_PENDING_CERTIFICATE,
            "case {case:?}: the CA must start out waiting for its certificate"
        );
        assert_refused(&router, case, region, &ca_arn, None).await;

        // That same CA is then put in service through Private CA's own procedure, and the
        // identical call is made again.
        harness::activate_private_ca(&router, &engine, region, &ca_arn).await;
        assert_eq!(
            harness::reported_private_ca_status(&router, region, &ca_arn).await,
            pca::STATUS_ACTIVE,
            "case {case:?}: the CA must now be in service"
        );

        let created = create_connector(&router, region, &ca_arn, None).await;
        assert_eq!(
            (created.status, created.error_type.as_str()),
            (202, ""),
            "case {case:?}: once the CA is in service the same call must be admitted, got {} / \
             {:?}",
            created.status,
            created.body
        );
        let arn = created.text("ConnectorArn");
        harness::release_pending(&engine, &arn);
        let read = harness::call(
            &router,
            region,
            "GET",
            &format!("/connectors/{}", label(&arn)),
            None,
        )
        .await;
        assert_eq!(
            (read.connector("Status"), read.connector("StatusReason")),
            (&json!(scep::state::STATUS_ACTIVE), &serde_json::Value::Null),
            "case {case:?}: the connector linking a CA in service must come up, got {:?}",
            read.body
        );
        assert_eq!(
            connectors_for(&router, region, &ca_arn).await,
            1,
            "case {case:?}: exactly the one connector the admitted call created must be listed"
        );
    }
}
