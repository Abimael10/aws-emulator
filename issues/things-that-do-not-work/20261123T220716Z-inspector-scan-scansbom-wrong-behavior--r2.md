Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 04b7737d40b7fda90f20de8a2ba64e1a389f2093

# Findings are ordered by the lexicographic bom-ref string, so a 10+-component document reports comp-10's finding before comp-2's while the same response's messages stay in document order

_Filed: 2026-11-23T22:07:16Z_
_Operation: ScanSbom | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `04b7737d40b7fda90f20de8a2ba64e1a389f2093`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler assigns every component of the report a sequential document-local ref `comp-<1-based position in the walk>` and then orders the matched findings by comparing those refs as STRINGS. Rust's `String` ordering is byte-lexicographic, so once a document carries ten or more components the order becomes `comp-1`, `comp-10`, `comp-11`, …, `comp-2`, i.e. the finding for the tenth component is reported before the finding for the second.

That contradicts the crate's own statements in two places:

1. `inspector_report`'s doc says the `INSPECTOR` form "reports one node per match, in the report's order". The report's order of components is the order `resolve_component_tree` builds them in — `out` is "filled in document order" with the ref carrying "the entry's 1-based position in the walk" — so ordering by the ref's *string* is not the report's order for any document with 10+ components.
2. Inside ONE response the two component-keyed lists are then ordered by two different rules: `vulnerabilities[]` is in lexicographic-ref order, while the `messages[]` list of skipped components is built by iterating `parsed.components`, i.e. in document order. A client reading both lists of the same scan sees the components enumerated in two incompatible sequences, although both lists key on the same `comp-N` refs.

The same sorted order is what the CycloneDX form's `vulnerabilities[]` carries (next to a `components[]` array that IS in document order) and what each `INSPECTOR_ALT` node's `vulnerablePackages[]` is built from, so all three report forms are affected.

Nothing in the crate justifies the lexicographic form: the sort comment's stated goal is only determinism across repeats, which numeric ordering satisfies equally.

## Evidence

The sort — a String-tuple comparison over the refs (services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:2064-2070):
```
            // Deterministic order, so a repeat of this `@idempotent` scan
            // reports the identical set in the identical sequence: by the
            // affected component's ref, then by vulnerability id.
            findings.sort_by(|a, b| {
                (&a.component.bom_ref, &a.advisory.cve_id)
                    .cmp(&(&b.component.bom_ref, &b.advisory.cve_id))
            });
```
The refs being compared are decimal positions carried in a string (src/ops/scan_sbom.rs:906-908, :917, :1002-1003):
```
/// The scan IDENTIFIES the components of the report itself, by a sequential
/// document-local ref ([`COMPONENT_REF_PREFIX`] plus the entry's 1-based
/// position in the walk)
```
```
/// first level is matched like any other. `out` is filled in document order,
```
```
        *next_ref += 1;
        let bom_ref = format!("{COMPONENT_REF_PREFIX}{next_ref}");
```
The rule the ordering breaks (src/ops/scan_sbom.rs:1871-1872):
```
/// ([`InspectorForm`]): `INSPECTOR` reports one node per match, in the report's
/// order, with no vendor-advisory data;
```
The sibling list in the SAME response, ordered by document position instead (src/ops/scan_sbom.rs:2121-2128):
```
        let messages: Vec<Value> = parsed
            .components
            .iter()
            .filter_map(|component| {
                skipped
                    .get(&component.bom_ref)
                    .map(|reason| (component, *reason))
            })
```
Test coverage: the multi-component suite never pins an order — it compares finding ids as sets and its largest row carries five components, so the lexicographic order is unobserved. tests/all/scan_sbom_component_fan_out.rs:205-214:
```
        let expected: BTreeSet<&str> = row.entries.iter().filter_map(|entry| entry.cve).collect();
        let actual: BTreeSet<&str> = findings
            .iter()
            .filter_map(|finding| finding["id"].as_str())
            .collect();
        assert_eq!(
            actual, expected,
```
The two seeded npm advisories used in the reproduction are src/state.rs:594-615 (`cve_id: "CVE-2021-23337"`, `package_name: "lodash"`, affected before 4.17.21) and src/state.rs:616-635 (`cve_id: "CVE-2021-44906"`, `package_name: "minimist"`, affected before 1.2.6).

## Steps to Reproduce

1. POST /scan/sbom with outputFormat INSPECTOR and an SBOM of ten components (bomFormat "CycloneDX", specVersion "1.5"): position 2 is {"type":"library","name":"lodash","version":"4.17.20","purl":"pkg:npm/lodash@4.17.20"}, position 10 is {"type":"library","name":"minimist","version":"1.2.5","purl":"pkg:npm/minimist@1.2.5"}, and the other eight are clean npm components.
2. Read the 200 body: the components are stamped comp-1 .. comp-10 in document order, but sbom.vulnerabilities[0].id is CVE-2021-44906 (comp-10) and sbom.vulnerabilities[1].id is CVE-2021-23337 (comp-2) — the tenth component's finding precedes the second's, which is not "the report's order" the handler doc states.
3. Observe the same body's sbom.messages[] entries for the clean/uncovered components: they appear in document order (comp-1, comp-3, comp-4, …), so one response enumerates the same components in two different sequences.
4. Repeat with outputFormat CYCLONE_DX_1_5: sbom.components[] is in document order while sbom.vulnerabilities[*].affects[0].ref runs comp-10 before comp-2.
