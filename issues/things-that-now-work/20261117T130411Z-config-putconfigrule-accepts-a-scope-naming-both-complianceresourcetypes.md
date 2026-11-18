Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: config.config-rule-scope-tag-only-accepted-and-combined-update-rejected
Issue-Shape-Id: com.amazonaws.configservice#StarlingDoveService
Issue-Key: config.config-rule-scope-tag-only-accepted-and-combined-update-rejected|_error#18|AssertionError|raise AssertionError(f"expected a rejection, but it succeeded: {result!r}")
Issue-Commit: cea05121548b3a16dc11942b4ef340d67b81162b

# config PutConfigRule accepts a Scope naming both ComplianceResourceTypes and TagKey, instead of refusing 400 InvalidParameterValueException "Scope cannot be applied to both resource and tag"

> _Note: this issue was identified against the code base at commit `cea05121548b3a16dc11942b4ef340d67b81162b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutConfigRule

## Description
ONE CAUSE must be closed.

Cause 1 of 1: PutConfigRule does not enforce the mutual exclusion between a Scope's resource dimension (ComplianceResourceTypes / ComplianceResourceId) and its tag dimension (TagKey / TagValue), so an update that adds a TagKey to an already resource-scoped rule is accepted with HTTP 200.

AWS:      A Config rule's Scope selects resources along exactly ONE dimension. Smithy: the Scope shape's own documentation states "The scope can include one or more resource types, a combination of a tag key and value, or a combination of one resource type and one resource ID" — the three forms are alternatives, not combinable. A PutConfigRule whose ConfigRule.Scope carries BOTH ComplianceResourceTypes and a TagKey is refused with the modeled client error InvalidParameterValueException (Smithy: PutConfigRule.errors lists InvalidParameterValueException, errorKind client, httpError 400 defaulted), HTTP 400, error code "InvalidParameterValueException", message "Scope cannot be applied to both resource and tag". That message is entirely fixed text — it interpolates nothing from the request. Because the call is refused, the named rule keeps the Scope it already had: a rule previously created with Scope={"ComplianceResourceTypes": [...]} still describes back with exactly those resource types and with NO TagKey member, and stays ConfigRuleState ACTIVE. The single-dimension neighbours are both legal and are the check's positive controls: a Scope naming only TagKey and TagValue (no resource types at all) is accepted and describes back as exactly those two members, and a Scope naming only ComplianceResourceTypes is accepted.

Emulator: answers HTTP 200 with an empty JSON body (content-type application/x-amz-json-1.1, content-length 2, ResponseMetadata.HTTPStatusCode 200) for the combined Scope and overwrites the existing rule, so no InvalidParameterValueException is ever raised for this cross-field combination.

Why:      A client that relies on AWS to reject a combined resource+tag Scope gets a rule whose selection semantics do not exist on real AWS: the stored Scope names both dimensions, and what the rule actually evaluates against diverges between the emulator and AWS. Worse, the accepted write MUTATES a rule AWS would have left untouched — code that catches InvalidParameterValueException and relies on the pre-existing Scope surviving the failed update instead finds the rule silently reconfigured, and the InvalidParameterValueException branch for this class is dead code against the emulator.

Fix:      PutConfigRule's handler validation — add the Scope cross-field check (reject when any of ComplianceResourceTypes/ComplianceResourceId is present together with TagKey/TagValue) and run it BEFORE the rule is persisted, so the stored rule is unchanged, raising 400 InvalidParameterValueException with the fixed message above. Related open issue 20261022T190340Z-config-putconfigrule-and-putdeliverychannel-are-overly-permissive-inconsistent covers other PutConfigRule cross-field gaps (ComplianceResourceId without exactly one ComplianceResourceType, MaximumExecutionFrequency on a change-triggered managed rule, SourceDetails on an Owner=AWS rule) but does NOT cover the resource-vs-tag mutual exclusion, so this cause is net new.

Traces:   config.config-rule-scope-tag-only-accepted-and-combined-update-rejected; handler at services/service-com-amazonaws-configservice-starlingdoveservice/src/ops/put_config_rule.rs.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (emulator serving: 158/182 checks passed, bootstrap_signal false).

Failing check config.config-rule-scope-tag-only-accepted-and-combined-update-rejected [mismatch]:
  File "check_config_config_rule_scope_tag_only_accepted_and_combined_update_rejected.py", line 31, in _error
    raise AssertionError(f"expected a rejection, but it succeeded: {result!r}")
  AssertionError: expected a rejection, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', 'content-length': '2', ...}, 'RetryAttempts': 0}}

The refused call, as the check issues it (names are per-run values from ctx.unique_name and are not part of the requirement):
  PutConfigRule ConfigRule={'ConfigRuleName': <existing resource-scoped rule>, 'Source': {'Owner': 'AWS', 'SourceIdentifier': 'S3_BUCKET_VERSIONING_ENABLED'}, 'Scope': {'ComplianceResourceTypes': ['AWS::S3::Bucket'], 'TagKey': <tag key>}}

Expected (AWS): HTTP 400, Error.Code 'InvalidParameterValueException', Error.Message 'Scope cannot be applied to both resource and tag' (fixed text, no interpolation), and DescribeConfigRules for that rule still reporting ConfigRuleState 'ACTIVE', Scope.ComplianceResourceTypes == ['AWS::S3::Bucket'] and no 'TagKey' member.
Actual (emulator): HTTP 200, empty body, and the rule's Scope replaced by the combined resource+tag Scope.

Positive controls in the same check succeeded against the emulator, so the gap is specific to the combined Scope and not a blanket refusal/acceptance: Scope={'TagKey': k, 'TagValue': v} with no resource types was accepted and described back as exactly those two members, and Scope={'ComplianceResourceTypes': ['AWS::S3::Bucket']} was accepted and reached ACTIVE.

Model grounding (services/service-com-amazonaws-configservice-starlingdoveservice/smithy-model/com-amazonaws-configservice-starlingdoveservice.json): PutConfigRule models InvalidParameterValueException (client, httpError 400 defaulted); shape Scope documents the three alternative scope forms (resource types; a tag key and value; one resource type and one resource ID).

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this branch.
2. Replay the behavioral suite simparity-behavioral-aws-v1, or re-replay the check id in trace_ids.
3. Ensure a configuration recorder exists (PutConfigurationRecorder with a Config service role), as the check does.
4. PutConfigRule with ConfigRule={'ConfigRuleName': 'r-tagonly', 'Source': {'Owner': 'AWS', 'SourceIdentifier': 'S3_BUCKET_VERSIONING_ENABLED'}, 'Scope': {'TagKey': 'k', 'TagValue': 'v'}} and confirm DescribeConfigRules returns Scope == {'TagKey': 'k', 'TagValue': 'v'} once ACTIVE (positive control — passes today).
5. PutConfigRule with ConfigRule={'ConfigRuleName': 'r-res', 'Source': {'Owner': 'AWS', 'SourceIdentifier': 'S3_BUCKET_VERSIONING_ENABLED'}, 'Scope': {'ComplianceResourceTypes': ['AWS::S3::Bucket']}} and wait for ConfigRuleState ACTIVE.
6. PutConfigRule again for 'r-res' with Scope={'ComplianceResourceTypes': ['AWS::S3::Bucket'], 'TagKey': 'k2'}. Observed: HTTP 200 with an empty body. Expected: HTTP 400 InvalidParameterValueException, message 'Scope cannot be applied to both resource and tag'.
7. DescribeConfigRules(ConfigRuleNames=['r-res']): observed Scope now carries TagKey 'k2'. Expected: unchanged Scope with only ComplianceResourceTypes ['AWS::S3::Bucket'] and no TagKey, state ACTIVE.
