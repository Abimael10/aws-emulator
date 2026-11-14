Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected wafv2.put-managed-rule-set-versions-invalid-version-format-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected|run#8|AssertionError|assert code == "WAFNonexistentItemException", f"got {code!r} instead"
Issue-Key: wafv2.put-managed-rule-set-versions-invalid-version-format-rejected|run#9|AssertionError|assert code == "WAFNonexistentItemException", f"got {code!r} instead"
Issue-Commit: 398174cd9c472752d418d5e79e2cc5fa7d1d5d88

# wafv2 PutManagedRuleSetVersions: a well-formed AssociatedRuleGroupArn naming no rule group is refused WAFInvalidParameterException where AWS returns WAFNonexistentItemException

> _Note: this issue was identified against the code base at commit `398174cd9c472752d418d5e79e2cc5fa7d1d5d88`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutManagedRuleSetVersions

## Description
Cause 1 of 1: PutManagedRuleSetVersions misclassifies the refusal when the call addresses a managed rule set that does not exist and its VersionsToPublish entries carry a syntactically well-formed wafv2 rule-group ARN that resolves to no rule group. Failing checks: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected, wafv2.put-managed-rule-set-versions-invalid-version-format-rejected.

AWS: Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729 model, PutManagedRuleSetVersions) models both WAFInvalidParameterException (client, HTTP 400) and WAFNonexistentItemException (client, HTTP 400). WAFNonexistentItemException is documented as "WAF couldn't perform the operation because your resource doesn't exist...". Recorded behavior: for a request whose Name/Scope/Id name no managed rule set of the account and whose VersionsToPublish value carries a well-formed arn:aws:wafv2:<region>:<account>:regional/rulegroup/<name>/<uuid> that names no existing rule group, AWS refuses with HTTP 400, error code WAFNonexistentItemException and the message "AWS WAF couldn't perform the operation because your resource doesn't exist." (that sentence is fixed text for this error class, with no variable parts; the apostrophes are the U+2019 right single quotation mark as WAF emits them). i.e. AWS resolves the TARGET managed rule set identity before it judges whether a well-formed rule-group reference resolves, and a nonexistent target wins. The same holds when the version key is itself a valid VersionKeyString such as "1.0" — the only thing unresolvable is the target and the referenced rule group.

Emulator: both calls come back with error code WAFInvalidParameterException instead (assertion: "got 'WAFInvalidParameterException' instead" of "WAFNonexistentItemException"). The handler judges rule-group references against state before resolving the put's target: services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/put_managed_rule_set_versions.rs, the loop over rule_group_references at lines 291-313 returns WAFInvalidParameterException (crate::state::invalid_arn_exception) for any well-formed rule-group ARN that does not resolve in the requested scope, and only afterwards (lines 335-352) does it emit WAFNonexistentItemException for an unknown (Scope, Name, Id). The module doc at lines 11-22 states that ordering explicitly, so the ordering, not a stray branch, is the defect.

Why: a client (or vendor tooling) that distinguishes "the managed rule set does not exist, create it" from "the ARN I passed is unusable" branches the wrong way: WAFNonexistentItemException is the retry/eventual-consistency signal WAF documents ("you might just need to wait a few minutes"), while WAFInvalidParameterException tells the caller its request body is wrong. Any code keyed on the error code — retry wrappers, CLI/SDK error handling, IaC providers — takes the wrong branch, and the parameter error also carries a Field/Parameter/Reason the recording does not contain.

Fix: the operation's handler and its refusal ordering — resolve the target managed rule set identity (Scope, Name, Id) and emit WAFNonexistentItemException before the state-resolution half of the AssociatedRuleGroupArn judgement; keep the pure-syntax ARN check (an ARN that is not a rule-group ARN at all) where it is, since no recording contradicts it. File and line citations are under Traces.

Traces: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected (nonexistent rule-group ARN, version key "1.0"); wafv2.put-managed-rule-set-versions-invalid-version-format-rejected (same shape, asserts the not-found class and its exact sentence). Code: services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/put_managed_rule_set_versions.rs:257-264, :291-313, :335-352.

Not covered by any open issue: 20261026T120000Z-wafv2-two-refusals-carry-the-right-class-but-not-wafs-own-sentence.md owns only the invalid version-KEY ValidationException wording for this operation (right class, wrong sentence); this finding is a wrong error CLASS on a different input shape, and 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md is about operations answering 200 instead of refusing.

## Evidence
Check wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected:
  AssertionError at check_wafv2_put_managed_rule_set_versions_nonexistent_rule_group_rejected.py:49 — assert code == "WAFNonexistentItemException": got 'WAFInvalidParameterException' instead.
  Request: PutManagedRuleSetVersions(Name=<unique fake name>, Scope=REGIONAL, Id=<uuid>, LockToken=<uuid>, VersionsToPublish={"1.0": {"AssociatedRuleGroupArn": "arn:aws:wafv2:<region>:<account>:regional/rulegroup/<nonexistent-name>/<uuid>"}}).
  Expected: HTTP 400, Error.Code = WAFNonexistentItemException, Error.Message = "AWS WAF couldn't perform the operation because your resource doesn't exist." (fixed text, U+2019 apostrophes, no request-derived values).
  Actual: HTTP 400 with Error.Code = WAFInvalidParameterException (the invalid-ARN envelope: Field RESOURCE_ARN, Parameter = the rejected ARN, Reason = the ARN-not-valid sentence).

Check wafv2.put-managed-rule-set-versions-invalid-version-format-rejected:
  AssertionError at check_wafv2_put_managed_rule_set_versions_invalid_version_format_rejected.py:52 — assert code == "WAFNonexistentItemException": got 'WAFInvalidParameterException' instead.
  Same request shape (valid version key "1.0", well-formed rule-group ARN naming nothing), same expected refusal, same wrong class returned.
  Positive control in both checks passed: ListAvailableManagedRuleGroups(Scope=REGIONAL) returned a non-empty catalog, so the refusal is target-specific, not an account-wide failure.

Model grounding: smithy_query(operation=PutManagedRuleSetVersions) lists errors WAFInternalErrorException(500), WAFInvalidOperationException(400), WAFInvalidParameterException(400), WAFNonexistentItemException(400), WAFOptimisticLockException(400).

## Steps to Reproduce
1. Boot the emulator and point a botocore wafv2 client at it.
2. Call PutManagedRuleSetVersions with Name=<any name matching ^[\\w\\-]+$ that no managed rule set uses>, Scope="REGIONAL", Id=<random uuid>, LockToken=<random uuid>, VersionsToPublish={"1.0": {"AssociatedRuleGroupArn": "arn:aws:wafv2:<region>:<account>:regional/rulegroup/does-not-exist/00000000-0000-0000-0000-000000000000"}}.
3. Observe Error.Code == "WAFInvalidParameterException".
4. Expected: Error.Code == "WAFNonexistentItemException" with message "AWS WAF couldn't perform the operation because your resource doesn't exist." (U+2019 apostrophes), HTTP 400, and nothing published — ListManagedRuleSets(Scope="REGIONAL") must still not contain that Name/Id (the emulator already satisfies the no-effect part).
5. Re-replay: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected and wafv2.put-managed-rule-set-versions-invalid-version-format-rejected.
