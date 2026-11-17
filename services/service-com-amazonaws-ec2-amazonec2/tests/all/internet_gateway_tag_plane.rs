// SPDX-License-Identifier: CC0-1.0

//! The tag plane over an INTERNET GATEWAY id, end to end over the wire the service speaks
//! (`ec2Query`, through the shared [`super::ec2_query`] harness).
//!
//! The rules under test:
//!
//! 1. `CreateTags` takes "the IDs of the resources" of any kind this account holds a taggable
//!    resource of, so an `igw-` id the service itself minted is tagged like any other: the
//!    call answers HTTP 200 with the `<return>true</return>` success envelope its `Unit`
//!    output serialises to, the tags it wrote are merged over the ones the create assigned,
//!    and the write reads back through the gateway's OWN describe, through the
//!    `tag:<key>` / `tag-key` filters of that describe, and through `DescribeTags` under the
//!    `internet-gateway` `ResourceType`.
//! 2. `DeleteTags` resolves the SAME id — "if you omit this parameter, we delete all
//!    user-defined tags for the specified resources" (`DeleteTagsRequest$Tags`), and a request
//!    naming keys removes only those — answering the same 200 `<return>true</return>`
//!    envelope, and only the gateway the request names changes.
//! 3. An internet-gateway id this account holds no gateway for — one `DeleteInternetGateway`
//!    removed, or one the service never minted — is refused by BOTH tagging calls, and the
//!    one a delete removed is refused by `DescribeInternetGateways` too, with the gateway
//!    kind's own `InvalidInternetGatewayID.NotFound` (EC2 API "Error codes") and the sentence
//!    `The internetGateway ID '<id>' does not exist`, built from the id the request carried —
//!    whatever that id spells, so the envelope carrying it stays well-formed XML; nothing of
//!    the refused request is written.
//!
//! Each table row is a distinct input along what those rules generalise over — how many tags
//! a request carries and what they spell, whether the gateway is attached to a VPC, whether
//! the gateway already carried tags from its create, whether a removal names keys or none,
//! and what makes an id unresolvable — so no single recorded request is the specification.
//! The sibling suite `taggable_resource_kinds` asserts the same two rules over the other
//! resource kinds, and `internet_gateways` pins what one gateway's record carries.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    element, envelope_children, error_code, error_message, filters, indexed, tag_report,
    tag_specification, tagging, DescribedTag,
};
use super::wire::{child_text, document, param, set_items};

use ec2::state::INTERNET_GATEWAY_TAGGABLE_RESOURCE;

/// The Region every call here is signed for. An internet gateway carries no Region member, so
/// the Region only picks which account's ambient network the store already holds.
const REGION: &str = "us-east-2";

/// EC2's code for an internet-gateway id that names no gateway of the account (EC2 API
/// "Error codes"), written out here rather than read from the crate so the assertion names
/// the wire value a client sees.
const NOT_FOUND_CODE: &str = "InvalidInternetGatewayID.NotFound";

/// One `action` call carrying `params`, as its HTTP status and response body.
async fn call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// The emulator the suite drives, one store per test row.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One internet gateway, created over the wire carrying `tags`, as its `igw-` id.
async fn create_gateway(router: &MultiServiceRouter, case: &str, tags: &[(&str, &str)]) -> String {
    let params = tag_specification(INTERNET_GATEWAY_TAGGABLE_RESOURCE, tags);
    let (status, body) = call(router, "CreateInternetGateway", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateInternetGateway must succeed, got {status} with body {body}"
    );
    element(&body, "internetGatewayId")
        .unwrap_or_else(|| panic!("case {case:?}: the create answered no internetGatewayId: {body}"))
}

/// One VPC, as the network a gateway attaches to.
async fn create_vpc(router: &MultiServiceRouter, case: &str, cidr: &str) -> String {
    let (status, body) = call(router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVpc must succeed, got {status} with body {body}"
    );
    element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: the create answered no vpcId: {body}"))
}

