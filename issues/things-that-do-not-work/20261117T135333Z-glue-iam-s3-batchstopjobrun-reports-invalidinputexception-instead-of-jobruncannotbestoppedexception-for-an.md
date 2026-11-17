Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam__s3.batch-stop-job-run-rejects-a-succeeded-run-but-stops-a-running-one
Issue-Key: glue__iam__s3.batch-stop-job-run-rejects-a-succeeded-run-but-stops-a-running-one|run#11|AssertionError|assert errors_by_run.get(finished_run_id) == _STOP_REJECTED, (
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# BatchStopJobRun reports InvalidInputException instead of JobRunCannotBeStoppedException for an already-finished run

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchStopJobRun

## Description
Cause 1 of 1: the per-run error for a run that has already finished carries the wrong error code and message.

AWS: Smithy (glue BatchStopJobRun) returns per-run entries in `Errors[]` with an ErrorDetail; the recording shows a run already in state SUCCEEDED reported as ErrorCode `JobRunCannotBeStoppedException` with ErrorMessage `Job Run cannot be stopped in current state.` — a fixed sentence with no request-derived parts. A still-RUNNING run in the same call is stopped successfully.

Emulator: the same entry is reported as ErrorCode `InvalidInputException` with a message of its own invention: `Job run jr_<id> has already finished in state SUCCEEDED and cannot be stopped` (the only request-derived parts are the run id and the state). Both the code and the sentence diverge from AWS.

Why: a client branching on ErrorCode — the documented way to tell "this run cannot be stopped because it is finished" from "your input was invalid" — takes the wrong branch, and InvalidInputException reads as a caller bug rather than a state conflict.

Fix: the BatchStopJobRun handler's per-run error classification and message for a run in a terminal state.

Traces: glue__iam__s3.batch-stop-job-run-rejects-a-succeeded-run-but-stops-a-running-one.

## Evidence
Expected per-run error: `('JobRunCannotBeStoppedException', 'Job Run cannot be stopped in current state.')`.
Actual: `('InvalidInputException', 'Job run jr_5ed2db1fcef95ba1f6145030bf8e0faa01f4afa3418d7f7f032b5f2bc8edd0ab has already finished in state SUCCEEDED and cannot be stopped')`

## Steps to Reproduce
1. Create a Glue job and start two runs; let one reach SUCCEEDED and keep the other RUNNING.
2. Call BatchStopJobRun(JobName=<job>, JobRunIds=[<succeeded>, <running>]).
Observed: the finished run's Errors[] entry is InvalidInputException with a bespoke message. Expected: ErrorCode JobRunCannotBeStoppedException, ErrorMessage "Job Run cannot be stopped in current state."; the running run is stopped.
