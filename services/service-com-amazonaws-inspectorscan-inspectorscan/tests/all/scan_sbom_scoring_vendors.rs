// SPDX-License-Identifier: CC0-1.0

//! The SCORING VENDORS a finding is enriched with, driven end-to-end over the same router the
//! emulator serves with.
//!
//! RULE: a finding reports one rating per scoring vendor the matched advisory holds a score
//! from — the advisory's own publisher and Amazon Inspector on the 0-10 CVSS severity scale,
//! and the exploit-prediction vendor on its own 0-1 probability scale — and a vendor the
//! advisory holds no score from is reported with no rating at all rather than a claimed one.
//! The two scales are not interchangeable: a reader prioritising by exploit probability needs
//! the probability, not the severity.
//!
//! Every value is a projection of the matched advisory record, so the same score reaches the
//! CycloneDX form's `ratings[]` and the Inspector-native form's `epss`: the two forms cannot
//! disagree about one advisory.
//!
//! The rows differ along what the rule generalises over: which advisory matched (six, across
//! npm, PyPI, Maven, Go and OS packages), how exploited it is (probabilities from well under
//! one percent to well over nine tenths), and whether the advisory carries an exploit estimate
//! at all — the last row is an advisory injected into the same store the handler reads with no
//! estimate, which must be reported with no such rating.
//!
//! The advisory catalog these rows are written against is
//! `src/state.rs::seed_vulnerability_db`.

use serde_json::{json, Value};

use super::scan_report_annotations::inject_advisory;
use super::scan_sbom::{bom, component, emulator, post_scan};

/// The scoring vendor an exploit-probability rating is reported under.
const EPSS: &str = "EPSS";

/// The vendors a seeded advisory holds a score from, in the order a finding reports them.
const SEEDED_VENDORS: &[&str] = &["NVD", "Amazon Inspector", EPSS];

/// One row: a component coordinate, and the advisory a scan of it must match.
struct Row {
    case: &'static str,
    name: &'static str,
    version: &'static str,
    purl: &'static str,
    cve: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "an npm package with a low exploit probability",
        name: "lodash",
        version: "4.17.20",
        purl: "pkg:npm/lodash@4.17.20",
        cve: "CVE-2021-23337",
    },
    Row {
        case: "a PyPI package",
        name: "urllib3",
        version: "1.26.4",
        purl: "pkg:pypi/urllib3@1.26.4",
        cve: "CVE-2021-33503",
    },
    Row {
        case: "a Maven package with a very high exploit probability",
        name: "log4j-core",
        version: "2.12.1",
        purl: "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
        cve: "CVE-2021-44228",
    },
    Row {
        case: "another Maven package with a very high exploit probability",
        name: "commons-collections",
        version: "3.2.1",
        purl: "pkg:maven/commons-collections/commons-collections@3.2.1",
        cve: "CVE-2015-7501",
    },
    Row {
        case: "a Go module",
        name: "gin",
        version: "v1.7.0",
        purl: "pkg:golang/github.com/gin-gonic/gin@v1.7.0",
        cve: "CVE-2020-28483",
    },
    Row {
        case: "an OS package",
        name: "glibc",
        version: "2.37",
        purl: "pkg:rpm/redhat/glibc@2.37",
        cve: "CVE-2023-4911",
    },
];

/// The one finding a scan of `purl` reports in the CycloneDX form.
async fn cyclonedx_finding(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    version: &str,
    purl: &str,
    case: &str,
) -> Value {
    let request = json!({
        "sbom": bom(vec![component("ref-1", name, version, purl)]),
        "outputFormat": "CYCLONE_DX_1_5",
    });
    let (status, response) = post_scan(router, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status}: {response}"
    );
    let findings = response["sbom"]["vulnerabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        findings.len(),
        1,
        "case {case:?}: expected exactly one finding, got {findings:?}"
    );
    findings[0].clone()
}

/// The rating `vendor` published on `finding`, or `None` when the finding carries none.
fn rating_of(finding: &Value, vendor: &str) -> Option<Value> {
    finding["ratings"]
        .as_array()?
        .iter()
        .find(|rating| rating["source"]["name"].as_str() == Some(vendor))
        .cloned()
}

