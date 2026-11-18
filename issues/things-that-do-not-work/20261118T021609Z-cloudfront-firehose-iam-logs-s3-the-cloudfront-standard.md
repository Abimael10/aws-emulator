Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id
Issue-Key: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id|_poll_until#64|Timeout|last = probe()
Issue-Commit: 86d32c23846cd48a29a50565dea948b0a247c0e9

# cloudfront__firehose__iam__logs__s3: the CloudFront standard-logging-v2 scenario never reaches a verdict — the check exhausts its whole 60 s budget with no answer

> _Note: this issue was identified against the code base at commit `86d32c23846cd48a29a50565dea948b0a247c0e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDelivery

## Description
One root cause, grounded in one failing behavioral check that the replay reports owned by no open issue and blocked by no unimplemented stub.

Cause 1 of 1: the CloudFront standard logging v2 end-to-end scenario produces no verdict at all — the check is killed by its own per-check timeout instead of reaching any assertion.
AWS: recorded behavior only (no single modelled operation is at issue). On AWS the scenario completes: a distribution created with the managed CachingOptimized cache policy settles to Status=Deployed, CloudWatch Logs standard logging v2 resources (CreateDeliverySource for the distribution, CreateDeliveryDestination for an S3 bucket and for a Firehose delivery stream, then CreateDelivery for each) are accepted in us-east-1, two viewer requests (a Miss then a Hit) are served, and both the S3 and the Firehose delivery carry one record per request with the fields x-edge-request-id, cs-uri-stem, sc-status and x-edge-result-type, joinable across the two destinations on x-edge-request-id.
Emulator: the check never gets that far — the replay reports only `Error: Check timed out after 60.0s`, so no step of the scenario produced an answer within the budget and nothing was graded. Hypothesis: the first bounded wait in the body is GetDistribution polled at 15 s intervals until Status=Deployed, so a distribution that never leaves InProgress (or any control-plane call on the Logs delivery resources that does not answer) consumes the whole budget before the data-plane legs run; the timeout alone does not say which call hangs.
Why: a client cannot configure or observe CloudFront standard logging v2 at all in the emulator — neither the distribution-ready transition nor the delivery-source/destination/delivery lifecycle nor the per-request log records can be exercised, so every behavior that depends on edge log delivery is unobservable rather than merely wrong.
Fix: whichever component stalls on this path — the CloudFront distribution status machine (GetDistribution must settle to Deployed) and/or the CloudWatch Logs standard-logging-v2 delivery resources (CreateDeliverySource / CreateDeliveryDestination / CreateDelivery and the S3+Firehose log delivery that follows). Instrumenting which call does not answer is the first step.
Traces: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (796/1144 passed; bootstrap_signal false; every service in this check's heading is bootstrapped).

Failing check `cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id`, verdict mismatch, with the entire detail being:
  `Error: Check timed out after 60.0s`
No assertion message, no HTTP status and no error code were produced, i.e. the scenario reached no verdict. The check body's first bounded wait is `_wait_distribution_deployed` (GetDistribution polled every 15 s until Status == 'Deployed', ceiling 900 s cut to the runner's remaining time); after it the check creates the Logs delivery source, the S3 and Firehose delivery destinations and the two deliveries in us-east-1, issues two viewer requests and then joins the S3 and Firehose records on x-edge-request-id over the fields x-edge-request-id, cs-uri-stem, sc-status, x-edge-result-type. None of that was observed.

## Steps to Reproduce
1. git_clone the repo, build and boot the emulator from the checkout, then replay simparity-behavioral-aws-v1 and observe `cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id` end with `Check timed out after 60.0s` and no assertion.
2. By hand: CreateDistribution with the managed CachingOptimized cache policy (658327ea-f89d-4fab-a63d-7e88639e58f6) over an S3 origin, then poll GetDistribution — confirm whether Status ever becomes `Deployed` and how long it takes.
3. In us-east-1 call CreateDeliverySource for that distribution, CreateDeliveryDestination for an S3 bucket and for a Firehose delivery stream, then CreateDelivery for each, and confirm each call answers.
4. Issue two identical viewer GETs through the distribution (expect Miss then Hit) and confirm one log record per request lands in BOTH destinations carrying x-edge-request-id, cs-uri-stem, sc-status and x-edge-result-type, with the same x-edge-request-id joining the two records.
5. Re-replay the check and confirm it completes and passes.
