// SPDX-License-Identifier: CC0-1.0

//! Deleting a CloudFront VPC origin, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test:
//!
//! 1. a delete carrying the VPC origin's CURRENT version token removes the one record the
//!    create persisted and answers the resource it removed with that token — after which the
//!    id answers no read at all (`EntityNotFound`), a second delete answers the same, and the
//!    `Name` the endpoint configuration held is free for another create;
//! 2. the precondition is the conditional-write rule every mutation of this service runs: an
//!    `If-Match` that is no version token at all is `InvalidIfMatchVersion` (400), a
//!    well-formed token that is not this VPC origin's current one is `PreconditionFailed`
//!    (412), the quoted and weak-comparison forms of the current token are the current token,
//!    and NO refusal removes or rewrites anything;
//! 3. the precondition is evaluated before anything about the VPC origin's own state, so a
//!    caller holding a stale token learns that whatever the origin is doing;
//! 4. a VPC origin whose create is still propagating cannot be deleted yet (`IllegalDelete`,
//!    400) — and the delete lands once the CLOCK has settled the deployment, with no call
//!    asking for it;
//! 5. a VPC origin a stored distribution still fronts cannot be deleted at all
//!    (`CannotDeleteEntityWhileInUse`, 409), while one nothing fronts still deletes;
//! 6. an `Id` outside the form CloudFront writes a VPC origin identifier in is the argument
//!    the service cannot read (`InvalidArgument`, 400), and one of that form the store holds
//!    nothing under is the VPC origin that is absent (`EntityNotFound`, 404) — both decided
//!    before the precondition, since neither addresses a stored version.
//!
//! The rows span what the rules generalise over: the kind of resource the endpoint fronts
//! (an Application Load Balancer, a Network Load Balancer, an EC2 instance), both ports,
//! every value `OriginProtocolPolicy` admits, whether the configuration lists
//! `OriginSslProtocols`, the shape of the endpoint name, the FORM the `If-Match` header is
//! written in, and where the version token submitted came from (this origin, a sibling
//! origin, or nothing this service ever minted).
//!
//! The codes and sentences are LITERALS here rather than the crate's own constants, so a
//! reworded refusal fails this suite. The wire harness (the router, the request helpers and
//! the element scanner) is [`crate::distribution_lifecycle`]'s, and the create is
//! [`crate::vpc_origin_lifecycle`]'s.

use std::time::Instant;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::VPC_ORIGIN_ID_PREFIX;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, Answer, ACCOUNT_ID, API_VERSION, POLL_INTERVAL,
    READY_DEADLINE,
};
use crate::distributions_by_web_acl_id::encode_label;
use crate::key_value_store_lifecycle::escape;
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};

// ---------------------------------------------------------------------------
// What the refusals read like
// ---------------------------------------------------------------------------

/// `VpcOrigin$Status` while the endpoint is still being deployed to the edge locations, and
/// the terminal status the clock owes it.
const DEPLOYING: &str = "Deploying";
const DEPLOYED: &str = "Deployed";

/// The sentence a read of a VPC origin the store does not hold answers with.
const NOT_FOUND_MESSAGE: &str = "The specified vpc origin does not exist";

/// The sentence a delete of a VPC origin whose endpoint has not been deployed yet answers
/// with.
const ILLEGAL_DELETE_MESSAGE: &str =
    "The request failed because delete is not allowed while the VPC origin is being deployed.";

/// The sentence a delete of a VPC origin a distribution still fronts answers with.
const IN_USE_MESSAGE: &str = "The entity cannot be deleted while it is in use.";

/// The sentence a conditional write carrying no readable version token answers with.
const INVALID_IF_MATCH_MESSAGE: &str = "The If-Match version is missing or not valid for the \
     resource.";

