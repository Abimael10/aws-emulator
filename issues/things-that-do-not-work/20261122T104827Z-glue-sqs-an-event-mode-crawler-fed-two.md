Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__sqs.glue-crawler-event-queue-batch-of-multiple-paths-positive
Issue-Key: glue__sqs.glue-crawler-event-queue-batch-of-multiple-paths-positive|_wait_for_queue_drained#29|Timeout|time.sleep(interval_s)
Issue-Commit: eb235f107f11a58ccabfb1e09a1e98cf48d2a7c2

# glue__sqs: an event-mode crawler fed two queued S3 events for two paths never reaches a verdict — the check is cut off at the harness cap

> _Note: this issue was identified against the code base at commit `eb235f107f11a58ccabfb1e09a1e98cf48d2a7c2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartCrawler

## Description
One net-new root cause reached through the glue__sqs event-mode crawler path. The failing check is not marked ALREADY FILED and the replay did not attribute it to any open issue (it is not marked OWNED).

Cause 1 of 1: a CRAWL_EVENT_MODE crawler whose S3 target carries an EventQueueArn makes no progress when two S3 ObjectCreated notifications for two distinct child prefixes are queued ahead of one incremental crawl — the flow never settles, so no assertion is ever evaluated.
  AWS:      Smithy (glue): CreateCrawler/UpdateCrawler -> CrawlerTargets.S3Targets[].{Path, EventQueueArn, DlqEventQueueArn} and RecrawlPolicy.RecrawlBehavior with the CRAWL_EVENT_MODE value; GetCrawler reports Crawler.State and Crawler.LastCrawl.Status; GetPartitions refuses an absent table with EntityNotFoundException (errorKind client, httpError 400). The crawl semantics themselves are recorded behavior only: on AWS the first run is a full listing crawl that publishes one table per child prefix of the include path; StartCrawler then returns and the crawler leaves RUNNING for State 'READY' with LastCrawl.Status 'SUCCEEDED' well inside the budget; the incremental (event-mode) run RECEIVES the queued S3 notifications and DELETES each message it consumed, so SQS GetQueueAttributes reports ApproximateNumberOfMessages 0 and ApproximateNumberOfMessagesNotVisible 0 afterwards; and each consumed path is reflected as exactly one new partition on the table covering that path — two messages for two child prefixes yield exactly one new partition on each of the two tables. Every one of those steps therefore reaches a pass/fail decision.
  Emulator: the run produces no verdict at all. The replay reports only 'Error: Check timed out after 60.0s' — no AWS error code, no non-2xx, no assertion failure; the flow simply does not advance to any of the observations above within the harness cap, even though the check's own waits budget 600 s for the crawler to reach READY and 60 s for the queue to drain. Because the only evidence is the timeout, the blocking step is not pinned by the replay output; the surface it blocks on is the same event-mode crawl + SQS consumption that open issue 20261113T104338Z-glue-sqs-2-net-new-causes-an-event.md shows inert (its Cause 1 records this very check previously failing EARLIER, with GetPartitions refused EntityNotFoundException 'Table tablea not found.'), so the crawl's event-queue consumer and its per-child-prefix table/partition publication are the first places to look. That issue's recorded replay key for this check no longer matches (the failure moved from a GetPartitions refusal to a no-verdict timeout), so this recorded unit is not re-replayed by it and is filed here.
  Why:      the whole S3-event-driven incremental crawl pattern is unverifiable and, as far as a client can tell, inert. A client wiring S3 -> SQS -> Glue crawler with RecrawlBehavior=CRAWL_EVENT_MODE gets neither the new partitions it depends on nor a drained event queue, and gets no refusal to debug: the crawler reports healthy while downstream consumers re-read the same notifications forever and queries over the newly written paths see no new partition.
  Fix:      the crawler run executor in the glue service — the event-mode crawl path must terminate (State back to READY with LastCrawl.Status SUCCEEDED), must receive-and-delete from the configured EventQueueArn, and must add one partition per consumed path to the table covering that path, so that each of those observations answers instead of hanging.
  Traces:   check glue__sqs.glue-crawler-event-queue-batch-of-multiple-paths-positive.

## Evidence
Replay output (behavioral suite, section ### glue__sqs):

[mismatch] glue__sqs.glue-crawler-event-queue-batch-of-multiple-paths-positive
  Error: Check timed out after 60.0s

Expected (AWS), from the check's assertions in order:
  1. after the first (full) crawl, GetCrawler reports State 'READY' and LastCrawl.Status 'SUCCEEDED';
  2. GetPartitions answers 200 with exactly 1 partition for each of the two tables covering the two child prefixes;
  3. after two S3 ObjectCreated messages are queued and one incremental crawl runs, GetCrawler again reports READY / SUCCEEDED;
  4. SQS GetQueueAttributes on the event queue reports ApproximateNumberOfMessages 0 and ApproximateNumberOfMessagesNotVisible 0 (the crawler deleted what it consumed);
  5. GetPartitions answers 200 with exactly 2 partitions for each table.

Actual (emulator): none of 1..5 is ever evaluated — the run is cut at the harness cap with no error returned by any API call and no assertion verdict. No status code, error code or message diverges here; what diverges is that the flow never settles.

Related but NOT covering this check: open issue 20261113T104338Z-glue-sqs-2-net-new-causes-an-event.md lists this check id in its trace ids, but its recorded key for it is 'glue|GetPartitions|EntityNotFoundException' — the failure has since moved to a no-verdict timeout, so the replay did not mark this check OWNED and it is not re-replayed by that issue.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (glue, s3, sqs, iam and sts all serve).
2. Create an S3 bucket and put two objects with different schemas under two child prefixes of one include path: data/tablea/dt=2024-01-01/data.csv and data/tableb/dt=2024-01-01/data.csv.
3. Create an SQS event queue; create an IAM role trusted by glue.amazonaws.com and allowed s3:GetObject/PutObject/ListBucket on the bucket and sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes (etc.) on that queue; create a Glue database.
4. glue CreateCrawler with Targets={'S3Targets':[{'Path':'s3://<bucket>/data/','EventQueueArn':<queue arn>}]}, RecrawlPolicy={'RecrawlBehavior':'CRAWL_EVENT_MODE'}, SchemaChangePolicy={'UpdateBehavior':'UPDATE_IN_DATABASE','DeleteBehavior':'LOG'}.
5. glue StartCrawler; poll GetCrawler until State 'READY' and assert LastCrawl.Status 'SUCCEEDED'; GetPartitions for each of the two tables and expect 1 partition each.
6. Put one new object under each child prefix (dt=2024-01-02) and SendMessage the corresponding S3 ObjectCreated notification for each onto the event queue BEFORE starting the crawl.
7. glue StartCrawler again; poll GetCrawler to READY/SUCCEEDED; then SQS GetQueueAttributes on the event queue expecting ApproximateNumberOfMessages 0 and ApproximateNumberOfMessagesNotVisible 0; then GetPartitions expecting 2 partitions per table.
8. Observe that the flow never reaches those observations — the check is cut off with no verdict and no API refusal.
9. Or re-replay: glue__sqs.glue-crawler-event-queue-batch-of-multiple-paths-positive.
