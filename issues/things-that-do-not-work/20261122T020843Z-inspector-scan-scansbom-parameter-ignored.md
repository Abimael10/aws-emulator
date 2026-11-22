Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 0b600564062992274bd01b2b8270b52f52f6e224

# OS-package purl distribution namespace is required but its value is discarded, so one report both reports a finding for a component and declares it skipped as unresolvable

_Filed: 2026-11-22T02:08:43Z_
_Operation: ScanSbom | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `0b600564062992274bd01b2b8270b52f52f6e224`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

For the OS ecosystems (`deb`, `rpm`, `apk`) the scan reads the purl `namespace` (the distribution), REQUIRES it to be present to consider the coordinate resolvable, and then throws its VALUE away before the advisory lookup — `canonical_package_name` returns the bare `name` for those three types, and `lookup_key` keys the advisory index on `(package_type, package_name)` only. The namespace therefore acts as a presence flag and never as part of the coordinate, which produces two results the crate's own text says are impossible:

1. A SELF-CONTRADICTING REPORT. For `pkg:deb/openssl@3.0.6` (no distribution), `skip_reason` returns `UnsupportedPurlNamespace` because `purl_namespace` is `None`, while `matching_advisories("deb", "openssl", "3.0.6")` hits the seeded CVE-2022-3602 record (affected 3.0.0..3.0.7) through the very same key. The handler loop inserts the component into `skipped` AND pushes the match into `findings` in the same iteration, with no exclusion between them. The 200 response then carries a `vulnerabilities[]` node whose `vulnerablePackages[].bomRef` is that component plus a `messages[]` entry reading "Component skipped: unsupported purl namespace." (and, in the CycloneDX form, the same sentence as the component's own `amazon:inspector:sbom_scanner:info` property). `skip_reason`'s own doc says it answers `None` "when it was ruled on (the advisory data holds rules for its package…)" — here the data does hold a rule and a finding was reported, yet the component is reported as skipped.

2. CROSS-DISTRIBUTION MATCHES. Because the value is discarded, `pkg:deb/ubuntu/openssl@3.0.6` (or any non-empty namespace) resolves to the same key as the stored `deb`/`openssl` advisory and reports the same finding — although `SCANNED_ECOSYSTEMS` states that each OS namespace "names the DISTRIBUTION its packages were built by" and that "[t]he same package name is a different package, with different advisories, in each of them". No stored `VulnRecord` carries a distribution at all, so the store cannot make the distinction the comment asserts.

This is distinct from the open ScanSbom wrong-behavior issue about mistyped `metadata`/`properties` members: that one is in the CycloneDX report builder, this one is in the coordinate resolution + skip decision that feeds every output format.

## Evidence

Rules the crate states:
- services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:292-294 — "/// Why the scan drew no conclusion about `component`, or `None` when it was ruled on (the advisory data holds rules for its package, whether or not this release fell inside an affected range)."
- src/ops/scan_sbom.rs:266-270 — "/// The component's purl does not name a package the scanner can look up: its ecosystem is outside [`SCANNED_ECOSYSTEMS`], or the coordinate carries no `namespace` where that ecosystem's packages are named with one"
- src/ops/scan_sbom.rs:1253-1257 — "// The OS package ecosystems are all reported under one package manager, and each names the DISTRIBUTION its packages were built by: `debian`, `redhat`, `alpine`. The same package name is a different package, with different advisories, in each of them, so a coordinate naming none of them identifies no OS package."
- src/state.rs:280-287 — "/// Does the advisory data hold any rule for this package coordinate? /// /// This is what a scan reports a component as SKIPPED by: the store holds nothing about the package…"
- tests/all/scan_skip_reasons.rs:13-14 (test module doc) — "A component the advisory data holds rules for was ruled on and is reported as skipped in neither form."

Code that breaks them:
- src/ops/scan_sbom.rs:679-685 (the namespace VALUE dropped for OS types):
```
    match namespace {
        None => name.to_string(),
        // The vendor namespace names the distribution, not the package.
        Some(_) if matches!(lowered.as_str(), "deb" | "rpm" | "apk") => name.to_string(),
```
- src/state.rs:403-411 — `fn lookup_key(package_type: &str, package_name: &str) -> (String, String)` keys the index on type+name only; no namespace/distribution anywhere in `VulnRecord` (src/state.rs:42-85).
- src/ops/scan_sbom.rs:306-311 (skip fires on namespace PRESENCE, before the lookup):
```
    if component.purl.is_some() && !resolves_to_a_scanned_package(component) {
        return Some(SkipReason::UnsupportedPurlNamespace);
    }
    if !store.has_supported_rules(&component.package_type, &component.package_name) {
```
  with src/ops/scan_sbom.rs:321-326 `Some((_, Namespace::Required)) => component.purl_namespace.is_some(),` and src/ops/scan_sbom.rs:1258-1260 `("deb", "OS", Namespace::Required), ("rpm", "OS", Namespace::Required), ("apk", "OS", Namespace::Required),`.
- src/ops/scan_sbom.rs:1903-1919 (skipped AND matched in the same iteration, no exclusion):
```
            for component in &parsed.components {
                if let Some(reason) = skip_reason(&store, component) {
                    skipped.insert(component.bom_ref.clone(), reason);
                }
                for index in store.matching_advisories(
                    &component.package_type,
                    &component.package_name,
                    &component.version,
                ) {
```
- The rule that makes it reachable: src/state.rs:781-791 seeds `cve_id: "CVE-2022-3602"`, `package_name: "openssl"`, `package_type: "deb"`, `affected: VersionRange::between(ReleaseVersion::new(3, 0, 0), ReleaseVersion::new(3, 0, 7))`.

Test coverage (the contradiction is pinned on one side only and never cross-checked):
- tests/all/scan_skip_reasons.rs:257-263 pins `purl: "pkg:deb/openssl@3.0.6", version: "3.0.6", expected: Some(UNSUPPORTED_NAMESPACE)` with the stated reason at :232-235 "the group, the module path, the vendor and the distribution are each what makes the name a package, so without one there is no package to hold a rule for" — false in this crate, since :303-309 (`"pkg:deb/debian/openssl@3.0.6"`, `expected: None`) proves the data DOES hold a rule under the key `("deb", "openssl")` the namespace-less coordinate resolves to.
- The test asserts only `messages.len()` and the component's `…:info` property (tests/all/scan_skip_reasons.rs:331-376); it never asserts that the skipped component has no finding, so the simultaneous finding is untested.

## Steps to Reproduce

1. POST /scan/sbom with `{"outputFormat":"INSPECTOR","sbom":{"bomFormat":"CycloneDX","specVersion":"1.5","components":[{"type":"library","name":"openssl","version":"3.0.6","purl":"pkg:deb/openssl@3.0.6"}]}}` (the exact coordinate pinned at tests/all/scan_skip_reasons.rs:259).
2. Read the 200 body: `sbom.vulnerabilities[0].id` is `CVE-2022-3602` affecting `comp-1` and `sbom.vulnerability_count.high` is 1, while `sbom.messages[0].info_message` reads "Component skipped: unsupported purl namespace." for the same component — the scan both ruled on it and declares it unresolvable.
3. Repeat with `"outputFormat":"CYCLONE_DX_1_5"`: `components[0].properties` carries `amazon:inspector:sbom_scanner:info` = "Component skipped: unsupported purl namespace." while `vulnerabilities[0].affects[0].ref` is that same `comp-1`.
4. Change the purl to `pkg:deb/ubuntu/openssl@3.0.6` (any namespace at all): the component is no longer reported as skipped and the same `deb`/`openssl` advisory is reported, although src/ops/scan_sbom.rs:1255-1257 states the same package name in another distribution is a different package with different advisories.
