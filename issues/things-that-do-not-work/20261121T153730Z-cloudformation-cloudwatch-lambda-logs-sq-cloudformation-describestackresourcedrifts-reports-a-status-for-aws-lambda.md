Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__lambda__logs__sqs__ssm.cfn-change-set-queue-worker-update-workflow
Issue-Key: cloudformation__cloudwatch__lambda__logs__sqs__ssm.cfn-change-set-queue-worker-update-workflow|_assert_drift_reports_out_of_band_write#25|AssertionError|assert esm is None, (
Issue-Commit: b61d91764c5b64113b88eda248364bbad90afe06

# cloudformation: DescribeStackResourceDrifts reports a status for AWS::Lambda::EventSourceMapping, a resource type that does not support drift detection (and calls it IN_SYNC while Expected/ActualProperties differ)

> _Note: this issue was identified against the code base at commit `b61d91764c5b64113b88eda248364bbad90afe06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResourceDrifts

## Description
One net-new root cause in CloudFormation's drift-detection resource coverage.

Cause 1 of 1: after DetectStackDrift completes on a stack that contains an AWS::Lambda::EventSourceMapping, DescribeStackResourceDrifts returns a StackResourceDrifts entry for that mapping. AWS does not.

AWS: cloudformation DetectStackDrift only examines resource types that support drift detection; a resource of an unsupported type is not drift-checked, and DescribeStackResourceDrifts does not surface it. The model states this directly: DescribeStackResourceDriftsInput.StackResourceDriftStatusFilters documents the returned statuses as DELETED / MODIFIED / IN_SYNC / UNKNOWN and says of NOT_CHECKED "CloudFormation doesn't currently return this value" (Smithy: cloudformation DescribeStackResourceDrifts, input member StackResourceDriftStatusFilters docs; enum StackResourceDriftStatus values IN_SYNC / MODIFIED / DELETED / NOT_CHECKED / UNKNOWN / UNSUPPORTED). AWS::Lambda::EventSourceMapping is not a drift-supported type, so the correct result set for such a stack contains the supported resources only (here the SSM parameter MODIFIED after an out-of-band PutParameter, and the SQS queue IN_SYNC) and no entry at all whose LogicalResourceId is the mapping's. Beyond the omission itself, AWS never reports IN_SYNC for an entry whose ExpectedProperties and ActualProperties disagree: IN_SYNC means "the resource's actual configuration matches its expected template configuration", and a difference is reported as MODIFIED with a PropertyDifferences list.

Emulator: the drift results include an extra entry for the event-source-mapping logical id, carrying ResourceType 'AWS::Lambda::EventSourceMapping', a PhysicalResourceId (the mapping UUID), StackResourceDriftStatus 'IN_SYNC', and an ExpectedProperties/ActualProperties pair that do not even agree with each other — ExpectedProperties holds the template's {EventSourceArn, FunctionName, BatchSize, Enabled} while ActualProperties holds {BatchSize, EventSourceArn, FunctionResponseTypes, MaximumBatchingWindowInSeconds} (FunctionName and Enabled dropped, two unrequested members added), with no PropertyDifferences. So the generic drift reader is being run over a type it does not model, and its output is then labelled in-sync regardless.

Why: a client that reads DescribeStackResourceDrifts to enumerate what CloudFormation could and could not verify is told the mapping was checked and is in sync. Drift tooling (CDK diff-style reconciliation, compliance sweeps, 'is this stack still what the template says' gates) therefore treats an unverifiable resource as verified, and the inverse never shows up: a real out-of-band change to the mapping can never be reported because the entry is hard-labelled IN_SYNC. The self-contradicting Expected/Actual pair also makes any client that diffs those two JSON blobs itself (rather than trusting the status) report drift on a resource AWS says nothing about.

Fix: the cloudformation drift-detection resource coverage — the per-resource-type drift reader registry must classify AWS::Lambda::EventSourceMapping as not drift-supported and exclude it from DetectStackDrift's checked set and from the DescribeStackResourceDrifts result list, instead of falling through to a generic reader. The same registry must not emit IN_SYNC for an entry whose ActualProperties differ from its ExpectedProperties: for a supported type that is MODIFIED with PropertyDifferences. Note the related but distinct open issue 20261117T134237Z-cloudformation-lambda-3-causes-aws-lambda-version-is-absent-from.md, which covers the opposite defect for a different type (AWS::Lambda::Version, a drift-SUPPORTED type, being absent from the results); it does not cover an unsupported type being present.

Traces: cloudformation__cloudwatch__lambda__logs__sqs__ssm.cfn-change-set-queue-worker-update-workflow (leg 'drift').

## Evidence
Failing check: cloudformation__cloudwatch__lambda__logs__sqs__ssm.cfn-change-set-queue-worker-update-workflow, leg 'drift' (_assert_drift_reports_out_of_band_write).

Expected (AWS): DescribeStackResourceDrifts after a completed DetectStackDrift returns an entry for the SSM parameter resource with StackResourceDriftStatus MODIFIED and a PropertyDifferences entry on /Value (DifferenceType NOT_EQUAL), an entry for the queue resource with StackResourceDriftStatus IN_SYNC, and NO entry whose LogicalResourceId is the event-source mapping, because AWS::Lambda::EventSourceMapping does not support drift detection.

Actual (emulator), verbatim from the replay:
  AssertionError: AWS::Lambda::EventSourceMapping does not support drift detection; CloudFormation must omit it from describe_stack_resource_drifts entirely rather than reporting any status for it, got {'StackId': 'arn:aws:cloudformation:<region>:<account>:stack/<stack-name>/<stack-uuid>', 'LogicalResourceId': 'WorkerEsm', 'PhysicalResourceId': '<esm-uuid>', 'ResourceType': 'AWS::Lambda::EventSourceMapping', 'ExpectedProperties': '{"EventSourceArn":"arn:aws:sqs:<region>:<account>:<queue-name>","FunctionName":"<function-name>","BatchSize":1,"Enabled":true}', 'ActualProperties': '{"BatchSize":1,"EventSourceArn":"arn:aws:sqs:<region>:<account>:<queue-name>","FunctionResponseTypes":[],"MaximumBatchingWindowInSeconds":0}', 'StackResourceDriftStatus': 'IN_SYNC', 'Timestamp': <timestamp>}

(The stack id/uuid, mapping uuid, generated queue and function names, region, account and timestamp are per-run values and are not part of the requirement. The requirement is: no StackResourceDrifts entry of ResourceType AWS::Lambda::EventSourceMapping at all; and, for any entry that is returned, StackResourceDriftStatus IN_SYNC only when ActualProperties equals ExpectedProperties.)

The two property blobs returned for the mapping also disagree with each other — FunctionName and Enabled present in ExpectedProperties are absent from ActualProperties, and FunctionResponseTypes/MaximumBatchingWindowInSeconds appear only in ActualProperties — while the status is still IN_SYNC and PropertyDifferences is absent.

## Steps to Reproduce
1. CreateStack with a template declaring: an AWS::SQS::Queue, an AWS::Lambda::Function worker, an AWS::SSM::Parameter (String) the worker reads, and an AWS::Lambda::EventSourceMapping ('WorkerEsm') binding the queue to the function with BatchSize 1 and Enabled true. Wait for CREATE_COMPLETE (the check reaches this leg after a change-set update, but a plain create is enough).
2. Change one resource out of band: ssm PutParameter on the stack's parameter with Overwrite=true and a new Value.
3. cloudformation DetectStackDrift on the stack, then poll DescribeStackDriftDetectionStatus until DetectionStatus is DETECTION_COMPLETE.
4. cloudformation DescribeStackResourceDrifts on the stack (no StackResourceDriftStatusFilters), paging NextToken to collect every entry.
5. Observed: the result list contains an entry with LogicalResourceId 'WorkerEsm', ResourceType 'AWS::Lambda::EventSourceMapping' and StackResourceDriftStatus 'IN_SYNC', whose ExpectedProperties and ActualProperties differ and which carries no PropertyDifferences.
   Expected: no entry for that logical id at all (AWS::Lambda::EventSourceMapping does not support drift detection), with only the SSM parameter (MODIFIED, PropertyDifferences on /Value with DifferenceType NOT_EQUAL) and the queue (IN_SYNC) present.
