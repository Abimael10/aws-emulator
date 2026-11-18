// SPDX-License-Identifier: CC0-1.0

//! Whether the certificate an operation NAMES is one a listener can actually PRESENT,
//! asked of the service that owns it.
//!
//! RULE: a listener that terminates TLS serves the handshake with its certificate's key
//! material, and the service that holds the certificate reports whether there is any —
//! Certificate Manager issues a certificate before it has one (`CertificateDetail$Status`:
//! "a certificate enters status PENDING_VALIDATION upon being requested … unless it
//! fails"). So every operation of this API that attaches a certificate to a listener
//! admits exactly the certificates that service reports `ISSUED`, and refuses a
//! certificate in ANY other status with HTTP 400, the error code
//! `UnsupportedCertificate` and the sentence Elastic Load Balancing states for it —
//! creating or changing nothing, and leaving the certificate as the owning service holds
//! it.
//!
//! The rows span what the rule generalises over: the status the owning service reports,
//! and how the certificate came to be in it — material imported with its private key
//! (`ISSUED`), a request still waiting on proof of domain control
//! (`PENDING_VALIDATION`), a request for a name the provider itself operates (`FAILED`)
//! and one for a name the registries reserve rather than delegate (`FAILED` by another
//! route). Each row is driven through all three operations that attach a certificate —
//! `CreateListener`, `AddListenerCertificates` and `ModifyListener` — the admitted row
//! read back through a describe, and every refused one required to leave the listener and
//! the certificate exactly as they were.
//!
//! Every certificate here is a real resource of Certificate Manager, created through that
//! service's own API over one shared engine, and every status asserted on is the one that
//! service answered rather than a value written here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

// The certificate material, the import that puts it in Certificate Manager's own store and
// the one call into that service are the HTTPS suite's, so no suite drifts on how a
// certificate gets into the service that holds it.
use crate::alb_https_listener_tls::{certificate_manager, import, mint};
use crate::provisioning_loop::{action, call, p, Reply};
// The crate's ONE reader of an element's text with the serializer's XML entities resolved,
// so a refusal message is compared as the VALUE the caller sees.
use crate::target_group_attributes_and_modification::unescaped;

/// The wire error code Elastic Load Balancing refuses a certificate no listener can present
/// with. The model declares no shape for it, so the code is the service's own
/// (`service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::UNSUPPORTED_CERTIFICATE`).
const UNSUPPORTED_CERTIFICATE: &str = "UnsupportedCertificate";

/// The status Certificate Manager reports for a certificate it holds key material for
/// (`CertificateStatus`, `smithy_query(query="shape",shape="com.amazonaws.acm#CertificateStatus")`).
const ISSUED: &str = "ISSUED";

/// The status of a request still waiting on proof of domain control, from the same enum.
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";

/// The status of a request that never got as far as validating, from the same enum.
const FAILED: &str = "FAILED";

/// The port every listener below is created on.
const PORT: &str = "443";

/// How long a case gives a status the clock owes to fall due. Generous: the case waits for
/// the CONDITION in a poll loop, so this only bounds a status that never arrives at all.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before asking the owning service again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A router carrying this service and the service that HOLDS certificates over ONE shared
/// engine, exactly as the assembled router mounts them.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// Request a public certificate for `domain_name` through Certificate Manager's own
/// `RequestCertificate`, and answer the ARN it was stored under.
async fn request_certificate(router: &MultiServiceRouter, case: &str, domain_name: &str) -> String {
    let body = serde_json::json!({ "DomainName": domain_name });
    let (status, parsed) = certificate_manager(router, "RequestCertificate", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestCertificate must answer 200, got {status} {parsed}"
    );
    parsed["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CertificateArn in {parsed}"))
        .to_string()
}

