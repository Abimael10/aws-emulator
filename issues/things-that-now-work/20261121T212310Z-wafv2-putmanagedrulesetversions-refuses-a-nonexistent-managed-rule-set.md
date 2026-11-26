Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected wafv2.put-managed-rule-set-versions-invalid-version-format-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected|run#8|AssertionError|assert code == "WAFNonexistentItemException", f"got {code!r} instead"
Issue-Key: wafv2.put-managed-rule-set-versions-invalid-version-format-rejected|run#9|AssertionError|assert code == "WAFNonexistentItemException", f"got {code!r} instead"
Issue-Commit: 8da13561ffbb6e76217d22709198f3ddd173c276

# wafv2 PutManagedRuleSetVersions refuses a nonexistent managed rule set with WAFInvalidParameterException instead of WAFNonexistentItemException (the body's rule-group reference is resolved before the put's target)

> _Note: this issue was identified against the code base at commit `8da13561ffbb6e76217d22709198f3ddd173c276`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutManagedRuleSetVersions

## Description
This is ONE issue with ONE cause.

Cause 1 of 1: PutManagedRuleSetVersions resolves the rule-group reference carried in `VersionsToPublish` against the store BEFORE it resolves the managed rule set the put targets, so a call naming a managed rule set that does not exist is refused with the wrong error class whenever its body also carries a rule-group ARN that does not resolve.