/// A well-formed version token this service never minted: the shape CloudFront writes one in
/// (`E` followed by 13 characters of the identifier alphabet), so what refuses it is the
/// COMPARISON rather than the form.
const UNISSUED_TOKEN: &str = "EZZZZZZZZZZZZ9";

// ---------------------------------------------------------------------------
// The VPC origins under test
// ---------------------------------------------------------------------------

/// One `VpcOriginEndpointConfig` a case creates and then deletes.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    name: String,
    /// "The ARN of the CloudFront VPC origin endpoint configuration" — the resource in the
    /// caller's VPC the origin fronts.
    arn: String,
    http_port: i32,
    https_port: i32,
    /// One of `OriginProtocolPolicy`'s values (`http-only`, `match-viewer`, `https-only`, from
    /// `smithy_query(query="shape",shape="OriginProtocolPolicy")`).
    origin_protocol_policy: &'static str,
    /// The `SslProtocol` values the configuration lists, or `None` where it names no
    /// `OriginSslProtocols` wrapper at all.
    ssl_protocols: Option<Vec<&'static str>>,
}

impl Row {
    /// The `CreateVpcOriginRequest` document naming this configuration.
    fn xml(&self) -> String {
        let mut out = format!(
            "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <VpcOriginEndpointConfig>"
        );
        out.push_str(&format!("<Name>{}</Name>", escape(&self.name)));
        out.push_str(&format!("<Arn>{}</Arn>", escape(&self.arn)));
        out.push_str(&format!("<HTTPPort>{}</HTTPPort>", self.http_port));
        out.push_str(&format!("<HTTPSPort>{}</HTTPSPort>", self.https_port));
        out.push_str(&format!(
            "<OriginProtocolPolicy>{}</OriginProtocolPolicy>",
            self.origin_protocol_policy
        ));
        if let Some(protocols) = &self.ssl_protocols {
            out.push_str(&format!(
                "<OriginSslProtocols><Quantity>{}</Quantity><Items>",
                protocols.len()
            ));
            for protocol in protocols {
                out.push_str(&format!("<SslProtocol>{protocol}</SslProtocol>"));
            }
            out.push_str("</Items></OriginSslProtocols>");
        }
        out.push_str("</VpcOriginEndpointConfig></CreateVpcOriginRequest>");
        out
    }
}

/// How the `If-Match` header of a delete is written: the forms a caller holding the CURRENT
/// token may send it in.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// Bare, as this service's own `ETag` response header reports it.
    Bare,
    /// Quoted, as HTTP entity tags are normally written.
    Quoted,
    /// Quoted behind the weak-comparison prefix.
    Weak,
}

impl Form {
    /// `token` written in this form.
    fn written(self, token: &str) -> String {
        match self {
            Self::Bare => token.to_string(),
            Self::Quoted => format!("\"{token}\""),
            Self::Weak => format!("W/\"{token}\""),
        }
    }
}

/// The VPC origins rule 1 is asserted over, each with the form its delete writes the current
/// token in.
///
/// The rows differ along the kind of resource the endpoint fronts, both ports, the protocol
/// policy, whether the configuration lists `OriginSslProtocols`, the shape of the name, and
/// the form of the `If-Match` header.
fn delete_table() -> Vec<(Row, Form)> {
    vec![
        (
            Row {
                case: "an Application Load Balancer on the documented default ports",
                name: "orders-alb".to_string(),
                arn: load_balancer_arn("orders"),
                http_port: 80,
                https_port: 443,
                origin_protocol_policy: "http-only",
                ssl_protocols: None,
            },
            Form::Bare,
        ),
        (
            Row {
                case: "a Network Load Balancer on custom ports, with every SSL protocol",
                name: "n".repeat(64),
                arn: format!(
                    "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/net/edge/\
                     fedcba9876543210"
                ),
                http_port: 8080,
                https_port: 8443,
                origin_protocol_policy: "https-only",
                ssl_protocols: Some(vec!["SSLv3", "TLSv1", "TLSv1.1", "TLSv1.2"]),
            },
            Form::Quoted,
        ),
        (
            Row {
                case: "an EC2 instance on the lowest unprivileged ports",
                name: "a".to_string(),
                arn: format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
                http_port: 1_024,
                https_port: 1_025,
                origin_protocol_policy: "match-viewer",
                ssl_protocols: Some(vec!["TLSv1.2"]),
            },
            Form::Weak,
        ),
    ]
}

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

