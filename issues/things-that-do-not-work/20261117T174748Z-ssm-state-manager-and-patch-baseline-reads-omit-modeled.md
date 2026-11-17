Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-association-tag-change-and-remove-last
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-association-tag-change-and-remove-last ssm-association-unset-optional-fields-absent-and-update-read-back ssm-create-patch-baseline-and-inspect-patches ssm-describe-effective-patches-for-new-baseline ssm-patch-baseline-create-describe-effective-delete ssm-state-manager-association-workflow ssm-automation-stop-and-signal-error-paths
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|CreateAssociation|2xx->2xx|$.AssociationDescription.Date
Issue-Key: ssm|CreateAssociation|2xx->2xx|$.AssociationDescription.LastUpdateAssociationDate
Issue-Key: ssm|DescribeAssociation|2xx->2xx|$.AssociationDescription.Date
Issue-Key: ssm|DescribeAssociation|2xx->2xx|$.AssociationDescription.LastUpdateAssociationDate
Issue-Key: ssm|UpdateAssociation|2xx->2xx|$.AssociationDescription.Date
Issue-Key: ssm|UpdateAssociation|2xx->2xx|$.AssociationDescription.LastUpdateAssociationDate
Issue-Key: ssm|DescribeEffectivePatchesForPatchBaseline|2xx->2xx|$.EffectivePatches[*]
Issue-Key: ssm|DescribeAssociationExecutions|2xx->2xx|$.AssociationExecutions[*]
Issue-Key: ssm|StopAutomationExecution|4xx->4xx|$.Message
Issue-Related: 20261105T101017Z-ssm-state-manager-associations-refusal-wording-response-projection.md
Issue-Commit: 0d074a71eaca712c4eb2c65274b3985903118640

# State Manager and patch baseline reads omit modeled members and never finish executing (5 causes)

