// SPDX-License-Identifier: CC0-1.0

//! The State Manager associations in EFFECT on ONE managed node, end to end:
//! `DescribeEffectiveInstanceAssociations` reporting the associations `CreateAssociation`
//! persisted together with the association document each of them applies.
//!
//! RULE: the report answers, for the node it names, one entry per association of the Region
//! the request was signed for which applies to that node — the association was created FOR
//! the node (the deprecated `InstanceId` member), its latest run was dispatched to the node,
//! or its targets select it — each entry carrying that association's id, the node the request
//! asked about, the association version in force and the content of the document version that
//! version applies; a well-formed node id no association applies to is reported as applying
//! none rather than refused; and the entries are served in the store's own ascending
//! association-id order in pages of at most `MaxResults`, where the `NextToken` is the id the
//! next page starts at and a token shaped like no association id is `InvalidNextToken`.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention and one reading of the store with the create, listing and status suites.
//!
//! The content an entry carries is asserted against what `GetDocument` reports for the same
//! document and version — the document store's own answer — rather than against a body
//! written out here, so the two readers cannot be made to agree by a literal.
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every
//! node describe reads), because a node enters it only by REGISTERING and no public API of
//! this service registers one.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! which targeting form selects the node, which document (published, created, JSON, YAML) the
//! association applies, which document version it names, how big a page is and which shapes a
//! resume token can take — rather than the single report the recorded unit happened to take.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    associations, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, call_in_region, created, error_code, service, AGENT, NODE, OTHER_NODE, REGION, SHELL,
};

/// A hybrid node of [`REGION`] (`mi-` and 17 characters, as the modelled `InstanceId` pattern
/// spells an on-premises node), so the table spans node kind.
const HYBRID: &str = "mi-01234567890123456";

/// The tag key every seeded node carries a value for, so a tag target selects a subset of the
/// inventory rather than all of it.
const TAG_KEY: &str = "Env";

/// The tag value [`NODE`] and [`HYBRID`] carry.
const PROD: &str = "prod";

/// The tag value [`OTHER_NODE`] carries.
const DEV: &str = "dev";

/// A well-formed node id nothing registered and no association names (the shorter form the
/// modelled `InstanceId` pattern admits).
const UNKNOWN: &str = "i-0dddeeee";

/// The nodes of [`REGION`] every row of the targeting table is read against.
const SEEDED: &[&str] = &[NODE, OTHER_NODE, HYBRID];

/// One registered node: its id, kind and the one tag a target narrows on.
fn node(id: &str, resource_type: &str, tag_value: &str) -> ManagedNodeRecord {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, resource_type, at);
    record.properties.ping_status = Some("Online".to_string());
    record.tags = [(TAG_KEY.to_string(), tag_value.to_string())]
        .into_iter()
        .collect::<BTreeMap<String, String>>();
    record
}

/// Register `record` in [`REGION`] the way a node's own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, record: ManagedNodeRecord) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// The inventory every suite below reports against: two EC2 nodes and one hybrid node in
/// [`REGION`], spanning node kind and tag value.
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    register(engine, node(NODE, RESOURCE_TYPE_EC2_INSTANCE, PROD));
    register(engine, node(OTHER_NODE, RESOURCE_TYPE_EC2_INSTANCE, DEV));
    register(engine, node(HYBRID, RESOURCE_TYPE_MANAGED_INSTANCE, PROD));
}

/// `DescribeEffectiveInstanceAssociations` with `body`, signed for [`REGION`].
async fn report(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(
        router,
        REGION,
        "DescribeEffectiveInstanceAssociations",
        body,
    )
    .await
}

/// The whole response of a report that must have succeeded.
async fn reported(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = report(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeEffectiveInstanceAssociations, got \
         {status} with {parsed:?}"
    );
    parsed
}

