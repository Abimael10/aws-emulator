Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-user-pool-minimal-create-defaults-and-mfa-off
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-user-pool-minimal-create-defaults-and-mfa-off
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|CreateUserPool|2xx->2xx|$.UserPool.AcrConfiguration
Issue-Key: cognito-idp|DescribeUserPool|2xx->2xx|$.UserPool.AcrConfiguration
Issue-Related: 20261118T085409Z-cognito-idp-date-members-are-serialized-with-sub.md
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp CreateUserPool omits the default AcrConfiguration map

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserPool

## Description
1 cause, visible on both the create and the read path of a minimally-specified user pool.

Cause 1 of 1: a pool created without an AcrConfiguration is reported with no AcrConfiguration at all, instead of the four fixed default ACR levels.
AWS: Smithy: UserPoolType.AcrConfiguration targets AcrConfigurationType, a map (@length 0..4) keyed by AcrLevelKeyType (@pattern ^Level[1-4]$) with AcrLevelConfigType values; its documentation says it "Defines the custom names that Amazon Cognito reports for the four fixed authentication context class reference (ACR) levels in a user pool. The map is keyed by level, from Level1 through Level4, and each value provides the custom name for that level." Recorded behavior: a CreateUserPool request that supplies no AcrConfiguration comes back with the map fully populated with the service defaults — Level1..Level4 whose AcrValue is urn:cognito:loa:1 .. urn:cognito:loa:4 — and DescribeUserPool reports the same. The keys and the urn:cognito:loa:<level> values are fixed text; nothing in them comes from the request.
Emulator: the member is absent from both CreateUserPool and DescribeUserPool (missing, expected the four-level map, got None).
Why: a client that reads AcrConfiguration to learn which acr claim values the pool will report (for example to configure an OIDC relying party's acr_values, or to render the level names) finds nothing and cannot distinguish "defaults in force" from "feature absent"; code that indexes AcrConfiguration['Level2'] fails outright.
Fix: user-pool default materialization in the cognito-idp crate (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — the CreateUserPool handler must store the four default ACR levels on the pool record so that both it and the DescribeUserPool serializer report them.
Traces: cognito-idp-user-pool-minimal-create-defaults-and-mfa-off#task#1 (CreateUserPool), #task#2 (DescribeUserPool).

The same two steps also carry the sub-millisecond CreationDate/LastModifiedDate divergence filed as "cognito-idp date members are serialized with sub-millisecond precision"; that is a separate serializer cause and is not re-filed here.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture cognito-idp-user-pool-minimal-create-defaults-and-mfa-off:

task#1 CreateUserPool, status 200 -> 200:
  $.UserPool.AcrConfiguration: missing (expected {'Level1': {'AcrValue': 'urn:cognito:loa:1'}, 'Level2': {'AcrValue': 'urn:cognito:loa:2'}, 'Level3': {'AcrValue': 'urn:cognito:loa:3'}, 'Level4': {'AcrValue': 'urn:cognito:loa:4'}}, got None)
  $.UserPool.CreationDate / LastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991633.0983636)
task#2 DescribeUserPool, status 200 -> 200: the identical AcrConfiguration key is missing with the identical expected map.

Smithy: com.amazonaws.cognitoidentityprovider#AcrConfigurationType is a map, @length min 0 max 4, key AcrLevelKeyType @pattern ^Level[1-4]$, value AcrLevelConfigType; UserPoolType.AcrConfiguration is the member. The default level names urn:cognito:loa:1..4 are recorded behavior.

## Steps to Reproduce
1. Replay fixture cognito-idp-user-pool-minimal-create-defaults-and-mfa-off.
2. Step task#1: CreateUserPool with no AcrConfiguration in the request -> read UserPool.AcrConfiguration; the emulator returns no such member, AWS returns Level1..Level4 with AcrValue urn:cognito:loa:1..4.
3. Step task#2: DescribeUserPool for that pool id -> the member is still absent.
