// SPDX-License-Identifier: CC0-1.0

//! What a `ScanSbom` CycloneDX report says about the SCANNER that answered it,
//! end-to-end over HTTP.
//!
//! The rule under test: `metadata.tools.services[]`'s entry for the scanning
//! service reports that service's own build identity — the '+'-joined build ids
//! of the parts a scan's answer is produced by, read from the service's state
//! ([`ComAmazonawsInspectorscanInspectorscanState::scanner_build`]) — so it is
//! stable across reads of an unchanged scanner, moves when the advisory data the
//! scanner matches against moves, and is never a value belonging to something
//! other than the scanner (the serving crate's manifest version, say).

use super::scan_sbom::{bom, component, emulator, post_scan};
use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::state::{
    build_index, ComAmazonawsInspectorscanInspectorscanState, ReleaseVersion, VersionRange,
    VulnRecord,
};
use std::sync::Arc;

/// The service entry the scan reports itself under.
const SERVICE_NAME: &str = "Amazon Inspector Scan SBOM API";

/// How many hex digits one build id of the reported identity is written with.
const BUILD_ID_HEX_DIGITS: usize = 8;

/// `metadata.tools.services[]`'s `version` for the scanning service.
fn reported_scanner_version(report: &Value) -> String {
    let services = report["metadata"]["tools"]["services"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    services
        .iter()
        .find(|service| service["name"].as_str() == Some(SERVICE_NAME))
        .and_then(|service| service["version"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            panic!("the report must name {SERVICE_NAME:?} in metadata.tools.services, got {report}")
        })
}

/// Scan a one-component document named by `package` and answer the scanner
/// version the CycloneDX report attributes to the scanning service.
async fn scanner_version_for(
    router: &smithy_server_runtime::MultiServiceRouter,
    package: &str,
) -> String {
    let request = json!({
        "sbom": bom(vec![component(
            "ref-a",
            package,
            "1.0.0",
            &format!("pkg:npm/{package}@1.0.0"),
        )]),
        "outputFormat": "CYCLONE_DX_1_5",
    });
    let (status, response) = post_scan(router, &request).await;
    assert_eq!(
        status, 200,
        "case {package:?}: expected HTTP 200, got {status}: {response}"
    );
    reported_scanner_version(&response["sbom"])
}

/// An advisory the seeded catalog does not carry, pushed into the SAME store the
/// handler reads, so the advisory data the scanner matches against changes.
fn inject_advisory(engine: &Arc<engine::InMemoryEngine>, id: &str, package_name: &str) {
    let state = engine.state::<ComAmazonawsInspectorscanInspectorscanState>();
    let mut state = state.lock();
    state.vulnerability_db.push(VulnRecord {
        cve_id: id.to_string(),
        source: "NVD".to_string(),
        source_url: None,
        package_name: package_name.to_string(),
        package_type: "npm".to_string(),
        // npm packages are not scoped by a distribution.
        distribution: None,
        affected: VersionRange::before(ReleaseVersion::new(9, 0, 0)),
        fixed_in_version: Some("9.0.0".to_string()),
        cvss_v3_score: 5.0,
        cvss_v3_vector: "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:L/I:N/A:N".to_string(),
        nvd_severity: "MEDIUM".to_string(),
        inspector_score: 5.0,
        inspector_cvss_vector: "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:L/I:N/A:N".to_string(),
        inspector_severity: "MEDIUM".to_string(),
        // The scoring system has published no estimate for this fixture, which
        // is reported as no EPSS rating at all.
        epss_score: None,
        cwes: vec![20],
        description: format!("{id} applies to {package_name}."),
        published: "2024-01-01T00:00:00Z".to_string(),
        modified: "2024-02-01T00:00:00Z".to_string(),
    });
    let rebuilt = build_index(&state.vulnerability_db);
    state.index = rebuilt;
}

/// The reported identity is three `+`-joined build ids, each written as lowercase
/// hex digits.
fn assert_build_id_form(case: &str, version: &str) {
    let ids: Vec<&str> = version.split('+').collect();
    assert_eq!(
        ids.len(),
        3,
        "case {case:?}: expected three '+'-joined build ids, got {version:?}"
    );
    for id in &ids {
        assert_eq!(
            id.len(),
            BUILD_ID_HEX_DIGITS,
            "case {case:?}: expected each build id to be {BUILD_ID_HEX_DIGITS} hex digits, got \
             {id:?} in {version:?}"
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "case {case:?}: expected a lowercase hex build id, got {id:?} in {version:?}"
        );
    }
}

#[tokio::test]
async fn the_report_names_the_scanners_own_build_identity() {
    let (engine, router) = emulator();

    let first = scanner_version_for(&router, "left-pad").await;
    assert_build_id_form("a scan of a document naming one package", &first);

    // It describes the SCANNER, so it is not the version of whatever crate is
    // serving the request.
    assert_ne!(
        first,
        env!("CARGO_PKG_VERSION"),
        "case \"the serving crate's manifest version\": the reported scanner build must not be \
         the serving package's version, got {first:?}"
    );

    // It is the identity the service's own state holds.
    let held = engine
        .state::<ComAmazonawsInspectorscanInspectorscanState>()
        .lock()
        .scanner_build()
        .version();
    assert_eq!(
        first, held,
        "case \"the identity held in state\": the report must carry it, got {first:?}"
    );

    // Stable across reads: another scan, of another document, reports the same
    // scanner.
    let again = scanner_version_for(&router, "right-pad").await;
    assert_eq!(
        first, again,
        "case \"a second scan against the same scanner\": the identity must not move, got \
         {again:?}"
    );

    // And it MOVES when the advisory data the scanner matches against changes:
    // the two scanner-component ids stay, the advisory-snapshot id does not.
    inject_advisory(&engine, "CVE-2099-0001", "example-package");
    let after = scanner_version_for(&router, "left-pad").await;
    assert_build_id_form("a scan against changed advisory data", &after);
    assert_ne!(
        first, after,
        "case \"changed advisory data\": the reported identity must move with the data the \
         scanner matches against, got {after:?}"
    );
    let before_ids: Vec<&str> = first.split('+').collect();
    let after_ids: Vec<&str> = after.split('+').collect();
    assert_eq!(
        before_ids[..2],
        after_ids[..2],
        "case \"changed advisory data\": only the advisory-snapshot id moves, got {first:?} then \
         {after:?}"
    );
}
