// SPDX-License-Identifier: CC0-1.0

//! Rotating the certificate behind a LIVE HTTPS listener, on the address its load balancer
//! publishes.
//!
//! The rule under test: **the value a load balancer publishes as its `DNSName` is an address
//! of THIS emulator that its listeners answer on, and an HTTPS listener there terminates each
//! handshake with the key material Certificate Manager holds AT THAT MOMENT for the
//! `Certificates[].CertificateArn` the listener names — so new material behind the same name
//! is presented from the next handshake on, the published address does not move, and a
//! connection already open when the material changed is still served.**
//!
//! Grounded in the model: `LoadBalancer$DNSName` is "the public DNS name of the load
//! balancer" — the address a client sends its traffic to —
//! `CreateListenerInput$Certificates` is "[HTTPS and TLS listeners] The default certificate
//! for the listener", `AddListenerCertificates` "Adds the specified SSL server certificate to
//! the certificate list for the specified HTTPS or TLS listener", and Certificate Manager's
//! `ImportCertificateRequest$CertificateArn` names "the [ARN] of an imported certificate to
//! replace" ("Include this argument only when you want to replace a previously imported
//! certificate"), which replaces the material without touching the resource anything names.
//!
//! The rows span what the rule generalises over: the FORM the rotation takes. The material
//! can be replaced under the ARN the listener already names (so nothing of the load
//! balancer changes at all), the listener's default certificate can be pointed at another
//! ARN, or the replacement can land under an ARN that is a certificate-LIST entry, which a
//! client reaches by the name it asks for. Each row asserts the same three things across its
//! rotation: what a handshake before it is presented, that a connection opened before it is
//! still served after it, and what a handshake after it is presented.
//!
//! [`crate::alb_https_listener_tls`] pins the selection among ONE fixed list and
//! [`crate::alb_https_listener_certificate_list_state`] pins that it follows the list the
//! control plane holds; what these rows add is the rotation of the MATERIAL behind a name and
//! that it drops no request. Every certificate is minted by `rcgen` and brought in through
//! Certificate Manager's own `ImportCertificate`, and the leaf a handshake is presented is
//! compared byte for byte with the imported DER, so nothing but the imported material can
//! pass. The clock-owed move to `active` is placed in the store rather than slept for, and
//! the handshake, dial and control-plane helpers are the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use crate::alb_https_listener_tls::{
    add_listener_certificate, answer_on, create_listener, create_load_balancer, emulator,
    handshake, import, import_material, is_reachable_authority, make_active, mint,
    modify_default_certificate, tls_exchange, Minted, SERVED_BODY,
};
use crate::provisioning_loop::{action, call, p};

/// The name the listener's default certificate covers.
const DEFAULT_NAME: &str = "default.rotation.example.com";

/// The name the certificate on the listener's certificate list covers.
const ENTRY_NAME: &str = "entry.rotation.example.net";

/// Which ARN the replacement material lands under, which is the FORM the rotation takes.
enum Rotation {
    /// New material imported under the very ARN the listener names as its default: the
    /// control plane of the load balancer is not touched at all.
    NamedArn,
    /// A second certificate imported under an ARN of its own, which `ModifyListener` then
    /// makes the listener's default.
    ReplacingDefault,
    /// New material imported under the ARN of a certificate-LIST entry, which a client
    /// reaches by the name it asks for.
    ListEntry,
}

/// One row: the case, the load balancer's `Name`, the name the client's `ClientHello` asks
/// for, and how the material behind that name is rotated.
struct Row {
    case: &'static str,
    load_balancer: &'static str,
    asked_for: &'static str,
    rotation: Rotation,
}

/// Every form the rotation takes, against the name each one is observed through.
const ROWS: &[Row] = &[
    Row {
        case: "new material under the ARN the listener names",
        load_balancer: "rotate-same-arn-lb",
        asked_for: DEFAULT_NAME,
        rotation: Rotation::NamedArn,
    },
    Row {
        case: "another certificate made the listener default",
        load_balancer: "rotate-new-default-lb",
        asked_for: DEFAULT_NAME,
        rotation: Rotation::ReplacingDefault,
    },
    Row {
        case: "new material under a certificate-list entry",
        load_balancer: "rotate-list-entry-lb",
        asked_for: ENTRY_NAME,
        rotation: Rotation::ListEntry,
    },
];

/// Replace the material held under `arn` through Certificate Manager's own
/// `ImportCertificate`, which is the operation that takes "the [ARN] of an imported
/// certificate to replace".
async fn reimport(router: &MultiServiceRouter, case: &str, arn: &str, minted: &Minted) {
    let answered = import_material(router, case, Some(arn), minted).await;
    assert_eq!(
        answered, arn,
        "case {case}: a replacement must answer the ARN it replaced, got {answered}"
    );
}

