Issue-Source: trace-sweep-divergence
Issue-Trace: cost-explorer-anomaly-validation-negatives
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cost-explorer-anomaly-validation-negatives
Issue-Shape-Id: com.amazonaws.costexplorer#AWSInsightsIndexService
Issue-Key: ce|DeleteAnomalySubscription|4xx->4xx|status
Issue-Commit: 4ed63421c2a299f1b98968b719f9a71bb0543024

# ce DeleteAnomalySubscription serves UnknownSubscriptionException at HTTP 404 instead of Cost Explorer's 400

> _Note: this issue was identified against the code base at commit `4ed63421c2a299f1b98968b719f9a71bb0543024`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAnomalySubscription

## Description
Replay of the Cost Explorer anomaly fixtures against the emulator finds ONE net-new root cause in the anomaly-SUBSCRIPTION delete path (services/service-com-amazonaws-costexplorer-awsinsightsindexservice). A fixer MUST close this one cause before submitting.

--- Cause 1 of 1: the unknown-subscription refusal on DeleteAnomalySubscription is served at the error shape's modelled 404 rather than at the 400 Cost Explorer answers ---

AWS: Smithy costexplorer#DeleteAnomalySubscription takes a required SubscriptionArn (GenericString, @length 0..1024, @pattern ^[\S\s]*$, doc "The unique identifier of the cost anomaly subscription that you want to delete") and declares exactly two errors: LimitExceededException (httpError 400, defaulted) and UnknownSubscriptionException (@httpError 404, doc "The cost anomaly subscription does not exist for the account."). An ARN the account holds no subscription under — never created, or already deleted — is the unknown-subscription condition. The ERROR CLASS is the model's; the WIRE STATUS is recorded behavior: Cost Explorer answers this dangling-reference refusal with HTTP 400, not the 404 the shape's @httpError declares, with the body unchanged — __type "UnknownSubscriptionException" and the same Message. Cost Explorer is a JSON-RPC (awsJson) service and serves its dangling-reference refusals as bad requests; the emulator's own crate already records that rule for the sibling refusals (scaffolded.rs SERVED_AT_BAD_REQUEST, "recorded wire behaviour"). The message itself is NOT at issue here: the replay reports the diverging key set as `status` alone, so $.__type and $.Message already match the recording — only the status is wrong. No value from the recording (the generated subscription ARN/id) is part of the requirement.

Emulator: the handler resolves the ARN, finds no subscription, and correctly raises the modelled UnknownSubscriptionException with the right code and message — but it is serialized at HTTP 404, the shape's modelled @httpError, where AWS sends 400. The cause is the service's per-shape status-override table: it lists only UnknownMonitorException and ResourceNotFoundException as served-at-400, so the third dangling-reference refusal of the same family, UnknownSubscriptionException, falls through to the modelled status. Replay: expected 400, got 404; divergence_key service=ce|DeleteAnomalySubscription|status=400->404|keys=status.

Why: on a JSON-protocol endpoint a 404 reads to SDK retry classifiers, HTTP proxies and gateways as a missing ROUTE rather than as a dangling resource reference the caller must fix — the exact reasoning the crate already writes down for its other two refusals. A client tearing down subscriptions idempotently (delete, then delete again, or delete by a stale ARN) classifies the response by status before it ever reads __type: it sees a transport/endpoint failure and retries a call that can never succeed, instead of treating the subscription as already gone. Cross-operation consistency breaks too: the same account-holds-no-such-resource condition answers 400 from the monitor operations and 404 from this one.

Fix: this belongs in the service's error-serialization rule, not in the handler — the per-service served-at-400 shape set that the operation error channel's http_status path consults must include the unknown-SUBSCRIPTION refusal alongside the unknown-monitor and resource-not-found ones, so every operation that raises it (DeleteAnomalySubscription and any other subscription-resolving operation) answers HTTP 400 with the body the handler already builds. Do not change the error class or the message, and do not override the status inside the handler.

