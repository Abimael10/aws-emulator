// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for an account's EGRESS-ONLY internet gateways:
//! `CreateEgressOnlyInternetGateway` mints one for a VPC,
//! `DescribeEgressOnlyInternetGateways` reports it back and
//! `DeleteEgressOnlyInternetGateway` takes it away. Driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol"). ec2Query is
//! selected by the `ec2` signing name in the request's SigV4 credential scope, so every call
//! here carries one.
//!
//! The rules under test:
//!
//! 1. A create PERSISTS the resource it answered: the very next read of that gateway reports
//!    the SAME record — its `eigw-` id, the tags the request assigned, and an attachment to
//!    the VPC the request named in the `attached` state, because "creates an egress-only
//!    internet gateway for your VPC" (`CreateEgressOnlyInternetGateway`) is itself the
//!    attachment: this kind of gateway has no `Attach…` call.
//! 2. A `ClientToken` makes the create idempotent — the same token replayed reports the one
//!    gateway it minted and creates nothing, replayed with any other parameter it is refused
//!    with `IdempotentParameterMismatch` — and a create carrying no token mints a gateway of
//!    its own every time.
//! 3. A `VpcId` the account holds no VPC for is refused, with nothing stored.
//! 4. `EgressOnlyInternetGatewayIds` narrows the report to the ids named, and an id the
//!    account holds no gateway for narrows it to nothing rather than being refused.
//! 5. Each filter the operation documents (`tag:<key>` and `tag-key`, the two
//!    `DescribeEgressOnlyInternetGatewaysRequest$Filters` lists) selects on the member it
//!    names, and an undocumented filter name is refused rather than reported as an empty set.
//! 6. `MaxResults` pages the report in `EgressOnlyInternetGatewayId` order, with a `NextToken`
//!    only while a further gateway follows the page; a token this service did not issue is
//!    refused.
//! 7. A `DryRun` of any of the three operations answers `DryRunOperation` and changes nothing.
//! 8. A delete removes the gateway it names and nothing else, reports `returnCode` true, and
//!    an id naming no gateway of the account is refused with `InvalidGatewayID.NotFound`.
//! 9. The gateway is a resource of its VPC: `DeleteVpc` is refused while the VPC has one, and
//!    succeeds once it is deleted.
//! 10. A route may target the gateway (`CreateRoute`'s `EgressOnlyInternetGatewayId`), and
//!     the route goes to `blackhole` when the gateway it points at is deleted.
//! 11. A tag specification naming any other resource type is refused, with nothing stored.
//!
//! Every table row is a distinct input along what those rules generalise over — how many tags
//! a gateway carries and what they spell, which VPC it belongs to, how many gateways the
//! account holds, which selector narrows the report and how large a page is asked for — so no
//! single recorded request is the specification.

use ec2::state::{
    EGRESS_ONLY_INTERNET_GATEWAY_ID_PREFIX, EGRESS_ONLY_INTERNET_GATEWAY_TAGGABLE_RESOURCE,
    MAX_CLIENT_TOKEN_CHARS, MAX_TAGS_PER_RESOURCE, MAX_TAG_KEY_CHARS, MAX_TAG_VALUE_CHARS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{error_code, error_message, filters, indexed, member};
use super::wire::{child, child_text, document, is_long_id, param, set_items, Param};

/// The Region every call here is signed for. An egress-only internet gateway carries no Region
/// member, so the Region only picks which account's ambient network the store already holds.
const REGION: &str = "us-east-1";

/// The `AttachmentStatus` a live attachment is reported in
/// (`smithy_query(query="shape",shape="AttachmentStatus")`), which is the state the create
/// writes.
const ATTACHED: &str = "attached";

/// One egress-only internet gateway as a response reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedGateway {
    egress_only_internet_gateway_id: Option<String>,
    /// Every attachment of the gateway, as `(state, vpcId)`.
    attachments: Vec<(Option<String>, Option<String>)>,
    tags: Vec<(String, String)>,
}

/// The gateway one `<egressOnlyInternetGateway>` or `<egressOnlyInternetGatewaySet>` item
/// reports.
///
/// Read off the TREE: a tag's `<value>` and an attachment's `<state>` are children of their
/// own `<item>`, so scanning the body for an element name would mix two records together.
fn gateway_from(node: roxmltree::Node<'_, '_>) -> ReportedGateway {
    ReportedGateway {
        egress_only_internet_gateway_id: child_text(node, "egressOnlyInternetGatewayId"),
        attachments: set_items(node, "attachmentSet")
            .into_iter()
            .map(|item| (child_text(item, "state"), child_text(item, "vpcId")))
            .collect(),
        tags: set_items(node, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "key").unwrap_or_default(),
                    child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect(),
    }
}

