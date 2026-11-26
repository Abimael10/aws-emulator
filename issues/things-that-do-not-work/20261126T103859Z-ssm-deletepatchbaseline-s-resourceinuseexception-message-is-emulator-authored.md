Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.e2e-patch-baseline-compliance-flow
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.e2e-patch-baseline-compliance-flow|run#23|AssertionError|assert in_use["Message"] == (
Issue-Commit: f214ea914d97c777aba1c48734a44751ff321c53

# ssm DeletePatchBaseline's ResourceInUseException message is emulator-authored, breaking the end-to-end patch-baseline compliance flow

> _Note: this issue was identified against the code base at commit `f214ea914d97c777aba1c48734a44751ff321c53`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePatchBaseline

## Description
Cause 1 of 1: DeletePatchBaseline on a baseline that is still registered to a patch group is refused with the right status and code but the wrong message (failing check: ssm.e2e-patch-baseline-compliance-flow).

AWS: Smithy (ssm): DeletePatchBaseline input {BaselineId: BaselineId, required, length 20..128, pattern ^[a-zA-Z0-9_\-:/]{20,128}$}; modeled errors InternalServerError (server, httpError 500) and ResourceInUseException (client, httpError 400), the latter documented "Error returned if an attempt is made to delete a patch baseline that is registered for a patch group." The refusal is HTTP 400 ResourceInUseException with the message "PatchBaseline to be deleted is used for Patch Group: <patch group>" — everything up to and including the ": " is fixed text, and the one variable part is the name of the patch group the baseline is still registered to, taken from the emulator's own state. The baseline and its registration survive the rejected delete (GetPatchBaseline still answers 200 with the same BaselineId), and once DeregisterPatchBaselineForPatchGroup has run, DeletePatchBaseline succeeds and GetPatchBaseline then fails DoesNotExistException "Patch Baseline to be retrieved does not exist.".

Emulator: the 400 status and the ResourceInUseException code are correct, but the message is "Patch baseline is registered with patch group <patch group>" — different fixed text throughout (lower-cased "Patch baseline", "is registered with patch group", no "to be deleted", no colon). Only the patch-group value matches.

Why: the message is the only part of this refusal that carries information, so re-wording it is the whole divergence. A client that matches or displays the service's sentence sees wording SSM never emits, and runbooks/automation keyed on AWS's documented sentence ("PatchBaseline to be deleted is used for Patch Group: ...", which names the blocking group in a parseable position after the colon) cannot extract the group to deregister.

Fix: the DeletePatchBaseline handler's in-use refusal message builder (the patch-group state helper that formats the baseline-in-use sentence). Open issue 20261120T214510Z-ssm-5-net-new-causes-patchbaseline-tag-reads.md describes this same wording as its Cause 3, grounded in ssm.delete-patch-baseline-rejects-when-registered-to-group and ssm.patch-baseline-delete-while-registered-rejected; it does NOT carry this check's replay key, so the end-to-end compliance flow still fails after that issue's causes are closed. A fixer closing that cause should re-replay this check too.

Traces: ssm.e2e-patch-baseline-compliance-flow (the negative leg after PutComplianceItems; the compliance roll-up and ListComplianceItems assertions ahead of it passed).

## Evidence
Failing check ssm.e2e-patch-baseline-compliance-flow, at the assertion on the refusal message:

AssertionError: Patch baseline is registered with patch group sim2real-pg-0e0834fee28f

The check asserts in_use['Code'] == 'ResourceInUseException' (passes) and then in_use['Message'] == f"PatchBaseline to be deleted is used for Patch Group: {patch_group}" (fails, printing the actual message).

Expected: HTTP 400, ResourceInUseException, Message "PatchBaseline to be deleted is used for Patch Group: <patch group>".
Actual: HTTP 400, ResourceInUseException, Message "Patch baseline is registered with patch group <patch group>".

The group name shown ('sim2real-pg-0e0834fee28f') is a per-run generated value filling the template, not part of the requirement.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it (the behavioral suite does this).
2. ssm CreatePatchBaseline(Name=<n>, OperatingSystem='AMAZON_LINUX_2', ApprovalRules={'PatchRules':[{'PatchFilterGroup':{'PatchFilters':[{'Key':'CLASSIFICATION','Values':['Security']}]},'ApproveAfterDays':7,'ComplianceLevel':'CRITICAL'}]}) — note <BaselineId>.
3. ssm RegisterPatchBaselineForPatchGroup(BaselineId=<BaselineId>, PatchGroup=<g>).
4. ssm DeletePatchBaseline(BaselineId=<BaselineId>) — the call is refused 400 ResourceInUseException (correct); compare its Message with "PatchBaseline to be deleted is used for Patch Group: <g>". The emulator answers "Patch baseline is registered with patch group <g>".
5. ssm GetPatchBaseline(BaselineId=<BaselineId>) — still present, as required.
6. ssm DeregisterPatchBaselineForPatchGroup(BaselineId=<BaselineId>, PatchGroup=<g>) then DeletePatchBaseline(BaselineId=<BaselineId>) — succeeds, and GetPatchBaseline then fails DoesNotExistException.
