// SPDX-License-Identifier: CC0-1.0

//! What the ACCESS LOG a load balancer delivers says about the TLS CONNECTION a request
//! arrived over.
//!
//! The rule under test: **a load balancer that records the requests it answered records the
//! facts of the connection each one arrived over — for a request an HTTPS listener served,
//! the protocol version and the cipher suite the handshake negotiated (`ssl_protocol`,
//! `ssl_cipher`, spelled as the security-policy catalogue this service publishes spells
//! them), the name the `ClientHello` asked for (`domain_name`) and the ARN of the
//! certificate presented for that name (`chosen_cert_arn`) — and carries the absent marker
//! in each of them for a request that arrived in the clear, which negotiated none of
//! them.**
//!
//! Grounded in the model and the published catalogue: `LoadBalancerAttribute$Key` names the
//! destination ("`access_logs.s3.enabled` - Indicates whether access logs are enabled…",
//! "`access_logs.s3.bucket` - The name of the S3 bucket for the access logs",
//! "`access_logs.s3.prefix` - The prefix for the location in the S3 bucket for the access
//! logs"); `CreateListenerInput$SslPolicy` is "[HTTPS and TLS listeners] The security policy
//! that defines which protocols and ciphers are supported", and `SslPolicy$SslProtocols` /
//! `Cipher$Name` are the two spellings `DescribeSSLPolicies` publishes, which is where a
//! reader of a record looks the two values up; `AddListenerCertificates` "Adds the specified
//! SSL server certificate to the certificate list for the specified HTTPS or TLS listener",
//! the list the name a client asks for selects the presented certificate from. The record's
//! field order is Elastic Load Balancing's documented access-log format.
//!
//! The rows span what the rule generalises over: the name a client asks for (a name the
//! default certificate covers, a label under a WILDCARD entry of the certificate list, a
//! name no certificate covers, and no name at all), the security policy the listener
//! negotiates under (one whose published protocols stop at TLS 1.2 and one that publishes
//! TLS 1.3), and — as the other side of the rule — a listener in the clear, which records
//! none of it.
//!
//! Nothing is staged: every certificate is minted by `rcgen`, imported through Certificate
//! Manager's own `ImportCertificate`, and named on the listener through
//! `CreateListener`/`AddListenerCertificates`; every request is made by a real `rustls`
//! client over a real connection to the address the load balancer publishes; and every
//! record is read back where a client reads it — out of the bucket, through Amazon S3's own
//! API, decompressed as a reader decompresses it.

use std::io::Read as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::alb_https_listener_tls::{
    add_listener_certificate, client_config, create_listener, create_load_balancer, dial, import,
    make_active, mint, published_port, Minted, EXCHANGE_TIMEOUT, SERVED_BODY,
};
use crate::provisioning_loop::{action, call, p, REGION};

/// The bucket every record below is delivered into. One bucket, a prefix per load balancer,
/// so a row reads the records of the load balancer it made its request through.
const LOG_BUCKET: &str = "elbv2-tls-access-logs";

/// The three attributes that turn access logging on and say where it goes, from
/// `LoadBalancerAttribute$Key`'s own catalog.
const ENABLED_ATTRIBUTE: &str = "access_logs.s3.enabled";
const BUCKET_ATTRIBUTE: &str = "access_logs.s3.bucket";
const PREFIX_ATTRIBUTE: &str = "access_logs.s3.prefix";

/// What an access-log file's name ends with: the log, gzipped — which is what tells a record
/// apart from the file the delivery verified the destination with.
const LOG_FILE_SUFFIX: &str = ".log.gz";

/// What a record's field carries where it does not apply.
const ABSENT: &str = "-";

/// The positions of the fields these rows read, in the documented order Elastic Load
/// Balancing publishes for an HTTP/HTTPS entry (1-based in the documentation, 0-based here).
const TYPE_FIELD: usize = 0;
const TARGET_ADDRESS_FIELD: usize = 4;
const ELB_STATUS_FIELD: usize = 8;
const TARGET_STATUS_FIELD: usize = 9;
const REQUEST_FIELD: usize = 12;
const SSL_CIPHER_FIELD: usize = 14;
const SSL_PROTOCOL_FIELD: usize = 15;
const TARGET_GROUP_FIELD: usize = 16;
const DOMAIN_NAME_FIELD: usize = 18;
const CHOSEN_CERTIFICATE_FIELD: usize = 19;

