// SPDX-License-Identifier: CC0-1.0

//! Where the ACME surface of an ACM ACME endpoint is PUBLISHED, end to end over the wire.
//!
//! RULE: a server-managed URL this service hands a client names an address that client
//! can reach, and the address this emulator SERVES that resource at. `AcmeEndpoint$EndpointUrl`
//! is the single handle an ACME client (certbot, lego, acme.sh, the python `acme` library)
//! is configured with — it fetches that URL verbatim and follows the resource map the body
//! carries — so the URL is minted on the Region's ACME enrollment host, whose leading two
//! labels are the enrollment label and the Region: in AWS's own DNS zone for a caller that
//! resolves AWS's hostnames, and, for a caller that reached this emulator directly, in the
//! zone a resolver answers with this machine's loopback interface and on the port this
//! emulator serves that surface on (`state::acme_socket`, whose answers carry the field
//! names RFC 8555 spells). The endpoint's own id and the resource are the whole path either
//! way. Every URL the ACME surface then answers with — the resource map inside the
//! directory, an account's own URL — is on the authority the client fetched it from, so the
//! whole account lifecycle is reachable for a client that rewrites no host.
//!
//! The tables span what the rule generalises over: the shape of the endpoint a caller
//! reached (an IPv4 loopback address with and without a port, a loopback NAME, an IPv6
//! literal), a caller that addressed the Region's published control-plane host, and a
//! caller that named no host at all; the Region the endpoint lives in (including a name of
//! three hyphen-separated parts); two endpoints per Region, so one host serves many; and
//! every resource the directory names beneath the published URL. What the published address
//! serves, field name by field name, is [`crate::acme_nonce_field_name`].
//!
//! Every URL fetched here is one a DIFFERENT operation (`DescribeAcmeEndpoint`,
//! `ListAcmeEndpoints`) advertised, fetched with that URL's own host and path and no
//! credential of any kind — exactly what a client holding it does — so an answer that is
//! not computed from the persisted endpoint record cannot pass.

use std::collections::BTreeSet;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::acme_account_registration::{
    external_account_binding, header, jws, post, AccountKey, Identification,
};
use crate::acme_directory_endpoint::{
    activate_endpoints_now, body_json, create_endpoint, endpoint_id, fetch, split_url,
    DIRECTORY_MEMBERS, DNS_SUFFIX, ENROLLMENT_LABEL,
};
use crate::acme_external_account_binding_lifecycle::create_binding;
use crate::certificate_lifecycle::{call, router_with_engine, ACCOUNT, REGION};

/// The DNS zone a resolver answers with this machine's loopback interface, which RFC 6761
/// reserves `localhost` and every name under it for — the zone this emulator may announce a
/// name of its own in when it is reached at an ADDRESS.
///
/// Written out here rather than read from the service, so this test pins the published
/// form: a name in this zone leads back to the interface this emulator's listeners are
/// bound on, and nowhere else.
const LOOPBACK_ZONE: &str = "localhost";

/// The Regions the first table spans: the recorded one and one in another geography, so
/// the published authority is asserted for more than one Region's enrollment host.
const REGIONS: &[&str] = &["us-west-2", "eu-west-1"];

/// One row: what the caller reached, and the `Host` it addressed (`None` for a caller
/// that names no host at all, which is what an in-process caller sends).
const ADDRESSES: &[(&str, Option<&str>)] = &[
    ("the loopback address and the data port", Some("127.0.0.1:9297")),
    ("the loopback address with no port", Some("127.0.0.1")),
    ("a loopback name", Some("localhost:9297")),
    ("an IPv6 loopback literal", Some("[::1]:9297")),
];

/// The caller-chosen text every row's `CreateAcmeEndpoint` carries in each member that
/// takes any (`crate::acme_directory_endpoint::create_endpoint` puts it in the endpoint's
/// tags), so a URL derived from the request could not escape notice.
const MARKER: &str = "client-supplied-marker";

