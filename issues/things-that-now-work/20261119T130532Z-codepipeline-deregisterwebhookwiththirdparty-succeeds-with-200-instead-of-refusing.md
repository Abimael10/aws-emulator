Issue-Source: trace-sweep-divergence
Issue-Trace: codepipeline-webhook-put-list-deregister-delete-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-webhook-put-list-deregister-delete-lifecycle
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|DeregisterWebhookWithThirdParty|4xx->2xx|$
Issue-Key: codepipeline|DeregisterWebhookWithThirdParty|4xx->2xx|status
Issue-Commit: 64f6f67e6c728f97090b4bbd99ab3de70e5e9de8

# codepipeline DeregisterWebhookWithThirdParty succeeds with 200 instead of refusing a webhook whose targetPipeline does not exist

> _Note: this issue was identified against the code base at commit `64f6f67e6c728f97090b4bbd99ab3de70e5e9de8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterWebhookWithThirdParty

## Description
Cause 1 of 1: DeregisterWebhookWithThirdParty answers an empty 200 success for a webhook whose stored targetPipeline names no existing pipeline, where AWS refuses the call with a 400 ValidationException. The dangling-target-pipeline refusal is missing from (or unreachable in) this operation's validation on this path.

AWS: Smithy (codepipeline-2015-07-09): DeregisterWebhookWithThirdParty takes a single input member `webhookName` (WebhookName, @length 1..100, pattern ^[A-Za-z0-9.@\-_]+$) and an empty output, and models exactly two errors, both client/HTTP 400: ValidationException and WebhookNotFoundException. The model does not state the ordering of the checks, so the refusal condition here is recorded behavior only: the recording shows that when the named webhook resolves but its definition's targetPipeline names a pipeline that does not exist, AWS returns HTTP 400 with __type "ValidationException" and the message
    Webhook <webhookName> references a pipeline <targetPipeline> which does not exist
Fixed text of that message: "Webhook ", " references a pipeline ", " which does not exist". Its two variable parts are taken from the request and from state: <webhookName> is the request's webhookName, and <targetPipeline> is the targetPipeline stored on that webhook by PutWebhook. The names in the recording (webhook `sim2real-probe-codepipeline-cp-webhook-a6d41248`, pipeline `sim2real-probe-codepipeline-cp-pipeline-ref-a6d41248`) are per-run generated values that fill that template; they are not part of the requirement.

Emulator: the same call returns HTTP 200 with an empty JSON body ({}), i.e. it reports the third-party deregistration as having succeeded. Replay detail for the step: "status: expected 400, got 200" and "$: empty_body (expected {'__type': 'ValidationException', 'message': 'Webhook ... references a pipeline ... which does not exist'}, got {})". Note the emulator DOES know this sentence on another path — open issue 20261117T111420Z-codepipeline-deregisterwebhookwiththirdparty-validates-the-webhook-s-target-pipeline.md reports the emulator emitting exactly this pipeline message where AWS owes a GITHUB_HMAC authentication-type refusal. That issue is about the message chosen for an UNAUTHENTICATED webhook and carries no replay key for this step; it does not cover this divergence, where the refusal is not raised at all and the operation answers 200.

Why: a client relying on AWS's behavior treats a 2xx from DeregisterWebhookWithThirdParty as proof that the webhook was unregistered with the third party and moves on — typically to DeleteWebhook or to tearing down the pipeline wiring. Against the emulator that success is false: the refusal AWS owes (a webhook that cannot be deregistered because its target pipeline is gone) is swallowed, so error-handling and remediation branches are never exercised, and any test that asserts the ValidationException passes against AWS and fails against the emulator. It also makes the operation indistinguishable from the legitimate success case, so a client cannot tell a real deregistration from a no-op.

Fix: DeregisterWebhookWithThirdParty's validation in the CodePipeline webhook handler. After resolving the webhook by name (WebhookNotFoundException when it is absent), the handler must evaluate the stored webhook's targetPipeline against pipeline state and raise ValidationException (HTTP 400) with AWS's sentence above, interpolating the request's webhookName and the stored targetPipeline, rather than falling through to the empty success response. Coordinate with the ordering fix in the open issue named above so that the authentication-type refusal is checked first and this pipeline-existence refusal is still reachable for a GITHUB_HMAC webhook.

Traces: codepipeline-webhook-put-list-deregister-delete-lifecycle#task#4 (the preceding PutWebhook/ListWebhooks steps of this fixture pass, so the webhook and its dangling targetPipeline are stored as AWS stores them).

## Evidence
Fixture: codepipeline-webhook-put-list-deregister-delete-lifecycle, step task#4 (operation DeregisterWebhookWithThirdParty).

Expected (recorded AWS response):
  HTTP 400
  {"__type": "ValidationException", "message": "Webhook <webhookName> references a pipeline <targetPipeline> which does not exist"}
  — in the recording: "Webhook sim2real-probe-codepipeline-cp-webhook-a6d41248 references a pipeline sim2real-probe-codepipeline-cp-pipeline-ref-a6d41248 which does not exist". Fixed text: "Webhook " / " references a pipeline " / " which does not exist"; variable parts: the request's webhookName and the targetPipeline stored on that webhook.

Actual (emulator):
  HTTP 200
  {}

Replay verdict: mismatch. Divergence key service=codepipeline|DeregisterWebhookWithThirdParty|status=400->200|keys=$,status. Detail: "status: expected 400, got 200" / "$: empty_body (expected {'__type': 'ValidationException', 'message': 'Webhook ... references a pipeline ... which does not exist'}, got {})". affected_count=1.

Model evidence: Smithy DeregisterWebhookWithThirdParty — input {webhookName: WebhookName @length 1..100, pattern ^[A-Za-z0-9.@\-_]+$}, output {} (empty), errors ValidationException (client, 400) and WebhookNotFoundException (client, 400). So a 400 ValidationException is a modeled outcome of this operation; the empty 200 the emulator returns is the success shape and is only correct when no refusal condition holds.

## Steps to Reproduce
1. Pick a pipeline name that does not exist in the account/region (e.g. "cp-pipeline-ref").
2. PutWebhook with webhook = {name: "cp-webhook", targetPipeline: "cp-pipeline-ref", targetAction: "Source", filters: [{jsonPath: "$.ref", matchEquals: "refs/heads/main"}], authentication: "GITHUB_HMAC", authenticationConfiguration: {SecretToken: "..."}} — accepted, and the webhook is returned with its ARN.
3. ListWebhooks — the webhook is listed with the dangling targetPipeline echoed back (matches AWS; this step passes today).
4. Call DeregisterWebhookWithThirdParty(webhookName="cp-webhook").
   - Expected: HTTP 400, __type "ValidationException", message "Webhook cp-webhook references a pipeline cp-pipeline-ref which does not exist".
   - Observed: HTTP 200 with an empty body {}.
5. Control: create a pipeline actually named "cp-pipeline-ref", then repeat step 4 — the call must then succeed with the empty 200, which distinguishes the legitimate success from the refusal the emulator currently swallows.
6. Replay the bound fixture step to confirm: codepipeline-webhook-put-list-deregister-delete-lifecycle, step task#4.