/// The two `SslProtocols` spellings a listener of this emulator completes a handshake under,
/// as `DescribeSSLPolicies` publishes them.
const TLS_1_2: &str = "TLSv1.2";
const TLS_1_3: &str = "TLSv1.3";

/// The security policy a listener that names none negotiates under, and one whose published
/// protocol list carries TLS 1.3 beside TLS 1.2.
const DEFAULT_POLICY: &str = "ELBSecurityPolicy-2016-08";
const TLS13_POLICY: &str = "ELBSecurityPolicy-TLS13-1-2-2021-06";

/// How long a row waits for the record the delivery owes it, and how long between two looks.
/// Generous on purpose: the delivery happens in this process, so a loaded host does not
/// decide a verdict.
const BUDGET: Duration = Duration::from_secs(30);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// This service, Certificate Manager and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the certificate the listener presents is a resource
/// of Certificate Manager, imported through its own API, and the bucket the record is
/// delivered into is the bucket these rows read.
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// The Amazon S3 client these rows read the delivered records with, built by the one shared
/// constructor so it reads the store the delivery wrote into.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    service_common::s3_client::s3_client(&EmbeddedEmulator::from_router(router.clone()), REGION)
}

/// Create the bucket the access log is delivered into, in the Region every request here is
/// signed for, and write the grant that admits the delivery into it — setup a client does on
/// AWS before it can turn logging on at all, through the crate's one spelling of that policy.
async fn log_bucket(router: &MultiServiceRouter) {
    s3(router)
        .create_bucket()
        .bucket(LOG_BUCKET)
        .create_bucket_configuration(
            aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(aws_sdk_s3::types::BucketLocationConstraint::from(REGION))
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: creating {LOG_BUCKET} must succeed, got {error:?}"));
    crate::access_log_destination::grant_log_delivery(router, LOG_BUCKET).await;
}

/// Turn access logging on for `lb_arn`, into the bucket above under `prefix`.
async fn enable_access_logs(router: &MultiServiceRouter, lb_arn: &str, prefix: &str) {
    let mut form = action("ModifyLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Attributes.member.1.Key", ENABLED_ATTRIBUTE));
    form.push(p("Attributes.member.1.Value", "true"));
    form.push(p("Attributes.member.2.Key", BUCKET_ATTRIBUTE));
    form.push(p("Attributes.member.2.Value", LOG_BUCKET));
    form.push(p("Attributes.member.3.Key", PREFIX_ATTRIBUTE));
    form.push(p("Attributes.member.3.Value", prefix));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup ModifyLoadBalancerAttributes: expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// Every access-log FILE the log bucket holds under `prefix`, by key, with the record each one
/// carries — the gzip stream decompressed, as a reader of the log decompresses it.
///
/// The file the delivery verified the destination with when logging was switched on is no
/// record and carries no log line, so it is not read here; that object is pinned in
/// `all/access_log_destination.rs`.
async fn delivered(router: &MultiServiceRouter, prefix: &str) -> Vec<(String, String)> {
    let listed = s3(router)
        .list_objects_v2()
        .bucket(LOG_BUCKET)
        .prefix(prefix)
        .send()
        .await
        .unwrap_or_else(|error| panic!("listing {LOG_BUCKET} must succeed, got {error:?}"));
    let mut records = Vec::new();
    for object in listed.contents() {
        let key = object.key().unwrap_or_default().to_string();
        if !key.ends_with(LOG_FILE_SUFFIX) {
            continue;
        }
        let fetched = s3(router)
            .get_object()
            .bucket(LOG_BUCKET)
            .key(&key)
            .send()
            .await
            .unwrap_or_else(|error| panic!("fetching {key} must succeed, got {error:?}"));
        let bytes = fetched
            .body
            .collect()
            .await
            .unwrap_or_else(|error| panic!("reading {key} must succeed, got {error:?}"))
            .into_bytes();
        let mut text = String::new();
        flate2::read::GzDecoder::new(bytes.as_ref())
            .read_to_string(&mut text)
            .unwrap_or_else(|error| panic!("{key} must be a gzip stream, got {error}"));
        records.push((key, text.trim_end().to_string()));
    }
    records
}

/// The record delivered for the request this row made: the one object under `prefix` that
/// `seen` does not hold, looked for again until the bucket holds it or [`BUDGET`] passes.
async fn record_of(
    router: &MultiServiceRouter,
    prefix: &str,
    case: &str,
    seen: &mut Vec<String>,
) -> (String, String) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let fresh: Vec<(String, String)> = delivered(router, prefix)
            .await
            .into_iter()
            .filter(|(key, _)| !seen.contains(key))
            .collect();
        match fresh.as_slice() {
            [one] => {
                seen.push(one.0.clone());
                return one.clone();
            }
            [] => assert!(
                Instant::now() < deadline,
                "case {case:?}: the load balancer delivered no access-log record"
            ),
            several => panic!(
                "case {case:?}: expected one new access-log record, got {}: {several:?}",
                several.len()
            ),
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One record line read the way a reader of the log reads it: the fields are
/// space-separated, and a quoted field carries its own spaces.
fn fields(line: &str) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => quoted = !quoted,
            ' ' if !quoted => fields.push(std::mem::take(&mut field)),
            other => field.push(other),
        }
    }
    fields.push(field);
    fields
}

