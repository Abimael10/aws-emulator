// SPDX-License-Identifier: CC0-1.0

//! How a scan of a MULTI-COMPONENT document attributes findings, driven end-to-end over the
//! same router the emulator serves with.
//!
//! RULE: every component of the submitted document is matched against the advisory data
//! independently of every other, so a document carrying several independently vulnerable
//! components reports a finding for EACH of them — each finding's `affects[]` naming the
//! bom-ref the report assigned that component, and no component's advisories swallowed by
//! another's. That is what makes "no findings" mean something: a component reported clean was
//! ruled on, rather than passed over because a sibling matched first.
//!
//! The rows differ along what the rule generalises over: how many components carry an advisory
//! (two, three, four), whether they share an ecosystem (two Maven artifacts) or span several
//! (Maven, npm, PyPI, Go, OS packages), where the vulnerable entries sit relative to clean
//! ones (first, last, interleaved), and how deep they sit (top level, and below an assembly).
//!
//! Every coordinate and every expected advisory comes from `src/state.rs::seed_vulnerability_db`.

use serde_json::{json, Value};
use std::collections::BTreeSet;

use super::scan_sbom::{bom, component, emulator, post_scan};

/// One component of a row: the coordinate, and the advisory a scan of it must report — or
/// `None` for a coordinate the advisory data holds nothing matching for, which must be
/// reported clean.
struct Entry {
    name: &'static str,
    version: &'static str,
    purl: &'static str,
    cve: Option<&'static str>,
}

/// One row: a document's components, in the order the request carries them.
struct Row {
    case: &'static str,
    entries: &'static [Entry],
}

/// A clean coordinate the advisory data holds no rule for.
const CLEAN_NPM: Entry = Entry {
    name: "left-pad",
    version: "1.0.0",
    purl: "pkg:npm/left-pad@1.0.0",
    cve: None,
};

/// A coordinate the advisory data HAS a rule for, at a release past the affected range: ruled
/// on and clean.
const FIXED_LODASH: Entry = Entry {
    name: "lodash",
    version: "4.17.21",
    purl: "pkg:npm/lodash@4.17.21",
    cve: None,
};

const LOG4J: Entry = Entry {
    name: "log4j-core",
    version: "2.12.1",
    purl: "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
    cve: Some("CVE-2021-44228"),
};

const COMMONS_COLLECTIONS: Entry = Entry {
    name: "commons-collections",
    version: "3.2.1",
    purl: "pkg:maven/commons-collections/commons-collections@3.2.1",
    cve: Some("CVE-2015-7501"),
};

const JACKSON: Entry = Entry {
    name: "jackson-databind",
    version: "2.9.10",
    purl: "pkg:maven/com.fasterxml.jackson.core/jackson-databind@2.9.10",
    cve: Some("CVE-2020-36518"),
};

const LODASH: Entry = Entry {
    name: "lodash",
    version: "4.17.20",
    purl: "pkg:npm/lodash@4.17.20",
    cve: Some("CVE-2021-23337"),
};

const URLLIB3: Entry = Entry {
    name: "urllib3",
    version: "1.26.4",
    purl: "pkg:pypi/urllib3@1.26.4",
    cve: Some("CVE-2021-33503"),
};

const GIN: Entry = Entry {
    name: "gin",
    version: "v1.7.0",
    purl: "pkg:golang/github.com/gin-gonic/gin@v1.7.0",
    cve: Some("CVE-2020-28483"),
};

const GLIBC: Entry = Entry {
    name: "glibc",
    version: "2.37",
    purl: "pkg:rpm/redhat/glibc@2.37",
    cve: Some("CVE-2023-4911"),
};

const ROWS: &[Row] = &[
    Row {
        case: "two vulnerable Maven artifacts of one ecosystem",
        entries: &[LOG4J, COMMONS_COLLECTIONS],
    },
    Row {
        case: "the same two in the reverse order",
        entries: &[COMMONS_COLLECTIONS, LOG4J],
    },
    Row {
        case: "two vulnerable components of different ecosystems",
        entries: &[LOG4J, LODASH],
    },
    Row {
        case: "three vulnerable components across three ecosystems",
        entries: &[COMMONS_COLLECTIONS, URLLIB3, GIN],
    },
    Row {
        case: "four vulnerable components, one per ecosystem",
        entries: &[JACKSON, LODASH, URLLIB3, GLIBC],
    },
    Row {
        case: "a clean component first, two vulnerable ones after it",
        entries: &[CLEAN_NPM, LOG4J, COMMONS_COLLECTIONS],
    },
    Row {
        case: "vulnerable components interleaved with clean and ruled-on ones",
        entries: &[
            LOG4J,
            FIXED_LODASH,
            COMMONS_COLLECTIONS,
            CLEAN_NPM,
            URLLIB3,
        ],
    },
    Row {
        case: "a clean component last",
        entries: &[COMMONS_COLLECTIONS, GIN, CLEAN_NPM],
    },
];

/// The bom-ref the report assigned the component named `name` at `version`.
fn reported_ref(components: &Value, name: &str, version: &str) -> Option<String> {
    components.as_array()?.iter().find_map(|entry| {
        (entry["name"].as_str() == Some(name) && entry["version"].as_str() == Some(version))
            .then(|| entry["bom-ref"].as_str().map(str::to_string))
            .flatten()
    })
}

