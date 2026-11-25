Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge
Issue-Key: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge|_poll_get#47|Timeout|return last
Issue-Commit: ebe019cd5c5e58690a0a27ca877e231450bd6d5b

# cloudfront__events: the S3 Object Created → EventBridge → Lambda → CreateInvalidation edge-refresh chain reaches no verdict — the check exhausts its whole 60 s budget

> _Note: this issue was identified against the code base at commit `ebe019cd5c5e58690a0a27ca877e231450bd6d5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInvalidation

## Description
ONE net-new root cause for this service, grounded in one failing check.

Cause 1 of 1: the end-to-end "origin overwrite → event → Lambda → invalidation → fresh bytes at the edge" chain never advances, so the check never reaches any assertion and is killed by the suite cap.

AWS: recorded behavior only for the chain itself (the Smithy model describes the APIs, not propagation or delivery). The model states the observable states the check waits on: Smithy cloudfront#Distribution.Status — "The distribution's status. When the status is Deployed, the distribution's information is fully propagated to all CloudFront edge locations" (required member); cloudfront#Invalidation.Status — "When the invalidation batch is finished, the status is Completed" (required member); ListInvalidations returns InvalidationList with Quantity 0 and no Items while nothing has been invalidated. On AWS the recorded sequence is: a distribution over an OAC-protected private S3 origin becomes Deployed; CloudTrail aws.cloudfront "AWS API Call via CloudTrail" records reach an EventBridge rule's SQS target (the check's channel-liveness barrier, an UpdateOriginAccessControl record); a viewer GET of the distribution's domain returns 200 with the origin bytes and a second GET returns the same bytes with X-Cache starting "Hit"; overwriting the origin object does not change the cached bytes and creates no invalidation (Quantity 0); putting an object under the rule's trigger/ prefix delivers aws.s3 "Object Created" to the rule, which invokes the Lambda target, which calls cloudfront:CreateInvalidation, producing exactly one invalidation whose InvalidationBatch.CallerReference is the trigger object key and whose Paths.Items is the single configured path; that invalidation reaches Status Completed, after which the edge serves the new bytes; the CreateInvalidation call itself arrives as one event with source "aws.cloudfront", detail-type "AWS API Call via CloudTrail", detail.eventName "CreateInvalidation", detail.requestParameters.distributionId equal to the distribution id and detail.userIdentity.arn containing ":assumed-role/<lambda role name>/"; invalidation completion itself emits no event, so the only aws.cloudfront event about that invalidation is its API-call record.
Emulator: the check produces no verdict at all — replay reports only "Check timed out after 60.0s". It is blocked before any of the above assertions can be graded. Two blockers are already visible for this emulator in this same run and in open issues: (a) matched EventBridge events are not delivered to their targets, so the check's channel-liveness barrier (an aws.cloudfront CloudTrail API-call record arriving at the SQS sinks) never satisfies and the aws.s3 Object Created trigger never invokes the Lambda — the same component as open issue 20261024T182555Z-cloudfront-events-a-matched-eventbridge-rule-never-produces.md, which carries three other check ids and not this one; and (b) a deployed distribution's edge domain serves nothing for an OAC-protected S3 origin, so the first and second viewer GETs cannot return the origin bytes or an X-Cache hit — the same component as open issues 20261029T175823Z-cloudfront-s3-an-oac-protected-s3-origin-never.md and 20261118T131652Z-cloudfront-s3-the-invalidate-once-re-cache-sequence.md. Neither existing issue carries this check's replay key, so this scenario is not covered by them. Hypothesis: the hang is the first of those barriers that the check reaches (distribution Status/edge readiness, then the CloudTrail-to-SQS liveness barrier), not a slow-but-correct path.
Why: a client cannot build the canonical cache-invalidation-on-content-change workflow against this emulator. Nothing observable distinguishes "the event did not match", "the event matched but was never delivered", "the Lambda never ran", "the invalidation was never created" and "the edge never served" — all five present as silence until the caller's own deadline, and the asserted negative properties (an untriggered overwrite creates no invalidation; completion emits no event) can never be demonstrated because the positive half never happens.
Fix: the EventBridge rule-matching and target-delivery path in the eventbridge/cloudwatchevents service (PutEvents dispatch plus the Lambda target invoker, and the aws.s3 Object Created / CloudTrail aws.cloudfront API-call event sources), together with the CloudFront edge data plane that serves a deployed distribution over an OAC-protected S3 origin and applies an invalidation to its cache, and the CloudFront invalidation state machine that advances Invalidation.Status to Completed.
Traces: cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge.

## Evidence
Replay output, verbatim:
- [mismatch] cloudfront__events.xsvc-cloudfront-1-s3-object-created-lambda-invalidation-refreshes-edge — "Error: Check timed out after 60.0s". No assertion message was produced, so the check never reached a graded step.

Check contract (its own docstring): "S3 Object Created -> EventBridge -> Lambda -> CreateInvalidation: the edge serves stale bytes after an untriggered overwrite and the new bytes once the invalidation completes; the call is a CloudTrail event attributed to the Lambda's role, and completion emits no event."

The blocking waits, in the order the check reaches them (its own internal budgets are capped by the harness's remaining budget):
1. poll get_distribution(Id=<dist>)["Distribution"]["Status"] until "Deployed".
2. channel-liveness barrier: an aws.cloudfront "AWS API Call via CloudTrail" record for UpdateOriginAccessControl must reach both SQS sinks fed by EventBridge rules (failure text would be "CHANNEL NOT LIVE (not a routing verdict)").
3. viewer GET of https://<distribution domain>/page.txt must answer (200, <origin bytes>), and the second GET must answer the same bytes with X-Cache starting "Hit".
4. list_invalidations(DistributionId=<dist>) must report Quantity 0 after a non-trigger overwrite.
5. after put_object under trigger/, list_invalidations must show exactly one invalidation with InvalidationBatch.CallerReference == <trigger key> and Paths.Items == ["/page.txt"]; get_invalidation Status must reach "Completed"; the edge must then serve the new bytes.
6. the CreateInvalidation API-call event must arrive with (source, detail-type, detail.eventName) == ("aws.cloudfront", "AWS API Call via CloudTrail", "CreateInvalidation"), detail.requestParameters.distributionId == <dist id>, and detail.userIdentity.arn containing ":assumed-role/<role name>/".
None of these were observed; the check was cut off with no verdict.

Model grounding: Smithy cloudfront#Distribution.Status (required) — "When the status is Deployed, the distribution's information is fully propagated to all CloudFront edge locations"; cloudfront#Invalidation.Status (required) — "When the invalidation batch is finished, the status is Completed"; cloudfront#Invalidation.InvalidationBatch (required) carries CallerReference and Paths.

Related open issues that describe the same components but do NOT carry this check's replay key: 20261024T182555Z-cloudfront-events-a-matched-eventbridge-rule-never-produces.md, 20261029T175823Z-cloudfront-s3-an-oac-protected-s3-origin-never.md, 20261118T131652Z-cloudfront-s3-the-invalidate-once-re-cache-sequence.md.

## Steps to Reproduce
1. Create a private S3 bucket and enable EventBridge notifications on it (put_bucket_notification_configuration with EventBridgeConfiguration {}). Put an object at key page.txt with the v1 bytes.
2. Create an origin access control (s3 / always / sigv4) and a distribution whose single origin is that bucket with the OAC attached, default cache behavior targeting it with the AWS-managed caching-optimized cache policy; grant cloudfront.amazonaws.com s3:GetObject on the bucket conditioned on AWS:SourceArn == the distribution ARN.
3. Create an IAM role for Lambda with cloudfront:CreateInvalidation on the distribution ARN and a Lambda whose handler calls create_invalidation with CallerReference = the event's detail.object.key and Paths.Items = ["/page.txt"].
4. Create an EventBridge rule on {"source":["aws.s3"],"detail-type":["Object Created"],"detail":{"bucket":{"name":[<bucket>]},"object":{"key":[{"prefix":"trigger/"}]}}} with the Lambda as target, and add the lambda:InvokeFunction permission for events.amazonaws.com with the rule ARN as SourceArn.
5. Create two SQS-backed sinks via EventBridge rules: one on aws.cloudfront "AWS API Call via CloudTrail" with detail.eventName CreateInvalidation, and one on every aws.cloudfront event. Start a CloudTrail trail.
6. Wait for get_distribution Status == "Deployed"; prove the CloudTrail→EventBridge→SQS channel live with an UpdateOriginAccessControl call; GET the distribution domain's /page.txt twice (expect 200 + v1 bytes, then an X-Cache "Hit").
7. Overwrite page.txt with v2 bytes (no trigger prefix): the edge must still serve v1 and list_invalidations must report Quantity 0.
8. Put an object under trigger/: exactly one invalidation must appear naming that key and ["/page.txt"], reach Status "Completed", after which the edge serves the v2 bytes; the CreateInvalidation API call must arrive as one aws.cloudfront CloudTrail event attributed to the Lambda's assumed role, and completion must emit no event.
Against the emulator, the sequence never advances past step 6 and the check is killed at the 60 s cap with no verdict.
Or replay the check id in trace_ids.
