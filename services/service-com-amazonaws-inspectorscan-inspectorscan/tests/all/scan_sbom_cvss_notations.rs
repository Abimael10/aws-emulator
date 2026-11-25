// SPDX-License-Identifier: CC0-1.0

//! The CVSS NOTATION a report reads off a stored vector, driven end-to-end over the same
//! router the emulator serves with.
//!
//! RULE: the notation a CVSS vector is written in is read once from the vector's own
//! `CVSS:<version>/` prefix, so both report forms project the same notation for the same
//! stored vector — the CycloneDX form's `ratings[].method` names the schema spelling of that
//! version exactly when the Inspector-native form's `inspectorScoreDetails.adjustedCvss` and
//! `cvss[]` report the version, and a vector in a notation neither knows is rated `other` by
//! the one form and carries no version in the other.
//!
//! The rows differ along what the rule generalises over: which notation the vector states
//! (each version the one table names), a stated version the table does not name, a vector
//! with no version prefix at all, and a prefix with no field separator after the version.
//!
//! The method spellings are the `vulnerability.ratings[].method` values the CycloneDX 1.5
//! schema enumerates (as `src/ops/scan_sbom.rs` cites them); the versions are what the
//! vector's own prefix states, per the CVSS vector-string grammar.

use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::state::{
    build_index, ComAmazonawsInspectorscanInspectorscanState, ReleaseVersion, VersionRange,
    VulnRecord,
};
use std::sync::Arc;

use super::scan_sbom::{bom, component, emulator, post_scan};

/// The method the CycloneDX schema's own catch-all, used for a vector in a notation the
/// report forms do not recognise.
const OTHER: &str = "other";

/// One row: a vector a stored advisory holds, and the notation both report forms must read
/// off it.
struct Row {
    case: &'static str,
    /// The package the advisory applies to, which is also how a scan selects this row.
    package: &'static str,
    /// The id the injected advisory is stored under.
    cve: &'static str,
    vector: &'static str,
    /// The CycloneDX `ratings[].method` for this vector.
    method: &'static str,
    /// The CVSS version the Inspector-native form reports for this vector, or `None` when
    /// the vector states no notation the forms know.
    version: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a CVSS v4.0 vector",
        package: "notation-v40",
        cve: "CVE-2024-80001",
        vector: "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N",
        method: "CVSSv4",
        version: Some("4.0"),
    },
    Row {
        case: "a CVSS v3.1 vector",
        package: "notation-v31",
        cve: "CVE-2024-80002",
        vector: "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
        method: "CVSSv31",
        version: Some("3.1"),
    },
    Row {
        case: "a CVSS v3.0 vector",
        package: "notation-v30",
        cve: "CVE-2024-80003",
        vector: "CVSS:3.0/AV:N/AC:L/PR:N/UI:N/S:U/C:L/I:L/A:L",
        method: "CVSSv3",
        version: Some("3.0"),
    },
    Row {
        case: "a vector stating a version the table does not name",
        package: "notation-v20",
        cve: "CVE-2024-80004",
        vector: "CVSS:2.0/AV:N/AC:L/Au:N/C:P/I:P/A:P",
        method: OTHER,
        version: None,
    },
    Row {
        case: "a vector with no version prefix at all",
        package: "notation-bare",
        cve: "CVE-2024-80005",
        vector: "AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
        method: OTHER,
        version: None,
    },
    Row {
        case: "a prefix stating a version with no field separator after it",
        package: "notation-truncated",
        cve: "CVE-2024-80006",
        vector: "CVSS:3.1",
        method: OTHER,
        version: None,
    },
];

