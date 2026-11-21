// SPDX-License-Identifier: CC0-1.0

//! What CloudFront decides about a VPC origin request's ARGUMENTS, before it resolves or
//! stores anything, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test:
//!
//! 1. an operation ADDRESSED by a VPC origin identifier in its path reads the FORM of that
//!    value before it resolves it: only the VPC origin namespace label followed by a body of
//!    the width and alphabet this service issues one at is an identifier the service reads, so
//!    a value of that form the store holds nothing under is the VPC origin that is ABSENT
//!    (`EntityNotFound`, 404) while every other value is the argument the service cannot read
//!    (`InvalidArgument`, 400, "<member> format is invalid" naming the member it arrived in);
//! 2. `CreateVpcOrigin` validates the endpoint configuration's `Arn` before it mints
//!    anything: a value outside the FORM that member takes — no ARN at all, or an ARN of a
//!    service that owns no resource a VPC origin fronts — is refused on the member's form ("The
//!    parameter <member path> Invalid Format."), one inside that form whose resource part names
//!    no resource a VPC origin can front fails that member's validation ("The parameter <member
//!    path> ValidationError."), and an ARN naming one of the resources a VPC origin fronts is
//!    accepted and read back;
//! 3. a port no origin can be reached on is refused with the one sentence this service states
//!    for that condition, wherever an origin configuration carries a port — the VPC origin
//!    endpoint configuration and a distribution's custom origin alike;
//! 4. every one of those refusals mints nothing: the VPC origin created before them is still
//!    the only one the service answers for.
//!
//! The wire harness (the router, the request helpers and the element scanner) is the one
//! [`crate::distribution_lifecycle`] already builds, and the VPC origin writer is
//! [`crate::vpc_origin_lifecycle`]'s.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    id_format_invalid_message, parameter_invalid_format_message, parameter_validation_error_message,
    DEFAULT_ORIGIN_PORTS, MAX_ORIGIN_PORT, MIN_UNPRIVILEGED_ORIGIN_PORT,
    ORIGIN_PORT_OUT_OF_RANGE_MESSAGE, VPC_ORIGIN_ENDPOINT_CONFIG_ARN_MEMBER_PATH,
    VPC_ORIGIN_ID_PREFIX, VPC_ORIGIN_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, Answer, ACCOUNT_ID, API_VERSION,
};
use crate::distributions_by_web_acl_id::encode_label;
use crate::key_value_store_lifecycle::escape;
use crate::resource_policy_lifecycle::message;
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};

// ---------------------------------------------------------------------------
// Calling the surface
// ---------------------------------------------------------------------------

/// `GET /2020-05-31/vpc-origin/{Id}`.
async fn get_vpc_origin(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/vpc-origin/{id}"),
        None,
        None,
    )
    .await
}

/// `GET /2020-05-31/distributionsByVpcOriginId/{VpcOriginId}`.
async fn list_by_vpc_origin(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/distributionsByVpcOriginId/{id}"),
        None,
        None,
    )
    .await
}

/// The `CreateVpcOriginRequest` document for one endpoint configuration.
fn create_body(name: &str, arn: &str, http_port: i32, https_port: i32) -> String {
    format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{}</Name><Arn>{}</Arn>\
         <HTTPPort>{http_port}</HTTPPort><HTTPSPort>{https_port}</HTTPSPort>\
         <OriginProtocolPolicy>http-only</OriginProtocolPolicy>\
         </VpcOriginEndpointConfig></CreateVpcOriginRequest>",
        escape(name),
        escape(arn)
    )
}

/// A `DistributionConfig` whose one origin is a CUSTOM origin reached on `http_port` and
/// `https_port` — the other member of this service that carries an origin's ports.
fn distribution_config_with_ports(reference: &str, http_port: i32, https_port: i32) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin><Id>primary</Id>\
         <DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>{http_port}</HTTPPort>\
         <HTTPSPort>{https_port}</HTTPSPort>\
         <OriginProtocolPolicy>http-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>{reference}</Comment><Enabled>true</Enabled></DistributionConfig>"
    )
}

