Issue-Severity: MEDIUM
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# codepipeline: DeregisterWebhookWithThirdParty validates the webhook's target pipeline before its authentication type, so an UNAUTHENTICATED webhook is refused with a "references a pipeline ... which does not exist" message instead of AWS's GITHUB_HMAC authentication-type refusal

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterWebhookWithThirdParty

## Description
Cause 1 of 1: DeregisterWebhookWithThirdParty performs its checks in the wrong order — it resolves the webhook's targetPipeline first and refuses on that, so the authentication-type refusal AWS owes is never reached. Failing check: codepipeline.webhook-targeting-absent-pipeline-lifecycle.

AWS: Smithy: DeregisterWebhookWithThirdParty(webhookName) declares exactly two errors, ValidationException and WebhookNotFoundException (both client, HTTP 400). Recorded behavior: third-party deregistration is only meaningful for a webhook whose authentication is GITHUB_HMAC, so AWS checks the authentication type of the named webhook first and refuses anything else with ValidationException and the message
    Webhook <webhookName> must be configured with GITHUB_HMAC authentication type. Type is <authentication>.
Fixed text: "Webhook ", " must be configured with GITHUB_HMAC authentication type. Type is ", "." — the two variable parts are the request's webhookName and the stored webhook's authentication value (UNAUTHENTICATED here). This refusal is reached even when the webhook's targetPipeline names no pipeline: PutWebhook accepts a webhook whose targetPipeline does not exist (the check's earlier steps confirm the emulator accepts it too, lists it with its ARN and echoes its definition), so a dangling targetPipeline is not a refusal condition for this operation at all.

Emulator: the HTTP status and the error code are right (ValidationException) but the message is a different, emulator-authored refusal on a different condition:
    Webhook <name> references a pipeline <pipeline> which does not exist
That sentence is not a message AWS produces for this operation, and the condition it reports — the dangling target pipeline — is not a condition AWS refuses on here. The authentication-type check is therefore either absent or ordered after the pipeline-existence check.

Why: a client cannot act on the refusal. The operator is told to create a pipeline, when the actual requirement is that the webhook be re-created with GITHUB_HMAC authentication; and a client that branches on the message (or on the presence of the authentication-type sentence) to decide whether a webhook is third-party-registrable takes the wrong branch. A dangling target pipeline is a legitimate, AWS-accepted state for a webhook, so refusing on it leaks a non-condition.

Fix: DeregisterWebhookWithThirdParty's validation in the CodePipeline webhook handler — after resolving the webhook by name (WebhookNotFoundException when absent), check the stored webhook's authentication FIRST and raise ValidationException with AWS's GITHUB_HMAC sentence; drop the target-pipeline-existence refusal from this operation entirely.

Traces: codepipeline.webhook-targeting-absent-pipeline-lifecycle. The check's earlier steps all pass (PutWebhook accepts a dangling targetPipeline and echoes the definition, ListWebhooks lists it with its ARN, DeleteWebhook answers 200, a repeat DeleteWebhook answers 200, and the deleted name is refused WebhookNotFoundException), so this is the sole surviving divergence in that flow. No open codepipeline webhook issue covers it: 20261111T061123Z-codepipeline-publishes-webhook-urls-on-a-webhooks-aws.md is about the published webhook URL host.

## Evidence
Check: codepipeline.webhook-targeting-absent-pipeline-lifecycle

Setup (all accepted, matching AWS): PutWebhook with a definition whose targetPipeline names a pipeline that does not exist, targetAction "Source", one filter, authentication "UNAUTHENTICATED", empty authenticationConfiguration. The webhook is created, listed with its ARN and its definition echoed verbatim.

Request: DeregisterWebhookWithThirdParty(webhookName=<that webhook's name>)

Expected (AWS): HTTP 400, Code "ValidationException", Message
  "Webhook <webhookName> must be configured with GITHUB_HMAC authentication type. Type is UNAUTHENTICATED."

Actual (emulator): HTTP 400, Code "ValidationException" (correct), Message
  "Webhook <webhookName> references a pipeline <targetPipeline> which does not exist"

Replay output:
  File "check_codepipeline_webhook_targeting_absent_pipeline_lifecycle.py", line 22, in expect_error
    assert err.get("Message") == message
  AssertionError: expected message 'Webhook sim2real-behav-cp-wh-sibling-ccc6ac71e20c must be configured with GITHUB_HMAC authentication type. Type is UNAUTHENTICATED.', got 'Webhook sim2real-behav-cp-wh-sibling-ccc6ac71e20c references a pipeline sim2real-behav-cp-pl-absent-e13b21fda2a7 which does not exist'

The webhook and pipeline names shown are per-run generated values; the requirement is the fixed sentence with the request's webhookName and the stored webhook's authentication value substituted in.

## Steps to Reproduce
1. PutWebhook with webhook = {name: "wh1", targetPipeline: "no-such-pipeline", targetAction: "Source", filters: [{jsonPath: "$.ref", matchEquals: "refs/heads/main"}], authentication: "UNAUTHENTICATED", authenticationConfiguration: {}}. It is accepted (as on AWS) even though no pipeline named "no-such-pipeline" exists.
2. Call DeregisterWebhookWithThirdParty(webhookName="wh1").
3. Observe the refusal:
   - Expected: ValidationException, HTTP 400, message "Webhook wh1 must be configured with GITHUB_HMAC authentication type. Type is UNAUTHENTICATED."
   - Observed: ValidationException, HTTP 400, message "Webhook wh1 references a pipeline no-such-pipeline which does not exist".
4. Control: create a pipeline named "no-such-pipeline" (or point the webhook at an existing pipeline) and repeat step 2 — the authentication-type refusal must be raised for an UNAUTHENTICATED webhook regardless of whether the target pipeline exists, which shows the two checks are mis-ordered rather than the pipeline check being merely extra.
5. Call DeregisterWebhookWithThirdParty on a webhook name that was deleted: WebhookNotFoundException must still be raised (this part already matches).