/// The `Associations` a response carries.
fn entries(response: &Value) -> Vec<Value> {
    response["Associations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `Associations` a report of `instance_id` carries.
async fn effective(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Vec<Value> {
    let response = reported(router, case, &json!({"InstanceId": instance_id})).await;
    entries(&response)
}

/// The `AssociationId`s a response reports, in the order it reported them.
fn ids(response: &Value) -> Vec<String> {
    entries(response)
        .iter()
        .map(|entry| {
            entry["AssociationId"]
                .as_str()
                .unwrap_or("<unset>")
                .to_string()
        })
        .collect()
}

/// A string member of an entry.
fn member(entry: &Value, name: &str) -> String {
    entry[name].as_str().unwrap_or("<unset>").to_string()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// The `AssociationId` a create reported.
fn association_id(description: &Value) -> String {
    description["AssociationId"]
        .as_str()
        .unwrap_or("<unset>")
        .to_string()
}

/// The `Content` `GetDocument` reports for one document and version selector — the document
/// store's own answer, which an entry's content has to agree with.
async fn document_content(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    document_version: Option<&str>,
) -> String {
    let mut body = json!({"Name": name});
    if let Some(version) = document_version {
        body["DocumentVersion"] = json!(version);
    }
    let (status, parsed) = call(router, "GetDocument", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetDocument {body} must succeed, got {status} with {parsed:?}"
    );
    parsed["Content"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetDocument reports Content, got {parsed:?}"))
        .to_string()
}

/// One create, and which of the seeded nodes the association it persists applies to.
struct Targeting {
    case: &'static str,
    request: Value,
    applies_to: &'static [&'static str],
}

/// One row per targeting form a create can take, along the axis "which nodes is this
/// association in effect on".
fn targetings() -> Vec<Targeting> {
    vec![
        Targeting {
            case: "the node named through the deprecated InstanceId member",
            request: json!({"Name": AGENT, "InstanceId": NODE}),
            applies_to: &[NODE],
        },
        Targeting {
            case: "one node id target",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [OTHER_NODE]}],
            }),
            applies_to: &[OTHER_NODE],
        },
        Targeting {
            case: "two node ids in one target",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE, HYBRID]}],
            }),
            applies_to: &[NODE, HYBRID],
        },
        Targeting {
            case: "a target selecting every node of the Region",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": ["*"]}],
            }),
            applies_to: SEEDED,
        },
        Targeting {
            case: "a tag target, which selects the nodes carrying that tag value",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": format!("tag:{TAG_KEY}"), "Values": [PROD]}],
            }),
            applies_to: &[NODE, HYBRID],
        },
        Targeting {
            case: "a tag target carrying the other value",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": format!("tag:{TAG_KEY}"), "Values": [DEV]}],
            }),
            applies_to: &[OTHER_NODE],
        },
        Targeting {
            case: "a resource-group target, which no node of this emulator is in",
            request: json!({
                "Name": AGENT,
                "Targets": [{"Key": "resource-groups:Name", "Values": ["web-tier"]}],
            }),
            applies_to: &[],
        },
        Targeting {
            case: "another document, with the parameters it declares, on one node id target",
            request: json!({
                "Name": SHELL,
                "Parameters": {"commands": ["echo one"]},
                "Targets": [{"Key": "InstanceIds", "Values": [HYBRID]}],
            }),
            applies_to: &[HYBRID],
        },
    ]
}

