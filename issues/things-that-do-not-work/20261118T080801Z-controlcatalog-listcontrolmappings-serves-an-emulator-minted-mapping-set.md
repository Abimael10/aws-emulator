Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-control-mappings
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-control-mappings
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControlMappings|2xx->2xx|$.ControlMappings[*]
Issue-Commit: a0e7eb49813c70256d7072893bf6da53a4aacf65

# controlcatalog ListControlMappings serves an emulator-minted mapping set instead of the published catalog (10 causes)

> _Note: this issue was identified against the code base at commit `a0e7eb49813c70256d7072893bf6da53a4aacf65`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControlMappings

## Description
ListControlMappings is answered from an emulator-authored mapping set rather than from AWS's published control-mapping catalog. The shared fault has two faces: for one control ARN the emulator returns a mapping list of its own invention (wrong MappingType per position, framework names/items that are not the catalog's, and COMMON_CONTROL mappings pointing at CommonControlArns that do not exist in the catalog), and for every other catalogued control ARN it returns no mapping rows at all while still answering 200. Ten net-new causes, one per control-ARN filter the corpus exercises; each must be closed before this issue is submitted.

Shared statement of the defect
AWS: Smithy (controlcatalog) ListControlMappings — POST /list-control-mappings, output ListControlMappingsResponse.ControlMappings (@required list of ControlMapping), ControlMapping = {ControlArn @required, MappingType @required (enum FRAMEWORK|COMMON_CONTROL|RELATED_CONTROL), Mapping @required (union Framework|CommonControl|RelatedControl)}; FrameworkMappingDetails = {Name @required, Item @required}; CommonControlMappingDetails = {CommonControlArn @required, @pattern ^arn:(aws(?:[-a-z]*)?):controlcatalog:::common-control/[0-9a-z]+$}; RelatedControlMappingDetails = {ControlArn, RelationType @required}. Input filter ControlMappingFilter.ControlArns is a 1-element list: "When specified, only mappings associated with these controls are returned." The CONTENT of the catalog is recorded behavior only: the fixture records, per control ARN, the exact set of FRAMEWORK/COMMON_CONTROL/RELATED_CONTROL rows AWS publishes for that control. The requirement is that a ControlArns-filtered read returns that whole published set, with each row's MappingType and Mapping payload as published — not a locally generated stand-in.
Emulator: for the first control ARN it answers 200 with a 30-row list whose rows do not correspond to the published rows at any position; for the nine other control ARNs it answers 200 with none of the published rows present ($.ControlMappings[i]: missing, got None), i.e. an empty or truncated list.
Why: a client that reads a control's compliance coverage (which frameworks and items a control satisfies, which common control it implements, which controls are its alternatives) gets either nothing or fabricated coverage. Nothing — a compliance-mapping report comes back blank and the caller concludes the control maps to no framework. Fabricated — the caller records a control as satisfying a framework item it does not satisfy, and follows COMMON_CONTROL mappings to common-control ARNs that no other catalog operation can resolve. Both are silently wrong because the status is 200.
Fix: the ListControlMappings handler's backing catalog data (the controlcatalog crate's control-mapping seed data) plus the ControlArns filter lookup that selects rows for a control ARN — the published mapping rows must be seeded and returned verbatim, not synthesized per call.

