Issue-Source: trace-sweep-divergence
Issue-Trace: cost-explorer-anomaly-monitor-lifecycle
Issue-Step: task#15
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cost-explorer-anomaly-monitor-lifecycle cost-explorer-anomaly-validation-negatives
Issue-Shape-Id: com.amazonaws.costexplorer#AWSInsightsIndexService
Issue-Key: ce|ListTagsForResource|4xx->4xx|$.Message
Issue-Key: ce|ListTagsForResource|4xx->4xx|status
Issue-Key: ce|TagResource|4xx->4xx|$.Message
Issue-Key: ce|TagResource|4xx->4xx|status
Issue-Key: ce|UntagResource|4xx->4xx|$.Message
Issue-Key: ce|UntagResource|4xx->4xx|status
Issue-Commit: 1490d5d01f48dd097a9435a06996bc23d586834e

# ce tagging ResourceNotFoundException diverges: served 404 with the model's doc sentence instead of 400 "Resource not found"

> _Note: this issue was identified against the code base at commit `1490d5d01f48dd097a9435a06996bc23d586834e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
All three Cost Explorer tagging operations refuse an unresolvable `ResourceArn` with the wrong HTTP status AND a message the emulator took from the Smithy documentation string rather than AWS's wire text. One shared root cause, three operations (3 causes bundled).

Shared AWS behavior (recorded behavior only for the status and the message; model cited for identity): Cost Explorer is awsJson1_1. For a well-formed `ResourceArn` (Smithy: `Arn`, @length 20..2048, pattern ^arn:aws[-a-z0-9]*:[a-z0-9]+:[-a-z0-9]*:[0-9]{12}:[-a-zA-Z0-9/:_]+$) that names no resource the account holds, the recorded responses are HTTP 400 with error identity `ResourceNotFoundException` and body `Message` = "Resource not found". That message is WHOLLY FIXED text: no ARN, id, name, number or timestamp is interpolated into it (the ARN the caller sent has its own member, `ResourceNotFoundException$ResourceName`). The service's modeled @httpError for this shape is 404 (Smithy: `ResourceNotFoundException`, errorKind client, @httpError 404, doc "The specified ARN in the request doesn't exist."), but the recorded wire status is 400 — the same service-level status override the crate already applies to `UnknownMonitorException` (see services/service-com-amazonaws-costexplorer-awsinsightsindexservice/src/scaffolded.rs `served_status`, which overrides ONLY that one shape and lets every other modeled error keep its @httpError).

Shared emulator behavior: HTTP 404, with `Message` set to the model's documentation sentence "The specified ARN in the request doesn't exist." — i.e. the doc string is being served as the error message, and the service's status rule is not applied to this shape.

Cause 1 of 3: ListTagsForResource on a well-formed ARN the account does not hold.
  AWS: 400, ResourceNotFoundException, Message "Resource not found" (fixed text).
  Emulator: 404, Message "The specified ARN in the request doesn't exist."
  Fixture: cost-explorer-anomaly-monitor-lifecycle, step task#15.

Cause 2 of 3: TagResource on a well-formed ARN the account does not hold (Smithy: errors LimitExceededException 400, ResourceNotFoundException 404, TooManyTagsException 400).
  AWS: 400, ResourceNotFoundException, Message "Resource not found".
  Emulator: 404, Message "The specified ARN in the request doesn't exist."
  Fixture: cost-explorer-anomaly-validation-negatives, step task#4.

Cause 3 of 3: UntagResource on a well-formed ARN the account does not hold (Smithy: errors LimitExceededException 400, ResourceNotFoundException 404).
  AWS: 400, ResourceNotFoundException, Message "Resource not found".
  Emulator: 404, Message "The specified ARN in the request doesn't exist."
  Fixture: cost-explorer-anomaly-validation-negatives, step task#5.

Why this matters: a 404 from a JSON-protocol AWS endpoint reads to SDK retry middleware and to HTTP proxies as a missing route/endpoint rather than as a dangling resource reference the caller must fix — the crate's own `served_status` doc states exactly that for the monitor refusal, so the tagging family contradicts the rule the same crate established. A client that branches on the error message (or on an HTTP 400 vs 404 split before parsing the body) treats a tag-a-missing-resource call differently against the emulator than against AWS, and tooling that retries 404s as transient endpoint failures loops on a request that can never succeed.

Fix: this belongs in the service's shared error path, not in three handlers. (a) The per-service served-status rule in the crate's error channel must serve `ResourceNotFoundException` at 400 as well, keeping the body's `__type` and message as the handler built them. (b) The shared unknown-tagged-resource refusal builder in the crate's state module must carry Cost Explorer's own fixed sentence "Resource not found" instead of the model's documentation sentence; if the ARN is to be reported at all it belongs in `ResourceNotFoundException$ResourceName`, not spliced into `Message`.

Related but NOT covering this: no open issue names Cost Explorer (`ce`) at all — a keyword search for "cost-explorer" returns none, and the open ResourceNotFoundException-wording issues are all for other services.

Traces: cost-explorer-anomaly-monitor-lifecycle#task#15 (ListTagsForResource), cost-explorer-anomaly-validation-negatives#task#4 (TagResource), cost-explorer-anomaly-validation-negatives#task#5 (UntagResource).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200):

1) ce ListTagsForResource — fixture cost-explorer-anomaly-monitor-lifecycle, step task#15
   expected status 400, got 404
   $.Message: expected 'Resource not found', got "The specified ARN in the request doesn't exist."
   divergence_key: service=ce|ListTagsForResource|status=400->404|keys=$.Message,status

2) ce TagResource — fixture cost-explorer-anomaly-validation-negatives, step task#4
   expected status 400, got 404
   $.Message: expected 'Resource not found', got "The specified ARN in the request doesn't exist."
   divergence_key: service=ce|TagResource|status=400->404|keys=$.Message,status

3) ce UntagResource — fixture cost-explorer-anomaly-validation-negatives, step task#5
   expected status 400, got 404
   $.Message: expected 'Resource not found', got "The specified ARN in the request doesn't exist."
   divergence_key: service=ce|UntagResource|status=400->404|keys=$.Message,status

Model evidence (services/service-com-amazonaws-costexplorer-awsinsightsindexservice/smithy-model/com-amazonaws-costexplorer-awsinsightsindexservice.json):
  ResourceNotFoundException: errorKind client, httpError 404, members Message (ErrorMessage), ResourceName (Arn), doc "The specified ARN in the request doesn't exist." — identical to the string the emulator serves as Message.
  ListTagsForResource / TagResource / UntagResource all declare ResourceNotFoundException.

Code evidence:
  services/service-com-amazonaws-costexplorer-awsinsightsindexservice/src/scaffolded.rs:201 `served_status` overrides the status for UnknownMonitorException ONLY (-> 400) and returns the modeled status for every other error, so ResourceNotFoundException leaves at 404.
  services/service-com-amazonaws-costexplorer-awsinsightsindexservice/src/ops/tag_resource.rs:172-177 raises `ResourceNotFoundException` via the shared `crate::state::unknown_tagged_resource(&input.resource_arn)` builder — the one place all three operations' message text comes from.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the bound wire corpus (`replay_wire_corpus`).
2. Cause 1: replay fixture `cost-explorer-anomaly-monitor-lifecycle` step `task#15` — a ce ListTagsForResource with a well-formed `ResourceArn` for a resource the account does not hold (the monitor deleted earlier in the fixture). Observe HTTP 404 and Message "The specified ARN in the request doesn't exist." instead of HTTP 400 and Message "Resource not found".
3. Cause 2: replay fixture `cost-explorer-anomaly-validation-negatives` step `task#4` — ce TagResource against the same class of unresolvable ARN. Same divergence.
4. Cause 3: replay fixture `cost-explorer-anomaly-validation-negatives` step `task#5` — ce UntagResource against the same class of unresolvable ARN. Same divergence.
5. All three must be re-replayed after a fix: the status rule and the message text are separate edits (error channel vs state helper) and either one alone leaves the steps mismatched.
