// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ExportClientVpnClientConfiguration`, driven over the wire the
//! service actually speaks (`ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness), with Certificate Manager mounted on the SAME engine, because the server
//! certificate a Client VPN endpoint presents is that service's resource.
//!
//! The rule under test: the operation "downloads the contents of the Client VPN endpoint
//! configuration file for the specified Client VPN endpoint", and that file "includes the
//! Client VPN endpoint and certificate information clients need to establish a connection
//! with the Client VPN endpoint" (`ExportClientVpnClientConfiguration`) — so every value in
//! the file it hands back is COMPUTED FROM THE STORED ENDPOINT the request names: the
//! transport protocol and port it listens on, the DNS name clients dial, the chain of trust
//! ACM holds for the certificate it presents (and nothing at all where ACM holds none), and
//! the credential prompts its authentication options demand. An endpoint id the account
//! holds no endpoint for is refused the one way this surface refuses one, a request carrying
//! no endpoint id at all is refused as the parameter it left out, and `DryRun` reports the
//! permission outcome and exports nothing.
//!
//! Every table row is a distinct input along what that generalises over: which transport
//! protocol and which published port, whether either was stored at all, which authentication
//! kinds and how many, whether the endpoint carries its own DNS name, whether ACM holds
//! material for its certificate, and which form of unresolvable id.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_acm_certificatemanager as acm;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::ops::types::{
    CertificateAuthentication, ClientVpnAuthentication, ClientVpnEndpoint,
    DirectoryServiceAuthentication, FederatedAuthentication,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, CLIENT_VPN_AUTHENTICATION_CERTIFICATE,
    CLIENT_VPN_AUTHENTICATION_DIRECTORY_SERVICE, CLIENT_VPN_AUTHENTICATION_FEDERATED,
};

