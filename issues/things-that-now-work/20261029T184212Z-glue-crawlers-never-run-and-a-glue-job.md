Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.cfn-deploy-glue-crawler-s3-target-stack-positive emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative emr__glue__s3.eventbridge-s3-object-created-triggers-glue-crawler-positive emr__glue__s3.glue-job-missing-s3-getobject-source-rejected
Issue-Key: emr__glue__s3.cfn-deploy-glue-crawler-s3-target-stack-positive|_start_crawler_with_retry#12|Timeout|time.sleep(backoff_s)
Issue-Key: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative|_start_crawler_with_retry#16|Timeout|time.sleep(backoff_s)
Issue-Key: emr__glue__s3.eventbridge-s3-object-created-triggers-glue-crawler-positive|_poll_until#26|Timeout|time.sleep(interval_s)
Issue-Key: emr__glue__s3.glue-job-missing-s3-getobject-source-rejected|_start_and_wait_with_retry#11|Timeout|time.sleep(delay)
Issue-Commit: 969cd59636329c2a8786028d0edb680658e5b91d

# Glue crawlers never run and a Glue job run never executes, reached through the emr__glue__s3 cross-service checks (2 causes)

> _Note: this issue was identified against the code base at commit `969cd59636329c2a8786028d0edb680658e5b91d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartCrawler

## Description
This issue bundles 2 net-new root causes in the Glue crate, grounded in four `emr__glue__s3` cross-service checks whose failing calls are all Glue calls (so the service is attributed to `glue`). All four are reported only as `Check timed out after 60.0s`: each one wraps the Glue call in a bounded retry/poll loop (StartCrawler is retried 5 times with a 15 s backoff; the job run is started up to 4 times with a 20 s delay), so the emulator's refusal/wrong terminal state is swallowed by that loop and the harness kills the check instead of surfacing an assertion. Related open issues cover the same code paths for OTHER replay keys and do NOT carry these check ids: 20261024T015202Z-glue-startcrawler-stub.md (StartCrawler scaffold stub — the suite attributes only emr__glue__s3.cli-glue-crawler-create-run-check-catalog-workflow to it), 20261029T175551Z-glue-job-runs-reached-through-the-emr-glue.md and 20261025T000830Z-glue-job-runs-are-recorded-but-never-executed.md (Glue job runs settling FAILED without executing the script).

--- Cause 1 of 2: a crawler never crawls — it never reports a finished crawl and infers nothing into the Data Catalog ---
AWS: Smithy: `Crawler.State` is READY | RUNNING | STOPPING and `LastCrawlInfo.Status` is SUCCEEDED | CANCELLED | FAILED with StartTime, LogGroup/LogStream and (on failure) ErrorMessage. StartCrawler on a READY crawler returns 200 and moves it to RUNNING; when the crawl finishes the crawler is READY again with LastCrawl.Status=SUCCEEDED, and GetTables on the target database returns one inferred table whose StorageDescriptor.Location is the crawled S3 prefix (`s3://<bucket>/data/`) and whose Columns are the inferred schema (3 columns for the seeded id,name,value CSV).
Emulator: the crawler never completes a crawl. The checks poll `GetCrawler(Name=...).Crawler` for State==READY with a LastCrawl (timeout_s=300) after retrying StartCrawler, and are killed at the 60 s harness ceiling — the 5x15 s StartCrawler retry window alone consumes it, consistent with StartCrawler still answering the scaffold refusal (`operation StartCrawler is scaffolded but not implemented in this emulator`, which the suite reports elsewhere as NotImplemented) and with there being no crawler run state machine behind it. No table is inferred into the catalog.
Why: the whole crawl-then-query flow is dead: no table is created, so every downstream catalog read (Athena/Hive/Spark over a crawled prefix) fails, an S3-event-driven crawler pipeline delivers nothing, and the CloudFormation-declared-crawler negative case (a crawler whose S3 target bucket does not exist must make the stack fail with the crawler's own S3-target 404 validation in the failure reason) can never be graded because its in-file positive control never completes.
Fix: the Glue crate's crawler execution path (services/service-com-amazonaws-glue-awsglue — the StartCrawler handler and the crawler run state that backs GetCrawler): implement StartCrawler, drive the crawler RUNNING -> READY with a populated LastCrawlInfo, and have the crawl actually read its S3 targets and write the inferred database/table into the catalog state.
Traces: emr__glue__s3.cfn-deploy-glue-crawler-s3-target-stack-positive, emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative, emr__glue__s3.eventbridge-s3-object-created-triggers-glue-crawler-positive.

--- Cause 2 of 2: a Glue job run is settled without executing the script, so neither the permission-denied run nor the permitted run is distinguishable ---
AWS: recorded behavior only: StartJobRun runs the job's script; a run whose role lacks s3:GetObject on the source prefix ends FAILED with Glue's own error text identifying the unreadable source path, and writes nothing under --OUTPUT_PATH; a run whose role has that permission ends SUCCEEDED and leaves a `part-*` object under --OUTPUT_PATH containing the computed row count. JobRun.ErrorMessage on a genuine failure is AWS-authored text naming the failure.
Emulator: the run settles terminal immediately without executing anything and without the source-read failure signature, so the check's `is_acceptable` predicate (FAILED *and* the error shows the denied S3 read) is never satisfied; it restarts the run up to four times with a 20 s delay and is killed at 60 s. The permitted (positive-control) run equally never reaches SUCCEEDED and writes no output object. This is the same execution gap already filed for other replay keys in 20261029T175551Z-glue-job-runs-reached-through-the-emr-glue.md (emulator-authored ErrorMessage `This emulator records job runs but executes no job script, so the job run ended without running.`), which does not carry this check.
Why: a client cannot tell an IAM-denied source read from any other failure, and the permission boundary the check exists to prove is unobservable; nothing is ever written under --OUTPUT_PATH.
Fix: the Glue crate's job-run execution path and state machine (the StartJobRun handler and the job-run state read by GetJobRun) — execute the referenced script, settle SUCCEEDED with its S3 side effects when the run role permits the source read, and FAILED with AWS-shaped error text identifying the unreadable source when it does not.
Traces: emr__glue__s3.glue-job-missing-s3-getobject-source-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator (group emr__glue__s3: 1/46 checks passing). All four checks report exactly one detail: `Error: Check timed out after 60.0s`.

Cause 1:
- emr__glue__s3.cfn-deploy-glue-crawler-s3-target-stack-positive — deploys a CFN stack (bucket + crawler role + AWS::Glue::Crawler over `s3://<bucket>/data/`), seeds data/part-00000.csv, then `_start_crawler_with_retry(attempts=5, backoff_s=15)` followed by `_poll_until(GetCrawler(...).Crawler, State==READY, timeout_s=300)` and asserts `Crawler.LastCrawl.Status == "SUCCEEDED"`, exactly 1 table in the database, 3 inferred columns and StorageDescriptor.Location == `s3://<bucket>/data/`. The check never gets past the start/poll loop.
- emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative — same positive control first (crawler on a real prefix must reach READY with LastCrawl SUCCEEDED and create 1 table) before judging the negative stack, which must end ROLLBACK_COMPLETE/CREATE_FAILED with "404" and "s3 target" in the stack failure reasons. Times out in the positive control.
- emr__glue__s3.eventbridge-s3-object-created-triggers-glue-crawler-positive — S3 EventBridge notification -> rule -> Lambda target that calls StartCrawler; the check then waits to observe the crawler actually run to completion. Times out.
The suite separately reports `An error occurred (NotImplemented) when calling the StartCrawler operation: operation StartCrawler is scaffolded but not implemented in this emulator` for the sibling check emr__glue__s3.cli-glue-crawler-create-run-check-catalog-workflow, which is the refusal these three loops swallow.

Cause 2:
- emr__glue__s3.glue-job-missing-s3-getobject-source-rejected — two sibling jobs over the same bucket (`data/input.csv` with 3 rows). The denied run is started via `_start_and_wait_with_retry(..., is_acceptable=lambda r: r["JobRunState"] == "FAILED" and _is_source_read_blocked(r), timeout_s=900, retries=3, delay=20)` and must leave `output/bad-run/` empty; the permitted run must reach SUCCEEDED with a `part-*` object under `output/good-run/` whose content is `3`. Neither condition is ever met and the check is killed at 60 s.

## Steps to Reproduce
Boot the emulator and replay these checks from suite simparity-behavioral-aws-v1 (cross-service group emr__glue__s3; every hanging call is a Glue call):
1. Cause 1 — emr__glue__s3.cfn-deploy-glue-crawler-s3-target-stack-positive: CreateStack with a bucket + crawler role + AWS::Glue::Crawler targeting `s3://<bucket>/data/`, put one CSV object there, call StartCrawler, then poll GetCrawler until State==READY and assert LastCrawl.Status==SUCCEEDED and that GetTables returns one inferred 3-column table located at `s3://<bucket>/data/`. Replay emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative (same positive control, then a stack whose crawler targets a nonexistent bucket must fail with the crawler's S3-target 404 in the failure reason) and emr__glue__s3.eventbridge-s3-object-created-triggers-glue-crawler-positive (S3 Object Created -> EventBridge rule -> Lambda -> StartCrawler, then observe the crawl finish).
2. Cause 2 — emr__glue__s3.glue-job-missing-s3-getobject-source-rejected: create two roles (one without s3:GetObject on `data/*`), CreateJob twice over the same PySpark script, StartJobRun both with --SOURCE_PATH/--OUTPUT_PATH, and poll GetJobRun. Expect the denied run FAILED with error text naming the unreadable source and an empty `output/bad-run/`, and the permitted run SUCCEEDED with a `part-*` object under `output/good-run/` containing `3`.

## Resolution

Cause 1 (the crawler that never crawls) was already implemented when this report was picked
up: `StartCrawler` performs the crawl, the crawler leaves `READY` and is owed its way back one
span later with a populated `LastCrawl`, and the datasets found behind its Amazon S3 targets
are written into the target database. What was MISSING for the third check was the other half
of that flow: a crawler's `S3Target$Path` was never resolved against the object store, so a
crawler whose target bucket does not exist was created and the CloudFormation stack that
declares it reached CREATE_COMPLETE instead of rolling back. `CreateCrawler` and
`UpdateCrawler` now resolve every Amazon S3 target against the store and refuse a path whose
bucket it does not hold with `InvalidInputException`, naming the target and the store's own 404
(`state::s3_target_refusal`, `state::messages::s3_target_not_found`). Pinned by
`tests/all/crawler_s3_target_resolution.rs`. The three crawler checks of this report pass.

Cause 2 (the job run that settles without executing its script) was likewise implemented: a
Spark job run executes its script with the Spark engine and the AWS Glue ETL library on its
import path, reading and writing as the job's `Role`. What this change fixed is HOW a source
the role may not read is reported: a read resolves its source path before it plans anything,
and an object store answers a caller it may not read the same way whether or not a file is
there — so a refused resolution is reported as Spark's analysis failure for a path that does
not exist, naming the path (`services/common/src/spark_runtime/filesystem.rs`,
`state/glue_etl/table_data.rs`), where it previously surfaced the store's raw 403. A refused
WRITE still reports the store's own refusal. Pinned by
`services/service-com-amazonaws-glue-awsglue/tests/all/job_run_source_authorization.rs` and,
on the assembled router with request authentication ON, by
`core/router/tests/all/zz_glue_job_source_permission_probe.rs`: the denied run comes to rest
FAILED naming its unreadable source with nothing under its output path, and the permitted run
SUCCEEDED with one `part-` object holding the count of the source's rows.

`emr__glue__s3.glue-job-missing-s3-getobject-source-rejected` still ends as `Check timed out
after 60.0s`: it burns its 60 s harness budget inside its own retry sleep
(`_start_and_wait_with_retry`), and the check's script and its `_is_source_read_blocked`
predicate are not readable from this checkout, so what it needs beyond the rule above is
unknown from here. Open issue
`20261114T025704Z-emr-glue-s3-a-glue-etl-job-run.md` carries that same check.