/// The one gateway a `CreateEgressOnlyInternetGateway` response carries.
fn created_gateway(case: &str, body: &str) -> ReportedGateway {
    let parsed = document(case, body);
    let node = child(parsed.root_element(), "egressOnlyInternetGateway").unwrap_or_else(|| {
        panic!("case {case:?}: expected an <egressOnlyInternetGateway> in {body}")
    });
    gateway_from(node)
}

/// The gateways a `DescribeEgressOnlyInternetGateways` response carries, in report order.
fn described_gateways(case: &str, body: &str) -> Vec<ReportedGateway> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "egressOnlyInternetGatewaySet")
        .into_iter()
        .map(gateway_from)
        .collect()
}

/// The ids of the gateways a `DescribeEgressOnlyInternetGateways` response carries, in report
/// order.
fn described_ids(case: &str, body: &str) -> Vec<String> {
    described_gateways(case, body)
        .into_iter()
        .map(|gateway| {
            gateway
                .egress_only_internet_gateway_id
                .unwrap_or_default()
        })
        .collect()
}

/// The `TagSpecifications` form fields that assign `tags` to the gateway, as ec2Query flattens
/// them — and nothing at all for a request that assigns none.
fn tag_params(tags: &[(&str, &str)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        EGRESS_ONLY_INTERNET_GATEWAY_TAGGABLE_RESOURCE,
    )];
    for (position, (key, value)) in tags.iter().enumerate() {
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Key", position + 1),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Value", position + 1),
            value,
        ));
    }
    params
}

/// One `CreateEgressOnlyInternetGateway` call carrying `params`.
async fn create(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, "CreateEgressOnlyInternetGateway", params).await
}

/// One `DescribeEgressOnlyInternetGateways` call carrying `params`.
async fn describe(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, "DescribeEgressOnlyInternetGateways", params).await
}

/// One `DeleteEgressOnlyInternetGateway` call carrying `params`.
async fn delete(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, "DeleteEgressOnlyInternetGateway", params).await
}

/// One call of `action` that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = super::ec2_query::call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A VPC with `cidr_block`, as its id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "vpcId")
        .and_then(|node| node.text().map(str::to_string))
        .unwrap_or_else(|| panic!("case {case:?}: the create answered no vpcId: {body}"))
}

/// A gateway of `vpc_id` carrying `tags`, created over the wire, as the record the create
/// answered.
async fn create_gateway(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    tags: &[(&str, &str)],
) -> ReportedGateway {
    let mut params = vec![param("VpcId", vpc_id)];
    params.extend(tag_params(tags));
    let (status, body) = create(router, &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the create, got {status} with body {body}"
    );
    created_gateway(case, &body)
}

/// The id of a gateway of `vpc_id` carrying `tags`, created over the wire.
async fn create_gateway_id(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    tags: &[(&str, &str)],
) -> String {
    create_gateway(router, case, vpc_id, tags)
        .await
        .egress_only_internet_gateway_id
        .unwrap_or_else(|| {
            panic!("case {case:?}: the create answered no egressOnlyInternetGatewayId")
        })
}

/// The one gateway `gateway_id` names, read back through the describe.
async fn read_back(router: &MultiServiceRouter, case: &str, gateway_id: &str) -> ReportedGateway {
    let (status, body) = describe(
        router,
        &indexed("EgressOnlyInternetGatewayId", &[gateway_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading {gateway_id} back, got {status} with body \
         {body}"
    );
    let reported = described_gateways(case, &body);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly the gateway asked for, got {reported:?}"
    );
    reported
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a one-element report has a first element: {body}"))
}

/// Every gateway the account holds, in report order.
async fn all_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = describe(router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the unfiltered report, got {status} with body \
         {body}"
    );
    described_ids(case, &body)
}

/// One row of the create table: the tags a request assigns to the gateway it creates.
#[derive(Debug, Clone)]
struct GatewayCase {
    case: &'static str,
    tags: Vec<(String, String)>,
}

/// The create table: one row per kind of tag payload a caller can assign, spanning the count
/// and the size of what a gateway holds. The published bounds are this crate's shared tag
/// bounds (`MAX_TAGS_PER_RESOURCE`, `MAX_TAG_KEY_CHARS`, `MAX_TAG_VALUE_CHARS`), so the last
/// two rows sit exactly on them.
fn gateway_cases() -> Vec<GatewayCase> {
    let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
    vec![
        GatewayCase {
            case: "no tag specification at all",
            tags: Vec::new(),
        },
        GatewayCase {
            case: "one tag",
            tags: vec![pair("Name", "ipv6-egress")],
        },
        GatewayCase {
            case: "three tags, one of them with an empty value",
            tags: vec![
                pair("Name", "alpha"),
                pair("Tier", "web"),
                pair("Note", ""),
            ],
        },
        GatewayCase {
            case: "a value carrying spaces and punctuation",
            tags: vec![pair("Project.Name", "egress only (eu-west)")],
        },
        GatewayCase {
            case: "a key and a value at their published lengths",
            tags: vec![pair(
                &"k".repeat(MAX_TAG_KEY_CHARS),
                &"v".repeat(MAX_TAG_VALUE_CHARS),
            )],
        },
        GatewayCase {
            case: "as many tags as one resource holds",
            tags: (0..MAX_TAGS_PER_RESOURCE)
                .map(|index| pair(&format!("key-{index}"), &format!("value-{index}")))
                .collect(),
        },
    ]
}

