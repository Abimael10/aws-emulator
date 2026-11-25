// SPDX-License-Identifier: CC0-1.0

//! The links a `ScanSbom` report publishes, end-to-end over HTTP.
//!
//! The rule under test: a report names an advisory's address at most ONCE — on
//! the CycloneDX vulnerability node's own `source`, as the page the advisory's
//! publisher published it at (the store's `source_url`) — and publishes no other
//! address anywhere. In particular a RATING names only the scoring vendor that
//! produced it (a rating is not a publication to dereference), the
//! Inspector-native forms name the publisher by name alone, and no form
//! republishes an advisory's reference pages: a report hands the caller no
//! further host to follow.
//!
//! The rows span every record of `src/state.rs::seed_vulnerability_db` — the
//! catalog is read out of the store the handler answers from, so a record added
//! to it is covered without touching this file — in every `outputFormat` the
//! model declares (`ops::types::OUTPUT_FORMAT_VALUES`), and the members the two
//! report forms must not publish a link on are pinned by name.

use super::scan_inspector_alt_form::node_for;
use super::scan_report_annotations::inject_advisory;
use super::scan_sbom::{bom, emulator, post_scan};
use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::ops::types::OUTPUT_FORMAT_VALUES;
use service_com_amazonaws_inspectorscan_inspectorscan::state::{
    ComAmazonawsInspectorscanInspectorscanState, VersionRange, VulnRecord,
};
use std::collections::BTreeSet;
use std::sync::Arc;

/// The page the National Vulnerability Database published an advisory at, as the
/// recorded CycloneDX report carries it on a node's `source`: `vuln/detail/<id>`
/// under its own host. Written out here, independently of what the store holds,
/// so a change to the catalog's page for that advisory is caught rather than
/// mirrored.
const NVD_PAGE_PREFIX: &str = "https://nvd.nist.gov/vuln/detail/";

/// The advisory whose page the recording pins, published at
/// `<NVD_PAGE_PREFIX><id>`.
const RECORDED_PAGE_ADVISORY: &str = "CVE-2021-44228";

/// The report form that answers the submitted CycloneDX document augmented with
/// the scan, and the only one whose vulnerability node names an address.
const CYCLONEDX_FORMAT: &str = "CYCLONE_DX_1_5";

/// Members no report may carry: the Inspector form's per-finding `sourceUrl` and
/// `referenceUrls`, and the CycloneDX node's `advisories`, whose every entry
/// CycloneDX types as a reference URL.
const FORBIDDEN_MEMBERS: &[&str] = &["sourceUrl", "referenceUrls", "advisories"];

/// The advisory catalog the handler answers from, read out of the SAME store.
fn catalog(engine: &Arc<engine::InMemoryEngine>) -> Vec<VulnRecord> {
    let state = engine.state::<ComAmazonawsInspectorscanInspectorscanState>();
    let state = state.lock();
    state.vulnerability_db.clone()
}

/// A release inside `range`, so a component carrying it matches the advisory:
/// the interval's lower bound when it states one, else anything below its upper
/// bound. `None` for a range that bounds nothing and so matches no release.
fn affected_release(range: &VersionRange) -> Option<String> {
    if let Some(introduced) = range.from_including {
        return Some(format!(
            "{}.{}.{}",
            introduced.major, introduced.minor, introduced.patch
        ));
    }
    range.before_excluding.map(|_| "0.0.1".to_string())
}

/// The purl of a component `record` applies to, written so the handler resolves
/// the stored coordinate back out of it: the maven groupId and the Go module
/// path prefix are the purl `namespace`, and an ecosystem that scopes its
/// packages by a distribution names the record's own distribution there (the
/// package name is the last segment either way).
fn purl_for(record: &VulnRecord, version: &str) -> String {
    let package_type = record.package_type.as_str();
    let name = record.package_name.as_str();
    if let Some(distribution) = &record.distribution {
        return format!("pkg:{package_type}/{distribution}/{name}@{version}");
    }
    match package_type {
        "maven" => match name.split_once(':') {
            Some((group, artifact)) => format!("pkg:maven/{group}/{artifact}@{version}"),
            None => format!("pkg:maven/{name}@{version}"),
        },
        _ => format!("pkg:{package_type}/{name}@{version}"),
    }
}

