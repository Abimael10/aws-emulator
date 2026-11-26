Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected
Issue-Key: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected|_run_job_to_terminal#10|Timeout|time.sleep(5)
Issue-Commit: 38a0a8578508fa045104c1fd6763f635f665f954

# emr__glue__s3: the glue.amazonaws.com role-trust job-run differential still reaches no verdict — a Glue pythonshell run never settles in a terminal JobRunState, so neither the mistrusted role's FAILED+ErrorMessage nor the control's SUCCEEDED+marker is observable

> _Note: this issue was identified against the code base at commit `38a0a8578508fa045104c1fd6763f635f665f954`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
1 net-new root cause, grounded in 1 failing behavioral check whose replay key is carried by no OPEN issue.

Prior filing, named for the fixer: 20261118T205847Z-emr-glue-s3-a-glue-pythonshell-job-run.md carries this exact check in its Issue-Trace-Ids and is now marked FIXED, yet the check still fails with the identical symptom (`Error: Check timed out after 60.0s`), so that fix did not make this check pass. The failure is a non-convergence, not a refusal, so it is restated here with its own replay key rather than as a reopened refusal. Related OPEN issues recording the same engine gap reached through other checks, which should be closed together with this one: 20261030T000623Z-glue-job-runs-never-execute-so-a-role.md, 20261029T185330Z-glue-job-runs-are-recorded-but-never-executed-e42472.md, 20261025T000830Z-glue-job-runs-are-recorded-but-never-executed.md, 20261124T203912Z-emr-glue-s3-the-eventbridge-glue-job-state-58c9b9.md. None of those lists this check's replay key, so re-replaying their trace lists does not exercise it.

Cause 1 of 1: the Glue job-run engine does not resolve a pythonshell job run to a terminal JobRunState, and does not evaluate the job Role's trust policy for the glue.amazonaws.com service principal when the run is started. A role trusting ec2.amazonaws.com and an otherwise identical role trusting glue.amazonaws.com are therefore indistinguishable, and a client that starts a run and polls GetJobRun makes no progress at all. Failing check id: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected.

AWS: Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json) — StartJobRun input {JobName (NameString, @required, length 1..255), Arguments, Timeout (@range min 1), MaxCapacity, ...} returns {JobRunId}; its modeled errors are ConcurrentRunsExceededException, EntityNotFoundException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException (all HTTP 400) and InternalServiceException (HTTP 500). A role Glue cannot assume is NOT among them, so StartJobRun is accepted and the condition surfaces on the RUN, not on the call. GetJobRun input {JobName (@required), RunId (@required), PredecessorsIncluded (default false)} returns JobRun, whose JobRunState enum is STARTING | RUNNING | STOPPING | STOPPED | SUCCEEDED | FAILED | TIMEOUT | ERROR | WAITING | EXPIRED (terminal: STOPPED, SUCCEEDED, FAILED, TIMEOUT, ERROR) and whose ErrorMessage (ErrorString) is documented as "An error message associated with this job run." On AWS a run started under a role whose trust policy does not allow sts:AssumeRole for glue.amazonaws.com settles FAILED within seconds, carrying a non-empty ErrorMessage of the role-assumption-failure class — recorded behavior only for the exact wording: its fixed text is the cannot-assume-the-role / verify-the-role's-trust-relationship sentence, and its variable part is the role ARN taken from the job definition's Role. The run's side effects never happen, so the script's marker object is absent (s3:HeadObject refused 404). The matching run whose role trusts glue.amazonaws.com settles SUCCEEDED with no ErrorMessage and its pythonshell PutObject lands the marker key with ContentLength > 0. The requirements are: StartJobRun accepted for both; a terminal JobRunState reached by both; FAILED + non-empty ErrorMessage naming the unassumable role for the mistrusted arm with no marker object; SUCCEEDED for the control with its marker object present. The run id, the role/bucket names, the timestamps and the exact byte count are per-run values and not part of the requirement.
Emulator: `Error: Check timed out after 60.0s` — no assertion in the check is ever reached. The client's bounded start-and-poll cycle (StartJobRun, then poll GetJobRun until JobRunState is in {SUCCEEDED, FAILED, STOPPED, TIMEOUT, ERROR}, re-starting only while the terminal record is FAILED with "assume role" in its ErrorMessage, across ~4x IAM's documented 20 s propagation ceiling) consumes the whole budget: neither arm ever presents an accepted terminal record. Hypothesis: the job-run state machine never advances a pythonshell run out of its non-terminal state, and no sts:AssumeRole trust evaluation of the job's Role is performed when the run is started, so neither FAILED+ErrorMessage nor SUCCEEDED+marker is ever observable.
Why: a client cannot tell a running Glue job from a finished one, cannot read why a job failed, and cannot gate downstream work (catalog writes, Glue Job State Change events, trigger/workflow chaining, bookmark advancement) on a run's outcome — polling GetJobRun never terminates. Worse for fidelity, a trust policy that omits glue.amazonaws.com is a configuration error AWS reports loudly on the run; here it is silently indistinguishable from a correct role, so a broken deployment looks exactly like a healthy one and CI that asserts on the failure reason passes for the wrong reason.
Fix: the Glue job-run executor and the StartJobRun/GetJobRun run-state machine in the glue service crate — evaluate the job Role's trust policy for the glue.amazonaws.com service principal when a run starts, drive JobRunState through RUNNING to a terminal state with StartedOn/CompletedOn/ExecutionTime populated, set the role-assumption ErrorMessage on the FAILED terminal state when that trust is absent, leave ErrorMessage absent on success, and actually execute the job's Command.ScriptLocation so an assumable role's script side effect lands in S3.
Traces: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected (suite report: "Error: Check timed out after 60.0s").