/// What Certificate Manager reports for `arn`: its status, and the resources it reports as
/// using the certificate.
async fn described(router: &MultiServiceRouter, case: &str, arn: &str) -> (String, Vec<String>) {
    let body = serde_json::json!({ "CertificateArn": arn });
    let (status, parsed) = certificate_manager(router, "DescribeCertificate", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeCertificate must answer 200, got {status} {parsed}"
    );
    let certificate = &parsed["Certificate"];
    let in_use_by = certificate["InUseBy"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    (
        certificate["Status"].as_str().unwrap_or_default().to_string(),
        in_use_by,
    )
}

/// Wait until Certificate Manager reports `status` for `arn`.
///
/// A status the clock owes is stored on the record and materialised by whichever call next
/// touches that service's store, so the wait is for the CONDITION: the owning service's own
/// read is asked again until it reports the status, or [`STATUS_DEADLINE`] passes.
async fn await_status(router: &MultiServiceRouter, case: &str, arn: &str, status: &str) {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let (reported, _) = described(router, case, arn).await;
        if reported == status {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the owning service to report {status} for {arn}, got \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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
async fn create_listener(router: &MultiServiceRouter, lb_arn: &str, certificate: &str) -> Reply {
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
        "case {case:?}: a listener naming an issued certificate must be created, got {} {}",
        reply.status, reply.body
    );
    reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no ListenerArn in {}", reply.body))
}

/// The certificate ARNs a listener's certificate LIST reports.
async fn listener_certificates(router: &MultiServiceRouter, listener_arn: &str) -> Vec<String> {
    let mut form = action("DescribeListenerCertificates");
    form.push(p("ListenerArn", listener_arn));
    let reply = call(router, &form).await;
    crate::provisioning_loop::members(&reply.body, "Certificates")
        .iter()
        .map(|member| {
            crate::provisioning_loop::first_text(member, "CertificateArn").unwrap_or_default()
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

/// Elastic Load Balancing's sentence for a certificate no listener can present: fixed text
/// around the one ARN the request carried.
fn expected_message(arn: &str) -> String {
    format!(
        "The certificate '{arn}' must have a fully-qualified domain name, a supported \
         signature, and a supported key size."
    )
}

/// Assert one refusal: the status, the awsQuery code and the whole message.
fn assert_unsupported(case: &str, operation: &str, reply: &Reply, arn: &str) {
    let stated = unescaped(&reply.text("Message").unwrap_or_default());
    assert_eq!(
        (reply.status, reply.error_code(), stated),
        (
            400,
            UNSUPPORTED_CERTIFICATE.to_string(),
            expected_message(arn)
        ),
        "case {case:?} on {operation}: expected 400 {UNSUPPORTED_CERTIFICATE} naming {arn:?}, \
         got {}",
        reply.body
    );
}

/// One row: how the case reads, the certificate it names, the status the owning service
/// reports for it, and whether a listener can present it.
struct Row {
    case: &'static str,
    arn: String,
    status: &'static str,
    presentable: bool,
}

/// The rows, every certificate created through the owning service's own API.
///
/// A request for a name the provider operates and one for a name the registries reserve
/// both end `FAILED`, by different routes, and a request for an ordinary registrable name
/// stays `PENDING_VALIDATION` while it waits for proof of domain control — so the table
/// reaches each status through the path that really produces it.
async fn rows(router: &MultiServiceRouter) -> Vec<Row> {
    let issued = import(
        router,
        "an imported certificate",
        &mint(&["issued.example.com"]),
    )
    .await;
    let pending = request_certificate(
        router,
        "a request waiting on proof of domain control",
        "pending-listener-probe.com",
    )
    .await;
    let provider_operated = request_certificate(
        router,
        "a request for a name the provider operates",
        "listener.amazonaws.com",
    )
    .await;
    let reserved = request_certificate(
        router,
        "a request for a name the registries reserve",
        "listener.example.org",
    )
    .await;
    // Both failures are outcomes the clock owes the request, so the rows wait for the
    // owning service to report them rather than assuming the status is already there.
    await_status(
        router,
        "a request for a name the provider operates",
        &provider_operated,
        FAILED,
    )
    .await;
    await_status(
        router,
        "a request for a name the registries reserve",
        &reserved,
        FAILED,
    )
    .await;
    vec![
        Row {
            case: "material imported with its private key",
            arn: issued,
            status: ISSUED,
            presentable: true,
        },
        Row {
            case: "a request still waiting on proof of domain control",
            arn: pending,
            status: PENDING_VALIDATION,
            presentable: false,
        },
        Row {
            case: "a request for a name the provider itself operates",
            arn: provider_operated,
            status: FAILED,
            presentable: false,
        },
        Row {
            case: "a request for a name the registries reserve",
            arn: reserved,
            status: FAILED,
            presentable: false,
        },
    ]
}

#[tokio::test]
async fn only_an_issued_certificate_is_admitted_wherever_a_listener_is_given_one() {
    let (router, _engine) = emulator();
    // The certificate every listener below starts with: imported with its private key, so
    // it is one a listener really can present.
    let baseline = import(
        &router,
        "the certificate a listener starts with",
        &mint(&["baseline.example.com"]),
    )
    .await;

    for (index, row) in rows(&router).await.iter().enumerate() {
        let case = row.case;
        // The row's premise: the owning service really reports the status the row is about.
        let (reported, _) = described(&router, case, &row.arn).await;
        assert_eq!(
            reported, row.status,
            "case {case:?}: the owning service must report {} for {}",
            row.status, row.arn
        );

        // (a) `CreateListener`: the default certificate of a listener being created. A
        // refused row must leave no listener behind at all.
        let create_lb = create_load_balancer(&router, &format!("usable-create-{index}")).await;
        let created = create_listener(&router, &create_lb, &row.arn).await;
        if row.presentable {
            assert_eq!(
                created.status, 200,
                "case {case:?} on CreateListener: expected 200, got {} {}",
                created.status, created.body
            );
            let listener = created
                .text("ListenerArn")
                .unwrap_or_else(|| panic!("case {case:?}: no ListenerArn"));
            assert_eq!(
                default_certificate(&router, &listener).await,
                row.arn,
                "case {case:?} on CreateListener: the listener must report the certificate it \
                 was created with"
            );
        } else {
            assert_unsupported(case, "CreateListener", &created, &row.arn);
            let mut listeners = action("DescribeListeners");
            listeners.push(p("LoadBalancerArn", &create_lb));
            let reported = call(&router, &listeners).await;
            assert!(
                crate::provisioning_loop::members(&reported.body, "Listeners").is_empty(),
                "case {case:?} on CreateListener: a refused create must leave no listener, got \
                 {}",
                reported.body
            );
            // And the certificate is left as the owning service holds it: the refusal makes
            // no load balancer a consumer of it, and moves its status nowhere.
            let (status, in_use_by) = described(&router, case, &row.arn).await;
            assert_eq!(
                (status.as_str(), in_use_by.as_slice()),
                (row.status, &[][..]),
                "case {case:?} on CreateListener: a refused create must leave the certificate \
                 untouched"
            );
        }

        // (b) `AddListenerCertificates`: an entry of the certificate LIST of a listener
        // that already presents a certificate it can serve. A refused row must leave that
        // list as it was.
        let add_lb = create_load_balancer(&router, &format!("usable-add-{index}")).await;
        let add_listener = listener_with(&router, case, &add_lb, &baseline).await;
        let mut form = action("AddListenerCertificates");
        form.push(p("ListenerArn", &add_listener));
        form.push(p("Certificates.member.1.CertificateArn", &row.arn));
        let added = call(&router, &form).await;
        let listed = listener_certificates(&router, &add_listener).await;
        if row.presentable {
            assert_eq!(
                added.status, 200,
                "case {case:?} on AddListenerCertificates: expected 200, got {} {}",
                added.status, added.body
            );
            assert!(
                listed.contains(&row.arn),
                "case {case:?} on AddListenerCertificates: the list must report the certificate \
                 added, got {listed:?}"
            );
        } else {
            assert_unsupported(case, "AddListenerCertificates", &added, &row.arn);
            assert!(
                !listed.contains(&row.arn),
                "case {case:?} on AddListenerCertificates: a refused call must add nothing, got \
                 {listed:?}"
            );
        }

        // (c) `ModifyListener`: the default certificate of a listener that already presents
        // one. A refused row must leave the listener presenting the certificate it had.
        let modify_lb = create_load_balancer(&router, &format!("usable-modify-{index}")).await;
        let modify_listener = listener_with(&router, case, &modify_lb, &baseline).await;
        let mut form = action("ModifyListener");
        form.push(p("ListenerArn", &modify_listener));
        form.push(p("Certificates.member.1.CertificateArn", &row.arn));
        let modified = call(&router, &form).await;
        let reported = default_certificate(&router, &modify_listener).await;
        if row.presentable {
            assert_eq!(
                (modified.status, reported.as_str()),
                (200, row.arn.as_str()),
                "case {case:?} on ModifyListener: expected the certificate named to be stored, \
                 got {} {}",
                modified.status,
                modified.body
            );
        } else {
            assert_unsupported(case, "ModifyListener", &modified, &row.arn);
            assert_eq!(
                reported, baseline,
                "case {case:?} on ModifyListener: a refused modification must leave the \
                 certificate it had, got {reported:?}"
            );
        }
    }
}
