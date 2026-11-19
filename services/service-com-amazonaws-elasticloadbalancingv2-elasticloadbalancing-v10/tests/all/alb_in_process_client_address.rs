// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a load balancer received a request from, for a request a client INSIDE this
//! emulator opened the connection for.
//!
//! The rule under test: **the address a load balancer publishes is served for a client of
//! this emulator too, and the facts of the connection such a client opens are the load
//! balancer's own account of that request — the address it was opened from is what a
//! `source-ip` condition is evaluated against and what the `client:port` field of the
//! access-log record carries, and a connection this emulator observed no address for carries
//! the absent marker in that field rather than an invented address or port.**
//!
//! Grounded in the model: `LoadBalancer$DNSName` is "the public DNS name of the load
//! balancer" — the address a client is handed and then dials, so it has to be an address
//! something answers on, for every client of this emulator and not only for one outside it;
//! `SourceIpConditionConfig$Values` is "the source IP addresses, in CIDR format … If you
//! specify multiple addresses, the condition is satisfied if the source IP address of the
//! request matches one of the CIDR blocks. This condition is not satisfied by the addresses
//! in the X-Forwarded-For header", so the address judged is the one the CONNECTION came
//! from; `LoadBalancerAttribute$Key` names the access-log destination
//! (`access_logs.s3.enabled`, `access_logs.s3.bucket`, `access_logs.s3.prefix`), and the
//! record's field order and its `client:port` field are Elastic Load Balancing's documented
//! access-log format.
//!
//! The rows differ along what the rule generalises over: the address the connection is
//! opened from (two addresses of different private blocks, a documentation address, and a
//! connection that states none at all), the CIDR the rule is pointed at, and what the client
//! CLAIMS about itself in an `X-Forwarded-For` of its own.
//!
//! Nothing is staged: the load balancer, its listener and its rules are written through the
//! control plane's own operations; each connection is opened to the very address the control
//! plane published, through the register of planes this emulator serves
//! (`service_common::dialable_data_planes`), which is the seam a function, an instance or a
//! task of the emulated network reaches a load balancer over; and every record is read back
//! where a client reads it — out of the bucket, through Amazon S3's own API, decompressed as
//! a reader decompresses it.

use std::io::Read as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::alb_https_listener_tls::{make_active, published_port};
use crate::provisioning_loop::{action, call, p, REGION};

/// The bucket every record below is delivered into, with a prefix per load balancer.
const LOG_BUCKET: &str = "elbv2-client-address-logs";

/// The three attributes that turn access logging on and say where it goes, from
/// `LoadBalancerAttribute$Key`'s own catalog.
const ENABLED_ATTRIBUTE: &str = "access_logs.s3.enabled";
const BUCKET_ATTRIBUTE: &str = "access_logs.s3.bucket";
const PREFIX_ATTRIBUTE: &str = "access_logs.s3.prefix";

/// What an access-log file's name ends with: the log, gzipped — which tells a record apart
/// from the file the delivery verified the destination with.
const LOG_FILE_SUFFIX: &str = ".log.gz";

/// What a record's field carries where the load balancer has no value for it.
const ABSENT: &str = "-";

/// The position of the `client:port` field in the documented order Elastic Load Balancing
/// publishes for an HTTP/HTTPS entry (4th, so index 3 here).
const CLIENT_FIELD: usize = 3;

/// The path both rules below are written for, and the body each answers with: the one the
/// `source-ip` rule serves and the one the fall-through behind it serves.
const MATCHED_PATH: &str = "/from";
const BY_SOURCE_IP: &str = "by-source-ip";
const FELL_THROUGH: &str = "fell-through";

/// How long a case gives one exchange on a connection it opened in this process, and how
/// long it waits for the record the delivery owes it beside how long between two looks.
/// Generous on purpose: every hop is in this process, so a loaded host decides no verdict.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const BUDGET: Duration = Duration::from_secs(30);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// This service and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them: the bucket the records land in is the bucket these rows read.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
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

/// Create the log bucket and write the grant that admits the delivery into it — the setup a
/// client does on AWS before it can turn logging on at all.
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

