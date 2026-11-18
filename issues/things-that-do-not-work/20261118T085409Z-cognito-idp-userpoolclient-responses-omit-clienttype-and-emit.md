Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-user-pool-client-empty-lists-explicit-token-units-and-legacy-errors-then-not-found
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-user-pool-client-empty-lists-explicit-token-units-and-legacy-errors-then-not-found
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.AllowedOAuthFlows
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.AllowedOAuthScopes
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.CallbackURLs
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.ClientType
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.CreationDate
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.ExplicitAuthFlows
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.LastModifiedDate
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.LogoutURLs
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.ReadAttributes
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.SupportedIdentityProviders
Issue-Key: cognito-idp|CreateUserPoolClient|2xx->2xx|$.UserPoolClient.WriteAttributes
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.AllowedOAuthFlows
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.AllowedOAuthScopes
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.CallbackURLs
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.ClientType
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.CreationDate
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.ExplicitAuthFlows
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.LastModifiedDate
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.ReadAttributes
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.SupportedIdentityProviders
Issue-Key: cognito-idp|DescribeUserPoolClient|2xx->2xx|$.UserPoolClient.WriteAttributes
Issue-Key: cognito-idp|UpdateUserPoolClient|2xx->2xx|$.UserPoolClient.ClientType
Issue-Key: cognito-idp|UpdateUserPoolClient|2xx->2xx|$.UserPoolClient.CreationDate
Issue-Key: cognito-idp|UpdateUserPoolClient|2xx->2xx|$.UserPoolClient.LastModifiedDate
Issue-Related: 20261102T111936Z-cognito-idp-response-payloads-diverge-read-paths-omit.md
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp UserPoolClient responses omit ClientType and emit empty list members

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeUserPoolClient

## Description
Every app-client read path (CreateUserPoolClient, DescribeUserPoolClient, UpdateUserPoolClient) returns a UserPoolClient structure that differs from AWS's in the same two ways. 3 causes; each must be closed before submitting.

Cause 1 of 3: the UserPoolClient structure omits ClientType.
AWS: recorded behavior only — this copy of the Smithy model's UserPoolClientType carries no ClientType member, while the recorded CreateUserPoolClient / DescribeUserPoolClient / UpdateUserPoolClient responses all report UserPoolClient.ClientType with the value 'REGULAR' for an ordinary (non-machine) app client created without any special request member. The value is a fixed enum-style token, not a per-run value.
Emulator: the member is absent from all three responses (reported as missing, expected 'REGULAR', got None).
Why: a client that reads ClientType to tell a regular app client from other client types gets no value at all and cannot branch; SDKs surface it as None where AWS always populates it.
Fix: the UserPoolClient serializer plus the app-client record in the cognito-idp crate's state (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — the client must carry and report its type on every read path, not just on create.
Traces: cognito-idp-user-pool-client-empty-lists-explicit-token-units-and-legacy-errors-then-not-found#task#2 (CreateUserPoolClient), #task#3 (DescribeUserPoolClient), #task#4 (UpdateUserPoolClient), #task#5 (DescribeUserPoolClient after the update).

Cause 2 of 3: list members the request did not set are serialized as empty arrays instead of being omitted.
AWS: Smithy: UserPoolClientType.ReadAttributes / WriteAttributes (ClientPermissionListType), ExplicitAuthFlows (ExplicitAuthFlowsListType), SupportedIdentityProviders, CallbackURLs, LogoutURLs, AllowedOAuthFlows, AllowedOAuthScopes are all optional list members with no @default. The ReadAttributes/WriteAttributes documentation is explicit that when the request does not specify them "ReadAttributes doesn't return any information" — i.e. the member is absent, not an empty list. Recorded behavior: for a client created with none of these members, the recorded response contains none of them.
Emulator: serializes each of them as [] (reported as unexpected_present, expected None, got []), including ExplicitAuthFlows on a subsequent describe after an update that left it unset.
Why: 'unset' and 'set to the empty list' are different states for an app client — an empty ReadAttributes/ExplicitAuthFlows reads as "this client may read nothing / use no auth flow", so a client that round-trips a Describe result into an Update (or that renders the configuration) changes the client's meaning, and presence checks on the member always succeed.
Fix: the UserPoolClient serializer in the cognito-idp crate — omit optional list members that were never set rather than defaulting them to an empty vector.
Traces: cognito-idp-user-pool-client-empty-lists-explicit-token-units-and-legacy-errors-then-not-found#task#2, #task#3, #task#5.

Cause 3 of 3: UserPoolClient.CreationDate / LastModifiedDate carry sub-millisecond precision (1794991611.111371, 1794991611.1430976) where the recorded epoch-seconds shape does not. This is the same serializer fault filed as "cognito-idp date members are serialized with sub-millisecond precision" (representative cognito-idp-user-pool-tag-change-and-remove-last#task#1); it is repeated here because these four steps also fail on it and must be re-replayed after that fix.
AWS: Smithy: UserPoolClientType.CreationDate / LastModifiedDate -> DateType (timestamp, awsJson1_1 epoch-seconds).
Emulator: full float clock reading.
Why/Fix: as in that issue (shared DateType serializer).
Traces: the same four steps.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture cognito-idp-user-pool-client-empty-lists-explicit-token-units-and-legacy-errors-then-not-found:

task#2 CreateUserPoolClient, status 200 -> 200:
  $.UserPoolClient.ClientType: missing (expected 'REGULAR', got None)
  $.UserPoolClient.CreationDate / LastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991611.111371)
  $.UserPoolClient.ReadAttributes / WriteAttributes / ExplicitAuthFlows / SupportedIdentityProviders / CallbackURLs / LogoutURLs / AllowedOAuthFlows / AllowedOAuthScopes: unexpected_present (expected None, got [])
task#3 DescribeUserPoolClient: identical key set and values.
task#4 UpdateUserPoolClient: $.UserPoolClient.ClientType missing (expected 'REGULAR'); CreationDate 1794991611.111371, LastModifiedDate 1794991611.1430976.
task#5 DescribeUserPoolClient: ClientType missing; $.UserPoolClient.ExplicitAuthFlows unexpected_present (expected None, got []).

Smithy (com.amazonaws.cognitoidentityprovider): UserPoolClientType has no ClientType member in this model copy (so 'REGULAR' is recorded behavior only); ReadAttributes/WriteAttributes/ExplicitAuthFlows/SupportedIdentityProviders/CallbackURLs/LogoutURLs/AllowedOAuthFlows/AllowedOAuthScopes are optional lists with no @default; CreationDate/LastModifiedDate target DateType.

## Steps to Reproduce
1. Replay fixture cognito-idp-user-pool-client-empty-lists-explicit-token-units-and-legacy-errors-then-not-found.
2. Step task#2: CreateUserPoolClient with only UserPoolId and ClientName -> response UserPoolClient has no ClientType and carries eight empty list members.
3. Step task#3: DescribeUserPoolClient for that client -> same divergences.
4. Step task#4: UpdateUserPoolClient -> ClientType still absent.
5. Step task#5: DescribeUserPoolClient again -> ExplicitAuthFlows is served as [] where AWS omits it.
