Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53resolver.associate-resolver-query-log-config-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: route53resolver.associate-resolver-query-log-config-nonexistent-rejected|run#9|AssertionError|assert code == "ResourceNotFoundException", f"expected ResourceNotFoundException, got {code!r}"
Issue-Commit: e2c5b0f48e2fab2584b6e4e1c711e4515b8cfc17

# Route 53 Resolver AssociateResolverQueryLogConfig with a nonexistent ResolverQueryLogConfigId is refused as InvalidParameterException instead of ResourceNotFoundException [RSLVR-01601]

> _Note: this issue was identified against the code base at commit `e2c5b0f48e2fab2584b6e4e1c711e4515b8cfc17`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateResolverQueryLogConfig

## Description
Cause 1 of 1: AssociateResolverQueryLogConfig mis-classifies a not-found query logging configuration as a parameter-validation failure.

AWS: Smithy (services/service-com-amazonaws-route53resolver-route53resolver/smithy-model/com-amazonaws-route53resolver-route53resolver.json) models AssociateResolverQueryLogConfig with input members ResolverQueryLogConfigId (required, @length 1-64) and ResourceId (required, @length 1-64), and binds both ResourceNotFoundException and InvalidParameterException (each HTTP 400, awsJson1_1). A well-formed config id that satisfies the length constraint but names no existing configuration is a not-found condition, not a malformed parameter: AWS refuses it with HTTP 400 and error code ResourceNotFoundException, message "[RSLVR-01601] The specified query logging configuration doesn't exist." plus Route 53 Resolver's per-request trailing ` Trace Id: "<root>"` clause. Fixed text of that message: the `[RSLVR-01601] ` code prefix and the sentence "The specified query logging configuration doesn't exist."; the only per-request part is the Trace Id value, which the check strips before comparing. Nothing in the message is derived from the request, so no recorded id or name belongs in the template.

Emulator: the same call (ResolverQueryLogConfigId="rqlc-0123456789abcdef0", a syntactically valid id that was never created, ResourceId = a real VPC id) is refused with error code 'InvalidParameterException'. The check asserts the code first, so the replay stops at `expected ResourceNotFoundException, got 'InvalidParameterException'`; the accompanying Message is therefore also unverified and must be brought to AWS's RSLVR-01601 wording as part of the fix.

Why: a client cannot distinguish "this configuration does not exist" (retry/create it, or treat the association as absent) from "your request was malformed" (a bug in the caller). botocore/SDK catch blocks written against route53resolver.exceptions.ResourceNotFoundException — including the idempotent cleanup pattern this very check uses for disassociate/delete — do not match InvalidParameterException, so the exception escapes and tears down otherwise-correct client code. Error-code-driven retry and provisioning logic branches the wrong way.

Fix: the AssociateResolverQueryLogConfig handler's lookup path in the route53resolver service crate — the config-id resolution step must distinguish a syntactically valid but unknown ResolverQueryLogConfigId (raise ResourceNotFoundException with the RSLVR-01601 message) from a parameter that violates the modeled constraints (InvalidParameterException), rather than funneling every failed lookup into InvalidParameterException. The shared not-found message builder should carry the RSLVR-01601 prefix and the trailing Trace Id clause the rest of this service already emits.

Traces: behavioral check route53resolver.associate-resolver-query-log-config-nonexistent-rejected (mismatch), assertion at check_route53resolver_associate_resolver_query_log_config_nonexistent_rejected.py line 37.

## Evidence
Check: route53resolver.associate-resolver-query-log-config-nonexistent-rejected [mismatch]

Request: AssociateResolverQueryLogConfig(ResolverQueryLogConfigId="rqlc-0123456789abcdef0" (never created), ResourceId=<a freshly created, available VPC id>)

Expected (AWS): HTTP 400, Error.Code = "ResourceNotFoundException", Error.Message = "[RSLVR-01601] The specified query logging configuration doesn't exist." (+ Route 53 Resolver's trailing ` Trace Id: "..."` clause, stripped by the check before comparison).

Actual (emulator): HTTP 400 with Error.Code = "InvalidParameterException".

Replay output:
  File "check_route53resolver_associate_resolver_query_log_config_nonexistent_rejected.py", line 37, in run
    assert code == "ResourceNotFoundException", f"expected ResourceNotFoundException, got {code!r}"
  AssertionError: expected ResourceNotFoundException, got 'InvalidParameterException'

Model evidence: AssociateResolverQueryLogConfig binds ResourceNotFoundException (client error, HTTP 400) alongside InvalidParameterException; ResolverQueryLogConfigId is @length(min 1, max 64) and the probe id satisfies that constraint, so it is not a parameter violation.

## Steps to Reproduce
1. Boot the emulator and point a botocore route53resolver client at it.
2. Create a VPC via ec2.create_vpc(CidrBlock="10.93.0.0/16") and wait for vpc_available; note the VpcId.
3. Call route53resolver.associate_resolver_query_log_config(ResolverQueryLogConfigId="rqlc-0123456789abcdef0", ResourceId=<VpcId>) — the config id is well-formed but was never created.
4. Inspect the raised ClientError: Error.Code is "InvalidParameterException"; AWS returns "ResourceNotFoundException" with Message "[RSLVR-01601] The specified query logging configuration doesn't exist."
5. Equivalent: replay behavioral check route53resolver.associate-resolver-query-log-config-nonexistent-rejected.
