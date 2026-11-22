// SPDX-License-Identifier: CC0-1.0

//! A certificate Certificate Manager will never issue, offered to an Elastic Load Balancing
//! listener that terminates TLS — the whole hand-off, over both services' own wires
//! (`awsJson1_1` for Certificate Manager, awsQuery for Elastic Load Balancing) on ONE
//! engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULES under test:
//!
//! 1. A certificate request fails FAST when it names a domain no evidence from the account
//!    could ever cover: "a certificate enters status PENDING_VALIDATION upon being
//!    requested, unless it fails for any of the reasons given in the troubleshooting topic
//!    Certificate request fails" (`smithy_query(query="doc",
//!    shape="com.amazonaws.acm#CertificateDetail", member="Status")`). The CLOCK owes such a
//!    request a terminal `CertificateStatus` `FAILED` carrying the `FailureReason` that
//!    names verification obtained outside the account,
//!    `ADDITIONAL_VERIFICATION_REQUIRED` — the reason "exists only when the certificate
//!    status is FAILED" (`CertificateDetail$FailureReason` doc) — while the request is still
//!    one of the certificates ACM would have provided (`Type` `AMAZON_ISSUED`,
//!    `CertificateDetail$Type` doc) with the requested `DomainName` echoed and nothing
//!    issued behind it, so no `Serial` and no consumer.
//! 2. A listener that terminates TLS serves the handshake with its certificate's key
//!    material, and only a certificate the owning service reports `ISSUED` has any. So
//!    Elastic Load Balancing admits exactly those, and refuses a request naming a
//!    certificate in any other status with HTTP 400, the error code
//!    `UnsupportedCertificate` and the sentence it states for that code — whose only
//!    variable part is the certificate ARN the request carried — creating NO listener and
//!    leaving the certificate exactly as the owning service holds it.
//! 3. The admitted counterpart is the control: a certificate the owning service reports
//!    `ISSUED` is accepted on the byte-identical call, becomes the load balancer's only
//!    listener, is the one entry of that listener's certificate list, and makes Certificate
//!    Manager report the LOAD BALANCER among the resources using it
//!    (`CertificateDetail$InUseBy`, "a list of ARNs for the Amazon Web Services resources
//!    that are using the certificate").
//!
//! This suite lives here because neither service can show the rules on its own: the status
//! is Certificate Manager's fact, the refusal is Elastic Load Balancing's, and the refusal
//! is reached by READING that status out of the one shared engine. What it adds over
//! `elbv2_certificate_in_use_by_acm.rs` (which drives an issued certificate through to the
//! `InUseBy` reference) is the never-issued half and the refusal it earns.
//!
//! The rows span what rule 2 generalises over: EVERY entry of the three catalogs the
//! certificate service publishes for names no account can prove control of (its reserved
//! top-level labels, its reserved second-level names and the domains the provider operates,
//! whose sources are RFC 2606/6761/6762/7686, IANA's Example Domains and the provider's own
//! zones), the form of the requested name (a plain label, a wildcard, several labels deep,
//! mixed case), the `ValidationMethod` asked for (every modelled value and the absence of
//! one) and the TLS-terminating listener protocol and port the certificate is offered to.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_acm_certificatemanager::state::{
    FAILURE_REASON_ADDITIONAL_VERIFICATION_REQUIRED, PROVIDER_OPERATED_DOMAINS,
    RESERVED_SECOND_LEVEL_NAMES, RESERVED_TOP_LEVEL_LABELS,
};
use smithy_server_runtime::MultiServiceRouter;

// The two services' wires, their shared mounting and the certificate that reaches `ISSUED`
// are the sibling suite's: ONE writer of each for this crate, so the two suites cannot drift
// on how the hand-off is wired.
use crate::elbv2_certificate_in_use_by_acm::{
    acm, described, elbv2, elbv2_ok, emulator, in_use_by, issued_certificate,
};
// The ONE element reader this test crate keeps for awsQuery suites (`all/query_wire.rs`:
// "Nothing new should write another one — call this module"); `elements` resolves the
// serializer's XML entities, so a refusal sentence compares as the VALUE a client sees.
use crate::query_wire::{blocks_of, elements, members_of, opt_element};