/// A VPC origin created over an endpoint ARN this service accepts, and the `(Id, ETag)` the
/// create handed back — the resource every "nothing was minted" check reads afterwards.
async fn created_vpc_origin(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = create_vpc_origin(
        router,
        &create_body(name, &load_balancer_arn(name), 80, 443),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {name}: expected HTTP 202 from a create this service accepts, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    (id, created.header("etag").to_string())
}

// ---------------------------------------------------------------------------
// The identifier forms
// ---------------------------------------------------------------------------

/// The two operations addressed by a VPC origin identifier in their path, each with the member
/// that identifier arrives in — read off the model: `GetVpcOrigin` takes `Id` on
/// `GET /2020-05-31/vpc-origin/{Id}` and `ListDistributionsByVpcOriginId` takes `VpcOriginId`
/// on `GET /2020-05-31/distributionsByVpcOriginId/{VpcOriginId}`
/// (`smithy_query(query="operation", operation="GetVpcOrigin,ListDistributionsByVpcOriginId")`).
const ADDRESSED_BY_AN_ID: [(&str, &str); 2] =
    [("GetVpcOrigin", "Id"), ("ListDistributionsByVpcOriginId", "VpcOriginId")];

/// Send `identifier` to the operation `name` names.
async fn addressed_call(router: &MultiServiceRouter, operation: &str, identifier: &str) -> Answer {
    match operation {
        "GetVpcOrigin" => get_vpc_origin(router, identifier).await,
        _ => list_by_vpc_origin(router, identifier).await,
    }
}

/// RULE 1, the refusal side: a value of no form a VPC origin identifier takes is the argument
/// the operation reports it cannot read — 400 `InvalidArgument` with the sentence naming the
/// member the value arrived in — on every operation addressed by one, and whatever the store
/// holds.
///
/// The rows differ along WHAT is wrong with the value: the namespace label (absent, upper
/// cased, another kind's), the body's width (short, long, two hundred characters, empty), its
/// alphabet (lower case, separators) and the kind of value submitted instead (a bare word, a
/// hyphenated token, an opaque identifier of the width CloudFront writes another kind's body
/// at, the VPC origin's own ARN) — the forms a caller reaches these routes with.
#[tokio::test]
async fn an_id_of_no_vpc_origin_form_is_an_invalid_argument() {
    let router = emulator();
    let (held, etag) = created_vpc_origin(&router, "vo-form-survivor").await;
    let held_arn = get_vpc_origin(&router, &held)
        .await
        .root()
        .find_text("Arn")
        .unwrap_or_default()
        .to_string();

    let rows: [(&str, String); 14] = [
        ("a bare word", "absent".to_string()),
        (
            "a hyphenated token outside the namespace",
            "not-a-vpc-origin-id".to_string(),
        ),
        ("no namespace label at all", "ABSENT0000000".to_string()),
        ("the namespace label upper cased", format!("VO_{}", "A".repeat(13))),
        ("another kind's namespace label", "dt_ABSENT0000000".to_string()),
        (
            "a body one character short",
            format!("{VPC_ORIGIN_ID_PREFIX}{}", "A".repeat(12)),
        ),
        (
            "a body one character long",
            format!("{VPC_ORIGIN_ID_PREFIX}{}", "A".repeat(14)),
        ),
        (
            "a body of two hundred characters",
            format!("{VPC_ORIGIN_ID_PREFIX}{}", "0".repeat(200)),
        ),
        ("an empty body", VPC_ORIGIN_ID_PREFIX.to_string()),
        (
            "an opaque body of another kind's width",
            format!("{VPC_ORIGIN_ID_PREFIX}2uVbA9KeWaADTbKzhj9lcKDoM25"),
        ),
        (
            "a lower-cased body",
            format!("{VPC_ORIGIN_ID_PREFIX}absent0000000"),
        ),
        (
            "a body carrying separators",
            format!("{VPC_ORIGIN_ID_PREFIX}ABSENT-000_000"),
        ),
        ("the held id in lower case", held.to_lowercase()),
        ("the VPC origin's own ARN", encode_label(&held_arn)),
    ];

    for (operation, member) in ADDRESSED_BY_AN_ID {
        let expected = id_format_invalid_message(member);
        for (case, identifier) in &rows {
            let answer = addressed_call(&router, operation, identifier).await;
            assert_eq!(
                answer.status, 400,
                "case {case}/{operation} ({identifier:?}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {case}/{operation} ({identifier:?}): expected InvalidArgument, got {:?} \
                 ({})",
                answer.error_code(),
                answer.raw
            );
            assert_eq!(
                message(&answer),
                expected,
                "case {case}/{operation} ({identifier:?}): expected {expected:?}, got {}",
                answer.raw
            );
        }
    }

    // Every refusal left the store as it was: the VPC origin held before them still reads back
    // under its own version token.
    let read = get_vpc_origin(&router, &held).await;
    assert_eq!(
        read.status, 200,
        "case after the refusals: expected HTTP 200, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "case after the refusals: expected the create's version token, got {:?}",
        read.header("etag")
    );
}

/// RULE 1, the absent side: an identifier OF the form this service issues that the store holds
/// nothing under is the VPC origin that is absent — 404 `EntityNotFound` with the one sentence
/// this service states for that condition — on every operation addressed by one.
///
/// The rows differ along the body: letters, digits, a mixture, and a held id with one character
/// changed.
#[tokio::test]
async fn a_readable_id_the_store_does_not_hold_is_entity_not_found() {
    let router = emulator();
    let (held, _) = created_vpc_origin(&router, "vo-absent-neighbour").await;
    let mut flipped: Vec<char> = held.chars().collect();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == 'A' { 'B' } else { 'A' };

    let rows: [(&str, String); 4] = [
        (
            "a body of letters",
            format!("{VPC_ORIGIN_ID_PREFIX}ABSENTABSENTX"),
        ),
        (
            "a body of digits",
            format!("{VPC_ORIGIN_ID_PREFIX}0123456789012"),
        ),
        (
            "a mixed body",
            format!("{VPC_ORIGIN_ID_PREFIX}A1B2C3D4E5F6G"),
        ),
        (
            "a held id with its last character changed",
            flipped.into_iter().collect(),
        ),
    ];

    for (operation, _) in ADDRESSED_BY_AN_ID {
        for (case, identifier) in &rows {
            let answer = addressed_call(&router, operation, identifier).await;
            assert_eq!(
                answer.status, 404,
                "case {case}/{operation} ({identifier:?}): expected HTTP 404, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "EntityNotFound",
                "case {case}/{operation} ({identifier:?}): expected EntityNotFound, got {:?} \
                 ({})",
                answer.error_code(),
                answer.raw
            );
            assert_eq!(
                message(&answer),
                VPC_ORIGIN_NOT_FOUND_MESSAGE,
                "case {case}/{operation} ({identifier:?}): expected \
                 {VPC_ORIGIN_NOT_FOUND_MESSAGE:?}, got {}",
                answer.raw
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The endpoint ARN
// ---------------------------------------------------------------------------

/// RULE 2, the accepted side: an `Arn` naming one of the resources a VPC origin fronts — an
/// Application Load Balancer, a Network Load Balancer or an EC2 instance — is accepted, and the
/// VPC origin created from it reads back.
///
/// The rows differ along the kind of resource named, the load balancer type code, the width of
/// the EC2 instance id (both forms EC2 issues), the partition, the region and the length of the
/// load balancer name at its published bound.
#[tokio::test]
async fn an_endpoint_arn_naming_a_resource_a_vpc_origin_fronts_is_accepted() {
    let router = emulator();

    let rows: [(&str, String); 6] = [
        (
            "an application load balancer",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/app/orders/\
                 0123456789abcdef"
            ),
        ),
        (
            "a network load balancer",
            format!(
                "arn:aws:elasticloadbalancing:eu-west-1:{ACCOUNT_ID}:loadbalancer/net/edge/\
                 fedcba9876543210"
            ),
        ),
        (
            "a load balancer name at the published bound",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/app/{}/\
                 0123456789abcdef",
                "n".repeat(32)
            ),
        ),
        (
            "an EC2 instance of the long id form",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
        ),
        (
            "an EC2 instance of the short id form",
            format!("arn:aws:ec2:us-west-2:{ACCOUNT_ID}:instance/i-01234567"),
        ),
        (
            "another partition",
            format!(
                "arn:aws-cn:elasticloadbalancing:cn-north-1:{ACCOUNT_ID}:loadbalancer/app/beijing/\
                 0123456789abcdef"
            ),
        ),
    ];

    for (index, (case, arn)) in rows.iter().enumerate() {
        let name = format!("accepted-{index}");
        let created = create_vpc_origin(&router, &create_body(&name, arn, 80, 443)).await;
        assert_eq!(
            created.status, 202,
            "case {case} ({arn:?}): expected HTTP 202, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().find_text("Id").unwrap_or_default().to_string();

        // Read back through the implemented reader: the stored record answers with the very ARN
        // the create submitted.
        let read = get_vpc_origin(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 reading {id:?} back, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.root()
                .at("VpcOriginEndpointConfig")
                .and_then(|config| config.find_text("Arn"))
                .unwrap_or_default(),
            arn.as_str(),
            "case {case}: expected the submitted endpoint ARN, got {}",
            read.raw
        );
    }
}

/// RULE 2, the refusal side: a value this service's argument validation does not accept is
/// refused 400 `InvalidArgument`, naming the member by its PATH in the request, and mints
/// nothing — with the sentence for a value outside the FORM the member takes (no ARN at all, or
/// an ARN of a service that owns no resource a VPC origin fronts) and the sentence for a value
/// inside that form whose resource part names no resource a VPC origin can front.
///
/// The rows differ along which stage the value fails at and why: a value that is no ARN (empty,
/// a word, a URL, too few segments, an empty partition or resource) or an ARN naming another
/// service's resource altogether, and an ARN of a service that owns one naming another resource
/// type, a load balancer kind CloudFront does not front, a load balancer name Elastic Load
/// Balancing could not have issued (past its published bound, carrying a character outside its
/// alphabet, beginning with the label it reserves), a resource id of neither service's form, or
/// no region or account at all.
#[tokio::test]
async fn an_endpoint_arn_the_validation_rejects_mints_nothing() {
    let router = emulator();
    let (held, etag) = created_vpc_origin(&router, "vo-arn-survivor").await;

    let unreadable = parameter_invalid_format_message(VPC_ORIGIN_ENDPOINT_CONFIG_ARN_MEMBER_PATH);
    let unvalidated =
        parameter_validation_error_message(VPC_ORIGIN_ENDPOINT_CONFIG_ARN_MEMBER_PATH);

    let rows: [(&str, String, &str); 15] = [
        ("the empty string", String::new(), unreadable.as_str()),
        ("a bare word", "not-an-arn".to_string(), unreadable.as_str()),
        (
            "a URL rather than an ARN",
            "https://orders.internal.example.com".to_string(),
            unreadable.as_str(),
        ),
        (
            "an ARN missing its resource segment",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}"),
            unreadable.as_str(),
        ),
        (
            "an ARN with an empty resource part",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:"),
            unreadable.as_str(),
        ),
        (
            "an ARN with no partition",
            format!("arn::ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
            unreadable.as_str(),
        ),
        (
            "another service's resource",
            "arn:aws:s3:::orders-bucket".to_string(),
            unreadable.as_str(),
        ),
        (
            "another resource type of a service that owns one",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:vpc/vpc-0123456789abcdef0"),
            unvalidated.as_str(),
        ),
        (
            "a load balancer kind no VPC origin fronts",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/gwy/gateway/\
                 0123456789abcdef"
            ),
            unvalidated.as_str(),
        ),
        (
            "a load balancer name past its published bound",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/app/{}/\
                 0123456789abcdef",
                "n".repeat(33)
            ),
            unvalidated.as_str(),
        ),
        (
            "a load balancer name outside its alphabet",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/app/orders.alb/\
                 0123456789abcdef"
            ),
            unvalidated.as_str(),
        ),
        (
            "a load balancer name under the reserved label",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/app/\
                 internal-orders/0123456789abcdef"
            ),
            unvalidated.as_str(),
        ),
        (
            "a load balancer id of no form that service issues",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/app/orders/xyz"
            ),
            unvalidated.as_str(),
        ),
        (
            "an instance id of no form EC2 issues",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-not-hexadecimal"),
            unvalidated.as_str(),
        ),
        (
            "an ARN naming no region or account",
            "arn:aws:ec2:::instance/i-0123456789abcdef0".to_string(),
            unvalidated.as_str(),
        ),
    ];

    for (case, arn, expected) in &rows {
        let refused = create_vpc_origin(&router, &create_body("arn-rejected", arn, 80, 443)).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({arn:?}): expected HTTP 400, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case} ({arn:?}): expected InvalidArgument, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            message(&refused),
            *expected,
            "case {case} ({arn:?}): expected {expected:?}, got {}",
            refused.raw
        );
        assert_eq!(
            refused.header("location"),
            "",
            "case {case} ({arn:?}): a refusal announces no resource, got {:?}",
            refused.header("location")
        );
    }

    // Nothing was minted: the VPC origin held before the refusals still reads back under its
    // own version token.
    let read = get_vpc_origin(&router, &held).await;
    assert_eq!(
        read.status, 200,
        "case after the refusals: expected HTTP 200, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "case after the refusals: expected the create's version token, got {:?}",
        read.header("etag")
    );
}