use super::ec2_query::{call, element, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The ec2Query action under test.
const ACTION: &str = "ExportClientVpnClientConfiguration";

/// The XML element the exported file arrives in
/// (`ExportClientVpnClientConfigurationResult$ClientConfiguration`'s `xmlName`).
const CONFIGURATION_ELEMENT: &str = "clientConfiguration";

/// The directives every exported file carries whatever the endpoint is, as the
/// configuration file published in the AWS Client VPN Administrator Guide carries them.
const FIXED_DIRECTIVES: [&str; 8] = [
    "client",
    "dev tun",
    "remote-random-hostname",
    "resolv-retry infinite",
    "nobind",
    "remote-cert-tls server",
    "cipher AES-256-GCM",
    "verb 3",
];

/// EC2 and Certificate Manager over ONE shared engine, exactly as the assembled router
/// mounts them, plus the engine so a case can read what a call did and did not write.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere.
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(acm::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One `awsJson1_1` call into Certificate Manager, as its status and its body.
async fn acm_call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://acm.{REGION}.amazonaws.com/"))
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/acm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(::bytes::Bytes::from(
            ::serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed certificate request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = ::serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A PEM body as the base64 a blob member carries on the wire.
fn blob(pem: &str) -> Value {
    json!(::aws_smithy_types::base64::encode(pem.as_bytes()))
}

/// One self-signed certificate and the key it belongs to, both as PEM — built by `rcgen`,
/// something other than the code under test.
fn self_signed(common_name: &str, ca: bool) -> (String, String) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key pair");
    let mut params =
        rcgen::CertificateParams::new(vec![common_name.to_string()]).expect("certificate params");
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    // A window open across any instant this suite runs at, so the import is inside the
    // certificate's validity period.
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2100, 1, 1);
    params.is_ca = if ca {
        rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained)
    } else {
        rcgen::IsCa::ExplicitNoCa
    };
    let certificate = params
        .self_signed(&key)
        .expect("a self-signed certificate")
        .pem();
    (certificate, key.serialize_pem())
}

/// Import a real certificate into Certificate Manager through its OWN `ImportCertificate`,
/// answering the ARN that service minted and the chain of trust it was given — so what the
/// export embeds is material the owning service holds rather than material this suite made
/// up.
async fn imported_certificate(router: &MultiServiceRouter, common_name: &str) -> (String, String) {
    let (certificate, private_key) = self_signed(common_name, false);
    let (chain, _) = self_signed(&format!("ca.{common_name}"), true);
    let (status, body) = acm_call(
        router,
        "ImportCertificate",
        &json!({
            "Certificate": blob(&certificate),
            "PrivateKey": blob(&private_key),
            "CertificateChain": blob(&chain),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ImportCertificate must answer 200, got {status} with body {body}"
    );
    let arn = body["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no CertificateArn in {body}"))
        .to_string();
    (arn, chain)
}

/// Request a certificate ACM has not issued yet, answering its ARN: a certificate the
/// account HOLDS, for which that service hands out no material at all.
async fn requested_certificate(router: &MultiServiceRouter, domain: &str) -> String {
    let (status, body) = acm_call(
        router,
        "RequestCertificate",
        &json!({ "DomainName": domain, "ValidationMethod": "DNS" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: RequestCertificate must answer 200, got {status} with body {body}"
    );
    body["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no CertificateArn in {body}"))
        .to_string()
}

/// One `ExportClientVpnClientConfiguration` call carrying `params`.
async fn export(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, ACTION, params).await
}

/// The exported file for `endpoint_id`, requiring the call to be served.
async fn exported(router: &MultiServiceRouter, case: &str, endpoint_id: &str) -> String {
    let (status, body) = export(
        router,
        &[(
            "ClientVpnEndpointId".to_string(),
            endpoint_id.to_string(),
        )],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    super::ec2_query::unescaped(
        &element(&body, CONFIGURATION_ELEMENT)
            .unwrap_or_else(|| panic!("case {case:?}: no <{CONFIGURATION_ELEMENT}> in {body}")),
    )
}

/// One authentication option of the kind `kind`, naming whatever that kind authenticates
/// against.
fn authentication(kind: &str) -> ClientVpnAuthentication {
    let mut option = ClientVpnAuthentication {
        r#type: Some(kind.to_string()),
        ..Default::default()
    };
    if kind == CLIENT_VPN_AUTHENTICATION_CERTIFICATE {
        option.mutual_authentication = Some(CertificateAuthentication {
            client_root_certificate_chain: Some(
                "arn:aws:acm:us-east-1:000000000000:certificate/client-root".to_string(),
            ),
        });
    }
    if kind == CLIENT_VPN_AUTHENTICATION_DIRECTORY_SERVICE {
        option.active_directory = Some(DirectoryServiceAuthentication {
            directory_id: Some("d-926example".to_string()),
        });
    }
    if kind == CLIENT_VPN_AUTHENTICATION_FEDERATED {
        option.federated_authentication = Some(FederatedAuthentication {
            saml_provider_arn: Some(
                "arn:aws:iam::000000000000:saml-provider/ClientVpn".to_string(),
            ),
            self_service_saml_provider_arn: None,
        });
    }
    option
}

/// One seeded endpoint: the members a writer chooses that the exported file is derived from,
/// and what the file must then carry.
struct Row {
    case: &'static str,
    /// The DNS name the endpoint was stored with, or `None` for one the store holds none
    /// for.
    dns_name: Option<&'static str>,
    transport_protocol: Option<&'static str>,
    vpn_port: Option<i32>,
    authentication_kinds: &'static [&'static str],
    /// The `proto` directive the file must carry.
    expected_protocol: &'static str,
    /// The port the `remote` directive must name.
    expected_port: i32,
    /// Whether the file must prompt for a user name and password.
    expects_user_pass: bool,
    /// Whether the file must hand authentication to the identity provider and let the client
    /// ask again interactively.
    expects_federate: bool,
}

/// The endpoints the table below is written against: each published transport protocol and
/// each published port (`CreateClientVpnEndpointRequest$TransportProtocol`, "tcp | udp";
/// `$VpnPort`, "Valid Values: 443 | 1194"), an endpoint stored with neither, every
/// `ClientVpnAuthenticationType` the model declares, two pairs of them together, and an
/// endpoint carrying a DNS name of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "udp on the default port, authenticated by certificate",
            dns_name: None,
            transport_protocol: Some("udp"),
            vpn_port: Some(443),
            authentication_kinds: &[CLIENT_VPN_AUTHENTICATION_CERTIFICATE],
            expected_protocol: "udp",
            expected_port: 443,
            expects_user_pass: false,
            expects_federate: false,
        },
        Row {
            case: "tcp on the alternate port, authenticated by certificate",
            dns_name: None,
            transport_protocol: Some("tcp"),
            vpn_port: Some(1194),
            authentication_kinds: &[CLIENT_VPN_AUTHENTICATION_CERTIFICATE],
            expected_protocol: "tcp",
            expected_port: 1194,
            expects_user_pass: false,
            expects_federate: false,
        },
        Row {
            case: "an endpoint authenticating users against a directory",
            dns_name: None,
            transport_protocol: Some("udp"),
            vpn_port: Some(1194),
            authentication_kinds: &[CLIENT_VPN_AUTHENTICATION_DIRECTORY_SERVICE],
            expected_protocol: "udp",
            expected_port: 1194,
            expects_user_pass: true,
            expects_federate: false,
        },
        Row {
            case: "an endpoint authenticating users against a SAML provider",
            dns_name: None,
            transport_protocol: Some("tcp"),
            vpn_port: Some(443),
            authentication_kinds: &[CLIENT_VPN_AUTHENTICATION_FEDERATED],
            expected_protocol: "tcp",
            expected_port: 443,
            // The provider authenticates the user itself, so the file hands authentication to
            // it rather than prompting for a password.
            expects_user_pass: false,
            expects_federate: true,
        },
        Row {
            case: "an endpoint authenticating by certificate and by SAML provider",
            dns_name: None,
            transport_protocol: Some("udp"),
            vpn_port: Some(443),
            authentication_kinds: &[
                CLIENT_VPN_AUTHENTICATION_CERTIFICATE,
                CLIENT_VPN_AUTHENTICATION_FEDERATED,
            ],
            expected_protocol: "udp",
            expected_port: 443,
            expects_user_pass: false,
            expects_federate: true,
        },
        Row {
            case: "an endpoint authenticating against a directory and a SAML provider",
            dns_name: None,
            transport_protocol: Some("udp"),
            vpn_port: Some(1194),
            authentication_kinds: &[
                CLIENT_VPN_AUTHENTICATION_DIRECTORY_SERVICE,
                CLIENT_VPN_AUTHENTICATION_FEDERATED,
            ],
            expected_protocol: "udp",
            expected_port: 1194,
            expects_user_pass: true,
            expects_federate: true,
        },
        Row {
            // "Default value: udp" (`$TransportProtocol`) and "Default Value: 443"
            // (`$VpnPort`): an endpoint the store holds neither for is reached on those.
            case: "an endpoint stored with neither a transport protocol nor a port",
            dns_name: None,
            transport_protocol: None,
            vpn_port: None,
            authentication_kinds: &[CLIENT_VPN_AUTHENTICATION_CERTIFICATE],
            expected_protocol: "udp",
            expected_port: 443,
            expects_user_pass: false,
            expects_federate: false,
        },
        Row {
            case: "an endpoint carrying a DNS name of its own",
            dns_name: Some("vpn.endpoint.example.test"),
            transport_protocol: Some("tcp"),
            vpn_port: Some(1194),
            authentication_kinds: &[CLIENT_VPN_AUTHENTICATION_DIRECTORY_SERVICE],
            expected_protocol: "tcp",
            expected_port: 1194,
            expects_user_pass: true,
            expects_federate: false,
        },
    ]
}

