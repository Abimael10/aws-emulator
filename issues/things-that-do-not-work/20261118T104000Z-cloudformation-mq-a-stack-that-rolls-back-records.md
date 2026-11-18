Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified
Issue-Key: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified|failed_reason#13|AssertionError|assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
Issue-Commit: 7f36b0d7bb42ff1fae0d29d904c044c9055c6237

# cloudformation__mq: a stack that rolls back records no CREATE_FAILED event row for the resource whose create failed, so DescribeStackEvents carries no resource-handler failure reason

> _Note: this issue was identified against the code base at commit `7f36b0d7bb42ff1fae0d29d904c044c9055c6237`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
ONE net-new root cause (Cause 1 of 1), grounded in the failing check cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified.

Cause 1 of 1 — a CloudFormation stack whose resource create is refused by the resource handler now reaches ROLLBACK_COMPLETE, but DescribeStackEvents contains no CREATE_FAILED row for the failing logical resource, so the reason the create failed is nowhere in the stack's event stream.
AWS: Smithy (cloudformation) models DescribeStackEvents (StackEvents of StackEvent) with members LogicalResourceId, ResourceStatus (ResourceStatus enum including CREATE_FAILED) and ResourceStatusReason. Recorded behavior: when a resource's create is refused by its resource handler, CloudFormation writes exactly ONE event row for that logical resource with ResourceStatus CREATE_FAILED whose ResourceStatusReason is the handler's wrapped service error, of the fixed shape 'Resource handler returned message: "<the service error message> (Service: <SdkId>, Status Code: <status>, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: <code>)' — the quoted sentence, "(Service: ", "Status Code: ", "Request ID: ", "RequestToken: " and "HandlerErrorCode: " are fixed text; the service message, the SDK id (here Mq), the status (here 400), the handler error code (here InvalidRequest) and the per-run request id/token are values from the refused call. The stack then ends ROLLBACK_COMPLETE and the refusal is attributable to the one resource that caused it. In this scenario the refused resource is an AWS::AmazonMQ::Broker declaring an EngineVersion Amazon MQ does not offer, and the refusal must come from the resource handler, not from CloudFormation's pre-provisioning validation (the check asserts there are no VALIDATION_ERROR operation-event rows, and that assertion passes).
Emulator: the stack does reach ROLLBACK_COMPLETE and no early-validation rows are emitted, but the event stream holds ZERO rows with LogicalResourceId "Broker" and ResourceStatus CREATE_FAILED — the check's helper, which collects every DescribeStackEvents row for that logical id in that status, gets []. There is therefore no ResourceStatusReason to read: the stack reports that it rolled back but not which resource failed or why.
Why: a client cannot diagnose a failed deployment. Every CloudFormation consumer — the console, `aws cloudformation describe-stack-events`, CDK/SAM deploy, CI pipelines — reads the failing resource's CREATE_FAILED row and its ResourceStatusReason to tell the user what was wrong with the template; against the emulator a rollback is opaque, and automation that greps the reason (or the HandlerErrorCode) to decide whether a failure is retryable or a user error gets nothing at all. A negative test asserting the handler's exact reason can never pass.
Fix: the stack-event recording in the cloudformation stack-operation engine — when a resource's create is refused, append a CREATE_FAILED StackEvent for that logical resource (exactly one, alongside the existing *_IN_PROGRESS rows) carrying ResourceStatusReason built by wrapping the underlying service error in the resource-handler template above (service sdkId, HTTP status and error message from the refused service call, HandlerErrorCode from the handler's classification), before the rollback rows. Hypothesis: the rollback path currently transitions the stack status without materialising per-resource failure events; if a CREATE_FAILED row is written under a different logical id or without a reason, the same row-construction site is where to correct it.
Relation to open issues: 20261115T175651Z-cloudformation-mq-mq-accepts-an-engineversion-amazon-mq.md lists this check id, but its replay keys are the earlier assertion `assert status == "ROLLBACK_COMPLETE"` (the stack then reached CREATE_COMPLETE) and the replay report does NOT mark this check as owned by it. The check has moved past that point — the unoffered EngineVersion is now refused and the stack rolls back — so what fails now is one step later and is the CloudFormation event-recording gap described here, not mq's EngineVersion validation. Also open and not covering it: 20261109T143402Z-cloudformation-mq-stack-outputs-for-aws-amazonmq-configuration.md (Outputs/Ref values for the configuration resource).
Traces: cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 22/31 passed (71.0%); service cloudformation__mq 0/3 (0%), 3 failed. Two of those three are marked OWNED (20261115T175651Z-... and 20261109T143402Z-...); the one below is marked neither ALREADY FILED nor OWNED.

