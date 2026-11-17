Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol__cloudformation__ssm.ssm-parameter-writeonly-properties-update-hidden-from-cloudcontrol
Issue-Key: cloudcontrol__cloudformation__ssm.ssm-parameter-writeonly-properties-update-hidden-from-cloudcontrol|run#19|AssertionError|assert write_only not in props_before, (
Issue-Commit: 743af1aef2870b34c6c9092ae87b31cfc860bafb

# cloudcontrol__cloudformation__ssm: Cloud Control GetResource projects the write-only AWS::SSM::Parameter properties (Description, AllowedPattern, Tier) into the resource model

> _Note: this issue was identified against the code base at commit `743af1aef2870b34c6c9092ae87b31cfc860bafb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResource

## Description
Cause 1 of 1: Cloud Control's GetResource for AWS::SSM::Parameter returns properties the resource type declares write-only, so the resource model leaks values AWS never reads back.

AWS: Cloud Control serialises ResourceDescription.Properties from the resource type's schema and omits every member listed in that schema's writeOnlyProperties. For AWS::SSM::Parameter, Description, AllowedPattern and Tier are write-only: they take effect on the parameter (SSM DescribeParameters/GetParameter show them) but never appear in the Cloud Control model, which carries only the readable members (Name, Type, Value, DataType, Policies-free view, Tags). The response stays 200; the divergence is in the body. Recorded behavior confirms the same projection before and after a stack update.

Emulator: GetResource(TypeName="AWS::SSM::Parameter", Identifier=<parameter name>) returns a Properties document that includes Description, AllowedPattern and Tier alongside the readable members, both before and after the update (the check fails on the first of the three, Description).

Why: Cloud Control's contract is that a write-only property is never observable through the control plane. A client that round-trips GetResource output into UpdateResource/CreateResource, or that diffs the model against a desired state, sees members AWS would not have returned: drift detection reports spurious equality or inequality, and a patch document built from the model can resend values the caller never meant to manage. It also leaks property values a schema deliberately hides.

Fix: the Cloud Control GetResource handler's resource-model serializer (and the shared resource-schema projection it uses for AWS::SSM::Parameter) must strip writeOnlyProperties from ResourceDescription.Properties.

Traces: behavioral check cloudcontrol__cloudformation__ssm.ssm-parameter-writeonly-properties-update-hidden-from-cloudcontrol.

## Evidence
Check: cloudcontrol__cloudformation__ssm.ssm-parameter-writeonly-properties-update-hidden-from-cloudcontrol [mismatch]

Expected (AWS): for each of ("Description", "AllowedPattern", "Tier"), the name is absent from json.loads(GetResource(...)["ResourceDescription"]["Properties"]), while Name/Type/Value are present.

Actual (emulator): "AssertionError: GetResource must never report the write-only Description, got {'Name': <param>, 'Arn': 'arn:aws:ssm:<region>:<account>:parameter/<param>', 'Type': 'String', 'Value': 'abc123', 'Tier': 'Standard', 'DataType': 'text', 'Description': 'desc-v1', 'AllowedPattern': '^[a-zA-Z0-9]+$', 'Tags': {...}}" — Description, AllowedPattern and Tier are all projected.

The properties did take effect in SSM (Description=desc-v1, AllowedPattern=^[a-zA-Z0-9]+$, Tier=Standard), so the defect is only the Cloud Control projection.

## Steps to Reproduce
1. CreateStack with one AWS::SSM::Parameter resource whose Properties set Name, Type=String, Value=abc123, Description=desc-v1, AllowedPattern=^[a-zA-Z0-9]+$ and Tier=Standard; wait for CREATE_COMPLETE.
2. Read the parameter through SSM to confirm Description/AllowedPattern/Tier took effect.
3. Call cloudcontrol GetResource(TypeName="AWS::SSM::Parameter", Identifier=<the parameter's physical id>) and json-decode ResourceDescription.Properties.
4. Emulator: Description, AllowedPattern and Tier are present. AWS: they are absent; only readable members appear.
5. Repeat after a change set that changes only those three properties and is executed — the emulator still projects them.
