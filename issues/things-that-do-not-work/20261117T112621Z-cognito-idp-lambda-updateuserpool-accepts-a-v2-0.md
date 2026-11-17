Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp__lambda.pre-token-generation-v2-rejected-on-the-lite-feature-plan
Issue-Key: cognito-idp__lambda.pre-token-generation-v2-rejected-on-the-lite-feature-plan|_error_code#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp__lambda: UpdateUserPool accepts a V2_0 PreTokenGenerationConfig on a LITE-tier pool instead of refusing FeatureUnavailableInTierException

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateUserPool

## Description
Cause 1 of 1: the feature-plan gate on pre-token-generation V2_0 is not enforced.
AWS: Smithy: UpdateUserPool.LambdaConfig (LambdaConfigType, carrying PreTokenGenerationConfig {LambdaVersion, LambdaArn}) and UpdateUserPool.UserPoolTier; the operation models FeatureUnavailableInTierException (HTTP 403). Token customization with a V2_0 pre-token-generation Lambda is an ESSENTIALS/PLUS feature: on a LITE-tier pool the update is refused FeatureUnavailableInTierException with the message 'The following features need to be disabled for the LITE pricing tier configured: Token Customization with Pre-Token Generation Lambda V2_0' — fixed text whose variable parts are the tier name and the feature list — and the pool's LambdaConfig keeps the V1_0 trigger it already had. The identical V2_0 update succeeds once the pool is raised to ESSENTIALS, pinning the refusal to the plan rather than to the Lambda.
Emulator: UpdateUserPool answers an empty HTTP 200 for the V2_0 config on a UserPoolTier='LITE' pool and stores it.
Why: a client gets no signal that the feature is not available in its plan, and the emulator lets a pool enter a configuration real AWS refuses — so code that handles FeatureUnavailableInTierException (and the tier upgrade that resolves it) is never exercised.
Fix: tier/feature validation in the UpdateUserPool (and CreateUserPool) handler's LambdaConfig path in the cognito-idp crate.
Traces: cognito-idp__lambda.pre-token-generation-v2-rejected-on-the-lite-feature-plan.

## Evidence
AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'content-length': '2', ...}} for UpdateUserPool(LambdaConfig={'PreTokenGenerationConfig': {'LambdaVersion': 'V2_0', 'LambdaArn': <trigger>}}) on a UserPoolTier='LITE' pool. Expected ('FeatureUnavailableInTierException', 'The following features need to be disabled for the LITE pricing tier configured: Token Customization with Pre-Token Generation Lambda V2_0') with the pool's PreTokenGenerationConfig.LambdaVersion left at 'V1_0'.

## Steps to Reproduce
Replay cognito-idp__lambda.pre-token-generation-v2-rejected-on-the-lite-feature-plan: create a pool with UserPoolTier='LITE', UpdateUserPool with a V1_0 PreTokenGenerationConfig (must succeed), then UpdateUserPool with the same Lambda as V2_0 (must be refused FeatureUnavailableInTierException and leave LambdaConfig unchanged), then raise the pool to ESSENTIALS and repeat the V2_0 update (must succeed).
