// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ELEMENTS an internet gateway reaches the wire with: the record
//! `CreateInternetGateway` answers, and the same record as a report of it carries. Driven
//! over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=…`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a reported internet gateway carries every element the live wire
//! renders on that shape** — the four members the model declares on
//! `com.amazonaws.ec2#InternetGateway` (`smithy_query(query="shape",shape="InternetGateway")`:
//! `attachmentSet`, `internetGatewayId`, `ownerId`, `tagSet`) AND the `association` element
//! the recorded `CreateInternetGateway` answer carries beside them, present and EMPTY for a
//! gateway no association has been made for — which is every gateway this service holds,
//! nothing here making one ("after creating the internet gateway, you attach it to a VPC
//! using AttachInternetGateway", `CreateInternetGateway`, and an ATTACHMENT is the record's
//! own `attachmentSet` member). The element is a fact of the SHAPE rather than of one
//! handler, so the create's `internetGateway` and every item of a describe's
//! `internetGatewaySet` carry it alike, and the record itself is untouched: the gateway a
//! create answered reads back through `DescribeInternetGateways` element for element.
//!
//! Every row is a distinct input along what that rule generalises over — how many tags the
//! gateway carries and what they spell, how large the account's estate already is when the
//! create runs, whether the gateway is afterwards attached to a VPC, and whether the answer
//! is a create's single element or an item of a report — so no single recorded request is
//! the specification. The sibling suite `internet_gateways` pins what the record holds and
//! how a report narrows; this one pins the element set of the record on the wire.

use ec2::state::{
    INTERNET_GATEWAY_ID_PREFIX, INTERNET_GATEWAY_TAGGABLE_RESOURCE, MAX_TAGS_PER_RESOURCE,
    MAX_TAG_KEY_CHARS, MAX_TAG_VALUE_CHARS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::indexed;
use super::wire::{child, child_text, document, is_long_id, param, set_items, Param};

/// The Region every call here is signed for. An internet gateway carries no Region member,
/// so the Region only picks which account's store the calls land in.
const REGION: &str = "us-east-1";

/// The element a reported gateway carries beside its modelled members.
const ASSOCIATION: &str = "association";

/// What a gateway's `association` element must be: present once, and empty.
fn empty_association() -> Vec<String> {
    vec![String::new()]
}

/// One internet gateway as a response reports it, element by element.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedGateway {
    internet_gateway_id: Option<String>,
    owner_id: Option<String>,
    /// Whether the record carried the `attachmentSet` container at all — told apart from one
    /// present and empty, which is what an unattached gateway reports.
    attachment_set_present: bool,
    attachments: Vec<(Option<String>, Option<String>)>,
    tags: Vec<(String, String)>,
    /// The text of every `association` element the record carried: an empty vector for a
    /// record that carried none at all, which is what this suite tells apart from one
    /// present and empty.
    associations: Vec<String>,
}

/// The gateway one `<internetGateway>` or `<internetGatewaySet>` item reports.
fn gateway_from(node: roxmltree::Node<'_, '_>) -> ReportedGateway {
    ReportedGateway {
        internet_gateway_id: child_text(node, "internetGatewayId"),
        owner_id: child_text(node, "ownerId"),
        attachment_set_present: child(node, "attachmentSet").is_some(),
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
        associations: node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == ASSOCIATION)
            .map(|child| child.text().unwrap_or_default().to_string())
            .collect(),
    }
}

/// The one gateway a `CreateInternetGateway` response carries.
fn created_gateway(case: &str, body: &str) -> ReportedGateway {
    let parsed = document(case, body);
    let node = child(parsed.root_element(), "internetGateway")
        .unwrap_or_else(|| panic!("case {case:?}: expected an <internetGateway> in {body}"));
    gateway_from(node)
}

/// The gateways a `DescribeInternetGateways` response carries, in the order it reported them.
fn described_gateways(case: &str, body: &str) -> Vec<ReportedGateway> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "internetGatewaySet")
        .into_iter()
        .map(gateway_from)
        .collect()
}