/// `DELETE /2020-05-31/vpc-origin/{Id}` under the `If-Match` header `if_match`.
async fn delete_vpc_origin(router: &MultiServiceRouter, id: &str, if_match: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/vpc-origin/{id}"),
        None,
        Some(if_match),
    )
    .await
}

/// Create the VPC origin `row` describes and report `(Id, ETag)`.
async fn created(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let answer = create_vpc_origin(router, &row.xml()).await;
    assert_eq!(
        answer.status, 202,
        "case {}: expected HTTP 202 from the create, got {} with {}",
        row.case, answer.status, answer.raw
    );
    let id = answer
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    assert!(
        id.starts_with(VPC_ORIGIN_ID_PREFIX) && id.len() > VPC_ORIGIN_ID_PREFIX.len(),
        "case {}: expected a minted VPC origin id, got {id:?} in {}",
        row.case,
        answer.raw
    );
    let etag = answer.header("etag").to_string();
    assert!(
        !etag.is_empty(),
        "case {}: expected a version token in the create's ETag header, got {}",
        row.case,
        answer.raw
    );
    (id, etag)
}

/// Look again until the VPC origin `id` reports itself [`DEPLOYED`] — the transition the
/// CLOCK owes it — or [`READY_DEADLINE`] passes, which fails the case. The token the create
/// handed out is reported back, so a caller can show the settlement did not rewrite it.
///
/// The CONDITION is what is waited for, never a span of the host's clock.
async fn awaited_deployment(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let read = get_vpc_origin(router, id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 while waiting for {DEPLOYED}, got {} with {}",
            read.status, read.raw
        );
        if read.root().find_text("Status").unwrap_or_default() == DEPLOYED {
            return read.header("etag").to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the VPC origin never reached {DEPLOYED}, last answer {}",
            read.raw
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert the VPC origin `id` is still readable at the version token `etag` — what every
/// refusal below has to leave behind.
async fn assert_still_held(router: &MultiServiceRouter, case: &str, id: &str, etag: &str) {
    let read = get_vpc_origin(router, id).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected the VPC origin to still read back, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "case {case}: a refusal must not rewrite the version token, got {:?}",
        read.header("etag")
    );
}

/// Assert `answer` is the refusal `code` carries, with status `status` and message `message`.
fn assert_refusal(answer: &Answer, case: &str, status: u16, code: &str, message: &str) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected HTTP {status}, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case}: expected {code}, got {:?} ({})",
        answer.error_code(),
        answer.raw
    );
    assert_eq!(
        answer.root().find_text("Message").unwrap_or_default(),
        message,
        "case {case}: expected message {message:?}, got {}",
        answer.raw
    );
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// RULE 1: the current version token deletes the VPC origin — the call answers the record it
/// removed with that token, and afterwards the id answers nothing, a second delete answers
/// the same, and the name is free again.
#[tokio::test]
async fn the_current_version_token_deletes_the_vpc_origin() {
    let router = emulator();

    for (row, form) in delete_table() {
        let case = row.case;
        let (id, etag) = created(&router, &row).await;
        // The delete may only run once the create has propagated (rule 4), and the settlement
        // is not a mutation, so the token the create handed out is still the current one.
        let settled_etag = awaited_deployment(&router, case, &id).await;
        assert_eq!(
            settled_etag, etag,
            "case {case}: the deployment must not rewrite the version token"
        );

        let deleted = delete_vpc_origin(&router, &id, &form.written(&etag)).await;
        assert_eq!(
            deleted.status, 202,
            "case {case}: expected HTTP 202 from the delete with {form:?}, got {} with {}",
            deleted.status, deleted.raw
        );
        assert_eq!(
            deleted.root().find_text("Id").unwrap_or_default(),
            id,
            "case {case}: expected the delete to answer the VPC origin it removed, got {}",
            deleted.raw
        );
        assert_eq!(
            deleted.root().find_text("Status").unwrap_or_default(),
            DEPLOYED,
            "case {case}: expected the status the record stood in, got {}",
            deleted.raw
        );
        assert_eq!(
            deleted
                .root()
                .at("VpcOriginEndpointConfig")
                .and_then(|config| config.text_at("Name"))
                .unwrap_or_default(),
            row.name,
            "case {case}: expected the stored endpoint configuration, got {}",
            deleted.raw
        );
        assert_eq!(
            deleted.header("etag"),
            etag,
            "case {case}: expected the removed record's version token, got {:?}",
            deleted.header("etag")
        );

        // Gone means gone: the id answers no read, and a second delete carrying the very token
        // that worked finds nothing to remove.
        let read = get_vpc_origin(&router, &id).await;
        assert_refusal(
            &read,
            case,
            404,
            "EntityNotFound",
            NOT_FOUND_MESSAGE,
        );
        let again = delete_vpc_origin(&router, &id, &etag).await;
        assert_refusal(
            &again,
            case,
            404,
            "EntityNotFound",
            NOT_FOUND_MESSAGE,
        );

        // The name the removed record held is free: the create that re-uses it is admitted and
        // mints a NEW identity rather than resurrecting the old one.
        let (second_id, _) = created(&router, &row).await;
        assert_ne!(
            second_id, id,
            "case {case}: expected a freshly minted id for the re-created VPC origin"
        );
    }
}

