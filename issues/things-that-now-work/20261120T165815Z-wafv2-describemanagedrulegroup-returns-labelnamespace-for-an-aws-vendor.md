Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wafv2.managed-rule-group-describe-catalog-contents
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2.managed-rule-group-describe-catalog-contents|run#19|AssertionError|assert "LabelNamespace" not in crs, (
Issue-Commit: 8345989f6f1c1ad254046c37ef06556dbc8ed377

# wafv2 DescribeManagedRuleGroup returns LabelNamespace for an AWS-vendor managed rule group, which real WAF omits

> _Note: this issue was identified against the code base at commit `8345989f6f1c1ad254046c37ef06556dbc8ed377`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeManagedRuleGroup

## Description
One net-new root cause in wafv2's managed-rule-group catalogue read.

Cause 1 of 1: DescribeManagedRuleGroup populates the optional output member `LabelNamespace` for AWS-vendor managed rule groups, where real WAF leaves the member off the response entirely.

AWS: Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json) models DescribeManagedRuleGroupResponse$LabelNamespace as an OPTIONAL LabelName (@length 1..1024, pattern ^[0-9A-Za-z_\-:]+$) documented as "The label namespace prefix for this rule group. All labels added by rules in this rule group have this prefix." Being optional, the model does not require it to be present. Recorded behavior only: for a vendor-published AWS managed rule group (VendorName="AWS", e.g. AWSManagedRulesCommonRuleSet, scope REGIONAL) the recorded 200 response carries NO LabelNamespace key at all; a client that needs the group's namespace derives it from the four leading segments of the fully-qualified names in AvailableLabels (for the core rule set, `awswaf:managed:aws:core-rule-set:`). The AWS-vendor path is the one recorded; nothing here states what a third-party vendor group returns, so only the AWS-vendor case is a requirement.

Emulator: the same call returns a 200 whose body includes LabelNamespace, set to the group's derived namespace (observed `awswaf:managed:aws:core-rule-set:` for AWSManagedRulesCommonRuleSet — the value is the group's own published namespace, not a literal requirement; the defect is that the member is present at all). The replayed check fails at `assert "LabelNamespace" not in crs` with "DescribeManagedRuleGroup does not report LabelNamespace for an AWS-vendor group; the namespace is derivable from AvailableLabels. Got 'awswaf:managed:aws:core-rule-set:'". No status code or error code diverges: this is a response-shape divergence on the success path.

Why: a client that distinguishes a vendor-published namespace from a locally-derived one gets the wrong answer — code that branches on `if 'LabelNamespace' in response` (or on the field being absent to fall back to parsing AvailableLabels) takes the opposite branch against the emulator than against AWS, and SDK round-trip/snapshot comparisons of the response shape diverge. Because the member is emitted unconditionally, no caller can ever exercise the absent-field path the real service always takes for AWS-vendor groups.

Fix: the DescribeManagedRuleGroup handler's output construction — services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/describe_managed_rule_group.rs sets `label_namespace: Some(group.managed_label_namespace())` unconditionally; it must leave the member unset (None) for AWS-vendor catalogue entries so the serializer omits the key, while AvailableLabels continues to publish the fully-qualified label names the namespace is read from. The rest of that check's catalogue assertions (per-group rules, capacity 700 for the core rule set, per-group label namespaces, Bot Control's ConsumedLabels, the us-east-1 SNS topic, version pinning) were not reached after this assertion, so re-replay the check after the fix to confirm nothing further diverges.

Traces: wafv2.managed-rule-group-describe-catalog-contents.

No existing open issue covers DescribeManagedRuleGroup (closest open wafv2 managed-rule issue, 20261120T102658Z-wafv2-putmanagedrulesetversions-misclassifies-an-invalid-managed-rule-set.md, is about PutManagedRuleSetVersions error classification and does not cover this).

## Evidence
Check: wafv2.managed-rule-group-describe-catalog-contents [mismatch]

Expected (real AWS, recorded): DescribeManagedRuleGroup(VendorName="AWS", Name="AWSManagedRulesCommonRuleSet", Scope="REGIONAL") -> HTTP 200 whose body has no `LabelNamespace` member; the namespace is only obtainable from the fully-qualified names in `AvailableLabels`.

Actual (emulator): HTTP 200 with `LabelNamespace` present.
Assertion failure:
  File "check_wafv2_managed_rule_group_describe_catalog_contents.py", line 144, in run
    assert "LabelNamespace" not in crs
  AssertionError: DescribeManagedRuleGroup does not report LabelNamespace for an AWS-vendor group; the namespace is derivable from AvailableLabels. Got 'awswaf:managed:aws:core-rule-set:'

Source: services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/describe_managed_rule_group.rs:321 — `label_namespace: Some(group.managed_label_namespace()),` (always Some).

Model: DescribeManagedRuleGroupResponse$LabelNamespace is optional (LabelName, len 1..1024), so omitting it is wire-legal.

## Steps to Reproduce
1. Boot the emulator and point a wafv2 client at it (REGIONAL scope).
2. Call ListAvailableManagedRuleGroups(Scope="REGIONAL") and confirm the AWS-vendor catalogue lists AWSManagedRulesCommonRuleSet.
3. Call DescribeManagedRuleGroup(VendorName="AWS", Name="AWSManagedRulesCommonRuleSet", Scope="REGIONAL").
4. Inspect the response dict: the emulator includes a `LabelNamespace` key; real AWS returns the response with that key absent, and the namespace must instead be read off the `AvailableLabels` entries.
5. Equivalent check replay: wafv2.managed-rule-group-describe-catalog-contents.
