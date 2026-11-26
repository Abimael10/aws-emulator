// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ListCertificateAssociations`: enumerating the certificate
//! associations one stored responder gateway carries.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * the listing reports exactly the associations the store holds for the addressed
//!   gateway, in a stable total order by `acmCertificateArn`, a page of at most
//!   `maxResults` at a time, handing out a `nextToken` only while associations
//!   remain and resuming exactly after the page that issued it;
//! * every listed entry carries what `GetCertificateAssociation` reports for that
//!   same association, so the listing and the single-association reader report one
//!   resource — including each status the clock moves the record through;
//! * an association is addressable only through the gateway it is on, so another
//!   gateway's associations are never reported, and a gateway this store holds no
//!   responder gateway for is the service's one not-found rather than an empty page;
//! * a `nextToken` belongs to the walk that issued it, so one gateway's token is not
//!   resumed from by another gateway's listing.
//!
//! The tables span what the rules generalise over: how many associations the gateway
//! holds relative to a page (none, fewer, exactly one page, several pages with a
//! short last one), the page size at each published bound of the modelled
//! `maxResults` range (`range(min: 1, max: 10)`, default 5) and one past each, and
//! certificate ARNs spanning each partition the modelled `AcmCertificateArn` pattern
//! admits (`smithy_query(query="shape", shape="AcmCertificateArn")`).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    CERTIFICATE_STATUS_ASSOCIATED, CERTIFICATE_STATUS_DISASSOCIATED,
    CERTIFICATE_STATUS_PENDING_ASSOCIATION, RESOURCE_NOT_FOUND_MESSAGE,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// How long a wait for a clock-owed transition is given before the test gives up.
/// Generous on purpose: the test waits for the CONDITION, and only a host that never
/// settles the record at all reaches this.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between two readings of the same records.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The modelled default of this operation's `maxResults`, from
/// `smithy_query(query="operation", operation="ListCertificateAssociations")`.
const DEFAULT_PAGE: usize = 5;

/// The code a token this walk never issued is answered with: the modelled
/// client-input error the operation declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The partitions the modelled `AcmCertificateArn` pattern admits
/// (`^arn:(aws|aws-cn|aws-us-gov):acm:…`), each with a region and an account the
/// pattern admits, so an ARN built from one sits inside the member's published
/// length bounds (75..256).
const PARTITIONS: &[(&str, &str, &str, &str)] = &[
    ("the aws partition", "aws", "us-west-2", "123456789012"),
    ("the aws-cn partition", "aws-cn", "cn-north-1", "210987654321"),
    (
        "the aws-us-gov partition",
        "aws-us-gov",
        "us-gov-west-1",
        "111122223333",
    ),
];

