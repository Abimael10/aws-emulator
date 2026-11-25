Issue-Source: trace-sweep-divergence
Issue-Trace: connecthealth-domain-lifecycle
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connecthealth-domain-lifecycle
Issue-Shape-Id: com.amazonaws.connecthealth#ConnectHealth
Issue-Key: connecthealth|DeleteDomain|4xx->4xx|$.message
Issue-Key: connecthealth|DeleteDomain|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: connecthealth|DeleteDomain|4xx->4xx|status
Issue-Commit: 24511e4ce5aef4d33e503a00bb0e57c4ce26e21d

# connecthealth DeleteDomain: an absent domainId is refused 403 AuthorizerConfigurationException with no message instead of the modelled 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `24511e4ce5aef4d33e503a00bb0e57c4ce26e21d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDomain

## Description
One net-new wire cause in the connecthealth domain lifecycle: the not-found arm of DeleteDomain is answered by an unmodelled authorization-layer refusal instead of the operation's one modelled error.

Cause 1 of 1: DeleteDomain addressed at a well-formed domainId this Region holds no domain for answers HTTP 403 with x-amzn-errortype AuthorizerConfigurationException and no error detail member, where the recorded service answers HTTP 404 ResourceNotFoundException with the message "Domain not found: <domainId>".

AWS: Smithy (services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json) models DeleteDomain as DELETE /domain/{domainId}, success 200 (codeDefaulted), @idempotent; input member domainId -> DomainId (@required, @httpLabel, @length 20..25, pattern ^(hai-|dom-)[a-z0-9]+$); output domainId/arn/status all @required; errors: exactly [ResourceNotFoundException (errorKind client, httpError 404)]. ResourceNotFoundException is structure { message: String }, documented "This error is thrown when the requested resource is not found." AuthorizerConfigurationException is declared on NO shape of this service's model and is not in its common error set (service_info lists authSchemes [sigv4], no common_errors), so it is an emulator-authored, unmodelled code. The recorded response for this step is HTTP 404 carrying x-amzn-errortype for connecthealth's not-found error and a body whose `message` reads "Domain not found: <domainId>": the FIXED text is "Domain not found: " and the only VARIABLE part is the domainId taken verbatim from the request's URI label. The recording's value dom-b2b2b2b2b2b2b2b2b2b2b is that run's request input filling the template, not part of the requirement.

Emulator: the same request is answered HTTP 403, header x-amzn-errortype: AuthorizerConfigurationException, and the body carries no `message` member at all (the replay reports $.message expected 'Domain not found: dom-b2b2b2b2b2b2b2b2b2b2b', got None). The source of the divergence is explicit in the handler: services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs splits the unresolvable-id space on an allocation ledger — when store.held_domain_mut(&region, &input.domain_id) resolves nothing and store.has_allocated_domain(&input.domain_id) is false, it returns crate::scaffolded::Scaffolded::unmodeled(AUTHORIZER_CONFIGURATION, AUTHORIZER_CONFIGURATION_STATUS, None), i.e. the hard-coded "AuthorizerConfigurationException" / 403 / no detail; only an id present in state.rs's allocated_domain_ids ledger reaches the modelled ResourceNotFoundException built from crate::service::error_detail::domain_not_found_message. The id this fixture step addresses is never created during the fixture, so it has no ledger entry and takes the 403 arm.

Why: a client cannot distinguish "this domain is already gone" from "my credentials or the service's authorizer are broken". Delete-if-exists and teardown code keys on ResourceNotFoundException to treat the delete as already done; against this emulator it sees a 403, which SDK retry/credential-refresh logic and operator runbooks treat as an auth fault, so idempotent teardown aborts and CloudFormation-style delete paths fail hard. The modelled 404 branch cannot be exercised at all, and because 403 is also what a genuinely unauthorized call looks like, real authorization defects become indistinguishable from an ordinary missing resource. A client reading the error detail on this path also gets no message where AWS sends a sentence.

Fix: the id-resolution path of the DeleteDomain handler (services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs) and the allocation ledger in src/state.rs that it consults. When the Region holds no domain for the requested domainId, the handler must answer the modelled ResourceNotFoundException at HTTP 404 with message "Domain not found: " + the domainId from the URI label, as the shared constructor in src/service/error_detail.rs already builds it; the unmodelled 403 arm must not cover this request class.

CONFLICTING PRIOR FILINGS — reconcile, do not simply flip. This exact step has oscillated: FIXED wire issues 20261029T202808Z-connecthealth-response-bodies-diverge-from-the-recorded-wire.md (Cause 2) and 20261123T070631Z-connecthealth-deletedomain-on-an-absent-domain-answers-403.md both required the 404 here, while FIXED behavioral issues 20261115T073931Z-connecthealth-deletedomain-answers-a-never-allocated-domainid-the.md and 20261125T034254Z-connecthealth-deletedomain-regressed-a-never-allocated-domainid-is.md required the 403 AuthorizerConfigurationException (with a null detail member) for check connecthealth.delete-domain-not-found, and the 403 arm now in the handler is what those two installed. Neither of those is open, so neither covers this failure. The modelled contract and this recording both demand 404 for the request class this fixture exercises, so the fix must make this fixture's id reach the modelled 404 while keeping the behavioral check green in the SAME build — narrow the unmodelled arm to the condition that check actually establishes (or seed/derive the ledger so this fixture's id is on the allocated side) rather than widening it to every id the store does not currently hold. A build where only one of the two corpora passes is the oscillation, not a fix.

