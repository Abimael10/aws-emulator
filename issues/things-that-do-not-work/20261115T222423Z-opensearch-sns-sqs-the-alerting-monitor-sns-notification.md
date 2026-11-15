Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch__sns__sqs.alerting-monitor-trigger-publishes-the-rendered-count-to-sns-and-withholds-it-when-false
Issue-Key: opensearch__sns__sqs.alerting-monitor-trigger-publishes-the-rendered-count-to-sns-and-withholds-it-when-false|_drain#26|Timeout|received = sqs.receive_message(
Issue-Commit: 0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764

# opensearch__sns__sqs: the alerting-monitor → SNS notification-channel delivery scenario never reaches a verdict — the check makes no observable progress and is cut off at the 60 s harness cap

> _Note: this issue was identified against the code base at commit `0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
Cause 1 of 1: the OpenSearch alerting monitor / notifications-channel path produces no observable progress, so the SNS fan-out contract cannot be graded at all.
AWS: recorded behavior only (the alerting and notifications plugins are domain REST APIs, not modeled in the Smithy service). On a managed domain the recorded sequence is: PUT /<index> and a refreshed _bulk load answer 200 with errors false; POST /_plugins/_notifications/configs with {config_id, config: {config_type: "sns", is_enabled: true, sns: {topic_arn, role_arn}}} answers 200/201 and GET of that config echoes the topic ARN; POST /_plugins/_alerting/monitors with a disabled query_level_monitor (one search input, one query_level_trigger whose condition is hits.total.value > 0, one action naming the notification channel with a mustache message template) answers 200/201 with an `_id`; POST /_plugins/_alerting/monitors/<id>/_execute answers 200 with trigger_results holding exactly one entry whose `triggered` is true, and the rendered message (the template's fixed text plus the hit count the engine computed for this run's documents) is published through SNS and arrives as exactly one SQS message within seconds. Emptying the index and re-executing leaves `triggered` false and delivers nothing; re-indexing one document resumes delivery with the new count. Each step answers in seconds.
Emulator: the check reports only `Error: Check timed out after 60.0s` — no assertion frame at all, so no step of the scenario completed and nothing in the chain can be attributed a value. Hypothesis: the unbounded waits in this scenario are the SQS long-poll drains for the delivered message (180 s budget) and the monitor execution itself, so either the monitor's SNS action never publishes to the topic or one of the alerting/notifications endpoints does not answer; a per-step 200 cannot be confirmed from this replay.
Why: alerting-to-SNS is the standard way an OpenSearch domain raises an operational signal. A client sees a monitor that appears configured and an SQS queue that stays empty, with no error anywhere: it cannot distinguish "the trigger did not fire" from "the action never published", so the withheld-when-false half of the contract is equally unobservable and an alarm pipeline silently delivers nothing.
Fix: the domain data plane's alerting/notifications plugin surface in the opensearch service crate (monitor create and _execute, notification config create/read) and the SNS publish it performs through the channel's role — the monitor execution must evaluate its trigger, render the mustache template and publish to the named topic so the subscribed SQS queue receives exactly one message per firing execution.
Traces: opensearch__sns__sqs.alerting-monitor-trigger-publishes-the-rendered-count-to-sns-and-withholds-it-when-false.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), `### opensearch__sns__sqs` section (0/1 passed); the failure is not marked ALREADY FILED or OWNED.

[mismatch] opensearch__sns__sqs.alerting-monitor-trigger-publishes-the-rendered-count-to-sns-and-withholds-it-when-false
  Error: Check timed out after 60.0s

No assertion frame is reported, so no step completed. Expected: 200 from PUT /<index> and from a refreshed _bulk with errors false; 200/201 from POST /_plugins/_notifications/configs and a GET echoing the topic ARN; 200/201 from POST /_plugins/_alerting/monitors returning an _id; 200 from POST /_plugins/_alerting/monitors/<id>/_execute with exactly one trigger_result whose triggered is true; exactly one SQS message whose Body is the rendered template (marker text + "hits=<count of matching documents>"); after _delete_by_query?refresh=true removes them, triggered false and no message within a window of 3x the observed delivery latency; after re-indexing one document, one message carrying hits=1. Actual: none of it is observable — the scenario is cut off at the cap.

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout.
2. Create an SNS topic, an SQS queue subscribed to it (raw delivery) and an IAM role the notification channel can use to publish; create a minimum single-node OpenSearch domain and wait for its published endpoint.
3. Over the domain endpoint (SigV4, signing name `es`): PUT /<index> with one shard/no replica and a {group: keyword, n: integer} mapping, then POST /_bulk?refresh=true with 6 documents all carrying group=a; the response must be 200 with errors false.
4. POST /_plugins/_notifications/configs with config_type "sns" naming the topic ARN and role ARN; GET the config back and confirm it carries the topic ARN.
5. POST /_plugins/_alerting/monitors with a disabled query_level_monitor: input search over <index> with {"size": 0, "query": {"term": {"group": "a"}}}, one query_level_trigger with condition hits.total.value > 0, and one action naming the notification config with message template "<marker> hits={{ctx.results.0.hits.total.value}}".
6. POST /_plugins/_alerting/monitors/<id>/_execute and ReceiveMessage (long poll) on the queue: AWS reports triggered true and delivers exactly one message with body "<marker> hits=6" within seconds.
7. POST /<index>/_delete_by_query?refresh=true {match_all}, execute again (triggered must be false, no message), then re-index one matching document and execute again (one message, "<marker> hits=1").
8. Replay opensearch__sns__sqs.alerting-monitor-trigger-publishes-the-rendered-count-to-sns-and-withholds-it-when-false; it is cut off at the 60 s cap with no step completed.
