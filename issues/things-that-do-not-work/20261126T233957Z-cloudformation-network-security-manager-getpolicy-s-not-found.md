Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected
Issue-Key: cloudformation__network-security-manager|GetPolicy|ResourceNotFoundException
Issue-Commit: e0fb8928b55d063818ff45281a7e96a54cacfd74

# cloudformation__network-security-manager: GetPolicy's not-found message drops the identifier's resource-type segment, stating "Policy not found: <policyId>" where AWS states "Policy not found: policy/<policyId>"

> _Note: this issue was identified against the code base at commit `e0fb8928b55d063818ff45281a7e96a54cacfd74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPolicy

## Description
One net-new cause, found replaying the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout. The check deploys the AWS::NetworkSecurityManager::{Rule,Template,Policy} control stack, deletes the stack, and then reads the removed policy back by its own policyArn; everything up to that last read matches, and only the refusal MESSAGE of the final GetPolicy diverges. The sibling failure in this same group (deploy-network-security-manager-waf-stack-reads-back-then-removed) is reported OWNED by 20261120T162640Z-cloudformation-network-security-manager-2-net-new-causes.md and is excluded from this issue.

Cause 1 of 1: the 404 ResourceNotFoundException for a policyIdentifier that names no policy states only the bare policy id, dropping the resource-type segment the identifier carries.

AWS: Smithy com.amazonaws.networksecuritymanager#GetPolicy — GET /policies/{policyIdentifier}, success @http code 200; input policyIdentifier is @httpLabel @required (PolicyIdentifier, @length 1..1010), documented "The identifier of the policy. This is the policy's Amazon Resource Name (ARN)". Modeled errors: AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ResourceNotFoundException is @error client / @httpError 404 with @required message plus optional resourceId/resourceType, documented "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again." The model does not fix the message string, so the exact wording is recorded behavior only: for a policy ARN whose policy no longer exists AWS answers ResourceNotFoundException with Error.Message "Policy not found: <resource segment of the identifier ARN>". Message anatomy: the FIXED text is "Policy not found: "; the single VARIABLE part is the RESOURCE field of the ARN the caller sent — everything after the ARN's final colon, i.e. "policy/<policyId>", taken from the request. (The recording's own value, policy/1501d71dc554ad34a722909e2c4d43e81, is one run's fill of that template and is not itself a requirement; the behavioral check derives the expectation from the policyArn the emulator itself published, so the requirement is independent of how ids are minted.)

Emulator: status (404), error shape and error code all match — the body's message is "Policy not found: 1501d71dc554ad34a722909e2c4d43e81", i.e. the resource-type prefix "policy/" has been stripped off the variable part. Replay detail verbatim: AssertionError: expected ('ResourceNotFoundException', 'Policy not found: policy/1501d71dc554ad34a722909e2c4d43e81'), got ('ResourceNotFoundException', 'Policy not found: 1501d71dc554ad34a722909e2c4d43e81'). The stripping is unconditional: the shared helper reduces any identifier to the part after the last '/'.

Why: the message is part of the contract a client reads. A caller, test, operator runbook, UI string or log-based alert that matches or displays the not-found text for a policy read sees a sentence AWS never sends, and it loses the resource-type qualification AWS puts in the text — with several reads in flight (policy, template, rule all addressed by ARN) the message no longer reproduces the identifier's resource segment, so the message cannot be joined back to the ARN that was asked for. Because the status and error code are right, the divergence is invisible until something reads the text.

Fix: belongs in the GetPolicy miss branch's construction of ResourceNotFoundException and in the shared message helper it delegates to — the helper that turns an identifier into the id quoted in the sentence must keep the identifier's resource segment (resource-type and id as the ARN presents them) for the policy read, instead of truncating at the last '/'. Whether to change the helper for every entity or to let the policy read word its own sentence is the fixer's call, but the other entities' recorded wordings must not regress (GetTemplate's is already filed separately as 20261124T100554Z-network-security-manager-gettemplate-not-found-refusal-states.md, and GetDeployment's is filed alongside this sweep).

Traces: check cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected — its final step, GetPolicy on the policyArn of the policy whose stack was just deleted. Code citations: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/get_policy.rs lines 191-201 (the `store.policy_facts(&input.policy_identifier)` miss branch calls `crate::state::resource_not_found(RESOURCE_TYPE_POLICY, &input.policy_identifier)`); services/.../src/state.rs lines 83-90 (`resource_not_found` formats `"{resource_type} not found: {resource_id}"`) and lines 100-106 (`entity_id_in` ends with `unversioned.split('/').next_back()`, which is what discards the "policy/" segment).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 against the emulator built from this checkout: 7/11 checks passed; 4 mismatched, 0 errored, 0 skipped. Service pass rates: cloudformation__network-security-manager 0/2 (2 failed), network-security-manager 7/9 (2 failed).

Failing check cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected, verdict mismatch:

  File "check_cloudformation__network_security_manager_deploy_network_security_manager_policy_ambiguous_reference_rejected.py", line 297, in run
    expect_error(lambda: nsm.get_policy(policyIdentifier=policies[0]), *not_found)
  File "...", line 154, in expect_error
    assert got == (code, message), f"expected {(code, message)!r}, got {got!r}"
  AssertionError: expected ('ResourceNotFoundException', 'Policy not found: policy/1501d71dc554ad34a722909e2c4d43e81'), got ('ResourceNotFoundException', 'Policy not found: 1501d71dc554ad34a722909e2c4d43e81')

The check builds its expectation from the emulator's own published ARN, so no live-account value is involved:
  policies = [p['policyArn'] for p in paginate(nsm, 'list_policies', 'policies') if p['policyName'] == good_policy_name]
  own_id = policies[0].rsplit(':', 1)[-1]          # the ARN's resource field, e.g. 'policy/<policyId>'
  not_found = ('ResourceNotFoundException', f'Policy not found: {own_id}')

Expected (AWS, recorded): HTTP 404, ResourceNotFoundException, message = "Policy not found: " + the identifier ARN's resource field ("policy/<policyId>").
Actual (emulator): HTTP 404, ResourceNotFoundException, message = "Policy not found: " + the bare policyId only.
Fixed text: "Policy not found: ". Variable part: the resource field of the request's policyIdentifier ARN. Nothing in the message is a per-run value such as a request id or timestamp; the hex id shown is this run's fill of the variable part.

Model evidence (com-amazonaws-networksecuritymanager-piccolocustomerapiservice.json): GetPolicy http {method GET, uri /policies/{policyIdentifier}, code 200}; policyIdentifier @httpLabel @required @length(1,1010); errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ResourceNotFoundException { message @required, resourceId, resourceType }.

Code evidence: src/ops/get_policy.rs:191-201 -> crate::state::resource_not_found(...); src/state.rs:83-90 message = format!("{resource_type} not found: {resource_id}"); src/state.rs:100-106 entity_id_in() truncates the identifier at the last '/'.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the behavioral suite (simparity-behavioral-aws-v1), or run only check cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected.
3. The check creates a CloudFormation stack with AWS::NetworkSecurityManager::Rule + ::Template + ::Policy where the policy's AssociatedTemplateAndRuleList entry names only the template (the positive control), waits for CREATE_COMPLETE, reads the policy back (ACTIVE, associatedTemplateAndRuleList == [{templateArn}]), then DeleteStack to DELETE_COMPLETE.
4. Call GetPolicy with policyIdentifier = that policy's policyArn (as published by ListPolicies), i.e. GET /policies/arn:aws:network-security-manager:<region>:<account>:policy/<policyId>.
5. Expect HTTP 404 ResourceNotFoundException with message "Policy not found: policy/<policyId>" (the ARN's resource field). Observe HTTP 404 ResourceNotFoundException with message "Policy not found: <policyId>" — the "policy/" segment is missing.