/// `row`'s tags as the borrowed pairs the request builder takes.
fn borrowed(row: &GatewayCase) -> Vec<(&str, &str)> {
    row.tags
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// RULE 1: every row of the create table is stored as the create answered it, attached to the
/// VPC the request named, and each create mints a gateway of its own.
#[tokio::test]
async fn a_created_gateway_is_read_back_exactly_as_it_was_created() {
    let (router, _engine) = super::ec2_query::emulator();

    // Two VPCs, so "the VPC the request named" is a row of the table rather than the one VPC
    // the account happens to hold.
    let first_vpc = create_vpc("the first VPC", &router, "10.0.0.0/16").await;
    let second_vpc = create_vpc("the second VPC", &router, "172.31.0.0/16").await;

    let mut created: Vec<String> = Vec::new();
    for (index, row) in gateway_cases().into_iter().enumerate() {
        let case = row.case;
        let vpc_id = if index % 2 == 0 { &first_vpc } else { &second_vpc };
        let answered = create_gateway(&router, case, vpc_id, &borrowed(&row)).await;

        let gateway_id = answered
            .egress_only_internet_gateway_id
            .clone()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the create answered no egressOnlyInternetGatewayId")
            });
        assert!(
            is_long_id(EGRESS_ONLY_INTERNET_GATEWAY_ID_PREFIX, &gateway_id),
            "case {case:?}: expected an {EGRESS_ONLY_INTERNET_GATEWAY_ID_PREFIX}-<17 hex \
             digits> id, got {gateway_id:?}"
        );
        assert_eq!(
            answered.attachments,
            vec![(Some(ATTACHED.to_string()), Some(vpc_id.clone()))],
            "case {case:?}: expected the gateway attached to {vpc_id} in state {ATTACHED}, got \
             {answered:?}"
        );
        assert_eq!(
            answered.tags, row.tags,
            "case {case:?}: expected the tags the request assigned, got {:?}",
            answered.tags
        );

        // The create persisted the record, so the very next read reports the same one.
        let stored = read_back(&router, case, &gateway_id).await;
        assert_eq!(
            stored, answered,
            "case {case:?}: expected the describe to report the record the create answered \
             ({answered:?}), got {stored:?}"
        );

        assert!(
            !created.contains(&gateway_id),
            "case {case:?}: expected a gateway of its own, got the id {gateway_id} a previous \
             create already minted"
        );
        created.push(gateway_id);
    }

    // Every one of them is reported, in `EgressOnlyInternetGatewayId` order — which the ids
    // this store mints are also created in, so the table's order is the report's order.
    let mut expected = created.clone();
    expected.sort();
    assert_eq!(
        all_ids(&router, "every created gateway").await,
        expected,
        "expected every created gateway reported in id order, from {created:?}"
    );
}

/// One row of the idempotency table: the case, the `ClientToken` it gives the create, and the
/// tags that create assigns — named, because the row is the shape clippy asks for a name for.
type TokenCase<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>);

