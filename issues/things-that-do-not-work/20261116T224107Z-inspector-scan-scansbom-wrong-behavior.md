Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 52f475a4504a9c514f88c04338789fa36aa3e85d

# CycloneDX report silently discards submitted metadata / metadata.tools / metadata.properties / component properties when they are not the type the report builder expects

_Filed: 2026-11-16T22:41:07Z_
_Operation: ScanSbom | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `52f475a4504a9c514f88c04338789fa36aa3e85d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

In the `CYCLONE_DX_1_5` form the report is built from a clone of the submitted document, and the handler documents the rule for every member it touches: the caller's value is PRESERVED and the scan's own data is appended beside it ("the report must not misreport what the caller sent").

Four sites break that rule for a submitted value whose JSON type the builder does not expect. In each the `_` arm of a `match` throws the caller's value away and writes the scan's own data in its place, so the response claims the caller sent something it did not:

* `annotate_skipped_components` — a skipped component whose `properties` is present but not an array has it REPLACED by a one-element array holding only the scanner's `…:info` annotation (`src/ops/scan_sbom.rs:1439-1444`), although the function's own doc promises "what the caller submitted there is preserved beside it".
* `cyclonedx_report` — a submitted `metadata` that is not an object is dropped and replaced by a fresh map (`:1522-1525`); a submitted `metadata.tools` that is neither an object nor an array is dropped (`:1534-1545`); a submitted `metadata.properties` that is not an array is dropped (`:1559-1562`), directly under the comment that says the summary is "appended to the document's own `metadata.properties` rather than replacing them".

The crate already states the opposite rule for the same class of input one screen earlier: a `components` member that is present and not an array is REFUSED with `ValidationException` precisely so that it is not dropped silently (`:741-751`). So a mistyped member is refused on one path and silently rewritten on four others — it is neither preserved nor reported, and the 200 response misstates the submitted document.

This is reachable from ordinary input: `sbom` is a Smithy `document`, i.e. arbitrary caller JSON, and nothing between the boundary and these sites constrains the type of `metadata`, `metadata.tools`, `metadata.properties` or a component's `properties`.

## Evidence

Rule the crate states:
- services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/scan_sbom.rs:1410-1411 — "/// The annotation is appended to the component's own `properties`, so what the caller submitted there is preserved beside it."
- src/ops/scan_sbom.rs:1557-1558 — "// And WHAT it found, as the namespaced scan summary, appended to the document's own `metadata.properties` rather than replacing them."
- src/ops/scan_sbom.rs:1506-1508 — "// `version` is the BOM's own revision number. It is PRESERVED as submitted — the report must not misreport what the caller sent —"
- src/ops/scan_sbom.rs:741-744 (the sibling path that refuses instead of dropping) — "/// The same rule the top level applies: a `components` member that is not an array is not a CycloneDX component list, and left to fall through it would drop the whole subtree silently — neither counted against the documented limit, nor matched, nor reported." with `Some(_) => Err(invalid(SbomDefect::InvalidValue("components")))` at :751.

Code that breaks it:
- src/ops/scan_sbom.rs:1439-1444:
```
            match object.get_mut("properties") {
                Some(Value::Array(list)) => list.push(property),
                _ => {
                    object.insert("properties".to_string(), json!([property]));
                }
            }
```
- src/ops/scan_sbom.rs:1522-1525:
```
    let mut metadata = match report.get("metadata") {
        Some(Value::Object(existing)) => existing.clone(),
        _ => Map::new(),
    };
```
- src/ops/scan_sbom.rs:1534-1545: `let mut tools = match metadata.remove("tools") { Some(Value::Object(existing)) => existing, Some(Value::Array(entries)) => {…}, _ => Map::new(), };`
- src/ops/scan_sbom.rs:1559-1562:
```
    let mut properties = match metadata.remove("properties") {
        Some(Value::Array(existing)) => existing,
        _ => Vec::new(),
    };
```

Test coverage (only the well-typed forms are pinned, so the drop is untested):
- tests/all/scan_report_annotations.rs:542-572 `the_scans_component_property_is_appended_to_the_submitted_ones` submits `"properties": [ { "name": "internal:team", … } ]` (array).
- tests/all/scan_report_annotations.rs:769-794 `the_scan_summary_is_appended_to_the_submitted_metadata_properties` submits `metadata.properties` as an array.
- tests/all/scan_report_annotations.rs:687-767 `the_cyclonedx_report_names_the_scanning_service_in_metadata_tools` covers only `tools` absent, object and array forms.

## Steps to Reproduce

1. POST /scan/sbom with `{"outputFormat":"CYCLONE_DX_1_5","sbom":{"bomFormat":"CycloneDX","specVersion":"1.5","metadata":{"tools":"acme-sbom-generator 1.0","properties":{"internal:build":"42"}},"components":[{"type":"library","name":"left-pad","version":"1.0.0","purl":"pkg:npm/left-pad@1.0.0","properties":{"internal:team":"platform"}}]}}` (left-pad is skipped, so the component is annotated).
2. Observe HTTP 200 and read the returned `sbom`: `components[0].properties` is `[{"name":"amazon:inspector:sbom_scanner:info",…}]` only — the submitted `{"internal:team":"platform"}` is gone; `metadata.properties` carries only the six summary entries — the submitted `internal:build` is gone; `metadata.tools` carries only `services` — the submitted string is gone.
3. Repeat with `"metadata": "generated by acme"`: the whole submitted `metadata` is replaced by the scan's own object.
4. Compare with the sibling rule: POST the same document with `"components": {}` (a `components` member that is not an array) and observe the 400 `ValidationException` "Input SBOM failed validation: Invalid value for \"components\"" — the mistyped member is refused there and silently rewritten in the four sites above.