/// `CertificateStatus` values this suite reads
/// (`smithy_query(query="shape", shape="com.amazonaws.acm#CertificateStatus")`).
const FAILED: &str = "FAILED";
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";

/// `CertificateType` of a certificate ACM was asked to provide
/// (`CertificateDetail$Type` doc: "for certificates provided by ACM, this value is
/// AMAZON_ISSUED").
const AMAZON_ISSUED: &str = "AMAZON_ISSUED";

/// The wire error code Elastic Load Balancing refuses a certificate no listener can present
/// with. The model declares no shape for it, so the code is that service's own
/// (`service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::UNSUPPORTED_CERTIFICATE`).
const UNSUPPORTED_CERTIFICATE: &str = "UnsupportedCertificate";

/// How long a wait for a status the clock owes may run. Generous: every wait below polls for
/// the CONDITION, so this bounds only a status that never arrives at all.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before asking the owning service again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The forms a requested name takes under one of the catalog suffixes. Cycled with a
/// different period from the methods and the listener brackets, so one form is not asserted
/// only under one method.
const FORMS: &[&str] = &["plain", "wildcard", "deep", "mixed case"];

/// Every `ValidationMethod` the model declares (`smithy_query(query="shape",
/// shape="com.amazonaws.acm#ValidationMethod")`). The absence of one is cycled in beside
/// them.
const METHODS: &[&str] = &["DNS", "EMAIL", "HTTP"];

/// The TLS-terminating listener protocols a certificate is bracketed to, each with the port
/// the row offers it on.
const BRACKETS: &[(&str, &str)] = &[("HTTPS", "443"), ("TLS", "8443"), ("HTTPS", "8443")];

/// Every suffix the certificate service publishes as one no evidence from the account could
/// cover. Read from the production catalogs rather than written out here, so a catalog that
/// grows is covered with it.
fn unvalidatable_suffixes() -> Vec<&'static str> {
    RESERVED_TOP_LEVEL_LABELS
        .iter()
        .chain(RESERVED_SECOND_LEVEL_NAMES)
        .chain(PROVIDER_OPERATED_DOMAINS)
        .copied()
        .collect()
}

/// The name one row requests under `suffix`, in the form `FORMS[form]` names.
fn requested_name(form: usize, suffix: &str) -> String {
    match FORMS[form] {
        "wildcard" => format!("*.{suffix}"),
        "deep" => format!("a.b.{suffix}"),
        // DNS labels are case-insensitive, so the catalogs must match whatever case the
        // request spelled — and the answer must echo that spelling back.
        "mixed case" => format!("Listener.{}", suffix.to_uppercase()),
        _ => format!("listener.{suffix}"),
    }
}

/// One row: how it reads, the `RequestCertificate` body it sends, the name it requested and
/// the listener bracket the certificate is offered to.
struct Row {
    case: String,
    body: Value,
    name: String,
    protocol: &'static str,
    port: &'static str,
}

/// One row per catalog entry, cycling the name forms, the validation methods (every
/// modelled value and, every fourth row, none at all) and the listener brackets.
fn rows() -> Vec<Row> {
    unvalidatable_suffixes()
        .into_iter()
        .enumerate()
        .map(|(index, suffix)| {
            let name = requested_name(index % FORMS.len(), suffix);
            let method = if index % 4 == 3 {
                None
            } else {
                Some(METHODS[index % METHODS.len()])
            };
            let mut body = json!({ "DomainName": name });
            if let Some(method) = method {
                body["ValidationMethod"] = json!(method);
            }
            let (protocol, port) = BRACKETS[index % BRACKETS.len()];
            Row {
                case: format!(
                    "{name} validated by {}, offered to a {protocol}:{port} listener",
                    method.unwrap_or("no named method")
                ),
                body,
                name,
                protocol,
                port,
            }
        })
        .collect()
}

/// Request one certificate through Certificate Manager's own operation and answer the ARN it
/// minted.
async fn request_certificate(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, answered) = acm(router, "RequestCertificate", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestCertificate must answer 200, got {status} with {answered}"
    );
    answered["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CertificateArn in {answered}"))
        .to_string()
}