/// Write one endpoint the way `CreateClientVpnEndpoint` would, and answer the id the store
/// minted for it.
fn seed_endpoint(engine: &engine::InMemoryEngine, row: &Row) -> String {
    let state = engine.state::<Ec2State>();
    let mut store = state.lock();
    let endpoint_id = store
        .insert_client_vpn_endpoint(ClientVpnEndpoint {
            transport_protocol: row.transport_protocol.map(str::to_string),
            vpn_port: row.vpn_port,
            authentication_options: Some(
                row.authentication_kinds
                    .iter()
                    .map(|kind| authentication(kind))
                    .collect(),
            ),
            ..Default::default()
        })
        .unwrap_or_else(|| panic!("case {:?}: the store must take the endpoint", row.case));
    if let Some(dns_name) = row.dns_name {
        store
            .client_vpn_endpoints
            .get_mut(&endpoint_id)
            .expect("the endpoint just stored")
            .dns_name = Some(dns_name.to_string());
    }
    endpoint_id
}

/// The DNS name clients dial an endpoint the store holds no name for by: the form the AWS
/// Client VPN documentation's own names take
/// (`cvpn-endpoint-0123456789abcdef0.prod.clientvpn.us-east-2.amazonaws.com`), written here
/// rather than taken from the code under test.
fn published_dns_name(endpoint_id: &str) -> String {
    format!("{endpoint_id}.prod.clientvpn.{REGION}.amazonaws.com")
}