// ---------------------------------------------------------------------------
// The origin ports
// ---------------------------------------------------------------------------

/// Every port an origin configuration may be reached on that this suite asserts over: the two
/// documented defaults and each published bound of the unprivileged range
/// ([`DEFAULT_ORIGIN_PORTS`], [`MIN_UNPRIVILEGED_ORIGIN_PORT`], [`MAX_ORIGIN_PORT`]).
fn accepted_ports() -> Vec<(String, i32)> {
    let mut rows: Vec<(String, i32)> = DEFAULT_ORIGIN_PORTS
        .iter()
        .map(|port| (format!("the documented default {port}"), *port))
        .collect();
    rows.push((
        "the lowest unprivileged port".to_string(),
        MIN_UNPRIVILEGED_ORIGIN_PORT,
    ));
    rows.push((
        "one above the lowest unprivileged port".to_string(),
        MIN_UNPRIVILEGED_ORIGIN_PORT + 1,
    ));
    rows.push(("the highest port".to_string(), MAX_ORIGIN_PORT));
    rows
}

/// Every port no origin can be reached on that this suite asserts over: one past each published
/// bound, the privileged ports around the two defaults, and the values no port can take at all.
fn refused_ports() -> Vec<(String, i32)> {
    vec![
        ("one below the lowest unprivileged port".to_string(), MIN_UNPRIVILEGED_ORIGIN_PORT - 1),
        ("one above the highest port".to_string(), MAX_ORIGIN_PORT + 1),
        ("one below the default HTTP port".to_string(), 79),
        ("one above the default HTTP port".to_string(), 81),
        ("one below the default HTTPS port".to_string(), 442),
        ("one above the default HTTPS port".to_string(), 444),
        ("the lowest port a TCP endpoint has".to_string(), 1),
        ("no port at all".to_string(), 0),
        ("a negative port".to_string(), -1),
    ]
}

