// SPDX-License-Identifier: CC0-1.0

//! The two Inspector-native report forms of `ScanSbom`, end-to-end over HTTP.
//!
//! The rules under test:
//!
//! * a report's `vulnerabilities` member is a LIST of vulnerability node
//!   OBJECTS, in both Inspector forms — never a map keyed by a node's own `id` —
//!   and every node names at least the advisory `id` and the `severity` it was
//!   rated at, so a client reads the member by indexing and iterating the nodes
//!   in it;
//! * the `INSPECTOR_ALT` form reports one node per MATCHED ADVISORY, listing
//!   every component of the scan that advisory affects, and carries
//!   `vendorAdvisories` on every node — projected from the matched advisory
//!   record's own publisher, vendor rating and instants — while
//!   the `INSPECTOR` form carries that member nowhere in its document, which is
//!   what distinguishes the two forms;
//! * both Inspector forms are distinguishable from the CycloneDX document.
//!
//! The advisory catalog these rows are written against is
//! `src/state.rs::seed_vulnerability_db`; the row that needs an advisory the
//! catalog does not carry injects one into the same store the handler reads.

use super::scan_report_annotations::inject_advisory;
use super::scan_sbom::{bom, component, emulator, post_scan};
use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::state::ComAmazonawsInspectorscanInspectorscanState;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The severity spellings a report rates a node at: the four named CVSS
/// severities plus the `other` bucket a finding rated outside them falls in
/// (`src/ops/scan_sbom.rs`, `SeverityBucket`).
const REPORTED_SEVERITIES: &[&str] = &["critical", "high", "medium", "low", "other"];

/// The member that distinguishes the `INSPECTOR_ALT` form from the `INSPECTOR`
/// one.
const VENDOR_ADVISORIES: &str = "vendorAdvisories";

/// One row of the form table: a document, and the vulnerability nodes the
/// `INSPECTOR_ALT` scan of it must report.
#[derive(Debug)]
struct AltCase {
    what: &'static str,
    sbom: Value,
    /// One entry per expected node: the advisory id, the severity the node is
    /// rated at, and the purls of every component that node must list under
    /// `vulnerablePackages`.
    nodes: Vec<(&'static str, &'static str, Vec<&'static str>)>,
}

/// An advisory the seeded catalog does not carry, in the SAME store the handler
/// reads, so a scan matches it like any other. Its node reports the vendor
/// advisory projected from that record, exactly as a seeded one does. The record
/// is injected by the shared helper in `scan_report_annotations.rs`.
const INJECTED_ADVISORY: &str = "CVE-2024-99001";

fn inject_unpublished_advisory(engine: &Arc<engine::InMemoryEngine>) {
    inject_advisory(
        engine,
        INJECTED_ADVISORY,
        "npm",
        "left-pad",
        "MEDIUM",
        vec![20],
    );
}

/// The advisory publisher the store records for `cve_id`.
fn vendor_source(engine: &Arc<engine::InMemoryEngine>, cve_id: &str) -> String {
    let state = engine.state::<ComAmazonawsInspectorscanInspectorscanState>();
    let state = state.lock();
    state
        .vulnerability_db
        .iter()
        .find(|record| record.cve_id == cve_id)
        .map(|record| record.source.clone())
        .unwrap_or_else(|| panic!("the store holds an advisory for {cve_id}"))
}

/// Whether `member` appears ANYWHERE in `document`, at any depth.
fn carries_member(document: &Value, member: &str) -> bool {
    match document {
        Value::Object(entries) => {
            entries.contains_key(member)
                || entries.values().any(|value| carries_member(value, member))
        }
        Value::Array(items) => items.iter().any(|item| carries_member(item, member)),
        _ => false,
    }
}

/// The node for `id` in a report's `vulnerabilities` list, and `None` when the
/// list carries none (or is not a list at all).
pub(crate) fn node_for<'r>(report: &'r Value, id: &str) -> Option<&'r Value> {
    report["vulnerabilities"]
        .as_array()?
        .iter()
        .find(|node| node["id"].as_str() == Some(id))
}

