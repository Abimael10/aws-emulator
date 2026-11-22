// SPDX-License-Identifier: CC0-1.0

//! The delete a still-propagating CloudFront VPC origin is refused with, and the retry that
//! lands once the clock has settled it — end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test:
//!
//! 1. the refusal a conditional delete answers a resource whose create is still propagating
//!    with is served with the service's own FIXED sentence for that condition: the same text
//!    for every such refusal, interpolating nothing — not the identifier addressed, not the
//!    name the configuration holds, not the ARN of the resource the endpoint fronts, not the
//!    version token submitted, and not the status word the record reports — so a caller that
//!    branches on the sentence to decide whether to retry recognises it;
//! 2. that refusal is TRANSIENT and writes nothing: the record and its version token stand as
//!    they were, so the very same call lands once the CLOCK has settled the deployment — with
//!    no call asking for the transition — answering the removed resource and its token, after
//!    which the id answers no read at all.
//!
//! The rows span what rule 1 generalises over: the kind of resource the endpoint fronts (an
//! Application Load Balancer, a Network Load Balancer, an EC2 instance), both ports, every
//! value `OriginProtocolPolicy` admits, whether the configuration lists `OriginSslProtocols`,
//! the shape of the endpoint name, and the FORM the `If-Match` header is written in — none of
//! which may reach the sentence.
//!
//! The codes and sentences are LITERALS here rather than the crate's own constants, so a
//! reworded refusal fails this suite. The wire harness (the router, the request helpers and
//! the element scanner) is [`crate::distribution_lifecycle`]'s, and the create is
//! [`crate::vpc_origin_lifecycle`]'s.

use std::time::Instant;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::VPC_ORIGIN_ID_PREFIX;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, emulator, Answer, ACCOUNT_ID, API_VERSION, POLL_INTERVAL, READY_DEADLINE,
};
use crate::key_value_store_lifecycle::escape;
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};

// ---------------------------------------------------------------------------
// What the answers read like
// ---------------------------------------------------------------------------

/// `VpcOrigin$Status` while the endpoint is still being deployed to the edge locations, and
/// the terminal status the clock owes it.
const DEPLOYING: &str = "Deploying";
const DEPLOYED: &str = "Deployed";

/// The sentence a delete of a VPC origin whose endpoint has not been deployed yet answers
/// with: fixed text for the condition, carrying nothing from the request or from state.
const ILLEGAL_DELETE_MESSAGE: &str =
    "The request failed because delete is not allowed for this vpc origin";

/// The sentence a read of a VPC origin the store does not hold answers with.
const NOT_FOUND_MESSAGE: &str = "The specified vpc origin does not exist";

// ---------------------------------------------------------------------------
// The VPC origins under test
// ---------------------------------------------------------------------------

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

/// One `VpcOriginEndpointConfig` a case creates, is refused the delete of, and then deletes.
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
    /// The form this row's delete writes the current version token in.
    form: Form,
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

