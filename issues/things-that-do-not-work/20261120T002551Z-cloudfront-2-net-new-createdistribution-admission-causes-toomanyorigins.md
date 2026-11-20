Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.distribution-origin-count-quota-boundary-rejected cloudfront.distribution-trusted-key-group-validation-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.distribution-origin-count-quota-boundary-rejected|expect_error#3|AssertionError|assert message == expected_message, (
Issue-Key: cloudfront.distribution-trusted-key-group-validation-rejected|_error#20|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 7a863d1ad62c4708957aba7eef669d745200ca52

# cloudfront: 2 net-new CreateDistribution admission causes — TooManyOrigins refusal carries the Smithy doc sentence instead of AWS's quota message, and a TrustedKeyGroups block naming a nonexistent key group is admitted

> _Note: this issue was identified against the code base at commit `7a863d1ad62c4708957aba7eef669d745200ca52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Two net-new root causes in the CloudFront control plane's CreateDistribution admission layer. They are independent; both must be closed before submitting. Related open issues that do NOT cover them: 20261117T151821Z-cloudfront-10-net-new-over-permissive-admission-causes.md (its Cause 2 covers the per-distribution Origins quota being *unenforced* — the emulator now does refuse, so what remains is the wrong message, Cause 1 below; it carries no trusted-key-group cause at all) and 20261117T151821Z-cloudfront-6-net-new-refusal-wording-causes-emulator.md (same wording family, but its causes are MinTTL, TooManyKeyGroupsAssociatedToDistribution, TooManyCacheBehaviors, SamplingRate, realtime-log identifiers and list Markers — not TooManyOrigins).

Cause 1 of 2: the TooManyOrigins refusal message is the error shape's modeled documentation sentence, not AWS's service message.
AWS: Smithy: CreateDistribution binds TooManyOrigins (errorKind client, httpError 400); the shape com.amazonaws.cloudfront#TooManyOrigins documents itself as "You cannot create more origins for the distribution." The *wire* message AWS returns for a config carrying one more origin than the per-distribution Origins limit is HTTP 400, Code `TooManyOrigins`, Message "Your request contains more Origins than are allowed per distribution." — entirely fixed text, with no counts, limits or origin ids interpolated into it. A config exactly at the limit is created and reports every submitted origin back from GetDistributionConfig.
Emulator: refuses with the right status and code but with Message "You cannot create more origins for the distribution." — i.e. it emits the Smithy doc string for the error shape as the error message. Hypothesis: the error's message is being defaulted from the model's documentation trait rather than from AWS's recorded per-operation text.
Why: an HTTP status, an error code and the error message are requirements. A client, SDK retry/triage rule or test that matches AWS's quota wording does not recognise this refusal, and anyone reading the message is told a generic "cannot create more" rather than that the *request* exceeded the per-distribution Origins quota.
Fix: the message emitted with TooManyOrigins in CreateDistribution's Origins-quota validation (and the same defaulting path if other error shapes take their message from the model documentation).
Traces: cloudfront.distribution-origin-count-quota-boundary-rejected.

Cause 2 of 2: a DefaultCacheBehavior.TrustedKeyGroups block naming a key-group id that does not exist is admitted.
AWS: Smithy: CreateDistribution binds TrustedKeyGroupDoesNotExist (errorKind client, httpError 400, documented "The specified key group does not exist."), alongside InconsistentQuantities (400) and InvalidArgument (400); TrustedKeyGroups has required Enabled and Quantity plus an Items list of key-group ids. Recorded refusal for a well-formed but unresolvable key-group id: HTTP 400, Code `TrustedKeyGroupDoesNotExist`, Message "One or more of your trusted keygroups do not exist." — entirely fixed text (it names no id, and stays plural whatever the Items length). Nothing is created: the rejected config does not appear in ListDistributions, and the real key group used by the check is left unchanged.
Emulator: answers HTTP 201 with a Location header and a full Distribution body for a TrustedKeyGroups block {Enabled: true, Quantity: 1, Items: [<id of no key group>]}, persisting the distribution with ActiveTrustedKeyGroups {Enabled: true, Quantity: 0} — an enabled trusted-key-group set with nothing in it, a state AWS cannot produce.
Why: a client that mistypes, or races the deletion of, a key group is told its signed-URL/signed-cookie trust was configured. The distribution then reports trust enabled while no key can ever verify a signature, so viewer requests are authorised (or refused) on a basis the client never asked for, and the client's own read-back of ActiveTrustedKeyGroups confirms a protection that does not exist.
Fix: CreateDistribution's (and UpdateDistribution's) cache-behavior validation — resolve every TrustedKeyGroups.Items id before admitting the config and raise TrustedKeyGroupDoesNotExist. Note that AWS validates the block's *shape* before resolving the ids inside it (Quantity vs Items length → InconsistentQuantities; Enabled with no Items and Items while disabled → InvalidArgument), and this check asserts that ordering after the id-resolution leg; those later legs were never reached here, so they are unverified behind this cause rather than known-good.
Traces: cloudfront.distribution-trusted-key-group-validation-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 560/702 passed. Both checks are listed under `### cloudfront` and are neither ALREADY FILED nor OWNED by an open issue.

