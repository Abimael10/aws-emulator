// SPDX-License-Identifier: CC0-1.0

//! The COORDINATE a `ScanSbom` lookup is keyed by, end-to-end over HTTP.
//!
//! The rule under test: a component is looked up by the whole coordinate its
//! ecosystem names a package with — including, where the ecosystem scopes its
//! packages by a distribution, the purl `namespace` that names that distribution
//! — so a component is ruled on exactly when the advisory data holds a rule for
//! that whole coordinate, and one report never both reports a finding for a
//! component and declares that component skipped.
//!
//! Which ecosystems scope their packages by a distribution (the ones reported
//! under the `OS` package manager) is `SCANNED_ECOSYSTEMS` in
//! `src/ops/scan_sbom.rs`; the advisory catalog these rows are written against,
//! and the distribution each of its records was published for, is
//! `src/state.rs::seed_vulnerability_db`.

use super::scan_sbom::{bom, component, emulator, post_scan};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The namespace the scan's own annotations live under in a CycloneDX report.
const NAMESPACE: &str = "amazon:inspector:sbom_scanner";

/// What a scan reports for a component whose purl names no package it looks up.
const UNSUPPORTED_NAMESPACE: &str = "Component skipped: unsupported purl namespace.";

/// What a scan reports for a component whose purl names a package it looks up
/// but whose coordinate the advisory data holds no rule for.
const NO_RULES: &str = "Component skipped: no supported rules found.";

/// One row of the coordinate table: a component, the vulnerability ids the scan
/// must report for it, and the sentence it must be reported as skipped with
/// (`None` when the scan ruled on it).
#[derive(Debug)]
struct CoordinateCase {
    what: &'static str,
    purl: &'static str,
    name: &'static str,
    version: &'static str,
    findings: &'static [&'static str],
    skipped: Option<&'static str>,
}

