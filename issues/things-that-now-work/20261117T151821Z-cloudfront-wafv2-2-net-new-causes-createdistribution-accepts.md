Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__wafv2.create-distribution-rejects-nonexistent-webacl-id cloudfront__wafv2.delete-webacl-blocked-while-cloudfront-distribution-names-it
Issue-Key: cloudfront__wafv2.create-distribution-rejects-nonexistent-webacl-id|_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: cloudfront__wafv2.delete-webacl-blocked-while-cloudfront-distribution-names-it|_error#15|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudfront__wafv2: 2 net-new causes — CreateDistribution accepts a WebACLId that resolves to nothing, and DeleteWebACL succeeds while a distribution still names the web ACL

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Two net-new root causes on the CloudFront/WAFv2 pairing; both must be closed before submitting. Together they leave the WAF association link unenforced in both directions.

Cause 1 of 2: CreateDistribution accepts a WebACLId that does not resolve to an existing web ACL.
AWS: Smithy: CreateDistribution binds InvalidWebACLId (httpError 400). Recorded refusal for a WebACLId naming a nonexistent CLOUDFRONT-scope web ACL: 400, Code `InvalidWebACLId`, Message "Web ACL is not accessible by the requester." — entirely fixed text — and nothing is created; the same call with a real web ACL ARN succeeds and GetDistribution echoes that ARN back.
Emulator: answers 201 and stores the bogus WebACLId.
Why: the distribution reports WAF protection that does not exist, so a client auditing its security posture by reading WebACLId back is told it is protected when no ACL is attached.
Fix: CreateDistribution/UpdateDistribution validation — resolve WebACLId (scope CLOUDFRONT) before admitting the config.
Traces: cloudfront__wafv2.create-distribution-rejects-nonexistent-webacl-id.

Cause 2 of 2: wafv2 DeleteWebACL succeeds while a deployed CloudFront distribution still names the web ACL.
AWS: Smithy: wafv2 DeleteWebACL binds WAFAssociatedItemException (httpError 400). Recorded refusal while a fully Deployed distribution's WebACLId names the ACL: Code `WAFAssociatedItemException`, Message "AWS WAF couldn't perform the operation because your resource is being used by another resource or it's associated with another resource." — fixed text (AWS writes the apostrophe as U+2019); after the distribution's WebACLId is cleared the delete succeeds, and a second DeleteWebACL then answers WAFNonexistentItemException.
Emulator: answers HTTP 200 with an empty JSON body and deletes the ACL out from under the distribution.
Why: the association guard that AWS uses to stop an operator deleting a live web ACL is absent, so a distribution is silently left pointing at a deleted ACL — exactly the state AWS refuses to create.
Fix: wafv2 DeleteWebACL's pre-delete association check — refuse while any CloudFront distribution's WebACLId references the ACL.
Traces: cloudfront__wafv2.delete-webacl-blocked-while-cloudfront-distribution-names-it.

## Evidence
Behavioral suite simparity-behavioral-aws-v1; both checks are listed under `### cloudfront__wafv2`, neither ALREADY FILED nor OWNED.

Cause 1 — create-distribution-rejects-nonexistent-webacl-id: expected ('InvalidWebACLId', 'Web ACL is not accessible by the requester.'); got HTTP 201 with Location /2020-05-31/distribution/<id> and the bogus WebACLId stored.
Cause 2 — delete-webacl-blocked-while-cloudfront-distribution-names-it: expected ('WAFAssociatedItemException', "AWS WAF couldn't perform the operation because your resource is being used by another resource or it's associated with another resource."); got HTTP 200 with body '{}' (the delete was performed).

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator.
2. Replay the simparity-behavioral-aws-v1 suite.
3. Re-run the two checks in trace_ids.
4. Cause 1: CreateDistribution with WebACLId naming a nonexistent CLOUDFRONT-scope web ACL — expect 400 InvalidWebACLId and no distribution; repeat with a real ACL ARN and expect 201 with that ARN echoed.
5. Cause 2: create a CLOUDFRONT-scope web ACL, attach it to a distribution, wait for Deployed, call wafv2 DeleteWebACL — expect WAFAssociatedItemException; clear WebACLId, delete again (expect success), delete once more (expect WAFNonexistentItemException).
