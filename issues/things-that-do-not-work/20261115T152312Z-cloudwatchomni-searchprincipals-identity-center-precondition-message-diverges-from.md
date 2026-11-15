Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatchomni-deep-link-and-identity-center-refusals
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatchomni-deep-link-and-identity-center-refusals
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni|SearchPrincipals|4xx->4xx|$.message
Issue-Commit: 4bf99a6da981b1c03b4c2a4da03064374e7befb5

# cloudwatchomni SearchPrincipals Identity Center precondition message diverges from AWS wording

> _Note: this issue was identified against the code base at commit `4bf99a6da981b1c03b4c2a4da03064374e7befb5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SearchPrincipals

## Description
SearchPrincipals correctly refuses a domain that is not provisioned with Identity Center with HTTP 400 / ValidationException, but the message body is the emulator's own prose instead of the text AWS emits for this precondition. The error message is part of the response contract, so the wording is a defect even though the status and error type are right. This bundle has ONE cause.

Cause 1 of 1: SearchPrincipals Identity-Center precondition message re-worded (fixture cloudwatchomni-deep-link-and-identity-center-refusals, step task#5).

AWS: Smithy SearchPrincipals (service com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend) takes required input members domainId (DomainId, pattern ^d-[0-9a-z]{1,25}$) and searchQuery (@length 1..128), and its modeled errors include ValidationException (httpError 400). The operation documentation states the precondition explicitly: "Searches Identity Center for users and groups in a domain. The domain must be configured with Identity Center." The model does not carry the message text, so for the wording this is recorded behavior only: when the target domain has no Identity Center (IdC) application ARN provisioned, AWS answers HTTP 400 with __type ValidationException and the message "Domain does not have an IdC application ARN configured. The domain must be provisioned with Identity Center before this operation can proceed." That message is entirely FIXED TEXT - it has no variable parts: it names neither the domain id from the request, nor the operation, nor the domain's identityProviders list. The requirement is therefore the literal two-sentence string above, emitted for every domain in this state.

Emulator: returns the right status (400) and error type, but with the message "Domain 'd-<domainId>' is not configured with Identity Center. SearchPrincipals requires a domain whose identityProviders include IDC." - different fixed text in both sentences, and it interpolates two values AWS's message does not contain at all: the requested domainId (a request value) and the operation name. Because the emulator's variable parts are drawn from the request, the text it returns differs per call, where AWS's is constant.

Why: a client relying on AWS's behavior cannot recognise the refusal. AWS's message is a constant, so SDK wrappers, Identity-Center onboarding flows and console-style callers routinely match it verbatim (or surface it to a user) to distinguish "this domain still needs IdC provisioning" from any other ValidationException on the same operation - both arrive as 400 ValidationException, so the message is the only discriminator. Against the emulator that match never fires, so the caller either reports a generic validation failure or retries a request that can never succeed until the domain is provisioned; and callers that log or display the text leak the domain id in an error where AWS deliberately emits no identifiers.

Fix: the SearchPrincipals handler's Identity-Center precondition check (the branch that rejects a domain whose identityProviders do not include IDC) - emit the fixed AWS message and stop interpolating the domain id and the operation name. If other operations share this precondition they should share one constant formatter rather than composing the message per operation.

Traces: cloudwatchomni-deep-link-and-identity-center-refusals#task#5.

Related but NOT covering this step: 20261111T063558Z-cloudwatchomni-error-messages-diverge-from-aws-wording-for.md bundles other message-wording causes on the SAME fixture (its task#3, CreateOneTimeDeepLinkCode) plus CreateView/DeleteView, and 20261111T022337Z-cloudwatchomni-conflict-and-validation-error-messages-diverge-from.md covers CreateAccessProfile/CreateView/DeleteDomain/UpdateDomain. Neither carries the SearchPrincipals replay key below, so neither would be re-replayed against this step.

## Evidence
Replay of simparity-wire-aws-v1, fixture cloudwatchomni-deep-link-and-identity-center-refusals, step task#5, operation SearchPrincipals: expected status 400, actual status 400 (status and error type agree). Diverging key $.message - value_mismatch:
  expected: 'Domain does not have an IdC application ARN configured. The domain must be provisioned with Identity Center before this operation can proceed.'
  actual:   "Domain 'd-ez1ctp3o1szky5t2sfwdtifq1' is not configured with Identity Center. SearchPrincipals requires a domain whose identityProviders include IDC."
The expected string is wholly fixed text (no request- or state-derived parts). In the actual string, "d-ez1ctp3o1szky5t2sfwdtifq1" is the domainId taken from the request (a per-run value, not a requirement) and "SearchPrincipals" is the operation name; everything else is emulator-authored fixed text that AWS does not use.
Model citation: Smithy SearchPrincipals - input domainId (required, pattern ^d-[0-9a-z]{1,25}$), searchQuery (required, @length 1..128); errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400); operation doc: "The domain must be configured with Identity Center."

## Steps to Reproduce
1. Replay the simparity-wire-aws-v1 wire corpus against the emulator.
2. Open fixture cloudwatchomni-deep-link-and-identity-center-refusals and run to step task#5: SearchPrincipals is called with the domainId of a domain whose identityProviders do not include IDC (no Identity Center application ARN provisioned) and a valid searchQuery.
3. Observe the response: HTTP 400 ValidationException, as expected, but $.message is "Domain '<domainId>' is not configured with Identity Center. SearchPrincipals requires a domain whose identityProviders include IDC." instead of the recorded AWS message 'Domain does not have an IdC application ARN configured. The domain must be provisioned with Identity Center before this operation can proceed.'
4. The divergence reproduces for any domain in that state; the emulator's text varies with the domainId supplied, while AWS's is constant.
