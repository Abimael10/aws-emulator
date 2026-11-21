Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-empty-root-group-rejected cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-round-trip-and-delete
Issue-Key: cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-empty-root-group-rejected|run#15|AssertionError|assert stack_failed == [_STACK_REASON], (
Issue-Key: cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-round-trip-and-delete|run#25|AssertionError|assert leaf_test == {"id": "MQTT_Connect", "testCase": None, "version": "0.0.0"}, (
Issue-Commit: 4f8289a55da0569c35b4094a258c497c237277f1

# cloudformation__iam__iotdeviceadvisor: a property-validation rollback records no stack CREATE_FAILED reason, and the Device Advisor suite's rootGroup round-trip drops the normalized testCase member

> _Note: this issue was identified against the code base at commit `4f8289a55da0569c35b4094a258c497c237277f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two net-new causes on the AWS::IoTCoreDeviceAdvisor::SuiteDefinition deploy path. The resource type is now recognized and does provision (the resolved issue 20261109T113659Z-cloudformation-iam-iotdeviceadvisor-...-is-rejected-as-an-unrecognized-resource-type covered that, and neither cause below is a refusal), but the rejection's bookkeeping and the suite's read-back still diverge.

Cause 1 of 2: when a template property fails CloudFormation's pre-provision validation, the stack rolls back but its stack-level CREATE_FAILED event carries NO ResourceStatusReason, so nothing in DescribeStackEvents says why the create failed.
AWS: recorded behavior only (CloudFormation does not model stack-event text). On a property-validation failure CloudFormation emits one CREATE_FAILED event whose LogicalResourceId is the stack name and whose ResourceStatusReason is "Validation failed with <N> error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation." — fixed text apart from <N>, the number of validation errors found in this template (1 in this check). The per-error detail is then served by DescribeEvents as an OperationEvents row with EventType=VALIDATION_ERROR: for this template LogicalResourceId=Suite, ValidationName=PROPERTY_VALIDATION, ValidationStatus=FAILED, ValidationFailureMode=FAIL, ValidationStatusReason="#/SuiteDefinitionConfiguration/RootGroup: expected minLength: 1, actual: 0", ValidationPath="/Resources/Suite/Properties/SuiteDefinitionConfiguration/RootGroup" (the member path and the two numbers come from the template and the member's modelled length; rootGroup is RootGroup with @length min 0 max 2048 and is required, and a non-qualification suite requires a non-empty value).
Emulator: the stack does reach ROLLBACK_COMPLETE and provisions nothing, but the stack-level CREATE_FAILED events have no reason at all — the replay reports "the stack's CREATE_FAILED reasons were []". Because the run stops there, the DescribeEvents VALIDATION_ERROR row above is unverified and must be produced too.
Why: an operator or CI job that reads DescribeStackEvents sees a stack that failed with an empty reason, so there is no signal that the failure was template validation (as opposed to a provisioning error) and no pointer to DescribeEvents; tooling that parses the reason to decide whether to retry or to surface a user error gets nothing to parse.
Fix: the pre-provision template-validation path of the CreateStack handler and the stack-event writer it feeds in the CloudFormation crate — the stack-level CREATE_FAILED event must carry the "Validation failed with <N> error(s)..." reason, and each failed property must be recorded as a VALIDATION_ERROR operation event with its ValidationName/Status/FailureMode/StatusReason/Path.
Traces: cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-empty-root-group-rejected.

Cause 2 of 2: GetSuiteDefinition echoes the rootGroup document exactly as it was submitted instead of returning Device Advisor's normalized form, so the leaf test object is missing its testCase member.
AWS: recorded behavior only — Smithy models rootGroup as an opaque string (SuiteDefinitionConfiguration.rootGroup -> RootGroup, @required, @length 0..2048), so the shape of the JSON inside it is not modelled. Device Advisor re-serializes the document it stores: the leaf test block comes back with every member present, the caller-supplied id and version plus an explicit "testCase": null when the caller named no test case, i.e. {"id": "<test id>", "testCase": null, "version": "<version>"}.
Emulator: returns the submitted text verbatim, so the leaf test reads {"id": "MQTT_Connect", "version": "0.0.0"} with the testCase key absent, and a client that indexes test["testCase"] raises KeyError where AWS gives None.
Why: the round-trip is not faithful — a client comparing what it declared against what Device Advisor reports (exactly what CloudFormation drift and this check do) sees a different document, and the absent key is a hard failure for consumers that read the member rather than a value difference.
Fix: the suite-definition state behind CreateSuiteDefinition / GetSuiteDefinition in the iotdeviceadvisor crate — normalize the rootGroup document on store (or on read) so every test object carries its full member set with testCase explicit null when unset, instead of echoing the caller's text.
Traces: cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-round-trip-and-delete.

## Evidence
Behavioral suite replay against the emulator (service heading `cloudformation__iam__iotdeviceadvisor`; neither check is marked ALREADY FILED or OWNED):

[mismatch] cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-empty-root-group-rejected
  AssertionError: the stack's CREATE_FAILED reasons were [], expected ['Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.']
  Template: one AWS::IoTCoreDeviceAdvisor::SuiteDefinition whose SuiteDefinitionConfiguration.RootGroup is the empty string. The stack correctly ends ROLLBACK_COMPLETE with no stack resources and no suite at Device Advisor; only the reason is missing. Still unverified behind that assertion: the DescribeEvents VALIDATION_ERROR row ('Suite', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', '#/SuiteDefinitionConfiguration/RootGroup: expected minLength: 1, actual: 0', '/Resources/Suite/Properties/SuiteDefinitionConfiguration/RootGroup').

[mismatch] cloudformation__iam__iotdeviceadvisor.deploy-iotcoredeviceadvisor-suitedefinition-round-trip-and-delete
  AssertionError: expected the declared MQTT_Connect test case, rootGroup carries {'id': 'MQTT_Connect', 'version': '0.0.0'}
  Expected: GetSuiteDefinition(suiteDefinitionId=<Ref of the Suite resource>).suiteDefinitionConfiguration.rootGroup, parsed, has tests[0].tests[0].test == {'id': 'MQTT_Connect', 'testCase': None, 'version': '0.0.0'}; the emulator omits the testCase member.
  Everything before that assertion matches: Ref and the Fn::GetAtt SuiteDefinitionArn / SuiteDefinitionVersion outputs agree with GetSuiteDefinition, and suiteDefinitionName / devicePermissionRoleArn / intendedForQualification round-trip.

Suite names, stack names, role ARNs and suite ids above are per-run values; the requirements are the CREATE_FAILED reason template and the normalized rootGroup test object.

## Steps to Reproduce
Cause 1:
1. CreateRole trusting iotdeviceadvisor.amazonaws.com.
2. CreateStack with a template declaring one AWS::IoTCoreDeviceAdvisor::SuiteDefinition whose SuiteDefinitionConfiguration = {SuiteDefinitionName: <name>, DevicePermissionRoleArn: <that role>, RootGroup: ""}.
3. Wait for the stack: it ends ROLLBACK_COMPLETE. DescribeStackEvents: AWS has one CREATE_FAILED event for the stack itself with ResourceStatusReason "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation."; the emulator's CREATE_FAILED event has no reason.
4. DescribeEvents(StackName=<stack id>): AWS returns an OperationEvents row with EventType=VALIDATION_ERROR naming the Suite resource, PROPERTY_VALIDATION/FAILED/FAIL, reason "#/SuiteDefinitionConfiguration/RootGroup: expected minLength: 1, actual: 0" and path "/Resources/Suite/Properties/SuiteDefinitionConfiguration/RootGroup".

Cause 2:
1. CreateStack (Capabilities=CAPABILITY_IAM) with an IAM role trusting iotdeviceadvisor.amazonaws.com plus an AWS::IoTCoreDeviceAdvisor::SuiteDefinition whose RootGroup is the JSON document {"configuration":{},"tests":[{"name":"<group>","configuration":{},"tests":[{"name":"<test>","configuration":{},"test":{"id":"MQTT_Connect","version":"0.0.0"}}]}]} and IntendedForQualification false; wait for CREATE_COMPLETE.
2. GetSuiteDefinition(suiteDefinitionId=<Ref of the Suite resource>) and parse suiteDefinitionConfiguration.rootGroup: AWS reports tests[0].tests[0].test == {"id": "MQTT_Connect", "testCase": null, "version": "0.0.0"}; the emulator reports {"id": "MQTT_Connect", "version": "0.0.0"}.
3. Re-replay both behavioral checks listed in trace_ids to confirm the two causes are closed.