/// The one line of `configuration` starting with `directive `, or `None`.
fn directive(configuration: &str, directive: &str) -> Option<String> {
    configuration
        .lines()
        .find(|line| line.starts_with(&format!("{directive} ")))
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// The file is derived from the stored endpoint
// ---------------------------------------------------------------------------

/// Every value the exported file carries is read off the endpoint the request names: the
/// transport protocol, the port, the DNS name and the credential prompts its authentication
/// demands — and the fixed directives of the published configuration file are all there.
#[tokio::test]
async fn the_exported_file_describes_the_endpoint_the_request_names() {
    let (router, engine) = emulator();

    for row in rows() {
        let case = row.case;
        let endpoint_id = seed_endpoint(&engine, &row);
        let configuration = exported(&router, case, &endpoint_id).await;

        for fixed in FIXED_DIRECTIVES {
            assert!(
                configuration.lines().any(|line| line == fixed),
                "case {case:?}: expected the file to carry {fixed:?}, got {configuration:?}"
            );
        }
        assert_eq!(
            configuration.lines().next(),
            Some("client"),
            "case {case:?}: expected a client configuration, got {configuration:?}"
        );
        assert_eq!(
            directive(&configuration, "proto"),
            Some(format!("proto {}", row.expected_protocol)),
            "case {case:?}: expected the endpoint's transport protocol, got {configuration:?}"
        );
        let expected_host = row
            .dns_name
            .map(str::to_string)
            .unwrap_or_else(|| published_dns_name(&endpoint_id));
        assert_eq!(
            directive(&configuration, "remote"),
            Some(format!("remote {expected_host} {}", row.expected_port)),
            "case {case:?}: expected the endpoint's own address, got {configuration:?}"
        );
        assert_eq!(
            configuration.lines().any(|line| line == "auth-user-pass"),
            row.expects_user_pass,
            "case {case:?}: expected a user name and password prompt: {}, got {configuration:?}",
            row.expects_user_pass
        );
        assert_eq!(
            configuration.lines().any(|line| line == "auth-federate"),
            row.expects_federate,
            "case {case:?}: expected authentication handed to the provider: {}, got \
             {configuration:?}",
            row.expects_federate
        );
        assert_eq!(
            configuration
                .lines()
                .any(|line| line == "auth-retry interact"),
            row.expects_federate,
            "case {case:?}: expected an interactive retry: {}, got {configuration:?}",
            row.expects_federate
        );
        // The endpoint presents no certificate the account holds material for, so the file
        // carries no certificate material at all rather than material nobody issued.
        assert!(
            !configuration.contains("<ca>") && !configuration.contains("BEGIN CERTIFICATE"),
            "case {case:?}: expected no certificate material, got {configuration:?}"
        );
        assert!(
            configuration.ends_with('\n'),
            "case {case:?}: expected a configuration file whose last directive ends, got \
             {configuration:?}"
        );
    }
}

/// Two endpoints of one account export two different files, and a change to an endpoint's
/// stored attributes is what the next export reports — so the file is computed from the
/// store rather than fixed.
#[tokio::test]
async fn each_endpoint_exports_its_own_file_and_a_later_change_shows_up() {
    let (router, engine) = emulator();
    let table = rows();
    let first = seed_endpoint(&engine, &table[0]);
    let second = seed_endpoint(&engine, &table[1]);

    let one = exported(&router, "the first endpoint", &first).await;
    let other = exported(&router, "the second endpoint", &second).await;
    assert_ne!(
        one, other,
        "two endpoints with different transport protocols and ports must export different \
         files, got {one:?} twice"
    );

    // The port the endpoint listens on is changed in the store; the next export reports it.
    engine
        .state::<Ec2State>()
        .lock()
        .client_vpn_endpoints
        .get_mut(&first)
        .expect("the stored endpoint")
        .vpn_port = Some(1194);
    let after = exported(&router, "after the port changed", &first).await;
    assert_eq!(
        directive(&after, "remote"),
        Some(format!("remote {} 1194", published_dns_name(&first))),
        "case \"after the port changed\": expected the stored port, got {after:?}"
    );
}

// ---------------------------------------------------------------------------
// The certificate information is the material ACM holds
// ---------------------------------------------------------------------------

/// The `<ca>` block is the chain of trust Certificate Manager holds for the server
/// certificate the endpoint presents, and an endpoint whose certificate that service hands
/// out no material for exports a file carrying no certificate material at all.
#[tokio::test]
async fn the_file_carries_the_certificate_material_the_account_holds() {
    let (router, _engine) = emulator();
    let (issued_arn, chain) = imported_certificate(&router, "vpn.example.test").await;
    let pending_arn = requested_certificate(&router, "pending.example.test").await;

    for (case, certificate_arn, expected_chain) in [
        (
            "a server certificate ACM holds a chain of trust for",
            issued_arn,
            Some(chain),
        ),
        (
            "a server certificate ACM has issued no material for",
            pending_arn,
            None,
        ),
    ] {
        let (endpoint_id, dns_name) = create_endpoint(&router, case, &certificate_arn).await;
        let configuration = exported(&router, case, &endpoint_id).await;
        assert_eq!(
            directive(&configuration, "remote"),
            Some(format!("remote {dns_name} 443")),
            "case {case:?}: expected the address the create minted, got {configuration:?}"
        );
        match expected_chain {
            Some(chain) => assert!(
                configuration.contains(&format!("<ca>\n{}\n</ca>", chain.trim())),
                "case {case:?}: expected the chain ACM holds, got {configuration:?}"
            ),
            None => assert!(
                !configuration.contains("<ca>") && !configuration.contains("BEGIN CERTIFICATE"),
                "case {case:?}: expected no certificate material, got {configuration:?}"
            ),
        }
    }
}

/// Create one Client VPN endpoint presenting `certificate_arn` through EC2's own
/// `CreateClientVpnEndpoint`, answering the id and the DNS name it minted.
async fn create_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    certificate_arn: &str,
) -> (String, String) {
    let params = vec![
        ("ClientCidrBlock".to_string(), "10.120.0.0/22".to_string()),
        (
            "ServerCertificateArn".to_string(),
            certificate_arn.to_string(),
        ),
        (
            "Authentication.1.Type".to_string(),
            CLIENT_VPN_AUTHENTICATION_CERTIFICATE.to_string(),
        ),
        (
            "Authentication.1.MutualAuthentication.ClientRootCertificateChainArn".to_string(),
            certificate_arn.to_string(),
        ),
        (
            "ConnectionLogOptions.Enabled".to_string(),
            "false".to_string(),
        ),
    ];
    let (status, body) = call(router, REGION, "CreateClientVpnEndpoint", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must be served, got {status} with body {body}"
    );
    let endpoint_id = element(&body, "clientVpnEndpointId")
        .unwrap_or_else(|| panic!("case {case:?}: no clientVpnEndpointId in {body}"));
    let dns_name =
        element(&body, "dnsName").unwrap_or_else(|| panic!("case {case:?}: no dnsName in {body}"));
    (endpoint_id, dns_name)
}