/// The refs a finding's `affects[]` names.
fn affected_refs(finding: &Value) -> BTreeSet<String> {
    finding["affects"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|affected| affected["ref"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn every_vulnerable_component_gets_its_own_finding() {
    let (_engine, router) = emulator();
    for row in ROWS {
        let components: Vec<Value> = row
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                component(
                    &format!("ref-{index}"),
                    entry.name,
                    entry.version,
                    entry.purl,
                )
            })
            .collect();
        let request = json!({
            "sbom": bom(components),
            "outputFormat": "CYCLONE_DX_1_5",
        });
        let (status, response) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {response}",
            row.case
        );
        let reported = &response["sbom"]["components"];
        let findings = response["sbom"]["vulnerabilities"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        // The findings reported are exactly the advisories the document's components carry,
        // with no component's advisory missing and none invented.
        let expected: BTreeSet<&str> = row.entries.iter().filter_map(|entry| entry.cve).collect();
        let actual: BTreeSet<&str> = findings
            .iter()
            .filter_map(|finding| finding["id"].as_str())
            .collect();
        assert_eq!(
            actual, expected,
            "case {:?}: expected one finding per vulnerable component, got {}",
            row.case, response["sbom"]["vulnerabilities"]
        );

        for entry in row.entries {
            let component_ref = reported_ref(reported, entry.name, entry.version)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the report must carry {} {}, got {reported}",
                        row.case, entry.name, entry.version
                    )
                });
            let attributed: Vec<&Value> = findings
                .iter()
                .filter(|finding| affected_refs(finding).contains(&component_ref))
                .collect();
            match entry.cve {
                Some(cve) => {
                    let ids: BTreeSet<&str> = attributed
                        .iter()
                        .filter_map(|finding| finding["id"].as_str())
                        .collect();
                    assert!(
                        ids.contains(cve),
                        "case {:?}: expected {cve} attributed to {} {} (bom-ref \
                         {component_ref}), got {ids:?}",
                        row.case,
                        entry.name,
                        entry.version
                    );
                }
                None => assert!(
                    attributed.is_empty(),
                    "case {:?}: {} {} carries no advisory and must be reported clean, got \
                     {attributed:?}",
                    row.case,
                    entry.name,
                    entry.version
                ),
            }
        }
    }
}

/// The fan-out is over the whole component TREE, not the top level: a vulnerable package
/// below an assembly gets its own finding beside a vulnerable sibling at the top.
#[tokio::test]
async fn a_component_below_an_assembly_gets_its_own_finding() {
    let (_engine, router) = emulator();
    let assembly = json!({
        "type": "application",
        "bom-ref": "ref-assembly",
        "name": "service",
        "version": "1.0.0",
        "components": [component(
            "ref-nested",
            COMMONS_COLLECTIONS.name,
            COMMONS_COLLECTIONS.version,
            COMMONS_COLLECTIONS.purl,
        )],
    });
    let request = json!({
        "sbom": bom(vec![
            component("ref-top", LOG4J.name, LOG4J.version, LOG4J.purl),
            assembly,
        ]),
        "outputFormat": "CYCLONE_DX_1_5",
    });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status}: {response}");

    let findings = response["sbom"]["vulnerabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids: BTreeSet<&str> = findings
        .iter()
        .filter_map(|finding| finding["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        ["CVE-2015-7501", "CVE-2021-44228"]
            .into_iter()
            .collect::<BTreeSet<&str>>(),
        "both the top-level and the nested component must be reported, got {}",
        response["sbom"]["vulnerabilities"]
    );

    let nested_ref = response["sbom"]["components"][1]["components"][0]["bom-ref"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "the report must carry the nested component, got {}",
                response["sbom"]["components"]
            )
        })
        .to_string();
    let attributed: BTreeSet<&str> = findings
        .iter()
        .filter(|finding| affected_refs(finding).contains(&nested_ref))
        .filter_map(|finding| finding["id"].as_str())
        .collect();
    assert!(
        attributed.contains("CVE-2015-7501"),
        "expected the nested component's own advisory attributed to bom-ref {nested_ref}, got \
         {attributed:?}"
    );
}

/// The aggregate severity counters the CycloneDX report carries are totals over the SAME
/// findings, so a document with two critical components counts two criticals.
#[tokio::test]
async fn the_scan_summary_counts_every_attributed_finding() {
    let (_engine, router) = emulator();
    // (case, the components, the critical total the summary must report)
    let rows: &[(&str, Vec<Entry>, u64)] = &[
        ("one critical component", vec![LOG4J], 1),
        (
            "two critical components",
            vec![LOG4J, COMMONS_COLLECTIONS],
            2,
        ),
        (
            "two critical components beside a clean one",
            vec![LOG4J, COMMONS_COLLECTIONS, CLEAN_NPM],
            2,
        ),
        ("no critical component", vec![LODASH], 0),
    ];
    for (case, entries, expected_criticals) in rows {
        let components: Vec<Value> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                component(
                    &format!("ref-{index}"),
                    entry.name,
                    entry.version,
                    entry.purl,
                )
            })
            .collect();
        let request = json!({
            "sbom": bom(components),
            "outputFormat": "CYCLONE_DX_1_5",
        });
        let (status, response) = post_scan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status}: {response}"
        );
        let counted = response["sbom"]["metadata"]["properties"]
            .as_array()
            .and_then(|properties| {
                properties
                    .iter()
                    .find(|property| {
                        property["name"].as_str()
                            == Some("amazon:inspector:sbom_scanner:critical_vulnerabilities")
                    })
                    .and_then(|property| property["value"].as_str())
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the scan summary properties, got {}",
                    response["sbom"]["metadata"]
                )
            });
        assert_eq!(
            counted, *expected_criticals,
            "case {case:?}: expected {expected_criticals} critical findings counted, got \
             {counted}"
        );
    }
}