/// Create an Application Load Balancer and answer its ARN and the address it published.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateLoadBalancer {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("setup CreateLoadBalancer {name}: no ARN in {}", reply.body));
    let published = reply
        .text("DNSName")
        .unwrap_or_else(|| panic!("setup CreateLoadBalancer {name}: no DNSName in {}", reply.body));
    (arn, published)
}

/// Create the HTTP:80 listener whose default action is the fall-through body, and answer its
/// ARN.
async fn create_listener(router: &MultiServiceRouter, lb_arn: &str) -> String {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        "200",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.ContentType",
        "text/plain",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.MessageBody",
        FELL_THROUGH,
    ));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body))
}

/// Add the `source-ip` rule for [`MATCHED_PATH`], pointed at `block`, and answer its ARN.
async fn create_source_ip_rule(
    router: &MultiServiceRouter,
    listener_arn: &str,
    block: &str,
) -> String {
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Priority", "20"));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p(
        "Conditions.member.1.PathPatternConfig.Values.member.1",
        MATCHED_PATH,
    ));
    form.push(p("Conditions.member.2.Field", "source-ip"));
    form.push(p(
        "Conditions.member.2.SourceIpConfig.Values.member.1",
        block,
    ));
    form.push(p("Actions.member.1.Type", "fixed-response"));
    form.push(p("Actions.member.1.FixedResponseConfig.StatusCode", "200"));
    form.push(p(
        "Actions.member.1.FixedResponseConfig.ContentType",
        "text/plain",
    ));
    form.push(p(
        "Actions.member.1.FixedResponseConfig.MessageBody",
        BY_SOURCE_IP,
    ));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateRule: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("RuleArn")
        .unwrap_or_else(|| panic!("setup CreateRule: no RuleArn in {}", reply.body))
}

/// Point the `source-ip` rule's condition at `block`, leaving its path condition and its
/// action as they were.
async fn point_the_rule_at(router: &MultiServiceRouter, rule_arn: &str, block: &str) {
    let mut form = action("ModifyRule");
    form.push(p("RuleArn", rule_arn));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p(
        "Conditions.member.1.PathPatternConfig.Values.member.1",
        MATCHED_PATH,
    ));
    form.push(p("Conditions.member.2.Field", "source-ip"));
    form.push(p(
        "Conditions.member.2.SourceIpConfig.Values.member.1",
        block,
    ));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup ModifyRule({block}): expected 200, got {} body {}",
        reply.status, reply.body
    );
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

