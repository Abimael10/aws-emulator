// SPDX-License-Identifier: CC0-1.0

//! Whether the certificate an operation NAMES exists at all, asked of the service that
//! OWNS it.
//!
//! RULE: a `Certificate$CertificateArn` is the ARN of a certificate another service holds —
//! a Certificate Manager certificate, or a server certificate uploaded to IAM — so every
//! operation of this API that takes one resolves it against THAT service, and an ARN the
//! owning service reports no certificate for names no certificate of the account: the call
//! is refused with the modelled `CertificateNotFoundException` (`CertificateNotFound`, 400)
//! and the listener's certificates are left exactly as they were.
//!
//! The rows span what the rule generalises over: which owning service the ARN names, and
//! how an ARN fails to resolve — a certificate that is really there, one of the Region the
//! request addresses that nothing created, one of another Region, one of another account,
//! a server certificate of a name nothing uploaded, and a value that is no certificate ARN
//! at all. Each row is driven through all three operations that take a certificate —
//! `CreateListener`, `AddListenerCertificates` and `ModifyListener` — and every accepted
//! row is read back through a describe while every refused one must leave nothing behind.
//!
//! [`a_certificate_the_owning_service_cannot_be_asked_about_is_admitted`] pins the other
//! half: silence establishes nothing. With no Certificate Manager mounted there is nothing
//! to ask, so a well-formed certificate ARN is admitted rather than refused for want of
//! another service — the same reading this crate gives the facts it resolves against EC2.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

// The certificate material and the import that puts it in Certificate Manager's own store
// are the HTTPS suite's: one minting and one import for the crate, so no suite drifts on
// how a certificate gets into the service that holds it.
use crate::alb_https_listener_tls::{import, mint};
use crate::provisioning_loop::{account, action, call, p, Reply, REGION};
// The crate's ONE reader of an element's text with the serializer's XML entities resolved,
// so a refusal message is compared as the VALUE the caller sees.
use crate::target_group_attributes_and_modification::unescaped;

/// The awsQuery code of `CertificateNotFoundException`
/// (`smithy_query(query="shape",shape="CertificateNotFoundException")`: `awsQueryError`
/// code `CertificateNotFound`, `httpError` 400).
const CERTIFICATE_NOT_FOUND: &str = "CertificateNotFound";

/// The port every listener below is created on.
const PORT: &str = "443";

/// A router over ONE shared engine carrying this service and — when `owners` says so — the
/// two services that HOLD the certificates a listener can present.
///
/// Mounting them is what makes the resolution real: the certificate a row accepts is one
/// really imported through Certificate Manager's own API, and the absence a row is refused
/// for is that service's own answer.
fn emulator(owners: bool) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    if owners {
        router.register_service(
            service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
    }
    (router, engine)
}

/// Create one Application Load Balancer and answer its ARN.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) -> String {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: CreateLoadBalancer must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {name:?}: no LoadBalancerArn in {}", reply.body))
}

/// The fixed-response default action every listener below carries, so a listener needs no
/// target group of its own.
fn fixed_response(form: &mut Vec<(String, String)>) {
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        "200",
    ));
}

/// Call `CreateListener` for an HTTPS listener whose default certificate is `certificate`.
async fn create_listener(
    router: &MultiServiceRouter,
    lb_arn: &str,
    certificate: &str,
) -> Reply {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTPS"));
    form.push(p("Port", PORT));
    form.push(p("Certificates.member.1.CertificateArn", certificate));
    fixed_response(&mut form);
    call(router, &form).await
}

/// Create an HTTPS listener that must succeed, and answer its ARN.
async fn listener_with(
    router: &MultiServiceRouter,
    case: &str,
    lb_arn: &str,
    certificate: &str,
) -> String {
    let reply = create_listener(router, lb_arn, certificate).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: a listener naming a certificate that is there must be created, got \
         {} {}",
        reply.status, reply.body
    );
    reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no ListenerArn in {}", reply.body))
}

/// The certificates a listener reports, each as `(arn, is_default)`, read back through the
/// operation that reports the list.
async fn listener_certificates(
    router: &MultiServiceRouter,
    listener_arn: &str,
) -> Vec<(String, String)> {
    let mut form = action("DescribeListenerCertificates");
    form.push(p("ListenerArn", listener_arn));
    let reply = call(router, &form).await;
    crate::provisioning_loop::members(&reply.body, "Certificates")
        .iter()
        .map(|member| {
            (
                crate::provisioning_loop::first_text(member, "CertificateArn").unwrap_or_default(),
                crate::provisioning_loop::first_text(member, "IsDefault").unwrap_or_default(),
            )
        })
        .collect()
}