/// One field of a record, or a panic naming the record when it carries no such field.
fn field(case: &str, record: &[String], index: usize) -> String {
    record
        .get(index)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the record carries no field {index}: {record:?}"))
}

/// Dial the address `published`, complete a TLS handshake asking for `server_name` (or
/// asking for no name at all, which is what a client dialling an address does), send one
/// request for `path` and answer with what was served.
async fn tls_request(
    case: &str,
    published: &str,
    server_name: Option<&str>,
    path: &str,
) -> String {
    let conn = dial(published).await;
    let asked_for = match server_name {
        Some(name) => rustls::pki_types::ServerName::try_from(name.to_string())
            .unwrap_or_else(|error| panic!("case {case:?}: {name} is no server name: {error}")),
        None => rustls::pki_types::ServerName::from(std::net::Ipv4Addr::LOCALHOST),
    };
    let connector = tokio_rustls::TlsConnector::from(client_config());
    let mut stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connector.connect(asked_for, conn))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the handshake did not complete in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
    // The host a client of a published address sends is the name it asked for, and the
    // address itself when it asked for none.
    let host = server_name.unwrap_or(published);
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the listener did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).to_string()
}

/// The same request over a connection in the clear, which is the control row.
async fn clear_request(case: &str, published: &str, path: &str) -> String {
    let mut stream = dial(published).await;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {published}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the listener did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).to_string()
}

/// The ciphers `DescribeSSLPolicies` publishes for one predefined policy, as the reply's own
/// text — which is what a reader of a record looks a logged `ssl_cipher` up in.
async fn published_ciphers(router: &MultiServiceRouter, policy: &str) -> String {
    let mut form = action("DescribeSSLPolicies");
    form.push(p("Names.member.1", policy));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup DescribeSSLPolicies({policy}): expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply.body
}

/// The prefix every record of one load balancer lands under, which is the layout Elastic
/// Load Balancing delivers them in.
fn layout_prefix(prefix: &str) -> String {
    format!(
        "{prefix}/AWSLogs/{}/elasticloadbalancing/{REGION}/",
        emulator_server::emulator_account_id()
    )
}

/// One row: the name the client asks for, which certificate the listener must then report as
/// the one it presented, and the protocol its policy negotiates.
struct Row {
    case: &'static str,
    server_name: Option<&'static str>,
    /// The `domain_name` the record must carry: the name asked for, and the absent marker
    /// for a client that asked for none.
    domain_name: &'static str,
    /// Which of the case's certificates the record must name as the chosen one.
    chosen: Chosen,
}

/// Which certificate a row expects to have been presented.
#[derive(Debug, PartialEq, Eq)]
enum Chosen {
    Default,
    Wildcard,
}