/// RULE 2 and RULE 3: the precondition. A header that is no version token at all is
/// `InvalidIfMatchVersion`, a well-formed token that is not this VPC origin's is
/// `PreconditionFailed` — whether it is a sibling's current token or one nothing minted — and
/// every refusal leaves the record and its token exactly as they were. Decided BEFORE the
/// VPC origin's own state, so a still-deploying origin answers the precondition's refusal
/// rather than its state's.
#[tokio::test]
async fn an_if_match_that_is_not_the_vpc_origins_own_version_is_refused() {
    let router = emulator();
    let table = delete_table();
    let (row, _) = table.first().expect("the delete table is not empty").clone();
    let (sibling_row, _) = table.get(2).expect("the delete table has a third row").clone();

    let (id, etag) = created(&router, &row).await;
    // A SIBLING VPC origin, so one row sends a token this service really issued — to another
    // resource.
    let (sibling_id, sibling_etag) = created(&router, &sibling_row).await;
    assert_ne!(
        sibling_etag, etag,
        "case the sibling: two VPC origins must not share a version token"
    );

    // Still deploying, which is the state rule 4 refuses a delete in: every row below has to
    // answer the PRECONDITION's refusal all the same.
    let deploying = get_vpc_origin(&router, &id).await;
    assert_eq!(
        deploying.root().find_text("Status").unwrap_or_default(),
        DEPLOYING,
        "case straight after the create: expected {DEPLOYING}, got {}",
        deploying.raw
    );

    // Rows along the FORM of the header: a value of no version-token form at all cannot name
    // any version, which is the condition `InvalidIfMatchVersion` states.
    let unreadable: [(&str, String); 5] = [
        ("a bare word", "not-a-version".to_string()),
        ("the VPC origin's own id", id.clone()),
        ("a token too short to be one", "E123".to_string()),
        ("the current token in lower case", etag.to_lowercase()),
        (
            "the current token with a character appended",
            format!("{etag}0"),
        ),
    ];
    for (case, header) in &unreadable {
        let refused = delete_vpc_origin(&router, &id, header).await;
        assert_refusal(
            &refused,
            case,
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_MESSAGE,
        );
        assert_still_held(&router, case, &id, &etag).await;
    }

    // Rows along WHERE a well-formed token came from: a sibling resource's current token and
    // one this service never minted are both "not this VPC origin's version".
    let stale: [(&str, String); 2] = [
        ("a sibling VPC origin's current token", sibling_etag.clone()),
        ("a token this service never minted", UNISSUED_TOKEN.to_string()),
    ];
    for (case, header) in &stale {
        let refused = delete_vpc_origin(&router, &id, header).await;
        assert_eq!(
            refused.status, 412,
            "case {case}: expected HTTP 412, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "PreconditionFailed",
            "case {case}: expected PreconditionFailed, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_still_held(&router, case, &id, &etag).await;
    }

    // The sibling the stale row borrowed a token from was never touched either.
    assert_still_held(&router, "the sibling", &sibling_id, &sibling_etag).await;

    // And the token the refusals were measured against still works: the VPC origin was never
    // consumed by any of them.
    awaited_deployment(&router, "after the refusals", &id).await;
    let deleted = delete_vpc_origin(&router, &id, &etag).await;
    assert_eq!(
        deleted.status, 202,
        "case after the refusals: expected HTTP 202, got {} with {}",
        deleted.status, deleted.raw
    );
}

/// RULE 4: a VPC origin whose create is still propagating cannot be deleted yet, and the
/// refusal stands only until the CLOCK has settled the deployment — no call asks for the
/// transition.
#[tokio::test]
async fn a_vpc_origin_that_is_still_deploying_cannot_be_deleted_yet() {
    let router = emulator();
    let (row, _) = delete_table()
        .into_iter()
        .nth(1)
        .expect("the delete table has a second row");
    let case = row.case;

    let (id, etag) = created(&router, &row).await;
    // The caller holds the right token and acts straight after the create, so the only thing
    // that can refuse this delete is the VPC origin's own state.
    let refused = delete_vpc_origin(&router, &id, &etag).await;
    assert_refusal(&refused, case, 400, "IllegalDelete", ILLEGAL_DELETE_MESSAGE);
    assert_still_held(&router, case, &id, &etag).await;

    // Once the clock owes the deployment, the very same call lands.
    awaited_deployment(&router, case, &id).await;
    let deleted = delete_vpc_origin(&router, &id, &etag).await;
    assert_eq!(
        deleted.status, 202,
        "case {case}: expected HTTP 202 once the VPC origin is {DEPLOYED}, got {} with {}",
        deleted.status, deleted.raw
    );
    let read = get_vpc_origin(&router, &id).await;
    assert_refusal(&read, case, 404, "EntityNotFound", NOT_FOUND_MESSAGE);
}

/// RULE 5: a VPC origin a stored distribution fronts through one of its origins cannot be
/// deleted, while a VPC origin nothing fronts still deletes.
#[tokio::test]
async fn a_vpc_origin_a_distribution_fronts_cannot_be_deleted() {
    let router = emulator();
    let table = delete_table();
    let (fronted_row, _) = table.first().expect("the delete table is not empty").clone();
    let (spare_row, _) = table.get(2).expect("the delete table has a third row").clone();

    let (fronted_id, fronted_etag) = created(&router, &fronted_row).await;
    let (spare_id, spare_etag) = created(&router, &spare_row).await;
    awaited_deployment(&router, fronted_row.case, &fronted_id).await;
    awaited_deployment(&router, spare_row.case, &spare_id).await;

    // One distribution whose single origin reaches the first VPC origin — the one place a
    // configuration names one.
    let config = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>vpc-origin-in-use</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin><Id>primary</Id>\
         <DomainName>primary.vpc-origin-in-use.example.com</DomainName>\
         <VpcOriginConfig><VpcOriginId>{fronted_id}</VpcOriginId></VpcOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>vpc-origin-in-use</Comment><Enabled>true</Enabled></DistributionConfig>"
    );
    let distribution = create_distribution(&router, &config).await;
    assert_eq!(
        distribution.status, 201,
        "case the fronting distribution: expected HTTP 201, got {} with {}",
        distribution.status, distribution.raw
    );

    let refused = delete_vpc_origin(&router, &fronted_id, &fronted_etag).await;
    assert_refusal(
        &refused,
        "a VPC origin a distribution fronts",
        409,
        "CannotDeleteEntityWhileInUse",
        IN_USE_MESSAGE,
    );
    assert_still_held(
        &router,
        "a VPC origin a distribution fronts",
        &fronted_id,
        &fronted_etag,
    )
    .await;

    // The VPC origin that distribution does NOT front is unaffected by the conflict: nothing
    // names it, so it deletes.
    let deleted = delete_vpc_origin(&router, &spare_id, &spare_etag).await;
    assert_eq!(
        deleted.status, 202,
        "case a VPC origin nothing fronts: expected HTTP 202, got {} with {}",
        deleted.status, deleted.raw
    );
}