/// Poll Certificate Manager until it reports a status other than `PENDING_VALIDATION` for
/// `arn`, which is what a client's waiter does, and answer what it then reports.
///
/// Nothing here touches the owning service's stored instants: the outcome has to fall due on
/// its own, so a request parked non-terminally would spin this loop out instead of settling.
async fn settled(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let detail = described(router, case, arn).await;
        if detail["Status"].as_str() != Some(PENDING_VALIDATION) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: waiting for a terminal status gave up, still {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Elastic Load Balancing's sentence for a certificate no listener can present: fixed text
/// around the one ARN the request carried.
fn expected_message(arn: &str) -> String {
    format!(
        "The certificate '{arn}' must have a fully-qualified domain name, a supported \
         signature, and a supported key size."
    )
}

/// Create one Application Load Balancer and answer its ARN.
async fn load_balancer(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = elbv2_ok(
        router,
        case,
        "CreateLoadBalancer",
        &[
            ("Name", name),
            ("Subnets.member.1", "subnet-11111111"),
            ("Subnets.member.2", "subnet-22222222"),
        ],
    )
    .await;
    opt_element(&created, "LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no LoadBalancerArn in {created}"))
}

/// The `CreateListener` form a row sends: one TLS-terminating listener whose default
/// certificate is `certificate` and whose default action needs no target group.
fn create_listener_form<'a>(
    lb_arn: &'a str,
    protocol: &'a str,
    port: &'a str,
    certificate: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("LoadBalancerArn", lb_arn),
        ("Protocol", protocol),
        ("Port", port),
        ("Certificates.member.1.CertificateArn", certificate),
        ("DefaultActions.member.1.Type", "fixed-response"),
        (
            "DefaultActions.member.1.FixedResponseConfig.StatusCode",
            "200",
        ),
        (
            "DefaultActions.member.1.FixedResponseConfig.ContentType",
            "text/plain",
        ),
        (
            "DefaultActions.member.1.FixedResponseConfig.MessageBody",
            "served by the admitted certificate",
        ),
    ]
}

/// The listener ARNs `DescribeListeners` reports for one load balancer.
async fn listeners_of(router: &MultiServiceRouter, case: &str, lb_arn: &str) -> Vec<String> {
    let reported = elbv2_ok(
        router,
        case,
        "DescribeListeners",
        &[("LoadBalancerArn", lb_arn)],
    )
    .await;
    blocks_of(&reported, "Listeners")
        .first()
        .map(|list| {
            members_of(list)
                .into_iter()
                .filter_map(|member| opt_element(member, "ListenerArn"))
                .collect()
        })
        .unwrap_or_default()
}

/// The certificate ARNs one listener's certificate list reports.
async fn listener_certificates(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
) -> Vec<String> {
    let reported = elbv2_ok(
        router,
        case,
        "DescribeListenerCertificates",
        &[("ListenerArn", listener_arn)],
    )
    .await;
    blocks_of(&reported, "Certificates")
        .first()
        .map(|list| {
            members_of(list)
                .into_iter()
                .filter_map(|member| opt_element(member, "CertificateArn"))
                .collect()
        })
        .unwrap_or_default()
}

/// RULE 1: the terminal failure a request for an uncoverable name settles into, read off
/// what the owning service reports.
fn assert_terminal_failure(row: &Row, detail: &Value) {
    let case = row.case.as_str();
    assert_eq!(
        (
            detail["Status"].as_str(),
            detail["FailureReason"].as_str(),
            detail["Type"].as_str(),
            detail["DomainName"].as_str(),
        ),
        (
            Some(FAILED),
            Some(FAILURE_REASON_ADDITIONAL_VERIFICATION_REQUIRED),
            Some(AMAZON_ISSUED),
            Some(row.name.as_str()),
        ),
        "case {case:?}: a request for a name no evidence from this account could cover must \
         settle FAILED with the reason it failed for, stay AMAZON_ISSUED and echo the \
         requested name, got {detail}"
    );
    assert!(
        detail["Serial"].is_null(),
        "case {case:?}: nothing was issued, so there is no Serial, got {detail}"
    );
    assert_eq!(
        detail["InUseBy"].as_array().map(Vec::as_slice),
        Some(&[][..]),
        "case {case:?}: nothing is using a certificate that was never issued, got {detail}"
    );
}