/// The report of a node lists exactly the associations in effect on THAT node, whatever
/// targeting form selected it, and no association that selects another node.
#[tokio::test]
async fn the_report_lists_the_associations_in_effect_on_the_node() {
    for row in targetings() {
        let (router, engine) = service();
        seed_nodes(&engine);

        let created = created(&router, row.case, &row.request).await;
        let id = association_id(&created);
        let wanted: BTreeSet<&str> = row.applies_to.iter().copied().collect();

        for seeded in SEEDED {
            let listed = effective(&router, row.case, seeded).await;
            let carries = listed
                .iter()
                .any(|entry| entry["AssociationId"].as_str() == Some(id.as_str()));
            assert_eq!(
                carries,
                wanted.contains(seeded),
                "case {:?}: expected node {seeded} to report the association: {}, got \
                 {listed:?}",
                row.case,
                wanted.contains(seeded)
            );
            assert_eq!(
                listed.len(),
                usize::from(wanted.contains(seeded)),
                "case {:?}: node {seeded} applies at most this one association, got {listed:?}",
                row.case
            );
            // Every entry names the node it was asked about, not the node a create happened
            // to name, carries the association version in force and the content of the
            // document that version applies.
            for entry in &listed {
                assert_eq!(
                    member(entry, "InstanceId"),
                    *seeded,
                    "case {:?}: expected the entry to name node {seeded}, got {entry:?}",
                    row.case
                );
                assert_eq!(
                    member(entry, "AssociationVersion"),
                    "1",
                    "case {:?}: a create mints the first association version, got {entry:?}",
                    row.case
                );
                let document = row.request["Name"].as_str().unwrap_or("<unset>");
                let expected = document_content(&router, row.case, document, None).await;
                assert_eq!(
                    member(entry, "Content"),
                    expected,
                    "case {:?}: expected the content of {document}, got {entry:?}",
                    row.case
                );
            }
        }

        // A well-formed node id nothing registered and no association names applies none,
        // which is a report of no association rather than a refusal.
        let none = effective(&router, row.case, UNKNOWN).await;
        assert_eq!(
            none,
            Vec::<Value>::new(),
            "case {:?}: expected a node nothing applies to to report none, got {none:?}",
            row.case
        );
    }
}

/// A `Command` document of the account's own, written in JSON; `step` makes each version's
/// body different.
fn command_json(step: &str) -> String {
    json!({
        "schemaVersion": "2.2",
        "description": format!("echo {step}"),
        "mainSteps": [{
            "action": "aws:runShellScript",
            "name": "runShell",
            "inputs": {"runCommand": [format!("echo {step}")]},
        }],
    })
    .to_string()
}

/// A `Command` document of the account's own, written in YAML — the other format a document
/// tree is written in, so the content an entry carries is told apart from the bytes a create
/// supplied.
const COMMAND_YAML: &str = r#"schemaVersion: '2.2'
description: echo from yaml
mainSteps:
  - action: 'aws:runShellScript'
    name: runShell
    inputs:
      runCommand:
        - echo yaml
"#;

/// One association, and the document version its entry has to report the content of.
struct ContentRow {
    case: &'static str,
    /// The document the create names.
    document: &'static str,
    /// The `DocumentVersion` the create names, if any.
    document_version: Option<&'static str>,
    /// The parameter values that document declares, so the parameter rule does not decide
    /// this suite's rows.
    parameters: Value,
    /// The version selector `GetDocument` is asked for to produce the expected content.
    expected_version: Option<&'static str>,
}

/// The documents of the account's own every content row is read against: one JSON document
/// updated to a second version, so the default version (1) and the latest (2) carry different
/// bodies, and one YAML document, whose content a read is served re-rendered in the format a
/// read that asks for none is served.
async fn seed_documents(router: &MultiServiceRouter) {
    for (case, operation, body) in [
        (
            "the account's own document",
            "CreateDocument",
            json!({"Name": OWN, "DocumentType": "Command", "Content": command_json("one")}),
        ),
        (
            "its second version",
            "UpdateDocument",
            json!({"Name": OWN, "Content": command_json("two")}),
        ),
        (
            "a document written in YAML",
            "CreateDocument",
            json!({
                "Name": OWN_YAML,
                "DocumentType": "Command",
                "DocumentFormat": "YAML",
                "Content": COMMAND_YAML,
            }),
        ),
    ] {
        let (status, parsed) = call(router, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must succeed, got {status} with {parsed:?}"
        );
    }
}