/// RULE: the record of a request an HTTPS listener served carries the protocol and cipher
/// the handshake negotiated, the name the client asked for, and the ARN of the certificate
/// presented for that name.
#[tokio::test]
async fn an_https_record_carries_the_negotiated_tls_the_sni_and_the_chosen_certificate() {
    let (router, engine) = emulator();
    log_bucket(&router).await;

    // Two certificates a client imported: the listener's default, and a WILDCARD one added
    // to its certificate list.
    let default_leaf: Minted = mint(&["default.log.example.com"]);
    let wildcard_leaf: Minted = mint(&["*.apps.log.example.org"]);
    let default_arn = import(&router, "the default certificate", &default_leaf).await;
    let wildcard_arn = import(&router, "a wildcard certificate", &wildcard_leaf).await;

    let (lb_arn, published) = create_load_balancer(&router, "tls-log-lb").await;
    let listener_arn = create_listener(
        &router,
        &lb_arn,
        "HTTPS",
        "443",
        Some(&default_arn),
        Some(DEFAULT_POLICY),
    )
    .await;
    add_listener_certificate(&router, &listener_arn, &wildcard_arn).await;
    let prefix = "edge-sni";
    enable_access_logs(&router, &lb_arn, prefix).await;
    make_active(&engine, &lb_arn);

    let catalogue = published_ciphers(&router, DEFAULT_POLICY).await;
    let rows: &[Row] = &[
        Row {
            case: "a name the default certificate covers",
            server_name: Some("default.log.example.com"),
            domain_name: "default.log.example.com",
            chosen: Chosen::Default,
        },
        Row {
            case: "a label under a wildcard entry of the certificate list",
            server_name: Some("api.apps.log.example.org"),
            domain_name: "api.apps.log.example.org",
            chosen: Chosen::Wildcard,
        },
        Row {
            case: "a name no certificate of this listener covers",
            server_name: Some("unknown.log.example.net"),
            domain_name: "unknown.log.example.net",
            chosen: Chosen::Default,
        },
        Row {
            case: "a client that asked for no name at all",
            server_name: None,
            domain_name: ABSENT,
            chosen: Chosen::Default,
        },
    ];
    let mut seen: Vec<String> = Vec::new();
    for row in rows {
        let path = "/served";
        let answered = tls_request(row.case, &published, row.server_name, path).await;
        assert!(
            answered.starts_with("HTTP/1.1 200") && answered.contains(SERVED_BODY),
            "case {:?}: expected the listener's own answer, got {answered:?}",
            row.case
        );
        let (key, line) = record_of(&router, prefix, row.case, &mut seen).await;
        let record = fields(&line);
        let expected_key = layout_prefix(prefix);
        assert!(
            key.starts_with(&expected_key),
            "case {:?}: expected the record under {expected_key}, got {key}",
            row.case
        );
        assert_eq!(
            field(row.case, &record, TYPE_FIELD),
            "https",
            "case {:?}: expected the HTTPS entry type, got {line}",
            row.case
        );
        assert_eq!(
            field(row.case, &record, SSL_PROTOCOL_FIELD),
            TLS_1_2,
            "case {:?}: expected the protocol the policy's published list stops at, got {line}",
            row.case
        );
        let cipher = field(row.case, &record, SSL_CIPHER_FIELD);
        assert_ne!(
            cipher, ABSENT,
            "case {:?}: expected the negotiated cipher, got {line}",
            row.case
        );
        assert!(
            catalogue.contains(&cipher),
            "case {:?}: expected {cipher} to be a cipher {DEFAULT_POLICY} publishes, got \
             {catalogue}",
            row.case
        );
        assert_eq!(
            field(row.case, &record, DOMAIN_NAME_FIELD),
            row.domain_name,
            "case {:?}: expected the name the client asked for, got {line}",
            row.case
        );
        let expected_arn = match row.chosen {
            Chosen::Default => &default_arn,
            Chosen::Wildcard => &wildcard_arn,
        };
        assert_eq!(
            &field(row.case, &record, CHOSEN_CERTIFICATE_FIELD),
            expected_arn,
            "case {:?}: expected the ARN of the presented certificate ({:?}), got {line}",
            row.case,
            row.chosen
        );
        // The rest of the entry is the same request seen from the listener's side: the
        // scheme and port it arrived on, the status the client was answered with, and the
        // absent marker in every target field, because a fixed response involves no target.
        let request_line = field(row.case, &record, REQUEST_FIELD);
        assert!(
            request_line.contains("https://") && request_line.contains(":443/served"),
            "case {:?}: expected the request line of an HTTPS request on the listener's port, \
             got {request_line}",
            row.case
        );
        assert_eq!(
            field(row.case, &record, ELB_STATUS_FIELD),
            "200",
            "case {:?}: expected the status the client was answered with, got {line}",
            row.case
        );
        for (name, index) in [
            ("target:port", TARGET_ADDRESS_FIELD),
            ("target_status_code", TARGET_STATUS_FIELD),
            ("target_group_arn", TARGET_GROUP_FIELD),
        ] {
            assert_eq!(
                field(row.case, &record, index),
                ABSENT,
                "case {:?}: expected {name} absent for a request no target answered, got {line}",
                row.case
            );
        }
    }
}

