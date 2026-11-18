Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id
Issue-Key: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id|_poll_until#63|Timeout|last = probe()
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# CloudFront standard logging v2 to S3 and Firehose never produces a joinable access-log record, so the check reaches no verdict

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDelivery

## Description
Cause 1 of 1: a distribution with two standard-logging-v2 deliveries (S3 and Firehose) never emits the access-log records those deliveries are supposed to carry.
AWS: recorded behavior only — standard logging v2 is configured in us-east-1 as a CloudWatch Logs delivery source for the distribution plus delivery destinations (an S3 bucket and a Firehose delivery stream) wired by CreateDelivery; once viewer requests are served, each request produces one access-log record in EVERY configured destination, and the records for the same request carry the same edge request id in both, so they can be joined on it.
Emulator: the check makes no progress at all and is killed by the harness at its 60 s budget — no record arrives in either destination, so there is nothing to join.
Why: logging v2 is the only way to get CloudFront access logs into Firehose/CloudWatch; a consumer sees configured, healthy-looking deliveries and an empty destination forever.
Fix: the standard-logging-v2 delivery path for a CloudFront delivery source — the edge's per-request access-log record generation and its fan-out to each configured delivery destination (including the shared edge request id).
Related open issue that looks similar but does not carry this replay key: 20261110T105638Z-cloudfront-logs-cloudfront-standard-logging-v2-delivers-no-fa23e8.md.
Failing check: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id

## Evidence
[mismatch] cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id — 'Error: Check timed out after 60.0s'. Expected: a served viewer request produces one access-log record in the S3 destination and one in the Firehose destination, both carrying the same edge request id. Actual: no verdict; no record in either destination.

## Steps to Reproduce
1. In us-east-1 create a CloudWatch Logs delivery source for a CloudFront distribution plus two delivery destinations (S3 bucket, Firehose stream) and CreateDelivery for each.
2. Serve a request (then a repeat, for a cache Hit) through the distribution and poll both destinations for access-log records carrying a common edge request id.