Cause 1 — cloudfront.distribution-origin-count-quota-boundary-rejected (mismatch):
  AssertionError: expected message 'Your request contains more Origins than are allowed per distribution.', got 'You cannot create more origins for the distribution.'
  The error code assertion passed first, so the emulator answers Code `TooManyOrigins` with the wrong Message. The message it returns is byte-for-byte the documentation sentence of com.amazonaws.cloudfront#TooManyOrigins in the service's Smithy model.

Cause 2 — cloudfront.distribution-trusted-key-group-validation-rejected (mismatch):
  AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'Location': '/2020-05-31/distribution/<id>', 'ETag': '<etag>', 'Distribution': {'Id': '<id>', 'Status': 'InProgress', 'ActiveTrustedKeyGroups': {'Enabled': True, 'Quantity': 0}, 'DistributionConfig': {... 'DefaultCacheBehavior': {... 'TrustedKeyGroups': {'Enabled': True, 'Quantity': 1, 'Items': ['<key-group id that does not exist>']}}}}}
  Expected: HTTP 400, Code `TrustedKeyGroupDoesNotExist`, Message "One or more of your trusted keygroups do not exist.", and no distribution created. (Host, port, generated distribution id/ETag, request id and timestamp in the replayed response are per-run values and are not part of the requirement.)

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator.
2. Replay the simparity-behavioral-aws-v1 suite.
3. Re-run exactly the checks in trace_ids.
4. Cause 1: call CreateDistribution with a DistributionConfig whose Origins.Quantity is one more than the per-distribution Origins limit (100 custom origins + 1), each origin a distinct CustomOriginConfig, DefaultCacheBehavior targeting the first origin with the managed CachingOptimized cache policy. Assert HTTP 400 / Code `TooManyOrigins` / Message exactly 'Your request contains more Origins than are allowed per distribution.'; then confirm a config at exactly the limit is accepted and GetDistributionConfig returns Origins.Quantity = limit with every submitted origin id.
5. Cause 2: CreatePublicKey + CreateKeyGroup to have one real key group, then call CreateDistribution with DefaultCacheBehavior.TrustedKeyGroups = {Enabled: true, Quantity: 1, Items: ['8f4d1c2b-6a3e-4f5d-9b8c-1e2f3a4b5c6d']} (a well-formed id naming no key group). Assert HTTP 400 / Code `TrustedKeyGroupDoesNotExist` / Message exactly 'One or more of your trusted keygroups do not exist.', that ListDistributions shows no distribution with the rejected config's Comment, and that GetKeyGroup on the real key group is unchanged. The same check then asserts the shape-before-ids ordering (Quantity 2 with 1 item → InconsistentQuantities 'The specified quantity of TrustedKeyGroups (2) does not match the actual quantity supplied (1).'; Enabled true with no Items → InvalidArgument 'The parameter TrustedKeyGroups must contain items if enabled.'; Enabled false with Items → InvalidArgument 'The parameter TrustedKeyGroups must not contain items if disabled.') and that a consistent block on the real key group is accepted with ViewerProtocolPolicy 'allow-all' and resolves into ActiveTrustedKeyGroups.