> _Note: this issue was identified against the code base at commit `0d074a71eaca712c4eb2c65274b3985903118640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAssociation

## Description
Five net-new root causes in the SSM State Manager, patch-baseline and Automation planes, all on ssm. A fixer must close ALL FIVE before submitting.

Cause 1 of 5 - CreateAssociation/DescribeAssociation omit AssociationDescription.DocumentVersion.
AWS: Smithy - AssociationDescription.DocumentVersion targets `DocumentVersion` with pattern ^([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)$. When an association is created without an explicit document version, the recorded AWS response carries DocumentVersion = '$DEFAULT' (fixed literal, not a per-run value).
Emulator: the member is absent from the response entirely.
Why: a client that reads DocumentVersion back to decide whether the association is pinned to a version or tracks the default sees nothing, and SDK fields typed as non-optional strings decode to empty.
Fix: the CreateAssociation / DescribeAssociation response projection in the ssm service - default the stored association's document version to $DEFAULT and serialize it.
Traces: ssm-association-tag-change-and-remove-last#task#2 (representative; cluster also covers ssm-association-unset-optional-fields-absent-and-update-read-back#task#2) and ssm-association-unset-optional-fields-absent-and-update-read-back#task#3.

Cause 2 of 5 - association timestamps are serialized out of shape.
AWS: Smithy - AssociationDescription.Date and .LastUpdateAssociationDate target `DateTime` (@timestamp), serialized as epoch seconds under awsJson1_1. Every recorded AWS timestamp in this corpus carries at most millisecond resolution (e.g. 1791789721.726); the corpus asserts the member's shape rather than the instant, and the emulator's value does not satisfy it.
Emulator: emits the raw clock float - Date = 1794937147.1664493 and LastUpdateAssociationDate = 1794937147.1664493 (CreateAssociation), 1794937245.5360205 / 1794937245.7490706 (UpdateAssociation and the DescribeAssociation read-backs). Replay verdict on each: "value_mismatch (expected '<shaped-not-asserted>', got ...)". Hypothesis: the serializer writes the f64 directly instead of the millisecond-resolution value AWS publishes.
Why: a client comparing a timestamp it read back against one the real service would have produced, or re-serializing the record, sees a value of a shape AWS never emits.
Fix: the shared timestamp serializer behind the association handlers in the ssm service (the same normalization the Parameter Store reads need).
Traces: ssm-association-tag-change-and-remove-last#task#2, ssm-association-unset-optional-fields-absent-and-update-read-back#task#4 (UpdateAssociation), #task#5 (DescribeAssociation).

Cause 3 of 5 - DescribeEffectivePatchesForPatchBaseline answers 200 with an empty list.
AWS: Smithy - DescribeEffectivePatchesForPatchBaselineResult.EffectivePatches is an EffectivePatchList of EffectivePatch{Patch, PatchStatus}; for a freshly created Windows baseline AWS answers 200 with the patches the baseline's rules make effective, each carrying Patch{Classification, ContentUrl, Description, Id, KbNumber, Language, MsrcNumber, MsrcSeverity, Product, ProductFamily, ReleaseDate, Title, Vendor} and PatchStatus{ComplianceLevel, DeploymentStatus, ApprovalDate where approved}. The recordings show 2-5 such entries.
Emulator: answers 200 but EffectivePatches is empty - every indexed element is reported missing.
Why: a client that creates a baseline and then inspects which patches it approves gets an empty answer with a success status, so patch-compliance tooling concludes the baseline approves nothing instead of failing loudly.
Fix: the DescribeEffectivePatchesForPatchBaseline handler plus the seeded patch catalog in the ssm service state - evaluate the baseline's approval rules against the catalog instead of returning an empty list.
Traces: ssm-create-patch-baseline-and-inspect-patches#task#3, ssm-describe-effective-patches-for-new-baseline#task#2, ssm-patch-baseline-create-describe-effective-delete#task#3.

Cause 4 of 5 - DescribeAssociationExecutions reports an execution that never progresses.
AWS: Smithy - AssociationExecution{AssociationId, AssociationVersion (pattern ^(([$]LATEST)|([1-9][0-9]*))$), ExecutionId, Status, DetailedStatus, CreatedTime, ResourceCountByStatus}. The recorded execution for the association created in the fixture is {AssociationVersion: '1', Status: 'Success', DetailedStatus: 'Success', ResourceCountByStatus: '{}', ExecutionId and CreatedTime per-run}.
Emulator: returns {AssociationVersion: '2', Status: 'Pending', ExecutionId, CreatedTime} - the status never leaves Pending, DetailedStatus and ResourceCountByStatus are omitted, and the execution is attributed to association version 2 rather than the version that ran (1).
Why: a client that polls DescribeAssociationExecutions until Status is a terminal value never terminates, and one that aggregates ResourceCountByStatus or reads DetailedStatus gets nothing; the wrong AssociationVersion also misattributes the execution to a later revision of the association.
Fix: the association-execution state machine in the ssm service (association state + DescribeAssociationExecutions projection) - drive the execution to its terminal Success status, record the version that ran, and serialize DetailedStatus and ResourceCountByStatus.
Traces: ssm-state-manager-association-workflow#task#7.

Cause 5 of 5 - StopAutomationExecution's refusal message is missing its final period.
AWS: Smithy - StopAutomationExecution raises InvalidAutomationStatusUpdateException (client, HTTP 400) when a completion request is sent to an Auto-mode automation. Recorded message: "Failed to complete automation <automationExecutionId>, automation is running in Auto mode. Completion request is only supported in Interactive mode." The two sentences are fixed text and the only variable part is the automation execution id taken from the request; the trailing period is part of the fixed text.
Emulator: emits the same text with the final period dropped - "... Completion request is only supported in Interactive mode". HTTP 400 on both sides.
Why: callers and test suites that match the documented message exactly (string equality or an anchored pattern) do not recognise the emulator's refusal, so an expected failure path is reported as an unexpected one.
Fix: the StopAutomationExecution refusal message in the ssm service handler/validation.
Traces: ssm-automation-stop-and-signal-error-paths#task#4.

None of these nine clusters is owned: the replay report listed owned_by = [] for each. The open issues 20261105T101017Z-ssm-state-manager-associations-refusal-wording-response-projection.md and 20261112T101725Z-ssm-association-reads-and-writes-diverge-not-found.md own the DocumentVersion/Overview gaps on OTHER fixture steps and do not carry the steps above; 20261114T164648Z-ssm-2-net-new-causes-sendautomationsignal-stopstep-precondition.md owns SendAutomationSignal in the same fixture, not StopAutomationExecution's message.

## Evidence
Replay of trace set simparity-wire-aws-v1 (180 fixtures, 907 steps, score 74.3%):
- ssm-association-tag-change-and-remove-last#task#2 CreateAssociation (200/200): $.AssociationDescription.DocumentVersion missing (expected '$DEFAULT'); $.AssociationDescription.Date value_mismatch (expected '<shaped-not-asserted>', got 1794937147.1664493); $.AssociationDescription.LastUpdateAssociationDate likewise. Cluster also covers ssm-association-unset-optional-fields-absent-and-update-read-back#task#2.
- ssm-association-unset-optional-fields-absent-and-update-read-back#task#3 DescribeAssociation: DocumentVersion missing (expected '$DEFAULT'), Date/LastUpdateAssociationDate got 1794937245.5360205.
- same fixture #task#4 UpdateAssociation and #task#5 DescribeAssociation: Date got 1794937245.5360205, LastUpdateAssociationDate got 1794937245.7490706, both against '<shaped-not-asserted>'.
- ssm-create-patch-baseline-and-inspect-patches#task#3, ssm-describe-effective-patches-for-new-baseline#task#2, ssm-patch-baseline-create-describe-effective-delete#task#3 DescribeEffectivePatchesForPatchBaseline (200/200): $.EffectivePatches[0..4] missing, e.g. expected {'Patch': {'Classification': 'Updates', 'Id': '4a56e378-4c48-4284-abb0-a36028f5dbde', 'KbNumber': 'KB2032276', 'Product': 'Windows7', ...}, 'PatchStatus': {'ComplianceLevel': 'UNSPECIFIED', 'DeploymentStatus': 'EXPLICIT_APPROVED'}}, got None.
- ssm-state-manager-association-workflow#task#7 DescribeAssociationExecutions (200/200): expected {'AssociationVersion': '1', 'Status': 'Success', 'DetailedStatus': 'Success', 'ResourceCountByStatus': '{}', 'CreatedTime': <shaped>, 'ExecutionId': <shaped>}; got {'AssociationVersion': '2', 'Status': 'Pending', 'ExecutionId': 'be5354c2-...', 'CreatedTime': 1794937227.6355016} with no DetailedStatus and no ResourceCountByStatus.
- ssm-automation-stop-and-signal-error-paths#task#4 StopAutomationExecution (400/400): $.Message expected '... Completion request is only supported in Interactive mode.'; got the same text without the trailing period.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1/2: replay ssm-association-tag-change-and-remove-last#task#2 and ssm-association-unset-optional-fields-absent-and-update-read-back#task#3/#task#4/#task#5; inspect AssociationDescription.DocumentVersion, .Date and .LastUpdateAssociationDate.
4. Cause 3: replay ssm-create-patch-baseline-and-inspect-patches#task#3 (and the two other patch-baseline fixtures) - CreatePatchBaseline for a Windows baseline then DescribeEffectivePatchesForPatchBaseline must return the effective patches, not an empty list.
5. Cause 4: replay ssm-state-manager-association-workflow#task#7 - the association execution must report AssociationVersion '1', Status/DetailedStatus 'Success' and ResourceCountByStatus '{}'.
6. Cause 5: replay ssm-automation-stop-and-signal-error-paths#task#4 - StopAutomationExecution with Type=Complete against an Auto-mode automation must answer the documented message including its final period.