/// RULE 3 on the VPC origin surface: a port no origin can be reached on is refused with the one
/// sentence this service states for that condition, on either port member, and mints nothing;
/// every port an origin CAN be reached on is accepted and read back.
#[tokio::test]
async fn an_endpoint_port_outside_the_allowed_range_is_refused() {
    let router = emulator();
    let (held, etag) = created_vpc_origin(&router, "vo-port-survivor").await;

    for (index, (case, port)) in accepted_ports().into_iter().enumerate() {
        let name = format!("port-accepted-{index}");
        let created = create_vpc_origin(
            &router,
            &create_body(&name, &load_balancer_arn(&name), port, port),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case} (port {port}): expected HTTP 202, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().find_text("Id").unwrap_or_default().to_string();
        let read = get_vpc_origin(&router, &id).await;
        assert_eq!(
            read.root().find_text("HTTPPort").unwrap_or_default(),
            port.to_string(),
            "case {case}: expected the submitted HTTPPort, got {}",
            read.raw
        );
    }

    // Each refused port on each member in turn, with the other member naming a port this
    // service accepts, so the refusal is about the value rather than about the pair.
    for (case, port) in refused_ports() {
        for (member, http_port, https_port) in
            [("HTTPPort", port, 443), ("HTTPSPort", 80, port)]
        {
            let refused = create_vpc_origin(
                &router,
                &create_body(
                    "port-rejected",
                    &load_balancer_arn("port-rejected"),
                    http_port,
                    https_port,
                ),
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case}/{member} (port {port}): expected HTTP 400, got {} with {}",
                refused.status, refused.raw
            );
            assert_eq!(
                refused.error_code(),
                "InvalidArgument",
                "case {case}/{member} (port {port}): expected InvalidArgument, got {:?} ({})",
                refused.error_code(),
                refused.raw
            );
            assert_eq!(
                message(&refused),
                ORIGIN_PORT_OUT_OF_RANGE_MESSAGE,
                "case {case}/{member} (port {port}): expected \
                 {ORIGIN_PORT_OUT_OF_RANGE_MESSAGE:?}, got {}",
                refused.raw
            );
        }
    }

    let read = get_vpc_origin(&router, &held).await;
    assert_eq!(
        read.status, 200,
        "case after the refusals: expected HTTP 200, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "case after the refusals: expected the create's version token, got {:?}",
        read.header("etag")
    );
}

