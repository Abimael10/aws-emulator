Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: ba126ac941bc10c8fac660a876f0f74074caa177

# The stored-severity classification is implemented twice (cyclonedx_severity and SeverityBucket::of), so the two report forms can drift on the same advisory field

_Filed: 2026-11-25T16:34:07Z_
_Operation: ScanSbom | Class: maintainability_

> _Note: this issue was identified against the code base at commit `ba126ac941bc10c8fac660a876f0f74074caa177`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

One rule — which stored severity spellings are the four named severities, after case folding, and what an unrecognised spelling falls back to — is implemented twice in this crate's own handler, once per report form:

* `cyclonedx_severity` (src/ops/scan_sbom.rs:1284-1291) lowercases the stored value and matches `critical | high | medium | low | info | none`, maps `informational` to `info`, and falls back to `unknown`. It renders the CycloneDX form's `vulnerabilities[].ratings[].severity`.
* `SeverityBucket::of` + `SeverityBucket::as_str` (src/ops/scan_sbom.rs:1311-1330) lowercases the stored value and matches `critical | high | medium | low`, with everything else falling into `Other`/`"other"`. It renders the Inspector forms' `vulnerabilities[].severity`, the `vendorAdvisories[].severity` of the `INSPECTOR_ALT` form, and the per-bucket totals of `vulnerability_count` / the namespaced `metadata.properties` summary.

Both tables read the SAME two stored fields (`VulnRecord::nvd_severity` and `VulnRecord::inspector_severity`): `nvd_severity` goes through `cyclonedx_severity` at src/ops/scan_sbom.rs:1535 and through `SeverityBucket::of` at src/ops/scan_sbom.rs:1850; `inspector_severity` goes through `cyclonedx_severity` at :1544 and through `SeverityBucket::of` at :1358 and :1926. So teaching the service a stored spelling (or changing which spellings count as one of the four named severities) has to be done in two places, and missing one leaves one report form rating an advisory the other form rates differently.

That is exactly the drift the crate elsewhere states it keeps out, in the same file and for the same pair of report forms: the CVSS notation table is deliberately ONE table read by both forms so that "a notation added here reaches the CycloneDX `ratings[].method` and the Inspector-native `version` together instead of one form rating a vector the other drops" (src/ops/scan_sbom.rs:1220-1227) — a shape a previous maintainability issue on this operation already collapsed into `cvss_notation`.

Fix direction: derive both renderings from ONE table of stored severity spellings (the four named severities), with each form keeping its own fallback spelling (`unknown`/`info`/`none` for the CycloneDX schema, `other` for the Inspector bucket). No response value and no existing test changes: no seeded advisory and no test fixture carries a spelling that reaches the divergent arms (`informational`, `info`, `none`), and the one test row that uses an unrecognised spelling (`UNTRIAGED`) asserts only the bucket side.

## Evidence

