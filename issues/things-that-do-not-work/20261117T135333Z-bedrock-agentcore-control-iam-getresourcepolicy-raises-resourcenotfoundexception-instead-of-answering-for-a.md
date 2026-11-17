Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control__iam.get-resource-policy-empty-contrasts-delete-not-found-then-roundtrip
Issue-Key: bedrock-agentcore-control__iam|GetResourcePolicy|ResourceNotFoundException
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# GetResourcePolicy raises ResourceNotFoundException instead of answering for a resource with no policy

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourcePolicy

## Description
Cause 1 of 1: a read of an absent resource policy is refused instead of answered.

AWS: recorded behavior only for the pairing — the check contrasts GetResourcePolicy with DeleteResourcePolicy and records that `ResourceNotFoundException` with the message `Resource Policy doesn't exist for requested arn` is what *DeleteResourcePolicy* answers for a resource that has no policy, while GetResourcePolicy on that same resource succeeds (an empty/absent policy) before any PutResourcePolicy, and then round-trips the policy once one is put.

Emulator: GetResourcePolicy itself raises the delete-path refusal: `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the GetResourcePolicy operation: Resource Policy doesn't exist for requested arn`. The message's fixed text is AWS's wording for the *delete* refusal; the variable part is the requested resource ARN. Applying it to the read makes the two operations indistinguishable.

Why: a client that reads a policy before writing one (the normal "is there a policy?" probe) gets an exception instead of an empty answer, so it cannot tell "no policy yet" from "resource does not exist", and library code that treats ResourceNotFoundException as fatal breaks on a perfectly healthy resource.

Fix: the GetResourcePolicy handler — answer 200 for a resource that exists with no policy attached, and keep the ResourceNotFoundException/"Resource Policy doesn't exist for requested arn" refusal on the DeleteResourcePolicy path.

Traces: bedrock-agentcore-control__iam.get-resource-policy-empty-contrasts-delete-not-found-then-roundtrip.

## Evidence
Expected: GetResourcePolicy on a resource with no policy answers successfully (empty policy); only DeleteResourcePolicy answers ResourceNotFoundException "Resource Policy doesn't exist for requested arn".
Actual (uncaught in replay): `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the GetResourcePolicy operation: Resource Policy doesn't exist for requested arn`

## Steps to Reproduce
1. Create the agentcore resource and do NOT put a resource policy on it.
2. Call GetResourcePolicy(resourceArn=<arn>).
Observed: ResourceNotFoundException "Resource Policy doesn't exist for requested arn". Expected: a successful empty-policy response (that error belongs to DeleteResourcePolicy).
3. Call DeleteResourcePolicy(resourceArn=<arn>) — this one should raise it.