/// RULE 3 on the distribution surface: the SAME rule and the SAME sentence answer a custom
/// origin's ports, which is what makes it one rule of the service rather than one operation's
/// check.
#[tokio::test]
async fn a_custom_origin_port_outside_the_allowed_range_is_refused() {
    let router = emulator();

    for (index, (case, port)) in accepted_ports().into_iter().enumerate() {
        let created = create_distribution(
            &router,
            &distribution_config_with_ports(&format!("port-ok-{index}"), port, port),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case} (port {port}): expected HTTP 201, got {} with {}",
            created.status, created.raw
        );
    }

    for (case, port) in refused_ports() {
        for (member, http_port, https_port) in
            [("HTTPPort", port, 443), ("HTTPSPort", 80, port)]
        {
            let refused = create_distribution(
                &router,
                &distribution_config_with_ports(
                    &format!("port-bad-{member}-{port}"),
                    http_port,
                    https_port,
                ),
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case}/{member} (port {port}): expected HTTP 400, got {} with {}",
                refused.status, refused.raw
            );
            assert_eq!(
                refused.error_code(),
                "InvalidArgument",
                "case {case}/{member} (port {port}): expected InvalidArgument, got {:?} ({})",
                refused.error_code(),
                refused.raw
            );
            assert_eq!(
                message(&refused),
                ORIGIN_PORT_OUT_OF_RANGE_MESSAGE,
                "case {case}/{member} (port {port}): expected \
                 {ORIGIN_PORT_OUT_OF_RANGE_MESSAGE:?}, got {}",
                refused.raw
            );
        }
    }
}
