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
Issue-Commit: 922651469fb4c41ba18c92667367f3b4105911ff

# connecthealth: DeleteDomain on an absent domain answers 403 AuthorizerConfigurationException instead of 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `922651469fb4c41ba18c92667367f3b4105911ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDomain

## Description
One net-new wire cause in the connecthealth domain lifecycle: the not-found path of DeleteDomain is answered by an authorization-layer failure rather than the modelled resource-not-found error.

Cause 1 of 1: DeleteDomain for a domainId that does not exist returns HTTP 403 with x-amzn-errortype AuthorizerConfigurationException and no modelled error body, instead of HTTP 404 ResourceNotFoundException.

AWS: Smithy (services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json) models DeleteDomain as DELETE /domain/{domainId} (success 200, @idempotent) with input member domainId -> DomainId (@required, @httpLabel, @length 20..25, pattern ^(hai-|dom-)[a-z0-9]+$) and exactly ONE modelled error: ResourceNotFoundException, errorKind client, httpError 404, structure member `message` -> String, documented "This error is thrown when the requested resource is not found." AuthorizerConfigurationException is not an error of this operation and not in the service's error set at all. The recorded response for a well-formed but non-existent domainId is HTTP 404 with x-amzn-errortype naming the connecthealth not-found error and a body whose `message` reads "Domain not found: <domainId>". In that message the fixed text is "Domain not found: " and the only variable part is the domainId echoed verbatim from the request's URI label (the recording's value dom-b2b2b2b2b2b2b2b2b2b2b is that run's request input, not part of the requirement).

Emulator: the same request is answered HTTP 403. The response carries header x-amzn-errortype: AuthorizerConfigurationException and no `message` member at all ($.message is absent where the 404 body carries the sentence above). The status class is wrong (403 vs 404), the error code is wrong (AuthorizerConfigurationException vs ResourceNotFoundException), and the error message is missing entirely.

Why: a client cannot distinguish "this domain is gone" from "my credentials or the service's authorizer are broken". Delete-if-exists and teardown/cleanup code keys on ResourceNotFoundException to treat the delete as already done and proceed; against the emulator it instead sees a 403, which SDK retry/credential-refresh logic and operator runbooks treat as an auth fault — so idempotent teardown aborts, CloudFormation-style delete paths fail hard, and no test can exercise the modelled 404 branch. Because 403 is also what a genuinely unauthorized call looks like, real authorization defects become indistinguishable from ordinary missing resources.

Fix: the DeleteDomain handler/routing path in the connecthealth crate. The domain lookup must run and, when the state holds no domain for the requested domainId, return the modelled 404 ResourceNotFoundException with message "Domain not found: <domainId from the request label>" instead of falling through to the generic 403 authorizer rejection. Hypothesis: the DELETE /domain/{domainId} route never reaches the operation handler (unmatched route or an auth/authorizer pre-filter on this method/URI), so the request is refused before any state lookup — that is why the response is an authorizer error with no connecthealth error body rather than a connecthealth error with the wrong status.

Traces: connecthealth-domain-lifecycle#task#5 (DeleteDomain of an absent domain; the preceding lifecycle steps in the same fixture pass).

Not covered elsewhere: open issue 20261114T103624Z-connecthealth-3-net-new-causes-in-startpatientinsightsjob-clienttoken.md covers StartPatientInsightsJob behavior and 20261109T181259Z-cloudformation-connecthealth-aws-healthagent-domain-is-not-a.md covers the CloudFormation resource type; neither carries this fixture step's replay key.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: 62 steps, 60 passed, 2 mismatched, 0 errored, 0 blocked, score 96.8%, bootstrap_signal=false, emulator_up=true. One divergence cluster is OWNED by an existing s3 issue and excluded; the cluster below is net-new.

Fixture connecthealth-domain-lifecycle, step task#5, operation DeleteDomain, verdict mismatch, affected_count 1:
  status: expected 404, got 403
  $.message: type_mismatch (expected 'Domain not found: dom-b2b2b2b2b2b2b2b2b2b2b', got None)
  $headers.x-amzn-errortype: shape_mismatch (expected the connecthealth not-found error type, got 'AuthorizerConfigurationException')

The domainId in the expected message is the request's own URI label, i.e. a per-run request value filling the fixed template 'Domain not found: <domainId>'.

Model citations used (services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json): DeleteDomain DELETE /domain/{domainId}, code 200 (defaulted), @idempotent; input domainId -> DomainId @required @httpLabel @length(20,25) pattern ^(hai-|dom-)[a-z0-9]+$; output domainId/arn/status all @required; errors: [ResourceNotFoundException (client, httpError 404)]. ResourceNotFoundException = structure { message: String }, doc 'This error is thrown when the requested resource is not found.'

## Steps to Reproduce
Boot the emulator from this checkout and point a connecthealth client at its data listener.

1. Replay fixture connecthealth-domain-lifecycle; steps up to task#4 pass (domain created and read back).
2. At step task#5, issue DeleteDomain with a domainId that is well-formed for the modelled constraints (matches ^(hai-|dom-)[a-z0-9]+$, length 20..25, e.g. the 25-char 'dom-b2b2b2b2b2b2b2b2b2b2b' used in the recording) but was never created in this emulator's state:
     DELETE /domain/dom-b2b2b2b2b2b2b2b2b2b2b
   Expect: HTTP 404, x-amzn-errortype naming connecthealth's ResourceNotFoundException, body {"message": "Domain not found: dom-b2b2b2b2b2b2b2b2b2b2b"} — fixed text 'Domain not found: ' plus the requested domainId.
   Observe: HTTP 403, x-amzn-errortype: AuthorizerConfigurationException, no `message` member in the body.
3. Control: DeleteDomain for a domain that DOES exist in the emulator still answers 200 with domainId/arn/status, so the fault is confined to the not-found path.
