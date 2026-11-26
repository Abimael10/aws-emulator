Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent__cloudformation__ssm.kbcfn-export-name-already-claimed-by-the-kb-stack-rejected
Issue-Key: bedrock-agent__cloudformation__ssm.kbcfn-export-name-already-claimed-by-the-kb-stack-rejected|run#12|AssertionError|assert want in reasons, f"expected {want!r} among {reasons!r}"
Issue-Commit: cb678ff2ea6dc380378270cb2637fdde44efeb9e

# bedrock-agent__cloudformation__ssm: the rollback reason for a create blocked by a duplicate export name carries a doubled period ("...by stack <stack>.. Rollback requested by user.")

> _Note: this issue was identified against the code base at commit `cb678ff2ea6dc380378270cb2637fdde44efeb9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
1 net-new root cause. The failing check reported owned_by=[] and no stub, so nothing already filed carries it. The nearest open neighbours are "cloudformation: 4 net-new causes — change-set operation events empty, stack events mis-ordered, export-in-use rollback reason misworded, ListImports refuses a live importer" (20261116T042921Z-cloudformation-4-net-new-causes-change-set-operation.md), whose Cause 4 is the UPDATE_ROLLBACK reason for an export that is IN USE by an importer ("Delete canceled. Cannot delete export ... as it is in use by ..."), a different condition and a different message, and "bedrock-agent__cloudformation__ssm: 2 net-new causes — the in-use-export guard cancels an UpdateStack that does not change the exported value" (20261107T053030Z) and "an UPDATE_ROLLBACK does not restore the stack's Outputs/Exports" (20261116T022245Z) — neither covers the CREATE-time duplicate-export-name refusal reason. This cause is about the stack-level status reason the emulator writes when a CREATE is rolled back because its Output declares an Export name another stack already exports.

Cause 1 of 1: the stack-level rollback reason for a create blocked by an already-claimed export name contains one extra '.' between the refusal sentence and "Rollback requested by user.".
AWS: Smithy: CloudFormation#DescribeStackEvents -> StackEvent.ResourceStatusReason ("Success/failure message associated with the resource."); the message text itself is recorded behavior only. When a CreateStack publishes an Output with an Export whose Name is already exported by another stack, AWS refuses the create and rolls it back, and the stack-level event (LogicalResourceId = the stack name, ResourceType AWS::CloudFormation::Stack) carries exactly one reason sentence pair: fixed text "Export with name " + <ExportName> + " is already exported by stack " + <exporting stack name> + ". Rollback requested by user." — i.e. a single period after the exporting stack name, then a single space, then the fixed rollback sentence. The only variable parts are the export name (taken from the request's template/parameters) and the name of the stack that already owns that export (taken from CloudFormation's export index). The stack's terminal status for this condition is ROLLBACK_COMPLETE.
Emulator: the same event's ResourceStatusReason is "Export with name <ExportName> is already exported by stack <exporting stack name>.. Rollback requested by user." — two consecutive periods after the exporting stack name. Replay shows the emulator's stack-level reasons as ['Export with name <export> is already exported by stack <stack>.. Rollback requested by user.', 'User Initiated'] where the recording has '...by stack <stack>. Rollback requested by user.'. The surrounding behavior is right (the create does roll back and the pre-existing export, its value and its sole importer are untouched); only the reason text diverges, so the failure is purely in how the reason string is composed: the refusal sentence already ends with a period and the rollback suffix is concatenated with a leading '.' (or the suffix is appended as ". Rollback requested by user." onto a message that already terminates in '.').
Why: the ResourceStatusReason is the only machine- and human-readable explanation of why a create rolled back. Deployment tooling (CDK, cfn-lint-driven pipelines, in-house deployers) and operators match on AWS's exact sentence to classify "export name already claimed" and to extract the owning stack's name from it; a reason with a doubled period fails an exact-match or anchored-regex classification, so the condition is reported as an unknown rollback cause and the owning stack is not surfaced to the user.
Fix: the CloudFormation create-rollback path that composes the stack-level ResourceStatusReason for an export-name collision — the message template that joins the export-collision sentence with the "Rollback requested by user." suffix must not insert a second period (join the already-terminated sentence with a single space).
Traces: bedrock-agent__cloudformation__ssm.kbcfn-export-name-already-claimed-by-the-kb-stack-rejected.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (354/639 passed; emulator serving, bootstrap_signal=false). Failing check: [mismatch] bedrock-agent__cloudformation__ssm.kbcfn-export-name-already-claimed-by-the-kb-stack-rejected.

Assertion detail (verbatim from the report):
  AssertionError: expected 'Export with name sim2real-kbcfn-13eaff28e93e-bedrock-kb-MetadataKBKnowledgeBaseId is already exported by stack sim2real-kbcfn-13eaff28e93e-bedrock-kb. Rollback requested by user.' among ['Export with name sim2real-kbcfn-13eaff28e93e-bedrock-kb-MetadataKBKnowledgeBaseId is already exported by stack sim2real-kbcfn-13eaff28e93e-bedrock-kb.. Rollback requested by user.', 'User Initiated']

Expected (AWS): "Export with name <exportName> is already exported by stack <exportingStackName>. Rollback requested by user."
Actual (emulator): "Export with name <exportName> is already exported by stack <exportingStackName>.. Rollback requested by user."

Fixed text (a requirement): "Export with name ", " is already exported by stack ", ". Rollback requested by user." — one period, one space.
Variable parts (filled from the request and emulator state, NOT requirements in their recorded values): <exportName> (here sim2real-kbcfn-<token>-bedrock-kb-MetadataKBKnowledgeBaseId) and <exportingStackName> (here sim2real-kbcfn-<token>-bedrock-kb). The sim2real-* names, the random token, stack ids, timestamps and request ids are per-run values and are not part of the requirement.

The check reads these reasons from DescribeStackEvents (paginated), selecting events whose LogicalResourceId equals the stack name and that carry a ResourceStatusReason. All other assertions in the same check about this condition hold: the duplicate-export create ends ROLLBACK_COMPLETE, the existing export's value is unchanged, and ListImports for that export still returns exactly the one real importer.

## Steps to Reproduce
1. Build the emulator from this checkout and replay simparity-behavioral-aws-v1 (or run the single check bedrock-agent__cloudformation__ssm.kbcfn-export-name-already-claimed-by-the-kb-stack-rejected).
2. CreateStack A from a template whose Outputs publish an Export, e.g. Export Name "<stackA>-MetadataKBKnowledgeBaseId"; wait for CREATE_COMPLETE.
3. CreateStack B from a template whose Output declares an Export with that SAME Name.
4. Wait for B to reach a terminal status — it is ROLLBACK_COMPLETE, as AWS does.
5. Call DescribeStackEvents(StackName=B) and read the ResourceStatusReason of the stack-level events (LogicalResourceId == B).
   Observed: "Export with name <name> is already exported by stack <stackA>.. Rollback requested by user." (two periods).
   Expected: "Export with name <name> is already exported by stack <stackA>. Rollback requested by user." (one period).