/// RULE 2: the `ClientToken` decides whether a repeat is a replay or a second gateway.
#[tokio::test]
async fn a_client_token_replays_the_gateway_it_minted() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;
    let other_vpc = create_vpc("the other VPC", &router, "172.31.0.0/16").await;

    // No token at all: each call is a gateway of its own, however identical the request.
    let first = create_gateway_id(&router, "the first untokened create", &vpc_id, &[]).await;
    let second = create_gateway_id(&router, "the repeat", &vpc_id, &[]).await;
    assert_ne!(
        first, second,
        "an untokened repeat must mint a second gateway, not replay the first"
    );

    // (case, the token, the tags) — each row a token given once and then replayed.
    let rows: Vec<TokenCase> = vec![
        ("a token and no tags", "token-plain", Vec::new()),
        ("a token and one tag", "token-tagged", vec![("Name", "edge")]),
        (
            "a token and two tags, replayed in the other order",
            "token-two",
            vec![("Name", "edge"), ("Tier", "web")],
        ),
    ];
    for (case, token, tags) in rows {
        let mut params = vec![param("VpcId", &vpc_id), param("ClientToken", token)];
        params.extend(tag_params(&tags));

        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the first create, got {status} with body \
             {body}"
        );
        let minted = created_gateway(case, &body);
        assert_eq!(
            member(&body, "clientToken").as_deref(),
            Some(token),
            "case {case:?}: expected the token the request carried echoed, got body {body}"
        );

        // The same request again: the one gateway that token minted, and nothing new. Where
        // the row carries more than one tag the replay names them in the OTHER order — tags
        // are a SET, so the order a request lists them in is not a difference the idempotency
        // check may see.
        let mut reversed = tags.clone();
        reversed.reverse();
        let mut replay_params = vec![param("VpcId", &vpc_id), param("ClientToken", token)];
        replay_params.extend(tag_params(&reversed));
        let before = all_ids(&router, case).await;
        let (status, body) = create(&router, &replay_params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the replay, got {status} with body {body}"
        );
        assert_eq!(
            created_gateway(case, &body),
            minted,
            "case {case:?}: expected the replay to report the gateway the token minted"
        );
        assert_eq!(
            all_ids(&router, case).await,
            before,
            "case {case:?}: a replay must create nothing"
        );

        // The same token with any other parameter names a gateway the caller did not ask for.
        let mismatches: Vec<(&str, Vec<Param>)> = vec![
            ("another VPC", {
                let mut params = vec![param("VpcId", &other_vpc), param("ClientToken", token)];
                params.extend(tag_params(&tags));
                params
            }),
            ("another tag payload", {
                let mut params = vec![param("VpcId", &vpc_id), param("ClientToken", token)];
                params.extend(tag_params(&[("Name", "something-else")]));
                params
            }),
        ];
        for (what, params) in mismatches {
            let (status, body) = create(&router, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?} with {what}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "IdempotentParameterMismatch",
                "case {case:?} with {what}: expected the idempotency refusal, got body {body}"
            );
            assert_eq!(
                all_ids(&router, case).await,
                before,
                "case {case:?} with {what}: a refused create must store nothing"
            );
        }
    }

    // A create that carried no token answers none at all, rather than an empty one.
    let (status, body) = create(&router, &[param("VpcId", &vpc_id)]).await;
    assert_eq!(status, 200, "the untokened create: got {status}: {body}");
    assert_eq!(
        member(&body, "clientToken"),
        None,
        "a create carrying no token must answer no clientToken, got body {body}"
    );
}

/// RULE 2's bound: the `ClientToken` the store keeps is held to this crate's published
/// client-token length — a token exactly on [`MAX_CLIENT_TOKEN_CHARS`] is accepted and one
/// character past it is refused, with nothing stored.
#[tokio::test]
async fn a_client_token_is_bounded_in_length() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;

    let on_the_bound = "t".repeat(MAX_CLIENT_TOKEN_CHARS);
    let (status, body) = create(
        &router,
        &[
            param("VpcId", &vpc_id),
            param("ClientToken", &on_the_bound),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "a token of {MAX_CLIENT_TOKEN_CHARS} characters: expected HTTP 200, got {status} with \
         body {body}"
    );
    let minted = created_gateway("a token on the bound", &body)
        .egress_only_internet_gateway_id
        .unwrap_or_default();

    let past_the_bound = "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1);
    let (status, body) = create(
        &router,
        &[
            param("VpcId", &vpc_id),
            param("ClientToken", &past_the_bound),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "a token one character past the bound: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "a token one character past the bound: expected the invalid-value refusal, got body \
         {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("clientToken accepts at most {MAX_CLIENT_TOKEN_CHARS} characters"),
        "a token one character past the bound: expected the wording naming the bound, got body \
         {body}"
    );
    assert_eq!(
        all_ids(&router, "after the refused token").await,
        vec![minted],
        "a refused create must store nothing"
    );
}

/// RULE 3: a `VpcId` the account holds no VPC for is refused, in each form a client can send
/// one, and nothing is stored on the way to the refusal.
#[tokio::test]
async fn a_vpc_id_naming_no_vpc_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();

    for unknown in [
        "vpc-0123456789abcdef0",
        "vpc-00000000000000999",
        "vpc-12345678",
        "not-a-vpc",
    ] {
        let (status, body) = create(&router, &[param("VpcId", unknown)]).await;
        assert_eq!(
            status, 400,
            "case {unknown:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVpcId.Malformed",
            "case {unknown:?}: expected the unresolvable-VPC refusal, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The vpc ID '{unknown}' is malformed"),
            "case {unknown:?}: expected the recorded wording, got body {body}"
        );
        assert!(
            all_ids(&router, unknown).await.is_empty(),
            "case {unknown:?}: a refused create must store nothing"
        );
    }
}

