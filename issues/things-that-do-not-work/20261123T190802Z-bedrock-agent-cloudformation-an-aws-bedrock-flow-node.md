Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected
Issue-Key: bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected|run#18|AssertionError|assert rows == [
Issue-Commit: 39248dbcbc494eaef95a3174aa24756222042862

# bedrock-agent__cloudformation: an AWS::Bedrock::Flow node Type outside the resource schema's enum rolls the stack back but DescribeEvents emits no VALIDATION_ERROR OperationEvent

> _Note: this issue was identified against the code base at commit `39248dbcbc494eaef95a3174aa24756222042862`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
One net-new root cause in CloudFormation's early property-validation reporting, observed on an AWS::Bedrock::Flow whose Definition.Nodes[*].Type is outside the resource type's node-type enum.

Cause 1 of 1: the stack is correctly refused and rolled back and no flow is left behind, but the operation event that records WHY is never produced, so DescribeEvents reports no validation error for the rejected nested property.

AWS: Smithy (com.amazonaws.cloudformation) — DescribeEvents takes StackName/ChangeSetName/OperationId/Filters/NextToken and returns `OperationEvents: list<OperationEvent>`, paginated on NextToken with items=OperationEvents. OperationEvent.EventType is an enum with values STACK_EVENT, PROGRESS_EVENT, VALIDATION_ERROR, PROVISIONING_ERROR, HOOK_INVOCATION_ERROR, and OperationEvent carries the validation members ValidationName (string), ValidationStatus (enum FAILED|SKIPPED), ValidationFailureMode (HookFailureMode, enum FAIL|WARN), ValidationStatusReason (string) and ValidationPath (string) beside LogicalResourceId. When a template property value — including one nested inside a structured property such as a Flow Definition's node list — is outside the resource type's schema enum, AWS fails the operation in property validation and records exactly ONE VALIDATION_ERROR OperationEvent for that resource: LogicalResourceId = the template's logical id, ValidationName = PROPERTY_VALIDATION, ValidationStatus = FAILED, ValidationFailureMode = FAIL, ValidationPath = the JSON pointer of the offending member within the template (/Resources/<LogicalId>/Properties/<...path to the member...>, with list elements addressed by zero-based index), and ValidationStatusReason = "<rejected value> is not a valid enum value. Supported values: [<the resource schema's permitted values, comma-separated>]". In that message the fixed text is " is not a valid enum value. Supported values: [" and the closing "]"; the variable parts are the rejected value taken from the submitted template, the permitted-value list taken from the resource type's schema, and the logical id plus member path in ValidationPath. For the template this check submits (a node named FlowOutputNode, the second entry of Definition.Nodes, with Type set to a value outside the enum) that reads as the single row ('FlowResource', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', '<rejected type> is not a valid enum value. Supported values: [Input, Output, KnowledgeBase, Condition, Lex, Prompt, LambdaFunction, Agent, Storage, Retrieval, Iterator, Collector, InlineCode, Loop, LoopInput, LoopController]', '/Resources/FlowResource/Properties/Definition/Nodes/1/Type').

Emulator: the refusal itself is right — the stack reaches ROLLBACK_COMPLETE (the preceding assertion passed) — but DescribeEvents(StackName=<stack id>), paged over all OperationEvents, yields NO event whose EventType is VALIDATION_ERROR. The projection of (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath) over those events is the empty list [] where AWS returns the single row above. The property-schema rejection exists only as a rollback, never as an addressable, machine-readable event, and the node-type enum of the AWS::Bedrock::Flow resource schema is therefore never named.

Why: a client diagnosing a refused deployment reads the VALIDATION_ERROR rows of DescribeEvents to learn WHICH member of WHICH resource was wrong and which values are allowed — that is the entire purpose of the validation members. Against the emulator it gets a bare ROLLBACK_COMPLETE with no path, no reason and no failure mode; it cannot distinguish a template property-schema rejection from a downstream Bedrock provisioning failure, and any CI gate, console view or deployment tool that renders those rows renders nothing.

Fix: CloudFormation's template/property-validation stage and the operation-event store DescribeEvents reads — the validation stage must emit a VALIDATION_ERROR OperationEvent (ValidationName/ValidationStatus/ValidationFailureMode/ValidationStatusReason/ValidationPath populated as above) when a resource property value, at any depth inside a structured property, is outside its resource-type schema enum; and the AWS::Bedrock::Flow resource schema must declare the permitted node Type values so the reason can enumerate them and the path can address the offending list element.

Related but not covering this check: open issues 20261123T150355Z-cloudformation-ec2-iam-sagemaker-an-aws-sagemaker-domain.md, 20261123T033806Z-cloudformation-redshift-an-aws-redshift-eventsubscription-with-a.md and 20261122T043035Z-cloudformation-rekognition-a-duplicate-custom-named-aws-rekognition.md describe the same missing-operation-event shape under other service headings and for top-level (non-nested) properties; none carries this check's replay key, and the suite report lists this check as neither already filed nor owned.

Traces: bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected.

## Evidence
Expected (AWS): the VALIDATION_ERROR OperationEvents of the rolled-back stack project to exactly one row
  [('FlowResource', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', '<rejected node type> is not a valid enum value. Supported values: [Input, Output, KnowledgeBase, Condition, Lex, Prompt, LambdaFunction, Agent, Storage, Retrieval, Iterator, Collector, InlineCode, Loop, LoopInput, LoopController]', '/Resources/FlowResource/Properties/Definition/Nodes/1/Type')]
(the rejected value is the one the submitted template carried; the supported-value list and the path come from the resource schema and the template, not from any one run).

Actual (emulator): [] — DescribeEvents returns no OperationEvent with EventType == 'VALIDATION_ERROR'. Replay output:
  [mismatch] bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected
  File "check_bedrock_agent__cloudformation_deploy_flow_node_type_enum_rejected.py", line 211, in run
    assert rows == [
  AssertionError: validation rows []

The preceding assertions in the same check passed: the positive-control stack (same template, a valid node Type) reached CREATE_COMPLETE and its flow is listed and gettable with the Ref/GetAtt ids, and the rejecting stack reached ROLLBACK_COMPLETE. So the refusal, the rollback and the control deploy are all right, and only the VALIDATION_ERROR operation event is missing.

## Steps to Reproduce
1. CreateStack (Capabilities=[CAPABILITY_IAM]) with an AWS::IAM::Role for bedrock.amazonaws.com and an AWS::Bedrock::Flow whose Definition.Nodes holds an Input node and a second node whose Type is a value outside the resource's node-type enum (e.g. "BogusType"), connected by one Data connection.
2. Poll DescribeStacks(StackName=<StackId>) until terminal — the status is ROLLBACK_COMPLETE, as AWS does.
3. Call cloudformation:DescribeEvents(StackName=<StackId>), page all OperationEvents and keep those with EventType == 'VALIDATION_ERROR'.
4. Observe there are none. On AWS there is exactly one, with LogicalResourceId=FlowResource, ValidationName=PROPERTY_VALIDATION, ValidationStatus=FAILED, ValidationFailureMode=FAIL, ValidationPath=/Resources/FlowResource/Properties/Definition/Nodes/1/Type and ValidationStatusReason naming the rejected value and the permitted node types.
5. For contrast, deploy the same template with the node Type set to "Output": the stack reaches CREATE_COMPLETE and the flow is listed by bedrock-agent:ListFlows (this leg already passes).