/// Attach `internet_gateway_id` to a VPC of its own, which must succeed.
async fn attach_to_a_vpc(router: &MultiServiceRouter, case: &str, internet_gateway_id: &str) {
    let vpc_id = create_vpc(router, case, "10.9.0.0/16").await;
    let (status, body) = call(
        router,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", internet_gateway_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: attaching {internet_gateway_id} to {vpc_id} must succeed, got {status} \
         with body {body}"
    );
}

/// The tags `DescribeInternetGateways` reports for the one gateway `internet_gateway_id`
/// names, as key -> value — the round trip every assertion about a write reads through.
async fn gateway_tags(
    router: &MultiServiceRouter,
    case: &str,
    internet_gateway_id: &str,
) -> BTreeMap<String, String> {
    let (status, body) = call(
        router,
        "DescribeInternetGateways",
        &indexed("InternetGatewayId", &[internet_gateway_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {internet_gateway_id} back must succeed, got {status} with body \
         {body}"
    );
    let parsed = document(case, &body);
    let items = set_items(parsed.root_element(), "internetGatewaySet");
    let item = items
        .into_iter()
        .find(|item| {
            child_text(*item, "internetGatewayId").as_deref() == Some(internet_gateway_id)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {internet_gateway_id} in the report: {body}")
        });
    set_items(item, "tagSet")
        .into_iter()
        .map(|tag| {
            (
                child_text(tag, "key").unwrap_or_default(),
                child_text(tag, "value").unwrap_or_default(),
            )
        })
        .collect()
}

/// The gateway ids `DescribeInternetGateways` selects for `entries`, in report order.
async fn ids_selected_by(
    router: &MultiServiceRouter,
    case: &str,
    entries: &[(&str, &[&str])],
) -> Vec<String> {
    let (status, body) = call(router, "DescribeInternetGateways", &filters(entries)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the filtered describe must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "internetGatewaySet")
        .into_iter()
        .filter_map(|item| child_text(item, "internetGatewayId"))
        .collect()
}

/// `(key, value)` pairs as the owned map an assertion compares.
fn owned_tags(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The `DescribeTags` rows the gateway `internet_gateway_id` must report for `tags`.
fn expected_report(
    internet_gateway_id: &str,
    tags: &BTreeMap<String, String>,
) -> BTreeSet<DescribedTag> {
    tags.iter()
        .map(|(key, value)| {
            (
                internet_gateway_id.to_string(),
                INTERNET_GATEWAY_TAGGABLE_RESOURCE.to_string(),
                key.clone(),
                value.clone(),
            )
        })
        .collect()
}

/// Assert that `action` answered the `requestId` + `return=true` success envelope EC2 serves
/// for a tagging call, whose modelled output is `Unit`.
fn assert_tagging_succeeded(case: &str, action: &str, status: u16, body: &str) {
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        envelope_children(case, body, action, "return"),
        vec!["true".to_string()],
        "case {case:?}: expected {action}Response to carry return=true, got {body}"
    );
    assert!(
        envelope_children(case, body, action, "requestId")
            .first()
            .is_some_and(|request_id| !request_id.is_empty()),
        "case {case:?}: expected {action}Response to carry a requestId, got {body}"
    );
}

/// One row of the tagging table.
struct Row {
    /// What the row is called in the assertions.
    case: &'static str,
    /// The tags the gateway's CREATE assigns, which a later write merges over.
    created_with: &'static [(&'static str, &'static str)],
    /// The tags `CreateTags` then writes.
    written: &'static [(&'static str, &'static str)],
    /// Whether the gateway is attached to a VPC before it is tagged — a gateway's tags are no
    /// function of its attachment, so both forms must behave the same.
    attached: bool,
}

/// The rows differ along what rule 1 generalises over: how many tags a request carries, what
/// the keys and values spell (an empty value, two keys differing only in case, a key the
/// create already assigned, a non-ASCII value), whether the gateway already carried tags, and
/// whether a VPC is attached.
const ROWS: [Row; 6] = [
    Row {
        case: "one tag on an untagged, unattached gateway",
        created_with: &[],
        written: &[("Name", "edge")],
        attached: false,
    },
    Row {
        case: "a tag with an empty value beside one whose key differs only in case",
        created_with: &[],
        written: &[("env", ""), ("Env", "staging")],
        attached: false,
    },
    Row {
        case: "three tags on a gateway attached to a VPC",
        created_with: &[],
        written: &[("Name", "prod-edge"), ("owner", "network"), ("tier", "public")],
        attached: true,
    },
    Row {
        case: "a write that overwrites one create-time tag and adds another",
        created_with: &[("Name", "minted"), ("keep", "me")],
        written: &[("Name", "renamed"), ("added", "later")],
        attached: false,
    },
    Row {
        case: "a non-ASCII value on a gateway that already carries a tag",
        created_with: &[("stage", "ready")],
        written: &[("owner", "réseau-équipe")],
        attached: true,
    },
    Row {
        // The value a caller sends reaches the wire inside an XML element, so a value
        // carrying the markup characters must come back as it was sent rather than as
        // markup the response invents.
        case: "a value and a key carrying XML metacharacters",
        created_with: &[],
        written: &[("note<tag>", "a<b> & c'd\"e")],
        attached: false,
    },
];

/// RULE 1: `CreateTags` tags an internet-gateway id and the write reads back through the
/// gateway's own describe, its tag filters and `DescribeTags`.
#[tokio::test]
async fn create_tags_writes_onto_an_internet_gateway_and_the_write_reads_back() {
    for row in &ROWS {
        let case = row.case;
        let router = emulator();
        let internet_gateway_id = create_gateway(&router, case, row.created_with).await;
        // A second gateway the request never names, so every assertion below is about the
        // gateway the id named rather than about every gateway the account holds.
        let untouched_id = create_gateway(&router, case, &[]).await;
        if row.attached {
            attach_to_a_vpc(&router, case, &internet_gateway_id).await;
        }

        let (status, body) = call(
            &router,
            "CreateTags",
            &tagging(&internet_gateway_id, row.written),
        )
        .await;
        assert_tagging_succeeded(case, "CreateTags", status, &body);

        // "When you specify an existing tag key, the value is overwritten with the new value"
        // (`CreateTags`): the create-time tags stay, except where a key is written again.
        let mut expected = owned_tags(row.created_with);
        expected.extend(owned_tags(row.written));

        let reported = gateway_tags(&router, case, &internet_gateway_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the gateway to report {expected:?}, got {reported:?}"
        );

        let report = tag_report(&router, REGION, case, &internet_gateway_id).await;
        let wanted = expected_report(&internet_gateway_id, &expected);
        assert_eq!(
            report, wanted,
            "case {case:?}: expected DescribeTags to report {wanted:?}, got {report:?}"
        );

        // The `tag:<key>` and `tag-key` filters of the gateway's own describe match on what
        // the tag plane wrote, so a client can find the gateway by the Name it just set.
        for &(key, value) in row.written {
            // A filter value is matched only where the request can carry one: an ec2Query
            // body gives no `Filter.N.Value.1` for an empty value, so a tag stored with one
            // is asserted through `tag-key` alone.
            if !value.is_empty() {
                let name = format!("tag:{key}");
                let by_value = ids_selected_by(&router, case, &[(name.as_str(), &[value])]).await;
                assert_eq!(
                    by_value,
                    vec![internet_gateway_id.clone()],
                    "case {case:?}: expected tag:{key}={value:?} to select only \
                     {internet_gateway_id}, got {by_value:?}"
                );
            }
            let by_key = ids_selected_by(&router, case, &[("tag-key", &[key])]).await;
            assert_eq!(
                by_key,
                vec![internet_gateway_id.clone()],
                "case {case:?}: expected tag-key={key:?} to select only {internet_gateway_id}, \
                 got {by_key:?}"
            );
        }

        let untouched = gateway_tags(&router, case, &untouched_id).await;
        assert!(
            untouched.is_empty(),
            "case {case:?}: expected {untouched_id} to carry no tag, got {untouched:?}"
        );
    }
}

/// RULE 2: `DeleteTags` resolves the same internet-gateway id, removing the keys it names and
/// every user-defined tag when it names none.
#[tokio::test]
async fn delete_tags_removes_from_an_internet_gateway_the_keys_it_names() {
    for row in &ROWS {
        let case = row.case;
        let router = emulator();
        let internet_gateway_id = create_gateway(&router, case, row.created_with).await;
        let untouched_id = create_gateway(&router, case, &[("Name", "spare")]).await;
        if row.attached {
            attach_to_a_vpc(&router, case, &internet_gateway_id).await;
        }

        let (status, body) = call(
            &router,
            "CreateTags",
            &tagging(&internet_gateway_id, row.written),
        )
        .await;
        assert_tagging_succeeded(case, "CreateTags", status, &body);
        let mut expected = owned_tags(row.created_with);
        expected.extend(owned_tags(row.written));

        // One key named, with no value: "we delete any tag with this key regardless of its
        // value" (`DeleteTagsRequest$Tags`) — and only that key's tag.
        let (removed_key, _) = row.written[0];
        let (status, body) = call(
            &router,
            "DeleteTags",
            &[
                param("ResourceId.1", &internet_gateway_id),
                param("Tag.1.Key", removed_key),
            ],
        )
        .await;
        assert_tagging_succeeded(case, "DeleteTags", status, &body);
        expected.remove(removed_key);
        let reported = gateway_tags(&router, case, &internet_gateway_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?} left after removing {removed_key:?}, got \
             {reported:?}"
        );

        // No tag named at all: "if you omit this parameter, we delete all user-defined tags
        // for the specified resources" (`DeleteTagsRequest$Tags`).
        let (status, body) = call(
            &router,
            "DeleteTags",
            &[param("ResourceId.1", &internet_gateway_id)],
        )
        .await;
        assert_tagging_succeeded(case, "DeleteTags", status, &body);
        let reported = gateway_tags(&router, case, &internet_gateway_id).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: expected no tag left on {internet_gateway_id}, got {reported:?}"
        );
        let report = tag_report(&router, REGION, case, &internet_gateway_id).await;
        assert!(
            report.is_empty(),
            "case {case:?}: expected DescribeTags to report nothing for \
             {internet_gateway_id}, got {report:?}"
        );

        // Only the gateway the requests named changed.
        let untouched = gateway_tags(&router, case, &untouched_id).await;
        assert_eq!(
            untouched,
            owned_tags(&[("Name", "spare")]),
            "case {case:?}: expected {untouched_id} to keep its own tag, got {untouched:?}"
        );
    }
}

