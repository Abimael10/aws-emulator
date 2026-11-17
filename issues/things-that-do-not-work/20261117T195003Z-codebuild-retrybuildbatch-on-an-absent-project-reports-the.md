Issue-Source: trace-sweep-divergence
Issue-Trace: codebuild-build-and-batch-registry-reads
Issue-Step: task#13
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codebuild-build-and-batch-registry-reads
Issue-Shape-Id: com.amazonaws.codebuild#CodeBuild_20161006
Issue-Key: codebuild|RetryBuildBatch|4xx->4xx|$.message
Issue-Commit: a8fcfffc679813c60869f4713da5f56b7116d30a

# CodeBuild RetryBuildBatch on an absent project reports the batch as missing instead of the project

> _Note: this issue was identified against the code base at commit `a8fcfffc679813c60869f4713da5f56b7116d30a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RetryBuildBatch

## Description
ONE net-new root cause in the codebuild crate (services/service-com-amazonaws-codebuild-codebuild-20161006). RetryBuildBatch's not-found path names the wrong resource class: it reports the build batch as missing where AWS reports the owning project as missing, so the refusal message a client reads identifies neither the right resource type nor the right identifier.

In the wording below the fixed text is what is quoted outside angle brackets; the angle-bracketed parts are interpolated from the request and from service state (project name, project ARN, batch id) and must never be hard-coded from one recording.

Cause 1 of 1: RetryBuildBatch(id='<project-name>:<uuid>') for a project that does not exist is refused with a BuildBatch-not-found message instead of AWS's project-not-found message.
AWS: Smithy RetryBuildBatch(id NonEmptyString @length min 1, idempotencyToken String, retryType RetryBuildBatchType) -> {buildBatch}; modeled errors InvalidInputException and ResourceNotFoundException (both errorKind client, httpError 400). A CodeBuild build-batch id is '<project-name>:<batch-uuid>', so AWS resolves the project named by the id's prefix FIRST and, when no such project exists, answers HTTP 400 with message 'Project cannot be found: <project ARN>' — fixed text 'Project cannot be found: ' followed by the ARN of the project named in the submitted id (arn:aws:codebuild:<region>:<account>:project/<project-name>), built from the request's project-name prefix and the caller's region/account. The batch id itself is never echoed, because the batch's absence is not what AWS reports when its project is absent.
Emulator: HTTP 400 (status matches, and the error code matches — $.message is the only diverging field) with message 'BuildBatch cannot be found: <project-name>:<batch-uuid>', i.e. the emulator skips the project-existence check, goes straight to a batch-registry lookup, and reports the batch id from the request as the missing resource.
Why: a client is told a specific build batch is missing when in fact the whole project is absent, so recovery logic points at the wrong resource — code that reacts by re-creating or re-listing batches under a project that does not exist loops instead of re-provisioning the project, and exact-match handling or log alerting keyed on AWS's 'Project cannot be found: ' phrasing never fires. The emulator also leaks the batch uuid into a message AWS keeps free of it.
Fix: the RetryBuildBatch handler's resource-resolution order and not-found message in the codebuild crate — resolve the project named by the id's '<project-name>:' prefix before the batch lookup and emit ResourceNotFoundException with 'Project cannot be found: <project ARN>'; keep the batch-not-found wording only for the case where the project exists and the batch does not.
Traces: codebuild-build-and-batch-registry-reads#task#13.

Related open issues that do NOT cover this step: 20261029T175929Z-codebuild-absent-resource-and-invalid-input-error-messages.md is the same wording family on the same fixture but carries replay keys for task#6 (ListBuildsForProject), task#7 (ListBuildBatchesForProject) and task#12 (StopBuildBatch) only — it has no key for task#13 and does not touch RetryBuildBatch; 20261029T204615Z-codebuild-error-message-wording-diverges-from-aws-on.md covers behavioral checks on other operations. Neither owns this step, so it must be replayed.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 182 steps, 176 passed, 6 mismatched, score 96.7% (the other mismatches are blocked by unimplemented operations or owned by existing issues).

Cause 1 — codebuild-build-and-batch-registry-reads#task#13 RetryBuildBatch, expected 400 / actual 400, divergence on $.message only (value_mismatch):
  expected: 'Project cannot be found: arn:aws:codebuild:<region>:<account>:project/<project-name>'
  actual:   'BuildBatch cannot be found: <project-name>:<batch-uuid>'
In the recorded run the interpolated values were the probe project 'sim2real-probe-codebuild-codebuild-absent-build-proj-27ec1275' (ARN arn:aws:codebuild:us-west-2:000000000000:project/sim2real-probe-codebuild-codebuild-absent-build-proj-27ec1275) and batch uuid 33333333-3333-3333-3333-333333333333 — per-run values filling the template, not part of the requirement. Fixed text: 'Project cannot be found: '. Variable part: the project ARN derived from the id's project-name prefix plus the caller's region and account.

Divergence key: service=codebuild|RetryBuildBatch|status=400->400|keys=$.message. Cluster size 1 (affected: codebuild-build-and-batch-registry-reads#task#13); no stub_path and no owning issue.

Smithy model consulted: services/service-com-amazonaws-codebuild-codebuild-20161006/smithy-model/com-amazonaws-codebuild-codebuild-20161006.json — RetryBuildBatch(input {id: NonEmptyString @length min 1, idempotencyToken, retryType: RetryBuildBatchType}) -> {buildBatch}, errors InvalidInputException and ResourceNotFoundException (client, httpError 400, defaulted).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from this checkout.
2. Replay the wire corpus (replay_wire_corpus) and read the codebuild divergence for fixture codebuild-build-and-batch-registry-reads, step task#13.
3. Or reproduce directly against the emulator endpoint: without creating any project, call RetryBuildBatch(id='<a-project-name-that-was-never-created>:<any uuid>').
4. Expect HTTP 400, __type ResourceNotFoundException, message 'Project cannot be found: arn:aws:codebuild:<region>:<account>:project/<that project name>'.
5. Observe HTTP 400 with message 'BuildBatch cannot be found: <project-name>:<uuid>'.
6. Control: CreateProject with that name, then RetryBuildBatch(id='<that name>:<a uuid that is not a batch of it>') — here the project exists, so the batch-not-found wording is the correct answer; only the absent-project case must change.
7. Re-replay codebuild-build-and-batch-registry-reads#task#13 to confirm the fix.