/// The `n`th distinct certificate ARN of `partition`, as the modelled
/// `AcmCertificateArn` pattern spells one.
fn certificate_arn(partition: usize, n: usize) -> String {
    let (_, partition, region, account) = PARTITIONS[partition % PARTITIONS.len()];
    format!("arn:{partition}:acm:{region}:{account}:certificate/{n:08x}-1111-2222-3333-444455556666")
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// One answered call: the status, the wire error code (absent on a success) and the
/// parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    fn message(&self) -> String {
        text(&self.body, "message")
    }
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate -> handle
/// -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one restJson1 request and return what came back.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, code, body }
    }

    /// Create an `EXTERNAL` responder gateway — the only type a certificate
    /// associates with — and return the id the service minted.
    async fn responder_gateway(&self, case: &str, token: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/responder-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": 443,
                    "protocol": "HTTPS",
                    "domainName": "responder.example.com",
                    "gatewayType": "EXTERNAL",
                    "clientToken": token.to_string(),
                })),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    /// Create a requester gateway and return the id the service minted.
    async fn requester_gateway(&self, case: &str, token: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": token.to_string(),
                })),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    /// Associate `count` certificates with one gateway and return their ARNs sorted
    /// — the order the listing must enumerate them in. The ARNs cycle through every
    /// partition the modelled pattern admits, so a listing of more than one
    /// association reports more than one partition.
    async fn associate_many(&self, case: &str, gateway_id: &str, count: usize) -> Vec<String> {
        let mut arns = Vec::new();
        for n in 0..count {
            let arn = certificate_arn(n, n);
            let answered = self.associate(gateway_id, &arn, &format!("{case}-cert-{n}")).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected AssociateCertificate #{n} to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            arns.push(arn);
        }
        arns.sort();
        arns
    }

    async fn associate(&self, gateway_id: &str, arn: &str, token: &str) -> Answer {
        self.send(
            "POST",
            &format!("/responder-gateway/{gateway_id}/certificate"),
            Some(json!({ "acmCertificateArn": arn, "clientToken": token })),
        )
        .await
    }

    async fn get_association(&self, gateway_id: &str, arn: &str) -> Answer {
        let encoded = utf8_percent_encode(arn, NON_ALPHANUMERIC).to_string();
        self.send(
            "GET",
            &format!("/responder-gateway/{gateway_id}/certificate?acmCertificateArn={encoded}"),
            None,
        )
        .await
    }

    async fn disassociate(&self, gateway_id: &str, arn: &str) -> Answer {
        let encoded = utf8_percent_encode(arn, NON_ALPHANUMERIC).to_string();
        self.send(
            "DELETE",
            &format!("/responder-gateway/{gateway_id}/certificate?acmCertificateArn={encoded}"),
            None,
        )
        .await
    }

    /// One page of the listing, as the caller asked for it.
    async fn list(&self, gateway_id: &str, max_results: Option<i64>, token: Option<&str>) -> Answer {
        let mut query: Vec<String> = Vec::new();
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(token) = token {
            query.push(format!("nextToken={token}"));
        }
        let base = format!("/responder-gateway/{gateway_id}/certificates");
        let path = match query.is_empty() {
            true => base,
            false => format!("{base}?{}", query.join("&")),
        };
        self.send("GET", &path, None).await
    }

    /// Walk the whole listing at `max_results` per call, returning every page's
    /// `certificateAssociations` in the order the pages came back.
    async fn walk(&self, case: &str, gateway_id: &str, max_results: Option<i64>) -> Vec<Vec<Value>> {
        let mut pages: Vec<Vec<Value>> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let answered = self.list(gateway_id, max_results, token.as_deref()).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected ListCertificateAssociations to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            pages.push(associations(case, &answered.body));
            token = answered.body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages.len() <= 64,
                "case {case:?}: the walk did not terminate, {} page(s) so far",
                pages.len()
            );
        }
        pages
    }

    /// Walk the listing until it reports `expected` associations all in `status`, or
    /// a generous deadline passes: settling is a transition the clock owes, so the
    /// test waits for the CONDITION rather than for a span of time.
    async fn walked_settled(
        &self,
        case: &str,
        gateway_id: &str,
        expected: usize,
        status: &str,
    ) -> Vec<Value> {
        let give_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            let walked: Vec<Value> = self
                .walk(case, gateway_id, Some(1))
                .await
                .into_iter()
                .flatten()
                .collect();
            let settled = walked
                .iter()
                .filter(|association| text(association, "status") == status)
                .count();
            if walked.len() == expected && settled == expected {
                return walked;
            }
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected {expected} association(s) all reporting {status:?}, got {walked:?}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }
}

/// The `certificateAssociations` member of a listing response: the model declares it
/// required, so an exhausted enumeration answers an empty list rather than an absent
/// member.
fn associations(case: &str, body: &Value) -> Vec<Value> {
    body["certificateAssociations"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected certificateAssociations to be a list, got {body:?}")
        })
        .clone()
}

/// The `acmCertificateArn`s of a page, in the order it reported them.
fn arns(page: &[Value]) -> Vec<String> {
    page.iter()
        .map(|association| text(association, "acmCertificateArn"))
        .collect()
}

/// Assert `answered` is this service's one not-found.
fn assert_not_found(case: &str, answered: &Answer, request_values: &[&str]) {
    assert_eq!(
        answered.status, 404,
        "case {case:?}: expected HTTP 404, got {} {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        answered.code
    );
    assert_eq!(
        answered.message(),
        RESOURCE_NOT_FOUND_MESSAGE,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_MESSAGE:?}, got {:?}",
        answered.message()
    );
    for value in request_values {
        assert!(
            !answered.message().contains(value),
            "case {case:?}: expected the refusal to name nothing from the request, got {:?}",
            answered.message()
        );
    }
}

/// One row of the pagination table: how many certificates the gateway carries, and
/// the page size the walk asks for.
struct Row {
    name: &'static str,
    stored: usize,
    /// `None` exercises the member's modelled default of 5.
    max_results: Option<i64>,
}