/// Rotate the material a client asking for this row's name is presented, in the form the row
/// names.
async fn rotate(
    router: &MultiServiceRouter,
    row: &Row,
    listener_arn: &str,
    default_arn: &str,
    entry_arn: &str,
    replacement: &Minted,
) {
    match row.rotation {
        Rotation::NamedArn => reimport(router, row.case, default_arn, replacement).await,
        Rotation::ListEntry => reimport(router, row.case, entry_arn, replacement).await,
        Rotation::ReplacingDefault => {
            let arn = import(router, row.case, replacement).await;
            modify_default_certificate(router, listener_arn, &arn).await;
        }
    }
}

/// What a later read reports as the load balancer's own published address.
async fn described_address(router: &MultiServiceRouter, case: &str, lb_arn: &str) -> String {
    let mut form = action("DescribeLoadBalancers");
    form.push(p("LoadBalancerArns.member.1", lb_arn));
    let read = call(router, &form).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected 200 from the read, got {} body {}",
        read.status, read.body
    );
    read.text("DNSName")
        .unwrap_or_else(|| panic!("case {case}: no DNSName in {}", read.body))
}

/// Assert that `answer` is the listener's own answer, out of its own actions.
fn served_by_the_listener(case: &str, state: &str, answer: &str) {
    assert!(
        answer.starts_with("HTTP/1.1 200") && answer.contains(SERVED_BODY),
        "case {case:?} {state}: expected the listener's own answer carrying {SERVED_BODY:?}, \
         got {answer:?}"
    );
}

/// RULE: an HTTPS listener on the address its load balancer published terminates each
/// handshake with the material Certificate Manager holds for the certificate it names at that
/// moment — so a rotation is presented from the next handshake on, whichever form it takes —
/// while the published address stays the one a read reports and a connection opened before
/// the rotation is still served after it.
#[tokio::test]
async fn a_rotated_certificate_is_presented_on_the_published_address_with_no_request_dropped() {
    for row in ROWS {
        let (router, engine) = emulator();

        // The two certificates the listener starts with: its default, and one entry on its
        // certificate list.
        let default_leaf = mint(&[DEFAULT_NAME]);
        let entry_leaf = mint(&[ENTRY_NAME]);
        let default_arn = import(&router, row.case, &default_leaf).await;
        let entry_arn = import(&router, row.case, &entry_leaf).await;

        let (lb_arn, published) = create_load_balancer(&router, row.load_balancer).await;
        assert!(
            is_reachable_authority(&published),
            "case {:?}: expected the load balancer to publish an address a client can dial, \
             got {published:?}",
            row.case
        );
        let listener_arn =
            create_listener(&router, &lb_arn, "HTTPS", "443", Some(&default_arn), None).await;
        add_listener_certificate(&router, &listener_arn, &entry_arn).await;
        make_active(&engine, &lb_arn);

        // The material behind the name this row asks for, before anything rotates it.
        let before: &Minted = match row.rotation {
            Rotation::ListEntry => &entry_leaf,
            Rotation::NamedArn | Rotation::ReplacingDefault => &default_leaf,
        };
        let presented = tls_exchange(row.case, &published, Some(row.asked_for)).await;
        assert_eq!(
            presented.leaf,
            before.der,
            "case {:?} before the rotation: expected the imported leaf ({} bytes), got {} bytes",
            row.case,
            before.der.len(),
            presented.leaf.len()
        );
        served_by_the_listener(row.case, "before the rotation", &presented.answer);

        // A connection that completed its handshake BEFORE the rotation, with its request
        // still to come.
        let (mut in_flight, in_flight_leaf) =
            handshake(row.case, &published, Some(row.asked_for)).await;
        assert_eq!(
            in_flight_leaf, before.der,
            "case {:?}: a handshake before the rotation is presented the material held then, \
             got {} bytes",
            row.case,
            in_flight_leaf.len()
        );

        // The rotation: new material, covering the very name the client asks for.
        let replacement = mint(&[row.asked_for]);
        assert_ne!(
            replacement.der, before.der,
            "case {:?}: the rotation must bring material of its own",
            row.case
        );
        rotate(
            &router,
            row,
            &listener_arn,
            &default_arn,
            &entry_arn,
            &replacement,
        )
        .await;

        // No request dropped: the connection opened before the rotation is served on.
        let answer = answer_on(row.case, &mut in_flight, row.asked_for).await;
        served_by_the_listener(row.case, "on a connection open across the rotation", &answer);

        // From the next handshake on, the rotated material is what the listener presents.
        let presented = tls_exchange(row.case, &published, Some(row.asked_for)).await;
        assert_eq!(
            presented.leaf,
            replacement.der,
            "case {:?} after the rotation: expected the replacing leaf ({} bytes), got {} bytes",
            row.case,
            replacement.der.len(),
            presented.leaf.len()
        );
        served_by_the_listener(row.case, "after the rotation", &presented.answer);

        // A rotation is a change of certificate, not of address: the load balancer still
        // publishes the address it was reached at throughout.
        let reported = described_address(&router, row.case, &lb_arn).await;
        assert_eq!(
            reported, published,
            "case {:?}: expected the published address to stay {published:?}, got {reported:?}",
            row.case
        );
    }
}
