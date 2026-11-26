// SPDX-License-Identifier: CC0-1.0

//! The `DocumentFormat` of a document Systems Manager publishes, through
//! `ListDocumentVersions`, `DescribeDocument`, `ListDocuments` and `GetDocument`.
//!
//! The rule under test: a document Systems Manager publishes was authored by Amazon, so
//! "the document format, either JSON or YAML" (`DocumentVersionInfo$DocumentFormat`) a read
//! reports for it is the format THAT document's published body is written in — a fact of the
//! catalogue, identical in every account, Region and run — and not the format a create that
//! declares none falls back to. `GetDocument` is the one read that may be asked for another
//! format: it re-renders the body into the format the request asked for and reports the
//! format it actually returned, while every other read keeps reporting the authored one.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct.
//!
//! The suites are tables spanning the whole published catalogue — every document type and
//! both authored formats — crossed with every `DocumentFormat` a read can ask for, rather
//! than the one document a recorded unit happened to list.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, router};

/// The catalogue the service serves (`crate::state::PUBLISHED_DOCUMENTS`), so each suite
/// below spans every entry of it rather than a few.
const CATALOGUE: &[service_com_amazonaws_ssm_amazonssm::state::PublishedDocument] =
    service_com_amazonaws_ssm_amazonssm::state::PUBLISHED_DOCUMENTS;

/// The `DocumentFormat` values a document TREE is written in, from the model's enum
/// (`smithy_query(query="shape", shape="DocumentFormat")`: `YAML`, `JSON`, `TEXT`) without
/// the free-text one, which carries no tree and no document the catalogue publishes is in.
const TREE_FORMATS: &[&str] = &["JSON", "YAML"];

/// The format a `GetDocument` that declares none is served in: "JSON is the default
/// format" (`GetDocumentRequest$DocumentFormat`).
const REQUEST_DEFAULT_FORMAT: &str = "JSON";

/// The `DocumentVersionInfo` of the default version of `name`, asserting the read resolved
/// the document without any create.
async fn default_version(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, response) = call(router, "ListDocumentVersions", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case}: ListDocumentVersions {name} must resolve a document AWS publishes, \
         got {status} {response}"
    );
    let versions = response["DocumentVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    versions
        .into_iter()
        .find(|version| version["IsDefaultVersion"] == json!(true))
        .unwrap_or_else(|| {
            panic!("case {case}: {name} lists a default version, got {response}");
        })
}