/// The rows differ along what the rule generalises over: the number of stored
/// associations relative to a page (none, fewer, exactly one page, several pages with
/// a short last one) and the page size, at both published bounds of the modelled
/// `maxResults` range.
const ROWS: &[Row] = &[
    Row {
        name: "a gateway with no certificate at all",
        stored: 0,
        max_results: None,
    },
    Row {
        name: "fewer certificates than the default page",
        stored: 3,
        max_results: None,
    },
    Row {
        name: "exactly the default page",
        stored: 5,
        max_results: None,
    },
    Row {
        name: "several default pages with a short last one",
        stored: 12,
        max_results: None,
    },
    Row {
        name: "one certificate per page, the range's minimum",
        stored: 4,
        max_results: Some(1),
    },
    Row {
        name: "a page size that divides the store exactly",
        stored: 9,
        max_results: Some(3),
    },
    Row {
        name: "the range's maximum over several pages",
        stored: 17,
        max_results: Some(10),
    },
    Row {
        name: "a page larger than the gateway's certificates",
        stored: 2,
        max_results: Some(10),
    },
];

#[tokio::test]
async fn the_listing_reports_every_association_of_the_gateway_exactly_once_in_pages() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let gateway_id = fabric.responder_gateway(case, "paging-responder").await;
        let created = fabric.associate_many(case, &gateway_id, row.stored).await;

        let page_size = usize::try_from(row.max_results.unwrap_or(DEFAULT_PAGE as i64))
            .expect("a positive page");
        let pages = fabric.walk(case, &gateway_id, row.max_results).await;

        let expected_pages = row.stored.div_euclid(page_size)
            + usize::from(row.stored.rem_euclid(page_size) != 0 || row.stored == 0);
        assert_eq!(
            pages.len(),
            expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {} association(s) at {page_size} per page, got {}",
            row.stored,
            pages.len()
        );
        for (index, page) in pages.iter().enumerate() {
            assert!(
                page.len() <= page_size,
                "case {case:?}: page {index} holds {} association(s), more than the {page_size} asked for: {:?}",
                page.len(),
                arns(page)
            );
        }

        let walked: Vec<String> = arns(&pages.iter().flatten().cloned().collect::<Vec<Value>>());
        assert_eq!(
            walked, created,
            "case {case:?}: expected the walk to report every stored association exactly once in ARN order, got {walked:?}"
        );
        let distinct: BTreeSet<&String> = walked.iter().collect();
        assert_eq!(
            distinct.len(),
            walked.len(),
            "case {case:?}: expected no association to be reported twice, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_next_token_is_handed_out_only_while_associations_remain() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let gateway_id = fabric.responder_gateway(case, "token-responder").await;
        let created = fabric.associate_many(case, &gateway_id, row.stored).await;

        let mut seen = 0usize;
        let mut token: Option<String> = None;
        loop {
            let answered = fabric
                .list(&gateway_id, row.max_results, token.as_deref())
                .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected 200, got {} {:?}",
                answered.status, answered.body
            );
            seen += associations(case, &answered.body).len();

            let remaining = created.len().saturating_sub(seen);
            let more_pages_due = remaining > 0;
            assert_eq!(
                answered.body["nextToken"].is_string(),
                more_pages_due,
                "case {case:?}: with {remaining} of {} association(s) still unread, expected nextToken present={more_pages_due}, got {:?}",
                created.len(),
                answered.body["nextToken"]
            );
            token = answered.body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen,
            created.len(),
            "case {case:?}: expected the walk to read {} association(s), got {seen}",
            created.len()
        );
    }
}

#[tokio::test]
async fn every_listed_association_carries_what_the_single_association_reader_reports() {
    let case = "one certificate per partition the pattern admits";
    let fabric = Fabric::new();
    let gateway_id = fabric.responder_gateway(case, "roundtrip-responder").await;
    let created = fabric
        .associate_many(case, &gateway_id, PARTITIONS.len())
        .await;

    // Compared once every association has settled, so the listing and the reader are
    // not being asked about a status the clock is still moving.
    let listed = fabric
        .walked_settled(
            case,
            &gateway_id,
            created.len(),
            CERTIFICATE_STATUS_ASSOCIATED,
        )
        .await;
    assert_eq!(
        arns(&listed),
        created,
        "case {case:?}: expected the listing to report every associated certificate in ARN order, got {listed:?}"
    );

    for association in &listed {
        let arn = text(association, "acmCertificateArn");
        let read = fabric.get_association(&gateway_id, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetCertificateAssociation to resolve the listed {arn:?}, got {} {:?}",
            read.status, read.body
        );
        // Every member the modelled `CertificateAssociationSummary` declares, read
        // back out of the same record the single-association reader reads.
        for member in ["acmCertificateArn", "status", "associatedAt", "updatedAt"] {
            assert_eq!(
                association[member], read.body[member],
                "case {case:?}: expected the listed {member:?} of {arn:?} to match the reader's, got {:?} and {:?}",
                association[member], read.body[member]
            );
        }
        // The summary states nothing the model does not declare for it: the request
        // states the gateway, so the entry does not.
        assert!(
            association["gatewayId"].is_null(),
            "case {case:?}: expected the summary of {arn:?} to carry no gatewayId, got {:?}",
            association["gatewayId"]
        );
    }
}

