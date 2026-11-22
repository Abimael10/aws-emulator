// SPDX-License-Identifier: CC0-1.0

//! The CATALOGUE of documents Systems Manager publishes, as a client discovers it through
//! `ListDocuments`.
//!
//! The rules under test:
//!
//! 1. An unfiltered listing of a FRESH account reports every document Systems Manager
//!    publishes — the whole catalogue, walked page by page — and reports each one by the
//!    identity the store holds for it: `Owner` `Amazon`, its own `DocumentType`, the
//!    `DocumentFormat` and `SchemaVersion` its body declares, the platforms its body leaves
//!    it compatible with, the `TargetType` the catalogue names for it, and a `CreatedDate`.
//!    Every name the listing reports is a name a read resolves without a create.
//! 2. A `DocumentIdentifier` carries a `TagList` whether or not the document is tagged: a
//!    document with no tags reports an EMPTY list rather than omitting the member, the same
//!    way `DescribeDocument` reports it.
//! 3. A `Filters` key that names no attribute a listing can search on is refused with HTTP
//!    400 `ValidationException` whose message names the rejected key — the modelled
//!    `InvalidFilterKey` shape carries no member to say which key was wrong.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct.
//!
//! The suites are tables: the rows span the document types and target types of the
//! catalogue, the tag sets of a document, and the shapes an unrecognised filter key comes
//! in — rather than the one listing a recorded unit happened to make.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router, REGION};

/// The catalogue the service serves (`crate::state::PUBLISHED_DOCUMENTS`), so each suite
/// below spans every entry of it rather than a few.
const CATALOGUE: &[service_com_amazonaws_ssm_amazonssm::state::PublishedDocument] =
    service_com_amazonaws_ssm_amazonssm::state::PUBLISHED_DOCUMENTS;

/// The `Owner` a published document reports: `Owner` is "the Amazon Web Services user that
/// created the document" (`DocumentDescription$Owner`), and Amazon created these.
const AMAZON: &str = "Amazon";

/// The upper bound of the modelled `MaxResults` of a document listing
/// (`ListDocumentsRequest$MaxResults`, range 1..50): the largest page a walk can ask for.
const MAX_PAGE_SIZE: i64 = 50;

/// How many pages a walk may take before the test calls the resume token broken. The
/// catalogue is hundreds of documents and a page holds up to [`MAX_PAGE_SIZE`], so this is
/// generous.
const MAX_PAGES: usize = 200;

/// Every `DocumentIdentifier` a listing narrowed by `body` reports, keyed by name, walked
/// page by page.
///
/// A complete listing is a WALK: the catalogue is larger than one page, so the pages of one
/// listing are what reports it. A name reported twice fails the walk, because a document is
/// reported once.
async fn listed(router: &MultiServiceRouter, case: &str, body: &Value) -> BTreeMap<String, Value> {
    let mut reported: BTreeMap<String, Value> = BTreeMap::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut page_body = body.clone();
        page_body["MaxResults"] = json!(MAX_PAGE_SIZE);
        if let Some(token) = &token {
            page_body["NextToken"] = json!(token);
        }
        let (status, page) = call(router, "ListDocuments", &page_body).await;
        assert_eq!(
            status, 200,
            "case {case}: ListDocuments {page_body} must succeed, got {status} {page}"
        );
        let identifiers = page["DocumentIdentifiers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            identifiers.len() as i64 <= MAX_PAGE_SIZE,
            "case {case}: a page holds at most {MAX_PAGE_SIZE} documents, got {} in {page}",
            identifiers.len()
        );
        for identifier in identifiers {
            let name = identifier["Name"].as_str().unwrap_or_default().to_string();
            assert!(
                reported.insert(name.clone(), identifier).is_none(),
                "case {case}: {name} must be reported once by the walk"
            );
        }
        token = page["NextToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert!(
        token.is_none(),
        "case {case}: the walk must end within {MAX_PAGES} pages, got a token after {} \
         documents",
        reported.len()
    );
    reported
}

/// The `DescribeDocument` report of `name`, asserting the read resolved it.
async fn describe(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, response) = call(router, "DescribeDocument", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case}: DescribeDocument {name} must resolve without a create, got {status} \
         {response}"
    );
    response["Document"].clone()
}