/// A component this scan reports a finding for, keyed by its purl.
fn purls_of(node: &Value) -> BTreeSet<String> {
    node["vulnerablePackages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|package| package["purl"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Documents spanning what the alternate form generalises over: one component,
/// one advisory across two components, several advisories in several
/// ecosystems, nothing matched at all, and an advisory the store holds no
/// vendor publication for.
fn alt_cases() -> Vec<AltCase> {
    vec![
        AltCase {
            what: "one maven component inside the log4shell range",
            sbom: bom(vec![component(
                "comp-log4j",
                "log4j-core",
                "2.12.1",
                "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
            )]),
            nodes: vec![(
                "CVE-2021-44228",
                "critical",
                vec!["pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1"],
            )],
        },
        AltCase {
            what: "two versions of one package, both inside one advisory's range",
            sbom: bom(vec![
                component(
                    "comp-log4j-a",
                    "log4j-core",
                    "2.12.1",
                    "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
                ),
                component(
                    "comp-log4j-b",
                    "log4j-core",
                    "2.14.1",
                    "pkg:maven/org.apache.logging.log4j/log4j-core@2.14.1",
                ),
            ]),
            nodes: vec![(
                "CVE-2021-44228",
                "critical",
                vec![
                    "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
                    "pkg:maven/org.apache.logging.log4j/log4j-core@2.14.1",
                ],
            )],
        },
        AltCase {
            what: "three advisories across three ecosystems, beside a clean component",
            sbom: bom(vec![
                component("comp-urllib3", "urllib3", "1.26.4", "pkg:pypi/urllib3@1.26.4"),
                component(
                    "comp-gin",
                    "gin",
                    "v1.7.0",
                    "pkg:golang/github.com/gin-gonic/gin@v1.7.0",
                ),
                component("comp-lodash", "lodash", "4.17.20", "pkg:npm/lodash@4.17.20"),
                component("comp-clean", "is-odd", "3.0.1", "pkg:npm/is-odd@3.0.1"),
            ]),
            nodes: vec![
                (
                    "CVE-2020-28483",
                    "high",
                    vec!["pkg:golang/github.com/gin-gonic/gin@v1.7.0"],
                ),
                ("CVE-2021-23337", "high", vec!["pkg:npm/lodash@4.17.20"]),
                ("CVE-2021-33503", "high", vec!["pkg:pypi/urllib3@1.26.4"]),
            ],
        },
        AltCase {
            what: "a component no advisory applies to",
            sbom: bom(vec![component(
                "comp-clean",
                "lodash",
                "4.17.21",
                "pkg:npm/lodash@4.17.21",
            )]),
            nodes: Vec::new(),
        },
    ]
}

#[tokio::test]
async fn the_inspector_alt_report_lists_vulnerability_nodes_carrying_vendor_advisories() {
    let (engine, router) = emulator();
    for case in alt_cases() {
        let request = json!({ "sbom": case.sbom, "outputFormat": "INSPECTOR_ALT" });
        let (status, response) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {response}",
            case.what
        );
        let report = &response["sbom"];

        // The member is a LIST of node objects, so indexing and iterating it
        // yields vulnerability nodes rather than advisory-id strings.
        let nodes = report["vulnerabilities"].as_array().unwrap_or_else(|| {
            panic!(
                "case {:?}: vulnerabilities must be a JSON array, got {}",
                case.what, report["vulnerabilities"]
            )
        });
        assert_eq!(
            nodes.len(),
            case.nodes.len(),
            "case {:?}: expected {} vulnerability node(s), got {}",
            case.what,
            case.nodes.len(),
            report["vulnerabilities"]
        );
        for (position, node) in nodes.iter().enumerate() {
            assert!(
                node.is_object(),
                "case {:?}: vulnerabilities[{position}] must be an object, got {node}",
                case.what
            );
        }

        for (id, severity, purls) in &case.nodes {
            let node = node_for(report, id).unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a node for {id}, got {}",
                    case.what, report["vulnerabilities"]
                )
            });
            assert_eq!(
                node["severity"].as_str(),
                Some(*severity),
                "case {:?}: expected {id} rated {severity}, got {node}",
                case.what
            );
            assert!(
                REPORTED_SEVERITIES.contains(&node["severity"].as_str().unwrap_or_default()),
                "case {:?}: {id} must be rated in one of {REPORTED_SEVERITIES:?}, got {node}",
                case.what
            );

            // One node per matched advisory: every component the advisory
            // affects is listed on that one node.
            let expected: BTreeSet<String> = purls.iter().map(|purl| purl.to_string()).collect();
            let reported = purls_of(node);
            assert_eq!(
                reported, expected,
                "case {:?}: expected {id} to list {expected:?}, got {reported:?}",
                case.what
            );

            // And it carries the vendor advisories for it, projected from the
            // matched record rather than invented: one entry for the advisory
            // the node reports.
            let advisories = node[VENDOR_ADVISORIES].as_array().unwrap_or_else(|| {
                panic!(
                    "case {:?}: {id} must carry {VENDOR_ADVISORIES} as an array, got {node}",
                    case.what
                )
            });
            assert_eq!(
                advisories.len(),
                1,
                "case {:?}: {id} reports one vendor advisory, got {node}",
                case.what
            );
            let source = vendor_source(&engine, id);
            for entry in advisories {
                assert_eq!(
                    entry["source"].as_str(),
                    Some(source.as_str()),
                    "case {:?}: every vendor advisory of {id} names its publisher, got {entry}",
                    case.what
                );
                assert_eq!(
                    entry["id"].as_str(),
                    Some(*id),
                    "case {:?}: every vendor advisory names its advisory, got {entry}",
                    case.what
                );
                assert!(
                    entry["vendorCreatedAt"].as_str().is_some()
                        && entry["vendorUpdatedAt"].as_str().is_some(),
                    "case {:?}: every vendor advisory of {id} carries its instants, got {entry}",
                    case.what
                );
            }
        }
    }
}

