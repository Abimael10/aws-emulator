Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: secretsmanager.list-secrets-invalid-filter-key-value-and-page-size-rejected
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secretsmanager.list-secrets-invalid-filter-key-value-and-page-size-rejected|run#19|AssertionError|assert message == (
Issue-Commit: a6b847bcf1dd9a51c7255422cfcd25c4cc7e9736

# secretsmanager: ListSecrets' ValidationException for an unknown SortBy lists the enum value set in the model's declaration order instead of AWS's order

> _Note: this issue was identified against the code base at commit `a6b847bcf1dd9a51c7255422cfcd25c4cc7e9736`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListSecrets

## Description
One net-new Secrets Manager defect found by the behavioral replay. Cause count: 1.

=== Cause 1 of 1: the sortBy enum-value-set refusal message renders the allowed values in the wrong order ===
AWS: Smithy (com.amazonaws.secretsmanager#ListSecrets): input member SortBy targets SortByType, an enum whose declared values are created-date, last-accessed-date, last-changed-date, name; SortOrder targets SortOrderType; MaxResults targets MaxResultsType with @range min 1 max 100; Filters targets FiltersListType (0..10) whose Filter.Key is FilterNameStringType (declared description, name, tag-key, tag-value, primary-region, owning-service, all). A SortBy value outside the enum is refused with HTTP 400, Error.Code ValidationException and the generic constraint sentence whose ENUM LIST is emitted in AWS's own order, NOT the model's declaration order (recorded behavior for the ordering; the 400 and the constraint-envelope wording are the standard Smithy validation envelope):
  "1 validation error detected: Value '<the rejected value>' at 'sortBy' failed to satisfy constraint: Member must satisfy enum value set: [last-accessed-date, name, created-date, last-changed-date]"
Fixed text: everything except the quoted rejected value — the member path 'sortBy' and the bracketed value set, including its order, are fixed. Variable part: only the rejected value, taken verbatim from the request (here 'updated-date'). The same check pins the sibling message for an unknown filter key, where AWS likewise uses a non-model order, "[all, owning-service, name, tag-key, description, tag-value, primary-region]" at 'filters.1.member.key' — and that assertion PASSED, so the emulator already carries AWS's observed ordering for the filter-key enum and only the sortBy enum is rendered from the model.
Emulator: status 400 and Error.Code ValidationException are correct, but the message is "1 validation error detected: Value 'updated-date' at 'sortBy' failed to satisfy constraint: Member must satisfy enum value set: [created-date, last-accessed-date, last-changed-date, name]" — the value set in SortByType's declaration order. Hypothesis: the sortBy validator formats the list by iterating the generated enum variants, while the filters.key validator uses a hand-written list that matches AWS.
Why: the error message is part of the contract. A client, test or runbook that asserts on, logs or diffs the refusal sentence (and conformance/parity suites that compare it byte-for-byte against AWS) sees a mismatch even though the operation refuses correctly, and anything that parses the bracketed set to present the caller the allowed values in AWS's documented order gets a different ordering from the emulator than from AWS.
Also pinned by this check and NOT exercised, because the assertion fires before them: the two positive controls at the boundaries just past the refusals — ListSecrets(Filters=[{Key:'name',Values:[N]}], MaxResults=100) must list the secret (100 is the documented maximum), and ListSecrets(Filters=[{Key:'name',Values:[N]}], SortBy='last-changed-date') must list it. Both must be re-run with the fix.
Fix: the ListSecrets input validation in the secretsmanager service crate — the enum-value-set message for the SortBy member must emit AWS's fixed ordering of SortByType's values, the way the filters.N.member.key validator already does, rather than deriving the list from the model's declaration order.
Traces: secretsmanager.list-secrets-invalid-filter-key-value-and-page-size-rejected

Note on neighbouring filed work: open issue 20261117T143604Z-secretsmanager-4-net-new-causes-batchgetsecretvalue-maxresults-and.md covers other ListSecrets/validation-refusal defects (BatchGetSecretValue MaxResults, PutResourcePolicy length, the primary-region filter, StopReplicationToReplica) but does not carry this check's replay key and says nothing about enum-value-set ordering, so it does not cover this cause.

## Evidence
Behavioral suite simparity-behavioral-aws-v1.

Cause 1 — [mismatch] secretsmanager.list-secrets-invalid-filter-key-value-and-page-size-rejected, call ListSecrets(Filters=[{"Key":"name","Values":[<the check's secret name>]}], SortBy="updated-date"):
  AssertionError: unexpected message for an unknown SortBy: "1 validation error detected: Value 'updated-date' at 'sortBy' failed to satisfy constraint: Member must satisfy enum value set: [created-date, last-accessed-date, last-changed-date, name]"
  Expected: HTTP 400, Error.Code ValidationException, Message "1 validation error detected: Value 'updated-date' at 'sortBy' failed to satisfy constraint: Member must satisfy enum value set: [last-accessed-date, name, created-date, last-changed-date]" — only the quoted rejected value comes from the request; the member path and the bracketed set with its ordering are fixed text.
  Actual: HTTP 400, Error.Code ValidationException (both correct), with the value set in model-declaration order [created-date, last-accessed-date, last-changed-date, name].
  Controls that PASSED earlier in the same check (so these refusals are already right): an unknown filter key 'secret-name' -> ValidationException "...Value 'secret-name' at 'filters.1.member.key' failed to satisfy constraint: Member must satisfy enum value set: [all, owning-service, name, tag-key, description, tag-value, primary-region]"; a filter value containing '*' -> ValidationException with the length+pattern constraint sentence at 'filters.1.member.values'; MaxResults=101 -> ValidationException "...Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"; and the name filter converging on the created secret.
  Not yet exercised because the check stopped at this assertion: ListSecrets(..., MaxResults=100) must list the secret, and ListSecrets(..., SortBy="last-changed-date") must list the secret.

## Steps to Reproduce
1. CreateSecret(Name=N, SecretString="x") in the test Region; poll ListSecrets(Filters=[{"Key":"name","Values":[N]}]) until it returns exactly [N].
2. ListSecrets(Filters=[{"Key":"name","Values":[N]}], SortBy="updated-date") — a value outside SortByType; botocore does not validate it client-side.
3. Expect HTTP 400 / ValidationException with Message "1 validation error detected: Value 'updated-date' at 'sortBy' failed to satisfy constraint: Member must satisfy enum value set: [last-accessed-date, name, created-date, last-changed-date]".
   Observe the same status and code but the set rendered as [created-date, last-accessed-date, last-changed-date, name].
4. Compare with the sibling refusal in the same flow, which is already correct: ListSecrets(Filters=[{"Key":"secret-name","Values":[N]}]) -> "...at 'filters.1.member.key' ... enum value set: [all, owning-service, name, tag-key, description, tag-value, primary-region]".
5. Positive controls that must also pass after the fix: ListSecrets(Filters=[{"Key":"name","Values":[N]}], MaxResults=100) -> [N]; ListSecrets(Filters=[{"Key":"name","Values":[N]}], SortBy="last-changed-date") -> [N].
6. ForceDeleteWithoutRecovery the secret.
