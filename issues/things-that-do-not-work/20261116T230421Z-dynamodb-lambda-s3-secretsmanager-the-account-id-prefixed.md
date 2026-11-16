Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__s3__secretsmanager.deploy-cfn-role-trusts-s3-service-for-batch-operations
Issue-Key: dynamodb__lambda__s3__secretsmanager.deploy-cfn-role-trusts-s3-service-for-batch-operations|_create_job#19|EgressRefused|resp = s3control.create_job(
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# dynamodb__lambda__s3__secretsmanager: the account-ID-prefixed s3control host is not served, so the S3 Batch Operations job in the CFN role-trust check is unreachable

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateJob

## Description
Cause 1 of 1: every AccountId-bearing S3 Control operation is addressed at an account-ID-prefixed host, and the emulator serves no address for that host, so the S3 Batch Operations call is refused before a byte is sent.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml, endpointPrefix 's3-control', sigv4Name 's3'): CreateJob, POST /v20180820/jobs, input member AccountId {httpHeader 'x-amz-account-id', hostLabel: true, pattern ^\d{12}$, required}, operation trait endpoint{hostPrefix: '{AccountId}.'}; DescribeJob carries the same hostPrefix. The SDK prepends '<AccountId>.' to the configured endpoint authority and the service answers there, CreateJob returning 200 with JobId.

Emulator: the emulator is reached at 127.0.0.1 and offers no address for the prefixed host, so the call never leaves the runner: 'CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1' / 'egress refused: 000000000000.127.0.0.1 is not the endpoint under test (127.0.0.1) ... the emulator announces an address other than the endpoint it is reached on'. No status, no body.

Why: a CloudFormation-provisioned role whose purpose is to be assumed by S3 Batch Operations can never be exercised: the client gets a connection failure instead of a CreateJob response, so neither the job nor the per-object tagging it performs is observable.

Fix: the emulator's endpoint/host handling for the s3control service — the address it announces must also answer the '{AccountId}.' prefixed authority (serve the host alias / strip the leading 12-digit account label when routing), instead of only the bare endpoint host.

Traces: dynamodb__lambda__s3__secretsmanager.deploy-cfn-role-trusts-s3-service-for-batch-operations (the check's s3control.create_job(AccountId=<account>, ...) is refused at the prefixed host).

## Evidence
dynamodb__lambda__s3__secretsmanager.deploy-cfn-role-trusts-s3-service-for-batch-operations ->
Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
  simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on
Expected: CreateJob 200 with a JobId and DescribeJob reporting its status, using the stack-provisioned role that trusts the S3 Batch Operations service principal. Actual: no HTTP exchange at all — the account-prefixed authority the SDK builds from the operation's endpoint hostPrefix is not served.

## Steps to Reproduce
1. Replay the behavioral suite and run dynamodb__lambda__s3__secretsmanager.deploy-cfn-role-trusts-s3-service-for-batch-operations.
2. Minimal reproduction: with an SDK pointed at the emulator, call s3control CreateJob(AccountId='<12-digit account>', ...). The SDK addresses '<AccountId>.<emulator-host>' per the operation's endpoint hostPrefix '{AccountId}.'; the emulator serves no address for that host and the request is refused before transmission.