/// The VPC origins the rules are asserted over.
///
/// The rows differ along the kind of resource the endpoint fronts, both ports, every value
/// `OriginProtocolPolicy` admits, whether the configuration lists `OriginSslProtocols`, the
/// shape of the name, and the form the `If-Match` header is written in — so a sentence that
/// interpolated any of them would read differently in at least one row.
fn delete_table() -> Vec<Row> {
    vec![
        Row {
            case: "an Application Load Balancer on the documented default ports",
            name: "orders-alb".to_string(),
            arn: load_balancer_arn("orders"),
            http_port: 80,
            https_port: 443,
            origin_protocol_policy: "http-only",
            ssl_protocols: None,
            form: Form::Bare,
        },
        Row {
            case: "a Network Load Balancer on custom ports, with every SSL protocol",
            name: "n".repeat(64),
            arn: format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/net/edge/\
                 fedcba9876543210"
            ),
            http_port: 8_080,
            https_port: 8_443,
            origin_protocol_policy: "https-only",
            ssl_protocols: Some(vec!["SSLv3", "TLSv1", "TLSv1.1", "TLSv1.2"]),
            form: Form::Quoted,
        },
        Row {
            case: "an EC2 instance on the highest ports, matching the viewer",
            name: "a".to_string(),
            arn: format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
            http_port: 65_535,
            https_port: 65_534,
            origin_protocol_policy: "match-viewer",
            ssl_protocols: Some(vec!["TLSv1.2"]),
            form: Form::Weak,
        },
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

/// Create the VPC origin `row` describes and report `(Id, ETag)`, asserting the create answers
/// the non-terminal status the clock owes the deployment away.
async fn created(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let answer = create_vpc_origin(router, &row.xml()).await;
    assert_eq!(
        answer.status, 202,
        "case {}: expected HTTP 202 from the create, got {} with {}",
        row.case, answer.status, answer.raw
    );
    assert_eq!(
        answer.root().find_text("Status").unwrap_or_default(),
        DEPLOYING,
        "case {}: expected a created VPC origin to report {DEPLOYING}, got {}",
        row.case,
        answer.raw
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
/// CLOCK owes it — or [`READY_DEADLINE`] passes, which fails the case. The token that read
/// reports is handed back, so a caller can show the settlement did not rewrite it.
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

/// RULE 1: the sentence the still-propagating delete is refused with is the service's fixed
/// text for the condition — one sentence for every row, interpolating nothing a row varies.
#[tokio::test]
async fn the_still_deploying_delete_refusal_states_the_condition_and_nothing_else() {
    let router = emulator();
    let mut sentences: Vec<(&'static str, String)> = Vec::new();
    for row in delete_table() {
        let case = row.case;
        let (id, etag) = created(&router, &row).await;
        // The caller holds the right token and acts straight after the create, so the only
        // thing that can refuse this delete is the VPC origin's own state.
        let refused = delete_vpc_origin(&router, &id, &row.form.written(&etag)).await;
        assert_refusal(&refused, case, 400, "IllegalDelete", ILLEGAL_DELETE_MESSAGE);

        // Nothing this service minted for the request reaches the sentence: the refusal states
        // the CONDITION alone.
        let message = refused
            .root()
            .find_text("Message")
            .unwrap_or_default()
            .to_string();
        for (what, value) in [
            ("the VPC origin id", id.as_str()),
            ("the endpoint ARN", row.arn.as_str()),
            ("the version token", etag.as_str()),
            ("the status word", DEPLOYING),
        ] {
            assert!(
                !message.contains(value),
                "case {case}: the refusal must interpolate nothing, but its message \
                 {message:?} carries {what} {value:?}"
            );
        }
        sentences.push((case, message));

        // The refusal wrote nothing: the record still reads back at the token it was refused
        // against, which is what makes the retry below the SAME call.
        let read = get_vpc_origin(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the refused VPC origin to still read back, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.header("etag"),
            etag,
            "case {case}: a refusal must not rewrite the version token, got {:?}",
            read.header("etag")
        );
    }

    // One sentence for every row: the endpoint name, the ports, the protocol policy, the SSL
    // protocols and the form the token was written in all vary, so a sentence that carried any
    // of them would differ here.
    let (first_case, first) = sentences
        .first()
        .cloned()
        .expect("the delete table has rows");
    for (case, message) in &sentences {
        assert_eq!(
            message, &first,
            "case {case}: every refusal of this condition must read the same as case \
             {first_case:?}, got {message:?}"
        );
    }
}

/// RULE 2: the refusal is transient — once the CLOCK has settled the deployment, with no call
/// asking for it, the very same delete lands and the VPC origin is gone.
#[tokio::test]
async fn the_same_delete_lands_once_the_clock_has_settled_the_deployment() {
    let router = emulator();
    for row in delete_table() {
        let case = row.case;
        let (id, etag) = created(&router, &row).await;
        let if_match = row.form.written(&etag);

        // The settlement neither rewrites the version token nor needs a call of its own: the
        // token the create handed out is the one the deployed record reports.
        let settled_etag = awaited_deployment(&router, case, &id).await;
        assert_eq!(
            settled_etag, etag,
            "case {case}: the settlement must not rewrite the version token, got \
             {settled_etag:?}"
        );

        let deleted = delete_vpc_origin(&router, &id, &if_match).await;
        assert_eq!(
            deleted.status, 202,
            "case {case}: expected HTTP 202 once the VPC origin is {DEPLOYED}, got {} with {}",
            deleted.status, deleted.raw
        );
        assert_eq!(
            deleted.root().find_text("Id").unwrap_or_default(),
            id,
            "case {case}: expected the delete to answer the record it removed, got {}",
            deleted.raw
        );
        assert_eq!(
            deleted.header("etag"),
            etag,
            "case {case}: expected the delete to answer the removed record's version token, \
             got {:?}",
            deleted.header("etag")
        );

        // Gone means gone: the id answers no read at all afterwards.
        let read = get_vpc_origin(&router, &id).await;
        assert_refusal(&read, case, 404, "EntityNotFound", NOT_FOUND_MESSAGE);
    }
}
