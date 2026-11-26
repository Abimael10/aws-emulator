Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected
Issue-Key: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected|_run_job_to_terminal#10|Timeout|time.sleep(5)
Issue-Commit: eceaa93e215f1972a402507aeb12de03c3dfcf45

# emr__glue__s3: a Glue pythonshell job run never settles — a role that does not trust glue.amazonaws.com produces no FAILED run with an assume-role ErrorMessage, and the correctly-trusted control never SUCCEEDS or lands its marker object

> _Note: this issue was identified against the code base at commit `eceaa93e215f1972a402507aeb12de03c3dfcf45`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
Cause 1 of 1: the Glue job-run engine does not resolve a job run to a terminal state for the role-trust permission boundary. A pythonshell job whose execution role's trust policy names ec2.amazonaws.com (so Glue cannot assume it) never presents as a FAILED run carrying an assume-role ErrorMessage, and the otherwise-identical job whose role trusts glue.amazonaws.com never settles SUCCEEDED nor lands the object its script writes. A client that starts the run and polls GetJobRun makes no progress at all and is cut off at the harness budget. Failing check: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected.

AWS: Smithy com.amazonaws.glue#StartJobRun accepts {JobName (required), Arguments, ...} and returns StartJobRunResponse{JobRunId}; its modeled errors are ConcurrentRunsExceededException, EntityNotFoundException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException and InternalServiceException — a role Glue cannot assume is NOT one of them, so StartJobRun is accepted and the condition surfaces on the run, not on the call. com.amazonaws.glue#GetJobRun returns JobRun whose JobRunState enum is STARTING | RUNNING | STOPPING | STOPPED | SUCCEEDED | FAILED | TIMEOUT | ERROR | WAITING | EXPIRED (terminal: STOPPED, SUCCEEDED, FAILED, TIMEOUT, ERROR) and whose ErrorMessage (ErrorString) is documented as "An error message associated with this job run." On AWS, a run started under a role whose trust policy does not allow sts:AssumeRole for the glue.amazonaws.com service principal settles FAILED within seconds of StartJobRun, with a non-empty ErrorMessage of the role-assumption-failure class: its fixed text is the inability-to-assume-the-role sentence directing the caller to verify the role's trust relationship, and its variable part is the role ARN taken from the job definition's Role. The model does not state that text (recorded behavior only for the exact wording); what the model and the documented run-status page do establish as requirements are the terminal JobRunState FAILED, the non-empty ErrorMessage, and that the run's side effects never happen. The matching run whose role trusts glue.amazonaws.com settles SUCCEEDED and its pythonshell script's PutObject lands the marker key with a non-zero ContentLength. The run id, the role and bucket names, the timestamps and the exact byte count are per-run values, not part of the requirement.
Emulator: `Error: Check timed out after 60.0s` — no assertion in the check is ever reached. The client's start-and-poll cycle is bounded (poll GetJobRun until JobRunState is in {SUCCEEDED, FAILED, STOPPED, TIMEOUT, ERROR}, and re-start only while the terminal record is FAILED with "assume role" in its ErrorMessage, across ~4x IAM's documented 20 s propagation ceiling), so the budget is consumed inside that loop: neither arm reaches an accepted terminal record. Hypothesis: the job-run state machine never advances a pythonshell run out of its non-terminal state, and no sts:AssumeRole trust evaluation of the job's Role is performed when the run is started, so the mistrusted role and the correctly-trusted role are indistinguishable and neither FAILED+ErrorMessage nor SUCCEEDED+marker is ever observable.
Why: a client cannot tell a running job from a finished one, cannot read why a job failed, and cannot gate downstream work on a run's outcome — polling GetJobRun never terminates. Worse for fidelity, a role whose trust policy omits glue.amazonaws.com is a configuration error AWS reports loudly on the run; here it is silently indistinguishable from a correct role, so the trust boundary AWS enforces is invisible and a broken deployment looks healthy.
Fix: the Glue job-run executor and the StartJobRun/GetJobRun run-state machine — evaluating the job Role's trust policy for the glue.amazonaws.com service principal when a run is started, populating the terminal FAILED state with the role-assumption ErrorMessage when that trust is absent, and actually executing the pythonshell script (so its S3 side effect lands) for a run whose role is assumable.
Traces: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected.

