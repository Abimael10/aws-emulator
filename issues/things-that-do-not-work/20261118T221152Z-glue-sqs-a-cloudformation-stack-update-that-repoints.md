Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__sqs.deploy-stack-update-changes-sqs-queue-arn-positive
Issue-Key: glue__sqs.deploy-stack-update-changes-sqs-queue-arn-positive|_poll_until#31|Timeout|time.sleep(interval_s)
Issue-Commit: df2850aa9d064c019fbc411bddb81c97e6aaa2a5

# glue__sqs: a CloudFormation stack update that repoints the S3 notification and the crawler's EventQueueArn at a second queue never settles — the event-mode rewire check reaches no verdict

> _Note: this issue was identified against the code base at commit `df2850aa9d064c019fbc411bddb81c97e6aaa2a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
One net-new root cause in the glue__sqs CloudFormation-deployed, event-mode crawler path. This failing check is not marked ALREADY FILED and is not marked OWNED by any open issue in this replay, so it is filed here.

Cause 1 of 1: a CloudFormation stack UPDATE that repoints both an S3 bucket's NotificationConfiguration and an AWS::Glue::Crawler S3Target EventQueueArn from one SQS queue to a second one does not rewire the event-mode crawl, and the flow never advances to any verdict.

AWS: Smithy (glue) — S3Target{Path, Exclusions, ConnectionName, SampleSize, EventQueueArn, DlqEventQueueArn} where EventQueueArn is documented as 'A valid Amazon SQS ARN'; RecrawlPolicy.RecrawlBehavior is an enum whose CRAWL_EVENT_MODE value 'specifies crawling only the changes identified by Amazon S3 events'; Crawler.State is the enum READY|RUNNING|STOPPING and LastCrawl.Status is the enum SUCCEEDED|CANCELLED|FAILED. Smithy (cloudformation) — UpdateStack drives the stack through UPDATE_COMPLETE. Beyond those shapes this is recorded behavior only: on AWS the whole flow settles inside its budget — CreateStack reaches CREATE_COMPLETE, the first full crawl drives the crawler back to State READY with LastCrawl.Status SUCCEEDED and one partition per crawled prefix, each S3 ObjectCreated notification lands on whichever queue the bucket notification currently points at and is consumed (and deleted) by the next event-mode crawl so GetQueueAttributes ApproximateNumberOfMessages returns to 0, UpdateStack reaches UPDATE_COMPLETE, and after the update a new object's event is delivered ONLY to the second queue while the first queue's ApproximateNumberOfMessages stays 0. Every assertion in the flow is therefore evaluated and the check reaches a pass/fail decision.

Emulator: the check produces no AWS error, no non-2xx and no assertion failure — it makes no progress and is cut off at the harness cap with 'Error: Check timed out after 60.0s'. Because nothing was ever refused, the replay output does not pin which poll stalls; the flow polls, in order, CFN DescribeStacks for CREATE_COMPLETE, glue GetCrawler for State READY after StartCrawler, SQS GetQueueAttributes ApproximateNumberOfMessages on queue 1, CFN DescribeStacks for UPDATE_COMPLETE after UpdateStack, and SQS GetQueueAttributes on queue 2.

Why: a client that redirects an S3 -> SQS -> Glue crawler pipeline onto a new queue by updating its CloudFormation stack cannot tell from this emulator whether the rewire took effect at all. Worst case the emulator silently keeps publishing to, and crawling from, the retired queue, so post-update objects are never catalogued (GetPartitions keeps answering the pre-update partition count) while the retired queue accumulates messages forever. Because the flow never settles, none of that is observable as an error — it just hangs, which is the one outcome a caller cannot program against.

Fix: the CloudFormation stack-UPDATE path for AWS::S3::Bucket NotificationConfiguration and AWS::Glue::Crawler (its S3Targets[].EventQueueArn) must apply the new queue ARNs to live state and settle the stack at UPDATE_COMPLETE; and the glue crawler run executor's event-mode path must then receive-and-delete from the newly configured EventQueueArn and return the crawler to State READY with LastCrawl.Status SUCCEEDED. The open issue 20261113T104338Z-glue-sqs-2-net-new-causes-an-event.md describes a related inert event-mode crawl and its Cause 2 fix pointer explicitly defers 'the stack UPDATE that repoints S3 NotificationConfiguration and the crawler's EventQueueArn' to later; this replay did not mark the check as owned by it, so that recorded unit is not re-replayed by that issue and the rewire cause is recorded here.

Traces: glue__sqs.deploy-stack-update-changes-sqs-queue-arn-positive.

## Evidence
Replay output (behavioral suite, section ### glue__sqs):

  [mismatch] glue__sqs.deploy-stack-update-changes-sqs-queue-arn-positive
    Error: Check timed out after 60.0s

Expected (AWS):
  - CreateStack -> DescribeStacks StackStatus CREATE_COMPLETE.
  - StartCrawler -> GetCrawler Crawler.State 'READY' with Crawler.LastCrawl.Status 'SUCCEEDED' (LastCrawlStatus enum: SUCCEEDED|CANCELLED|FAILED); GetPartitions returns 1 partition after the first full crawl.
  - A second object's S3 ObjectCreated event appears on queue 1 (GetQueueAttributes ApproximateNumberOfMessages >= 1), the next CRAWL_EVENT_MODE crawl consumes it (partition count 2) and drains it (ApproximateNumberOfMessages back to '0').
  - UpdateStack with the template whose bucket NotificationConfiguration and crawler S3Target.EventQueueArn both point at queue 2 -> DescribeStacks StackStatus UPDATE_COMPLETE.
  - A post-update object's event appears ONLY on queue 2 (ApproximateNumberOfMessages >= 1) while queue 1 stays at '0'; the next crawl raises the partition count to 3 and drains queue 2.

Actual (emulator):
  - No verdict at all. No API call returned an error code or a non-2xx status; the check made no progress and was cut at the harness cap after 60.0s, so not one of the assertions above was ever evaluated.
  - The timeout value (60.0s) and the harness cap are per-run properties of the runner, not requirements; the requirement being violated is that the flow settles and the post-update event is delivered to, and crawled from, the queue the updated stack names.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (cloudformation, glue, s3, iam and sqs all serve).
2. CreateStack with a template containing: two AWS::SQS::Queue resources (Queue1, Queue2) plus an AWS::SQS::QueuePolicy for each allowing s3.amazonaws.com sqs:SendMessage on the queue ARN; an AWS::S3::Bucket whose NotificationConfiguration has one QueueConfiguration (Event 's3:ObjectCreated:*', Queue = Queue1's Arn); an AWS::IAM::Role trusted by glue.amazonaws.com with SQS receive/delete/get-attributes and S3 read on the bucket; an AWS::Glue::Database; and an AWS::Glue::Crawler whose Targets.S3Targets[0] has Path = s3://<bucket>/data/ and EventQueueArn = Queue1's Arn, with RecrawlPolicy.RecrawlBehavior = CRAWL_EVENT_MODE. Outputs expose both queue URLs. Wait for CREATE_COMPLETE.
3. PutObject data/dt=2023-01-01/file1.csv, StartCrawler, poll GetCrawler until Crawler.State == 'READY'; assert Crawler.LastCrawl.Status == 'SUCCEEDED' and GetPartitions returns 1 partition.
4. PutObject data/dt=2023-01-02/file2.csv; poll SQS GetQueueAttributes ApproximateNumberOfMessages on Queue1 until >= 1; StartCrawler, wait READY; assert 2 partitions; poll Queue1 until ApproximateNumberOfMessages == 0.
5. UpdateStack with the same template except that BOTH the bucket NotificationConfiguration Queue and the crawler's S3Target EventQueueArn resolve to Queue2's Arn; wait for UPDATE_COMPLETE.
6. PutObject data/dt=2023-01-03/file3.csv; poll GetQueueAttributes on Queue2 until ApproximateNumberOfMessages >= 1 and assert Queue1 is still 0.
7. StartCrawler, wait READY, assert LastCrawl.Status 'SUCCEEDED' and 3 partitions; poll Queue2 to 0 and assert Queue1 is still 0.
   Expected: every step settles and the assertions are evaluated. Observed: the flow never advances to a verdict and is cut off at the harness cap with no error returned by any call.
8. Or re-replay: glue__sqs.deploy-stack-update-changes-sqs-queue-arn-positive.
