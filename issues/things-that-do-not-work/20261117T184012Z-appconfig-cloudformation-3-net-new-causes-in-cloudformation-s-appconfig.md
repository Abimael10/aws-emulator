Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete appconfig__cloudformation.deploy-cfn-appconfig-extension-association-tag-updates-round-trip appconfig__cloudformation.deploy-cfn-appconfig-extension-name-with-slash-rejected appconfig__cloudformation.deploy-cfn-appconfig-resource-aws-prefixed-tag-rolls-back-update appconfig__cloudformation.deploy-cfn-appconfig-resources-tag-updates-round-trip-through-tag-resource
Issue-Key: appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete|run#13|KeyError|outputs = {o["OutputKey"]: o["OutputValue"] for o in stack["Outputs"]}
Issue-Key: appconfig__cloudformation.deploy-cfn-appconfig-extension-association-tag-updates-round-trip|_outputs#13|KeyError|return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Key: appconfig|GetExtension|ResourceNotFoundException
Issue-Key: appconfig__cloudformation.deploy-cfn-appconfig-resource-aws-prefixed-tag-rolls-back-update|run#33|AssertionError|assert got_final == tags_v2, f"expected tags {tags_v2!r} after the legal update, got {got_final!r}"
Issue-Key: appconfig__cloudformation.deploy-cfn-appconfig-resources-tag-updates-round-trip-through-tag-resource|_assert_tags#26|AssertionError|assert got == expected, f"{where}: {label} tags {got!r}, expected {expected!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 3 net-new causes in CloudFormation's AppConfig resources: stack Outputs omit OutputValue, an Extension is never created, and tag updates never reach the AppConfig resource

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
Three distinct defects reached through CloudFormation stacks that declare AWS::AppConfig::* resources. Each cause has its own failing check ids.

Cause 1 of 3: the Output rows DescribeStacks returns for a CREATE_COMPLETE AppConfig stack carry OutputKey but no OutputValue.
AWS: Smithy: cloudformation#Output declares members OutputKey, OutputValue, Description, ExportName; AWS populates OutputValue with the resolved Ref/GetAtt for every output of a stack that reached CREATE_COMPLETE.
Emulator: the Output structure is serialized without the OutputValue member, so a client that reads `o["OutputValue"]` raises KeyError and nothing in the stack's outputs is readable.
Why: every consumer of stack outputs (the CLI's `describe-stacks --query Stacks[0].Outputs`, the CDK's outputs file, cross-stack wiring, any script that feeds an output into the next call) gets a structurally invalid output list rather than a value. Note the related open issue 20261109T144630Z-cloudformation-elasticbeanstalk-stack-outputs-omit-outputvalue-a-stack.md describes the same symptom for Elastic Beanstalk stacks; it does not cover these checks.
Fix: the CloudFormation DescribeStacks output serializer / the stack's output-resolution state.
Traces: appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete, appconfig__cloudformation.deploy-cfn-appconfig-extension-association-tag-updates-round-trip

Cause 2 of 3: an AWS::AppConfig::Extension the stack reports as created does not exist in AppConfig.
AWS: recorded behavior only. A stack declaring a valid AWS::AppConfig::Extension creates it, so appconfig GetExtension answers with the extension; only after the stack is deleted does GetExtension answer ResourceNotFoundException with the message `Extension could not be found for account <accountId>.` (fixed text plus the caller's account id).
Emulator: GetExtension answers ResourceNotFoundException `Extension could not be found for account <accountId>.` while the stack still owns the extension — the resource handler reports success without creating anything.
Why: a client that deploys an extension and then configures it (or an extension association that must point at it) cannot find the resource the stack claims to own; the negative control in the same check can no longer be distinguished from the positive one.
Fix: the AWS::AppConfig::Extension resource handler's Create (it must call appconfig CreateExtension and record the id).
Traces: appconfig__cloudformation.deploy-cfn-appconfig-extension-name-with-slash-rejected

Cause 3 of 3: a stack update that changes tag values on AppConfig resources leaves the live tags unchanged.
AWS: recorded behavior only. Updating the Tags of an AWS::AppConfig::Application (and its sibling AppConfig resources) in a template drives appconfig TagResource/UntagResource, so ListTagsForResource afterwards reports exactly the template's new tag set — a changed value replaces the old one and an added key appears.
Emulator: after the update reports success, ListTagsForResource still reports the pre-update tag set: the value change and the added key are both lost.
Why: tag-driven cost allocation, access control and inventory all read the live tags, so they keep seeing stale values after a successful stack update; a subsequent update that depends on the tag state behaves unpredictably.
Fix: the AWS::AppConfig::* resource handlers' Update path (tag diffing into TagResource/UntagResource).
Traces: appconfig__cloudformation.deploy-cfn-appconfig-resource-aws-prefixed-tag-rolls-back-update, appconfig__cloudformation.deploy-cfn-appconfig-resources-tag-updates-round-trip-through-tag-resource

## Evidence
Cause 1: `{o["OutputKey"]: o["OutputValue"] for o in stack["Outputs"]}` raises `KeyError: 'OutputValue'` on a CREATE_COMPLETE stack (both checks).

Cause 2: `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the GetExtension operation: Extension could not be found for account 000000000000.` raised while the owning stack is still CREATE_COMPLETE.

Cause 3: AssertionError: "expected tags {'sim2real-stage': 'prod'} after the legal update, got {'sim2real-stage': 'beta'}"; and "after changing a value and adding a key: application tags {'sim2real-stage': 'beta'}, expected {'sim2real-stage': 'prod', 'sim2real-owner': 'payments'}".

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete
2. appconfig__cloudformation.deploy-cfn-appconfig-extension-association-tag-updates-round-trip
3. appconfig__cloudformation.deploy-cfn-appconfig-extension-name-with-slash-rejected
4. appconfig__cloudformation.deploy-cfn-appconfig-resource-aws-prefixed-tag-rolls-back-update
5. appconfig__cloudformation.deploy-cfn-appconfig-resources-tag-updates-round-trip-through-tag-resource