/// RULE 4: the id list narrows the report, and an id naming no gateway of the account narrows
/// it to nothing rather than being refused.
#[tokio::test]
async fn an_id_list_narrows_the_report() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;

    let first = create_gateway_id(&router, "the first gateway", &vpc_id, &[("Name", "a")]).await;
    let second = create_gateway_id(&router, "the second gateway", &vpc_id, &[("Name", "b")]).await;
    let third = create_gateway_id(&router, "the third gateway", &vpc_id, &[]).await;

    // (case, the ids asked for, the ids expected back)
    let selections: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        (
            "no id at all reports the whole store",
            Vec::new(),
            vec![first.as_str(), second.as_str(), third.as_str()],
        ),
        ("one id", vec![second.as_str()], vec![second.as_str()]),
        (
            "two of the three, asked for in the other order",
            vec![third.as_str(), first.as_str()],
            vec![first.as_str(), third.as_str()],
        ),
        (
            "the same id twice is still one row",
            vec![first.as_str(), first.as_str()],
            vec![first.as_str()],
        ),
        (
            "a known id beside one the account holds no gateway for",
            vec![first.as_str(), "eigw-0123456789abcdef0"],
            vec![first.as_str()],
        ),
        (
            "an id the account holds no gateway for, whatever its form",
            vec!["eigw-0123456789abcdef0", "eigw-12345678", "not-an-eigw"],
            Vec::new(),
        ),
    ];
    for (case, asked, expected) in selections {
        let (status, body) =
            describe(&router, &indexed("EgressOnlyInternetGatewayId", &asked)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            described_ids(case, &body),
            super::ec2_query::owned(&expected),
            "case {case:?}: expected exactly {expected:?}, got body {body}"
        );
    }
}

/// One row of the filter table: the case, the `Filters` entries it sends, and the gateway ids
/// it must select — named, because the row is the shape clippy asks for a name for.
type FilterCase<'a> = (&'a str, Vec<(&'a str, Vec<&'a str>)>, Vec<&'a str>);

/// RULE 5: each documented filter selects on the member it names, and a name the operation
/// does not document is refused rather than reported as an empty set.
#[tokio::test]
async fn every_documented_filter_selects_on_its_member() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;

    let alpha = create_gateway_id(
        &router,
        "the tagged gateway",
        &vpc_id,
        &[("Name", "alpha"), ("Tier", "web")],
    )
    .await;
    let beta =
        create_gateway_id(&router, "the other tagged gateway", &vpc_id, &[("Name", "beta")]).await;
    let untagged = create_gateway_id(&router, "the untagged gateway", &vpc_id, &[]).await;

    // (case, the filters, the ids expected back)
    let cases: Vec<FilterCase> = vec![
        (
            "tag:<key> selects by the value under that key",
            vec![("tag:Name", vec!["alpha"])],
            vec![alpha.as_str()],
        ),
        (
            "tag:<key> over two values is their OR",
            vec![("tag:Name", vec!["alpha", "beta"])],
            vec![alpha.as_str(), beta.as_str()],
        ),
        (
            "tag:<key> with a value nothing carries selects nothing",
            vec![("tag:Name", vec!["gamma"])],
            Vec::new(),
        ),
        (
            "tag:<key> on a key only one gateway carries",
            vec![("tag:Tier", vec!["web"])],
            vec![alpha.as_str()],
        ),
        (
            "tag-key selects whatever carries the key, whatever its value",
            vec![("tag-key", vec!["Name"])],
            vec![alpha.as_str(), beta.as_str()],
        ),
        (
            "two filters are joined with an AND",
            vec![("tag-key", vec!["Name"]), ("tag:Name", vec!["beta"])],
            vec![beta.as_str()],
        ),
        (
            "two filters whose AND nothing satisfies",
            vec![("tag:Name", vec!["alpha"]), ("tag:Tier", vec!["db"])],
            Vec::new(),
        ),
        (
            "a filter carrying no value at all selects nothing",
            vec![("tag-key", Vec::new())],
            Vec::new(),
        ),
    ];
    for (case, entries, expected) in cases {
        let as_slices: Vec<(&str, &[&str])> = entries
            .iter()
            .map(|(name, values)| (*name, values.as_slice()))
            .collect();
        let (status, body) = describe(&router, &filters(&as_slices)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            described_ids(case, &body),
            super::ec2_query::owned(&expected),
            "case {case:?}: expected exactly {expected:?}, got body {body}"
        );
    }
    // Every gateway is still there: a filtered read selects, it never consumes.
    let mut held = vec![alpha, beta, untagged];
    held.sort();
    assert_eq!(
        all_ids(&router, "after the filtered reads").await,
        held,
        "a filtered read must leave the store as it was"
    );

    // A filter name the operation does not document is refused — including the names the
    // SIBLING describe documents, since this operation's `Filters` lists only the two tag
    // ones, and a `tag:` naming no key.
    for name in [
        "attachment.state",
        "attachment.vpc-id",
        "egress-only-internet-gateway-id",
        "owner-id",
        "Tag-Key",
        "tag:",
    ] {
        let (status, body) = describe(&router, &filters(&[(name, &["anything"])])).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {name:?}: expected an invalid-filter refusal, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The filter '{name}' is invalid"),
            "case {name:?}: expected the invalid-filter wording, got body {body}"
        );
    }
}