Cause 1 of 10 — fixture controlcatalog-catalog-list-control-mappings, step task#1 (Filter.ControlArns = [arn:aws:controlcatalog:::control/1038h7d21e6g8i10g6v3xsssi]). Expected the published rows for that control, e.g. FRAMEWORK NIST-CSF-v1.1 / PR.AC-4, FRAMEWORK AWS-WAF-v10 / SEC08-BP04, FRAMEWORK CIS-v7.1 / 16.3, FRAMEWORK PCI-DSS-v4.0 / 8.2.1, FRAMEWORK PCI-DSS-v3.2.1 / 8.6 (30 rows). Emulator returned 30 rows of its own: position 0 a COMMON_CONTROL row naming common-control/c07785c59b5622421b172680b, position 1 FRAMEWORK AWS-WAF-v10 / SEC04-BP02, position 2 FRAMEWORK CIS-v8.0 / 13.1, position 3 FRAMEWORK CMMC-Level2 / SI.L2-3.14.6. This is the fabricated-mapping face of the fault.
Cause 2 of 10 — step task#2, control/1ks6hwrtiiia8t2djn1iys62x: all 42 published rows missing (CIS-v8.0 / 3.3, CRI-Profile-v2.2 / PR.IR-01.03, ISO-IEC-27001:2022-Annex-A / 8.22, NIST-SP-800-53-r5 / SC-7, COMMON_CONTROL common-control/1d01qjgc798y7qvapb22amb3a, ...).
Cause 3 of 10 — step task#3, control/27lgd3oknluio5kzcl324kr0d: all 9 published rows missing, including the RELATED_CONTROL row {control/ywt2dac1mdoeffk216hldg2q, COMPLEMENTARY}.
Cause 4 of 10 — step task#4, control/39o509cq5imkyls30q7qc6ynd: all 44 published rows missing (FedRAMP-r5 / SC-7 (5), NIST-CSF-v1.1 / PR.AC-5, CIS-AWS-Benchmark-v1.2 / 4.2, ...).
Cause 5 of 10 — step task#5, control/45du52gha9l59sznumimptmcj: all 13 published rows missing (CRI-Profile-v2.2 / PR.PS-04.01, SSAE-18-SOC-2-Oct-2023 / C1.1, COMMON_CONTROL common-control/cka0tukunopu3vqts04wqfq2z, ...).
Cause 6 of 10 — step task#6, control/55i15zi1ernedla1x1ymgz3gb: all 6 published rows missing, including RELATED_CONTROL {control/keijcuz1a7n70grfn62tb2jj, ALTERNATIVE} and COMMON_CONTROL common-control/dchokxd0zjgz3nmih9mal12jw.
Cause 7 of 10 — step task#7, control/5w9ie29h0nydz0kiqjaw8f5wg: all 16 published rows missing (CMMC-Level2 / CM.L2-3.4.2, NIST-SP-800-171-r2 / 3.4.2, COMMON_CONTROL common-control/30mf4xxk2johagsi97ynxz8ae, ...).
Cause 8 of 10 — step task#8, control/6m6ffltyr8ddio6ocdabw8xak: all 18 published rows missing (AWS-WAF-v10 / REL09-BP01, NIST-SP-800-53-r5 / CP-10(2), COMMON_CONTROL common-control/5nzyjix1vm9734750ghlfxew7, ...).
Cause 9 of 10 — step task#9, control/7e2vbhsghm8rh3nveq0kft19u: all 11 published rows missing (CRI-Profile-v2.2 / PR.IR-04.02, ACSC-ISM-02-Mar-2023 / ISM-1580, RELATED_CONTROL {control/dluqcbql2n5lgummqhdfjdntg, ALTERNATIVE}, ...).
Cause 10 of 10 — step task#11, control/8s5b7drmv03d0viy7kzxzzy0y: all 24 published rows missing (FedRAMP-r5 / CM-8, CIS-v8.0 / 1.1, PCI-DSS-v4.0 / 12.5.1, COMMON_CONTROL common-control/gr180qll01cux9k41amnf002, ...).

Related open issues that do NOT cover this: 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md and 20261116T095506Z-controlcatalog-common-control-and-domain-catalog-text-is.md concern control/common-control/domain TEXT and ARNs, not the ControlMappings rows; the earlier ListControlMappings issues (20261106T141234Z..., 20261029T195408Z...) are resolved and so cover nothing here.

## Evidence
Replay of fixture controlcatalog-catalog-list-control-mappings (service controlcatalog, operation ListControlMappings, verdict mismatch, expected_status 200 / actual_status 200 for all ten causes).

