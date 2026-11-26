Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connecthealth.delete-domain-not-found
Issue-Shape-Id: com.amazonaws.connecthealth#ConnectHealth
Issue-Key: connecthealth.delete-domain-not-found|run#4|AssertionError|assert code == "AuthorizerConfigurationException", f"expected AuthorizerConfigurationException, got {code!r}"
Issue-Commit: a0aa0de572b711afa73e89ad307a81e6c57d7c90

# connecthealth DeleteDomain: a never-allocated domainId is refused the modelled 404 ResourceNotFoundException instead of the recorded 403 AuthorizerConfigurationException with a null message

> _Note: this issue was identified against the code base at commit `a0aa0de572b711afa73e89ad307a81e6c57d7c90`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDomain

## Description
REOPENS 20261125T034254Z-connecthealth-deletedomain-regressed-a-never-allocated-domainid-is.md: its fix left the call refused. The call is still refused, but with the wrong refusal class — the handler now answers EVERY unresolvable domainId the modelled 404, so the never-allocated arm that fix installed is gone again. This is the same oscillation that file and 20261115T073931Z-connecthealth-deletedomain-answers-a-never-allocated-domainid-the.md already went through, not a newly discovered behaviour.

One net-new cause.

Cause 1 of 1: DeleteDomain addressed at a syntactically valid domainId that was never allocated to any account is refused with the operation's modelled not-found error instead of the authorization-layer refusal the recording shows for that request class. Failing check: connecthealth.delete-domain-not-found.

AWS: recorded behavior only for this request class. The model (smithy_query operation=DeleteDomain, services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json) declares DeleteDomain as DELETE /domain/{domainId}, success 200 (codeDefaulted), @idempotent, input domainId -> DomainId (@required, @httpLabel, @length 20..25, pattern ^(hai-|dom-)[a-z0-9]+$), output domainId/arn/status all @required, and exactly ONE error: ResourceNotFoundException (errorKind client, httpError 404, structure { message: String }, "This error is thrown when the requested resource is not found."). AuthorizerConfigurationException is declared on NO shape of this service's model and is not in its common error set, so it is not model-derivable: the requirement here comes from the recording the check replays. The recording for a domainId that is well-formed but was never allocated to any account is a refusal whose error code is AuthorizerConfigurationException and whose error detail carries NO message member at all (a null Message, not an empty sentence) — the authorization layer refuses before the resource layer is ever consulted, so there is no resource name to put in a message. The code and the absent message are the requirement; the particular id the check uses (a 24-character dom- id) is a request value filling the template, not part of it. The modelled 404 ResourceNotFoundException with message "Domain not found: " + the requested domainId remains the requirement for an id this endpoint ONCE held and no longer holds (created-then-deleted, or purged) — that is the arm open issue 20261125T195918Z-connecthealth-deletedomain-an-absent-domainid-is-refused-403.md asks for, and it must stay.

Emulator: services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs now collapses the whole unresolvable-id space onto the modelled error: when store.held_domain_mut(&region, &input.domain_id) resolves nothing it returns ResourceNotFoundException { message: Some(domain_not_found_message(&domain_id)) } unconditionally, so the never-allocated id is answered HTTP 404, x-amzn-errortype ResourceNotFoundException, body message "Domain not found: <domainId>". The allocation ledger the previous build consulted (state.rs allocated_domain_ids / has_allocated_domain) is no longer read on this path at all. The replay reports: expected AuthorizerConfigurationException, got 'ResourceNotFoundException'.

Why: a client cannot tell an unauthorized or cross-account/never-allocated id from a resource of its own that is already gone. Teardown and delete-if-exists code keys on ResourceNotFoundException to decide "already deleted, nothing to do"; against this emulator it reaches that conclusion for ids that were never the caller's at all, so a typo'd or foreign domainId is silently swallowed as a successful cleanup instead of surfacing as a refusal the operator must look at. Code that keys on the authorization refusal (credential refresh, cross-account probes) never sees it, and the emulator additionally serves an error message naming an id it has no knowledge of, where AWS sends no detail member at all.

Fix: the id-resolution arm of the DeleteDomain handler (services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs) together with the allocation bookkeeping in src/state.rs it must consult. The handler needs TWO arms, not one: an id this endpoint minted and no longer holds (deleted/purged, including the repeat-delete path) is the modelled 404 ResourceNotFoundException with the shared message from src/service/error_detail.rs; an id that was never allocated at all is the recorded authorization refusal — error code AuthorizerConfigurationException at its recorded status, with no error-detail message member emitted. CONFLICT TO RECONCILE, do not simply flip the arms back: open wire issue 20261125T195918Z-connecthealth-deletedomain-an-absent-domainid-is-refused-403.md requires the 404 for fixture connecthealth-domain-lifecycle#task#5, whose id is also not minted during that fixture, and the pure ledger test is exactly what made that fixture fail. The predicate must be chosen so BOTH that wire step and this behavioral check pass in the SAME build (e.g. seed/derive the allocation ledger so a recorded fixture's domainId counts as allocated, rather than widening either arm to cover every id the store does not currently hold). A build where only one of the two corpora is green is the oscillation, not the fix.

