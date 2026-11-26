Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-resource-server-and-oauth-scope-enforcement
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-resource-server-and-oauth-scope-enforcement
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|UpdateResourceServer|2xx->2xx|$.ResourceServer.Scopes[*].ScopeDescription
Issue-Key: cognito-idp|UpdateResourceServer|2xx->2xx|$.ResourceServer.Scopes[*].ScopeName
Issue-Key: cognito-idp|ListResourceServers|2xx->2xx|$.ResourceServers[Name="probe-api"].Scopes[*].ScopeDescription
Issue-Key: cognito-idp|ListResourceServers|2xx->2xx|$.ResourceServers[Name="probe-api"].Scopes[*].ScopeName
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp resource-server Scopes are reordered instead of kept in request order

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateResourceServer

## Description
1 cause, observable on the write response and on the subsequent list.

Cause 1 of 1: the ResourceServer.Scopes list is returned in an order other than the order the request supplied.
AWS: Smithy: UpdateResourceServer.Scopes targets ResourceServerScopeListType, a list (@length 0..100) of ResourceServerScopeType with NO @uniqueItems and no sort trait, and the output ResourceServer (ResourceServerType) is required. A Smithy list is ordered, so the recorded responses report the scopes in exactly the order of the request's Scopes member — positions 1 and 2 of the recorded list are the 'admin' scope ('Admin access') then the 'write' scope ('Write access'), matching how the update was sent.
Emulator: returns them swapped — position 1 is 'write'/'Write access' and position 2 is 'admin'/'Admin access' — and ListResourceServers reports the same permuted order for that resource server, so the reordering is stored, not just a serialization artifact of one response.
Why: a client that indexes the list positionally, renders it to a user, or compares a Describe/List result against the configuration it just submitted sees a different list from the one it wrote; configuration-drift detection and idempotency checks report a spurious change on every read.
Fix: scope storage and serialization for the resource-server record in the cognito-idp crate (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — the UpdateResourceServer handler must preserve the request's list order instead of placing scopes in a sorted/keyed container, and the ListResourceServers serializer must emit them in that stored order.
Traces: cognito-idp-resource-server-and-oauth-scope-enforcement#task#4 (UpdateResourceServer), #task#5 (ListResourceServers).

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture cognito-idp-resource-server-and-oauth-scope-enforcement:

task#4 UpdateResourceServer, 200 -> 200:
  $.ResourceServer.Scopes[1].ScopeName: expected 'admin', got 'write'
  $.ResourceServer.Scopes[1].ScopeDescription: expected 'Admin access', got 'Write access'
  $.ResourceServer.Scopes[2].ScopeName: expected 'write', got 'admin'
  $.ResourceServer.Scopes[2].ScopeDescription: expected 'Write access', got 'Admin access'
task#5 ListResourceServers, 200 -> 200:
  $.ResourceServers[Name="probe-api"].Scopes[1]/[2] show the identical swap.

Smithy: com.amazonaws.cognitoidentityprovider#ResourceServerScopeListType is type list, @length min 0 max 100, member ResourceServerScopeType — ordered, no @uniqueItems, no sorting trait; UpdateResourceServer output ResourceServer is required.

## Steps to Reproduce
1. Replay fixture cognito-idp-resource-server-and-oauth-scope-enforcement.
2. Step task#4: UpdateResourceServer for the 'probe-api' resource server with a Scopes list whose entries are in a fixed order (read, admin, write as recorded) -> the response's Scopes[1] and Scopes[2] come back swapped.
3. Step task#5: ListResourceServers for the pool -> the same resource server reports the same permuted order, confirming the order was stored, not just serialized differently.
