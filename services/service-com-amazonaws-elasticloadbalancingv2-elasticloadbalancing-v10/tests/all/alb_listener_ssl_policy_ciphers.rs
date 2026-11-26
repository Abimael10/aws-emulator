// SPDX-License-Identifier: CC0-1.0

//! Which CIPHER a client negotiates with an HTTPS listener, end to end in one process: a
//! real `rustls` client dials the address the load balancer published offering a cipher
//! list of its own, and the suite reads what the handshake selected — or that there was no
//! handshake at all.
//!
//! The rule under test: **a listener's security policy bounds the cipher a handshake
//! negotiates exactly as it bounds the TLS version — the suite selected is the FIRST cipher
//! of the policy's own published list that the client also offered, whatever order the
//! client prefers them in, and a client offering only ciphers the policy does not publish
//! completes no handshake at all.**
//!
//! Grounded in the model: `CreateListenerInput$SslPolicy` is "[HTTPS and TLS listeners] The
//! security policy that defines which protocols and ciphers are supported", and the policies
//! `DescribeSSLPolicies` publishes carry their ciphers with a `Cipher$Priority` each — "the
//! name of the cipher" and its position in the policy's own priority order. The expectations
//! below are read off that published catalogue (this crate's `DescribeSSLPolicies`, whose
//! names and orders are Elastic Load Balancing's own), never off the handshake's own
//! preferences.
//!
//! The rows span what the rule generalises over: a policy that prefers the AES-128 suite of
//! a pair and one (the RFC 9151 profile) that prefers the AES-256 one, with the client
//! offering the pair in BOTH orders, so only the policy's order can decide; a listener that
//! names no policy, which runs under the default one; a TLS 1.3 policy that publishes the
//! ChaCha20 suite and a FIPS policy of the same shape that does not; and clients whose whole
//! offer the policy excludes, which are refused.
//!
//! Nothing is staged in a store: the certificate is minted by `rcgen` and imported through
//! Certificate Manager's own `ImportCertificate`, the load balancer and its listener are
//! written through this service's own API, and the client is a real TLS client dialling the
//! published address. The clock-owed move to `active` is placed in the store rather than
//! slept for, exactly as the suites beside this one place it.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::alb_https_listener_tls::{
    cipher_catalogue_name, client_config_offering, create_listener, create_load_balancer, dial,
    emulator, import, make_active, mint, EXCHANGE_TIMEOUT,
};

/// The name the certificate covers and the client asks for.
const SERVER_NAME: &str = "ciphers.listener.example.com";

/// The TLS 1.2 ECDHE suites of the pair every policy of the families below publishes both
/// sizes of, as `Cipher$Name` spells them. An ECDSA certificate is what the cases present,
/// so the ECDSA suite of each size is the one a handshake can use.
const AES_128: &str = "ECDHE-ECDSA-AES128-GCM-SHA256";
const AES_256: &str = "ECDHE-ECDSA-AES256-GCM-SHA384";

/// The TLS 1.3 suites, as the catalogue spells them.
const TLS13_AES_128: &str = "TLS_AES_128_GCM_SHA256";
const TLS13_CHACHA20: &str = "TLS_CHACHA20_POLY1305_SHA256";

/// A TLS 1.2 suite no published policy carries: this emulator's TLS implementation can
/// negotiate it, and no security policy of Elastic Load Balancing offers it.
const UNPUBLISHED: &str = "ECDHE-ECDSA-CHACHA20-POLY1305";

/// Dial the published address offering `versions` and `suites`, and answer the cipher the
/// handshake selected — or the handshake's own failure, which is what a client offering
/// nothing the listener's policy publishes is answered with.
async fn negotiated_cipher(
    published: &str,
    versions: &[&'static rustls::SupportedProtocolVersion],
    suites: &[&str],
) -> Result<String, String> {
    let conn = dial(published).await;
    let asked_for = rustls::pki_types::ServerName::try_from(SERVER_NAME.to_string())
        .map_err(|error| format!("{SERVER_NAME} is no server name: {error}"))?;
    let connector = tokio_rustls::TlsConnector::from(client_config_offering(versions, suites));
    let stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connector.connect(asked_for, conn))
        .await
        .map_err(|_| "the handshake did not complete in time".to_string())?
        .map_err(|error| error.to_string())?;
    let suite = stream
        .get_ref()
        .1
        .negotiated_cipher_suite()
        .ok_or_else(|| "the handshake selected no cipher suite".to_string())?;
    cipher_catalogue_name(suite.suite())
        .map(str::to_string)
        .ok_or_else(|| format!("the catalogue spells no name for {:?}", suite.suite()))
}

/// Bring up one Application Load Balancer whose HTTPS listener runs under `ssl_policy` (or
/// under the default policy when the case names none), and answer the address it published.
async fn listener_on(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    certificate_arn: &str,
    ssl_policy: Option<&str>,
) -> String {
    let (lb_arn, published) = create_load_balancer(router, name).await;
    create_listener(
        router,
        &lb_arn,
        "HTTPS",
        "443",
        Some(certificate_arn),
        ssl_policy,
    )
    .await;
    make_active(engine, &lb_arn);
    published
}