#[tokio::test]
async fn the_listing_follows_the_record_through_every_status_the_clock_moves_it_to() {
    let case = "a certificate associated and then removed";
    let fabric = Fabric::new();
    let gateway_id = fabric.responder_gateway(case, "status-responder").await;
    let arn = certificate_arn(0, 1);

    let associated = fabric.associate(&gateway_id, &arn, "status-cert").await;
    assert_eq!(
        associated.status, 200,
        "case {case:?}: expected AssociateCertificate to answer 200, got {} {:?}",
        associated.status, associated.body
    );
    // The transient status the create left is listable before the clock settles it.
    let pending = fabric
        .walk(case, &gateway_id, None)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<Value>>();
    assert_eq!(
        arns(&pending),
        vec![arn.clone()],
        "case {case:?}: expected the fresh association to be listed, got {pending:?}"
    );
    let listed_status = text(&pending[0], "status");
    assert!(
        listed_status == CERTIFICATE_STATUS_PENDING_ASSOCIATION
            || listed_status == CERTIFICATE_STATUS_ASSOCIATED,
        "case {case:?}: expected a fresh association to be listed as pending or associated, got {listed_status:?}"
    );
    let settled = fabric
        .walked_settled(case, &gateway_id, 1, CERTIFICATE_STATUS_ASSOCIATED)
        .await;

    let removing = fabric.disassociate(&gateway_id, &arn).await;
    assert_eq!(
        removing.status, 200,
        "case {case:?}: expected DisassociateCertificate to answer 200, got {} {:?}",
        removing.status, removing.body
    );
    // Removal is asynchronous and terminal: the listing follows the record to its
    // terminal status rather than dropping it the moment the removal is accepted.
    let removed = fabric
        .walked_settled(case, &gateway_id, 1, CERTIFICATE_STATUS_DISASSOCIATED)
        .await;
    assert_eq!(
        arns(&removed),
        vec![arn.clone()],
        "case {case:?}: expected the removed certificate to still be listed, got {removed:?}"
    );
    assert_eq!(
        removed[0]["associatedAt"], settled[0]["associatedAt"],
        "case {case:?}: expected associatedAt to stay the instant the association was asked for, got {:?}",
        removed[0]["associatedAt"]
    );
    assert_ne!(
        removed[0]["updatedAt"], Value::Null,
        "case {case:?}: expected the removal to be dated, got {:?}",
        removed[0]
    );
}

#[tokio::test]
async fn the_listing_reports_only_the_addressed_gateways_associations_and_changes_nothing() {
    let case = "two gateways, each with its own certificates";
    let fabric = Fabric::new();
    let mine_gateway = fabric.responder_gateway(case, "scoped-mine").await;
    let other_gateway = fabric.responder_gateway(case, "scoped-other").await;

    let mine = fabric.associate_many("mine", &mine_gateway, 3).await;
    let theirs = fabric.associate_many("theirs", &other_gateway, 4).await;

    let listed = fabric
        .walk(case, &mine_gateway, None)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<Value>>();
    assert_eq!(
        arns(&listed),
        mine,
        "case {case:?}: expected only the addressed gateway's associations, got {listed:?}"
    );
    let other_listed = fabric
        .walk(case, &other_gateway, None)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<Value>>();
    assert_eq!(
        arns(&other_listed),
        theirs,
        "case {case:?}: expected the other gateway's own listing to report its associations, got {other_listed:?}"
    );

    // A read changes nothing a later caller sees: once the associations have
    // settled, the same call answers identically and the single-association reader
    // still resolves each of them.
    let settled = fabric
        .walked_settled(case, &mine_gateway, mine.len(), CERTIFICATE_STATUS_ASSOCIATED)
        .await;
    let again = fabric
        .walked_settled(case, &mine_gateway, mine.len(), CERTIFICATE_STATUS_ASSOCIATED)
        .await;
    assert_eq!(
        again, settled,
        "case {case:?}: expected a repeated listing to answer identically, got {again:?} after {settled:?}"
    );
    for arn in &mine {
        let read = fabric.get_association(&mine_gateway, arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected {arn:?} to still be readable after the listing, got {} {:?}",
            read.status, read.body
        );
    }
}