/// Send one `GET` for [`MATCHED_PATH`] over a connection opened to `published` — the address
/// the control plane published — stating `from` as the address the connection comes from, and
/// answer what the load balancer served.
///
/// `from` is `None` for a client that states no address of its own, which is what a
/// connection the core accepted on the socket states.
async fn request_from(
    case: &str,
    engine: &Arc<engine::InMemoryEngine>,
    published: &str,
    from: Option<std::net::SocketAddr>,
    forwarded: &[&str],
) -> String {
    let planes = service_common::dialable_data_planes::DialablePlanes::of(&**engine)
        .unwrap_or_else(|| panic!("case {case:?}: the engine must hold the register of planes"));
    let port = published_port(published);
    let dialed = planes
        .dialable("127.0.0.1", port)
        .unwrap_or_else(|| panic!("case {case:?}: nothing serves the published {published}"));
    let mut conn = dialed
        .plane
        .dial(from)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the connection must open, got {error}"));
    let mut request = format!("GET {MATCHED_PATH} HTTP/1.1\r\nHost: {published}\r\n");
    for claimed in forwarded {
        request.push_str(&format!("X-Forwarded-For: {claimed}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    conn.write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, conn.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the load balancer did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).to_string()
}

/// Which of the two rules answered, read off the raw answer: exactly one of the two bodies.
fn served_body(case: &str, answered: &str) -> String {
    assert!(
        answered.starts_with("HTTP/1.1 200"),
        "case {case:?}: expected one of the two rules to answer 200, got {answered:?}"
    );
    let served: Vec<&str> = [BY_SOURCE_IP, FELL_THROUGH]
        .into_iter()
        .filter(|body| answered.contains(body))
        .collect();
    assert_eq!(
        served.len(),
        1,
        "case {case:?}: expected exactly one of the two bodies, got {served:?} in {answered:?}"
    );
    served[0].to_string()
}

/// Every access-log record the bucket holds under `prefix`, by key, with the line each file
/// carries — the gzip stream decompressed, as a reader of the log decompresses it.
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
) -> String {
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
                return one.1.clone();
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

/// One row: where the connection comes from, what the client claims about itself, the CIDR
/// block the rule is pointed at, which body must answer, and what the record's `client:port`
/// field must then carry.
struct Row {
    case: &'static str,
    /// The address the connection states it was opened from, and `None` for a connection
    /// that states none.
    from: Option<&'static str>,
    forwarded: &'static [&'static str],
    block: &'static str,
    body: &'static str,
    client_field: &'static str,
}

/// RULE: the address the connection was opened from is what the load balancer has for the
/// request — the `source-ip` condition is evaluated against it, and the record's
/// `client:port` carries it — while a connection this emulator observed no address for
/// carries the absent marker there.
#[tokio::test]
async fn the_record_and_the_source_ip_condition_carry_the_address_the_connection_came_from() {
    let (router, engine) = emulator();
    log_bucket(&router).await;
    let (lb_arn, published) = create_load_balancer(&router, "client-address-lb").await;
    let listener_arn = create_listener(&router, &lb_arn).await;
    let rule_arn = create_source_ip_rule(&router, &listener_arn, "10.0.0.0/8").await;
    let prefix = "from";
    enable_access_logs(&router, &lb_arn, prefix).await;
    make_active(&engine, &lb_arn);

    // The rows span what the rule generalises over: two addresses of different private
    // blocks and one of the documentation range (RFC 5737 reserves 192.0.2.0/24, so no
    // client of this emulator is really there), a block that holds the connecting address
    // and one that does not, a client claiming another address in an `X-Forwarded-For` of
    // its own and one claiming a chain, and a connection that states no address at all.
    let rows: &[Row] = &[
        Row {
            case: "a block that holds the address the connection came from",
            from: Some("10.0.3.17:51234"),
            forwarded: &[],
            block: "10.0.0.0/8",
            body: BY_SOURCE_IP,
            client_field: "10.0.3.17:51234",
        },
        Row {
            case: "that very address as a /32",
            from: Some("10.0.3.17:40001"),
            forwarded: &[],
            block: "10.0.3.17/32",
            body: BY_SOURCE_IP,
            client_field: "10.0.3.17:40001",
        },
        Row {
            case: "another private block's address, against a block that holds it",
            from: Some("172.16.9.4:33333"),
            forwarded: &[],
            block: "172.16.0.0/12",
            body: BY_SOURCE_IP,
            client_field: "172.16.9.4:33333",
        },
        Row {
            case: "a block that does not hold the address the connection came from",
            from: Some("192.0.2.9:44444"),
            forwarded: &[],
            block: "10.0.0.0/8",
            body: FELL_THROUGH,
            client_field: "192.0.2.9:44444",
        },
        Row {
            case: "the client claims an address inside the block it is not connecting from",
            from: Some("192.0.2.9:44445"),
            forwarded: &["10.0.3.17"],
            block: "10.0.0.0/8",
            body: FELL_THROUGH,
            client_field: "192.0.2.9:44445",
        },
        Row {
            case: "the client claims a whole chain inside that block",
            from: Some("192.0.2.9:44446"),
            forwarded: &["10.0.3.17, 10.0.3.18", "10.0.3.19"],
            block: "10.0.0.0/8",
            body: FELL_THROUGH,
            client_field: "192.0.2.9:44446",
        },
        Row {
            case: "a matching block, with the client claiming an address outside it",
            from: Some("10.0.3.17:40002"),
            forwarded: &["192.0.2.7"],
            block: "10.0.0.0/8",
            body: BY_SOURCE_IP,
            client_field: "10.0.3.17:40002",
        },
        Row {
            case: "a connection that states no address of its own",
            from: None,
            forwarded: &[],
            block: "10.0.0.0/8",
            body: FELL_THROUGH,
            client_field: ABSENT,
        },
    ];

    let mut seen: Vec<String> = Vec::new();
    for row in rows {
        point_the_rule_at(&router, &rule_arn, row.block).await;
        let from: Option<std::net::SocketAddr> = row.from.map(|stated| {
            stated
                .parse()
                .unwrap_or_else(|error| panic!("case {:?}: {stated} is no address: {error}", row.case))
        });
        let answered = request_from(row.case, &engine, &published, from, row.forwarded).await;
        let served = served_body(row.case, &answered);
        assert_eq!(
            served, row.body,
            "case {:?}: expected {} to answer, got {served}",
            row.case, row.body
        );
        let line = record_of(&router, prefix, row.case, &mut seen).await;
        let record = fields(&line);
        let client = record
            .get(CLIENT_FIELD)
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the record carries no client field: {line}", row.case));
        assert_eq!(
            client, row.client_field,
            "case {:?}: expected the client field {}, got {client} in {line}",
            row.case, row.client_field
        );
    }
}

/// RULE (nothing a client writes becomes the address its request came from): a request made
/// against the plane's own endpoint — carrying an `X-Forwarded-For` naming an address the
/// rule's block holds, and the name this emulator holds a connection's address under — is
/// attributed to no client address at all, so the `source-ip` condition does not match and
/// the record carries the absent marker.
#[tokio::test]
async fn no_header_a_client_writes_becomes_the_address_the_request_came_from() {
    let (router, engine) = emulator();
    log_bucket(&router).await;
    let (lb_arn, published) = create_load_balancer(&router, "unforgeable-lb").await;
    let listener_arn = create_listener(&router, &lb_arn).await;
    create_source_ip_rule(&router, &listener_arn, "10.0.0.0/8").await;
    let prefix = "forged";
    enable_access_logs(&router, &lb_arn, prefix).await;
    make_active(&engine, &lb_arn);

    let case = "a client claiming an address of the rule's own block";
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{published}{MATCHED_PATH}"))
        .header("host", &published)
        .header("x-forwarded-for", "10.0.3.17")
        .header("x-emulator-elbv2-client-address", "10.0.3.17:51234")
        .body(bytes::Bytes::new())
        .expect("the case's own request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected the listener to answer 200, got {status} body {body}"
    );
    assert_eq!(
        body, FELL_THROUGH,
        "case {case:?}: expected the claim to satisfy no source-ip condition, got {body}"
    );
    let mut seen: Vec<String> = Vec::new();
    let line = record_of(&router, prefix, case, &mut seen).await;
    let record = fields(&line);
    let client = record
        .get(CLIENT_FIELD)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the record carries no client field: {line}"));
    assert_eq!(
        client, ABSENT,
        "case {case:?}: expected no client address for a request this emulator observed none \
         for, got {client} in {line}"
    );
}

/// RULE (the published address itself): the address the control plane published is one this
/// emulator SERVES for a client of its own, so a connection opened to it reaches that load
/// balancer's listener — and no other address is served by this service.
#[tokio::test]
async fn the_published_address_is_served_for_a_client_inside_the_emulator() {
    let (router, engine) = emulator();
    let (lb_arn, published) = create_load_balancer(&router, "reachable-lb").await;
    create_listener(&router, &lb_arn).await;
    make_active(&engine, &lb_arn);

    let planes = service_common::dialable_data_planes::DialablePlanes::of(&*engine)
        .expect("the engine must hold the register of planes");
    // What this emulator serves at that address is this service, named by the SigV4 signing
    // name both Elastic Load Balancing models publish.
    assert_eq!(
        planes.served_by(&published).as_deref(),
        Some("elasticloadbalancing"),
        "expected the published address {published} to be served by Elastic Load Balancing, \
         got {:?}",
        planes.served_by(&published)
    );
    // The register holds the ONE address this load balancer published and no other.
    assert_eq!(
        planes.len(),
        1,
        "expected exactly the published address to be open, got {planes:?}"
    );

    let answered = request_from(
        "a client inside the emulator",
        &engine,
        &published,
        Some(
            "10.1.2.3:50000"
                .parse()
                .expect("the case's own client address"),
        ),
        &[],
    )
    .await;
    assert!(
        answered.starts_with("HTTP/1.1 200") && answered.contains(FELL_THROUGH),
        "expected the listener's own answer on the published address, got {answered:?}"
    );
    let port = published_port(&published);
    assert!(
        planes.dialable("127.0.0.1", port).is_some(),
        "expected the published port {port} to resolve to a plane of this emulator"
    );
}