/// Call one operation signed for `region` and addressed to `host`, returning
/// `(status, body)`.
///
/// The `Host` is the one thing this differs from the sibling suites' control-plane call
/// in: it is what a caller that reached this emulator at its own endpoint sends, and
/// what decides the address a server-managed URL is published at.
async fn call_at(
    router: &MultiServiceRouter,
    host: Option<&str>,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{region}/acm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        );
    if let Some(host) = host {
        request = request.header("host", host);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(
                    serde_json::to_vec(body).expect("the request body serializes"),
                ))
                .expect("a valid request"),
        )
        .await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The `EndpointUrl` `DescribeAcmeEndpoint` reports for `arn` to a caller that addressed
/// `host`.
async fn advertised_url(
    router: &MultiServiceRouter,
    case: &str,
    host: Option<&str>,
    region: &str,
    arn: &str,
) -> String {
    let (status, response) = call_at(
        router,
        host,
        region,
        "DescribeAcmeEndpoint",
        &json!({ "AcmeEndpointArn": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAcmeEndpoint must succeed, got {status} {response}"
    );
    response["AcmeEndpoint"]["EndpointUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no EndpointUrl in {response}"))
        .to_string()
}

/// The `EndpointUrl` `ListAcmeEndpoints` reports for `arn` to a caller that addressed
/// `host`.
async fn listed_url(
    router: &MultiServiceRouter,
    case: &str,
    host: Option<&str>,
    region: &str,
    arn: &str,
) -> String {
    let (status, response) = call_at(router, host, region, "ListAcmeEndpoints", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListAcmeEndpoints must succeed, got {status} {response}"
    );
    response["AcmeEndpoints"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no AcmeEndpoints in {response}"))
        .iter()
        .find(|summary| summary["AcmeEndpointArn"].as_str() == Some(arn))
        .and_then(|summary| summary["EndpointUrl"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: {arn} was not listed with a URL: {response}"))
        .to_string()
}

/// The zones a `Host` could name that this service publishes NOTHING in: a request may
/// carry any host, and these are the ones whose answer must therefore be minted on the
/// address this emulator announces instead.
///
/// Each is in the enrollment FORM — the enrollment label and the Region as the leading
/// labels — so each is a host this service claims and answers; what differs is the zone
/// behind them, which no caller gets to put into a URL this service publishes.
const UNPUBLISHED_ZONE_HOSTS: &[(&str, &str)] = &[
    ("another operator's zone", "evil.example.com"),
    ("a zone of one label", "internal"),
    ("a zone that looks like AWS's but is not", "amazonaws.com.attacker.test"),
];

/// RULE: a `Host` SELECTS between the authorities this service publishes and is never
/// itself published. A request addressing this Region's enrollment host in a zone this
/// service publishes nothing in is still served — it is the enrollment form, so it is this
/// service's to answer — but every URL in the answer, and the account URL a registration
/// through it PERSISTS, is minted on the authority this emulator announces, so nothing a
/// caller wrote reaches a value this service publishes or stores.
#[tokio::test]
async fn a_host_this_service_publishes_nothing_at_is_answered_on_the_announced_authority() {
    let (router, engine) = router_with_engine();
    let case = "a client that addressed a host this service does not publish";
    let endpoint_arn = create_endpoint(&router, case, REGION, MARKER).await;
    activate_endpoints_now(&engine);
    let id = endpoint_id(&endpoint_arn).to_string();

    // What the control plane announces to a caller on this machine: the authority every
    // answer below must be minted on, whatever the request addressed.
    let announced = split_url(
        case,
        &advertised_url(&router, case, Some("127.0.0.1:9297"), REGION, &endpoint_arn).await,
    )
    .0;

    for (what, zone) in UNPUBLISHED_ZONE_HOSTS {
        let case = format!("{case}: {what}");
        let addressed = format!("{ENROLLMENT_LABEL}.{REGION}.{zone}");
        let directory = format!("https://{addressed}/{id}/directory");

        let response = fetch(&router, &case, "GET", &directory).await;
        let status = response.status().as_u16();
        let document = body_json(&case, &response);
        assert_eq!(
            status, 200,
            "case {case:?}: the enrollment form is this service's to answer, got {status} \
             {document}"
        );
        for (member, resource) in DIRECTORY_MEMBERS {
            let named = document[*member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no {member} in {document}"));
            assert_eq!(
                named,
                format!("https://{announced}/{id}/{resource}"),
                "case {case:?}: {member} must be minted on the announced authority \
                 ({announced}), never on the host the request wrote"
            );
            assert!(
                !named.contains(zone),
                "case {case:?}: no part of the request's own host may reach a published \
                 URL, got {named:?}"
            );
        }
    }

    // And the account URL a registration through such a host leaves in the STORE: the
    // client follows the resource map it was served, so its JWS names the announced
    // newAccount URL, while the request itself is addressed to the unpublished host.
    let (what, zone) = UNPUBLISHED_ZONE_HOSTS[0];
    let case = format!("a registration through {what}");
    let addressed = format!("{ENROLLMENT_LABEL}.{REGION}.{zone}");
    let new_account = format!("https://{announced}/{id}/new-account");
    let new_nonce = format!("https://{announced}/{id}/new-nonce");

    let binding = create_binding(
        &router,
        &case,
        &json!({
            "AcmeEndpointArn": endpoint_arn,
            "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/AcmeIssuer"),
        }),
    )
    .await;
    let binding_arn = binding["AcmeExternalAccountBindingArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the binding carried no ARN"))
        .to_string();
    let (status, credentials) = call(
        &router,
        "GetAcmeExternalAccountBindingCredentials",
        &json!({ "AcmeExternalAccountBindingArn": binding_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the binding's credentials must be reported, got {status} {credentials}"
    );
    let key_id = credentials["KeyId"].as_str().unwrap_or_default().to_string();
    let mac_key = credentials["MacKey"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let nonce = header(&fetch(&router, &case, "HEAD", &new_nonce).await, "replay-nonce");
    let key = AccountKey::ecdsa("ES256");
    let payload = json!({
        "termsOfServiceAgreed": true,
        "contact": ["mailto:unpublished@example.com"],
        "externalAccountBinding":
            external_account_binding(&key_id, &mac_key, &new_account, &key.jwk()),
    });
    let signed = jws(
        &key,
        &Identification::Jwk,
        &nonce,
        &new_account,
        Some(&payload),
    );
    // Sent to the unpublished host, with the announced newAccount URL in the JWS — which
    // is exactly what a client that followed the served resource map does.
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("/{id}/new-account"))
        .header("host", &addressed)
        .header("content-type", "application/jose+json")
        .body(Bytes::from(signed.to_string()))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let account_url = header(&response, "location");
    assert_eq!(
        status, 201,
        "case {case:?}: the registration must be accepted, got {status} {:?}",
        String::from_utf8_lossy(response.body())
    );
    assert!(
        account_url.starts_with(&format!("https://{announced}/{id}/acct/")),
        "case {case:?}: the account URL must be minted on the announced authority \
         ({announced}), got {account_url:?}"
    );
    assert!(
        !account_url.contains(zone),
        "case {case:?}: the stored account URL must carry nothing the request wrote, got \
         {account_url:?}"
    );

    // The control plane reports the same stored value, so what was persisted is that URL
    // and not the host the registration was addressed to.
    let (status, reported) = call_at(
        &router,
        Some("127.0.0.1:9297"),
        REGION,
        "DescribeAcmeAccount",
        &json!({ "AcmeEndpointArn": endpoint_arn, "AccountUrl": account_url }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAcmeAccount must resolve the account URL, got {status} \
         {reported}"
    );
    assert_eq!(
        reported["AcmeAccount"]["AccountUrl"].as_str(),
        Some(account_url.as_str()),
        "case {case:?}: the stored account URL must be the announced one, got {reported}"
    );
}

/// Assert the directory at `url` is served and names its five resources under the same
/// base, returning the document.
async fn assert_directory_served(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let response = fetch(router, case, "GET", url).await;
    let status = response.status().as_u16();
    let document = body_json(case, &response);
    assert_eq!(
        status, 200,
        "case {case:?}: the advertised URL {url} must serve the ACME directory, got {status} \
         {document}"
    );
    let base = url
        .strip_suffix("/directory")
        .unwrap_or_else(|| panic!("case {case:?}: {url} is not a directory URL"));
    let (authority, _path) = split_url(case, url);
    for (member, resource) in DIRECTORY_MEMBERS {
        let named = document[*member].as_str().unwrap_or_default();
        assert_eq!(
            named,
            format!("{base}/{resource}"),
            "case {case:?}: expected {member} under {base}, got {document}"
        );
        assert_eq!(
            split_url(case, named).0,
            authority,
            "case {case:?}: every resource the directory names must be reachable at the \
             address the client fetched it from, got {named:?}"
        );
    }
    document
}

/// RULE: the advertised `EndpointUrl` is the address this emulator SERVES that endpoint's
/// ACME surface at and the caller can reach — and a plain `GET` of it, exactly as returned,
/// serves the RFC 8555 directory whose resources are on that same address.
#[tokio::test]
async fn the_advertised_endpoint_url_is_on_the_endpoint_the_caller_reached() {
    for region in REGIONS {
        let (router, engine) = router_with_engine();
        let arn = create_endpoint(&router, &format!("{region} endpoint"), region, MARKER).await;
        activate_endpoints_now(&engine);
        let id = endpoint_id(&arn).to_string();
        // The address the store says this emulator serves the ACME surface at, read from
        // the state the create opened it on rather than written here, and the enrollment
        // authority announced for it: the enrollment label and the Region as the leading
        // labels, the loopback zone behind them, and that address's own port.
        let served = served_acme_address(&engine, region);
        let port = served
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("{region}: the served address names no port: {served}"))
            .1
            .to_string();
        let announced = format!("{ENROLLMENT_LABEL}.{region}.{LOOPBACK_ZONE}:{port}");

        for (what, host) in ADDRESSES {
            let case = format!("{region}, a caller that reached {what}");
            let url = advertised_url(&router, &case, *host, region, &arn).await;
            let (authority, path) = split_url(&case, &url);

            // The authority is the Region's enrollment host, label by label, in the zone
            // that leads to this machine — and on the port this emulator serves that
            // surface at, so it is an address this caller reaches, whichever endpoint
            // shape it addressed.
            assert_eq!(
                authority, announced,
                "case {case:?}: expected the enrollment authority announced for the address \
                 this emulator serves the ACME surface at ({announced}), got {url:?}"
            );
            // Everything that identifies the resource is in the PATH, and nothing else is:
            // the endpoint's own id, then the directory resource.
            assert_eq!(
                path,
                format!("/{id}/directory"),
                "case {case:?}: expected the endpoint id and the directory resource as the \
                 whole path, got {url:?}"
            );
            assert!(
                !authority.contains(&id),
                "case {case:?}: the endpoint id must not be a host label, got {authority:?}"
            );
            // Server-managed: nothing the caller sent appears in it.
            for sent in [MARKER, "marker", ACCOUNT] {
                assert!(
                    !url.contains(sent),
                    "case {case:?}: a server-managed URL carries nothing the caller sent, but \
                     {url:?} contains {sent:?}"
                );
            }
            // Identical on every read, and the same URL both readers report.
            assert_eq!(
                advertised_url(&router, &case, *host, region, &arn).await,
                url,
                "case {case:?}: the URL must be identical on every read"
            );
            assert_eq!(
                listed_url(&router, &case, *host, region, &arn).await,
                url,
                "case {case:?}: the listing must advertise the same URL as the read"
            );

            // And it is a URL this service SERVES: the directory, and the fresh nonce the
            // resource map it carries names.
            let document = assert_directory_served(&router, &case, &url).await;
            let nonce_url = document["newNonce"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no newNonce in {document}"))
                .to_string();
            let response = fetch(&router, &case, "HEAD", &nonce_url).await;
            assert_eq!(
                response.status().as_u16(),
                200,
                "case {case:?}: HEAD {nonce_url} must answer 200"
            );
            assert!(
                !header(&response, "replay-nonce").is_empty(),
                "case {case:?}: HEAD {nonce_url} must answer a Replay-Nonce"
            );
        }

        // A caller that resolves AWS's own hostnames is told the Region's published
        // enrollment host, and that address serves the same endpoint's directory too —
        // both addresses name ONE endpoint record.
        for (what, host) in [
            ("the Region's control-plane host", Some(format!("acm.{region}.{DNS_SUFFIX}"))),
            ("no host at all", None),
        ] {
            let case = format!("{region}, a caller that addressed {what}");
            let url = advertised_url(&router, &case, host.as_deref(), region, &arn).await;
            assert_eq!(
                url,
                format!("https://{ENROLLMENT_LABEL}.{region}.{DNS_SUFFIX}/{id}/directory"),
                "case {case:?}: expected the Region's published enrollment host"
            );
            assert_directory_served(&router, &case, &url).await;
        }
    }
}

/// The `host:port` this emulator opened for the ACME surface, read off the service state —
/// the address a published `EndpointUrl` must name, asserted to be one a caller on this
/// machine can reach.
fn served_acme_address(engine: &std::sync::Arc<engine::InMemoryEngine>, region: &str) -> String {
    let store = engine.state::<service_com_amazonaws_acm_certificatemanager::state::ComAmazonawsAcmCertificatemanagerState>();
    let store = store.lock();
    let authority = store
        .acme_socket
        .as_ref()
        .unwrap_or_else(|| panic!("{region}: the create must have opened the ACME address"))
        .authority()
        .to_string();
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("{region}: the served address names no port: {authority}"));
    assert_eq!(
        host, "127.0.0.1",
        "{region}: the served ACME address must be this machine's own, got {authority:?}"
    );
    assert!(
        port.parse::<u16>().is_ok_and(|port| port > 0),
        "{region}: the served ACME address must name a port, got {authority:?}"
    );
    authority
}

/// RULE, to the end of the ACME account lifecycle: a client that is handed the
/// `EndpointUrl` and rewrites no host can register an account through it, and the account
/// URL that registration answers is a URL the control plane resolves — so
/// `DescribeAcmeAccount` and `RevokeAcmeAccount` are reachable for such a client.
#[tokio::test]
async fn an_account_registered_through_the_advertised_url_is_reported_by_the_control_plane() {
    let (router, engine) = router_with_engine();
    let host = Some("127.0.0.1:9297");
    let case = "a client that dials the advertised URL";
    let endpoint_arn = create_endpoint(&router, case, REGION, "registration-marker").await;
    activate_endpoints_now(&engine);

    // Everything the client knows comes from the advertised URL and the document at it.
    let url = advertised_url(&router, case, host, REGION, &endpoint_arn).await;
    let directory = assert_directory_served(&router, case, &url).await;
    let new_account = directory["newAccount"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no newAccount in {directory}"))
        .to_string();
    let new_nonce = directory["newNonce"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no newNonce in {directory}"))
        .to_string();

    // The external account binding this endpoint registers accounts with.
    let binding = create_binding(
        &router,
        case,
        &json!({
            "AcmeEndpointArn": endpoint_arn,
            "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/AcmeIssuer"),
        }),
    )
    .await;
    let binding_arn = binding["AcmeExternalAccountBindingArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the binding carried no ARN"))
        .to_string();
    let (status, credentials) = call(
        &router,
        "GetAcmeExternalAccountBindingCredentials",
        &json!({ "AcmeExternalAccountBindingArn": binding_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the binding's credentials must be reported, got {status} {credentials}"
    );
    let key_id = credentials["KeyId"].as_str().unwrap_or_default().to_string();
    let mac_key = credentials["MacKey"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Register, over the wire, at the URL the directory named.
    let nonce = fetch(&router, case, "HEAD", &new_nonce).await;
    let nonce = header(&nonce, "replay-nonce");
    let key = AccountKey::ecdsa("ES256");
    let payload = json!({
        "termsOfServiceAgreed": true,
        "contact": ["mailto:advertised@example.com"],
        "externalAccountBinding":
            external_account_binding(&key_id, &mac_key, &new_account, &key.jwk()),
    });
    let response = post(
        &router,
        case,
        &new_account,
        &jws(
            &key,
            &Identification::Jwk,
            &nonce,
            &new_account,
            Some(&payload),
        ),
    )
    .await;
    let status = response.status().as_u16();
    let document = body_json(case, &response);
    assert_eq!(
        status, 201,
        "case {case:?}: the registration must be accepted, got {status} {document}"
    );
    let account_url = header(&response, "location");
    assert_eq!(
        split_url(case, &account_url).0,
        split_url(case, &new_account).0,
        "case {case:?}: the account URL must be on the address the client is talking to, got \
         {account_url:?}"
    );

    // The control plane resolves that URL: the account it names is the one that was
    // registered, and revoking it is observable through the same handle.
    for (step, expected) in [("after the registration", "VALID"), ("after the revocation", "REVOKED")]
    {
        if step == "after the revocation" {
            let (status, response) = call_at(
                &router,
                host,
                REGION,
                "RevokeAcmeAccount",
                &json!({ "AcmeEndpointArn": endpoint_arn, "AccountUrl": account_url }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: RevokeAcmeAccount must succeed, got {status} {response}"
            );
        }
        let (status, response) = call_at(
            &router,
            host,
            REGION,
            "DescribeAcmeAccount",
            &json!({ "AcmeEndpointArn": endpoint_arn, "AccountUrl": account_url }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} {step}: DescribeAcmeAccount must resolve the advertised account \
             URL, got {status} {response}"
        );
        assert_eq!(
            response["AcmeAccount"]["AccountUrl"].as_str(),
            Some(account_url.as_str()),
            "case {case:?} {step}: the account must report the URL it was registered at, got \
             {response}"
        );
        assert_eq!(
            response["AcmeAccount"]["Status"].as_str(),
            Some(expected),
            "case {case:?} {step}: expected {expected}, got {response}"
        );
    }
}

/// The Regions the composition table below spans: the recorded one, two other geographies,
/// and one whose name carries three hyphen-separated parts — so the Region travels in the
/// SECOND label for every Region-name shape an ACM ARN may carry (`AcmeEndpointArn`'s
/// `@pattern` spells the Region `[a-z0-9-]+`).
const COMPOSITION_REGIONS: &[&str] = &[
    "us-west-2",
    "eu-central-1",
    "ap-southeast-2",
    "us-gov-west-1",
];

/// Assert `url` is composed as the rule states for `region`, `id` and `resource`, and
/// answer its authority: `https://`, the enrollment label and the Region as the leading two
/// labels of a host of at least three non-empty labels, the id and the resource as the
/// whole path, and no query or fragment anywhere.
fn assert_composition(case: &str, url: &str, region: &str, id: &str, resource: &str) -> String {
    assert!(
        url.starts_with("https://"),
        "case {case:?}: an ACME URL is https (RFC 8555 §6.1), got {url:?}"
    );
    assert!(
        !url.contains('?') && !url.contains('#'),
        "case {case:?}: the published URL carries no query and no fragment, got {url:?}"
    );
    let (authority, path) = split_url(case, url);
    // The port is part of the authority, not of the host name the labels are read off.
    let host = authority
        .rsplit_once(':')
        .map_or(authority.as_str(), |(host, _port)| host);
    let labels: Vec<&str> = host.split('.').collect();
    assert!(
        labels.len() > 2 && labels.iter().all(|label| !label.is_empty()),
        "case {case:?}: expected a host of at least three non-empty labels, got {host:?} from \
         {url:?}"
    );
    assert_eq!(
        &labels[..2],
        &[ENROLLMENT_LABEL, region],
        "case {case:?}: expected {url:?} on {ENROLLMENT_LABEL}.{region}.<zone>, got host \
         {host:?}"
    );
    assert_eq!(
        path,
        format!("/{id}/{resource}"),
        "case {case:?}: expected the endpoint id and {resource} as the whole path, got {url:?}"
    );
    assert!(
        !authority.contains(id),
        "case {case:?}: the endpoint id identifies the endpoint in the path, never as a host \
         label, got {authority:?}"
    );
    for sent in [MARKER, "marker", "cert-marker", ACCOUNT] {
        assert!(
            !url.contains(sent),
            "case {case:?}: a server-managed URL carries nothing the caller sent, but {url:?} \
             contains {sent:?}"
        );
    }
    authority
}

/// RULE: every `EndpointUrl` this service publishes — to a caller that reached this
/// emulator at any shape of its own endpoint, to one that addressed an AWS host, and to one
/// that addressed no host at all — is composed on the Region's ACME enrollment host with
/// the endpoint's own id and the `directory` resource as its whole path; it is the same from
/// both readers and on every read, and unique per endpoint.
#[tokio::test]
async fn every_published_endpoint_url_is_composed_on_the_region_enrollment_host() {
    for region in COMPOSITION_REGIONS {
        let (router, engine) = router_with_engine();
        // Two endpoints per Region, created from identical requests: one host serves many
        // endpoints, so what tells their URLs apart must be the path.
        let mut endpoints = Vec::new();
        for attempt in 0..2 {
            let case = format!("{region} endpoint #{attempt}");
            endpoints.push(create_endpoint(&router, &case, region, MARKER).await);
        }
        activate_endpoints_now(&engine);

        let mut callers: Vec<(String, Option<&str>)> = ADDRESSES
            .iter()
            .map(|(what, host)| (format!("a caller that reached {what}"), *host))
            .collect();
        callers.push(("a caller that addressed no host at all".to_string(), None));

        for (what, host) in &callers {
            // Every endpoint of the Region is published to this caller on ONE authority,
            // and told apart by the id in its path.
            let mut authorities: BTreeSet<String> = BTreeSet::new();
            let mut urls: BTreeSet<String> = BTreeSet::new();

            for arn in &endpoints {
                let case = format!("{region}, {what}");
                let id = endpoint_id(arn).to_string();
                let url = advertised_url(&router, &case, *host, region, arn).await;
                authorities.insert(assert_composition(&case, &url, region, &id, "directory"));

                // The same URL from the listing, and on every later read: a server-managed
                // value is one the service decided once.
                assert_eq!(
                    listed_url(&router, &case, *host, region, arn).await,
                    url,
                    "case {case:?}: the listing must publish the same URL as the read"
                );
                assert_eq!(
                    advertised_url(&router, &case, *host, region, arn).await,
                    url,
                    "case {case:?}: the URL must be identical on every read"
                );
                assert!(
                    urls.insert(url.clone()),
                    "case {case:?}: every endpoint must get its own URL, but {url} was \
                     published twice"
                );
            }

            assert_eq!(
                authorities.len(),
                1,
                "case {region:?}, {what}: one enrollment authority serves every endpoint of \
                 the Region, got {authorities:?}"
            );
            assert_eq!(
                urls.len(),
                endpoints.len(),
                "case {region:?}, {what}: expected one URL per endpoint, got {urls:?}"
            );
        }
    }
}

/// RULE, on the serving side: the URL published to a caller that reached this emulator is
/// one that caller can FETCH as it stands, and the resource map the directory at it carries
/// names every ACME resource on the same authority with the same path layout — which is
/// what lets a client resolve `newNonce`/`newAccount` relative to the directory it fetched.
#[tokio::test]
async fn the_published_url_serves_a_directory_whose_resources_share_its_authority() {
    for region in COMPOSITION_REGIONS {
        let (router, engine) = router_with_engine();
        let arn = create_endpoint(&router, region, region, MARKER).await;
        activate_endpoints_now(&engine);
        let id = endpoint_id(&arn).to_string();

        for (what, host) in ADDRESSES {
            let case = format!("{region}, a caller that reached {what}");
            let url = advertised_url(&router, &case, *host, region, &arn).await;
            let authority = assert_composition(&case, &url, region, &id, "directory");

            // Fetched exactly as a client holding it does: its own host, its own path, and
            // no credential of any kind.
            let response = fetch(&router, &case, "GET", &url).await;
            let status = response.status().as_u16();
            let document = body_json(&case, &response);
            assert_eq!(
                status, 200,
                "case {case:?}: the published URL {url} must serve the ACME directory, got \
                 {status} {document}"
            );
            for (member, resource) in DIRECTORY_MEMBERS {
                let named = document[*member]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no {member} in {document}"))
                    .to_string();
                let on = assert_composition(&case, &named, region, &id, resource);
                assert_eq!(
                    on, authority,
                    "case {case:?}: every resource the directory names must be on the \
                     authority the client fetched it from, got {named:?}"
                );
            }
        }

        // And the caller that resolves AWS's own hostnames is told the host AWS publishes
        // for the Region, which serves the same endpoint's directory too.
        let case = format!("{region}, a caller that resolves AWS's own hostnames");
        let url = advertised_url(&router, &case, None, region, &arn).await;
        assert_eq!(
            url,
            format!("https://{ENROLLMENT_LABEL}.{region}.{DNS_SUFFIX}/{id}/directory"),
            "case {case:?}: expected the Region's published enrollment host"
        );
        assert_directory_served(&router, &case, &url).await;
    }
}
