Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__sqs.deploy-sqs-queue-policy-missing-glue-principal-negative
Issue-Key: glue__sqs.deploy-sqs-queue-policy-missing-glue-principal-negative|run#11|AssertionError|assert "SQS:ReceiveMessage" in reasons, f"expected the rejection to name the missing action: {reasons}"
Issue-Commit: 225b72df8882575de41dd997e8ca538d39a59320

# glue__sqs: the CloudFormation-surfaced CreateCrawler refusal for an EventQueueArn the crawler role cannot consume does not name the missing SQS action

> _Note: this issue was identified against the code base at commit `225b72df8882575de41dd997e8ca538d39a59320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCrawler

## Description
Cause 1 of 1: when an event-mode AWS::Glue::Crawler is deployed with a role that holds no sqs:ReceiveMessage / sqs:DeleteMessage on the target's EventQueueArn, the InvalidInputException Glue raises — and that CloudFormation surfaces as the Crawler resource's CREATE_FAILED reason — does not name the SQS action the role is missing.

AWS: Glue validates the role's access to Targets.S3Targets[].EventQueueArn at CreateCrawler time (not lazily at the first crawl) and refuses the create. Smithy: CreateCrawler errors = AlreadyExistsException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException; InvalidInputException is errorKind=client, HTTP 400 (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json). The model carries no message text, so the message is recorded behavior only: the recording shows the reason CloudFormation relays for this resource naming the missing action literally as `SQS:ReceiveMessage`, with HandlerErrorCode `InvalidRequest` and the failing resource reported as `Crawler(CREATE_FAILED)`. The queue-ARN-shaped part of any such sentence is a request value (the EventQueueArn), the action name is fixed text.

Emulator: status and error class are right — the Crawler resource fails with `HandlerErrorCode: InvalidRequest` and the stack rolls back — but the relayed Glue message is only the generic existence/access sentence whose fixed text is "SQS queue " + " does not exist or the role provided does not have access to it." with the EventQueueArn as its one variable part. No SQS action appears anywhere in the reason, so the check's assertion that the rejection names the missing action fails.

Hypothesis: this cause and the open issue 20261114T111107Z-glue-sqs-createcrawler-s-eventqueuearn-refusal-carries-the.md are two recordings of the SAME refusal and must be reconciled rather than fixed in opposition — that issue requires the "SQS queue <arn> does not exist or the role provided does not have access to it." sentence (and the emulator now emits exactly it, which is why this check regressed into the opposite failure), while this recording requires the reason to name `SQS:ReceiveMessage`. The reading that satisfies both recordings is that AWS's message is ONE longer string that both conflates absent/unreadable AND states the SQS action(s) the role must be granted, and the emulator is currently emitting only the first clause. A fixer must close both keys together; neither recording's text may be dropped to satisfy the other.

Why: a client, a CDK/Terraform error matcher, or a log/alert rule keyed on the action named in Glue's refusal cannot tell "the queue does not exist" from "your crawler role is missing sqs:ReceiveMessage" — the two have completely different remediations, and the one actionable detail (which permission to grant) is absent from the only signal the deploy produces.

Fix: the CreateCrawler handler's EventQueueArn validation and the InvalidInputException message it builds, in the glue service crate's create_crawler op — the message must carry the SQS action the role lacks alongside the existence/access clause, built from the request's EventQueueArn; the CloudFormation AWS::Glue::Crawler resource handler already relays the Glue message verbatim and needs no change.

Traces: glue__sqs.deploy-sqs-queue-policy-missing-glue-principal-negative.

## Evidence
Failing check: [mismatch] glue__sqs.deploy-sqs-queue-policy-missing-glue-principal-negative (per-service pass rate glue__sqs 40/52, this failure neither ALREADY FILED nor OWNED).

AssertionError at check_glue__sqs_deploy_sqs_queue_policy_missing_glue_principal_negative.py:41:
  assert "SQS:ReceiveMessage" in reasons, f"expected the rejection to name the missing action: {reasons}"
  expected the rejection to name the missing action: Crawler(CREATE_FAILED): Resource handler returned message: "SQS queue arn:aws:sqs:us-west-2:<acct>:<queue-name> does not exist or the role provided does not have access to it." (RequestToken: <per-run>, HandlerErrorCode: InvalidRequest)

The assertions that pass first: the stack reaches ROLLBACK_COMPLETE, the failing resource is `Crawler(CREATE_FAILED)`, and `HandlerErrorCode: InvalidRequest` is present. The divergence is the Message only.

Expected (AWS, recorded): the CREATE_FAILED reason names the missing action `SQS:ReceiveMessage`.
Actual (emulator): the reason carries only "SQS queue <EventQueueArn> does not exist or the role provided does not have access to it." — no action named.

(The account id, Region and queue/crawler names above are per-run values from the recording; the requirement is the fixed text and which parts are request-derived.)

## Steps to Reproduce
1. Build and boot the emulator; replay glue__sqs.deploy-sqs-queue-policy-missing-glue-principal-negative from simparity-behavioral-aws-v1 (report heading `glue__sqs`).
2. Or directly: CreateStack with a template containing (a) an AWS::SQS::Queue, (b) an AWS::S3::Bucket, (c) an AWS::IAM::Role trusted by glue.amazonaws.com with AWSGlueServiceRole plus an inline policy granting s3:GetObject/s3:ListBucket on the bucket and sqs:SetQueueAttributes/sqs:GetQueueUrl/sqs:PurgeQueue (deliberately NOT sqs:ReceiveMessage or sqs:DeleteMessage) on the queue, (d) an AWS::Glue::Database, and (e) an AWS::Glue::Crawler with Role=<the role ARN>, Targets.S3Targets=[{Path: s3://<bucket>/data/, EventQueueArn: <queue ARN>}], RecrawlPolicy.RecrawlBehavior=CRAWL_EVENT_MODE.
3. Wait for the stack to settle and read DescribeStackEvents.
   Expected: the Crawler resource is CREATE_FAILED with HandlerErrorCode InvalidRequest and a reason that names the missing action SQS:ReceiveMessage.
   Actual: the reason is only "SQS queue <arn> does not exist or the role provided does not have access to it.".
4. Confirm the control half still holds: with sqs:ReceiveMessage/sqs:DeleteMessage granted, the identical CreateCrawler call succeeds and GetCrawler reports the EventQueueArn and CRAWL_EVENT_MODE.
