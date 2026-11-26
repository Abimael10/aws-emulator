Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-create-patch-baseline-and-inspect-patches
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-create-patch-baseline-and-inspect-patches ssm-describe-effective-patches-for-new-baseline ssm-patch-baseline-create-describe-effective-delete
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|DescribeEffectivePatchesForPatchBaseline|2xx->2xx|$.EffectivePatches[*]
Issue-Related: 20261125T060148Z-ssm-seeded-patch-and-document-reference-catalogs-are.md
Issue-Commit: 909b19ea020eb5a0e53f1bf3e57b93e5193187d7

# ssm DescribeEffectivePatchesForPatchBaseline does not compute a baseline's effective patch list from the real patch catalog

> _Note: this issue was identified against the code base at commit `909b19ea020eb5a0e53f1bf3e57b93e5193187d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEffectivePatchesForPatchBaseline

## Description
3 net-new causes, all on the same emulator component: DescribeEffectivePatchesForPatchBaseline never evaluates a patch baseline's rules/approval lists against the service's seeded patch catalog. It answers 200 with an EffectivePatches list that is either short/empty or populated from an emulator-generated pseudo-catalog, so no step of a patch-baseline workflow sees the patches the baseline actually makes effective.

Related but NOT covering: open issue 20261125T060148Z-ssm-seeded-patch-and-document-reference-catalogs-are.md carries only the replay key ssm|DescribeEffectivePatchesForPatchBaseline|2xx->2xx|$.EffectivePatches[*].Patch.ContentUrl (a per-field defect on entries that ARE returned) plus DescribeAvailablePatches/ListDocuments keys; none of the three clusters below is owned by it (replay reported owned_by empty for all three), because here whole EffectivePatches entries are absent or substituted.

Cause 1 of 3: a freshly created baseline with explicit approved patches reports NO effective patches at all. Fixture ssm-create-patch-baseline-and-inspect-patches, step task#3.
AWS: Smithy: DescribeEffectivePatchesForPatchBaseline takes a required BaselineId (@length 20-128, pattern ^[a-zA-Z0-9_\-:/]{20,128}$) and returns EffectivePatches (EffectivePatchList), each element an EffectivePatch of Patch + PatchStatus{DeploymentStatus, ComplianceLevel, ApprovalDate}; errors DoesNotExistException, InvalidResourceId, UnsupportedOperatingSystem, InternalServerError. The recording shows AWS returning, for a Windows baseline, one EffectivePatch per patch the baseline makes effective: patches named in the baseline's ApprovedPatches come back with PatchStatus.DeploymentStatus = EXPLICIT_APPROVED and ComplianceLevel = UNSPECIFIED when no compliance level was set, and each Patch carries the full catalog record (Id, Title, Description, ContentUrl, Vendor, ProductFamily, Product, Classification, MsrcSeverity, MsrcNumber, KbNumber, Language, ReleaseDate).
Emulator: returns 200 but the EffectivePatches list has no entries at the expected indices - the replay reports $.EffectivePatches[0] through $.EffectivePatches[4] all 'missing ... got None', i.e. five expected effective patches and nothing returned for them.
Why: a client that creates a baseline with ApprovedPatches and then asks which patches that baseline makes effective concludes the baseline approves nothing. Patch-compliance tooling that drives approval review, snapshot generation or reporting off this list silently does no work, and a test asserting EXPLICIT_APPROVED approval of a named KB fails against the emulator while passing on AWS.
Fix: the DescribeEffectivePatchesForPatchBaseline handler plus the patch-catalog/patch-baseline state in the ssm service crate - the handler must join the stored baseline (ApprovedPatches, ApprovedPatchesComplianceLevel, PatchRules/global filters, OperatingSystem) against the seeded patch catalog and emit one EffectivePatch per matching catalog record, rather than returning an empty or truncated list.

Cause 2 of 3: effective patches are served from an emulator-generated pseudo-catalog instead of the real patch records. Fixture ssm-describe-effective-patches-for-new-baseline, step task#2.
AWS: recorded behavior only for the catalog contents (the Smithy model types Patch but does not enumerate records). The recording shows real vendor catalog entries, e.g. a Windows7 ServicePacks patch with Id 11689671-f219-4480-bcd5-d2d4dbd5a6b6, KbNumber KB972455, Title 'Windows Server Update Services 3.0 SP2 (KB972455)', ContentUrl https://support.microsoft.com/en-us/kb/972455, MsrcSeverity Unspecified, MsrcNumber '' and a real ReleaseDate, with PatchStatus{DeploymentStatus APPROVED, ComplianceLevel UNSPECIFIED, ApprovalDate}. Ids, KB numbers, titles, descriptions and release dates are properties of the AWS patch catalog, not of the run.
Emulator: returns 200 with synthesized records in those positions instead - the replay reports $.EffectivePatches[0] 'missing (expected {...KB972455...}, got {Patch: {Id: 36260d86-224a-d1b2-5a04-a9d7cd984f2b, Title: "2026-02 ServicePacks for Windows7 (KB5031302)", Description: "ServicePacks published by Microsoft for Windows7 in the 2026-02 release.", KbNumber: KB5031302, Epoch: 0, ...}})', and likewise for $.EffectivePatches[1..3] and $.EffectivePatches[0].Patch.ContentUrl. The generated titles/descriptions follow a template ('<YYYY-MM> <Classification> for <Product> (KB<n>)' / '<Classification> published by Microsoft for <Product> in the <YYYY-MM> release.') and carry an Epoch member AWS does not return for Windows patches.
Why: every identifying field of a patch is fabricated, so a client cannot match an effective patch to a real KB/MSRC advisory, cannot look up its ContentUrl, and sees descriptions and release dates that drift with the emulator clock. Patch ids are the join key for InstancePatchState/compliance reporting, so a workflow that reads an id here and uses it elsewhere refers to a patch that does not exist.
Fix: the ssm patch catalog seed/state (the same catalog DescribeAvailablePatches reads) must hold the real catalog records, and DescribeEffectivePatchesForPatchBaseline must project those records rather than generating stand-ins at request time.

Cause 3 of 3: the same empty-list defect on the create/describe-effective/delete baseline flow. Fixture ssm-patch-baseline-create-describe-effective-delete, step task#3.
AWS: recorded behavior consistent with the model above - AWS returns the EffectivePatch entries the baseline makes effective, here two Windows records (an Updates patch for Windows7 x64 and a SecurityUpdates patch for WindowsServer2008R2 carrying MsrcNumber MS10-043 / MsrcSeverity Important), each with PatchStatus{DeploymentStatus EXPLICIT_APPROVED, ComplianceLevel UNSPECIFIED}.
Emulator: returns 200 with $.EffectivePatches[0] and $.EffectivePatches[1] 'missing ... got None'.
Why: the end-to-end patch-baseline lifecycle (create -> describe effective -> delete) cannot be exercised: the describe step reports nothing effective, so a client cannot confirm the baseline it just created approves the patches it asked for before deleting it.
Fix: same component as cause 1 - the DescribeEffectivePatchesForPatchBaseline handler's evaluation of stored baseline approval state against the seeded catalog.

Traces: ssm-create-patch-baseline-and-inspect-patches#task#3, ssm-describe-effective-patches-for-new-baseline#task#2, ssm-patch-baseline-create-describe-effective-delete#task#3.

## Evidence
Replay of corpus simparity-wire-aws-v1 (emulator up; 907 steps, 880 passed, score 97.0%). Three clusters, all service=ssm, operation=DescribeEffectivePatchesForPatchBaseline, verdict=mismatch, expected_status=200 / actual_status=200, stub_path empty, owned_by empty:

1) divergence_key service=ssm|DescribeEffectivePatchesForPatchBaseline|status=200->200|keys=$.EffectivePatches[0],$.EffectivePatches[1],$.EffectivePatches[2],$.EffectivePatches[3],$.EffectivePatches[4]
   fixture ssm-create-patch-baseline-and-inspect-patches step task#3
   detail: $.EffectivePatches[0]: missing (expected {'Patch': {'Classification': 'Updates', 'ContentUrl': 'https://support.microsoft.com/en-us/kb/2032276', 'Id': '4a56e378-4c48-4284-abb0-a36028f5dbde', 'KbNumber': 'KB2032276', 'MsrcSeverity': 'Unspecified', 'Product': 'Windows7', 'ProductFamily': 'Windows', 'Title': 'Security Update for Windows 7 for x64-based Systems (KB2032276)', 'Vendor': 'Microsoft', ...}, 'PatchStatus': {'ComplianceLevel': 'UNSPECIFIED', 'DeploymentStatus': 'EXPLICIT_APPROVED'}}, got None); same for indices 1-4.

2) divergence_key service=ssm|DescribeEffectivePatchesForPatchBaseline|status=200->200|keys=$.EffectivePatches[0],$.EffectivePatches[0].Patch.ContentUrl,$.EffectivePatches[1],$.EffectivePatches[2],$.EffectivePatches[3]
   fixture ssm-describe-effective-patches-for-new-baseline step task#2
   detail: $.EffectivePatches[0]: expected {'Patch': {'Id': '11689671-f219-4480-bcd5-d2d4dbd5a6b6', 'KbNumber': 'KB972455', 'Title': 'Windows Server Update Services 3.0 SP2 (KB972455)', 'ContentUrl': 'https://support.microsoft.com/en-us/kb/972455', 'Classification': 'ServicePacks', 'Product': 'Windows7', 'MsrcSeverity': 'Unspecified', ...}, 'PatchStatus': {'DeploymentStatus': 'APPROVED', 'ComplianceLevel': 'UNSPECIFIED', 'ApprovalDate': ...}}, got {'Patch': {'Id': '36260d86-224a-d1b2-5a04-a9d7cd984f2b', 'Title': '2026-02 ServicePacks for Windows7 (KB5031302)', 'Description': 'ServicePacks published by Microsoft for Windows7 in the 2026-02 release.', 'ContentUrl': 'https://support.microsoft.com/en-us/kb/5031302', 'KbNumber': 'KB5031302', 'MsrcSeverity': 'Important', 'Epoch': 0, ...}, 'PatchStatus': {'DeploymentStatus': 'APPROVED', 'ComplianceLevel': 'UNSPECIFIED', 'ApprovalDate': ...}}.

3) divergence_key service=ssm|DescribeEffectivePatchesForPatchBaseline|status=200->200|keys=$.EffectivePatches[0],$.EffectivePatches[1]
   fixture ssm-patch-baseline-create-describe-effective-delete step task#3
   detail: $.EffectivePatches[0] and [1]: missing (expected the Windows7 Updates KB2032276 record and the WindowsServer2008R2 SecurityUpdates KB2032276 record with MsrcNumber 'MS10-043'/MsrcSeverity 'Important', each with PatchStatus {'ComplianceLevel': 'UNSPECIFIED', 'DeploymentStatus': 'EXPLICIT_APPROVED'}), got None.

Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): DescribeEffectivePatchesForPatchBaseline input BaselineId required @length 20-128 pattern ^[a-zA-Z0-9_\-:/]{20,128}$, MaxResults 1-100, output EffectivePatches: EffectivePatchList + NextToken, paginated on EffectivePatches; errors DoesNotExistException, InvalidResourceId, UnsupportedOperatingSystem, InternalServerError. Patch ids, KB numbers, titles and release dates quoted above are catalog values from the recordings, cited to identify which records are missing or substituted - they are not per-run values.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay corpus simparity-wire-aws-v1 (replay_wire_corpus).
2. Cause 1: replay fixture ssm-create-patch-baseline-and-inspect-patches to step task#3 - CreatePatchBaseline for a Windows baseline with explicit approved patches, then call DescribeEffectivePatchesForPatchBaseline with the returned BaselineId. Observe 200 with no EffectivePatches entries where AWS returns five, each with PatchStatus.DeploymentStatus = EXPLICIT_APPROVED.
3. Cause 2: replay fixture ssm-describe-effective-patches-for-new-baseline to step task#2 - create a baseline and describe its effective patches. Observe 200 whose EffectivePatches entries are emulator-generated records ('<YYYY-MM> <Classification> for <Product> (KB<n>)' titles, template descriptions, an extra Epoch member) instead of the real catalog records AWS returns.
4. Cause 3: replay fixture ssm-patch-baseline-create-describe-effective-delete to step task#3 - create a baseline, describe effective patches, delete it. Observe 200 with both expected EffectivePatches entries absent.
5. Re-replay all three fixtures/steps after a fix; each must return one EffectivePatch per patch the stored baseline makes effective, with the catalog's own Patch fields and the correct PatchStatus DeploymentStatus/ComplianceLevel/ApprovalDate.