/// A one-component document whose component matches `record`, and the purl it
/// names it by. `None` for a record no release can match.
fn document_matching(record: &VulnRecord) -> Option<(Value, String)> {
    let version = affected_release(&record.affected)?;
    let purl = purl_for(record, &version);
    let short_name = record
        .package_name
        .rsplit([':', '/'])
        .next()
        .unwrap_or(record.package_name.as_str());
    let document = bom(vec![json!({
        "type": "library",
        "bom-ref": "comp-1",
        "name": short_name,
        "version": version,
        "purl": purl,
    })]);
    Some((document, purl))
}

/// Is `value` an absolute URI — a scheme followed by `://`, i.e. an address on
/// some host?
fn is_absolute_url(value: &str) -> bool {
    value.split_once("://").is_some_and(|(scheme, rest)| {
        !scheme.is_empty()
            && !rest.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// Every string value in `document`, at any depth.
fn string_values(document: &Value, into: &mut BTreeSet<String>) {
    match document {
        Value::String(text) => {
            into.insert(text.clone());
        }
        Value::Array(items) => items.iter().for_each(|item| string_values(item, into)),
        Value::Object(members) => members.values().for_each(|value| string_values(value, into)),
        _ => {}
    }
}

/// The absolute URLs in `response` that the caller did not submit — the ones the
/// SERVICE published.
fn service_published_urls(request: &Value, response: &Value) -> BTreeSet<String> {
    let mut submitted = BTreeSet::new();
    string_values(request, &mut submitted);
    let mut answered = BTreeSet::new();
    string_values(response, &mut answered);
    answered
        .into_iter()
        .filter(|value| is_absolute_url(value) && !submitted.contains(value))
        .collect()
}

/// Every path at which `document` carries one of `members`, with the value found
/// there.
fn members_found(document: &Value, members: &[&str], path: &str, into: &mut Vec<(String, Value)>) {
    match document {
        Value::Object(entries) => {
            for (name, value) in entries {
                let child = format!("{path}.{name}");
                if members.contains(&name.as_str()) {
                    into.push((child.clone(), value.clone()));
                }
                members_found(value, members, &child, into);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                members_found(item, members, &format!("{path}[{index}]"), into);
            }
        }
        _ => {}
    }
}

/// The whole rule, asserted against one answered scan: which members may carry
/// an address, which paths an `url` may sit at, and which addresses the service
/// may have published.
fn assert_report_links(
    case: &str,
    format: &str,
    request: &Value,
    response: &Value,
    records: &[VulnRecord],
) {
    let report = &response["sbom"];

    // A member the caller's own document carries is the caller's data, echoed
    // back by the CycloneDX form; only what the SERVICE added is judged.
    let mut submitted_values = Vec::new();
    members_found(request, FORBIDDEN_MEMBERS, "$", &mut submitted_values);
    let submitted_values: Vec<Value> = submitted_values.into_iter().map(|(_, v)| v).collect();
    let mut forbidden = Vec::new();
    members_found(report, FORBIDDEN_MEMBERS, "$", &mut forbidden);
    forbidden.retain(|(_, value)| !submitted_values.contains(value));
    assert!(
        forbidden.is_empty(),
        "case {case:?}: a report carries none of {FORBIDDEN_MEMBERS:?}, got {forbidden:?}"
    );

    // One address per reported node, and only in the CycloneDX form: the page
    // its publisher publishes that advisory id at.
    let mut expected: BTreeSet<String> = BTreeSet::new();
    let mut allowed_paths: BTreeSet<String> = BTreeSet::new();
    let nodes = report["vulnerabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (position, node) in nodes.iter().enumerate() {
        let id = node["id"].as_str().unwrap_or_default();
        let record = records
            .iter()
            .find(|record| record.cve_id == id)
            .unwrap_or_else(|| panic!("case {case:?}: the store holds an advisory for {id:?}"));
        if format != CYCLONEDX_FORMAT {
            continue;
        }
        let page = record.source_url.clone();
        assert_eq!(
            node["source"]["url"].as_str(),
            page.as_deref(),
            "case {case:?}: node {id} names the page its publisher publishes it at, got {node}"
        );
        for rating in node["ratings"].as_array().cloned().unwrap_or_default() {
            assert!(
                rating["source"]["url"].is_null(),
                "case {case:?}: a rating of {id} names only its scoring vendor, got {rating}"
            );
        }
        if let Some(page) = page {
            expected.insert(page);
            allowed_paths.insert(format!("$.vulnerabilities[{position}].source.url"));
        }
    }

    let mut urls = Vec::new();
    members_found(report, &["url"], "$", &mut urls);
    let mut submitted = BTreeSet::new();
    string_values(request, &mut submitted);
    for (path, value) in &urls {
        // An address the caller submitted is the caller's own data, echoed back.
        if value.as_str().is_some_and(|url| submitted.contains(url)) {
            continue;
        }
        assert!(
            allowed_paths.contains(path),
            "case {case:?}: an address sits only on a vulnerability node's own source, got \
             {value} at {path}"
        );
    }

    let published = service_published_urls(request, response);
    assert_eq!(
        published, expected,
        "case {case:?}: the report publishes exactly the publisher pages of the advisories it \
         reports, got {published:?} in {report}"
    );
}

/// Every record of the catalog, in every modelled report form.
#[tokio::test]
async fn every_matched_advisory_is_named_with_its_publishers_page_and_no_other_link() {
    let (engine, router) = emulator();
    let records = catalog(&engine);
    assert!(
        !records.is_empty(),
        "case \"the seeded catalog\": expected at least one advisory, got none"
    );

    for record in &records {
        let Some((document, purl)) = document_matching(record) else {
            continue;
        };
        for format in OUTPUT_FORMAT_VALUES {
            let case = format!("{} in {format}", record.cve_id);
            let request = json!({ "sbom": document.clone(), "outputFormat": format });
            let (status, response) = post_scan(&router, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status}: {response}"
            );
            let report = &response["sbom"];

            // The advisory IS reported — what the report drops is links, not the
            // finding or its provenance.
            let node = node_for(report, &record.cve_id).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a node for {purl}, got {}",
                    report["vulnerabilities"]
                )
            });
            let publisher = if *format == CYCLONEDX_FORMAT {
                node["source"]["name"].as_str()
            } else {
                node["source"].as_str()
            };
            assert_eq!(
                publisher,
                Some(record.source.as_str()),
                "case {case:?}: the node names the advisory's publisher, got {node}"
            );

            assert_report_links(&case, format, &request, &response, &records);
        }
    }
}