/// The name the account's own JSON document is created under.
const OWN: &str = "Echo-Message";

/// The name its YAML sibling is created under.
const OWN_YAML: &str = "Echo-From-Yaml";

/// The content an entry carries is the content of the document version the association
/// applies: a published document or one of the account's own, in either format a document
/// tree is written in, at the version the create named or the document's default.
#[tokio::test]
async fn the_entry_carries_the_content_of_the_document_version_it_applies() {
    let rows: &[ContentRow] = &[
        ContentRow {
            case: "a published command document, no version named",
            document: AGENT,
            document_version: None,
            parameters: json!({}),
            expected_version: None,
        },
        ContentRow {
            case: "a published command document at its first version",
            document: AGENT,
            document_version: Some("1"),
            parameters: json!({}),
            expected_version: Some("1"),
        },
        ContentRow {
            case: "a published command document with a parameter value",
            document: SHELL,
            document_version: Some("$DEFAULT"),
            parameters: json!({"commands": ["echo one"]}),
            expected_version: Some("$DEFAULT"),
        },
        ContentRow {
            case: "the account's own document, no version named, so its default",
            document: OWN,
            document_version: None,
            parameters: json!({}),
            expected_version: Some("$DEFAULT"),
        },
        ContentRow {
            case: "the account's own document at the version an update appended",
            document: OWN,
            document_version: Some("2"),
            parameters: json!({}),
            expected_version: Some("2"),
        },
        ContentRow {
            case: "the account's own document at its latest version",
            document: OWN,
            document_version: Some("$LATEST"),
            parameters: json!({}),
            expected_version: Some("$LATEST"),
        },
        ContentRow {
            case: "a document written in YAML",
            document: OWN_YAML,
            document_version: None,
            parameters: json!({}),
            expected_version: None,
        },
    ];

    for row in rows {
        // One association per row, each against its own store, so the entry a row asserts on
        // is the only one in effect however many rows the table holds.
        let (router, engine) = service();
        seed_nodes(&engine);
        seed_documents(&router).await;

        let mut request = json!({
            "Name": row.document,
            "Parameters": row.parameters,
            "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
        });
        if let Some(version) = row.document_version {
            request["DocumentVersion"] = json!(version);
        }
        let created = created(&router, row.case, &request).await;
        let id = association_id(&created);

        let listed = effective(&router, row.case, NODE).await;
        let entry = listed
            .iter()
            .find(|entry| entry["AssociationId"].as_str() == Some(id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the association {id} to be in effect on {NODE}, got \
                     {listed:?}",
                    row.case
                )
            })
            .clone();
        let entry = &entry;
        let expected =
            document_content(&router, row.case, row.document, row.expected_version).await;
        assert_eq!(
            member(entry, "Content"),
            expected,
            "case {:?}: expected the content of {} at {:?}, got {entry:?}",
            row.case,
            row.document,
            row.expected_version
        );
        assert_eq!(
            member(entry, "AssociationVersion"),
            "1",
            "case {:?}: a create mints the first association version, got {entry:?}",
            row.case
        );
    }
}