/// RULE 6: the report pages in id order, a token comes back only while a further gateway
/// follows the page, and a token this service did not issue is refused.
#[tokio::test]
async fn the_report_pages_in_id_order() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;

    let mut ids: Vec<String> = Vec::new();
    for index in 0..7 {
        ids.push(
            create_gateway_id(
                &router,
                "a gateway to page over",
                &vpc_id,
                &[("Index", &index.to_string())],
            )
            .await,
        );
    }
    ids.sort();

    // The smallest page the model admits (`DescribeEgressOnlyInternetGatewaysMaxResults`,
    // range 5..255), walked to the end: the last page carries no token.
    let page_size = 5usize;
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut params = vec![param("MaxResults", &page_size.to_string())];
        if let Some(cursor) = token.as_deref() {
            params.push(param("NextToken", cursor));
        }
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 200,
            "page {pages}: expected HTTP 200, got {status} with body {body}"
        );
        let page = described_ids("a page", &body);
        assert!(
            page.len() <= page_size,
            "page {pages}: expected at most {page_size} gateways, got {page:?}"
        );
        seen.extend(page);
        token = member(&body, "nextToken").filter(|value| !value.is_empty());
        pages += 1;
        assert!(
            pages <= ids.len(),
            "paging must terminate: {pages} pages for {} gateways, last body {body}",
            ids.len()
        );
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        pages, 2,
        "expected {} gateways to take two pages of {page_size}, took {pages}",
        ids.len()
    );
    assert_eq!(
        seen, ids,
        "expected the walk to report every gateway once, in id order, got {seen:?}"
    );

    // A page as large as the store needs no token at all. 255 is the top of the published
    // range, the largest page the operation can be asked for.
    let (status, body) = describe(&router, &[param("MaxResults", "255")]).await;
    assert_eq!(
        status, 200,
        "one page: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        described_ids("one page", &body),
        ids,
        "one page: expected every gateway, got body {body}"
    );
    assert_eq!(
        member(&body, "nextToken"),
        None,
        "one page: nothing follows the last gateway, so no token is due, got body {body}"
    );

    // A token this service never issued is refused rather than read as an id.
    for bogus in ["not-a-token", "ZWlndy0w", ""] {
        let (status, body) = describe(&router, &[param("NextToken", bogus)]).await;
        assert_eq!(
            status, 400,
            "case {bogus:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidPaginationToken",
            "case {bogus:?}: expected a pagination-token refusal, got body {body}"
        );
    }
}

/// RULE 7: a dry run of any of the three operations answers `DryRunOperation` and leaves the
/// store as it was.
#[tokio::test]
async fn a_dry_run_changes_nothing() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;
    let kept = create_gateway_id(&router, "the stored gateway", &vpc_id, &[("Name", "kept")]).await;

    // (case, the operation, its request fields)
    let dry_runs: Vec<(&str, &str, Vec<Param>)> = vec![
        (
            "a dry-run create with no tags",
            "CreateEgressOnlyInternetGateway",
            vec![param("VpcId", &vpc_id), param("DryRun", "true")],
        ),
        (
            "a dry-run create that would have assigned tags",
            "CreateEgressOnlyInternetGateway",
            {
                let mut params = vec![param("VpcId", &vpc_id)];
                params.extend(tag_params(&[("Name", "never-stored")]));
                params.push(param("DryRun", "true"));
                params
            },
        ),
        (
            "a dry-run describe",
            "DescribeEgressOnlyInternetGateways",
            vec![param("DryRun", "true")],
        ),
        (
            "a dry-run delete of the stored gateway",
            "DeleteEgressOnlyInternetGateway",
            vec![
                param("EgressOnlyInternetGatewayId", &kept),
                param("DryRun", "true"),
            ],
        ),
    ];
    for (case, action, params) in dry_runs {
        let (status, body) = super::ec2_query::call(&router, REGION, action, &params).await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected HTTP 412, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: expected the dry-run outcome, got body {body}"
        );
        assert_eq!(
            all_ids(&router, case).await,
            vec![kept.clone()],
            "case {case:?}: a dry run must leave the store as it was"
        );
    }
}