/// The page itself, not just its presence: the advisory the recording pins is
/// reported at its publisher's `vuln/detail/<id>` address, spelled here
/// independently of what the catalog holds.
#[tokio::test]
async fn the_published_page_is_the_publishers_own_address_for_the_advisory_id() {
    let (engine, router) = emulator();
    let records = catalog(&engine);
    let record = records
        .iter()
        .find(|record| record.cve_id == RECORDED_PAGE_ADVISORY)
        .unwrap_or_else(|| panic!("the catalog holds {RECORDED_PAGE_ADVISORY}"));
    let (document, purl) = document_matching(record)
        .unwrap_or_else(|| panic!("{RECORDED_PAGE_ADVISORY} states an affected range"));
    let request = json!({ "sbom": document, "outputFormat": CYCLONEDX_FORMAT });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");
    let node = node_for(&response["sbom"], RECORDED_PAGE_ADVISORY)
        .unwrap_or_else(|| panic!("expected a node for {purl}, got {}", response["sbom"]));
    assert_eq!(
        node["source"]["url"].as_str(),
        Some(format!("{NVD_PAGE_PREFIX}{RECORDED_PAGE_ADVISORY}").as_str()),
        "case {RECORDED_PAGE_ADVISORY:?}: the node names the publisher's own address for the \
         advisory id, got {node}"
    );
}

/// A publisher page the store does not hold is not invented: the node names the
/// advisory's publisher by `name` alone, in every report form.
#[tokio::test]
async fn an_advisory_the_store_holds_no_page_for_is_named_without_one() {
    let injected = "CVE-2024-99002";
    for format in OUTPUT_FORMAT_VALUES {
        let case = format!("an advisory with no publication page in {format}");
        let (engine, router) = emulator();
        inject_advisory(&engine, injected, "npm", "left-pad", "MEDIUM", vec![20]);
        let records = catalog(&engine);
        let record = records
            .iter()
            .find(|record| record.cve_id == injected)
            .unwrap_or_else(|| panic!("case {case:?}: the store holds the injected advisory"));
        assert!(
            record.source_url.is_none(),
            "case {case:?}: the injected record holds no page, got {:?}",
            record.source_url
        );

        let request = json!({
            "sbom": bom(vec![json!({
                "type": "library",
                "bom-ref": "comp-left-pad",
                "name": "left-pad",
                "version": "1.0.0",
                "purl": "pkg:npm/left-pad@1.0.0",
            })]),
            "outputFormat": format,
        });
        let (status, response) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status}: {response}"
        );
        let node = node_for(&response["sbom"], injected).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a node for the injected advisory, got {}",
                response["sbom"]["vulnerabilities"]
            )
        });
        let named = if *format == CYCLONEDX_FORMAT {
            node["source"]["name"].as_str()
        } else {
            node["source"].as_str()
        };
        assert_eq!(
            named,
            Some(record.source.as_str()),
            "case {case:?}: the node names the publisher, got {node}"
        );
        assert_report_links(&case, format, &request, &response, &records);
    }
}