Related open issues record the same engine gap reached through other checks, and this failure was not bound to any of them by the replay report, so it is filed here with its own replay key and should be closed together with them: 20261115T052937Z-emr-glue-s3-a-glue-etl-job-run-7a60e7.md and 20261114T025704Z-emr-glue-s3-a-glue-etl-job-run.md (a Glue ETL job run never reaches an accepted terminal state for the denied-source boundary), 20261109T185037Z-glue-sqs-3-net-new-causes-glue-job-state-change.md (an untrusted job role is accepted) and 20261029T185330Z-glue-job-runs-are-recorded-but-never-executed-e42472.md.

## Evidence
Failing check (mismatch): emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected
Replay output: `Error: Check timed out after 60.0s` — no assertion was reached.

Scenario: one bucket holding scripts/good.py and scripts/bad.py (each a boto3 s3.put_object of b'ok' to its own markers/<label>.txt key). Two IAM roles, each with AWSGlueServiceRole attached and differing ONLY in the trust principal of their sts:AssumeRole statement: the good role trusts {"Service": "glue.amazonaws.com"}, the bad role trusts {"Service": "ec2.amazonaws.com"}. Two Glue jobs, Command={Name: pythonshell, PythonVersion: 3.9, ScriptLocation: s3://<bucket>/scripts/<label>.py}, MaxCapacity=0.0625, Timeout=5 — one per role.

Expected per AWS:
- StartJobRun succeeds for both jobs (returns a JobRunId); no modeled error covers an unassumable role.
- GetJobRun for the good job reports JobRunState "SUCCEEDED", and HeadObject on markers/good.txt returns ContentLength > 0.
- GetJobRun for the bad job reports JobRunState "FAILED" with a non-empty ErrorMessage of the role-assumption-failure class (fixed text: the cannot-assume-the-role / verify-the-trust-relationship sentence; variable part: the role ARN from the job's Role), and HeadObject on markers/bad.txt is refused 404 — the script never ran.

Actual from the emulator: the bounded start-and-poll loop never observes an accepted terminal record for either arm, so the 60 s budget expires inside it. Neither the control's SUCCEEDED + marker object nor the mistrusted role's FAILED + ErrorMessage is ever observed, and the 404 assertion proving the bad script never ran is never reached.

## Steps to Reproduce
1. Create a bucket and put two pythonshell scripts in it: scripts/good.py and scripts/bad.py, each doing `boto3.client('s3').put_object(Bucket=<bucket>, Key='markers/<label>.txt', Body=b'ok')`.
2. iam.create_role(RoleName=<good>, AssumeRolePolicyDocument={Version 2012-10-17, Statement:[{Effect: Allow, Principal: {Service: "glue.amazonaws.com"}, Action: "sts:AssumeRole"}]}) and attach arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole.
3. iam.create_role(RoleName=<bad>, ...) identical except Principal {Service: "ec2.amazonaws.com"}; attach the same managed policy.
4. glue.create_job(Name=<good-job>, Role=<good role ARN>, Command={Name: "pythonshell", PythonVersion: "3.9", ScriptLocation: "s3://<bucket>/scripts/good.py"}, MaxCapacity=0.0625, Timeout=5); repeat for <bad-job> with the bad role and scripts/bad.py.
5. glue.start_job_run(JobName=<good-job>) and poll glue.get_job_run(JobName, RunId) until JobRunState is terminal. AWS: SUCCEEDED. Then s3.head_object(Bucket, Key="markers/good.txt") returns ContentLength > 0.
6. glue.start_job_run(JobName=<bad-job>) and poll the same way, retrying the start across ~80 s to absorb IAM trust propagation. AWS: the run settles FAILED with a non-empty ErrorMessage naming the role it could not assume, and s3.head_object(Bucket, Key="markers/bad.txt") is refused with 404.
7. Emulator: neither run reaches an accepted terminal record — GetJobRun never reports SUCCEEDED for the good job nor FAILED-with-an-assume-role-ErrorMessage for the bad one, so the polling loop runs out its budget and the check is cut off at 60 s.