/// A reported string list as owned strings.
fn string_list(identifier: &Value, member: &str) -> Vec<String> {
    identifier[member]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: an unfiltered listing of a fresh account reports the WHOLE published catalogue,
/// once each, and nothing else.
#[tokio::test]
async fn a_listing_reports_every_published_document() {
    let router = router();
    let reported = listed(&router, "the catalogue of a fresh account", &json!({})).await;

    let mut expected: Vec<&str> = CATALOGUE.iter().map(|entry| entry.name).collect();
    expected.sort_unstable();
    let listed_names: Vec<&str> = reported.keys().map(String::as_str).collect();
    assert_eq!(
        listed_names, expected,
        "a listing must report every document Systems Manager publishes, and nothing else"
    );
}

/// RULE: every catalogue entry is projected from the store — the owner, the type, the
/// target type, the format, the version and the date — and the same projection is what a
/// read of that document reports.
#[tokio::test]
async fn every_published_document_is_projected_from_the_catalogue() {
    let router = router();
    let reported = listed(&router, "the whole catalogue", &json!({})).await;

    for entry in CATALOGUE {
        let case = entry.name;
        let identifier = reported
            .get(entry.name)
            .unwrap_or_else(|| panic!("case {case}: the listing must report {case}"));
        assert_eq!(
            identifier["Owner"].as_str(),
            Some(AMAZON),
            "case {case}: a published document is owned by {AMAZON}, got {identifier}"
        );
        assert_eq!(
            identifier["DocumentType"].as_str(),
            Some(entry.document_type),
            "case {case}: expected DocumentType {}, got {identifier}",
            entry.document_type
        );
        assert_eq!(
            identifier["TargetType"].as_str(),
            entry.target_type,
            "case {case}: expected TargetType {:?}, got {identifier}",
            entry.target_type
        );
        assert_eq!(
            identifier["DocumentFormat"].as_str(),
            Some("JSON"),
            "case {case}: expected the format the catalogue body is written in, got \
             {identifier}"
        );
        assert_eq!(
            identifier["DocumentVersion"].as_str(),
            Some("1"),
            "case {case}: a published document holds one version, got {identifier}"
        );
        // The date is the catalogue's own datum for that document, to the millisecond
        // Systems Manager reports a document date at — not a reading of this store's clock.
        let reported_millis = identifier["CreatedDate"]
            .as_f64()
            .map(|date| (date * 1_000.0).round() as u64);
        assert_eq!(
            reported_millis,
            Some(entry.created_date_millis),
            "case {case}: expected the publication date the catalogue carries \
             ({}), got {identifier}",
            entry.created_date_millis
        );
        assert!(
            !string_list(identifier, "PlatformTypes").is_empty(),
            "case {case}: a document is compatible with at least one platform, got \
             {identifier}"
        );
    }
}

/// RULE: every name the listing reports is a name a READ resolves, with no create of any
/// kind, and the two agree member by member.
#[tokio::test]
async fn every_listed_document_resolves_and_agrees_with_the_listing() {
    let router = router();
    let reported = listed(&router, "the whole catalogue", &json!({})).await;

    for (name, identifier) in &reported {
        let document = describe(&router, name, name).await;
        for member in [
            "Name",
            "Owner",
            "DocumentType",
            "DocumentFormat",
            "SchemaVersion",
            "DocumentVersion",
            "TargetType",
        ] {
            assert_eq!(
                identifier[member], document[member],
                "case {name}: the listing's {member} must be the read's, got {identifier} \
                 and {document}"
            );
        }
        assert_eq!(
            string_list(identifier, "PlatformTypes"),
            string_list(&document, "PlatformTypes"),
            "case {name}: the listing's PlatformTypes must be the read's, got {identifier} \
             and {document}"
        );
    }
}

/// RULE: a filter narrows the catalogue by one attribute — the owner class, the document
/// type, the target type — and reports exactly the entries the store holds under it.
#[tokio::test]
async fn filters_select_the_catalogue_by_its_own_metadata() {
    let router = router();

    let every: Vec<&str> = {
        let mut names: Vec<&str> = CATALOGUE.iter().map(|entry| entry.name).collect();
        names.sort_unstable();
        names
    };
    let owned = listed(
        &router,
        "Owner=Amazon",
        &json!({ "Filters": [{ "Key": "Owner", "Values": ["Amazon"] }] }),
    )
    .await;
    assert_eq!(
        owned.keys().map(String::as_str).collect::<Vec<&str>>(),
        every,
        "Owner=Amazon must report the whole published catalogue"
    );

    // Every type the catalogue holds, and one it holds none of: a filter reports the
    // entries of that type and nothing else.
    let mut types: Vec<&str> = CATALOGUE.iter().map(|entry| entry.document_type).collect();
    types.sort_unstable();
    types.dedup();
    types.push("DeploymentStrategy");
    for document_type in types {
        let mut expected: Vec<&str> = CATALOGUE
            .iter()
            .filter(|entry| entry.document_type == document_type)
            .map(|entry| entry.name)
            .collect();
        expected.sort_unstable();
        let reported = listed(
            &router,
            document_type,
            &json!({ "Filters": [{ "Key": "DocumentType", "Values": [document_type] }] }),
        )
        .await;
        assert_eq!(
            reported.keys().map(String::as_str).collect::<Vec<&str>>(),
            expected,
            "case {document_type}: a DocumentType filter must report the documents of that \
             type"
        );
    }

    // Every target type the catalogue names, and one it names for nothing.
    let mut target_types: Vec<&str> = CATALOGUE
        .iter()
        .filter_map(|entry| entry.target_type)
        .collect();
    target_types.sort_unstable();
    target_types.dedup();
    target_types.push("/AWS::Lambda::Function");
    for target_type in target_types {
        let mut expected: Vec<&str> = CATALOGUE
            .iter()
            .filter(|entry| entry.target_type == Some(target_type))
            .map(|entry| entry.name)
            .collect();
        expected.sort_unstable();
        let reported = listed(
            &router,
            target_type,
            &json!({ "Filters": [{ "Key": "TargetType", "Values": [target_type] }] }),
        )
        .await;
        assert_eq!(
            reported.keys().map(String::as_str).collect::<Vec<&str>>(),
            expected,
            "case {target_type}: a TargetType filter must report the documents the catalogue \
             names it for"
        );
    }
}

/// RULE: a `DocumentIdentifier` always carries a `TagList` — empty for a document nothing
/// has tagged, the document's own tags otherwise — so a caller iterating `Tags` never has
/// to tell a missing member from an empty one.
#[tokio::test]
async fn a_listing_reports_a_tag_list_whether_or_not_the_document_is_tagged() {
    let router = router();
    let content = r#"{
  "schemaVersion": "2.2",
  "description": "echo",
  "mainSteps": [
    { "name": "echo", "action": "aws:runShellScript", "inputs": { "commands": ["echo hi"] } }
  ]
}"#;
    // One tag row: the case, the document's name, the tags the create supplies, and the
    // pairs a read of the listing must report for it.
    struct TagRow<'a> {
        case: &'a str,
        name: &'a str,
        tags: Value,
        expected: Vec<(&'a str, &'a str)>,
    }
    let rows: Vec<TagRow<'_>> = vec![
        TagRow {
            case: "a created document with no tags at all",
            name: "UntaggedDocument",
            tags: json!([]),
            expected: Vec::new(),
        },
        TagRow {
            case: "a created document with one tag",
            name: "SinglyTaggedDocument",
            tags: json!([{ "Key": "Environment", "Value": "Production" }]),
            expected: vec![("Environment", "Production")],
        },
        TagRow {
            case: "a created document with two tags",
            name: "DoublyTaggedDocument",
            tags: json!([
                { "Key": "Environment", "Value": "Production" },
                { "Key": "Project", "Value": "Apollo" },
            ]),
            expected: vec![("Environment", "Production"), ("Project", "Apollo")],
        },
    ];
    for row in &rows {
        let mut body = json!({ "Name": row.name, "Content": content });
        if !row.tags.as_array().is_some_and(|tags| tags.is_empty()) {
            body["Tags"] = row.tags.clone();
        }
        let (status, created) = call(&router, "CreateDocument", &body).await;
        assert_eq!(
            status, 200,
            "case {}: CreateDocument must succeed, got {status} {created}",
            row.case
        );
    }

    let reported = listed(&router, "the tag projection", &json!({})).await;
    for row in &rows {
        let case = row.case;
        let identifier = reported
            .get(row.name)
            .unwrap_or_else(|| panic!("case {case}: the listing must report {}", row.name));
        let mut pairs: Vec<(String, String)> = identifier["Tags"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case}: expected a Tags list, got {identifier}");
            })
            .iter()
            .map(|tag| {
                (
                    tag["Key"].as_str().unwrap_or_default().to_string(),
                    tag["Value"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        pairs.sort();
        let expected: Vec<(String, String)> = row
            .expected
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            pairs, expected,
            "case {case}: expected the tags the create supplied, got {identifier}"
        );
    }

    // And the same holds for every document nobody tagged, the published catalogue
    // included: the member is a list, never absent.
    for entry in CATALOGUE {
        let identifier = reported
            .get(entry.name)
            .unwrap_or_else(|| panic!("case {}: the listing must report it", entry.name));
        assert_eq!(
            identifier["Tags"].as_array().map(Vec::len),
            Some(0),
            "case {}: an untagged document reports an empty Tags list, got {identifier}",
            entry.name
        );
    }
}

/// RULE: a `Filters` key that names no attribute a document listing can search on is
/// refused with HTTP 400 `ValidationException` whose message names the rejected key.
#[tokio::test]
async fn an_unknown_filter_key_is_refused_as_a_validation_error_naming_the_key() {
    let router = router();
    // The keys a listing CAN search on are the Amazon Web Services-provided ones
    // `ListDocumentsRequest$Filters` lists — DocumentType, Name, Owner, PlatformTypes and
    // TargetType — and the `tag:tagName` form; every row below is outside that set.
    let longest = "K".repeat(128);
    let unknown_keys: Vec<(&str, &str)> = vec![
        ("a key naming no attribute of a document", "BogusFilterKey"),
        ("a key differing from a valid one only in case", "owner"),
        ("a key differing from a valid one in its plural", "DocumentTypes"),
        ("a key naming a member of the identifier that is not filterable", "SchemaVersion"),
        ("the tag prefix with no tag key after it", "tag:"),
        ("a tag prefix differing only in case", "Tag:Environment"),
        ("a key carrying a space", "Document Type"),
        ("a key at the modelled maximum length", longest.as_str()),
    ];

    for (case, key) in &unknown_keys {
        let (status, response) = call(
            &router,
            "ListDocuments",
            &json!({ "Filters": [{ "Key": key, "Values": ["anything"] }] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: the key {key:?} must be refused with HTTP 400, got {status} \
             {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case}: the key {key:?} must be refused as a ValidationException, got \
             {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("{key} is not a known filter key."),
            "case {case}: the refusal must name the rejected key, got {response}"
        );
    }

    // A filter naming NO key at all names no attribute either, and is refused the same way.
    let (status, response) = call(
        &router,
        "ListDocuments",
        &json!({ "Filters": [{ "Values": ["anything"] }] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a filter naming no key: must be refused with HTTP 400, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case a filter naming no key: must be refused as a ValidationException, got {response}"
    );

    // And a key the listing CAN search on is served rather than refused, so the refusal is
    // about the key naming no attribute and not about filtering at all.
    for key in ["DocumentType", "Name", "Owner", "PlatformTypes", "TargetType"] {
        let value = match key {
            "DocumentType" => "Command",
            "Owner" => "Amazon",
            "PlatformTypes" => "Linux",
            "TargetType" => "/AWS::EC2::Instance",
            _ => "AWS-",
        };
        let (status, response) = call(
            &router,
            "ListDocuments",
            &json!({ "Filters": [{ "Key": key, "Values": [value] }], "MaxResults": 1 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {key}: a key the listing can search on must be served, got {status} \
             {response}"
        );
    }

    // A tag key names an attribute of a document, so it is served too — in the Region the
    // request is signed for.
    let (status, response) = call(
        &router,
        "ListDocuments",
        &json!({ "Filters": [{ "Key": "tag:Environment", "Values": ["Production"] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a tag key: must be served in {REGION}, got {status} {response}"
    );
}