/// RULE 6: the `Id` is read before the precondition — a value outside the VPC origin
/// namespace is the argument the service cannot read, and one of that form the store holds
/// nothing under is the VPC origin that is absent. Neither depends on the `If-Match` the
/// request carried.
#[tokio::test]
async fn an_id_that_addresses_no_stored_vpc_origin_is_refused_before_the_precondition() {
    let router = emulator();
    let (row, _) = delete_table()
        .into_iter()
        .next()
        .expect("the delete table is not empty");
    let (id, etag) = created(&router, &row).await;

    // Rows along the FORM of the identifier: another resource kind's id, the VPC origin's own
    // ARN, an upper-cased namespace label, a bare word, and a label carrying a body of the
    // wrong width.
    let unreadable: [(&str, String); 5] = [
        ("another kind's identifier", UNISSUED_TOKEN.to_string()),
        (
            "the VPC origin's own ARN",
            encode_label(&format!("arn:aws:cloudfront::{ACCOUNT_ID}:vpcorigin/{id}")),
        ),
        (
            "an upper-cased namespace label",
            format!("VO_{}", &id[VPC_ORIGIN_ID_PREFIX.len()..]),
        ),
        ("a bare word", "vpc-origin".to_string()),
        (
            "a body of the wrong width",
            format!("{VPC_ORIGIN_ID_PREFIX}SHORT"),
        ),
    ];
    for (case, identifier) in &unreadable {
        let refused = delete_vpc_origin(&router, identifier, &etag).await;
        assert_refusal(&refused, case, 400, "InvalidArgument", "Id format is invalid");
    }

    // Rows along the BODY of an identifier the service DOES read that no VPC origin answers
    // to: a body of letters, a body of digits, and the held id with its last character
    // changed.
    let mut flipped: Vec<char> = id.chars().collect();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == 'A' { 'B' } else { 'A' };
    let absent: [(&str, String); 3] = [
        (
            "a body of letters no VPC origin holds",
            format!("{VPC_ORIGIN_ID_PREFIX}ABSENT0000000"),
        ),
        (
            "a body of digits no VPC origin holds",
            format!("{VPC_ORIGIN_ID_PREFIX}0123456789012"),
        ),
        (
            "the held id with its last character changed",
            flipped.into_iter().collect(),
        ),
    ];
    for (case, identifier) in &absent {
        let refused = delete_vpc_origin(&router, identifier, &etag).await;
        assert_refusal(&refused, case, 404, "EntityNotFound", NOT_FOUND_MESSAGE);
    }

    // Every one of those calls left the VPC origin the service does hold exactly as it was.
    assert_still_held(&router, "after the absent deletes", &id, &etag).await;
}
