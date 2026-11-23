Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.list-distributions-by-owned-resource-filters-correctly cloudfront.list-distributions-by-owned-resource-not-found-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.list-distributions-by-owned-resource-filters-correctly|run#9|AssertionError|assert message == "Resource has invalid syntax: ARNs must start with 'arn:': not-an-arn", (
Issue-Key: cloudfront.list-distributions-by-owned-resource-not-found-rejected|run#10|AssertionError|assert message == "Invalid parameter received. Resource Type", f"unexpected message {message!r}"
Issue-Commit: 616e510b6c9fc5a742ae081415d2f4cf49efcb62

# ListDistributionsByOwnedResource refuses a malformed ARN and an unsupported resource type with the resource-policy surface's sentences instead of its own (2 net-new causes)

> _Note: this issue was identified against the code base at commit `616e510b6c9fc5a742ae081415d2f4cf49efcb62`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDistributionsByOwnedResource

## Description
`ListDistributionsByOwnedResource` reads its required `ResourceArn` through the resource-policy surface's shared ARN judgement (`crate::state::resource_policy_arn_refusal`) and then answers with THAT surface's sentences. The error CODE and HTTP status are right in both refusals below (`InvalidArgument`, HTTP 400, per the model), but the MESSAGE is another operation's, so a client that matches on this operation's wording never sees it. Two net-new causes, both in this one operation's refusal path.

Note: the operation itself was implemented by the already-resolved issue 20261112T003830Z-cloudfront-listdistributionsbyownedresource-stub.md; that fix left the operation serving (its listing check passes and both refusals below are correctly classified as `InvalidArgument`), so this is not a reopen of a still-refused call — only the two refusal sentences diverge.

Cause 1 of 2: a `ResourceArn` that is no ARN at all is refused with the bare form sentence, not this operation's syntax sentence.
  AWS:      Smithy: `ListDistributionsByOwnedResource` input member `ResourceArn` (string, `@httpLabel`, required, no `@pattern`); modelled errors `AccessDenied` 403, `EntityNotFound` 404, `InvalidArgument` 400, `UnsupportedOperation` 400. The model says nothing about the message, so: recorded behavior only — a value that cannot be read as an ARN is refused `InvalidArgument` (HTTP 400) with the message `Resource has invalid syntax: ARNs must start with 'arn:': <ResourceArn>`. Its FIXED text is `Resource has invalid syntax: ARNs must start with 'arn:': `; the VARIABLE part is the value the request carried in `ResourceArn`, echoed verbatim after the final colon-space (in the recorded run that value was the submitted string, e.g. a bare non-ARN token — the token itself is request data, not part of the requirement).
  Emulator: answers `InvalidArgument` (400) with the message `ARNs must start with 'arn:'` — the shared `MALFORMED_ARN_MESSAGE`, with neither the `Resource has invalid syntax: ` prefix nor the echoed `ResourceArn` value.
  Why:      a client (or a test/operator tool) that classifies malformed input by this operation's message, or that recovers the offending ARN from the message to report which of several ARNs it submitted was bad, gets neither: the message is shorter than AWS's and drops the value entirely, so error-message assertions and log-scraping based on real AWS output fail against the emulator.
  Fix:      the refusal wording in this operation's handler/validation path — the `ResourceArnRefusal::Malformed` arm of `ListDistributionsByOwnedResource`'s ARN reading must build this operation's own `InvalidArgument` sentence (fixed prefix plus the request's `ResourceArn`) rather than reusing the service-wide malformed-ARN constructor.
  Traces:   cloudfront.list-distributions-by-owned-resource-filters-correctly (assertion: `unexpected message "ARNs must start with 'arn:'"`). Source: services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/list_distributions_by_owned_resource.rs:221-226 selecting `crate::state::malformed_arn()`, defined at services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/state/identity.rs:322-333.