/// An update appends an association version, and the entry reports the version in force and
/// the document THAT version applies — so a node is told to apply what the association says
/// now rather than what its create said.
#[tokio::test]
async fn an_update_moves_the_version_and_the_document_in_force() {
    let (router, engine) = service();
    seed_nodes(&engine);

    let created = created(
        &router,
        "the association an update moves",
        &json!({
            "Name": AGENT,
            "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
        }),
    )
    .await;
    let id = association_id(&created);

    // Each row is one update, with the association version it mints and the document the
    // entry then applies.
    let rows: &[(&str, Value, &str, &str)] = &[
        (
            "an update onto another document",
            json!({
                "AssociationId": id,
                "Name": SHELL,
                "Parameters": {"commands": ["echo two"]},
                "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
            }),
            "2",
            SHELL,
        ),
        (
            "an update back onto the first document",
            json!({
                "AssociationId": id,
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
            }),
            "3",
            AGENT,
        ),
    ];
    for (case, request, version, document) in rows {
        let (status, parsed) = call(&router, "UpdateAssociation", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateAssociation must succeed, got {status} with {parsed:?}"
        );

        let listed = effective(&router, case, NODE).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the one association in effect, got {listed:?}"
        );
        let entry = &listed[0];
        assert_eq!(
            member(entry, "AssociationVersion"),
            *version,
            "case {case:?}: expected the association version in force to be {version}, got \
             {entry:?}"
        );
        let expected = document_content(&router, case, document, None).await;
        assert_eq!(
            member(entry, "Content"),
            expected,
            "case {case:?}: expected the content of {document}, got {entry:?}"
        );
    }
}

/// Create `held` associations of [`REGION`], each targeting every node, and answer their ids
/// in ascending order.
async fn with_associations(router: &MultiServiceRouter, held: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..held {
        let case = format!("the association {index} of the page walk");
        let created = created(
            router,
            &case,
            &json!({
                "Name": AGENT,
                "AssociationName": format!("walk-{index}"),
                "Targets": [{"Key": "InstanceIds", "Values": ["*"]}],
            }),
        )
        .await;
        ids.push(association_id(&created));
    }
    ids.sort();
    ids
}

/// A page holds at most `MaxResults` entries and hands back the id the next page starts at, so
/// walking the pages reports every association in effect on the node exactly once, in the
/// store's own ascending order.
#[tokio::test]
async fn paging_reports_every_association_in_effect_exactly_once() {
    let (router, engine) = service();
    seed_nodes(&engine);
    // More associations than one page can hold, so even the request that names no
    // `MaxResults` has to be resumed.
    let held = 7;
    let whole = with_associations(&router, held).await;

    // The page sizes span the modelled range (`DescribeEffectiveInstanceAssociationsRequest$MaxResults`
    // targets `EffectiveInstanceAssociationMaxResults`, range 1..5): the minimum, sizes that
    // do and do not divide the held count, the maximum, and the request that names no
    // `MaxResults` at all, which is served up to that maximum.
    let maximum = usize::try_from(associations::MAX_EFFECTIVE_ASSOCIATION_PAGE_SIZE)
        .expect("the modelled maximum is positive");
    let sizes: &[Option<usize>] = &[Some(1), Some(2), Some(3), Some(4), Some(maximum), None];
    for size in sizes {
        let case = format!("a page size of {size:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut request = json!({"InstanceId": NODE});
            if let Some(size) = size {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = &token {
                request["NextToken"] = json!(token);
            }
            let response = reported(&router, &case, &request).await;
            let page = ids(&response);
            assert!(
                page.len() <= size.unwrap_or(maximum),
                "case {case:?}: expected at most {:?} entries in a page, got {page:?}",
                size.unwrap_or(maximum)
            );
            walked.extend(page);
            pages += 1;
            token = response["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages <= held + 1,
                "case {case:?}: the walk must terminate, got {walked:?}"
            );
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: expected every association exactly once in ascending id order, got \
             {walked:?}"
        );
        let expected_pages = held.div_ceil(size.unwrap_or(maximum));
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages, got {pages}"
        );
    }

    // A token of the modelled form names a position in the store's own order, so a report
    // resumed from one carries the associations at or after it and nothing before it.
    for (index, id) in whole.iter().enumerate() {
        let case = format!("a resume at the association {index}");
        let response = reported(&router, &case, &json!({"InstanceId": NODE, "NextToken": id})).await;
        let expected: Vec<String> = whole[index..].iter().take(maximum).cloned().collect();
        assert_eq!(
            ids(&response),
            expected,
            "case {case:?}: expected the walk to resume at {id}, got {:?}",
            ids(&response)
        );
    }
}