/// Rows differing along the part of the coordinate that decides the lookup: the
/// distribution (the one the advisory was published for, another one, none at
/// all, and the same one spelled in another case), the ecosystem the same
/// name/version pair is stated in, the form of the version, and — for the
/// ecosystems that do NOT scope their packages by a distribution — the namespace
/// that is part of the package's own name there.
fn cases() -> Vec<CoordinateCase> {
    vec![
        // The distribution the advisory was published for: ruled on, affected.
        CoordinateCase {
            what: "the distribution the advisory was published for",
            purl: "pkg:deb/debian/openssl@3.0.6",
            name: "openssl",
            version: "3.0.6",
            findings: &["CVE-2022-3602"],
            skipped: None,
        },
        CoordinateCase {
            // A Debian epoch and package release decorate the version, not the
            // coordinate, so the same advisory still applies.
            what: "the same coordinate with an epoch-and-release version",
            purl: "pkg:deb/debian/openssl@1:3.0.6-1",
            name: "openssl",
            version: "1:3.0.6-1",
            findings: &["CVE-2022-3602"],
            skipped: None,
        },
        CoordinateCase {
            // purl writes a distribution namespace in lowercase, so a document
            // spelling it otherwise names the same distribution.
            what: "the same distribution spelled in another case",
            purl: "pkg:deb/DEBIAN/openssl@3.0.6",
            name: "openssl",
            version: "3.0.6",
            findings: &["CVE-2022-3602"],
            skipped: None,
        },
        CoordinateCase {
            // Ruled on and clean: the coordinate resolves and the release is
            // past the advisory's fixed version.
            what: "the publishing distribution, past the fixed release",
            purl: "pkg:deb/debian/openssl@3.0.7",
            name: "openssl",
            version: "3.0.7",
            findings: &[],
            skipped: None,
        },
        // Another distribution: another package, with its own advisories.
        CoordinateCase {
            what: "another distribution's build of the same package",
            purl: "pkg:deb/ubuntu/openssl@3.0.6",
            name: "openssl",
            version: "3.0.6",
            findings: &[],
            skipped: Some(NO_RULES),
        },
        CoordinateCase {
            what: "a third distribution's build of the same package",
            purl: "pkg:deb/raspbian/openssl@3.0.6",
            name: "openssl",
            version: "3.0.6",
            findings: &[],
            skipped: Some(NO_RULES),
        },
        CoordinateCase {
            what: "no distribution at all",
            purl: "pkg:deb/openssl@3.0.6",
            name: "openssl",
            version: "3.0.6",
            findings: &[],
            skipped: Some(UNSUPPORTED_NAMESPACE),
        },
        // The same package name in another OS ecosystem.
        CoordinateCase {
            what: "the same name and release as an apk package",
            purl: "pkg:apk/alpine/openssl@3.0.6",
            name: "openssl",
            version: "3.0.6",
            findings: &[],
            skipped: Some(NO_RULES),
        },
        // The rpm catalog entry, and the coordinates around it.
        CoordinateCase {
            what: "an rpm package in its publishing distribution",
            purl: "pkg:rpm/redhat/glibc@2.37",
            name: "glibc",
            version: "2.37",
            findings: &["CVE-2023-4911"],
            skipped: None,
        },
        CoordinateCase {
            what: "an rpm package in another distribution",
            purl: "pkg:rpm/fedora/glibc@2.37",
            name: "glibc",
            version: "2.37",
            findings: &[],
            skipped: Some(NO_RULES),
        },
        CoordinateCase {
            what: "an rpm package with no distribution",
            purl: "pkg:rpm/glibc@2.37",
            name: "glibc",
            version: "2.37",
            findings: &[],
            skipped: Some(UNSUPPORTED_NAMESPACE),
        },
        CoordinateCase {
            what: "an rpm-catalogued name as a deb package",
            purl: "pkg:deb/debian/glibc@2.37",
            name: "glibc",
            version: "2.37",
            findings: &[],
            skipped: Some(NO_RULES),
        },
        // Ecosystems that do not scope their packages by a distribution: the
        // namespace there is part of the package's own name.
        CoordinateCase {
            what: "an npm package, an ecosystem with no distribution",
            purl: "pkg:npm/lodash@4.17.20",
            name: "lodash",
            version: "4.17.20",
            findings: &["CVE-2021-23337"],
            skipped: None,
        },
        CoordinateCase {
            what: "the same npm name under a scope",
            purl: "pkg:npm/%40scope/lodash@4.17.20",
            name: "lodash",
            version: "4.17.20",
            findings: &[],
            skipped: Some(NO_RULES),
        },
        CoordinateCase {
            what: "a maven artifact in its own group",
            purl: "pkg:maven/org.apache.logging.log4j/log4j-core@2.14.1",
            name: "log4j-core",
            version: "2.14.1",
            findings: &["CVE-2021-44228"],
            skipped: None,
        },
        CoordinateCase {
            what: "the same maven artifact id in another group",
            purl: "pkg:maven/com.example/log4j-core@2.14.1",
            name: "log4j-core",
            version: "2.14.1",
            findings: &[],
            skipped: Some(NO_RULES),
        },
    ]
}

/// The value of the CycloneDX property named `name`, if the array carries it.
fn property(properties: &Value, name: &str) -> Option<String> {
    properties
        .as_array()?
        .iter()
        .find(|entry| entry["name"].as_str() == Some(name))
        .and_then(|entry| entry["value"].as_str().map(str::to_string))
}