Related open issue that does NOT cover this cause: "ce anomaly-subscription refusals diverge: unknown-monitor create admitted, invalid-token message reworded" (20261104T005255Z-ce-anomaly-subscription-refusals-diverge-unknown-monitor-create.md) is about CreateAnomalySubscription's unknown-MONITOR gate and GetAnomalySubscriptions' invalid-token wording; it carries no replay key for this step and names no DeleteAnomalySubscription divergence.

Traces: cost-explorer-anomaly-validation-negatives#task#8 (DeleteAnomalySubscription, status 400 -> 404). Code the divergence lands on, citations only: services/service-com-amazonaws-costexplorer-awsinsightsindexservice/src/ops/delete_anomaly_subscription.rs:146-157 (the unknown-subscription refusal arm); .../src/scaffolded.rs:193-221 (SERVED_AT_BAD_REQUEST and served_status — the two-shape override set); .../src/scaffolded.rs:333-339 (http_status, where a modelled error takes served_status); .../src/state.rs:705-724 (unknown_monitor, whose doc states the served status is the service's, not the shape's).

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, bootstrap_signal false, 37 steps (26 passed, 11 mismatched, 0 errored, 0 blocked), score_pct 70.3, divergence_count 1. Ten of the eleven mismatches are unimplemented-operation stubs whose issues are filed already; the one divergence cluster below carries neither stub_path nor owned_by.

Cause 1 — cost-explorer-anomaly-validation-negatives#task#8, DeleteAnomalySubscription:
  verdict: mismatch
  detail: "status: expected 400, got 404"
  expected_status: 400, actual_status: 404
  diverging keys: status (ONLY — $.__type and $.Message match the recording)
  divergence_key: service=ce|DeleteAnomalySubscription|status=400->404|keys=status
  affected: cost-explorer-anomaly-validation-negatives#task#8 (affected_count 1)

Expected (AWS, recorded): HTTP 400, body __type "UnknownSubscriptionException" with its Message.
Actual (emulator): HTTP 404, same body.

Model facts used (smithy_query on services/service-com-amazonaws-costexplorer-awsinsightsindexservice/smithy-model/com-amazonaws-costexplorer-awsinsightsindexservice.json):
  DeleteAnomalySubscription: input SubscriptionArn (GenericString, required, length 0..1024, pattern ^[\S\s]*$); output empty; errors LimitExceededException (400, httpErrorDefaulted) and UnknownSubscriptionException (404).
  UnknownSubscriptionException: structure, errorKind client, @httpError 404, member Message -> ErrorMessage, doc "The cost anomaly subscription does not exist for the account."

Emulator-side evidence that the 400 rule exists but omits this shape (citations only):
  scaffolded.rs:180-196 — UNKNOWN_MONITOR_EXCEPTION and RESOURCE_NOT_FOUND_EXCEPTION are the only two members of SERVED_AT_BAD_REQUEST, documented as "the dangling-reference refusals, whose recorded wire status is 400 for every operation that raises them".
  scaffolded.rs:215-221 — served_status() returns 400 for a shape in that set and the modelled status otherwise; UnknownSubscriptionException is not in the set, so it keeps 404.
  delete_anomaly_subscription.rs:146-157 — the refusal arm returns UnknownSubscriptionException via crate::state::unknown_subscription(&input.subscription_arn), so code and message are correct and only the status is taken from the shape.

## Steps to Reproduce
Boot the emulator from this checkout and replay the Cost Explorer fixtures (replay_wire_corpus, trace set simparity-wire-aws-v1).

Cause 1 (cost-explorer-anomaly-validation-negatives#task#8):
  1. Call DeleteAnomalySubscription { SubscriptionArn: <a well-formed subscription ARN the account holds no subscription under> } — either an ARN that was never created, or one an earlier DeleteAnomalySubscription already removed.
  2. AWS answers HTTP 400 with body __type "UnknownSubscriptionException" and its Message.
  3. The emulator answers HTTP 404 with the SAME body: the error code and message are right, the status class is not.
  4. Contrast with the sibling condition to see the inconsistency: resolve a monitor ARN the account does not hold (e.g. DeleteAnomalyMonitor / CreateAnomalySubscription naming an unheld monitor) — that refusal IS served at 400 by the service's override table, while the subscription one is not.