Copy 1 — the CycloneDX spelling, services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:1278-1291:
```
/// The CycloneDX `ratings[].severity` spelling of a stored severity.
///
/// The CycloneDX 1.5 schema enumerates the severity values in lowercase
/// (`critical`, `high`, `medium`, `low`, `info`, `none`, `unknown`); the store
/// keeps Inspector's uppercase spelling, so this is the one place they are
/// mapped.
fn cyclonedx_severity(stored: &str) -> String {
    let lowered = stored.to_ascii_lowercase();
    match lowered.as_str() {
        "critical" | "high" | "medium" | "low" | "info" | "none" => lowered,
        "informational" => "info".to_string(),
        _ => "unknown".to_string(),
    }
}
```
(note the doc's own claim: "this is the one place they are mapped".)

Copy 2 — the Inspector spelling and the summary bucket, src/ops/scan_sbom.rs:1308-1330:
```
impl SeverityBucket {
    /// The bucket a stored severity falls in. The store keeps Inspector's
    /// uppercase spelling; a report spells a severity in lowercase.
    fn of(stored: &str) -> Self {
        match stored.to_ascii_lowercase().as_str() {
            "critical" => Self::Critical,
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Other,
        }
    }

    /// The bucket's reported spelling.
    fn as_str(self) -> &'static str {
```

The SAME stored field read through both tables — `nvd_severity`, src/ops/scan_sbom.rs:1530-1538:
```
    if !advisory.cvss_v3_vector.is_empty() {
        ratings.push(json!({
            // The advisory's own publisher is what scored it.
            "source": rating_source,
            "score": advisory.cvss_v3_score,
            "severity": cyclonedx_severity(&advisory.nvd_severity),
```
versus src/ops/scan_sbom.rs:1846-1853:
```
fn vendor_advisories(advisory: &VulnRecord) -> Vec<Value> {
    vec![json!({
        "id": advisory.cve_id,
        "source": advisory.source,
        "severity": SeverityBucket::of(&advisory.nvd_severity).as_str(),
```
and `inspector_severity`, src/ops/scan_sbom.rs:1544 (`"severity": cyclonedx_severity(&advisory.inspector_severity),`) versus src/ops/scan_sbom.rs:1926 (`"severity": SeverityBucket::of(&advisory.inspector_severity).as_str(),`) and src/ops/scan_sbom.rs:1358 (`let counter = match SeverityBucket::of(&finding.advisory.inspector_severity) {`).

The principle this crate states for the sibling table it DID unify, src/ops/scan_sbom.rs:1220-1227:
```
/// The CVSS notations a vector's version prefix can state: each specification
/// version as the vector spells it, paired with the method value the CycloneDX
/// 1.5 schema enumerates for that version.
///
/// ONE table, read by both report forms through [`cvss_notation`], so a notation
/// added here reaches the CycloneDX `ratings[].method` and the Inspector-native
/// `version` together instead of one form rating a vector the other drops.
const CVSS_NOTATIONS: &[(&str, &str)] = &[("4.0", "CVSSv4"), ("3.1", "CVSSv31"), ("3.0", "CVSSv3")];
```

Test coverage that pins both sides (so a single-table refactor is verifiable and must keep them green):
* the CycloneDX rating spelling — tests/all/scan_sbom.rs:505-509:
```
        assert_eq!(
            rating["severity"].as_str(),
            Some("critical"),
            "case {source:?}: CycloneDX 1.5 spells severities in lowercase, got {rating}"
        );
```
* the Inspector node spelling — tests/all/scan_inspector_alt_form.rs:239-245 (`node["severity"].as_str()` against `REPORTED_SEVERITIES`);
* the bucket table, including an unrecognised stored spelling — tests/all/scan_report_annotations.rs:312-351:
```
        ("an unrated advisory", "UNTRIAGED", vec![400], "other", "0"),
...
        let counts = &response["sbom"]["vulnerability_count"];
        assert_eq!(
            counts[bucket].as_u64(),
            Some(1),
```
No seeded advisory carries a spelling outside `HIGH`/`CRITICAL` (src/state.rs:635-891), so the divergent arms (`informational`, `info`, `none`) are unobserved by any response or test today.

## Steps to Reproduce

1. Open services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs and read `cyclonedx_severity` (lines 1284-1291) and `SeverityBucket::of`/`as_str` (lines 1311-1330): both case-fold the same stored severity field and both enumerate `critical`/`high`/`medium`/`low`, each with its own fallback.
2. Follow the call sites: `nvd_severity` is rendered through `cyclonedx_severity` at line 1535 (CycloneDX `ratings[].severity`) and through `SeverityBucket::of` at line 1850 (`INSPECTOR_ALT` `vendorAdvisories[].severity`); `inspector_severity` is rendered through `cyclonedx_severity` at line 1544 and through `SeverityBucket::of` at lines 1358 and 1926.
3. Compare with the crate's own stated rule for the neighbouring table at lines 1220-1227 ("ONE table, read by both report forms ... instead of one form rating a vector the other drops") — the severity classification is the copy that rule was written against.
4. Confirm the drift surface is live but untested: tests/all/scan_report_annotations.rs:315 injects an advisory rated `UNTRIAGED` and asserts only the Inspector bucket (`other`) and the CycloneDX malicious-package total, never the CycloneDX `ratings[].severity` the second table produces for that same record.