/// A `NextToken` shaped like no association id was not issued here, and a page size outside
/// the modelled range is refused before anything is reported.
#[tokio::test]
async fn a_token_this_report_never_issued_is_refused() {
    let (router, engine) = service();
    seed_nodes(&engine);
    with_associations(&router, 2).await;

    // Each row is a token outside the modelled `AssociationId` pattern
    // `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`: a
    // word, an id of the right groups but the wrong widths, one group short, one group too
    // many, and a group that is not hexadecimal.
    let tokens: &[(&str, &str)] = &[
        ("a word", "next-page"),
        ("the groups too short", "0123456-0000-0000-0000-000000000000"),
        ("one group short", "01234567-0000-0000-000000000000"),
        (
            "one group too many",
            "01234567-0000-0000-0000-000000000000-0000",
        ),
        (
            "a group that is not hexadecimal",
            "0123456g-0000-0000-0000-000000000000",
        ),
    ];
    for (case, token) in tokens {
        let (status, parsed) = report(&router, &json!({"InstanceId": NODE, "NextToken": token})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a token this report never issued, got {status} \
             with {parsed:?}"
        );
        let code = error_code(&parsed);
        assert_eq!(
            code, "InvalidNextToken",
            "case {case:?}: expected InvalidNextToken, got {code} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message, "The NextToken value is not valid",
            "case {case:?}: expected the service's own sentence, got {message:?}"
        );
    }

    // The page size the model bounds: one below the minimum and one above the maximum, which
    // for this operation is tighter than the association listing's own.
    for (case, max_results) in [
        (
            "below the modelled minimum",
            associations::MIN_EFFECTIVE_ASSOCIATION_PAGE_SIZE - 1,
        ),
        (
            "above the modelled maximum",
            associations::MAX_EFFECTIVE_ASSOCIATION_PAGE_SIZE + 1,
        ),
    ] {
        let (status, parsed) =
            report(&router, &json!({"InstanceId": NODE, "MaxResults": max_results})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a page size {max_results}, got {status} with \
             {parsed:?}"
        );
    }

    // An empty token is read the same way as one the request omitted: the first page.
    let response = reported(
        &router,
        "an empty token",
        &json!({"InstanceId": NODE, "NextToken": ""}),
    )
    .await;
    assert_eq!(
        ids(&response).len(),
        2,
        "case \"an empty token\": expected the first page, got {response:?}"
    );
}

/// The report is a READ: two reports taken of one node carry the same entries, and the store
/// holds afterwards exactly what it held before — no run is started, finished or re-dated by
/// being asked about.
#[tokio::test]
async fn a_report_changes_nothing_a_later_caller_sees() {
    let (router, engine) = service();
    seed_nodes(&engine);
    let held = with_associations(&router, 3).await;

    let before = stored_runs(&engine);
    let first = reported(&router, "the first report", &json!({"InstanceId": NODE})).await;
    let second = reported(&router, "the second report", &json!({"InstanceId": NODE})).await;
    let after = stored_runs(&engine);

    assert_eq!(
        ids(&first),
        held,
        "the report carries the associations the creates persisted, got {first:?}"
    );
    assert_eq!(
        first, second,
        "two reports of one node report the same thing, got {second:?}"
    );
    assert_eq!(
        before, after,
        "a report starts, finishes or re-dates no run, got {after:?}"
    );
}

/// Every association's stored run, as `(association id, dispatch instant, nodes)` — the state
/// a read must leave alone.
fn stored_runs(
    engine: &Arc<engine::InMemoryEngine>,
) -> Vec<(String, Option<SystemTime>, Vec<String>)> {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .associations
        .values()
        .map(|record| {
            (
                record.association_id.clone(),
                record.last_run().map(|run| run.started_at),
                record
                    .last_run()
                    .map(|run| run.nodes.clone())
                    .unwrap_or_default(),
            )
        })
        .collect()
}
