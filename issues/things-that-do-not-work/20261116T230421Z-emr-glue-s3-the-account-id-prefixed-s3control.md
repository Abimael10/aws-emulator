Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.iam-role-trust-s3-service-assumed-for-batch-op-positive emr__glue__s3.s3-batch-operation-role-missing-s3-trust-rejected
Issue-Key: emr__glue__s3.iam-role-trust-s3-service-assumed-for-batch-op-positive|_create_job#16|EgressRefused|return s3control.create_job(
Issue-Key: emr__glue__s3.s3-batch-operation-role-missing-s3-trust-rejected|_submit_job#10|EgressRefused|job_id = s3control.create_job(
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# emr__glue__s3: the account-ID-prefixed s3control host is not served, so both S3 Batch Operations checks never reach the emulator

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateJob

## Description
Cause 1 of 1: every AccountId-bearing S3 Control operation is addressed at an account-ID-prefixed host, and the emulator serves no address for that host, so both S3 Batch Operations checks are refused before a byte is sent.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml, endpointPrefix 's3-control', sigv4Name 's3'): CreateJob, POST /v20180820/jobs, input member AccountId {httpHeader 'x-amz-account-id', hostLabel: true, pattern ^\d{12}$, required}, operation trait endpoint{hostPrefix: '{AccountId}.'}; DescribeJob and UpdateJobStatus carry the same hostPrefix. The SDK prepends '<AccountId>.' to the configured endpoint authority and the service answers there — a well-formed job answers 200 with a JobId, and a job whose role does not trust the S3 Batch Operations service principal is refused by the service rather than by the transport.

Emulator: the emulator is reached at 127.0.0.1 and offers no address for the prefixed host, so neither call leaves the runner: 'CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1' / 'egress refused: 000000000000.127.0.0.1 is not the endpoint under test (127.0.0.1) ... the emulator announces an address other than the endpoint it is reached on'. No status, no body, for both the positive and the negative arm.

Why: S3 Batch Operations is entirely unreachable, so a client can neither run a job nor observe the refusal AWS gives for a role missing the s3/batchoperations trust — a connection failure is indistinguishable from any service answer.

Fix: the emulator's endpoint/host handling for the s3control service — the address it announces must also answer the '{AccountId}.' prefixed authority (serve the host alias / strip the leading 12-digit account label when routing), instead of only the bare endpoint host.

Traces: emr__glue__s3.iam-role-trust-s3-service-assumed-for-batch-op-positive, emr__glue__s3.s3-batch-operation-role-missing-s3-trust-rejected.

## Evidence
Both checks fail identically, before any service response:
- emr__glue__s3.iam-role-trust-s3-service-assumed-for-batch-op-positive -> Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1 / simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1) ...
- emr__glue__s3.s3-batch-operation-role-missing-s3-trust-rejected -> same error and traceback (egress.py refuse()).
Expected: CreateJob 200 + JobId and a terminal DescribeJob status for the trusted role; a service-level refusal for the untrusted role. Actual: no HTTP exchange at all — the account-prefixed authority the SDK builds from the operation's endpoint hostPrefix is not served.

## Steps to Reproduce
1. Replay the behavioral suite and run emr__glue__s3.iam-role-trust-s3-service-assumed-for-batch-op-positive and emr__glue__s3.s3-batch-operation-role-missing-s3-trust-rejected.
2. Minimal reproduction: with an SDK pointed at the emulator, call s3control CreateJob(AccountId='<12-digit account>', ...). The SDK addresses '<AccountId>.<emulator-host>' per the operation's endpoint hostPrefix '{AccountId}.'; the emulator serves no address for that host and the request is refused before transmission.