/// The default certificate a listener reports, read back through `DescribeListeners`.
async fn default_certificate(router: &MultiServiceRouter, listener_arn: &str) -> String {
    let mut form = action("DescribeListeners");
    form.push(p("ListenerArns.member.1", listener_arn));
    let reply = call(router, &form).await;
    reply.text("CertificateArn").unwrap_or_default()
}

/// Assert one refusal: the status, the awsQuery code and the whole message, which names the
/// value the request carried and nothing else.
fn assert_not_found(case: &str, reply: &Reply, arn: &str) {
    let stated = unescaped(&reply.text("Message").unwrap_or_default());
    assert_eq!(
        (reply.status, reply.error_code(), stated),
        (
            400,
            CERTIFICATE_NOT_FOUND.to_string(),
            format!("Certificate '{arn}' not found")
        ),
        "case {case:?}: expected 400 {CERTIFICATE_NOT_FOUND} naming {arn:?}, got {}",
        reply.body
    );
}

/// One row: how the case reads, the certificate ARN it names, and whether the service that
/// owns that certificate holds one under it.
struct Row {
    case: &'static str,
    value: String,
    held: bool,
}

/// The rows, built against `imported` — a certificate really imported into Certificate
/// Manager, and a different one from the certificate the listeners below start with, so the
/// accepted row really changes what each operation stores.
///
/// The refused forms walk the ways an ARN resolves to nothing: a Certificate Manager ARN of
/// this Region that nothing created, one of another Region, one of another account, a
/// server certificate of a name nothing uploaded to IAM, and a value that is no certificate
/// ARN at all.
fn rows(imported: &str) -> Vec<Row> {
    vec![
        Row {
            case: "an imported Certificate Manager certificate",
            value: imported.to_string(),
            held: true,
        },
        Row {
            case: "a Certificate Manager ARN of this Region that nothing created",
            value: format!(
                "arn:aws:acm:{REGION}:{}:certificate/00000000-0000-0000-0000-000000000000",
                account()
            ),
            held: false,
        },
        Row {
            case: "a Certificate Manager ARN of another Region",
            value: format!(
                "arn:aws:acm:eu-west-1:{}:certificate/11111111-1111-1111-1111-111111111111",
                account()
            ),
            held: false,
        },
        Row {
            case: "a Certificate Manager ARN of another account",
            value: format!(
                "arn:aws:acm:{REGION}:210987654321:certificate/22222222-2222-2222-2222-222222222222"
            ),
            held: false,
        },
        Row {
            case: "a server certificate name nothing uploaded to IAM",
            value: format!("arn:aws:iam::{}:server-certificate/never-uploaded", account()),
            held: false,
        },
        Row {
            case: "a value that is no certificate ARN at all",
            value: "certificate/not-an-arn".to_string(),
            held: false,
        },
    ]
}