/// RULE (the security-policy half): the protocol a record reports is the one the handshake
/// really negotiated, so a listener set to a policy that publishes TLS 1.3 records TLS 1.3
/// where one under the default policy records TLS 1.2.
#[tokio::test]
async fn the_recorded_protocol_is_the_one_the_listeners_policy_negotiated() {
    let (router, engine) = emulator();
    log_bucket(&router).await;
    let leaf = mint(&["policy.log.example.com"]);
    let arn = import(&router, "a certificate for a policy row", &leaf).await;

    let (lb_arn, published) = create_load_balancer(&router, "tls13-log-lb").await;
    create_listener(
        &router,
        &lb_arn,
        "HTTPS",
        "443",
        Some(&arn),
        Some(TLS13_POLICY),
    )
    .await;
    let prefix = "edge-policy";
    enable_access_logs(&router, &lb_arn, prefix).await;
    make_active(&engine, &lb_arn);

    let case = "a policy whose published protocol list carries TLS 1.3";
    let answered = tls_request(case, &published, Some("policy.log.example.com"), "/p").await;
    assert!(
        answered.starts_with("HTTP/1.1 200"),
        "case {case:?}: expected the listener's own answer, got {answered:?}"
    );
    let mut seen: Vec<String> = Vec::new();
    let (_key, line) = record_of(&router, prefix, case, &mut seen).await;
    let record = fields(&line);
    assert_eq!(
        field(case, &record, SSL_PROTOCOL_FIELD),
        TLS_1_3,
        "case {case:?}: expected the policy's highest published protocol, got {line}"
    );
    let cipher = field(case, &record, SSL_CIPHER_FIELD);
    let catalogue = published_ciphers(&router, TLS13_POLICY).await;
    assert!(
        catalogue.contains(&cipher),
        "case {case:?}: expected {cipher} to be a cipher {TLS13_POLICY} publishes, got \
         {catalogue}"
    );
    assert_eq!(
        field(case, &record, DOMAIN_NAME_FIELD),
        "policy.log.example.com",
        "case {case:?}: expected the name the client asked for, got {line}"
    );
    assert_eq!(
        &field(case, &record, CHOSEN_CERTIFICATE_FIELD),
        &arn,
        "case {case:?}: expected the ARN of the presented certificate, got {line}"
    );
}

/// RULE (the other side): a request that arrived in the clear negotiated no protocol, no
/// cipher, asked for no name and was presented no certificate, so the record carries the
/// absent marker in each of those fields.
#[tokio::test]
async fn a_record_for_a_request_in_the_clear_carries_no_connection_facts() {
    let (router, engine) = emulator();
    log_bucket(&router).await;
    let (lb_arn, published) = create_load_balancer(&router, "clear-log-lb").await;
    create_listener(&router, &lb_arn, "HTTP", "80", None, None).await;
    let prefix = "edge-clear";
    enable_access_logs(&router, &lb_arn, prefix).await;
    make_active(&engine, &lb_arn);
    // The address is this emulator's own, which is what the published port is taken from.
    assert!(
        published_port(&published) > 0,
        "expected the load balancer to publish a port a client can dial, got {published}"
    );

    let case = "a listener in the clear";
    let answered = clear_request(case, &published, "/clear").await;
    assert!(
        answered.starts_with("HTTP/1.1 200") && answered.contains(SERVED_BODY),
        "case {case:?}: expected the listener's own answer, got {answered:?}"
    );
    let mut seen: Vec<String> = Vec::new();
    let (_key, line) = record_of(&router, prefix, case, &mut seen).await;
    let record = fields(&line);
    assert_eq!(
        field(case, &record, TYPE_FIELD),
        "http",
        "case {case:?}: expected the HTTP entry type, got {line}"
    );
    for (name, index) in [
        ("ssl_cipher", SSL_CIPHER_FIELD),
        ("ssl_protocol", SSL_PROTOCOL_FIELD),
        ("domain_name", DOMAIN_NAME_FIELD),
        ("chosen_cert_arn", CHOSEN_CERTIFICATE_FIELD),
    ] {
        assert_eq!(
            field(case, &record, index),
            ABSENT,
            "case {case:?}: expected {name} absent for a connection in the clear, got {line}"
        );
    }
}
