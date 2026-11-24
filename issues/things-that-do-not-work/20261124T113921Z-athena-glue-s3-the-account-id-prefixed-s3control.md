Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__glue__s3.iam-role-trusts-s3-service-positive
Issue-Key: athena__glue__s3.iam-role-trusts-s3-service-positive|_create_and_run_job#24|EgressRefused|job_id = s3control.create_job(
Issue-Commit: df3e5af3e6ae07ab9a03dea7418cd4cc91eea7e3

# athena__glue__s3: the account-ID-prefixed s3control host is still unserved, so the S3-Batch-Operations trust check never reaches the emulator

> _Note: this issue was identified against the code base at commit `df3e5af3e6ae07ab9a03dea7418cd4cc91eea7e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateJob

## Description
Cause 1 of 1: every AccountId-bearing S3 Control operation is addressed by the SDK at an account-ID-prefixed authority, and the emulator serves no address for that host, so the call is refused before a byte is sent.

Note for triage: the open issue 20261116T230421Z-emr-glue-s3-the-account-id-prefixed-s3control.md describes the same root cause, but it carries only the emr__glue__s3 replay keys (emr__glue__s3.iam-role-trust-s3-service-assumed-for-batch-op-positive and emr__glue__s3.s3-batch-operation-role-missing-s3-trust-rejected). The failing check named here is a different check in a different service group and is NOT covered by that issue's keys, so it is filed separately; fixing the host routing should close both, and this issue's trace_ids are what must be re-replayed to confirm that.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml, endpointPrefix 's3-control', sigv4Name 's3'): CreateJob is POST /v20180820/jobs with input member AccountId bound as @httpHeader('x-amz-account-id') and @hostLabel, @pattern ^\d{12}$, required, and the operation carries @endpoint(hostPrefix: "{AccountId}."); DescribeJob and UpdateJobStatus carry the same hostPrefix. The SDK therefore prepends "<AccountId>." to the configured endpoint authority, and the service answers on that prefixed host: a well-formed CreateJob answers 200 with a JobId, DescribeJob then reports the job advancing to a terminal Status, and a role that does not trust batchoperations.s3.amazonaws.com is refused by the SERVICE (a modeled client error / a Failed job), never by the transport.

Emulator: the emulator is reached at its own endpoint host and announces/serves no address for the prefixed authority, so the request never leaves the runner: "CapabilityNotOffered: the target serves no address for <account-id>.<endpoint-host>" / "egress refused: '<account-id>.<endpoint-host>' is not the endpoint under test (<endpoint-host>); an emulator run may reach only the emulator ... the emulator announces an address other than the endpoint it is reached on". No HTTP status and no body are produced for CreateJob at all.

Why: S3 Batch Operations is entirely unreachable through the emulator. A client cannot submit a job, cannot poll it to a terminal state, and cannot observe the refusal AWS gives for a role missing the S3-Batch-Operations trust — a connection-level failure is indistinguishable from any service answer, so error handling and retry logic written against AWS behave arbitrarily here.

Fix: the emulator's endpoint/host routing for the s3control service — the address it announces must also answer the "{AccountId}." prefixed authority (serve the host alias, or strip a leading 12-digit account label when resolving the virtual host), instead of only the bare endpoint host.

Traces: athena__glue__s3.iam-role-trusts-s3-service-positive.

## Evidence
Check athena__glue__s3.iam-role-trusts-s3-service-positive (mismatch), failing on the first s3control CreateJob:
  Error: CapabilityNotOffered: the target serves no address for <account-id>.<endpoint-host>
  File "egress.py", line 371, in resolve -> refuse(...) -> simparity.egress.EgressRefused: egress refused: '<account-id>.<endpoint-host>' is not the endpoint under test (<endpoint-host>); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on
(the recorded run shows the 12-digit zero account id and the loopback endpoint host; both are per-environment values, the requirement is that the "<AccountId>." prefixed authority is served at all.)

Expected (AWS): s3control CreateJob -> HTTP 200 with <JobId>; DescribeJob -> Job.Status reaching "Complete" for a role whose trust policy allows sts:AssumeRole for Service batchoperations.s3.amazonaws.com; the PutObjectTagging job's tag then readable via GetObjectTagging.
Actual (emulator): no HTTP exchange at all — the request is refused at address resolution because the account-prefixed authority the SDK builds from the operation's @endpoint hostPrefix is not served.

Same symptom, different checks, already filed under emr__glue__s3: 20261116T230421Z-emr-glue-s3-the-account-id-prefixed-s3control.md.

## Steps to Reproduce
1. Replay the behavioral suite and run check athena__glue__s3.iam-role-trusts-s3-service-positive.
2. Minimal reproduction with any SDK pointed at the emulator:
   a. sts GetCallerIdentity -> Account.
   b. s3 CreateBucket + PutObject for a target object and a CSV manifest; iam CreateRole with a trust policy allowing sts:AssumeRole for Service "batchoperations.s3.amazonaws.com" and an inline policy granting s3:GetObject/PutObject/PutObjectTagging on the bucket.
   c. s3control CreateJob(AccountId=<the 12-digit account>, Operation={S3PutObjectTagging:{...}}, Manifest={...}, Report={...}, Priority=1, RoleArn=<role>).
3. Observe that the SDK addresses "<AccountId>.<emulator-endpoint-host>" per the operation's @endpoint hostPrefix "{AccountId}.", that the emulator serves no address for that host, and that the request is refused before transmission (no status, no body).
