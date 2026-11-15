Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs__ssm.cdk-destroy-deletes-a-create-failed-stack-and-its-live-resources cloudformation__sqs__ssm.ec2mr-cfnres-lookalike-physical-id-rejected-by-owning-service cloudformation__ec2__sqs__ssm.cfn-yaml-short-form-intrinsics-shape-resources
Issue-Key: cloudformation__sqs__ssm.cdk-destroy-deletes-a-create-failed-stack-and-its-live-resources|run#28|AssertionError|assert gone == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudformation__sqs__ssm.ec2mr-cfnres-lookalike-physical-id-rejected-by-owning-service|run#19|AssertionError|assert got == ("AWS.SimpleQueueService.NonExistentQueue",
Issue-Key: sqs|GetQueueAttributes|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# cloudformation__sqs__ssm: SQS's QueueDoesNotExist refusal keeps the legacy WSDL wording ("...does not exist for this wsdl version."), so no CloudFormation/CDK delete can be verified against SQS

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueAttributes

## Description
Cause 1 of 1: every SQS operation that fails to resolve a queue URL answers the pre-2023 query/WSDL-era not-found sentence instead of AWS's current QueueDoesNotExist message. Failing checks: cloudformation__sqs__ssm.cdk-destroy-deletes-a-create-failed-stack-and-its-live-resources, cloudformation__sqs__ssm.ec2mr-cfnres-lookalike-physical-id-rejected-by-owning-service, and cloudformation__ec2__sqs__ssm.cfn-yaml-short-form-intrinsics-shape-resources (that last check is listed by the suite under the heading `cloudformation__ec2__sqs__ssm`; it is the SAME root cause and the same sentence, so it is bundled here rather than filed twice). All three are CloudFormation/CDK flows that delete (or mutate one character of) a queue's PhysicalResourceId and then confirm the ground truth directly against SQS; each one receives the correct HTTP status and error code and the wrong message, so the flow fails on the message assertion only.

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The model does not carry the message string, so for the text this is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` That entire sentence is FIXED text — this error class interpolates NOTHING: no queue name, queue URL, account id or request id appears in it. In particular the trailing ` for this wsdl version` is AWS's retired query-API wording and is never emitted by current SQS.

Emulator: the status (400) and the code (`AWS.SimpleQueueService.NonExistentQueue`) are right, but the message is `The specified queue does not exist for this wsdl version.` The wording is identical across the three checks and across the operations they exercise (GetQueueAttributes on a queue whose stack was deleted, GetQueueAttributes on a one-character mutation of a live queue URL, GetQueueAttributes after a `cdk destroy` of a CREATE_FAILED stack), so it is one shared error constructor rather than a per-operation slip.

Why:      the error message is part of the contract for this error class. Clients that assert on, log, or pattern-match SQS's not-found sentence — CloudFormation/CDK integration suites that use native SQS as the ground truth that a delete really took effect, negative-path tests that distinguish "gone" from "other failure", and deletion-verification tooling — see a sentence AWS never produces, so an exact-match confirmation that the queue is gone fails even though the queue really is gone. It also misleads a reader into believing the emulator is serving a legacy SQS API version.

Fix:      the SQS service's shared QueueDoesNotExist / NonExistentQueue error construction — the single place the queue-resolution path for GetQueueAttributes, SetQueueAttributes, SendMessage, ReceiveMessage, DeleteMessage and GetQueueUrl builds this refusal — must emit the fixed sentence `The specified queue does not exist.` with no suffix, leaving status 400 and code `AWS.SimpleQueueService.NonExistentQueue` unchanged.

Related open issue: 20261115T161642Z-cloudcontrol-sqs-the-queuedoesnotexist-refusal-message-is-the-legacy.md describes the same wording defect but carries only Cloud Control replay keys (cloudcontrol.* checks); the suite did NOT mark these three CloudFormation checks as owned by it, so they are filed here with their own replay keys. 20261115T161642Z-cloudcontrol-sqs-setqueueattributes-on-a-deleted-queue-is.md is the SetQueueAttributes face of the same constructor. A fix to the shared constructor should close all of them together.

Traces:   the three failing check ids in trace_ids; each asserts the exact refusal message after a CloudFormation/CDK delete or a near-miss queue URL.

## Evidence
cloudformation__sqs__ssm.cdk-destroy-deletes-a-create-failed-stack-and-its-live-resources (check line 64):
  AssertionError: the queue the failed create left live answers ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.') after `cdk destroy`
  expected ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.')

cloudformation__sqs__ssm.ec2mr-cfnres-lookalike-physical-id-rejected-by-owning-service (check line 60):
  AssertionError: SQS answered ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.') for the near-miss URL <queue URL with its final character mutated>; real AWS raises ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist.")

cloudformation__ec2__sqs__ssm.cfn-yaml-short-form-intrinsics-shape-resources (check line 190/250):
  AssertionError: get_queue_attributes on the queue after stack deletion: expected message 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.'

Expected in all cases: HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' — wholly fixed text, no values taken from the request or from state.
Actual in all cases: HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist for this wsdl version.'

## Steps to Reproduce
1. Deploy a CloudFormation (or CDK) stack containing an AWS::SQS::Queue and read its PhysicalResourceId with DescribeStackResources — that id is the queue URL.
2. Confirm the positive control: `aws sqs get-queue-attributes --queue-url <url> --attribute-names QueueArn` succeeds.
3. Make the queue unresolvable, either by deleting the owning stack (or `cdk destroy`, including of a stack parked in CREATE_FAILED by --no-rollback), or by mutating exactly one character of the final segment of the queue URL.
4. Call `aws sqs get-queue-attributes --queue-url <deleted-or-mutated-url> --attribute-names QueueArn`.
5. Observe the refusal: status 400 and code AWS.SimpleQueueService.NonExistentQueue are correct, but Message is 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.'
6. The same wrong sentence appears from receive-message, send-message, set-queue-attributes and get-queue-url against the same missing queue.
