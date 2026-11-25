Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-control-mappings
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-control-mappings
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControlMappings|2xx->2xx|$.ControlMappings[*]
Issue-Commit: 24511e4ce5aef4d33e503a00bb0e57c4ce26e21d

# controlcatalog ListControlMappings serves a synthesized mapping set for ten published controls

> _Note: this issue was identified against the code base at commit `24511e4ce5aef4d33e503a00bb0e57c4ce26e21d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControlMappings

## Description
ListControlMappings is a read of AWS's published control-mapping catalog: for a ControlArns filter it must return every mapping row AWS publishes for that control, each row being the control's ARN plus one MappingType (FRAMEWORK / COMMON_CONTROL / RELATED_CONTROL) and the matching Mapping union member. The emulator instead answers from a mapping table it synthesizes: for two controls it returns rows whose framework names/items and mapping types are not the published ones, and for eight further controls it returns no rows at all. This bundle carries 10 causes; all 10 must be closed. Related open controlcatalog catalog-fidelity issues exist for other operations (20261123T051108Z-controlcatalog-serves-a-synthesized-control-catalog-getcontrol-404s.md for GetControl, 20261124T130941Z-controlcatalog-listcommoncontrols-rewords-published-common-control-text-and.md for ListCommonControls); none of them covers ListControlMappings, and every earlier ListControlMappings issue is resolved, so these are net-new.

Cause 1 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/1038h7d21e6g8i10g6v3xsssi].
AWS: Smithy: ListControlMappings POST /list-control-mappings, output ControlMappings (required list of ControlMapping{ControlArn required, MappingType required enum FRAMEWORK|COMMON_CONTROL|RELATED_CONTROL, Mapping required union Framework|CommonControl|RelatedControl}); FrameworkMappingDetails.Name and .Item are both required. The recorded 200 response for this control carries FRAMEWORK rows including {Name NIST-CSF-v1.1, Item PR.AC-4}, {Name AWS-WAF-v10, Item SEC08-BP04}, {Name CIS-v7.1, Item 16.3}, {Name PCI-DSS-v4.0, Item 8.2.1} and {Name PCI-DSS-v3.2.1, Item 8.6}; 30 row positions are compared.
Emulator: 200 with a differently-populated list — position 0 is a COMMON_CONTROL row instead of the FRAMEWORK/NIST-CSF-v1.1 row, and the FRAMEWORK rows name items the published mapping does not contain for this control (AWS-WAF-v10 SEC04-BP02 instead of SEC08-BP04, CIS-v8.0 13.1 instead of CIS-v7.1 16.3, CMMC-Level2 SI.L2-3.14.6 where PCI-DSS-v4.0 8.2.1 is published). All 30 compared rows diverge.
Why: a client reading which compliance-framework requirements a control satisfies gets framework/item pairs AWS never published for it, so compliance mapping reports and any framework-driven control selection are built on invented evidence.
Fix: the ListControlMappings handler's mapping catalog (the seeded control-mapping dataset) and its ControlArns-filter projection.
Traces: controlcatalog-catalog-list-control-mappings#task#1.

Cause 2 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/1ks6hwrtiiia8t2djn1iys62x].
AWS: same model surface as cause 1. The recorded 200 carries rows including FRAMEWORK {CRI-Profile-v2.2, PR.IR-01.03}, {CRI-Profile-v2.2, PR.AA-03.02}, {ISO-IEC-27001:2022-Annex-A, 8.22}, {NIST-SP-800-53-r5, SC-7} and a COMMON_CONTROL row for common-control/1d01qjgc798y7qvapb22amb3a; 41 row positions are compared.
Emulator: 200 whose rows name other frameworks and items entirely (CIS-v8.0 3.3, CMMC-Level2 AC.L2-3.1.3, CRI-Profile-v2.2 PR.AC-04.01, FedRAMP-r4 AC-3), and a FRAMEWORK row where the published row is COMMON_CONTROL — i.e. MappingType itself diverges.
Why: as cause 1; additionally a client filtering by MappingType sees a row classified as FRAMEWORK that AWS classifies as COMMON_CONTROL, so MappingTypes-filtered reads return the wrong set.
Fix: as cause 1 — the mapping catalog's rows for this control, including each row's MappingType classification.
Traces: controlcatalog-catalog-list-control-mappings#task#2.

Cause 3 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/27lgd3oknluio5kzcl324kr0d].
AWS: recorded 200 with 9 rows — FRAMEWORK (CRI-Profile-v2.2 PR.IR-04.01, ISO-IEC-27001:2022-Annex-A 8.9, PCI-DSS-v4.0 10.4.2, CRI-Profile-v2.2 PR.PS-04.02, ISO-IEC-27001:2022-Annex-A 8.15, CRI-Profile-v2.2 DE.AE-03.01), COMMON_CONTROL (common-control/be13otgkbmy60qc52qmdvq7ko) and RELATED_CONTROL (control/ywt2dac1mdoeffk216hldg2q, RelationType COMPLEMENTARY).
Emulator: 200 with the row absent at every compared position (got None) — the ControlMappings list is empty for this control.
Why: the control appears to have no framework, common-control or related-control relationships at all, so a client concludes the control maps to nothing.
Fix: as cause 1 — the mapping catalog has no rows seeded for this control ARN.
Traces: controlcatalog-catalog-list-control-mappings#task#3 (same cluster: #task#16).

Cause 4 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/39o509cq5imkyls30q7qc6ynd].
AWS: recorded 200 with 44 rows, including FRAMEWORK {FedRAMP-r5, SC-7 (5)}, {NIST-CSF-v1.1, PR.AC-5}, {CIS-AWS-Benchmark-v1.2, 4.2}, {FedRAMP-r5, SC-7}, {FedRAMP-r4, SC-7 (5)}, {PCI-DSS-v4.0, 1.3.1}, {CIS-v8.0, 12.2}, {FedRAMP-r5, AC-4}.
Emulator: 200 with an empty ControlMappings list — all 44 compared rows are missing.
Why: as cause 3.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#4.

Cause 5 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/45du52gha9l59sznumimptmcj].
AWS: recorded 200 with 13 rows, including FRAMEWORK {CRI-Profile-v2.2, PR.PS-04.01}, {NIST-CSF-v1.1, PR.IP-6}, {CIS-v8.0, 3.4}, {CCCS-Medium-Cloud-Control-May-2019, SI-12}, {SSAE-18-SOC-2-Oct-2023, C1.1}, {PCI-DSS-v3.2.1, 3.1} and a COMMON_CONTROL row for common-control/cka0tukunopu3vqts04wqfq2z.
Emulator: 200 with an empty ControlMappings list.
Why: as cause 3.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#5.

Cause 6 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/55i15zi1ernedla1x1ymgz3gb].
AWS: recorded 200 with 6 rows: FRAMEWORK {CRI-Profile-v2.2, PR.IR-03.01}, {CRI-Profile-v2.2, PR.IR-04.02}, {ACSC-ISM-02-Mar-2023, ISM-1579}, {ISO-IEC-27001:2013-Annex-A, A.12.1.3}, a RELATED_CONTROL row (control/keijcuz1a7n70grfn62tb2jj, ALTERNATIVE) and a COMMON_CONTROL row (common-control/dchokxd0zjgz3nmih9mal12jw).
Emulator: 200 with an empty ControlMappings list.
Why: as cause 3; the ALTERNATIVE related-control relationship is also lost, so a client cannot discover the substitutable control.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#6.

Cause 7 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/5w9ie29h0nydz0kiqjaw8f5wg].
AWS: recorded 200 with 16 rows, including COMMON_CONTROL (common-control/30mf4xxk2johagsi97ynxz8ae, common-control/ez1ccu4xkxrmlekcvjqo3jk59) and FRAMEWORK {CMMC-Level2, CM.L2-3.4.2}, {SSAE-18-SOC-2-Oct-2023, A1.1}, {NIST-SP-800-171-r2, 3.4.2}, {CCCS-Medium-Cloud-Control-May-2019, CM-6}, {NIST-CSF-v1.1, PR.IP-3}, {FedRAMP-r5, CM-6}.
Emulator: 200 with an empty ControlMappings list.
Why: as cause 3.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#7.

Cause 8 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/6m6ffltyr8ddio6ocdabw8xak].
AWS: recorded 200 with 18 rows, including FRAMEWORK {AWS-WAF-v10, REL09-BP01}, {NIST-SP-800-53-r5, CP-10(2)}, {FedRAMP-r5, CP-9}, {NIST-SP-800-53-r5, CP-9}, {CRI-Profile-v2.2, RC.RP-01.01}, {FedRAMP-r4, CP-9} and COMMON_CONTROL rows for common-control/5nzyjix1vm9734750ghlfxew7 and common-control/69j1wukdrzz7wubby8863bwed.
Emulator: 200 with an empty ControlMappings list.
Why: as cause 3.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#8 (same cluster: #task#10).

Cause 9 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/7e2vbhsghm8rh3nveq0kft19u].
AWS: recorded 200 with 11 rows, including FRAMEWORK {CRI-Profile-v2.2, PR.IR-04.02}, {ACSC-ISM-02-Mar-2023, ISM-1580}, {CRI-Profile-v2.2, PR.IR-03.01}, {NIST-SP-800-53-r5, SI-13(5)}, {AWS-WAF-v10, REL13-BP05}, RELATED_CONTROL rows (control/dluqcbql2n5lgummqhdfjdntg ALTERNATIVE, control/22yu4cabtvco34emorco1lgiu COMPLEMENTARY) and a COMMON_CONTROL row (common-control/dchokxd0zjgz3nmih9mal12jw).
Emulator: 200 with an empty ControlMappings list.
Why: as cause 3.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#9.

Cause 10 of 10: ListControlMappings, Filter.ControlArns = [arn:aws:controlcatalog:::control/8s5b7drmv03d0viy7kzxzzy0y].
AWS: recorded 200 with 24 rows, including FRAMEWORK {FedRAMP-r5, CM-8}, {CIS-v8.0, 1.1}, {CIS-v7.1, 1.5}, {NIST-SP-800-171-r2, 3.4.1}, {PCI-DSS-v4.0, 12.5.1}, {ACSC-ISM-02-Mar-2023, ISM-0336}, {PCI-DSS-v3.2.1, 12.5.1} and a COMMON_CONTROL row for common-control/gr180qll01cux9k41amnf002.
Emulator: 200 with an empty ControlMappings list.
Why: as cause 3.
Fix: as cause 1.
Traces: controlcatalog-catalog-list-control-mappings#task#11 (same cluster: #task#14).

## Evidence
Replay of fixture controlcatalog-catalog-list-control-mappings against the booted emulator (trace set simparity-wire-aws-v1), 10 divergence clusters, all verdict=mismatch, expected_status 200 / actual_status 200.

Cause 1 — step task#1, control/1038h7d21e6g8i10g6v3xsssi: $.ControlMappings[0] expected {MappingType FRAMEWORK, Mapping.Framework {Name NIST-CSF-v1.1, Item PR.AC-4}}, got {MappingType COMMON_CONTROL, Mapping.CommonControl.CommonControlArn arn:aws:controlcatalog:::common-control/c07785c59b5622421b172680b}; [1] expected {FRAMEWORK, AWS-WAF-v10 / SEC08-BP04}, got {FRAMEWORK, AWS-WAF-v10 / SEC04-BP02}; [2] expected {CIS-v7.1 / 16.3}, got {CIS-v8.0 / 13.1}; [3] expected {PCI-DSS-v4.0 / 8.2.1}, got {CMMC-Level2 / SI.L2-3.14.6}; 30 positions diverge.
Cause 2 — step task#2, control/1ks6hwrtiiia8t2djn1iys62x: [1] expected {CRI-Profile-v2.2 / PR.IR-01.03}, got {CIS-v8.0 / 3.3}; [2] expected {CRI-Profile-v2.2 / PR.AA-03.02}, got {CMMC-Level2 / AC.L2-3.1.3}; [3] expected {ISO-IEC-27001:2022-Annex-A / 8.22}, got {CRI-Profile-v2.2 / PR.AC-04.01}; [4] expected {NIST-SP-800-53-r5 / SC-7}, got {FedRAMP-r4 / AC-3}; [5] expected MappingType COMMON_CONTROL (common-control/1d01qjgc798y7qvapb22amb3a), got MappingType FRAMEWORK; 41 positions diverge.
Cause 3 — step task#3 (cluster also task#16), control/27lgd3oknluio5kzcl324kr0d: $.ControlMappings[0..8] all "missing (… got None)"; expected rows include FRAMEWORK CRI-Profile-v2.2/PR.IR-04.01, COMMON_CONTROL common-control/be13otgkbmy60qc52qmdvq7ko, RELATED_CONTROL control/ywt2dac1mdoeffk216hldg2q COMPLEMENTARY.
Cause 4 — step task#4, control/39o509cq5imkyls30q7qc6ynd: $.ControlMappings[0..43] all missing / got None.
Cause 5 — step task#5, control/45du52gha9l59sznumimptmcj: $.ControlMappings[0..12] all missing / got None.
Cause 6 — step task#6, control/55i15zi1ernedla1x1ymgz3gb: $.ControlMappings[0..5] all missing / got None.
Cause 7 — step task#7, control/5w9ie29h0nydz0kiqjaw8f5wg: $.ControlMappings[0..15] all missing / got None.
Cause 8 — step task#8 (cluster also task#10), control/6m6ffltyr8ddio6ocdabw8xak: $.ControlMappings[0..17] all missing / got None.
Cause 9 — step task#9, control/7e2vbhsghm8rh3nveq0kft19u: $.ControlMappings[0..10] all missing / got None.
Cause 10 — step task#11 (cluster also task#14), control/8s5b7drmv03d0viy7kzxzzy0y: $.ControlMappings[0..23] all missing / got None.

No cluster carried stub_path or owned_by, and every previously filed controlcatalog ListControlMappings issue is in the resolved folder.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Replay wire fixture controlcatalog-catalog-list-control-mappings (trace set simparity-wire-aws-v1).
3. Step task#1: POST /list-control-mappings with Filter.ControlArns = ["arn:aws:controlcatalog:::control/1038h7d21e6g8i10g6v3xsssi"] — compare each ControlMappings row's MappingType and Mapping member against the recorded response; the framework names/items and the row types diverge.
4. Step task#2: same call for control/1ks6hwrtiiia8t2djn1iys62x — rows diverge and one published COMMON_CONTROL row is served as FRAMEWORK.
5. Steps task#3, task#4, task#5, task#6, task#7, task#8, task#9, task#11: same call for control ARNs 27lgd3oknluio5kzcl324kr0d, 39o509cq5imkyls30q7qc6ynd, 45du52gha9l59sznumimptmcj, 55i15zi1ernedla1x1ymgz3gb, 5w9ie29h0nydz0kiqjaw8f5wg, 6m6ffltyr8ddio6ocdabw8xak, 7e2vbhsghm8rh3nveq0kft19u and 8s5b7drmv03d0viy7kzxzzy0y respectively — each answers 200 with an empty ControlMappings list where the recording returns 6 to 44 rows.