/// RULE: the cipher a handshake selects is the first of the listener's security policy's own
/// published list that the client offered — the policy's order, not the client's.
#[tokio::test]
async fn the_policys_own_priority_order_decides_which_offered_cipher_is_selected() {
    let (router, engine) = emulator();
    let leaf = mint(&[SERVER_NAME]);
    let certificate = import(&router, "the listener's certificate", &leaf).await;

    // The policies, each with the two orders a client may offer the pair in, and the suite
    // the policy's own priority order then selects. `ELBSecurityPolicy-FS-1-2-Res-2020-10`
    // publishes the AES-128 suite ahead of the AES-256 one;
    // `ELBSecurityPolicy-TLS13-1-2-RFC9151-FIPS-2023-07` publishes the AES-256 suite and no
    // AES-128 TLS 1.2 suite at all; a listener naming no policy runs under
    // `ELBSecurityPolicy-2016-08`, which leads with AES-128.
    let rows: &[(&str, Option<&str>, &str)] = &[
        (
            "a policy leading with the AES-128 suite",
            Some("ELBSecurityPolicy-FS-1-2-Res-2020-10"),
            AES_128,
        ),
        ("a listener naming no policy", None, AES_128),
        (
            "a policy publishing the AES-256 suite alone",
            Some("ELBSecurityPolicy-TLS13-1-2-RFC9151-FIPS-2023-07"),
            AES_256,
        ),
    ];
    for (index, (case, policy, expected)) in rows.iter().enumerate() {
        let published = listener_on(
            &router,
            &engine,
            &format!("cipher-order-lb-{index}"),
            &certificate,
            *policy,
        )
        .await;
        // Both client orders: what the client prefers must not move the selection.
        for offered in [[AES_128, AES_256], [AES_256, AES_128]] {
            let negotiated =
                negotiated_cipher(&published, &[&rustls::version::TLS12], &offered).await;
            assert_eq!(
                negotiated.as_deref(),
                Ok(*expected),
                "case {case:?} with the client offering {offered:?}: expected {expected:?}, got \
                 {negotiated:?}"
            );
        }
    }
}

/// RULE (the bound itself): a client offering only ciphers the listener's policy does not
/// publish completes no handshake, and one offering a cipher the policy does publish
/// negotiates it.
#[tokio::test]
async fn a_cipher_the_policy_does_not_publish_is_never_negotiated() {
    let (router, engine) = emulator();
    let leaf = mint(&[SERVER_NAME]);
    let certificate = import(&router, "the listener's certificate", &leaf).await;

    // A TLS 1.2 policy of the restricted forward-secrecy family: it publishes the two AEAD
    // ECDHE suites of each size and no ChaCha20 suite.
    let published = listener_on(
        &router,
        &engine,
        "cipher-bound-lb",
        &certificate,
        Some("ELBSecurityPolicy-FS-1-2-Res-2020-10"),
    )
    .await;
    let case = "a client offering only a suite no policy publishes";
    let refused = negotiated_cipher(&published, &[&rustls::version::TLS12], &[UNPUBLISHED]).await;
    assert!(
        refused.is_err(),
        "case {case:?}: expected no handshake, got {refused:?}"
    );
    let case = "the same client offering a suite the policy does publish";
    let negotiated = negotiated_cipher(
        &published,
        &[&rustls::version::TLS12],
        &[UNPUBLISHED, AES_256],
    )
    .await;
    assert_eq!(
        negotiated.as_deref(),
        Ok(AES_256),
        "case {case:?}: expected {AES_256:?}, got {negotiated:?}"
    );
}

/// RULE (the TLS 1.3 half): the same bound holds for the TLS 1.3 suites — a policy that
/// publishes the ChaCha20 suite negotiates it, and the FIPS policy of the same shape, which
/// publishes it not, refuses the client that offers it alone.
#[tokio::test]
async fn a_tls_13_policy_negotiates_only_the_tls_13_suites_it_publishes() {
    let (router, engine) = emulator();
    let leaf = mint(&[SERVER_NAME]);
    let certificate = import(&router, "the listener's certificate", &leaf).await;

    let publishes = listener_on(
        &router,
        &engine,
        "tls13-ciphers-lb",
        &certificate,
        Some("ELBSecurityPolicy-TLS13-1-3-2021-06"),
    )
    .await;
    let case = "a TLS 1.3 policy that publishes the ChaCha20 suite";
    let negotiated =
        negotiated_cipher(&publishes, &[&rustls::version::TLS13], &[TLS13_CHACHA20]).await;
    assert_eq!(
        negotiated.as_deref(),
        Ok(TLS13_CHACHA20),
        "case {case:?}: expected {TLS13_CHACHA20:?}, got {negotiated:?}"
    );

    let fips = listener_on(
        &router,
        &engine,
        "tls13-fips-ciphers-lb",
        &certificate,
        Some("ELBSecurityPolicy-TLS13-1-3-FIPS-2023-04"),
    )
    .await;
    let case = "the FIPS policy of the same shape, which publishes it not";
    let refused = negotiated_cipher(&fips, &[&rustls::version::TLS13], &[TLS13_CHACHA20]).await;
    assert!(
        refused.is_err(),
        "case {case:?}: expected no handshake, got {refused:?}"
    );
    let case = "a client of that FIPS policy offering a suite it does publish";
    let negotiated = negotiated_cipher(
        &fips,
        &[&rustls::version::TLS13],
        &[TLS13_CHACHA20, TLS13_AES_128],
    )
    .await;
    assert_eq!(
        negotiated.as_deref(),
        Ok(TLS13_AES_128),
        "case {case:?}: expected {TLS13_AES_128:?}, got {negotiated:?}"
    );
}