Traces: connecthealth-domain-lifecycle#task#5 (DeleteDomain of an absent domain; every earlier step of the same fixture passes).

Not covered elsewhere: open issue 20261114T103624Z-connecthealth-3-net-new-causes-in-startpatientinsightsjob-clienttoken.md covers StartPatientInsightsJob only, and 20261109T181259Z-cloudformation-connecthealth-aws-healthagent-domain-is-not-a.md covers the CloudFormation resource type; neither carries this fixture step's replay key.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: trace_set_id simparity-wire-aws-v1, emulator_up=true, bootstrap_signal=false, 7 fixtures replayed, 62 steps total, 60 passed, 2 mismatched, 0 errored, 0 blocked, score 96.8%, divergence_count 2. The other cluster (s3 GetObject, fixture connecthealth-patient-insights-job-completes-with-summary#task#13) is OWNED by open issue 20261120T164159Z-s3-getobject-serves-a-placeholder-summary-object-and.md and excluded. The cluster below is net-new.

Fixture connecthealth-domain-lifecycle, step task#5, service connecthealth, operation DeleteDomain, verdict mismatch, affected_count 1, stub_path empty, owned_by empty:
  status: expected 404, got 403
  $.message: type_mismatch (expected 'Domain not found: dom-b2b2b2b2b2b2b2b2b2b2b', got None)
  $headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'AuthorizerConfigurationException')
  divergence_key: service=connecthealth|DeleteDomain|status=404->403|keys=$.message,$headers.x-amzn-errortype,status

Requirement vs per-run values: the status (404), the error code (connecthealth's modelled ResourceNotFoundException) and the message template are requirements. In "Domain not found: dom-b2b2b2b2b2b2b2b2b2b2b", "Domain not found: " is fixed text and dom-b2b2b2b2b2b2b2b2b2b2b is the domainId value from the request's URI label — a request value, not part of the contract.

Model citations (smithy_query against services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json):
  - operation DeleteDomain -> http {method DELETE, uri /domain/{domainId}, code 200, codeDefaulted}, idempotent true; input domainId -> DomainId @required @httpLabel @length(20,25) pattern ^(hai-|dom-)[a-z0-9]+$; output domainId/arn/status @required; errors [{shape ResourceNotFoundException, errorKind client, httpError 404}].
  - shape ResourceNotFoundException -> structure, errorKind client, httpError 404, members {message: String}, doc "This error is thrown when the requested resource is not found."
  - service_info -> ConnectHealth 2025-01-29, restJson1, sigv4Name health-agent, arnNamespace health-agent. AuthorizerConfigurationException resolves to no shape in this model.

Code in this checkout: services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs — const AUTHORIZER_CONFIGURATION = "AuthorizerConfigurationException" (line 163) and AUTHORIZER_CONFIGURATION_STATUS = 403 (line 168); handle() lines 225-260 take the unmodelled 403 arm via Scaffolded::unmodeled(AUTHORIZER_CONFIGURATION, AUTHORIZER_CONFIGURATION_STATUS, None) when !store.has_allocated_domain(&input.domain_id), and only otherwise returns ResourceNotFoundException { message: Some(domain_not_found_message(&input.domain_id)) }. The ledger it consults is state.rs allocated_domain_ids / allocation_order / has_allocated_domain (lines ~971-1103), populated only by record_domain_allocation on a mint — so an id never minted by this emulator run can never reach the modelled 404.

Bootstrap gate: get_bootstrap_status() reports pending = [] and lists service-com-amazonaws-connecthealth-connecthealth as bootstrapped, so this fidelity finding is filed rather than deferred.

## Steps to Reproduce
Boot the emulator from this checkout and point a connecthealth (health-agent) client at its data listener.

1. Replay wire fixture connecthealth-domain-lifecycle. Steps through task#4 pass (the domain is created and read back).
2. At step task#5 the fixture issues DeleteDomain for a domainId that satisfies the modelled constraints (pattern ^(hai-|dom-)[a-z0-9]+$, length 20..25 — the recording uses the 25-character dom-b2b2b2b2b2b2b2b2b2b2b) but that this emulator never created:
     DELETE /domain/dom-b2b2b2b2b2b2b2b2b2b2b
   Expect: HTTP 404, x-amzn-errortype naming connecthealth's ResourceNotFoundException, body {"message": "Domain not found: dom-b2b2b2b2b2b2b2b2b2b2b"} — fixed text "Domain not found: " plus the requested domainId.
   Observe: HTTP 403, x-amzn-errortype: AuthorizerConfigurationException, no `message` member in the body.
3. Control (must keep passing): DeleteDomain for a domain that DOES exist still answers 200 with domainId/arn/status, so the fault is confined to the not-found path.
4. Regression guard against the oscillation: after fixing, replay BOTH the wire fixture connecthealth-domain-lifecycle (step task#5) and the behavioral check connecthealth.delete-domain-not-found in the same build and confirm both pass. A build where only one passes has re-flipped the arms rather than fixed them.