/// Every `DocumentIdentifier` an unfiltered `ListDocuments` reports, keyed by name, walked
/// page by page.
async fn listed(router: &MultiServiceRouter) -> BTreeMap<String, Value> {
    let mut reported: BTreeMap<String, Value> = BTreeMap::new();
    let mut token: Option<String> = None;
    // The catalogue is hundreds of documents and a page holds at most the modelled 50, so
    // this many pages is generous.
    for _ in 0..200 {
        let mut body = json!({ "MaxResults": 50 });
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let (status, page) = call(router, "ListDocuments", &body).await;
        assert_eq!(
            status, 200,
            "ListDocuments {body} must succeed, got {status} {page}"
        );
        for identifier in page["DocumentIdentifiers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let name = identifier["Name"].as_str().unwrap_or_default().to_string();
            reported.insert(name, identifier);
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
        "the walk of the catalogue must end, got a token after {} documents",
        reported.len()
    );
    reported
}

/// RULE: every read of a published document reports the format its published body is
/// authored in — the version listing, the describe and the document listing alike — and
/// that format is one the model declares for a document tree.
#[tokio::test]
async fn every_published_document_reports_the_format_its_body_is_authored_in() {
    let router = router();
    let identifiers = listed(&router).await;

    for entry in CATALOGUE {
        let case = entry.name;
        let authored = entry.document_format();
        assert!(
            TREE_FORMATS.contains(&authored),
            "case {case}: a published body is written in one of {TREE_FORMATS:?}, got \
             {authored}"
        );

        let version = default_version(&router, case, entry.name).await;
        assert_eq!(
            version["DocumentFormat"].as_str(),
            Some(authored),
            "case {case}: ListDocumentVersions must report the authored format {authored}, \
             got {version}"
        );

        let (status, described) = call(&router, "DescribeDocument", &json!({ "Name": entry.name })).await;
        assert_eq!(
            status, 200,
            "case {case}: DescribeDocument must resolve it too, got {status} {described}"
        );
        assert_eq!(
            described["Document"]["DocumentFormat"].as_str(),
            Some(authored),
            "case {case}: DescribeDocument must report the authored format {authored}, got \
             {described}"
        );

        let identifier = identifiers.get(entry.name).unwrap_or_else(|| {
            panic!("case {case}: a listing must report every published document");
        });
        assert_eq!(
            identifier["DocumentFormat"].as_str(),
            Some(authored),
            "case {case}: ListDocuments must report the authored format {authored}, got \
             {identifier}"
        );
    }

    // The catalogue carries BOTH authored formats, so none of the above is satisfied by one
    // format the service reports for everything: Amazon authors some of its documents in
    // YAML and some in JSON.
    for format in TREE_FORMATS {
        assert!(
            CATALOGUE
                .iter()
                .any(|entry| entry.document_format() == *format),
            "the catalogue publishes at least one document authored in {format}"
        );
    }
}

/// RULE: the authored format belongs to the DOCUMENT, not to the read that reported it: it
/// is the same on a second read, through another Region's endpoint, and in a second
/// emulator — a restart cannot move it.
#[tokio::test]
async fn an_authored_format_is_the_same_in_every_region_and_in_a_second_emulator() {
    let first = router();
    let second = router();

    for entry in CATALOGUE {
        let case = entry.name;
        let authored = entry.document_format();
        for (which, router) in [("the first emulator", &first), ("a restarted one", &second)] {
            let version = default_version(router, case, entry.name).await;
            assert_eq!(
                version["DocumentFormat"].as_str(),
                Some(authored),
                "case {case}: {which} reports the authored format {authored}, got {version}"
            );
        }
        for region in ["us-east-1", "eu-central-1", "ap-southeast-2"] {
            let (status, response) = call_in_region(
                &first,
                region,
                "ListDocumentVersions",
                &json!({ "Name": entry.name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: a published document resolves through {region}, got {status} \
                 {response}"
            );
            assert_eq!(
                response["DocumentVersions"][0]["DocumentFormat"].as_str(),
                Some(authored),
                "case {case}: the authored format is {authored} through {region} too, got \
                 {response}"
            );
        }
    }
}

/// The format the recorded catalogue of a real account reports each of these published
/// documents as being authored in.
///
/// Source: the `ssm` catalogue of the wire corpus `simparity-wire-aws-v1`
/// (`ListDocumentVersions` over the Amazon-owned documents). The format is a property of
/// the document AWS published, so it is the same in every account and Region; the rows
/// below are the ones that corpus pins, and the suites above hold the rule for every other
/// entry of the catalogue.
const RECORDED_AUTHORED_FORMATS: &[(&str, &str)] = &[
    ("AWS-ApplyAnsiblePlaybooks", "JSON"),
    ("AWS-CreateImage", "JSON"),
    ("AWS-InstallWindowsUpdates", "YAML"),
    ("AWS-RestartEC2Instance", "JSON"),
    ("AWS-RunRemoteScript", "JSON"),
    ("AWS-StartEC2Instance", "JSON"),
    ("AWS-StartPortForwardingSession", "JSON"),
    ("AWS-StartSSHSession", "JSON"),
    ("AWS-StopEC2Instance", "JSON"),
    ("AWS-UpdateLinuxAmi", "JSON"),
];

/// Each document whose authored format is known is served in THAT format — by the version
/// listing and by the describe alike.
#[tokio::test]
async fn the_known_authored_formats_are_served_as_the_catalogue_carries_them() {
    let router = router();
    for (name, expected) in RECORDED_AUTHORED_FORMATS {
        assert!(
            CATALOGUE.iter().any(|entry| entry.name == *name),
            "case {name}: the catalogue must publish a document the corpus pins a format for"
        );
        let version = default_version(&router, name, name).await;
        assert_eq!(
            version["DocumentFormat"].as_str(),
            Some(*expected),
            "case {name}: expected the authored format {expected}, got {version}"
        );
        let (status, described) = call(&router, "DescribeDocument", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {name}: DescribeDocument must resolve it, got {status} {described}"
        );
        assert_eq!(
            described["Document"]["DocumentFormat"].as_str(),
            Some(*expected),
            "case {name}: DescribeDocument must report the authored format {expected}, got \
             {described}"
        );
    }
    // Both formats are represented above, so the rows cannot be satisfied by one format the
    // service reports for everything.
    for format in TREE_FORMATS {
        assert!(
            RECORDED_AUTHORED_FORMATS
                .iter()
                .any(|(_, authored)| authored == format),
            "the recorded rows must carry a document authored in {format}"
        );
    }
}

/// RULE: `GetDocument` returns the published body in the format the request asks for — and
/// the DOCUMENT it returns is the same document whichever format that is: the same schema
/// version, description and parameter names, read out of the content the call returned.
///
/// The rows cross every authored format in the catalogue (one entry per format, picked from
/// the catalogue rather than named here) with every format a request can ask for, the
/// request that asks for none included.
#[tokio::test]
async fn a_published_body_is_served_in_the_format_the_request_asks_for() {
    let router = router();

    for authored in TREE_FORMATS {
        let entry = CATALOGUE
            .iter()
            .find(|entry| entry.document_format() == *authored)
            .unwrap_or_else(|| {
                panic!("the catalogue publishes a document authored in {authored}");
            });
        // What the document IS, read from the describe rather than restated here.
        let (status, described) = call(&router, "DescribeDocument", &json!({ "Name": entry.name })).await;
        assert_eq!(
            status, 200,
            "case {}: DescribeDocument must resolve it, got {status} {described}",
            entry.name
        );
        let document = &described["Document"];
        let schema_version = document["SchemaVersion"].as_str().unwrap_or_default().to_string();
        let description = document["Description"].as_str().unwrap_or_default().to_string();
        let parameters: Vec<String> = document["Parameters"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|parameter| parameter["Name"].as_str().unwrap_or_default().to_string())
            .collect();

        // `None` is the request that declares no format at all, which is served the
        // documented default.
        let requests: Vec<Option<&str>> = vec![None, Some("JSON"), Some("YAML")];
        for requested in requests {
            let case = format!(
                "{} authored in {authored}, asked for {}",
                entry.name,
                requested.unwrap_or("nothing")
            );
            let mut body = json!({ "Name": entry.name });
            if let Some(requested) = requested {
                body["DocumentFormat"] = json!(requested);
            }
            let (status, read) = call(&router, "GetDocument", &body).await;
            assert_eq!(
                status, 200,
                "case {case}: GetDocument must resolve the document, got {status} {read}"
            );
            let served = requested.unwrap_or(REQUEST_DEFAULT_FORMAT);
            assert_eq!(
                read["DocumentFormat"].as_str(),
                Some(served),
                "case {case}: expected the format the content was returned in ({served}), \
                 got {read}"
            );
            let content = read["Content"].as_str().unwrap_or_else(|| {
                panic!("case {case}: GetDocument returns the document's content, got {read}");
            });

            // The content is a document of the format it was reported in, and it is the
            // SAME document whichever format that is.
            if served == "JSON" {
                let tree: Value = serde_json::from_str(content).unwrap_or_else(|why| {
                    panic!("case {case}: the content must be JSON, got {why}: {content}");
                });
                assert_eq!(
                    (
                        tree["schemaVersion"].as_str().unwrap_or_default(),
                        tree["description"].as_str().unwrap_or_default(),
                    ),
                    (schema_version.as_str(), description.as_str()),
                    "case {case}: expected the document the describe reports, got {content}"
                );
                let served_parameters: Vec<String> = tree["parameters"]
                    .as_object()
                    .map(|parameters| parameters.keys().cloned().collect())
                    .unwrap_or_default();
                assert_eq!(
                    served_parameters, parameters,
                    "case {case}: expected the parameters the describe reports, got {content}"
                );
            } else {
                let mut trees = yaml_rust2::YamlLoader::load_from_str(content)
                    .unwrap_or_else(|why| {
                        panic!("case {case}: the content must be YAML, got {why}: {content}");
                    });
                assert_eq!(
                    trees.len(),
                    1,
                    "case {case}: the content is exactly one document, got {content}"
                );
                let tree = trees.remove(0);
                assert_eq!(
                    (
                        tree["schemaVersion"].as_str().unwrap_or_default(),
                        tree["description"].as_str().unwrap_or_default(),
                    ),
                    (schema_version.as_str(), description.as_str()),
                    "case {case}: expected the document the describe reports, got {content}"
                );
                let served_parameters: Vec<String> = tree["parameters"]
                    .as_hash()
                    .map(|parameters| {
                        parameters
                            .keys()
                            .map(|key| key.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(
                    served_parameters, parameters,
                    "case {case}: expected the parameters the describe reports, got {content}"
                );
            }

            // Asking for a format never moves what the document IS: the version listing
            // still reports the authored one.
            let version = default_version(&router, &case, entry.name).await;
            assert_eq!(
                version["DocumentFormat"].as_str(),
                Some(*authored),
                "case {case}: the authored format stays {authored} after a read in \
                 {served}, got {version}"
            );
        }
    }
}
