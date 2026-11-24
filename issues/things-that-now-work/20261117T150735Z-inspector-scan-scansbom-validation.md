Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: d1107ef412a33c5fd1cd764c3d91be4521975a1a

# A component's `purl`, `version` or `bom-ref` of the wrong JSON type is silently treated as absent instead of refused, so a vulnerable component is reported clean or merely "no supported rules"

_Filed: 2026-11-17T15:07:35Z_
_Operation: ScanSbom | Class: validation_

> _Note: this issue was identified against the code base at commit `d1107ef412a33c5fd1cd764c3d91be4521975a1a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every CycloneDX member the handler reads goes through `.and_then(Value::as_str)`, but the two groups then behave oppositely:

* `bomFormat`, `specVersion` and `components` match on the result and REFUSE anything that is not the expected type (`Invalid value for "bomFormat"`, `"specVersion"`, `"components"`); the crate's own comment for the nested `components` case says a mistyped member "left to fall through ... would drop the whole subtree silently — neither counted against the documented limit, nor matched, nor reported".
* `purl`, `version` and `bom-ref` fall through: `object.get("purl").and_then(Value::as_str)` yields `None` for `"purl": 42`, and the `None` arm — commented "// No purl: the entry names no ecosystem" — resolves the component under its plain `name` with an EMPTY `package_type`. The entry did carry a `purl`; it just was not a string, and the refusal class the crate already has for exactly this member (`SbomDefect::InvalidValue("purl")`) is never reached.

Consequences on input the model allows (`sbom` is a Smithy `document`, i.e. arbitrary caller JSON, and nothing between the boundary and these sites constrains member types):
* `"purl": 42` on a vulnerable `lodash@4.17.20`: the lookup key becomes `("", "lodash")`, which no index entry has, so the response is 200 with no finding and the misleading message "Component skipped: no supported rules found." — the scan claims it had no rule for a package it holds a rule for.
* `"version": 4.17.20` (a JSON number) with `"purl": "pkg:npm/lodash"`: the resolved version is the empty string, `normalize_version` returns `None`, so `version_in_range` is false and the component is reported CLEAN with no message at all — a vulnerable component silently turns into a successful clean scan.
* `"bom-ref": 5` on two entries: `submitted_bom_ref` returns `Ok(None)`, so the CycloneDX uniqueness rule the crate enforces for string refs is not applied at all.

## Evidence

The sibling paths that refuse a mistyped member:
- services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:915-919:
```
    match object.get("bomFormat").and_then(Value::as_str) {
        Some(CYCLONEDX_BOM_FORMAT) => {}
        // Absent, or naming a format that is not CycloneDX at all.
        _ => return Err(invalid(SbomDefect::InvalidValue("bomFormat"))),
    }
```
- src/ops/scan_sbom.rs:741-751 — "/// The same rule the top level applies: a `components` member that is not an array is not a CycloneDX component list, and left to fall through it would drop the whole subtree silently — neither counted against the documented limit, nor matched, nor reported." with `Some(_) => Err(invalid(SbomDefect::InvalidValue("components"))),`

The paths that do not:
- src/ops/scan_sbom.rs:814-819:
```
        let name = object.get("name").and_then(Value::as_str).unwrap_or_default();
        let declared_version = object
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let purl = object.get("purl").and_then(Value::as_str);
```
- src/ops/scan_sbom.rs:853-861:
```
            // No purl: the entry names no ecosystem, so it is scanned under its
            // own name and matches only an advisory recorded without one.
            None => (
                String::new(),
                name.to_string(),
                declared_version.to_string(),
                None,
            ),
```
- src/ops/scan_sbom.rs:764-771 — `submitted_bom_ref` returns `Ok(None)` for a non-string `bom-ref`, so the uniqueness check at :724-729 never runs for it.

The class rule the crate's own refusal suite states, which a non-string value is outside of:
- tests/all/scan_sbom_refusals.rs:259 — "// `purl`: every form that is not a package URL is one class." with rows for `"lodash@4.17.20"`, `"PKG:npm/lodash@4.17.20"`, `"pkg:npm"`, `"pkg:np m/lodash@4.17.20"` (all strings) each expecting `invalid_value("purl")` (:260-299).
- tests/all/scan_sbom_refusals.rs:155-181 shows the mistyped-member class IS pinned for the other members: "a bomFormat that is not a string" and "a specVersion that is not a string" are both refused 400. No row covers a non-string `purl`, `version` or `bom-ref`.

## Steps to Reproduce

1. POST /scan/sbom with {"sbom":{"bomFormat":"CycloneDX","specVersion":"1.5","version":1,"components":[{"type":"library","bom-ref":"ref-a","name":"lodash","version":"4.17.20","purl":42}]}}
2. Observe HTTP 200 with `vulnerabilities: []` and `messages[0].info_message == "Component skipped: no supported rules found."` (no `purl` member on the message), instead of the 400 `Input SBOM failed validation: Invalid value for "purl"` the crate answers for every other unreadable `purl`
3. POST the same document with `"purl": "pkg:npm/lodash"` and `"version": 4.17.20` (JSON number): observe HTTP 200 with no finding AND no skip message — the vulnerable component is reported clean
4. POST a document whose two components both carry `"bom-ref": 5`: observe HTTP 200 instead of the 400 `Invalid value for "bom-ref"` that two identical string refs produce (tests/all/scan_sbom_refusals.rs:302-309)
