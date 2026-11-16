Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 52f475a4504a9c514f88c04338789fa36aa3e85d

# The CVSS vector prefix table is implemented twice (cvss_method and cvss_version), so the two report forms can drift on the same vector

_Filed: 2026-11-16T22:41:07Z_
_Operation: ScanSbom | Class: maintainability_

> _Note: this issue was identified against the code base at commit `52f475a4504a9c514f88c04338789fa36aa3e85d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Two adjacent helpers in the same handler each hold their own copy of the same computation — reading a CVSS vector's version from its own `CVSS:<x.y>/` prefix. `cvss_method` maps the prefix to the CycloneDX `ratings[].method` spelling, `cvss_version` maps the same three prefixes to the version string the Inspector-native form reports. The prefix list is duplicated verbatim, and the two have different fall-throughs (`"other"` vs `None`), so a notation added to one copy and not the other makes the CycloneDX form rate a vector the Inspector form drops entirely (the CycloneDX rating is emitted whenever the vector is non-empty, while `inspectorScoreDetails.adjustedCvss` and `cvss[]` are emitted only when `cvss_version` recognises the prefix).

The mapping is 1:1 (`"4.0"`→`CVSSv4`, `"3.1"`→`CVSSv31`, `"3.0"`→`CVSSv3`, unrecognised→`other`), so one helper can be expressed over the other with every response byte-identical and every existing test unchanged.

## Evidence

Copy 1 — services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:203-213:
```
fn cvss_method(vector: &str) -> &'static str {
    if vector.starts_with("CVSS:4.0/") {
        "CVSSv4"
    } else if vector.starts_with("CVSS:3.1/") {
        "CVSSv31"
    } else if vector.starts_with("CVSS:3.0/") {
        "CVSSv3"
    } else {
        "other"
    }
}
```
Copy 2 — src/ops/scan_sbom.rs:217-227:
```
fn cvss_version(vector: &str) -> Option<&'static str> {
    if vector.starts_with("CVSS:4.0/") {
        Some("4.0")
    } else if vector.starts_with("CVSS:3.1/") {
        Some("3.1")
    } else if vector.starts_with("CVSS:3.0/") {
        Some("3.0")
    } else {
        None
    }
}
```
The two consumers that can drift apart: src/ops/scan_sbom.rs:1312-1320 (`if !advisory.cvss_v3_vector.is_empty() { … "method": cvss_method(&advisory.cvss_v3_vector) … }`) and src/ops/scan_sbom.rs:1674-1692 (`let adjusted = cvss_version(&advisory.inspector_cvss_vector).map(…)` / `let base: Vec<Value> = cvss_version(&advisory.cvss_v3_vector).map(…)`).

Existing tests covering both copies (they must stay unchanged): tests/all/scan_sbom.rs:490-504 asserts `rating["method"] == "CVSSv31"` and the `CVSS:3.1/` vector in the CycloneDX form; tests/all/scan_sbom_scoring_vendors.rs (whole file, e.g. `a_finding_reports_one_rating_per_scoring_vendor_the_advisory_holds` at :130 and `both_report_forms_project_the_same_exploit_probability` at :203) drives both report forms over the same advisories.

## Steps to Reproduce

1. Read services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:203-227 and note the identical `starts_with("CVSS:4.0/" | "CVSS:3.1/" | "CVSS:3.0/")` ladder in both helpers.
2. Note the divergent fall-throughs: `cvss_method` answers `"other"` for an unrecognised notation (so :1312 still emits a CycloneDX rating), while `cvss_version` answers `None` (so :1674-1692 emits no `adjustedCvss` and an empty `cvss[]`).
3. Confirm the drift surface: adding a prefix to one ladder only changes one form's output for the same stored advisory, while `cargo test -p service-com-amazonaws-inspectorscan-inspectorscan` keeps passing.