/// RULE 3's envelope half: the id the request carried is reflected into the refusal's
/// `Message`, and the envelope that carries it is still well-formed XML whatever the id
/// spells — `InternetGatewayId` publishes no pattern, so a client can send one carrying the
/// markup characters.
#[tokio::test]
async fn an_unresolvable_gateway_id_is_reflected_into_a_well_formed_refusal() {
    // Each row carries characters that are markup in the response's own XML envelope, so an
    // id that reached the `Message` unescaped would break the document a client parses.
    let ids = ["igw-0&<\"x", "igw-<item>0", "igw-'0123456789abcdef0'"];
    for absent_id in ids {
        let case = format!("the unresolvable id {absent_id:?}");
        let router = emulator();
        let (status, body) = call(
            &router,
            "DescribeInternetGateways",
            &indexed("InternetGatewayId", &[absent_id]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        // Parsing the body is the assertion that the envelope survived the id: a refusal is
        // rooted in `<Response>` whatever it refuses.
        let parsed = document(&case, &body);
        assert_eq!(
            parsed.root_element().tag_name().name(),
            "Response",
            "case {case:?}: expected the refusal envelope's root, got {body}"
        );
        // WHICH refusal an id of this spelling earns is not what this row pins — only that
        // the id the request carried comes back inside the message as it was sent, so the
        // document a client parses survives whatever the id spells.
        let message = error_message(&body);
        assert!(
            message.contains(absent_id),
            "case {case:?}: expected the id back as the request spelled it, got {message:?} in \
             {body}"
        );
    }
}

/// What makes the id of an unresolvable-id row name no gateway of the account.
#[derive(Debug, Clone, Copy)]
enum Absent {
    /// A gateway this account held, removed by `DeleteInternetGateway`.
    Deleted,
    /// An id of the long form the service mints that it never minted.
    NeverMinted(&'static str),
}

/// RULE 3: an internet-gateway id naming no gateway of the account is refused by both tagging
/// calls and by the gateway's own describe, with the gateway kind's not-found refusal, and
/// nothing of the refused request is written.
#[tokio::test]
async fn an_internet_gateway_id_naming_nothing_is_refused_by_the_whole_tag_plane() {
    let absent_ids: [Absent; 3] = [
        Absent::Deleted,
        Absent::NeverMinted("igw-00000000000000999"),
        Absent::NeverMinted("igw-0123456789abcdef0"),
    ];
    for absent in absent_ids {
        // The describe half is driven for the gateway this row DELETED, which is what the
        // suites beside this one leave unasserted: `internet_gateways` already pins the
        // refusal for an id the service never minted, and `delete_internet_gateway` pins the
        // status after a delete without its code or its sentence.
        let actions: &[&str] = match absent {
            Absent::Deleted => &["CreateTags", "DeleteTags", "DescribeInternetGateways"],
            Absent::NeverMinted(_) => &["CreateTags", "DeleteTags"],
        };
        for action in actions.iter().copied() {
            let case = format!("{absent:?} named by {action}");
            let router = emulator();
            // A gateway of the kind EXISTS and carries a tag, so each refusal below is about
            // the id the request named, and the tag it holds is what shows nothing was
            // written.
            let held_id = create_gateway(&router, &case, &[("Name", "held")]).await;

            let absent_id = match absent {
                Absent::NeverMinted(id) => id.to_string(),
                Absent::Deleted => {
                    let doomed = create_gateway(&router, &case, &[("Name", "doomed")]).await;
                    let (status, body) = call(
                        &router,
                        "DeleteInternetGateway",
                        &[param("InternetGatewayId", &doomed)],
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: deleting {doomed} must succeed, got {status} with body \
                         {body}"
                    );
                    doomed
                }
            };
            assert_ne!(
                absent_id, held_id,
                "case {case:?}: the absent id must name no gateway this row created"
            );

            let params = match action {
                "DescribeInternetGateways" => indexed("InternetGatewayId", &[absent_id.as_str()]),
                _ => tagging(&absent_id, &[("env", "prod")]),
            };
            let (status, body) = call(&router, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for {absent_id}, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                NOT_FOUND_CODE,
                "case {case:?}: expected {NOT_FOUND_CODE} for {absent_id}, got body {body}"
            );
            assert_eq!(
                error_message(&body),
                format!("The internetGateway ID '{absent_id}' does not exist"),
                "case {case:?}: expected EC2's wording for an unresolvable gateway id, got body \
                 {body}"
            );

            // The refused call wrote nothing: the gateway the account does hold keeps exactly
            // the tag its create assigned, and the deleted gateway's tags are gone from the
            // tag report.
            let held = gateway_tags(&router, &case, &held_id).await;
            assert_eq!(
                held,
                owned_tags(&[("Name", "held")]),
                "case {case:?}: expected {held_id} to keep its own tag, got {held:?}"
            );
            let report = tag_report(&router, REGION, &case, &absent_id).await;
            assert!(
                report.is_empty(),
                "case {case:?}: expected DescribeTags to report nothing for {absent_id}, got \
                 {report:?}"
            );
        }
    }
}