/// Documents spanning what the rule generalises over: nothing matched, two
/// components under one advisory, and several advisories across several
/// ecosystems.
fn documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a component no advisory applies to",
            bom(vec![json!({
                "type": "library",
                "bom-ref": "comp-clean",
                "name": "lodash",
                "version": "4.17.21",
                "purl": "pkg:npm/lodash@4.17.21",
            })]),
        ),
        (
            "two releases of one package under one advisory",
            bom(vec![
                json!({
                    "type": "library",
                    "bom-ref": "comp-log4j-a",
                    "name": "log4j-core",
                    "version": "2.12.1",
                    "purl": "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
                }),
                json!({
                    "type": "library",
                    "bom-ref": "comp-log4j-b",
                    "name": "log4j-core",
                    "version": "2.14.1",
                    "purl": "pkg:maven/org.apache.logging.log4j/log4j-core@2.14.1",
                }),
            ]),
        ),
        (
            "three advisories across three ecosystems",
            bom(vec![
                json!({
                    "type": "library",
                    "bom-ref": "comp-urllib3",
                    "name": "urllib3",
                    "version": "1.26.4",
                    "purl": "pkg:pypi/urllib3@1.26.4",
                }),
                json!({
                    "type": "library",
                    "bom-ref": "comp-gin",
                    "name": "gin",
                    "version": "v1.7.0",
                    "purl": "pkg:golang/github.com/gin-gonic/gin@v1.7.0",
                }),
                json!({
                    "type": "library",
                    "bom-ref": "comp-glibc",
                    "name": "glibc",
                    "version": "2.37",
                    "purl": "pkg:rpm/redhat/glibc@2.37",
                }),
            ]),
        ),
    ]
}

/// Whichever document is scanned, in whichever form, the only address in the
/// answer is a reported node's publisher page.
#[tokio::test]
async fn a_report_publishes_no_address_beside_the_nodes_own_source() {
    let (engine, router) = emulator();
    let records = catalog(&engine);
    for (what, document) in documents() {
        for format in OUTPUT_FORMAT_VALUES {
            let case = format!("{what} in {format}");
            let request = json!({ "sbom": document.clone(), "outputFormat": format });
            let (status, response) = post_scan(&router, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status}: {response}"
            );
            assert_report_links(&case, format, &request, &response, &records);
        }
    }
}

/// A link the CALLER submitted is the caller's own data: the CycloneDX report
/// answers the submitted document augmented with the scan, so a component's own
/// `externalReferences` survive — and the scan adds no address beside the
/// publisher page of what it reported.
#[tokio::test]
async fn a_link_the_caller_submitted_is_echoed_unchanged() {
    let (engine, router) = emulator();
    let records = catalog(&engine);
    let submitted_url = "https://components.example.invalid/log4j-core/2.12.1";
    let document = bom(vec![json!({
        "type": "library",
        "bom-ref": "comp-log4j",
        "name": "log4j-core",
        "version": "2.12.1",
        "purl": "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
        "externalReferences": [ { "type": "distribution", "url": submitted_url } ],
    })]);
    let request = json!({ "sbom": document, "outputFormat": CYCLONEDX_FORMAT });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");
    let mut answered = BTreeSet::new();
    string_values(&response["sbom"], &mut answered);
    assert!(
        answered.contains(submitted_url),
        "the report preserves the component reference the caller submitted, got {}",
        response["sbom"]
    );
    assert_report_links(
        "a caller-submitted component reference",
        CYCLONEDX_FORMAT,
        &request,
        &response,
        &records,
    );
}