cloudformation__mq.deploy-amazonmq-broker-engine-version-rejected-then-config-association-verified [mismatch]:
  File "check_cloudformation__mq_deploy_amazonmq_broker_engine_version_rejected_then_config_association_verified.py", line 314, in run
    reason = failed_reason(stack_events(cfn, bad_id), "Broker")
  line 134, in failed_reason
    assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
  AssertionError: expected one CREATE_FAILED row for Broker, got []

Assertions that PASSED before it, which locate the divergence precisely:
  - wait_stack(...) == "ROLLBACK_COMPLETE": the stack did roll back.
  - validation_rows(cfn, bad_id) == []: no VALIDATION_ERROR operation events, i.e. the refusal was not an early template validation.

Expected (AWS): DescribeStackEvents for the rolled-back stack contains exactly one row with LogicalResourceId "Broker" and ResourceStatus CREATE_FAILED, whose ResourceStatusReason matches 'Resource handler returned message: "<service message> (Service: Mq, Status Code: 400, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)' — the check then compares the extracted (message, service, status, handler) tuple against ("Broker engine version [<unoffered version>] is invalid for broker engine type [ACTIVEMQ]. Valid values: [<the versions DescribeBrokerEngineTypes offers, newest first>].", "Mq", "400", "InvalidRequest"). The bracketed version, the engine type and the version list are values from the request and the service catalog; the request id and request token are per-run.
Actual (emulator): no CREATE_FAILED row for that logical resource exists at all (empty list), so no reason is available to compare.

Template used for the refused stack: VPC + Subnet + SecurityGroup + AWS::AmazonMQ::Configuration + AWS::AmazonMQ::Broker {EngineType: ACTIVEMQ, EngineVersion: a version not in DescribeBrokerEngineTypes' ACTIVEMQ list, DeploymentMode: SINGLE_INSTANCE, HostInstanceType: mq.t3.micro, PubliclyAccessible: false} + AWS::AmazonMQ::ConfigurationAssociation.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from the checkout, and replay the behavioral suite (simparity-behavioral-aws-v1), service cloudformation__mq.
2. mq DescribeBrokerEngineTypes(EngineType="ACTIVEMQ") and pick an EngineVersion that is NOT in the offered list (the check uses "99.99" as a stand-in for any unoffered version).
3. CreateStack with a template declaring a VPC, a subnet, a security group, an AWS::AmazonMQ::Configuration (base64 ActiveMQ broker XML in Data), an AWS::AmazonMQ::Broker with that unoffered EngineVersion (EngineType ACTIVEMQ, SINGLE_INSTANCE, mq.t3.micro, PubliclyAccessible false, one user whose password comes from a NoEcho parameter) and an AWS::AmazonMQ::ConfigurationAssociation.
4. Poll DescribeStacks(StackName=<StackId>) until a terminal status; it reaches ROLLBACK_COMPLETE.
5. DescribeEvents(StackName=<StackId>): there must be no EventType VALIDATION_ERROR rows (the refusal is the resource handler's, not early validation). This holds.
6. Paginate DescribeStackEvents(StackName=<StackId>) and select rows with LogicalResourceId "Broker" and ResourceStatus "CREATE_FAILED". Expected: exactly one row, whose ResourceStatusReason is 'Resource handler returned message: "Broker engine version [<the unoffered version>] is invalid for broker engine type [ACTIVEMQ]. Valid values: [<offered ACTIVEMQ versions>]." (Service: Mq, Status Code: 400, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'. Observed: no such row (empty list), so the stack's rollback carries no failure reason for any resource.
7. Control (the same template with the newest offered version) deploys to CREATE_COMPLETE, confirming the template and the rest of the flow are serviceable.