Cause 2 of 2: a well-formed ARN naming a resource type this operation does not support is refused with the resource-policy READ sentence instead of the resource-type sentence.
  AWS:      Smithy: same operation and member as above; `InvalidArgument` 400 (`An argument is invalid.`). Recorded behavior only — a syntactically valid `arn:aws:cloudfront::<account>:<type>/<id>` whose `<type>` is not a type this operation supports (recorded for `distribution-tenant`, and the same for `connection-group`) is refused `InvalidArgument` (HTTP 400) with the message `Invalid parameter received. Resource Type`, which is fixed text in full — it carries nothing from the request or from state. The type is judged BEFORE existence: the ARN of a real, existing tenant and the same ARN with a fresh random id are refused identically, so this path never becomes `EntityNotFound` (that code is reserved for an ARN of a supported type naming nothing, message `The specified resource doesn't exist. Update your request and try again.`, which the emulator already gets right).
  Emulator: answers `InvalidArgument` (400) with the message `The ResourceArn parameter is required. Specify a valid ARN and try again.` — the resource-policy READ sentence. Besides being the wrong text, it tells the caller the parameter was MISSING when it was present and well-formed, naming the wrong remedy.
  Why:      a client distinguishing "you named a resource type this listing does not cover" from "you omitted ResourceArn" is misled into retrying with the parameter it already sent; message-matching error handling and user-facing surfacing of the real cause both break.
  Fix:      the refusal wording in this operation's handler/validation path — the `ResourceArnRefusal::WrongType` arm (and the equivalent fallback when the ARN's resource part resolves to no supported type) must emit this operation's fixed `Invalid parameter received. Resource Type` sentence instead of reusing the resource-policy read constructor.
  Traces:   cloudfront.list-distributions-by-owned-resource-not-found-rejected (assertion: `unexpected message 'The ResourceArn parameter is required. Specify a valid ARN and try again.'`); also asserted, after cause 1, by cloudfront.list-distributions-by-owned-resource-filters-correctly for a `connection-group` ARN. Source: services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/list_distributions_by_owned_resource.rs:227-231 and :246-252 selecting `crate::state::invalid_resource_policy_arn()`, defined at services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/state/identity.rs:302-311.

## Evidence
Cause 1 — malformed `ResourceArn` (a non-ARN string):
  Expected: code `InvalidArgument`, HTTP 400, message `Resource has invalid syntax: ARNs must start with 'arn:': <the submitted ResourceArn>` (fixed prefix + the request's value).
  Actual:   code `InvalidArgument`, HTTP 400, message `ARNs must start with 'arn:'`.
  Replay:   cloudfront.list-distributions-by-owned-resource-filters-correctly ->
            AssertionError: unexpected message "ARNs must start with 'arn:'"
            (assert message == "Resource has invalid syntax: ARNs must start with 'arn:': not-an-arn")

Cause 2 — well-formed CloudFront ARN of an unsupported resource type (a distribution-tenant ARN; likewise connection-group):
  Expected: code `InvalidArgument`, HTTP 400, message `Invalid parameter received. Resource Type` (fixed text in full), and the SAME answer for that ARN with a random, non-existent id — the type is checked before existence.
  Actual:   code `InvalidArgument`, HTTP 400, message `The ResourceArn parameter is required. Specify a valid ARN and try again.`
  Replay:   cloudfront.list-distributions-by-owned-resource-not-found-rejected ->
            AssertionError: unexpected message 'The ResourceArn parameter is required. Specify a valid ARN and try again.'
            (assert message == "Invalid parameter received. Resource Type")

Control that does match AWS (do not change it): a `vpcorigin` ARN naming nothing is refused `EntityNotFound` with `The specified resource doesn't exist. Update your request and try again.`, and the tenant read before/after the rejected lookups still resolves — the rejections disturb no state.

## Steps to Reproduce
1. Boot the cloudfront emulator.
2. Cause 1: call ListDistributionsByOwnedResource with ResourceArn set to a string that does not start with 'arn:' (e.g. a bare token). Observe HTTP 400 / InvalidArgument with message "ARNs must start with 'arn:'" instead of "Resource has invalid syntax: ARNs must start with 'arn:': <that value>".
3. Cause 2: create a multi-tenant (ConnectionMode=tenant-only) distribution and a distribution tenant under it, then call ListDistributionsByOwnedResource with ResourceArn = the tenant's Arn (and again with the same ARN with its id replaced by a random UUID, and with arn:aws:cloudfront::<account>:connection-group/<anything>). Observe HTTP 400 / InvalidArgument with message "The ResourceArn parameter is required. Specify a valid ARN and try again." instead of "Invalid parameter received. Resource Type".
4. Re-replay cloudfront.list-distributions-by-owned-resource-filters-correctly and cloudfront.list-distributions-by-owned-resource-not-found-rejected to confirm both causes are closed.
