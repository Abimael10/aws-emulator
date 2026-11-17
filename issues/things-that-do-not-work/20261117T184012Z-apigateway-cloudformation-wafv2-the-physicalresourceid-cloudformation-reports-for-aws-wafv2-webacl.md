Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__wafv2.deploy-webacl-association-blocks-a-matching-request-at-the-stage-then-stack-delete-removes-it apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack
Issue-Key: apigateway__cloudformation__wafv2.deploy-webacl-association-blocks-a-matching-request-at-the-stage-then-stack-delete-removes-it|run#14|IndexError|webacl_id = resources["WebAcl"].split("|")[1]
Issue-Key: apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack|run#13|IndexError|good_webacl_id = good_resources["WebAcl"].split("|")[1]
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# The PhysicalResourceId CloudFormation reports for AWS::WAFv2::WebACL is a generated logical name instead of the composite <Name>|<Id>|<Scope> primary identifier

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResources

## Description
Cause 1 of 1: a CloudFormation-provisioned AWS::WAFv2::WebACL carries a physical id that is not the resource type's composite primary identifier.
AWS: recorded behavior only (the WAFv2 resource schemas declare the WebACL primary identifier as the triple Name, Id, Scope). CloudFormation renders it as a single pipe-delimited string `<Name>|<Id>|<Scope>`, so a client splits on '|' and takes field 1 to get the WebACL Id it must pass to wafv2 GetWebACL / AssociateWebACL. Fixed text: the two '|' separators and the field order; variable parts: the WebACL name (from the template), the generated Id, and the Scope (REGIONAL or CLOUDFRONT, from the template).
Emulator: the physical id is a single generated token of the form `<stack-name>-<LogicalId>-<suffix>` with no '|' in it at all, so splitting yields one field and there is no way to recover the WebACL Id or Scope from the stack.
Why: every client that reads a WebACL out of a stack — the console, the CDK, any script that feeds DescribeStackResources into wafv2 GetWebACL, and the stage/association workflow itself — cannot address the WebACL. The failure is immediate (an index error on the split) rather than a wrong answer, so the whole downstream workflow is unreachable.
Fix: the AWS::WAFv2::WebACL resource handler's physical-id construction (and, for consistency, the same composite form for the sibling AWS::WAFv2::IPSet / ::RuleGroup / ::WebACLAssociation types the same stacks declare).
Traces: apigateway__cloudformation__wafv2.deploy-webacl-association-blocks-a-matching-request-at-the-stage-then-stack-delete-removes-it, apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack

## Evidence
Both checks fail at the same place: `webacl_id = resources["WebAcl"].split("|")[1]` raises `IndexError: list index out of range`, because the PhysicalResourceId the emulator returned contains no '|' — it is a generated name such as `<stack-name>-WebAcl-<suffix>`. Expected a value of the shape `<Name>|<Id>|<Scope>` whose field 1 is the WebACL Id accepted by wafv2 GetWebACL.

## Steps to Reproduce
Replay the behavioral suite and re-run these checks:
1. apigateway__cloudformation__wafv2.deploy-webacl-association-blocks-a-matching-request-at-the-stage-then-stack-delete-removes-it
2. apigateway__cloudformation__wafv2.deploy-webacl-duplicate-rule-priority-rejected-and-rolls-back-the-whole-stack
In each, deploy a stack with an AWS::WAFv2::WebACL and read its PhysicalResourceId from DescribeStackResources; it must be `<Name>|<Id>|<Scope>`.
