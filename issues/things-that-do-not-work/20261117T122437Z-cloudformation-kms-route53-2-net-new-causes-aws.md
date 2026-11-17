Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__kms__route53.deploy-route53-keysigningkey-inplace-update-replacement-and-dnssec-delete cloudformation__kms__route53.deploy-route53-keysigningkey-wrong-kms-key-spec-rolled-back
Issue-Key: cloudformation__kms__route53|CreateStack|ValidationError
Issue-Key: cloudformation__kms__route53.deploy-route53-keysigningkey-wrong-kms-key-spec-rolled-back|run#10|AssertionError|assert status == "ROLLBACK_COMPLETE", f"stack with a mis-specced key ended {status!r}"
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# cloudformation__kms__route53: 2 net-new causes — AWS::Route53::DNSSEC is an unrecognized resource type, and AWS::Route53::KeySigningKey accepts an RSA_2048 KMS key instead of rolling back with Route 53's refusal

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two net-new root causes for CloudFormation-driven Route 53 DNSSEC.

Cause 1 of 2: AWS::Route53::DNSSEC is not a registered CloudFormation resource type, so no DNSSEC-signing template can be submitted.
  AWS: recorded behavior only — CreateStack accepts a template containing AWS::Route53::DNSSEC (with a HostedZoneId) and provisions it; the type is part of the public registry alongside AWS::Route53::KeySigningKey. AWS's ValidationError 'Template format error: Unrecognized resource types: [<type>]' is reserved for types that genuinely do not exist.
  Emulator: CreateStack is refused 400 ValidationError 'Template format error: Unrecognized resource types: [AWS::Route53::DNSSEC]', so the whole key-signing-key lifecycle (in-place update, replacement, DNSSEC delete) is unreachable.
  Why: any stack that enables DNSSEC signing for a hosted zone — the standard way to pair a KeySigningKey with its zone — cannot be created at all.
  Fix: register the AWS::Route53::DNSSEC resource type and add its CloudFormation resource handler (create/update/delete calling Route 53's DNSSEC signing operations).
  Traces: cloudformation__kms__route53.deploy-route53-keysigningkey-inplace-update-replacement-and-dnssec-delete.

Cause 2 of 2: an AWS::Route53::KeySigningKey pointed at a KMS key whose spec DNSSEC cannot use is provisioned instead of failing.
  AWS: recorded behavior only — DNSSEC signing accepts only an ECC_NIST_P256 SIGN_VERIFY customer managed key. With an RSA_2048 key the stack settles ROLLBACK_COMPLETE and the Ksk resource's single CREATE_FAILED ResourceStatusReason is 'Resource handler returned message: "The customer managed key with ARN \'<keyArn>\' could not be used by Route 53 DNSSEC. Check to make sure that the customer managed key has not been deleted. Otherwise, review its configurations and key policy, to make sure it is in compliance with Route 53 DNSSEC requirements. (Service: Route53, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)' — fixed text with the key ARN from the template and the ids per call; the zone is left with no key-signing key of that name.
  Emulator: the stack ends CREATE_COMPLETE, so a key-signing key backed by an unusable key spec is created.
  Why: a template that must not deploy deploys, and the zone ends up holding a KSK that could never sign; the operator never sees Route 53's diagnosis of the key.
  Fix: the AWS::Route53::KeySigningKey resource handler must create through Route 53's CreateKeySigningKey, and that operation must validate the referenced KMS key's spec/usage/policy against KMS state, with the refusal wrapped into the resource's CREATE_FAILED reason.
  Traces: cloudformation__kms__route53.deploy-route53-keysigningkey-wrong-kms-key-spec-rolled-back.

## Evidence
deploy-route53-keysigningkey-inplace-update-replacement-and-dnssec-delete — botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::Route53::DNSSEC].

deploy-route53-keysigningkey-wrong-kms-key-spec-rolled-back — AssertionError: stack with a mis-specced key ended 'CREATE_COMPLETE'. Expected ROLLBACK_COMPLETE with the Ksk CREATE_FAILED reason quoted above (HandlerErrorCode InvalidRequest) and no KSK of that name on the zone.

## Steps to Reproduce
1. Cause 1: cfn.create_stack with a template containing {"Type": "AWS::Route53::DNSSEC", "Properties": {"HostedZoneId": <zone>}} -> expect the stack to be created, not a ValidationError about an unrecognized type.
2. Cause 2: create a KMS key with KeySpec=RSA_2048, KeyUsage=SIGN_VERIFY and the Route 53 DNSSEC key policy; cfn.create_stack with an AWS::Route53::KeySigningKey referencing it on a hosted zone; poll DescribeStacks -> expect ROLLBACK_COMPLETE, read the Ksk CREATE_FAILED ResourceStatusReason, and confirm route53 lists no key-signing key by that name. Repeat with an ECC_NIST_P256 key -> CREATE_COMPLETE.