## Evidence
Check: emr__glue__s3.glue-job-start-with-role-missing-glue-trust-rejected (verdict: mismatch)
Replay output: `Error: Check timed out after 60.0s` — no assertion was reached.

Scenario: one bucket holding scripts/good.py and scripts/bad.py (each a boto3 `s3.put_object(Bucket=<bucket>, Key='markers/<label>.txt', Body=b'ok')`). Two IAM roles with arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole attached, differing ONLY in the trust principal of their sts:AssumeRole statement: the good role trusts {"Service": "glue.amazonaws.com"}, the bad role trusts {"Service": "ec2.amazonaws.com"}. Two Glue jobs, Command={Name: "pythonshell", PythonVersion: "3.9", ScriptLocation: "s3://<bucket>/scripts/<label>.py"}, MaxCapacity=0.0625, Timeout=5 — one per role.

Expected per AWS:
- StartJobRun is accepted for both jobs (returns a JobRunId); no modeled error covers an unassumable role.
- GetJobRun for the good job reaches terminal JobRunState "SUCCEEDED" with no ErrorMessage, and s3:HeadObject on markers/good.txt returns ContentLength > 0.
- GetJobRun for the bad job reaches terminal JobRunState "FAILED" with a non-empty ErrorMessage of the role-assumption-failure class (fixed text: the cannot-assume-the-role / verify-the-trust-relationship sentence; variable part: the role ARN from the job's Role), and s3:HeadObject on markers/bad.txt is refused with code "404" — the script never ran.

Actual from the emulator: the bounded start-and-poll loop never observes an accepted terminal record for either arm, so the 60 s budget expires inside it. Neither the control's SUCCEEDED + marker object nor the mistrusted role's FAILED + ErrorMessage is ever observed, and the 404 assertion proving the bad script never ran is never reached.

Note: a prior issue with this exact replay key (20261118T205847Z-emr-glue-s3-a-glue-pythonshell-job-run.md) is marked fixed; the symptom is unchanged.

## Steps to Reproduce
1. s3:CreateBucket B (with CreateBucketConfiguration.LocationConstraint outside us-east-1). s3:PutObject B/scripts/good.py and B/scripts/bad.py, each running `boto3.client('s3').put_object(Bucket='B', Key='markers/<label>.txt', Body=b'ok')` and printing 'done'.
2. iam:CreateRole R_good with AssumeRolePolicyDocument {"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}; iam:AttachRolePolicy arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole.
3. iam:CreateRole R_bad — identical except Principal {"Service":"ec2.amazonaws.com"}; attach the same managed policy.
4. glue:CreateJob(Name=J_good, Role=R_good ARN, Command={Name:"pythonshell", PythonVersion:"3.9", ScriptLocation:"s3://B/scripts/good.py"}, MaxCapacity=0.0625, Timeout=5); repeat as J_bad with R_bad and scripts/bad.py.
5. glue:StartJobRun(JobName=J_good) and poll glue:GetJobRun(JobName=J_good, RunId=<id>) until JobRunState is in {SUCCEEDED, FAILED, STOPPED, TIMEOUT, ERROR}. AWS: SUCCEEDED with no ErrorMessage. Today: no terminal state is ever reported.
6. s3:HeadObject(B, "markers/good.txt") must return ContentLength > 0. Today the object does not exist.
7. glue:StartJobRun(JobName=J_bad) and poll the same way, retrying the start across ~80 s to absorb IAM trust propagation. AWS: the run settles FAILED with a non-empty ErrorMessage naming the role Glue could not assume. Today: no terminal state is ever reported.
8. s3:HeadObject(B, "markers/bad.txt") must be refused with code "404". Not reached today.
9. Emulator: steps 5 and 7 never converge, so the client's budget expires inside the poll loop and the check is cut off at 60 s.
