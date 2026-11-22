Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: entityresolution.matching-workflow-job-full-lifecycle
Issue-Shape-Id: com.amazonaws.entityresolution#AWSVeniceService
Issue-Key: entityresolution.matching-workflow-job-full-lifecycle|run#39|AssertionError|assert start_message == expected, f"expected message {expected!r}, got {start_message!r}"
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# entityresolution StartMatchingJob returns the GetMatchingWorkflow not-found wording ("MatchingWorkflow Not Found") instead of AWS's "Workflow not found."

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartMatchingJob

## Description
Cause 1 of 1: StartMatchingJob's ResourceNotFoundException message is wrong — the handler reuses the GetMatchingWorkflow reader's wording instead of StartMatchingJob's own.

AWS: Smithy (services/service-com-amazonaws-entityresolution-awsveniceservice/smithy-model/com-amazonaws-entityresolution-awsveniceservice.json): StartMatchingJob POST /matchingworkflows/{workflowName}/jobs, errors include ResourceNotFoundException (httpError 404, "The resource couldn't be found."); the operation doc states "The workflow must have previously been created using the CreateMatchingWorkflow endpoint". The model does not state the message text, so for the text this is recorded behavior only: against AWS, StartMatchingJob on a workflowName that no longer exists (deleted via DeleteMatchingWorkflow) answers HTTP 404, Code=ResourceNotFoundException, Message="Workflow not found." — that is the whole message, fixed text with no variable part: it does not name the workflow, so nothing from the request or from state is composed into it. Note the same recording shows GetMatchingWorkflow answering a DIFFERENT fixed string for the same missing workflow, "MatchingWorkflow Not Found"; per-operation wording is the requirement here, not one shared string.

Emulator: the same call answers 404/ResourceNotFoundException (status and code are correct) with Message="MatchingWorkflow Not Found" — the GetMatchingWorkflow wording. Replay output: AssertionError: expected message 'Workflow not found.', got 'MatchingWorkflow Not Found'. The preceding assertions of the same check (ConflictException on DeleteMatchingWorkflow while the job runs, and GetMatchingWorkflow's "MatchingWorkflow Not Found" after delete) pass, so only StartMatchingJob's not-found text diverges.

Why: a client that branches on, logs, or asserts the service's not-found text for a start attempt on a deleted/never-created workflow sees text AWS never emits. Error-message assertions in tests and operator runbooks that distinguish the start path ("Workflow not found.") from the read path ("MatchingWorkflow Not Found") mis-classify against the emulator, and the emulator teaches callers the wrong message for this operation.

Fix: the StartMatchingJob handler's not-found refusal and the message constant it reads — the ResourceNotFoundException branch of the handler must carry StartMatchingJob's own fixed text, not a value derived from the GetMatchingWorkflow constant. The comment on the constant records that it was deliberately spelled "in the same constant form as the recorded GET_MATCHING_WORKFLOW_NOT_FOUND" and "Not itself recorded" — the recording now shows that derivation is wrong for this operation. (Several sibling constants — GenerateMatchId, GetMatchId, BatchDeleteUniqueId, UpdateMatchingWorkflow, GetMatchingJob — were derived from the same Get wording; only StartMatchingJob is grounded in a failing check here, but the derivation they share is worth re-checking while fixing this one.)

Traces: entityresolution.matching-workflow-job-full-lifecycle (mismatch), asserting at check line 103 after the job reached SUCCEEDED and the workflow was deleted. Code citations: services/service-com-amazonaws-entityresolution-awsveniceservice/src/ops/start_matching_job.rs lines 225-234 (the ResourceNotFoundException branch) and services/service-com-amazonaws-entityresolution-awsveniceservice/src/state.rs lines 959-965 (START_MATCHING_JOB_WORKFLOW_NOT_FOUND = "MatchingWorkflow Not Found"), with the recorded sibling at line 877 (GET_MATCHING_WORKFLOW_NOT_FOUND).

No open issue covers this: the two open entityresolution-related issues are 20261109T145937Z-cloudformation-entityresolution-no-aws-entityresolution-resource-type-is.md (CloudFormation resource types) and 20261114T172309Z-entityresolution-getproviderservice-definition-documents-carry-synthesized-property-metadata.md (GetProviderService documents).

## Evidence
Expected (AWS, recorded): StartMatchingJob(workflowName=<deleted workflow>) -> HTTP 404, Error.Code="ResourceNotFoundException", Error.Message="Workflow not found." (entire message is fixed text; no request/state values interpolated).
Actual (emulator): HTTP 404, Error.Code="ResourceNotFoundException", Error.Message="MatchingWorkflow Not Found".
Replay detail: AssertionError: expected message 'Workflow not found.', got 'MatchingWorkflow Not Found' (check_entityresolution_matching_workflow_job_full_lifecycle.py line 103).
Control in the same check that passes: GetMatchingWorkflow on the same deleted workflow -> 404/ResourceNotFoundException/"MatchingWorkflow Not Found" (correct), so the two operations must not share one string.

## Steps to Reproduce
1. CreateSchemaMapping and CreateMatchingWorkflow (RULE_MATCHING, Glue input source, S3 output path) as in the check.
2. StartMatchingJob on the workflow and poll GetMatchingJob until the job is terminal (SUCCEEDED).
3. DeleteMatchingWorkflow(workflowName=<that workflow>) — succeeds now that the job is terminal.
4. Call StartMatchingJob(workflowName=<that same, now deleted, workflow>).
5. Observe the error: status 404 and code ResourceNotFoundException are correct, but Message is "MatchingWorkflow Not Found" where AWS answers "Workflow not found.".