/// The `TagSpecifications` form fields that assign `tags` to the gateway a create mints, as
/// ec2Query flattens them — and nothing at all for a request that assigns none.
fn tag_params(tags: &[(String, String)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        INTERNET_GATEWAY_TAGGABLE_RESOURCE,
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

/// One `CreateInternetGateway` call carrying `params`, as its status and body.
async fn create(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, "CreateInternetGateway", params).await
}

/// One create that has to succeed, and the body it answered.
async fn create_ok(router: &MultiServiceRouter, case: &str, params: &[Param]) -> String {
    let (status, body) = create(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateInternetGateway, got {status} with body \
         {body}"
    );
    body
}

/// The one gateway `internet_gateway_id` names, read back through the describe, and the body
/// it was read out of.
async fn read_back(
    router: &MultiServiceRouter,
    case: &str,
    internet_gateway_id: &str,
) -> (ReportedGateway, String) {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeInternetGateways",
        &indexed("InternetGatewayId", &[internet_gateway_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading {internet_gateway_id} back, got {status} \
         with body {body}"
    );
    let reported = described_gateways(case, &body);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly the gateway asked for, got {reported:?}"
    );
    let gateway = reported
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a one-element report has a first element"));
    (gateway, body)
}

/// One row of the create table: the tags a request assigns to the gateway it creates.
#[derive(Debug, Clone)]
struct GatewayCase {
    case: &'static str,
    tags: Vec<(String, String)>,
}

/// The create table: one row per kind of tag payload a caller can assign, spanning the
/// count and the size of what a created gateway holds. The published tag bounds are this
/// crate's shared ones (`MAX_TAGS_PER_RESOURCE`, `MAX_TAG_KEY_CHARS`,
/// `MAX_TAG_VALUE_CHARS`), so the last two rows sit exactly on them — the element set of
/// the answer is the same whichever payload the gateway carries.
fn gateway_cases() -> Vec<GatewayCase> {
    let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
    vec![
        GatewayCase {
            case: "no tag specification at all",
            tags: Vec::new(),
        },
        GatewayCase {
            case: "one tag",
            tags: vec![pair("Name", "border")],
        },
        GatewayCase {
            case: "three tags, one of them with an empty value",
            tags: vec![pair("Name", "beta"), pair("Tier", "edge"), pair("Note", "")],
        },
        GatewayCase {
            case: "a value carrying spaces and punctuation",
            tags: vec![pair("Project.Name", "border gateway (us-east)")],
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

/// RULE: every answer that reports a gateway carries an `association` element that is present
/// and empty, beside the four modelled members — whatever tag payload the gateway holds and
/// however many gateways the account already held.
#[tokio::test]
async fn every_reported_gateway_carries_a_present_and_empty_association_element() {
    let (router, _engine) = super::ec2_query::emulator();
    let account = engine::emulator_account_id().to_string();

    for row in gateway_cases() {
        let case = row.case;
        let body = create_ok(&router, case, &tag_params(&row.tags)).await;

        let answered = created_gateway(case, &body);
        assert_eq!(
            answered.associations,
            empty_association(),
            "case {case:?}: expected exactly one <{ASSOCIATION}> element, present and empty, \
             got {:?} in {body}",
            answered.associations
        );

        // The element is an ADDITION: every modelled member still reports what the record
        // holds, so a client reading the answer sees the whole gateway.
        let internet_gateway_id = answered
            .internet_gateway_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the create answered no internetGatewayId"));
        assert!(
            is_long_id(INTERNET_GATEWAY_ID_PREFIX, &internet_gateway_id),
            "case {case:?}: expected an {INTERNET_GATEWAY_ID_PREFIX}-<17 hex digits> id, got \
             {internet_gateway_id:?}"
        );
        assert_eq!(
            answered.owner_id.as_deref(),
            Some(account.as_str()),
            "case {case:?}: expected the account that owns the gateway, got {:?}",
            answered.owner_id
        );
        assert!(
            answered.attachment_set_present && answered.attachments.is_empty(),
            "case {case:?}: expected an attachmentSet present and empty on a freshly created \
             gateway, got present={} with {:?}",
            answered.attachment_set_present,
            answered.attachments
        );
        assert_eq!(
            answered.tags,
            row.tags,
            "case {case:?}: expected the tags the request assigned, got {:?}",
            answered.tags
        );

        // ROUND TRIP, and the other answer this rule reaches: the record the create answered
        // is the record a report of it carries — the same modelled members AND the same
        // `association` element, the element being a fact of the shape rather than of the
        // create.
        let (described, described_body) = read_back(&router, case, &internet_gateway_id).await;
        assert_eq!(
            described, answered,
            "case {case:?}: expected the create's gateway to read back element for element, \
             got {described:?} in {described_body}"
        );
    }
}

/// RULE: the element reports the association the gateway is under, which is none however the
/// gateway is used afterwards — an ATTACHMENT to a VPC is the record's `attachmentSet`
/// member, so attaching one changes that member and leaves the association empty.
#[tokio::test]
async fn the_association_element_is_empty_however_the_gateway_is_used_afterwards() {
    let (router, _engine) = super::ec2_query::emulator();

    // A VPC to attach to, so the row that attaches has a network of its own rather than the
    // account's default one.
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "CreateVpc",
        &[param("CidrBlock", "10.11.0.0/16")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the VPC an attachment needs must be created, got {status} with body {body}"
    );
    let vpc_id = {
        let parsed = document("the VPC an attachment needs", &body);
        child(parsed.root_element(), "vpc")
            .and_then(|vpc| child_text(vpc, "vpcId"))
            .unwrap_or_else(|| panic!("the create answered no vpcId: {body}"))
    };

    // Row 1: a gateway left exactly as the create made it.
    let case = "a gateway nothing was done to";
    let untouched = create_ok(&router, case, &[]).await;
    assert_eq!(
        created_gateway(case, &untouched).associations,
        empty_association(),
        "case {case:?}: expected one empty <{ASSOCIATION}>, got {untouched}"
    );

    // Row 2: a gateway attached to a VPC after its create.
    let case = "a gateway attached afterwards";
    let attached_body = create_ok(&router, case, &[]).await;
    let created = created_gateway(case, &attached_body);
    assert_eq!(
        created.associations,
        empty_association(),
        "case {case:?}: expected one empty <{ASSOCIATION}> at create time, got {attached_body}"
    );
    let internet_gateway_id = created
        .internet_gateway_id
        .unwrap_or_else(|| panic!("case {case:?}: the create answered no internetGatewayId"));
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: attaching {internet_gateway_id} to {vpc_id} must succeed, got {status} \
         with body {body}"
    );

    // The attachment is reported where the model puts it — on the record's `attachmentSet` —
    // and the association element is still there and still empty: attaching a VPC is not
    // making an association.
    let (described, described_body) = read_back(&router, case, &internet_gateway_id).await;
    assert_eq!(
        described.attachments.len(),
        1,
        "case {case:?}: the attached gateway must report its one attachment, got {described:?} \
         in {described_body}"
    );
    assert_eq!(
        described.attachments[0].1.as_deref(),
        Some(vpc_id.as_str()),
        "case {case:?}: the attachment must name the VPC it was made to, got {:?}",
        described.attachments[0]
    );
    assert_eq!(
        described.associations,
        empty_association(),
        "case {case:?}: an attached gateway must still report one empty <{ASSOCIATION}>, got \
         {described_body}"
    );
}

/// RULE: the element is a fact of every reported gateway, not of the first one — a report
/// carrying many gateways carries it on each item, and each create still mints a gateway of
/// its own.
#[tokio::test]
async fn every_item_of_a_report_carries_the_element() {
    let (router, _engine) = super::ec2_query::emulator();

    // Enough creates to span "the first", "one of many" and "the last of a run", kept small
    // so the suite stays fast.
    const CREATES: usize = 9;

    let mut minted: Vec<String> = Vec::new();
    for index in 0..CREATES {
        let case = format!("create number {} of {CREATES}", index + 1);
        let body = create_ok(&router, &case, &[]).await;
        let created = created_gateway(&case, &body);
        assert_eq!(
            created.associations,
            empty_association(),
            "case {case:?}: expected one <{ASSOCIATION}> element, present and empty, got {body}"
        );
        let internet_gateway_id = created
            .internet_gateway_id
            .unwrap_or_else(|| panic!("case {case:?}: the create answered no internetGatewayId"));
        assert!(
            !minted.contains(&internet_gateway_id),
            "case {case:?}: each create mints a gateway of its own, but {internet_gateway_id} \
             was answered twice (already had {minted:?})"
        );
        minted.push(internet_gateway_id);
    }

    // The whole estate in one answer: every item carries the element, the ambient gateway the
    // account's default VPC comes with included.
    let case = "every gateway the account holds";
    let (status, body) =
        super::ec2_query::call(&router, REGION, "DescribeInternetGateways", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the unfiltered report, got {status} with body \
         {body}"
    );
    let reported = described_gateways(case, &body);
    assert!(
        reported.len() >= CREATES,
        "case {case:?}: the report must carry at least the {CREATES} gateways created here, \
         got {reported:?}"
    );
    for gateway in &reported {
        assert_eq!(
            gateway.associations,
            empty_association(),
            "case {case:?}: every reported gateway must carry one empty <{ASSOCIATION}>, got \
             {gateway:?} in {body}"
        );
    }
}
