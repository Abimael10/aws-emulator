Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected
Issue-Key: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected|_retry_iam_propagation#8|Timeout|time.sleep(interval_s)
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue crawler-schedule refusal checks never complete: the control-plane calls do not answer within the budget

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateCrawlerSchedule

## Description
Cause 1 of 1: the crawler-schedule state-machine check (no-schedule crawler and unknown crawler refusals) never finishes.

AWS: Smithy (glue UpdateCrawlerSchedule / StartCrawlerSchedule / StopCrawlerSchedule) declares EntityNotFoundException and SchedulerTransitioningException/NoScheduleException class refusals; these are plain control-plane calls that answer immediately — the recording completes the whole sequence in well under the suite's budget.

Emulator: the replay reports `Check timed out after 60.0s`, so the sequence of schedule calls (start/stop on a crawler with no schedule, and the same on an unknown crawler) never returns the refusals the check grades. Nothing in this check waits on an asynchronous resource, so the stall is in answering the calls themselves.

Why: a client issuing crawler-schedule operations hangs instead of receiving the modeled refusal, which no retry/timeout strategy can turn into a usable error.

Fix: the crawler-schedule handlers (UpdateCrawlerSchedule/StartCrawlerSchedule/StopCrawlerSchedule) — answer the modeled refusal for a crawler with no schedule and EntityNotFoundException for an unknown crawler. Hypothesis: one of these calls blocks rather than returning.

Traces: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected.

## Evidence
Expected: each crawler-schedule call answers promptly with its modeled refusal (no-schedule crawler; unknown crawler -> EntityNotFoundException).
Actual: `Error: Check timed out after 60.0s` — the check never reached a verdict.

## Steps to Reproduce
1. CreateCrawler with no Schedule.
2. Call StartCrawlerSchedule/StopCrawlerSchedule on it, then the same on a crawler name that does not exist.
Observed: the check does not complete within 60s. Expected: prompt modeled refusals for both.