AWS: Smithy (com.amazonaws.wafv2#AWSWAF_20190729) PutManagedRuleSetVersions takes required `Name` (EntityName, @length 1..128, @pattern ^[\w\-]+$), `Scope`, `Id` and `LockToken` (both EntityId/LockToken, @length 1..36, @pattern ^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$), plus optional `RecommendedVersion` and the `VersionsToPublish` map; it models WAFInvalidParameterException, WAFNonexistentItemException, WAFInvalidOperationException and WAFOptimisticLockException (all client, httpError 400) and WAFInternalErrorException (500). WAFNonexistentItemException is documented "WAF couldn’t perform the operation because your resource doesn't exist. If you've just created a resource that you're using in this operation, you might just need to wait a few minutes." The recorded behavior for a `(Scope, Name, Id)` triple that no managed rule set of the account carries is HTTP 400 `WAFNonexistentItemException` with the message "AWS WAF couldn’t perform the operation because your resource doesn’t exist." — that whole sentence is FIXED text for this error class (note the typographic apostrophes U+2019 in "couldn’t" and "doesn’t"); it carries no request- or state-derived value. It is answered whatever the request body references: the target's existence is decided before the references the body carries are resolved. Nothing is published either way.

Emulator: HTTP 400 `WAFInvalidParameterException`. `services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/put_managed_rule_set_versions.rs` loops over `rule_group_references(&input)` and refuses with `crate::state::invalid_arn_exception(&arn)` as soon as one well-formed rule-group ARN fails to resolve in the requested scope, and only AFTER that loop does it ask `state.managed_rule_set(&input.scope, &input.name, &input.id)` and raise `WAFNonexistentItemException`. So for a nonexistent target whose body names a nonexistent rule group — the ordinary first call a vendor makes by mistake — the caller never sees the not-found class. The handler's own module doc states the required order and the code contradicts it: "Everything a judgement needs the STORE for is taken in the order WAF resolves it: the managed rule set the put TARGETS first, so an identity no managed rule set of this account carries is `WAFNonexistentItemException` whatever the body references, and only then whether a well-formed reference the body carries resolves to a rule group of this scope."

Why: a client branches on the exception class to decide what to do. WAFNonexistentItemException means "create the managed rule set, or wait for propagation"; WAFInvalidParameterException means "the request itself is wrong, do not retry". With the two swapped, vendor tooling that would have created the missing managed rule set (or re-polled while it propagated) instead treats the call as a permanently bad request and aborts, and a caller that retries-on-not-found never retries. The refusal also reports the body's ARN as the offending parameter when the actual fault is the target identity, so the one machine-readable statement of what is wrong points at the wrong member.

Fix: the store-resolution ORDER in this operation's handler (`src/ops/put_managed_rule_set_versions.rs`): resolve the put's target `(Scope, Name, Id)` and raise `WAFNonexistentItemException` before the `rule_group_references` resolution loop that raises `invalid_arn_exception`, i.e. restore the order the module doc already specifies. Keep the request-only judgement where it is — an `AssociatedRuleGroupArn` that is no rule-group ARN at all (`unacceptable_rule_group_reference`) is judged from the request alone and stays ahead of every store read. NOTE for the fixer: open issue `20261120T102658Z-wafv2-putmanagedrulesetversions-misclassifies-an-invalid-managed-rule-set.md` is a WIRE issue asking for the opposite precedence on its own recording (a RESOURCE_ARN WAFInvalidParameterException ahead of the lookup) and carries none of these behavioral check ids, so it does not cover this; reconcile the two by splitting the judgement WAF actually splits — a MALFORMED/unusable ARN is a parameter failure decided from the request before any lookup, while a WELL-FORMED reference that merely does not RESOLVE is decided after the target, so both recordings hold.

Traces: wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected, wafv2.put-managed-rule-set-versions-invalid-version-format-rejected.

## Evidence
Behavioral suite replay against the booted emulator (205/235 passed, 87.2%). Two checks under heading `### wafv2`, both [mismatch], fail on the same assertion:

1. wafv2.put-managed-rule-set-versions-nonexistent-rule-group-rejected — check_wafv2_put_managed_rule_set_versions_nonexistent_rule_group_rejected.py line 49: `assert code == "WAFNonexistentItemException", f"got {code!r} instead"` → AssertionError: got 'WAFInvalidParameterException' instead.

2. wafv2.put-managed-rule-set-versions-invalid-version-format-rejected — check_wafv2_put_managed_rule_set_versions_invalid_version_format_rejected.py line 52: same assertion → AssertionError: got 'WAFInvalidParameterException' instead.

Both call PutManagedRuleSetVersions with Scope=REGIONAL, a Name/Id/LockToken no managed rule set carries, and a single VersionsToPublish entry under the well-formed version key "1.0" whose AssociatedRuleGroupArn is a well-formed but nonexistent `arn:aws:wafv2:<region>:<account>:regional/rulegroup/<name>/<uuid>`. Expected: HTTP 400 WAFNonexistentItemException with message "AWS WAF couldn’t perform the operation because your resource doesn’t exist." (fixed text; no variable parts). Observed: HTTP 400 WAFInvalidParameterException, so the subsequent message assertion is never reached. The positive controls in both checks pass (ListAvailableManagedRuleGroups returns a non-empty catalog, ListManagedRuleSets shows nothing was published), so the divergence is the refusal's class alone, not a dead account or a leaked write.

The names, ids, account and region above are this run's generated values; the requirement is the error class and that fixed sentence for any `(Scope, Name, Id)` the account does not carry.

## Steps to Reproduce
1. Boot the emulator from this checkout and run the behavioral suite.
2. Re-replay the two check ids in trace_ids, or reproduce directly: with Scope=REGIONAL, call PutManagedRuleSetVersions with Name=<any EntityName no managed rule set of the account carries>, Id=<any fresh UUID>, LockToken=<any UUID>, and VersionsToPublish={"1.0": {"AssociatedRuleGroupArn": "arn:aws:wafv2:<region>:<account>:regional/rulegroup/<nonexistent-name>/<fresh-uuid>"}}.
3. Expect HTTP 400, Error.Code 'WAFNonexistentItemException', Error.Message "AWS WAF couldn’t perform the operation because your resource doesn’t exist."
4. Observed: HTTP 400 with Error.Code 'WAFInvalidParameterException' naming the body's AssociatedRuleGroupArn.
5. Control: ListManagedRuleSets(Scope='REGIONAL') must not list the Name/Id afterwards (this already holds), and ListAvailableManagedRuleGroups(Scope='REGIONAL') must stay non-empty (also holds).
