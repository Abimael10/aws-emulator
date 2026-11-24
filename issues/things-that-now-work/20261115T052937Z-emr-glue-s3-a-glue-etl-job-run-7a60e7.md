Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.glue-job-missing-s3-getobject-source-rejected
Issue-Key: emr__glue__s3.glue-job-missing-s3-getobject-source-rejected|_start_and_wait_with_retry#11|Timeout|time.sleep(delay)
Issue-Commit: 07acbfe4a3d7fac5eb5dd49030154c6544330d6e

# emr__glue__s3: a Glue ETL job run never settles in an accepted terminal state for the denied-source permission boundary, so GetJobRun polling exhausts the budget

> _Note: this issue was identified against the code base at commit `07acbfe4a3d7fac5eb5dd49030154c6544330d6e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
Cause 1 of 1: a Glue ETL job run started with a role that is explicitly denied s3:GetObject on the script's source prefix never settles into the terminal state AWS produces for that condition, so a client polling GetJobRun makes no progress and the check is cut off at the harness budget. Failing check: emr__glue__s3.glue-job-missing-s3-getobject-source-rejected.

AWS: Smithy com.amazonaws.glue#StartJobRun returns StartJobRunResponse{JobRunId}; com.amazonaws.glue#GetJobRun returns JobRun{JobRunState, ErrorMessage, ...} whose JobRunState enum carries the terminal values SUCCEEDED, FAILED, TIMEOUT, ERROR and STOPPED. A Glue 4.0 ETL run whose execution role is denied s3:GetObject on the source prefix settles FAILED, with an ErrorMessage reporting the source as a missing path (Hadoop's S3A filesystem, which the DynamicFrame CSV reader sits on, masks the 403 as a FileNotFoundException during schema inference, so the deterministic signature is "No such file or directory"), and writes nothing under the output prefix. The identical job run under a role that holds s3:GetObject on that prefix settles SUCCEEDED and lands at least one `part-*` object under the output prefix. Those two terminal states, the ErrorMessage class on the failing one, and the presence/absence of output are the requirement; the run id, timestamps and object names are per-run values.
Emulator: `Error: Check timed out after 60.0s`. The client's start+wait cycle is bounded (`is_acceptable` = FAILED with a source-read-blocked ErrorMessage for the negative arm, SUCCEEDED for the control) and retries only when the terminal run is NOT the accepted one, so the budget is consumed inside that loop: the denied-source run does not present as FAILED-with-a-source-read error. Hypothesis: the in-process job-script executor does not evaluate the execution role's S3 authorization for the source read, so the run settles in a state other than the documented FAILED/"No such file or directory", and the retry loop (4 attempts, 20 s apart) burns the budget before any assertion is reached.
Why: a client cannot tell a job that failed from one still running, cannot read why it failed, and cannot gate downstream work on run outcome. Worse for fidelity, an IAM misconfiguration on the job role produces the same observation as a correctly permissioned job, so a permission boundary that AWS enforces is invisible.
Fix: the Glue job-run executor and the StartJobRun/GetJobRun run-state machine — authorization of the job role for the source-prefix read performed by the run, and the terminal-state plus ErrorMessage population when that read is refused.
Traces: emr__glue__s3.glue-job-missing-s3-getobject-source-rejected.

Open issue 20261114T025704Z-emr-glue-s3-a-glue-etl-job-run.md records a Glue ETL job run that never reaches a terminal state for this same check id, and 20261029T175551Z-glue-job-runs-reached-through-the-emr-glue.md / 20261113T160144Z-emr-glue-s3-3-net-new-causes-an.md record the same engine gap under other groups; the replay report did not bind this run's failure to any of them, so it is filed here with its own replay key and should be closed together with them.

## Evidence
Failing check (mismatch): emr__glue__s3.glue-job-missing-s3-getobject-source-rejected
Replay output: `Error: Check timed out after 60.0s` — no assertion was reached.

Scenario: one bucket holding data/input.csv (3 rows) and scripts/read_csv.py; two Glue 4.0 glueetl jobs on that script, G.1X x2, MaxRetries=0, Timeout=15. Negative role: Allow s3:ListBucket on the bucket, Allow s3:GetObject/s3:PutObject on scripts/* and output/*, explicit Deny s3:GetObject on data/*. Positive control role: Allow s3:GetObject/ListBucket/PutObject on the bucket and all keys.

Expected per AWS: GetJobRun(JobName, RunId) for the negative run reports JobRunState "FAILED" with an ErrorMessage containing "No such file or directory" for the denied source path, and ListObjectsV2 under the output prefix returns KeyCount 0; GetJobRun for the control run reports "SUCCEEDED" with at least one `part-*` object under its output prefix whose single text value is the row count 3.
Actual from the emulator: the bounded start+wait loop never observes the accepted terminal condition for the negative arm, so the 60 s budget expires inside it and neither the FAILED verdict/ErrorMessage nor the control's output object is ever observed.

## Steps to Reproduce
1. Create a bucket; put `data/input.csv` (header + 3 rows) and `scripts/read_csv.py` (a Glue DynamicFrame CSV read of --SOURCE_PATH, writing the row count to --OUTPUT_PATH).
2. Create an IAM role trusting glue.amazonaws.com with AWSGlueServiceRole plus: Allow s3:ListBucket on the bucket, Allow s3:GetObject/s3:PutObject on `scripts/*` and `output/*`, and an explicit Deny of s3:GetObject on `data/*`.
3. glue.create_job(Name=<bad>, Role=<that role>, Command={Name: glueetl, ScriptLocation: s3://<bucket>/scripts/read_csv.py, PythonVersion: 3}, GlueVersion=4.0, WorkerType=G.1X, NumberOfWorkers=2, MaxRetries=0, Timeout=15).
4. glue.start_job_run(JobName=<bad>, Arguments={"--SOURCE_PATH": "s3://<bucket>/data/", "--OUTPUT_PATH": "s3://<bucket>/output/bad-run/"}) and poll glue.get_job_run until JobRunState is terminal.
5. AWS: FAILED with an ErrorMessage containing "No such file or directory" and no objects under output/bad-run/. Emulator: no accepted terminal state is observed and the poll runs out its budget.
6. Control: repeat steps 2-5 with a role granted s3:GetObject on the bucket and all keys — AWS settles SUCCEEDED with one `part-*` object holding "3".