#[tokio::test]
async fn a_certificate_the_owning_service_holds_none_for_is_refused_wherever_one_is_named() {
    let (router, _engine) = emulator(true);
    // Two certificates really imported: the one every listener below starts with, and the
    // one the accepted row names.
    let baseline = import(
        &router,
        "the certificate a listener starts with",
        &mint(&["baseline.example.com"]),
    )
    .await;
    let imported = import(
        &router,
        "the certificate the accepted row names",
        &mint(&["imported.example.com"]),
    )
    .await;

    for (index, row) in rows(&imported).iter().enumerate() {
        // (a) `CreateListener`: the default certificate of a listener being created. A
        // refused row must leave no listener behind at all.
        let create_lb = create_load_balancer(&router, &format!("cert-create-{index}")).await;
        let created = create_listener(&router, &create_lb, &row.value).await;
        if row.held {
            assert_eq!(
                created.status, 200,
                "case {:?} on CreateListener: expected 200, got {} {}",
                row.case, created.status, created.body
            );
            let listener = created
                .text("ListenerArn")
                .unwrap_or_else(|| panic!("case {:?}: no ListenerArn", row.case));
            assert_eq!(
                default_certificate(&router, &listener).await,
                row.value,
                "case {:?} on CreateListener: the listener must report the certificate it \
                 was created with",
                row.case
            );
        } else {
            assert_not_found(row.case, &created, &row.value);
            let mut listeners = action("DescribeListeners");
            listeners.push(p("LoadBalancerArn", &create_lb));
            let reported = call(&router, &listeners).await;
            assert!(
                crate::provisioning_loop::members(&reported.body, "Listeners").is_empty(),
                "case {:?} on CreateListener: a refused create must leave no listener, got {}",
                row.case,
                reported.body
            );
        }

        // (b) `AddListenerCertificates`: an entry of the certificate LIST of a listener
        // that already presents another certificate. A refused row must leave that list as
        // it was.
        let add_lb = create_load_balancer(&router, &format!("cert-add-{index}")).await;
        let add_listener = listener_with(&router, row.case, &add_lb, &baseline).await;
        let mut form = action("AddListenerCertificates");
        form.push(p("ListenerArn", &add_listener));
        form.push(p("Certificates.member.1.CertificateArn", &row.value));
        let added = call(&router, &form).await;
        let listed = listener_certificates(&router, &add_listener).await;
        if row.held {
            assert_eq!(
                added.status, 200,
                "case {:?} on AddListenerCertificates: expected 200, got {} {}",
                row.case, added.status, added.body
            );
            assert!(
                listed.iter().any(|(arn, _)| arn == &row.value),
                "case {:?} on AddListenerCertificates: the list must report the certificate \
                 added, got {listed:?}",
                row.case
            );
        } else {
            assert_not_found(row.case, &added, &row.value);
            assert!(
                listed.iter().all(|(arn, _)| arn != &row.value),
                "case {:?} on AddListenerCertificates: a refused call must add nothing, got \
                 {listed:?}",
                row.case
            );
        }

        // (c) `ModifyListener`: the default certificate of a listener that already has one.
        // A refused row must leave the listener presenting the certificate it had.
        let modify_lb = create_load_balancer(&router, &format!("cert-modify-{index}")).await;
        let modify_listener = listener_with(&router, row.case, &modify_lb, &baseline).await;
        let mut form = action("ModifyListener");
        form.push(p("ListenerArn", &modify_listener));
        form.push(p("Certificates.member.1.CertificateArn", &row.value));
        let modified = call(&router, &form).await;
        let reported = default_certificate(&router, &modify_listener).await;
        if row.held {
            assert_eq!(
                (modified.status, reported.as_str()),
                (200, row.value.as_str()),
                "case {:?} on ModifyListener: expected the certificate named to be stored, \
                 got {} {}",
                row.case,
                modified.status,
                modified.body
            );
        } else {
            assert_not_found(row.case, &modified, &row.value);
            assert_eq!(
                reported, baseline,
                "case {:?} on ModifyListener: a refused modification must leave the \
                 certificate it had, got {reported:?}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_certificate_the_owning_service_cannot_be_asked_about_is_admitted() {
    // No Certificate Manager and no IAM mounted: there is nothing to ask, so nothing is
    // known about the certificate and nothing is refused on it — a refusal is never assumed
    // from silence. The values are the very ones the table above refuses WITH those
    // services mounted, so this test pins the difference to the answer rather than to the
    // ARN.
    let (router, _engine) = emulator(false);
    let silent: [(&str, String); 2] = [
        (
            "a Certificate Manager certificate nothing created",
            format!(
                "arn:aws:acm:{REGION}:{}:certificate/00000000-0000-0000-0000-000000000000",
                account()
            ),
        ),
        (
            "a server certificate nothing uploaded",
            format!("arn:aws:iam::{}:server-certificate/never-uploaded", account()),
        ),
    ];
    for (index, (case, value)) in silent.iter().enumerate() {
        let lb = create_load_balancer(&router, &format!("cert-silent-{index}")).await;
        let listener = listener_with(&router, case, &lb, value).await;
        assert_eq!(
            default_certificate(&router, &listener).await,
            *value,
            "case {case:?}: the listener must report the certificate it was created with"
        );

        let mut form = action("AddListenerCertificates");
        form.push(p("ListenerArn", &listener));
        form.push(p("Certificates.member.1.CertificateArn", value));
        let added = call(&router, &form).await;
        assert_eq!(
            added.status, 200,
            "case {case:?} on AddListenerCertificates: expected 200, got {} {}",
            added.status, added.body
        );
    }
}
