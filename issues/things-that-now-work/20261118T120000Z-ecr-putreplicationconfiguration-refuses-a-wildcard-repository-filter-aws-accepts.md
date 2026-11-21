Issue-Source: fixer-observation
Issue-Severity: LOW
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Commit: 22b5fe70ea36a5c2dec965349fcbb5b79b3f8e57

# ecr PutReplicationConfiguration refuses a repositoryFilters value carrying the `*` wildcard that AWS accepts

> _Note: this issue was observed while fixing
> `issues/things-that-now-work/20261117T115047Z-ecr-5-net-new-refusal-fidelity-causes-imagenotfoundexception.md`,
> whose cause 3 established the expression ECR publishes for this value class. That fix
> made the REFUSAL quote AWS's expression; this issue is the other half — which values are
> ACCEPTED. No recorded unit of that issue sends a wildcard filter, so this is carried
> separately rather than claimed as fixed there._

## Operation
PutReplicationConfiguration

## Description
Cause 1 of 1: a replication rule's `repositoryFilters[].filter` may carry the `*` wildcard
at AWS and is refused here.

AWS: the expression ECR's own validator holds for `RepositoryFilter$filter` — the one it
quotes back in its `InvalidParameterException` constraint clause — is
`^(?=.{1,256}$)(?:[a-z0-9*]+(?:[._-][a-z0-9*]*)*/)*[a-z0-9*]*(?:[._-][a-z0-9*]*)*$`: it
admits `*` in every character class and carries the value's length as a leading lookahead.
The on-disk Smithy model's `com.amazonaws.ecr#RepositoryFilterValue` `@pattern` is the
narrower `^(?:[a-z0-9]+(?:[._-][a-z0-9]*)*/)*[a-z0-9]*(?:[._-][a-z0-9]*)*$`, with
`@length(2..256)` stated separately, so the model and the service disagree about this one
value class and the service's expression is what a caller is held to.

Emulator: a `PREFIX_MATCH` filter containing `*` (for example `team/*`) is refused HTTP 400
`InvalidParameterException` at the request boundary, with a clause that quotes AWS's
expression — an expression the refused value satisfies, so the refusal contradicts its own
constraint. A filter with no wildcard is accepted and stored, so only the wildcard forms
are affected.

Why: the wildcard is how an operator covers a namespace with one rule, so a configuration
AWS accepts cannot be applied here at all, and the refusal points at a rule the value did
not break.

Fix: what checks the value is the model-driven boundary walk, generated from the shipped
Smithy model's own `@pattern`; a service crate has no seam to widen a modelled constraint
(`override_refusal` can only word a refusal the walk already made), and the walk's
regular-expression engine supports no look-around, so AWS's expression cannot be installed
as the enforced one verbatim. Closing this means enforcing the published class some other
way — the wildcard-admitting body with the length bound checked beside it — so that the
values accepted and the expression quoted are the same rule.

## Evidence
`PutReplicationConfiguration` with
`replicationConfiguration.rules[0] = {destinations: [{region: 'us-west-2', registryId: <this account>}], repositoryFilters: [{filter: 'team/*', filterType: 'PREFIX_MATCH'}]}`:
  expected: HTTP 200, the configuration stored and reported back by `DescribeRegistry`
  actual:   HTTP 400 `InvalidParameterException`, "Invalid parameter at
            'replicationConfiguration.rules.1.member.repositoryFilters.1.member.filter'
            failed to satisfy constraint: 'Member must satisfy regular expression pattern:
            ^(?=.{1,256}$)(?:[a-z0-9*]+(?:[._-][a-z0-9*]*)*/)*[a-z0-9*]*(?:[._-][a-z0-9*]*)*$'"

## Steps to Reproduce
Call `PutReplicationConfiguration` with one rule whose single `repositoryFilters` entry has
`filterType` `PREFIX_MATCH` and a `filter` containing `*`; then `DescribeRegistry` to see
that nothing was stored.
