// SPDX-License-Identifier: CC0-1.0

//! What `CreateCoipPool` repeats back about a `LocalGatewayRouteTableId` it cannot read.
//!
//! The rule under test: a refusal that quotes a value the CALLER chose carries that class's
//! fixed sentence with the value the REQUEST sent between the quotes, byte for byte — so a
//! value that is no EC2 resource identifier at all is refused HTTP 400
//! `InvalidLocalGatewayRouteTableID.Malformed` with `The local-gateway-route-table ID '<id>' is
//! malformed`, whatever characters that value is made of, and the echo is the only variable
//! part of the sentence.
//!
//! The rows differ along what the rule generalises over — the FORM of the unreadable value:
//! text with no separator at all, a hexadecimal suffix with no prefix before it, a separator
//! with nothing before it, this resource's prefix with a suffix that is not hexadecimal, an
//! upper-case spelling of a well-formed id, a well-formed id with trailing whitespace, a value
//! carrying a space, a value carrying the quote character the sentence itself quotes with, a
//! value carrying the characters XML gives meaning to, and a value outside ASCII — so no single
//! row is the specification, and the recorded case is one row among them.
//!
//! Two control groups keep the classes apart: an id EC2 could have issued that the account
//! holds no table for is this resource's own `NotFound` sentence rather than this one, and a
//! route table the account DOES hold is created on, so the refusals above are about the value
//! and not about the operation refusing everything.

use std::sync::Arc;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_LOCAL_GATEWAY_ROUTE_TABLE_ID_MALFORMED,
    INVALID_LOCAL_GATEWAY_ROUTE_TABLE_ID_NOT_FOUND, LocalGatewayRouteTableSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::coip_pools::{REGION, described_ids};
use super::ec2_query::{call, element, error_code, error_message};

/// The ec2Query action under test.
const ACTION: &str = "CreateCoipPool";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The local gateway the seeded route table belongs to.
const LOCAL_GATEWAY_ID: &str = "lgw-00000000000000001";

/// The sentence this resource kind is refused a value that is no identifier at all with: fixed
/// text around the value the request sent, in single quotes.
fn malformed_sentence(id: &str) -> String {
    format!("The local-gateway-route-table ID '{id}' is malformed")
}

/// The sentence this resource answers an id EC2 could have issued that the account holds no
/// route table for with — the sibling class, which the rows below must not be answered with.
fn not_found_sentence(id: &str) -> String {
    format!("The LocalGateway Route Table ID '{id}' does not exist")
}

/// One `CreateCoipPool` call naming `route_table_id` and nothing else, as its HTTP status and
/// XML body.
async fn create(router: &MultiServiceRouter, route_table_id: &str) -> (u16, String) {
    call(
        router,
        REGION,
        ACTION,
        &[(
            "LocalGatewayRouteTableId".to_string(),
            route_table_id.to_string(),
        )],
    )
    .await
}

/// The emulator with ONE local gateway route table of the account in its store, written
/// through the store's own writer seam — the one `CreateLocalGatewayRouteTable` writes through
/// — and that table's minted id.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>, String) {
    let (router, engine) = super::ec2_query::emulator();
    let handle = engine.state::<Ec2State>();
    let route_table = handle
        .lock()
        .insert_local_gateway_route_table(LocalGatewayRouteTableSpec {
            local_gateway_id: LOCAL_GATEWAY_ID.to_string(),
            outpost_arn: format!(
                "arn:aws:outposts:{REGION}:{}:outpost/op-00000000000000001",
                engine::emulator_account_id()
            ),
            region: REGION.to_string(),
            mode: "coip".to_string(),
            state: "available".to_string(),
            becomes: None,
            tags: Vec::new(),
        })
        .expect("the store must take the seeded route table");
    (router, engine, route_table)
}

/// The values that are no EC2 resource identifier at all — nothing shaped like
/// `<prefix>-<lowercase hex>`, which is the form the request boundary reads an identifier by —
/// each with the name its case is reported under.
const UNREADABLE_VALUES: &[(&str, &str)] = &[
    ("text with no separator at all", "notanidatall"),
    // The value the recorded refusal of this operation carries: ONE row of the class.
    ("words joined by separators", "totally-not-an-id"),
    ("a hexadecimal suffix with no prefix", "0123456789abcdef0"),
    ("a separator with nothing before it", "-0123456789abcdef0"),
    ("this prefix with a suffix that is not hex", "lgw-rtb-zzzz"),
    (
        "an upper-case spelling of a well-formed id",
        "LGW-RTB-0123456789ABCDEF0",
    ),
    (
        "a well-formed id with trailing whitespace",
        "lgw-rtb-0123456789abcdef0 ",
    ),
    ("a value carrying a space", "my route table"),
    ("a value carrying the quote character", "it's-not-an-id"),
    ("a value carrying XML markup characters", "<pool>&route"),
    ("a value outside ASCII", "lgw-rtb-ünïcode"),
];