/// An advisory holding `vector` as BOTH its publisher's and its Inspector-adjusted vector,
/// pushed into the same store the handler reads so a scan of `package` matches it.
fn inject_advisory_with_vector(engine: &Arc<engine::InMemoryEngine>, row: &Row) {
    let Row {
        package, cve, vector, ..
    } = row;
    let state = engine.state::<ComAmazonawsInspectorscanInspectorscanState>();
    let mut state = state.lock();
    state.vulnerability_db.push(VulnRecord {
        cve_id: cve.to_string(),
        source: "NVD".to_string(),
        source_url: None,
        package_name: package.to_string(),
        package_type: "npm".to_string(),
        distribution: None,
        affected: VersionRange::before(ReleaseVersion::new(9, 0, 0)),
        fixed_in_version: Some("9.0.0".to_string()),
        cvss_v3_score: 7.5,
        cvss_v3_vector: vector.to_string(),
        nvd_severity: "HIGH".to_string(),
        inspector_score: 7.0,
        inspector_cvss_vector: vector.to_string(),
        inspector_severity: "HIGH".to_string(),
        epss_score: Some(0.25),
        cwes: vec![79],
        description: format!("An advisory for {package}."),
        published: "2024-01-01T00:00:00Z".to_string(),
        modified: "2024-01-02T00:00:00Z".to_string(),
    });
    let index = build_index(&state.vulnerability_db);
    state.index = index;
}

/// The one finding a scan of `package` reports in `output_format`.
async fn finding_for(
    router: &smithy_server_runtime::MultiServiceRouter,
    package: &str,
    output_format: &str,
    case: &str,
) -> Value {
    let request = json!({
        "sbom": bom(vec![component(
            "ref-1",
            package,
            "1.0.0",
            &format!("pkg:npm/{package}@1.0.0"),
        )]),
        "outputFormat": output_format,
    });
    let (status, response) = post_scan(router, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?} ({output_format}): expected HTTP 200, got {status}: {response}"
    );
    let findings = response["sbom"]["vulnerabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        findings.len(),
        1,
        "case {case:?} ({output_format}): expected exactly one finding, got {findings:?}"
    );
    findings[0].clone()
}

/// Every report form reads the notation off the one vector, so the CycloneDX method and the
/// Inspector-native version cannot disagree about it.
#[tokio::test]
async fn both_report_forms_read_one_cvss_notation_from_the_same_vector() {
    let (engine, router) = emulator();
    for row in ROWS {
        inject_advisory_with_vector(&engine, row);
    }

    for row in ROWS {
        let cyclonedx = finding_for(&router, row.package, "CYCLONE_DX_1_5", row.case).await;
        assert_eq!(
            cyclonedx["id"].as_str(),
            Some(row.cve),
            "case {:?}: expected the injected advisory, got {cyclonedx}",
            row.case
        );
        // Both CVSS ratings are scored from this one vector, so each names the same method.
        let methods: Vec<&str> = cyclonedx["ratings"]
            .as_array()
            .map(|ratings| {
                ratings
                    .iter()
                    .filter(|rating| rating["vector"].as_str() == Some(row.vector))
                    .map(|rating| rating["method"].as_str().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            methods,
            vec![row.method, row.method],
            "case {:?}: expected both CVSS ratings to name {:?}, got {}",
            row.case,
            row.method,
            cyclonedx["ratings"]
        );

        let inspector = finding_for(&router, row.package, "INSPECTOR", row.case).await;
        let adjusted = &inspector["inspectorScoreDetails"]["adjustedCvss"];
        assert_eq!(
            adjusted["version"].as_str(),
            row.version,
            "case {:?}: expected the adjusted score to report version {:?}, got {adjusted}",
            row.case,
            row.version
        );
        let base = inspector["cvss"].as_array().cloned().unwrap_or_default();
        let base_versions: Vec<Option<&str>> = base
            .iter()
            .map(|entry| entry["version"].as_str())
            .collect();
        assert_eq!(
            base_versions,
            row.version.into_iter().map(Some).collect::<Vec<_>>(),
            "case {:?}: expected cvss[] to report {:?}, got {}",
            row.case,
            row.version,
            inspector["cvss"]
        );

        // The drift the two forms must not have: a notation one form rates is a notation the
        // other versions, and a notation neither knows is reported as such by both.
        let rated = !methods.is_empty() && methods.iter().all(|method| *method != OTHER);
        let versioned = !adjusted["version"].is_null();
        assert_eq!(
            rated, versioned,
            "case {:?}: the CycloneDX method {:?} and the Inspector version {} read the same \
             vector {:?} and must recognise it alike",
            row.case, row.method, adjusted["version"], row.vector
        );
    }
}