#[tokio::test]
async fn an_advisory_the_catalog_does_not_carry_still_reports_its_vendor_advisory() {
    let (engine, router) = emulator();
    inject_unpublished_advisory(&engine);
    let request = json!({
        "sbom": bom(vec![component("comp-left-pad", "left-pad", "1.0.0", "pkg:npm/left-pad@1.0.0")]),
        "outputFormat": "INSPECTOR_ALT",
    });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");
    let node = node_for(&response["sbom"], INJECTED_ADVISORY).unwrap_or_else(|| {
        panic!(
            "expected a node for the injected advisory, got {}",
            response["sbom"]["vulnerabilities"]
        )
    });
    assert_eq!(
        node[VENDOR_ADVISORIES],
        json!([{
            "id": INJECTED_ADVISORY,
            "source": vendor_source(&engine, INJECTED_ADVISORY),
            "severity": "medium",
            "vendorCreatedAt": "2024-01-01T00:00:00Z",
            "vendorUpdatedAt": "2024-01-02T00:00:00Z",
        }]),
        "an advisory injected into the store reports the vendor advisory its record \
         holds, got {node}"
    );
}

#[tokio::test]
async fn only_the_alternate_form_carries_vendor_advisories() {
    let (_engine, router) = emulator();
    for case in alt_cases() {
        // The plain Inspector form: a list of nodes too, with the member
        // nowhere in the document.
        let request = json!({ "sbom": case.sbom, "outputFormat": "INSPECTOR" });
        let (status, plain) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?} in INSPECTOR: expected HTTP 200, got {status}: {plain}",
            case.what
        );
        assert!(
            plain["sbom"]["vulnerabilities"].is_array(),
            "case {:?} in INSPECTOR: vulnerabilities must be a JSON array, got {}",
            case.what,
            plain["sbom"]["vulnerabilities"]
        );
        assert!(
            !carries_member(&plain["sbom"], VENDOR_ADVISORIES),
            "case {:?}: the INSPECTOR form carries {VENDOR_ADVISORIES} nowhere, got {}",
            case.what,
            plain["sbom"]
        );

        let request = json!({ "sbom": case.sbom, "outputFormat": "INSPECTOR_ALT" });
        let (status, alt) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?} in INSPECTOR_ALT: expected HTTP 200, got {status}: {alt}",
            case.what
        );
        // The two forms are distinguishable exactly when the scan matched
        // something: the member rides on a node.
        assert_eq!(
            carries_member(&alt["sbom"], VENDOR_ADVISORIES),
            !case.nodes.is_empty(),
            "case {:?}: {VENDOR_ADVISORIES} is carried on every INSPECTOR_ALT node and nowhere \
             else, got {}",
            case.what,
            alt["sbom"]
        );

        // And both Inspector forms are distinguishable from the CycloneDX
        // document, which answers the submitted BOM augmented with the scan.
        let request = json!({ "sbom": case.sbom, "outputFormat": "CYCLONE_DX_1_5" });
        let (status, cyclonedx) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?} in CYCLONE_DX_1_5: expected HTTP 200, got {status}: {cyclonedx}",
            case.what
        );
        assert_eq!(
            cyclonedx["sbom"]["bomFormat"].as_str(),
            Some("CycloneDX"),
            "case {:?}: the CycloneDX form answers a CycloneDX BOM, got {}",
            case.what,
            cyclonedx["sbom"]
        );
        for (form, report) in [("INSPECTOR", &plain["sbom"]), ("INSPECTOR_ALT", &alt["sbom"])] {
            assert!(
                report["bomFormat"].is_null() && report["scanId"].as_str().is_some(),
                "case {:?}: the {form} form answers the Inspector-native report, not the \
                 submitted document, got {report}",
                case.what
            );
        }
    }
}

#[tokio::test]
async fn a_repeat_of_the_alternate_scan_answers_the_identical_report() {
    let (_engine, router) = emulator();
    for case in alt_cases() {
        let request = json!({ "sbom": case.sbom, "outputFormat": "INSPECTOR_ALT" });
        let (status, first) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {first}",
            case.what
        );
        let (status, second) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {second}",
            case.what
        );
        assert_eq!(
            first, second,
            "case {:?}: an @idempotent repeat answers the identical report; got {first} then \
             {second}",
            case.what
        );
    }
}