#[tokio::test]
async fn a_finding_reports_one_rating_per_scoring_vendor_the_advisory_holds() {
    let (_engine, router) = emulator();
    for row in ROWS {
        let finding = cyclonedx_finding(&router, row.name, row.version, row.purl, row.case).await;
        assert_eq!(
            finding["id"].as_str(),
            Some(row.cve),
            "case {:?}: expected the advisory for this coordinate, got {finding}",
            row.case
        );
        let vendors: Vec<&str> = finding["ratings"]
            .as_array()
            .map(|ratings| {
                ratings
                    .iter()
                    .map(|rating| rating["source"]["name"].as_str().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            vendors, SEEDED_VENDORS,
            "case {:?}: expected one rating per scoring vendor, got {}",
            row.case, finding["ratings"]
        );

        let epss = rating_of(&finding, EPSS).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected an {EPSS} rating, got {}",
                row.case, finding["ratings"]
            )
        });
        let probability = epss["score"].as_f64().unwrap_or_else(|| {
            panic!("case {:?}: expected an {EPSS} score, got {epss}", row.case)
        });
        assert!(
            (0.0..=1.0).contains(&probability),
            "case {:?}: an exploit probability lies in 0.0..=1.0, got {probability}",
            row.case
        );

        // The two scales are separate quantities: the severity ratings are the 0-10 CVSS
        // scores, and the probability is neither of them.
        for vendor in ["NVD", "Amazon Inspector"] {
            let rating = rating_of(&finding, vendor).unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a {vendor} rating, got {}",
                    row.case, finding["ratings"]
                )
            });
            let severity_score = rating["score"].as_f64().unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a {vendor} score, got {rating}",
                    row.case
                )
            });
            assert!(
                (0.0..=10.0).contains(&severity_score),
                "case {:?}: a CVSS base score lies in 0.0..=10.0, got {severity_score}",
                row.case
            );
            assert!(
                (severity_score - probability).abs() > f64::EPSILON,
                "case {:?}: the {vendor} severity {severity_score} and the exploit \
                 probability {probability} are different quantities and must not be one \
                 value reported twice",
                row.case
            );
        }
    }
}

/// The same score reaches both report forms, because both project the one advisory record.
#[tokio::test]
async fn both_report_forms_project_the_same_exploit_probability() {
    let (_engine, router) = emulator();
    for row in ROWS {
        let finding = cyclonedx_finding(&router, row.name, row.version, row.purl, row.case).await;
        let from_cyclonedx = rating_of(&finding, EPSS)
            .and_then(|rating| rating["score"].as_f64())
            .unwrap_or_else(|| panic!("case {:?}: expected an {EPSS} rating", row.case));

        let request = json!({
            "sbom": bom(vec![component("ref-1", row.name, row.version, row.purl)]),
            "outputFormat": "INSPECTOR",
        });
        let (status, response) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {response}",
            row.case
        );
        let node = &response["sbom"]["vulnerabilities"][0];
        assert_eq!(
            node["id"].as_str(),
            Some(row.cve),
            "case {:?}: expected the same advisory in the Inspector form, got {node}",
            row.case
        );
        let from_inspector = node["epss"]["score"].as_f64().unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the Inspector form to carry the probability, got {node}",
                row.case
            )
        });
        assert!(
            (from_inspector - from_cyclonedx).abs() <= f64::EPSILON,
            "case {:?}: both forms project one record, so the probability must agree: \
             CycloneDX {from_cyclonedx}, Inspector {from_inspector}",
            row.case
        );
    }
}

/// An advisory the scoring system has published no estimate for is reported with NO such
/// rating: the data is the record's, so an absent estimate is absent from the report rather
/// than filled in with a number nothing published.
#[tokio::test]
async fn an_advisory_with_no_estimate_carries_no_such_rating() {
    let (engine, router) = emulator();
    // `inject_advisory` pushes a record with no exploit estimate into the same store the
    // handler reads, so the scan matches it like any other.
    inject_advisory(
        &engine,
        "CVE-2024-00001",
        "npm",
        "left-pad",
        "MEDIUM",
        vec![20],
    );
    let finding = cyclonedx_finding(
        &router,
        "left-pad",
        "1.0.0",
        "pkg:npm/left-pad@1.0.0",
        "an advisory with no exploit estimate",
    )
    .await;
    assert_eq!(
        finding["id"].as_str(),
        Some("CVE-2024-00001"),
        "expected the injected advisory, got {finding}"
    );
    let vendors: Vec<&str> = finding["ratings"]
        .as_array()
        .map(|ratings| {
            ratings
                .iter()
                .map(|rating| rating["source"]["name"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        vendors,
        vec!["NVD", "Amazon Inspector"],
        "an advisory with no exploit estimate carries only the vendors it holds a score \
         from, got {}",
        finding["ratings"]
    );

    let request = json!({
        "sbom": bom(vec![component(
            "ref-1",
            "left-pad",
            "1.0.0",
            "pkg:npm/left-pad@1.0.0",
        )]),
        "outputFormat": "INSPECTOR",
    });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");
    assert!(
        response["sbom"]["vulnerabilities"][0]["epss"].is_null(),
        "the Inspector form reports no estimate either, got {}",
        response["sbom"]["vulnerabilities"][0]
    );
}
