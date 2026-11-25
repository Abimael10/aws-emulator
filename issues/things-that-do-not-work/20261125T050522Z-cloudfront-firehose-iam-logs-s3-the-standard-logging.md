Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id
Issue-Key: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id|_poll_until#56|Timeout|time.sleep(interval_s)
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# cloudfront__firehose__iam__logs__s3: the standard-logging-v2 scenario still reaches no verdict — the check exhausts its whole 60 s budget with no answer

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDelivery

## Description
One root cause, grounded in one failing behavioral check. The replay reports it owned by no open issue (the open issue named below carries this check id but a different replay key, so the check comes through unowned) and blocked by no unimplemented stub.

Cause 1 of 1: the CloudFront standard logging v2 end-to-end scenario produces no verdict at all — the check is killed by its own per-check timeout before any step answers, so neither the distribution-ready transition, nor the CloudWatch Logs delivery lifecycle, nor the per-request log records are ever observed.

AWS: recorded behavior only (no single modelled operation is at issue; the delivery resources are logs CreateDeliverySource / CreateDeliveryDestination / CreateDelivery, accepted only in us-east-1 regardless of the distribution's global scope). On AWS the scenario completes: a distribution created with a managed cache policy settles to Status=Deployed; a delivery source for the distribution, a delivery destination for an S3 bucket and one for a Firehose delivery stream, and a delivery for each, are all accepted; two viewer requests (a Miss then a Hit) are served at the edge; and both destinations receive one access-log record per request carrying x-edge-request-id, cs-uri-stem, sc-status and x-edge-result-type, with the same x-edge-request-id joining the S3 record to the Firehose record. The bucket/stream/distribution names and the request ids are per-run; the one-record-per-request-per-destination rule, the field set and the joinability on x-edge-request-id are the requirements.

Emulator: the check never gets that far — the whole replay detail is `Error: Check timed out after 60.0s`, so no step produced an answer within the budget and nothing was graded. Hypothesis (the timeout alone does not name the stalling call): the body's first bounded wait is GetDistribution polled until Status=='Deployed', and this same run shows the CloudFront edge of a Deployed distribution answering nothing in the sibling headings, so either the status transition or the first edge request consumes the budget before the Logs delivery legs run.

Why: a client cannot configure or observe CloudFront standard logging v2 at all — the delivery-source/destination/delivery lifecycle, the per-request access-log records and their joinability across S3 and Firehose are unobservable rather than merely wrong, and a caller waiting on a log record waits forever instead of getting a diagnosis.

Fix: whichever component stalls on this path — the CloudFront distribution status machine and edge data plane (GetDistribution must settle to Deployed and the edge must serve the two viewer requests) and/or the CloudWatch Logs standard-logging-v2 delivery resources (CreateDeliverySource / CreateDeliveryDestination / CreateDelivery) and the S3+Firehose log delivery that follows. Instrumenting which call does not answer is the first step.

Traces: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id. The OPEN issue 20261118T021609Z-cloudfront-firehose-iam-logs-s3-the-cloudfront-standard.md names this same check and the same subject, but its replay key is a `_poll_until#64|Timeout` frame while this run's failure is a harness kill with no frame at all, so the replay reports the check as owned by no issue; this filing re-grounds the same cause in this run's evidence and should be closed together with it. The resolved issue 20261117T115022Z-cloudfront-firehose-iam-logs-s3-cloudfront-standard-logging-v2-to-s3-and-firehose.md claimed this check earlier; nothing is refused with an HTTP status or error code here, so this is not described as a refusal regression.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (937/1144 passed; bootstrap_signal false; every service in this check's heading is bootstrapped). Per-service rate: cloudfront__firehose__iam__logs__s3 1/2 (50%).

Failing check `cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id`, verdict mismatch, with the entire detail being:
  `Error: Check timed out after 60.0s`
No assertion message, no HTTP status and no error code were produced.

Expected (AWS): after the distribution reaches Status=Deployed, logs CreateDeliverySource (the distribution), CreateDeliveryDestination (an S3 bucket and a Firehose delivery stream) and CreateDelivery (one per destination) all answer in us-east-1; two viewer GETs through the edge are served (Miss then Hit); each destination then holds one record per request with x-edge-request-id, cs-uri-stem, sc-status and x-edge-result-type, and the two records for one request share x-edge-request-id.
Actual (emulator): nothing observed — the scenario reached no verdict inside the 60 s harness cap.

## Steps to Reproduce
1. CreateDistribution with a managed cache policy over an S3 origin and poll GetDistribution — confirm whether Status ever becomes 'Deployed' and how long it takes.
2. In us-east-1, call logs CreateDeliverySource for that distribution, CreateDeliveryDestination for an S3 bucket and for a Firehose delivery stream, then CreateDelivery for each; confirm each call answers.
3. Issue two identical viewer GETs through the distribution domain (expect Miss then Hit) and confirm the edge answers at all.
4. Confirm one access-log record per request lands in BOTH destinations carrying x-edge-request-id, cs-uri-stem, sc-status and x-edge-result-type, and that the two records for one request share x-edge-request-id.
5. Re-replay cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id and confirm it completes with a verdict.