/// Both id lengths EC2 publishes for this resource (`lgw-rtb-` and 8 or 17 lowercase hex
/// digits), held by no route table of the account.
const ABSENT_IDS: &[&str] = &["lgw-rtb-0123456789abcdef0", "lgw-rtb-0a1b2c3d"];

/// RULE: a `LocalGatewayRouteTableId` that is no EC2 resource identifier at all is refused HTTP
/// 400 `InvalidLocalGatewayRouteTableID.Malformed` with this resource kind's fixed sentence, the
/// value the REQUEST sent quoted inside it byte for byte and escaped for the envelope it travels
/// in — and nothing is created.
#[tokio::test]
async fn an_unreadable_route_table_id_is_quoted_back_in_the_malformed_sentence() {
    let (router, _engine, _route_table) = emulator();

    for (case, value) in UNREADABLE_VALUES {
        let (status, body) = create(&router, value).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let answered_code = error_code(&body);
        assert_eq!(
            answered_code, INVALID_LOCAL_GATEWAY_ROUTE_TABLE_ID_MALFORMED,
            "case {case:?}: expected the code {INVALID_LOCAL_GATEWAY_ROUTE_TABLE_ID_MALFORMED:?}, \
             got {answered_code:?} in {body}"
        );
        let expected = malformed_sentence(value);
        let answered = error_message(&body);
        assert_eq!(
            answered, expected,
            "case {case:?}: expected the message {expected:?}, got {answered:?}"
        );
        // A refusal that quotes a value the caller chose still has to be a well-formed
        // response: the echo is escaped for the envelope rather than written into it raw.
        assert!(
            roxmltree::Document::parse(&body).is_ok(),
            "case {case:?}: the refusal must be well-formed XML, got {body}"
        );
    }

    let reported = described_ids(&router, "after every unreadable id", &[]).await;
    assert!(
        reported.is_empty(),
        "a refused create must store nothing, got {reported:?}"
    );
}

/// RULE: the sentence above belongs to the MALFORMED class alone — an id EC2 could have issued
/// that names no route table of the account is the resource's own `NotFound`, with its own
/// sentence quoting the same value, and still creates nothing.
#[tokio::test]
async fn an_id_ec2_could_have_issued_is_the_not_found_class_instead() {
    let (router, _engine, _route_table) = emulator();

    for absent in ABSENT_IDS {
        let case = format!("the unheld route table {absent:?}");
        let (status, body) = create(&router, absent).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let answered_code = error_code(&body);
        assert_eq!(
            answered_code, INVALID_LOCAL_GATEWAY_ROUTE_TABLE_ID_NOT_FOUND,
            "case {case:?}: expected the code {INVALID_LOCAL_GATEWAY_ROUTE_TABLE_ID_NOT_FOUND:?}, \
             got {answered_code:?} in {body}"
        );
        let expected = not_found_sentence(absent);
        let answered = error_message(&body);
        assert_eq!(
            answered, expected,
            "case {case:?}: expected the message {expected:?}, got {answered:?}"
        );
    }

    let reported = described_ids(&router, "after every unheld id", &[]).await;
    assert!(
        reported.is_empty(),
        "a refused create must store nothing, got {reported:?}"
    );
}

/// RULE: the refusals above are about the VALUE — the route table the account does hold is
/// created on, and the pool that create answers is the one `DescribeCoipPools` then reports.
#[tokio::test]
async fn the_route_table_the_account_holds_is_created_on() {
    let (router, _engine, route_table) = emulator();

    let (status, body) = create(&router, &route_table).await;
    assert_eq!(
        status, 200,
        "the seeded route table: expected HTTP 200, got {status} with body {body}"
    );
    let pool_id = element(&body, "poolId").unwrap_or_default();
    let reported_table = element(&body, "localGatewayRouteTableId").unwrap_or_default();
    assert_eq!(
        reported_table, route_table,
        "the seeded route table: expected localGatewayRouteTableId {route_table:?}, got \
         {reported_table:?} in {body}"
    );

    let reported = described_ids(&router, "after the accepted create", &[]).await;
    assert_eq!(
        reported,
        vec![pool_id.clone()],
        "expected DescribeCoipPools to report exactly the created pool {pool_id:?}, got \
         {reported:?}"
    );
}