/// RULE 8: a delete removes the gateway it names and nothing else, and an id naming no gateway
/// of the account is refused.
#[tokio::test]
async fn a_delete_removes_the_gateway_it_names() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;

    // Rows differing in the tag payload the removed gateway held and in how many gateways the
    // account holds beside it.
    let payloads: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("an untagged gateway", Vec::new()),
        ("a gateway with one tag", vec![("Name", "doomed")]),
        (
            "a gateway with several tags",
            vec![("Name", "doomed"), ("Tier", "web"), ("Note", "")],
        ),
    ];
    let mut kept: Vec<String> = Vec::new();
    for (case, tags) in payloads {
        let doomed = create_gateway_id(&router, case, &vpc_id, &tags).await;
        let neighbour = create_gateway_id(&router, case, &vpc_id, &[("Name", "neighbour")]).await;
        kept.push(neighbour.clone());

        let (status, body) = delete(
            &router,
            &[param("EgressOnlyInternetGatewayId", &doomed)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the delete, got {status} with body {body}"
        );
        assert_eq!(
            member(&body, "returnCode").as_deref(),
            Some("true"),
            "case {case:?}: expected returnCode true, got body {body}"
        );

        // The gateway is gone and every other one is still there.
        let mut expected = kept.clone();
        expected.sort();
        assert_eq!(
            all_ids(&router, case).await,
            expected,
            "case {case:?}: expected exactly the gateways the deletes left, got the store \
             reporting otherwise"
        );

        // Deleting it again refuses: the id names no gateway of the account any more.
        let (status, body) = delete(
            &router,
            &[param("EgressOnlyInternetGatewayId", &doomed)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from the second delete, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidGatewayID.NotFound",
            "case {case:?}: expected the not-found code, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The eigw ID '{doomed}' does not exist"),
            "case {case:?}: expected EC2's wording for an unresolvable id, got body {body}"
        );
    }

    // An id that names no gateway of the account, in each form a client can send one.
    for unknown in [
        "eigw-0123456789abcdef0",
        "eigw-00000000000000000",
        "eigw-12345678",
        "not-an-eigw",
    ] {
        let (status, body) = delete(
            &router,
            &[param("EgressOnlyInternetGatewayId", unknown)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {unknown:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidGatewayID.NotFound",
            "case {unknown:?}: expected the not-found code, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The eigw ID '{unknown}' does not exist"),
            "case {unknown:?}: expected EC2's wording for an unresolvable id, got body {body}"
        );
    }

    // A deleted gateway's `ClientToken` names nothing any more, so the same request mints a
    // new gateway rather than replaying a dangling id.
    let token = "token-after-delete";
    let params = vec![param("VpcId", &vpc_id), param("ClientToken", token)];
    let (status, body) = create(&router, &params).await;
    assert_eq!(status, 200, "the tokened create: got {status}: {body}");
    let minted = created_gateway("the tokened create", &body)
        .egress_only_internet_gateway_id
        .unwrap_or_default();
    let (status, body) = delete(
        &router,
        &[param("EgressOnlyInternetGatewayId", &minted)],
    )
    .await;
    assert_eq!(status, 200, "deleting the tokened gateway: got {status}: {body}");
    let (status, body) = create(&router, &params).await;
    assert_eq!(status, 200, "the replay after the delete: got {status}: {body}");
    let again = created_gateway("the replay after the delete", &body)
        .egress_only_internet_gateway_id
        .unwrap_or_default();
    assert_ne!(
        again, minted,
        "a token whose gateway was deleted must mint a new one, not replay {minted}"
    );
}

/// RULE 9: the gateway is a resource OF its VPC, so the VPC cannot be deleted while it holds
/// one.
#[tokio::test]
async fn a_vpc_holding_an_egress_only_gateway_cannot_be_deleted() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, cidr_block) in [
        ("a /16 VPC", "10.0.0.0/16"),
        ("a /24 VPC", "192.168.7.0/24"),
    ] {
        let vpc_id = create_vpc(case, &router, cidr_block).await;
        let gateway_id = create_gateway_id(&router, case, &vpc_id, &[("Name", "blocking")]).await;

        let (status, body) =
            super::ec2_query::call(&router, REGION, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 deleting a VPC that still holds \
             {gateway_id}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DependencyViolation",
            "case {case:?}: expected the dependency refusal, got body {body}"
        );

        // Once the gateway is gone the VPC goes too.
        let (status, body) = delete(
            &router,
            &[param("EgressOnlyInternetGatewayId", &gateway_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 deleting the gateway, got {status} with body \
             {body}"
        );
        let (status, body) =
            super::ec2_query::call(&router, REGION, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 deleting the VPC once nothing holds it, got \
             {status} with body {body}"
        );
    }
}

/// The `(state, target)` of every route of the table `route_table_id` names, read back through
/// `DescribeRouteTables`.
async fn reported_routes(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> Vec<(Option<String>, Option<String>)> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "routeSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<roxmltree::Node<'_, '_>>>()
        })
        .map(|item| {
            (
                child_text(item, "state"),
                child_text(item, "egressOnlyInternetGatewayId"),
            )
        })
        .collect()
}

