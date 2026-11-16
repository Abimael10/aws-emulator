Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__securityhub.securityhub-hub-invalid-mode-rejected-and-children-round-trip
Issue-Key: cloudformation__securityhub.securityhub-hub-invalid-mode-rejected-and-children-round-trip|run#6|AssertionError|assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
Issue-Commit: 36712e0283f049b61debb52d750d108c70e96514

# CloudFormation accepts an off-enum AWS::SecurityHub::Hub ControlFindingGenerator: the stack reaches CREATE_COMPLETE and subscribes the account instead of failing property validation and rolling back

> _Note: this issue was identified against the code base at commit `36712e0283f049b61debb52d750d108c70e96514`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause.

Cause 1 of 1: a CloudFormation stack that declares AWS::SecurityHub::Hub with a ControlFindingGenerator value outside the modelled set is provisioned instead of being refused by property validation. Failing check: cloudformation__securityhub.securityhub-hub-invalid-mode-rejected-and-children-round-trip.

AWS: Smithy (securityhub) EnableSecurityHub.ControlFindingGenerator targets the enum ControlFindingGenerator with values [STANDARD_CONTROL, SECURITY_CONTROL]; the AWS::SecurityHub::Hub resource schema constrains that property to the pattern ^(SECURITY_CONTROL|STANDARD_CONTROL)$. Recorded behavior: CreateStack with a value outside that pattern is refused by CloudFormation's pre-provision property validation before Security Hub is ever called. The stack ends ROLLBACK_COMPLETE and the offending resource carries exactly one validation row — ValidationType PROPERTY_VALIDATION, status FAILED, evaluation FAIL — whose message is the fixed text `Property value [<value>] does not match pattern: ^(SECURITY_CONTROL|STANDARD_CONTROL)$` (the bracketed value is copied verbatim from the submitted template) at the location `/Resources/<LogicalId>/Properties/ControlFindingGenerator`, built from the offending resource's logical id and the property name. The account stays unsubscribed, so DescribeHub keeps answering InvalidAccessException with the message `Account <account> is not subscribed to AWS Security Hub CSPM` (the account id comes from the caller's identity).

Emulator: the stack with the off-enum ControlFindingGenerator reaches CREATE_COMPLETE. No PROPERTY_VALIDATION row is produced, the create is not rolled back, and the hub is subscribed for the account with a value Security Hub does not model.

Why: a client relying on AWS's behavior loses the whole rejection class. A template typo or an off-enum mode is silently provisioned rather than refused, so the deploy reports success; downstream reads then see a hub whose ControlFindingGenerator is a value no Security Hub API accepts, and — because Hub is an account singleton — the account is left subscribed by a stack that on AWS would have rolled back and left it untouched. Any policy that depends on invalid templates failing closed (a CI gate asserting ROLLBACK_COMPLETE and the validation row) passes on an invalid template.

Fix: the CloudFormation property-validation pass for the AWS::SecurityHub::Hub resource type (the pre-provision schema/pattern check that emits PROPERTY_VALIDATION rows and fails the resource), and the Hub create handler, which must not call EnableSecurityHub with a non-enum ControlFindingGenerator.

Traces: cloudformation__securityhub.securityhub-hub-invalid-mode-rejected-and-children-round-trip (the stack created with the invalid mode; the check's later positive-control and child-resource assertions were never reached). Open securityhub issues (e.g. 20261109T123922Z-securityhub-describesecurityhubv2-stub.md, 20261029T180317Z-securityhub-mutations-are-not-reflected-by-later-reads.md) do not cover this: none concerns CloudFormation property validation of the Hub resource type.

## Evidence
Replay of cloudformation__securityhub.securityhub-hub-invalid-mode-rejected-and-children-round-trip:

  File "check_cloudformation__securityhub_securityhub_hub_invalid_mode_rejected_and_children_round_trip.py", line 315, in run
    assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
  AssertionError: rejected stack ended 'CREATE_COMPLETE'

Expected (AWS): stack status ROLLBACK_COMPLETE, and DescribeStackResource validation rows == [("Hub", "PROPERTY_VALIDATION", "FAILED", "FAIL", "Property value [<submitted value>] does not match pattern: ^(SECURITY_CONTROL|STANDARD_CONTROL)$", "/Resources/Hub/Properties/ControlFindingGenerator")], with the account still refused by DescribeHub (InvalidAccessException, "Account <account> is not subscribed to AWS Security Hub CSPM").
Actual (emulator): stack status CREATE_COMPLETE, no validation row, hub subscribed.

Fixed text vs variable parts of the required message: `Property value [` ... `] does not match pattern: ^(SECURITY_CONTROL|STANDARD_CONTROL)$` is fixed; the bracketed value is the value from the request template, and the location path's `Hub` segment is the offending resource's logical id.

Model: securityhub EnableSecurityHub.ControlFindingGenerator -> enum ControlFindingGenerator = [STANDARD_CONTROL, SECURITY_CONTROL].

## Steps to Reproduce
1. CreateStack with a template whose only resource is AWS::SecurityHub::Hub with Properties.ControlFindingGenerator set to a value outside the enum (e.g. "BOGUS_MODE") and Tags set.
2. Poll DescribeStacks until the stack reaches a terminal status.
3. AWS: the status is ROLLBACK_COMPLETE and the Hub resource carries the PROPERTY_VALIDATION/FAILED row quoted above; DescribeHub still answers InvalidAccessException for the account. Emulator: the status is CREATE_COMPLETE and the hub is subscribed.
