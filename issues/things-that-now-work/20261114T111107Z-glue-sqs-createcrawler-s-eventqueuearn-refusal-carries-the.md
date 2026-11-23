Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__sqs.glue-crawler-missing-sqs-receive-permission-rejected
Issue-Key: glue__sqs.glue-crawler-missing-sqs-receive-permission-rejected|run#19|AssertionError|assert create_message == expected_message, f"expected message {expected_message!r}, got {create_message!r}"
Issue-Commit: 54cc30c6fad79cfe6c1612c15a6c83a97395199b

# glue__sqs: CreateCrawler's EventQueueArn refusal carries the IAM identity-policy denial sentence instead of Glue's 'SQS queue <arn> does not exist or the role provided does not have access to it.'

> _Note: this issue was identified against the code base at commit `54cc30c6fad79cfe6c1612c15a6c83a97395199b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCrawler

## Description
Cause 1 of 1: the InvalidInputException Glue raises when the crawler role cannot read the S3 target's EventQueueArn is worded as a generic IAM identity-policy denial instead of Glue's own fixed sentence.

AWS: Glue validates the role's access to Targets.S3Targets[].EventQueueArn at CreateCrawler time (not lazily at crawl time) and refuses the create with InvalidInputException. Smithy: CreateCrawler errors = AlreadyExistsException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException; InvalidInputException is errorKind=client, HTTP 400 (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json). The model does not carry the message text, so the message is recorded behavior only: Glue answers ONE sentence whose fixed text is "SQS queue " + " does not exist or the role provided does not have access to it." and whose only variable part is the EventQueueArn copied verbatim from the request. The sentence deliberately conflates 'absent' and 'unreadable' and never names the acting principal, the SQS action, or the reason class.

Emulator: the status and error code are right (400 / InvalidInputException) but the Message is the identity-policy denial template of the underlying SQS authorization check, relaying the acting role and the SQS action to the Glue caller: "User: <crawler role ARN> is not authorized to perform: SQS:ReceiveMessage on resource: <queue ARN> because no identity-based policy allows the SQS:ReceiveMessage action". Fixed text in the emulator's string is "User: ", " is not authorized to perform: SQS:ReceiveMessage on resource: ", " because no identity-based policy allows the SQS:ReceiveMessage action"; the role ARN and queue ARN are request/state values. Neither the fixed text nor the shape of Glue's sentence survives.

Why: a client (or a log/alert rule, or a CDK/Terraform error matcher) keyed on Glue's documented wording never matches, so a crawler misconfiguration is reported as an unrelated SQS/IAM problem. It also leaks the principal and the internal action name into a Glue control-plane refusal that AWS never discloses them in, which is a different contract than the one callers are coded against.

Fix: the CreateCrawler handler's EventQueueArn validation must raise Glue's InvalidInputException with Glue's own message template built from the request's EventQueueArn, rather than propagating the message of the SQS/IAM authorization decision it performed internally. (The nearest open issues — 20261111T052620Z-emr-glue-s3-3-net-new-causes-the.md, which covers CreateCrawler accepting an S3 target in a nonexistent bucket, and 20261113T104338Z-glue-sqs-2-net-new-causes-an-event.md — do not cover this refusal's wording and do not carry this check's replay key.)

Traces: glue__sqs.glue-crawler-missing-sqs-receive-permission-rejected — the check asserts code == 'InvalidInputException' (passes) then the exact message (fails).

## Evidence
Failing check: [mismatch] glue__sqs.glue-crawler-missing-sqs-receive-permission-rejected

AssertionError raised at check_glue__sqs_glue_crawler_missing_sqs_receive_permission_rejected.py:54:
  expected message 'SQS queue arn:aws:sqs:us-west-2:<acct>:<bad-queue-name> does not exist or the role provided does not have access to it.'
  got            'User: arn:aws:iam::<acct>:role/<crawler-role-name> is not authorized to perform: SQS:ReceiveMessage on resource: arn:aws:sqs:us-west-2:<acct>:<bad-queue-name> because no identity-based policy allows the SQS:ReceiveMessage action'

(The account id, queue name and role name above are per-run values from the recording; only the fixed text and which parts are request-derived are the requirement.)

Error code and HTTP status agree with AWS (InvalidInputException / 400) — the divergence is the Message only.

## Steps to Reproduce
1. Create an S3 bucket with an object under a prefix, and an SQS queue Q.
2. Create an IAM role trusted by glue.amazonaws.com whose inline policy grants S3 access to the bucket but NO sqs:ReceiveMessage on Q.
3. Create a Glue database.
4. Call CreateCrawler with Role=<that role>, DatabaseName=<db>, Targets={"S3Targets":[{"Path":"s3://<bucket>/bad/","EventQueueArn":"<arn of Q>"}]}, RecrawlPolicy={"RecrawlBehavior":"CRAWL_EVENT_MODE"}.
5. Observe the error: code is InvalidInputException (correct) but Message is the IAM identity-policy denial sentence instead of "SQS queue <arn of Q> does not exist or the role provided does not have access to it."
