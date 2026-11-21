Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: network-security-manager.delete-rule-blocked-while-associated-with-policy
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager.delete-rule-blocked-while-associated-with-policy|run#11|AssertionError|assert code == "ConflictException", (
Issue-Commit: f2831bfd9793e6f7d4548f15bac1de57747f4451

# network-security-manager DeleteRule is an unimplemented scaffold: answers 501 NotImplemented instead of deleting the rule (204) or refusing with 409 ConflictException while a policy still references it

> _Note: this issue was identified against the code base at commit `f2831bfd9793e6f7d4548f15bac1de57747f4451`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteRule

## Description
One net-new root cause in the Network Security Manager (PiccoloCustomerAPIService, restJson1) rule lifecycle, found replaying the simparity-behavioral-aws-v1 suite against the emulator built from this checkout.

=== Cause 1 of 1: DeleteRule is never served — every call answers HTTP 501 `NotImplemented`, so neither the dependency-protection refusal nor the successful delete exists ===
AWS: Smithy model com.amazonaws.networksecuritymanager#DeleteRule — DELETE /rules/{ruleIdentifier}, @idempotent, success @http code 204 with an empty body (output targets Unit); input `ruleIdentifier` is @httpLabel + @required (RuleIdentifier, @length 1..1010), documented "The identifier of the rule. This is the rule's Amazon Resource Name (ARN)." Modeled errors: AccessDeniedException (403), ConflictException (409), InternalServerException (500, retryable), ThrottlingException (429), ValidationException (400). ConflictException is documented "The request conflicts with the current state of the resource. For example, the resource was modified concurrently, or it is in a state that does not allow the requested operation." and carries @required `message` plus optional `resourceId`/`resourceType`. Two behaviors are required of this operation and the rule state behind it:
  (a) While a published policy still references the rule (CreatePolicy with associatedTemplateAndRuleList=[{ruleIdentifier: <that rule's ARN>}]), DeleteRule is REFUSED with HTTP 409 / error code ConflictException and Error.Message "The resource is currently in use and cannot be modified" — that whole sentence is fixed text for this error class; no part of it is taken from the request or from state. The rule must survive the refusal: an immediately following GetRule still returns it with the same `ruleId`.
  (b) Once the only referencing policy has been deleted (DeletePolicy), the SAME DeleteRule call succeeds with 204 and the rule is really gone: GetRule for that identifier then answers HTTP 404 / ResourceNotFoundException with Error.Message "Rule not found: <ruleId>" — fixed text "Rule not found: " plus one variable part, the rule id taken from the request's identifier (a recording's own id value is just one run's fill of that template).
Emulator: `services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/delete_rule.rs` still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker (line 4) and its `handle` body returns `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteRule"))` (line 184), so every DeleteRule answers HTTP 501 with error code `NotImplemented` and the message "operation DeleteRule is scaffolded but not implemented in this emulator". In the replay the in-use delete gave code 'NotImplemented' where ConflictException/409 was required, and the positive-control delete-after-DeletePolicy is unreachable for the same reason. Nothing ever removes a rule from state, which is also why the service's GetRule miss path is never exercised (noted as a hypothesis in the open issue 20261118T031240Z-network-security-manager-rule-read-path-and-response.md, which faults GetRule's own contract and does NOT own DeleteRule; no open issue covers this operation).
Why: a client cannot delete a rule at all, and cannot discover that a rule is pinned by a policy. 501 is not a modeled outcome of this operation, so SDK error handling (`except nsm.exceptions.ConflictException` to unwind a policy before retrying the delete) is dead code and a 501 surfaces as an unexpected server fault rather than a client-correctable conflict; teardown and reconciliation loops leave rules behind forever, and any test that asserts a deleted rule is gone (GetRule → ResourceNotFoundException) can never pass.
Fix: implement DeleteRule in its handler (the single file above) and the rule entity state it mutates: resolve `ruleIdentifier` as an ARN against the rule store, refuse with ConflictException (409, the fixed message above) while any policy's associatedTemplateAndRuleList still references the rule and leave the rule untouched, otherwise remove the rule and return the modeled empty 204; then drop the SCAFFOLDED marker line. The removal must be visible to the rule read path so GetRule produces its modeled 404 afterwards.
Traces: behavioral check network-security-manager.delete-rule-blocked-while-associated-with-policy (group dependency-protection).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout: 0/11 checks passed; network-security-manager 0/9.

[mismatch] network-security-manager.delete-rule-blocked-while-associated-with-policy
  AssertionError: deleting a rule referenced by a policy gave 'NotImplemented', expected ConflictException
  (check_network_security_manager_delete_rule_blocked_while_associated_with_policy.py line 80)
  The check first created a rule (CreateRule: ruleName, firewallType=WAF, ruleType=CONFIGURATION, configuration={"DefaultAction":{"Allow":{}}}, isPublished=true) and a policy referencing it (CreatePolicy: priority=100, firewallType=WAF, associatedTemplateAndRuleList=[{ruleIdentifier: <ruleArn>}], wafConfig existingCustomerWebACLResolution=NO_REMEDIATION / conflictResolution=MERGE_WHERE_APPLICABLE, isPublished=true) — both answered, so the service is serving.
  Required of the DeleteRule on the referenced rule: HTTP 409, Error.Code ConflictException, Error.Message "The resource is currently in use and cannot be modified", with the rule still readable afterwards (GetRule returns the same ruleId).
  Observed: HTTP 501, Error.Code NotImplemented, Error.Message "operation DeleteRule is scaffolded but not implemented in this emulator".
  Unreachable because of the same stub: after DeletePolicy, DeleteRule must return 204 and GetRule must then answer 404 ResourceNotFoundException with message "Rule not found: <ruleId>".
  model: DeleteRule DELETE /rules/{ruleIdentifier} code 204, @idempotent, output Unit; errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ThrottlingException 429, ValidationException 400. ConflictException: @error client, @httpError 409, required message + optional resourceId/resourceType.
  handler: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/delete_rule.rs line 4 ('//! SCAFFOLDED, NOT IMPLEMENTED:') and line 184 (Err(Scaffolded::not_implemented("DeleteRule"))).

## Steps to Reproduce
Against the emulator's network-security-manager endpoint (botocore client "network-security-manager"):
1. nsm.create_rule(ruleName=<unique>, firewallType="WAF", ruleType="CONFIGURATION", configuration={"DefaultAction": {"Allow": {}}}, isPublished=True) → note ruleArn/ruleId.
2. nsm.create_policy(policyName=<unique>, priority=100, firewallType="WAF", associatedTemplateAndRuleList=[{"ruleIdentifier": ruleArn}], policyConfiguration={"remediationEnabled": False, "resourcesCleanUp": True, "wafConfig": {"existingCustomerWebACLResolution": "NO_REMEDIATION", "conflictResolution": "MERGE_WHERE_APPLICABLE"}}, isPublished=True) → note policyArn.
3. nsm.delete_rule(ruleIdentifier=ruleArn) — expect HTTP 409 ConflictException with message "The resource is currently in use and cannot be modified"; observe HTTP 501 NotImplemented. Then nsm.get_rule(ruleIdentifier=ruleArn) must still return the same ruleId.
4. nsm.delete_policy(policyIdentifier=policyArn), then nsm.delete_rule(ruleIdentifier=ruleArn) — expect an empty 204 success; then nsm.get_rule(ruleIdentifier=ruleArn) must raise ResourceNotFoundException (404) with message "Rule not found: <ruleId>".
Or re-replay the behavioral check network-security-manager.delete-rule-blocked-while-associated-with-policy.