/// RULE 10: a route may target the gateway, and goes to `blackhole` when the gateway it points
/// at is deleted.
#[tokio::test]
async fn a_route_can_target_the_gateway_and_blackholes_when_it_is_deleted() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;
    let gateway_id = create_gateway_id(&router, "the gateway", &vpc_id, &[]).await;

    let table_body = ok(
        "the route table",
        &router,
        "CreateRouteTable",
        &[param("VpcId", &vpc_id)],
    )
    .await;
    let route_table_id = {
        let parsed = document("the route table", &table_body);
        parsed
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "routeTableId")
            .and_then(|node| node.text().map(str::to_string))
            .unwrap_or_else(|| panic!("the create answered no routeTableId: {table_body}"))
    };

    // Two IPv6 destinations, so the target is tested over more than one route: "[IPv6 traffic
    // only] The ID of an egress-only internet gateway"
    // (`CreateRouteRequest$EgressOnlyInternetGatewayId`).
    for (case, destination) in [
        ("the default IPv6 route", "::/0"),
        ("one IPv6 block", "2600:1f18:4000::/48"),
    ] {
        let body = ok(
            case,
            &router,
            "CreateRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationIpv6CidrBlock", destination),
                param("EgressOnlyInternetGatewayId", &gateway_id),
            ],
        )
        .await;
        assert!(
            body.contains("<return>true</return>"),
            "case {case:?}: expected the create to report true, got {body}"
        );
    }

    let routes = reported_routes("the routes to the gateway", &router, &route_table_id).await;
    let to_gateway: Vec<&(Option<String>, Option<String>)> = routes
        .iter()
        .filter(|(_, target)| target.as_deref() == Some(gateway_id.as_str()))
        .collect();
    assert_eq!(
        to_gateway.len(),
        2,
        "expected both routes reported pointing at {gateway_id}, got {routes:?}"
    );
    for (state, _) in &to_gateway {
        assert_eq!(
            state.as_deref(),
            Some("active"),
            "expected a route to an attached gateway to be active, got {routes:?}"
        );
    }

    // A route to a gateway that is not there any more is a blackhole, which is what
    // `Route$State` means by it.
    let (status, body) = delete(
        &router,
        &[param("EgressOnlyInternetGatewayId", &gateway_id)],
    )
    .await;
    assert_eq!(status, 200, "deleting the gateway: got {status}: {body}");
    let routes = reported_routes("the routes after the delete", &router, &route_table_id).await;
    let to_gateway: Vec<&(Option<String>, Option<String>)> = routes
        .iter()
        .filter(|(_, target)| target.as_deref() == Some(gateway_id.as_str()))
        .collect();
    assert_eq!(
        to_gateway.len(),
        2,
        "expected both routes still reported after the delete, got {routes:?}"
    );
    for (state, _) in &to_gateway {
        assert_eq!(
            state.as_deref(),
            Some("blackhole"),
            "expected a route to a deleted gateway to be a blackhole, got {routes:?}"
        );
    }

    // And a route cannot be created to a gateway the account does not hold at all.
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "CreateRoute",
        &[
            param("RouteTableId", &route_table_id),
            param("DestinationIpv6CidrBlock", "2600:1f18:5000::/48"),
            param("EgressOnlyInternetGatewayId", "eigw-0123456789abcdef0"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "a route to an unknown gateway: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidGatewayID.NotFound",
        "a route to an unknown gateway: expected the not-found code, got body {body}"
    );
}

/// RULE 11: "you must specify the resource type to tag, otherwise the request will fail"
/// (`TagSpecification`), and the only type this action may name is the resource it creates.
#[tokio::test]
async fn a_tag_specification_for_another_resource_type_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc("the VPC", &router, "10.0.0.0/16").await;

    for other in ["vpc", "internet-gateway", "subnet"] {
        let params = vec![
            param("VpcId", &vpc_id),
            param("TagSpecification.1.ResourceType", other),
            param("TagSpecification.1.Tag.1.Key", "Name"),
            param("TagSpecification.1.Tag.1.Value", "misdirected"),
        ];
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {other:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {other:?}: expected the invalid-value refusal, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "'{other}' is not a valid resource type for tagging on \
                 CreateEgressOnlyInternetGateway"
            ),
            "case {other:?}: expected the wording naming the type and the action, got body {body}"
        );
        assert!(
            all_ids(&router, other).await.is_empty(),
            "case {other:?}: a refused create must store nothing"
        );
    }
}