#[tokio::test]
async fn a_gateway_the_store_does_not_hold_is_refused_with_the_one_not_found() {
    let fabric = Fabric::new();
    let stored = fabric.responder_gateway("not found", "missing-responder").await;
    let requester = fabric.requester_gateway("not found", "missing-requester").await;
    fabric.associate_many("not found", &stored, 2).await;

    let released = fabric.responder_gateway("not found", "released-responder").await;
    let deleted = fabric
        .send("DELETE", &format!("/responder-gateway/{released}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "expected DeleteResponderGateway to answer 200, got {} {:?}",
        deleted.status, deleted.body
    );

    for (case, gateway_id) in [
        ("a gateway id nothing minted", "rtb-gw-00000000000000000000".to_string()),
        (
            "a requester gateway, which has no certificate surface",
            requester.clone(),
        ),
        ("a responder gateway the store released", released.clone()),
    ] {
        let answered = fabric.list(&gateway_id, None, None).await;
        assert_not_found(case, &answered, &[gateway_id.as_str()]);
        assert!(
            answered.body["certificateAssociations"].is_null(),
            "case {case:?}: expected no page at all, got {:?}",
            answered.body
        );
    }
}

#[tokio::test]
async fn a_page_size_past_a_published_bound_is_refused() {
    let fabric = Fabric::new();
    let gateway_id = fabric.responder_gateway("bounds", "bounds-responder").await;
    fabric.associate_many("bounds", &gateway_id, 2).await;

    // The published bounds of the modelled `maxResults` range are 1 and 10; one past
    // each is refused before the handler runs, and each bound itself is accepted.
    for (case, max_results, expected) in [
        ("one below the minimum", 0, 400),
        ("the minimum", 1, 200),
        ("the maximum", 10, 200),
        ("one above the maximum", 11, 400),
    ] {
        let answered = fabric.list(&gateway_id, Some(max_results), None).await;
        assert_eq!(
            answered.status, expected,
            "case {case:?}: expected maxResults={max_results} to answer {expected}, got {} {:?}",
            answered.status, answered.body
        );
    }
}

#[tokio::test]
async fn one_gateways_pagination_token_is_not_resumed_from_by_another_listing() {
    let case = "a token replayed on another enumeration";
    let fabric = Fabric::new();
    let mine_gateway = fabric.responder_gateway(case, "cursor-mine").await;
    let other_gateway = fabric.responder_gateway(case, "cursor-other").await;
    let mine = fabric.associate_many("mine", &mine_gateway, 4).await;
    let theirs = fabric.associate_many("theirs", &other_gateway, 4).await;

    let first = fabric.list(&mine_gateway, Some(1), None).await;
    assert_eq!(
        first.status, 200,
        "case {case:?}: expected the first page to answer 200, got {} {:?}",
        first.status, first.body
    );
    let token = first.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a token while associations remain, got {:?}",
                first.body
            )
        })
        .to_string();

    // The issuing walk resumes from its own token, strictly after the page it
    // answered.
    let resumed = fabric.list(&mine_gateway, Some(1), Some(&token)).await;
    assert_eq!(
        resumed.status, 200,
        "case {case:?}: expected the issuing walk to resume from its own token, got {} {:?}",
        resumed.status, resumed.body
    );
    assert_eq!(
        arns(&associations(case, &resumed.body)),
        vec![mine[1].clone()],
        "case {case:?}: expected the next page to resume after the first association, got {:?}",
        resumed.body
    );

    // Another gateway's enumeration never issued it, so it is refused rather than
    // read as an index into that gateway's associations.
    let elsewhere = fabric.list(&other_gateway, Some(1), Some(&token)).await;
    assert_eq!(
        elsewhere.status, 400,
        "case {case:?}: expected another gateway's listing to refuse the token, got {} {:?}",
        elsewhere.status, elsewhere.body
    );
    assert_eq!(
        elsewhere.code, VALIDATION_EXCEPTION,
        "case {case:?}: expected the modelled client-input error, got {:?}",
        elsewhere.code
    );
    assert!(
        !theirs.is_empty(),
        "case {case:?}: the other gateway must hold associations for the refusal to be about the token"
    );
}
