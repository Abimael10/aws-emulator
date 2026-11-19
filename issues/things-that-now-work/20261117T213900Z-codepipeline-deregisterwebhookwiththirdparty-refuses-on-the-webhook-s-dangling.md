Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codepipeline.webhook-targeting-absent-pipeline-lifecycle
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|DeregisterWebhookWithThirdParty|ValidationException
Issue-Commit: a9ab6962eb5d2d8d5dd44a1b5b7b704cefe591f3

# codepipeline: DeregisterWebhookWithThirdParty refuses on the webhook's dangling targetPipeline instead of AWS's GITHUB_HMAC authentication-type refusal

> _Note: this issue was identified against the code base at commit `a9ab6962eb5d2d8d5dd44a1b5b7b704cefe591f3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterWebhookWithThirdParty

## Description
Cause 1 of 1: DeregisterWebhookWithThirdParty orders (or substitutes) its validation wrongly — it resolves the named webhook's targetPipeline and refuses on that pipeline not existing, so the authentication-type refusal AWS owes for a non-GITHUB_HMAC webhook is never produced. Failing check: codepipeline.webhook-targeting-absent-pipeline-lifecycle.

NOTE: open issue 20261117T111420Z-codepipeline-deregisterwebhookwiththirdparty-validates-the-webhook-s-target-pipeline.md describes the same condition, but the replay report does not mark this check as OWNED by it (it carries no replay key for codepipeline.webhook-targeting-absent-pipeline-lifecycle), so the check is still ungated and is filed here; if that issue is being worked, close both against the same fix.

AWS: Smithy (codepipeline-2015-07-09): DeregisterWebhookWithThirdParty takes only `webhookName` (WebhookName, @length 1..100, pattern ^[A-Za-z0-9.@\-_]+$) and declares exactly two errors, ValidationException and WebhookNotFoundException, both client, HTTP 400. The model states nothing about a target pipeline being a precondition — recorded behavior only: third-party registration applies solely to a GITHUB_HMAC webhook, so AWS resolves the webhook by name and then refuses any other authentication type with ValidationException (HTTP 400) and the message
    Webhook <webhookName> must be configured with GITHUB_HMAC authentication type. Type is <authentication>.
Fixed text: "Webhook ", " must be configured with GITHUB_HMAC authentication type. Type is ", "."; the variable parts are the request's webhookName and the stored webhook's `authentication` value. The recording also shows a dangling targetPipeline is not a refusal condition for this operation at all: PutWebhook accepts a webhook whose targetPipeline names no pipeline, ListWebhooks lists it with its ARN and definition, and the authentication-type refusal is still what DeregisterWebhookWithThirdParty returns.

Emulator: HTTP status (400) and error code (ValidationException) are right, but the message is an emulator-authored sentence reporting a different condition:
    Webhook <name> references a pipeline <targetPipeline> which does not exist
AWS produces no such message for this operation, and the condition it reports is a state AWS accepts. The authentication-type check is therefore missing or ordered after a pipeline-existence check that should not exist here.

Why: a client relying on AWS's behavior is told the wrong remedy — create the pipeline, when the requirement is that the webhook be configured with GITHUB_HMAC authentication. Clients (and the CLI/console flows) that branch on the refusal message or probe registrability take the wrong branch, and a webhook in a legitimate AWS-accepted state (dangling targetPipeline) is reported as broken.

Fix: the DeregisterWebhookWithThirdParty handler's validation in the CodePipeline webhook handler/state — resolve the webhook by name (WebhookNotFoundException when absent), then check the stored webhook's `authentication` FIRST and raise ValidationException with AWS's GITHUB_HMAC sentence; remove the target-pipeline-existence refusal from this operation.

Traces: codepipeline.webhook-targeting-absent-pipeline-lifecycle (every other step of that flow passes: PutWebhook echo, ListWebhooks ARN+definition, DeleteWebhook 200, repeat DeleteWebhook 200, WebhookNotFoundException on the deleted name).

## Evidence
Check: codepipeline.webhook-targeting-absent-pipeline-lifecycle

Setup (accepted by the emulator, as on AWS): PutWebhook with definition {name: <wh>, targetPipeline: <pipeline that does not exist>, targetAction: "Source", filters: [{jsonPath: "$.ref", matchEquals: "refs/heads/main"}], authentication: "UNAUTHENTICATED", authenticationConfiguration: {}}; the definition is echoed and the webhook is listed with its ARN.

Request: DeregisterWebhookWithThirdParty(webhookName=<wh>)

Expected (AWS): HTTP 400, Code "ValidationException", Message "Webhook <webhookName> must be configured with GITHUB_HMAC authentication type. Type is UNAUTHENTICATED." — fixed text as quoted, with webhookName from the request and the authentication value from the stored webhook.

Actual (emulator): HTTP 400, Code "ValidationException" (correct), Message "Webhook <webhookName> references a pipeline <targetPipeline> which does not exist".

Replay output:
  File "check_codepipeline_webhook_targeting_absent_pipeline_lifecycle.py", line 96, in run / line 22, in expect_error
    assert err.get("Message") == message
  AssertionError: expected message 'Webhook sim2real-behav-cp-wh-sibling-e41a469bc1f1 must be configured with GITHUB_HMAC authentication type. Type is UNAUTHENTICATED.', got 'Webhook sim2real-behav-cp-wh-sibling-e41a469bc1f1 references a pipeline sim2real-behav-cp-pl-absent-cd1f9cceea71 which does not exist'

The webhook and pipeline names are per-run generated values and are not part of the requirement; the requirement is the fixed sentence with the request's webhookName and the stored authentication type substituted.

## Steps to Reproduce
1. PutWebhook with webhook = {name: "wh1", targetPipeline: "no-such-pipeline", targetAction: "Source", filters: [{jsonPath: "$.ref", matchEquals: "refs/heads/main"}], authentication: "UNAUTHENTICATED", authenticationConfiguration: {}} — accepted, as on AWS.
2. Call DeregisterWebhookWithThirdParty(webhookName="wh1").
3. Expected: ValidationException, HTTP 400, message "Webhook wh1 must be configured with GITHUB_HMAC authentication type. Type is UNAUTHENTICATED.". Observed: ValidationException, HTTP 400, message "Webhook wh1 references a pipeline no-such-pipeline which does not exist".
4. Control (shows mis-ordering, not a merely extra check): create a pipeline named "no-such-pipeline" or re-point the webhook at an existing pipeline, then repeat step 2 — the authentication-type refusal must be raised for an UNAUTHENTICATED webhook regardless of whether the target pipeline exists.
5. Regression guard: DeregisterWebhookWithThirdParty on a deleted webhook name must still raise WebhookNotFoundException (already correct).