/// The array at `value`, or an empty one when it carries no array there.
fn array(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// The vulnerability ids an Inspector-form report names `bom_ref` in, through
/// the `vulnerablePackages[]` entries of its nodes.
fn inspector_findings_by_ref(report: &Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_ref: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for node in array(&report["vulnerabilities"]) {
        let id = node["id"].as_str().unwrap_or_default().to_string();
        for package in array(&node["vulnerablePackages"]) {
            if let Some(bom_ref) = package["bomRef"].as_str() {
                by_ref
                    .entry(bom_ref.to_string())
                    .or_default()
                    .insert(id.clone());
            }
        }
    }
    by_ref
}

/// The vulnerability ids a CycloneDX report names each `bom-ref` in, through its
/// `affects[].ref` entries.
fn cyclonedx_findings_by_ref(report: &Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_ref: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for node in array(&report["vulnerabilities"]) {
        let id = node["id"].as_str().unwrap_or_default().to_string();
        for affected in array(&node["affects"]) {
            if let Some(bom_ref) = affected["ref"].as_str() {
                by_ref
                    .entry(bom_ref.to_string())
                    .or_default()
                    .insert(id.clone());
            }
        }
    }
    by_ref
}

/// The skip sentence an Inspector-form report carries for `purl`, if any.
fn message_for(report: &Value, purl: &str) -> Option<String> {
    report["messages"]
        .as_array()?
        .iter()
        .find(|entry| entry["purl"].as_str() == Some(purl))
        .and_then(|entry| entry["info_message"].as_str().map(str::to_string))
}

/// The ids in `expected`, as a set.
fn expected_set(expected: &[&str]) -> BTreeSet<String> {
    expected.iter().map(|id| (*id).to_string()).collect()
}

#[tokio::test]
async fn an_advisory_applies_only_to_the_whole_coordinate_it_was_published_for() {
    let (_engine, router) = emulator();
    for case in cases() {
        let request = json!({
            "sbom": bom(vec![component("ref-a", case.name, case.version, case.purl)]),
            "outputFormat": "INSPECTOR",
        });
        let (status, response) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {response}",
            case.what
        );
        let report = &response["sbom"];

        let reported: BTreeSet<String> = array(&report["vulnerabilities"])
            .iter()
            .map(|node| node["id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported,
            expected_set(case.findings),
            "case {:?}: expected the findings {:?} for {}, got {:?}",
            case.what,
            case.findings,
            case.purl,
            report["vulnerabilities"]
        );

        assert_eq!(
            message_for(report, case.purl).as_deref(),
            case.skipped,
            "case {:?}: expected the skip decision {:?} for {}, got {:?}",
            case.what,
            case.skipped,
            case.purl,
            report["messages"]
        );
    }
}

#[tokio::test]
async fn one_report_never_both_rules_on_a_component_and_declares_it_skipped() {
    // Every row of the coordinate table in ONE document, so each decision is
    // taken beside the others, and in BOTH report forms: the decision is per
    // component, so a component reported as skipped carries no finding and a
    // component carrying a finding is reported as skipped in neither form.
    let cases = cases();
    let components: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(position, case)| {
            component(
                &format!("ref-{position}"),
                case.name,
                case.version,
                case.purl,
            )
        })
        .collect();

    let (_engine, router) = emulator();

    let request = json!({
        "sbom": bom(components.clone()),
        "outputFormat": "INSPECTOR",
    });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");
    let report = &response["sbom"];
    let by_ref = inspector_findings_by_ref(report);
    for (position, case) in cases.iter().enumerate() {
        // The scan assigns every component of the report its own `bom-ref`, in
        // document order, starting at 1.
        let bom_ref = format!("comp-{}", position + 1);
        let reported = by_ref.get(&bom_ref).cloned().unwrap_or_default();
        assert_eq!(
            reported,
            expected_set(case.findings),
            "case {:?}: expected the findings {:?} on {bom_ref}, got {:?}",
            case.what,
            case.findings,
            report["vulnerabilities"]
        );
        let skipped = message_for(report, case.purl);
        assert_eq!(
            skipped.as_deref(),
            case.skipped,
            "case {:?}: expected the skip decision {:?} for {}, got {skipped:?}",
            case.what,
            case.skipped,
            case.purl
        );
        assert!(
            skipped.is_none() || reported.is_empty(),
            "case {:?}: {bom_ref} is reported as skipped ({skipped:?}) and carries the findings \
             {reported:?}",
            case.what
        );
    }

    let request = json!({
        "sbom": bom(components),
        "outputFormat": "CYCLONE_DX_1_5",
    });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");
    let report = &response["sbom"];
    let by_ref = cyclonedx_findings_by_ref(report);
    for (position, case) in cases.iter().enumerate() {
        let bom_ref = format!("comp-{}", position + 1);
        let reported = by_ref.get(&bom_ref).cloned().unwrap_or_default();
        assert_eq!(
            reported,
            expected_set(case.findings),
            "case {:?}: expected the findings {:?} on {bom_ref}, got {:?}",
            case.what,
            case.findings,
            report["vulnerabilities"]
        );
        let properties = &report["components"][position]["properties"];
        let info = property(properties, &format!("{NAMESPACE}:info"));
        assert_eq!(
            info.as_deref(),
            case.skipped,
            "case {:?}: expected the annotation {:?} on {bom_ref}, got {properties}",
            case.what,
            case.skipped
        );
        assert!(
            info.is_none() || reported.is_empty(),
            "case {:?}: {bom_ref} is annotated as skipped ({info:?}) and carries the findings \
             {reported:?}",
            case.what
        );
    }
}