Cause 1 (task#1) — body diff, expected vs actual at the same positions:
  $.ControlMappings[0]: expected {ControlArn: .../control/1038h7d21e6g8i10g6v3xsssi, MappingType: FRAMEWORK, Mapping: {Framework: {Name: NIST-CSF-v1.1, Item: PR.AC-4}}}
                        got      {ControlArn: .../control/1038h7d21e6g8i10g6v3xsssi, MappingType: COMMON_CONTROL, Mapping: {CommonControl: {CommonControlArn: arn:aws:controlcatalog:::common-control/c07785c59b5622421b172680b}}}
  $.ControlMappings[1]: expected FRAMEWORK AWS-WAF-v10 / SEC08-BP04   got FRAMEWORK AWS-WAF-v10 / SEC04-BP02
  $.ControlMappings[2]: expected FRAMEWORK CIS-v7.1 / 16.3            got FRAMEWORK CIS-v8.0 / 13.1
  $.ControlMappings[3]: expected FRAMEWORK PCI-DSS-v4.0 / 8.2.1       got FRAMEWORK CMMC-Level2 / SI.L2-3.14.6
  $.ControlMappings[4]: expected FRAMEWORK PCI-DSS-v3.2.1 / 8.6       got FRAMEWORK CIS-... (divergent)
  divergence_key lists $.ControlMappings[0]..[29] — every one of the 30 rows diverges.

Causes 2-10 (task#2, #3, #4, #5, #6, #7, #8, #9, #11) — every expected row reports "missing (expected {...}, got None)", e.g.
  task#2: $.ControlMappings[0]: missing (expected {ControlArn: .../control/1ks6hwrtiiia8t2djn1iys62x, MappingType: FRAMEWORK, Mapping: {Framework: {Name: CIS-v8.0, Item: 3.3}}}, got None) ... through $.ControlMappings[41]
  task#3: $.ControlMappings[0..8] missing for control/27lgd3oknluio5kzcl324kr0d (cluster also covers task#16)
  task#4: $.ControlMappings[0..43] missing for control/39o509cq5imkyls30q7qc6ynd
  task#5: $.ControlMappings[0..12] missing for control/45du52gha9l59sznumimptmcj
  task#6: $.ControlMappings[0..5] missing for control/55i15zi1ernedla1x1ymgz3gb
  task#7: $.ControlMappings[0..15] missing for control/5w9ie29h0nydz0kiqjaw8f5wg
  task#8: $.ControlMappings[0..17] missing for control/6m6ffltyr8ddio6ocdabw8xak (cluster also covers task#10)
  task#9: $.ControlMappings[0..10] missing for control/7e2vbhsghm8rh3nveq0kft19u
  task#11: $.ControlMappings[0..23] missing for control/8s5b7drmv03d0viy7kzxzzy0y (cluster also covers task#14)

No error is involved: the status is 200 in every case, so the divergence is purely in the response body.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture controlcatalog-catalog-list-control-mappings.
2. Cause 1: POST /list-control-mappings with body {"Filter":{"ControlArns":["arn:aws:controlcatalog:::control/1038h7d21e6g8i10g6v3xsssi"]}} (step task#1). Observe 200 with 30 ControlMappings rows whose MappingType/Mapping values are not the published rows for that control (row 0 is a COMMON_CONTROL row naming a common-control ARN absent from the catalog).
3. Causes 2-10: repeat with Filter.ControlArns set to each of control/1ks6hwrtiiia8t2djn1iys62x (task#2), control/27lgd3oknluio5kzcl324kr0d (task#3), control/39o509cq5imkyls30q7qc6ynd (task#4), control/45du52gha9l59sznumimptmcj (task#5), control/55i15zi1ernedla1x1ymgz3gb (task#6), control/5w9ie29h0nydz0kiqjaw8f5wg (task#7), control/6m6ffltyr8ddio6ocdabw8xak (task#8), control/7e2vbhsghm8rh3nveq0kft19u (task#9), control/8s5b7drmv03d0viy7kzxzzy0y (task#11). Each answers 200 with none of the published mapping rows for that control.
4. Compare each response's ControlMappings against the rows the fixture step records for the same filter.