/// RULES 1-3: a never-issued certificate settles terminally, is refused by every
/// TLS-terminating listener it is offered to with the one sentence under the one code, and
/// the issued counterpart is admitted on the identical call and reported in use by the load
/// balancer.
#[tokio::test]
async fn a_never_issued_certificate_is_refused_by_a_tls_terminating_listener() {
    let router = emulator();
    let rows = rows();

    // Every request is made before anything is waited for, so the outcomes the clock owes
    // them fall due together rather than one span per row.
    let mut requested: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        let arn = request_certificate(&router, &row.case, &row.body).await;
        requested.push((row, arn));
    }

    for (index, (row, arn)) in requested.iter().enumerate() {
        let case = row.case.as_str();

        // RULE 1: the status the client's waiter is waiting for really arrives.
        let detail = settled(&router, case, arn).await;
        assert_terminal_failure(row, &detail);

        // RULE 2: the listener that would have to present it is refused, and nothing of the
        // refused call is left behind.
        let lb_arn = load_balancer(&router, case, &format!("refused-cert-{index}")).await;
        assert!(
            listeners_of(&router, case, &lb_arn).await.is_empty(),
            "case {case:?}: a load balancer just created has no listener yet"
        );
        let (status, body) = elbv2(
            &router,
            "CreateListener",
            &create_listener_form(&lb_arn, row.protocol, row.port, arn),
        )
        .await;
        let reported_code = elements(&body, "Code").first().cloned().unwrap_or_default();
        let reported_message = elements(&body, "Message")
            .first()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            (status, reported_code.as_str(), reported_message.as_str()),
            (
                400,
                UNSUPPORTED_CERTIFICATE,
                expected_message(arn).as_str()
            ),
            "case {case:?}: a certificate with no key material behind it must be refused \
             under {UNSUPPORTED_CERTIFICATE} naming {arn:?}, got {status} with {body}"
        );
        assert!(
            listeners_of(&router, case, &lb_arn).await.is_empty(),
            "case {case:?}: a refused CreateListener must leave no listener behind"
        );

        // RULE 2, the certificate side: the refusal changed nothing the owning service holds.
        let after = described(&router, case, arn).await;
        assert_eq!(
            after, detail,
            "case {case:?}: a refused listener must leave the certificate exactly as the \
             owning service held it, got {after}"
        );
    }

    // RULE 3: the control. An issued certificate is admitted on the identical call, is the
    // load balancer's only listener, is the one entry of that listener's list, and is
    // reported in use by the load balancer.
    let control = "an issued certificate offered to the same listener";
    let issued = issued_certificate(&router, control, "admitted.probe-domain.com").await;
    assert!(
        in_use_by(&router, control, &issued).await.is_empty(),
        "case {control:?}: nothing serves the certificate yet"
    );
    let lb_arn = load_balancer(&router, control, "admitted-cert").await;
    let created = elbv2_ok(
        &router,
        control,
        "CreateListener",
        &create_listener_form(&lb_arn, "HTTPS", "443", &issued),
    )
    .await;
    let listener_arn = opt_element(&created, "ListenerArn")
        .unwrap_or_else(|| panic!("case {control:?}: no ListenerArn in {created}"));
    assert_eq!(
        listeners_of(&router, control, &lb_arn).await,
        vec![listener_arn.clone()],
        "case {control:?}: the admitted call must leave exactly one listener"
    );
    assert_eq!(
        listener_certificates(&router, control, &listener_arn).await,
        vec![issued.clone()],
        "case {control:?}: the listener must hold exactly the certificate it was created with"
    );
    assert_eq!(
        in_use_by(&router, control, &issued).await,
        vec![lb_arn.clone()],
        "case {control:?}: the load balancer presenting the certificate is what uses it"
    );

    // And the refused certificates are still what they were: admitting one changed none of
    // them, and none of them became a consumer of anything.
    for (row, arn) in &requested {
        let detail = described(&router, &row.case, arn).await;
        assert_eq!(
            (
                detail["Status"].as_str(),
                detail["InUseBy"].as_array().map(Vec::as_slice)
            ),
            (Some(FAILED), Some(&[][..])),
            "case {:?}: a never-issued certificate stays FAILED and unused, got {detail}",
            row.case
        );
    }
}