Traces: behavioral check connecthealth.delete-domain-not-found (its positive control — DeleteDomain of a live domain answering 200 with domainId/status, and GetDomain afterwards answering 404 ResourceNotFoundException with "Domain not found: <domainId>" — passes today and must keep passing). Related open issue 20261125T195918Z-connecthealth-deletedomain-an-absent-domainid-is-refused-403.md carries the wire step for the OTHER arm and does not carry this check's replay key; open issue 20261114T103624Z-connecthealth-3-net-new-causes-in-startpatientinsightsjob-clienttoken.md covers StartPatientInsightsJob only.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 23/26 passed, 1 mismatched, 2 errored, 0 skipped, bootstrap_signal false. The two errored checks (cloudformation__connecthealth deploy-healthagent-domain-*) are marked OWNED by 20261109T181259Z-cloudformation-connecthealth-aws-healthagent-domain-is-not-a.md and are excluded. The cluster below is the only remaining failure.

Check connecthealth.delete-domain-not-found, verdict mismatch, service connecthealth, operation DeleteDomain:
  File "check_connecthealth_delete_domain_not_found.py", line 47, in run
    assert code == "AuthorizerConfigurationException", f"expected AuthorizerConfigurationException, got {code!r}"
  AssertionError: expected AuthorizerConfigurationException, got 'ResourceNotFoundException'

Expected (recorded): DeleteDomain on domainId "dom-" + "2"*20 — well-formed under @length 20..25 and pattern ^(hai-|dom-)[a-z0-9]+$, never allocated to any account — is refused with error code AuthorizerConfigurationException and a NULL error-detail message (the check asserts `message is None`).
Actual (emulator): HTTP 404 with error code ResourceNotFoundException and a populated message "Domain not found: <domainId>".

Requirement vs per-run values: the error code and the ABSENCE of a message member are the requirement; the specific 24-character dom- id in the check is a request value, not part of the contract. For the other arm (an id the endpoint once held), "Domain not found: " is fixed text and the domainId is taken from the request's URI label.

Model citations (smithy_query, model_path services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json):
  - operation DeleteDomain -> http {method DELETE, uri /domain/{domainId}, code 200, codeDefaulted}, idempotent true; input domainId -> DomainId @required @httpLabel @length(20,25) pattern ^(hai-|dom-)[a-z0-9]+$; output domainId/arn/status @required; errors [{shape ResourceNotFoundException, errorKind client, httpError 404}]. No AuthorizerConfigurationException shape exists in this model, so that code is recorded-behavior-only.

Code in this checkout: services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs lines 206-229 — the `let Some(domain) = store.held_domain_mut(&region, &input.domain_id) else { ... }` arm returns ResourceNotFoundException for EVERY unresolvable id, with no consultation of the allocation ledger; the module doc (lines 6-9) states this collapse explicitly. src/state.rs still carries allocated_domain_ids / has_allocated_domain, now unused by this path.

Bootstrap gate: get_bootstrap_status() reports pending = [] and lists connecthealth (service-com-amazonaws-connecthealth-connecthealth) as bootstrapped, so this fidelity finding is filed rather than deferred.

## Steps to Reproduce
Boot the emulator from this checkout and point a connecthealth (health-agent) client at its data listener.

1. Call DeleteDomain with a domainId that satisfies the modelled constraints but was never allocated by this emulator, e.g. domainId = "dom-" + "2"*20 (24 chars, matches ^(hai-|dom-)[a-z0-9]+$):
     DELETE /domain/dom-22222222222222222222
   Expect (recorded): a refusal whose error code is AuthorizerConfigurationException, with NO message member in the error detail.
   Observe: HTTP 404, x-amzn-errortype ResourceNotFoundException, body {"message": "Domain not found: dom-22222222222222222222"}.
2. Positive control (passes today, must keep passing): CreateDomain(name=<unique>) -> DeleteDomain(domainId=<new id>) answers 200 with domainId echoed and status in (DELETING, DELETED); a subsequent GetDomain(domainId=<new id>) answers ResourceNotFoundException with message "Domain not found: <domainId>".
3. Regression guard against the known oscillation: after fixing, replay BOTH behavioral check connecthealth.delete-domain-not-found AND wire fixture connecthealth-domain-lifecycle step task#5 (open issue 20261125T195918Z, which requires 404 ResourceNotFoundException there) in the same build and confirm both pass. A build where only one passes has re-flipped the arms rather than fixed them.