// ---------------------------------------------------------------------------
// Exporting is a read
// ---------------------------------------------------------------------------

/// Exporting changes nothing a later caller sees: two exports of one unchanged endpoint hand
/// back the same file, and the store holds exactly what it held before.
#[tokio::test]
async fn exporting_changes_nothing_a_later_caller_sees() {
    let (router, engine) = emulator();
    let table = rows();
    let endpoint_id = seed_endpoint(&engine, &table[3]);
    let before = format!(
        "{:?}",
        engine.state::<Ec2State>().lock().client_vpn_endpoints
    );

    let first = exported(&router, "the first export", &endpoint_id).await;
    let second = exported(&router, "the second export", &endpoint_id).await;
    assert_eq!(
        first, second,
        "a read must not change what a later caller sees: {first:?} then {second:?}"
    );
    let after = format!(
        "{:?}",
        engine.state::<Ec2State>().lock().client_vpn_endpoints
    );
    assert_eq!(
        before, after,
        "exporting a configuration must leave the stored endpoints exactly as they were"
    );
}

// ---------------------------------------------------------------------------
// What it refuses
// ---------------------------------------------------------------------------

/// An endpoint id the account holds no endpoint for is refused `Endpoint <id> does not
/// exist`, whatever form the value takes, and a request carrying no endpoint id at all is
/// refused as the parameter it left out.
#[tokio::test]
async fn an_endpoint_the_account_does_not_hold_is_refused() {
    let (router, engine) = emulator();
    let table = rows();
    let stored = seed_endpoint(&engine, &table[0]);

    for (case, unknown) in [
        ("this service's own id form", "cvpn-endpoint-0123456789abcdef0"),
        ("a shorter id of the same form", "cvpn-endpoint-1a2b3c4d"),
        ("an id of another resource kind", "vpc-0f1e2d3c4b5a69780"),
        ("a value that is no id at all", "not-an-endpoint"),
    ] {
        let (status, body) = export(
            &router,
            &[("ClientVpnEndpointId".to_string(), unknown.to_string())],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidClientVpnEndpointId.NotFound"),
            "case {case:?}: expected the one refusal for an unresolvable endpoint, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Endpoint {unknown} does not exist"),
            "case {case:?}: expected the refusal to name the endpoint the request sent"
        );
    }

    let (status, body) = export(&router, &[]).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "MissingParameter"),
        "case \"no endpoint id at all\": expected the absent parameter to be reported, got \
         {status} with body {body}"
    );
    assert_eq!(
        error_message(&body),
        "The request must include the ClientVpnEndpointId parameter. Add the required parameter \
         and retry the request.",
        "case \"no endpoint id at all\": expected the sentence for an absent required parameter"
    );

    // The endpoint the account DOES hold is still exported: the refusals above are about the
    // values the requests carried, not about this operation.
    let configuration = exported(&router, "the stored endpoint", &stored).await;
    assert!(
        configuration.starts_with("client\n"),
        "case \"the stored endpoint\": expected a client configuration, got {configuration:?}"
    );
}

/// `DryRun` reports the permission outcome and exports nothing — not even for an endpoint
/// the account does not hold, which a dry run never gets as far as resolving.
#[tokio::test]
async fn a_dry_run_reports_the_permission_outcome_and_exports_nothing() {
    let (router, engine) = emulator();
    let table = rows();
    let stored = seed_endpoint(&engine, &table[0]);

    for (case, endpoint_id) in [
        ("an endpoint the account holds", stored.as_str()),
        (
            "an endpoint the account does not hold",
            "cvpn-endpoint-0123456789abcdef0",
        ),
    ] {
        let (status, body) = export(
            &router,
            &[
                ("ClientVpnEndpointId".to_string(), endpoint_id.to_string()),
                ("DryRun".to_string(), "true".to_string()),
            ],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (412, "DryRunOperation"),
            "case {case:?}: expected the permitted side of the probe, got {status} with body \
             {body}"
        );
        assert!(
            element(&body, CONFIGURATION_ELEMENT).is_none(),
            "case {case:?}: a dry run must export no configuration, got {body}"
        );
    }
}
