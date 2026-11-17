Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: secretsmanager.batch-get-secret-value-max-results-above-twenty-and-fabricated-next-token-rejected secretsmanager.put-resource-policy-exceeds-max-length-rejected secretsmanager.list-secrets-primary-region-filter-matches-a-replica secretsmanager.stop-replication-to-replica-on-a-primary-secret-rejected
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secretsmanager.batch-get-secret-value-max-results-above-twenty-and-fabricated-next-token-rejected|run#11|AssertionError|assert code == "InvalidRequestException", f"MaxResults=21 must be refused, got {code!r}: {message!r}"
Issue-Key: secretsmanager.put-resource-policy-exceeds-max-length-rejected|run#16|AssertionError|assert code == "InvalidParameterException", (
Issue-Key: secretsmanager.list-secrets-primary-region-filter-matches-a-replica|run#24|AssertionError|assert by_away_primary == [], (
Issue-Key: secretsmanager.stop-replication-to-replica-on-a-primary-secret-rejected|run#13|AssertionError|assert message == expected, f"unexpected message for an unreplicated secret: {message!r}"
Issue-Commit: da5ca9a7bb6657991592368ee3d3c2d494ca2b65

# secretsmanager: 4 net-new causes — BatchGetSecretValue MaxResults and PutResourcePolicy length ceilings refuse as generic ValidationException, the ListSecrets primary-region filter matches a never-replicated secret, and StopReplicationToReplica cannot tell a standalone secret from a replicated primary

> _Note: this issue was identified against the code base at commit `da5ca9a7bb6657991592368ee3d3c2d494ca2b65`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetSecretValue

## Description
Four independent Secrets Manager defects found by the behavioral replay, all in the secretsmanager service. Cause count: 4.

=== Cause 1 of 4: BatchGetSecretValue's MaxResults ceiling is enforced as a generic Smithy constraint violation instead of the service's own refusal ===
AWS: Smithy (com.amazonaws.secretsmanager#BatchGetSecretValue): MaxResults targets MaxResultsBatchType, an integer with @range min 1 max 20; modeled errors are DecryptionFailure, InternalServiceError, InvalidNextTokenException, InvalidParameterException, InvalidRequestException and ResourceNotFoundException (all client errors httpError 400 except InternalServiceError). A request with MaxResults=21 is refused by the service with HTTP 400, Error.Code InvalidRequestException and the fixed message "Max results must not exceed 20." — fixed text with no variable part (recorded behavior for the sentence; the 400 and the error shapes are modeled). AWS does NOT answer the generic constraint-violation envelope for this input.
Emulator: answers HTTP 400 with Error.Code ValidationException and the generic constraint message "1 validation error detected: Value '21' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 20" — the wrong error class (ValidationException is not even a modeled BatchGetSecretValue error) and a message AWS never emits for this operation. The positive control at the boundary (MaxResults=20 with a name filter) passed.
Why: a client that catches InvalidRequestException to retry with a smaller page size never sees it — botocore raises an exception class (ValidationException) that is not in the operation's modeled error set, so typed handlers miss it entirely and only a string match on the message would work. The refusal also reads as a client-side/shape error rather than the service's documented page-size rule.
Why the same check's second half matters: the assertion fires before the fabricated-NextToken probe, so the InvalidNextTokenException "The pagination token is no longer valid." expectation in the same check was never exercised and must be re-checked with the fix.
Fix: the BatchGetSecretValue handler's input validation in the secretsmanager service crate — enforce the page-size ceiling in the operation handler and emit 400 / InvalidRequestException with AWS's fixed sentence, rather than letting the shared Smithy range-constraint validator answer first.
Traces: secretsmanager.batch-get-secret-value-max-results-above-twenty-and-fabricated-next-token-rejected

=== Cause 2 of 4: PutResourcePolicy's 20480-character ceiling is enforced as a generic Smithy constraint violation (which also echoes the caller's document) instead of the service's own refusal ===
AWS: Smithy (com.amazonaws.secretsmanager#PutResourcePolicy): ResourcePolicy targets NonEmptyResourcePolicyType with @length min 1 max 20480 and is required; modeled errors are InternalServiceError, InvalidParameterException, InvalidRequestException, MalformedPolicyDocumentException, PublicPolicyException and ResourceNotFoundException. A policy document one character over the maximum is refused with HTTP 400, Error.Code InvalidParameterException and the fixed message "ResourcePolicy exceeds size limit." — fixed text, no variable part, and the document itself is NOT quoted back (recorded behavior for the sentence; the 400 and the error shape are modeled). Secrets Manager's own documented limit for a resource policy is 20480 characters.
Emulator: answers HTTP 400 with Error.Code ValidationException and "1 validation error detected: Value '{\"Version\": \"2012-10-17\", \"Statement\": [{\"Sid\": \"XXXXXXXXXXXXXXX ...' at 'resourcePolicy' failed to satisfy constraint: Member must have length less than or equal to 20480" — the wrong error class (ValidationException is not a modeled PutResourcePolicy error), a message AWS never emits for this rule, and a response that echoes a prefix of the caller's submitted policy document back in the error text. The in-range positive controls before and after passed, and the refused oversized document correctly did not replace the attached policy.
Why: a client that branches on InvalidParameterException to shrink or split its policy sees an error class outside the operation's modeled set, so typed error handling fails; the echoed document also puts caller-supplied content into an error message where AWS puts none.
Fix: the PutResourcePolicy handler's input validation in the secretsmanager service crate — check the document length in the operation handler and refuse with 400 / InvalidParameterException and AWS's fixed sentence, without quoting the document.
Traces: secretsmanager.put-resource-policy-exceeds-max-length-rejected

=== Cause 3 of 4: the ListSecrets primary-region filter matches a secret that never took part in replication ===
AWS: Smithy (com.amazonaws.secretsmanager#ListSecrets): Filters targets FiltersListType (0..10) of Filter, whose Key is FilterNameStringType, an enum with values description, name, tag-key, tag-value, primary-region, owning-service, all. The primary-region filter matches on a secret's PrimaryRegion, which exists only for a secret that participates in replication: a replica (listed in the replica Region) and the primary of a replicated secret (listed in its own Region) carry it. A standalone, never-replicated secret has no PrimaryRegion at all, so it matches NO primary-region value — including the Region it happens to live in (recorded behavior; the model does not describe filter matching).
Emulator: in the second Region, ListSecrets(Filters=[{Key: name, Values: [<shared prefix>]}, {Key: primary-region, Values: [<that same Region>]}]) returned the standalone secret created in that Region, which has no replication at all. Expected an empty SecretList. The two preceding probes in the same check passed, so the replicated pair is filtered correctly: primary-region=<home Region> listed only the replica in the away Region and only the primary in the home Region. Hypothesis: the filter falls back to the secret's own Region when the secret carries no PrimaryRegion, instead of not matching.
Why: the filter over-matches, so a client using primary-region to enumerate exactly the secrets replicated from a given Region (a replication inventory, a cross-Region audit, a cleanup job) gets unrelated standalone secrets mixed in and can act on secrets that are not replicas. A fixed issue (20261023T182429Z-secretsmanager-describesecret-metadata-is-incomplete-replication-status-and-tags.md) covered DescribeSecret reporting PrimaryRegion on an unreplicated secret; the ListSecrets filter still behaves as if an unreplicated secret had one.
Fix: the ListSecrets filter evaluation in the secretsmanager service — the primary-region predicate must match only against a stored PrimaryRegion and never fall back to the secret's own Region, and secrets with no replication must carry no PrimaryRegion in the state it reads.
Traces: secretsmanager.list-secrets-primary-region-filter-matches-a-replica

=== Cause 4 of 4: StopReplicationToReplica emits the replicated-primary refusal message for a standalone secret, so the two states are indistinguishable ===
AWS: Smithy (com.amazonaws.secretsmanager#StopReplicationToReplica): input SecretId (SecretIdType, required, @length 1..2048); output ARN; modeled errors InvalidParameterException (400), InvalidRequestException (400), ResourceNotFoundException (400), InternalServiceError (500). The call is the replica-side promotion and AWS refuses it on the primary side with HTTP 400 / InvalidParameterException and two DISTINCT fixed messages: for a standalone, never-replicated secret (and for a secret just promoted to standalone) "Operation not permitted on a primary/standalone secret."; for a primary that currently HAS a live replica "Operation not permitted on a primary secret. Call must be made in replica secret's region." Both sentences are entirely fixed text — neither interpolates a secret name, ARN or Region (recorded behavior for the sentences; the 400 and the error shape are modeled).
Emulator: the status and the error code are right, but the message for the standalone/unreplicated case is the replicated-primary sentence "Operation not permitted on a primary secret. Call must be made in replica secret's region." The emulator therefore uses one message for both states and never emits AWS's "primary/standalone" sentence, so a caller cannot tell "this secret has no replica at all" from "you called in the wrong Region".
Why: the message is part of the contract, and the one the emulator emits mis-diagnoses the standalone case: it sends a caller to retry in a replica Region that does not exist, instead of telling it the secret is not a replica. Clients, tests and runbooks that branch on or log either sentence cannot distinguish the two refusals. The assertion is the check's first, so the rest of that check (replicate, refuse on the replicated primary, replica-side promotion, re-refusal on the promoted secret) was never exercised.
Fix: the StopReplicationToReplica handler's refusal path in the secretsmanager service crate — select the message from the secret's replication state (no replicas / standalone vs. primary with live replicas) and emit each of AWS's fixed sentences verbatim, with no interpolation. Open issue 20261116T052754Z-secretsmanager-stopreplicationtoreplica-on-a-primary-standalone-secret-still-returns.md reports the same wrong sentence from the secretsmanager.replication-lifecycle check (and itself reopened 20261025T120559Z); it does not carry this check's replay key, and it treats the single sentence as the only requirement, whereas this check also pins the distinct replicated-primary message — so the handler must emit both. If one fix covers both, they will close together.
Traces: secretsmanager.stop-replication-to-replica-on-a-primary-secret-rejected

## Evidence
Behavioral suite simparity-behavioral-aws-v1.

Cause 1 — [mismatch] secretsmanager.batch-get-secret-value-max-results-above-twenty-and-fabricated-next-token-rejected, call BatchGetSecretValue(Filters=[{Key:'name',Values:[<name>]}], MaxResults=21):
  AssertionError: MaxResults=21 must be refused, got 'ValidationException': "1 validation error detected: Value '21' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 20"
  Expected: 400 InvalidRequestException, Message "Max results must not exceed 20."
  Actual: 400 ValidationException with the generic constraint text above.
  Not yet exercised because the check stopped here: BatchGetSecretValue(Filters=..., NextToken='<fabricated>') must answer 400 InvalidNextTokenException "The pagination token is no longer valid."

Cause 2 — [mismatch] secretsmanager.put-resource-policy-exceeds-max-length-rejected, call PutResourcePolicy(SecretId=<arn>, ResourcePolicy=<a valid JSON policy exactly 20481 characters long>):
  AssertionError: a resource policy one character over the 20480-character maximum must be refused with InvalidParameterException; got 'ValidationException': '1 validation error detected: Value \'{"Version": "2012-10-17", "Statement": [{"Sid": "XXXXXXXXXXXXXXX ...\' at \'resourcePolicy\' failed to satisfy constraint: Member must have length less than or equal to 20480'
  Expected: 400 InvalidParameterException, Message "ResourcePolicy exceeds size limit." (no part of the document quoted back)
  Actual: 400 ValidationException with the constraint text, which quotes a prefix of the submitted document.
  Effect-absent assertions after the refusal passed: the attached in-range policy was unchanged and a fresh in-range PutResourcePolicy still succeeded.

Cause 3 — [mismatch] secretsmanager.list-secrets-primary-region-filter-matches-a-replica:
  AssertionError: primary-region='us-east-1' in us-east-1 must not match the unreplicated standalone secret 'sim2real-behav-lspr-<run>-standalone', got ['sim2real-behav-lspr-<run>-standalone']
  Expected: ListSecrets(Filters=[{Key:'name',Values:[<shared prefix>]},{Key:'primary-region',Values:[<the Region the call is made in>]}]) -> SecretList == [] when the only secret in that Region is a standalone, never-replicated secret.
  Actual: the standalone secret is listed.
  Controls that passed in the same check: primary-region=<home Region> in the away Region listed only the replica, and in the home Region listed only the primary. (Region names and secret names are this run's values; the requirement is that a secret with no PrimaryRegion matches no primary-region value.)

Cause 4 — [mismatch] secretsmanager.stop-replication-to-replica-on-a-primary-secret-rejected, call StopReplicationToReplica(SecretId=<arn of a freshly created, never-replicated secret>):
  AssertionError: unexpected message for an unreplicated secret: "Operation not permitted on a primary secret. Call must be made in replica secret's region."
  Expected: 400 InvalidParameterException, Message "Operation not permitted on a primary/standalone secret."
  Actual: 400 InvalidParameterException (code correct), Message "Operation not permitted on a primary secret. Call must be made in replica secret's region." — the sentence AWS reserves for a primary that still has a live replica.
  Also pinned by the same check and not yet exercised: after ReplicateSecretToRegions reaches InSync, StopReplicationToReplica on the primary must answer exactly "Operation not permitted on a primary secret. Call must be made in replica secret's region.", and after the replica-side promotion the promoted secret must answer "Operation not permitted on a primary/standalone secret." again.

## Steps to Reproduce
Cause 1:
1. CreateSecret(Name=N, SecretString="x").
2. BatchGetSecretValue(Filters=[{"Key":"name","Values":[N]}], MaxResults=20) -> serves the one secret (control).
3. BatchGetSecretValue(Filters=[{"Key":"name","Values":[N]}], MaxResults=21) -> expect 400 InvalidRequestException "Max results must not exceed 20."; observe 400 ValidationException with the generic range-constraint message.
4. BatchGetSecretValue(Filters=[...], NextToken="not-a-real-token") -> expect 400 InvalidNextTokenException "The pagination token is no longer valid."

Cause 2:
1. CreateSecret(Name=N, SecretString="x") -> ARN.
2. PutResourcePolicy(SecretId=ARN, ResourcePolicy=<valid policy under 20480 chars>) then GetResourcePolicy -> the policy reads back (control).
3. Build the same policy with a Sid padded so the document is exactly 20481 characters and call PutResourcePolicy(SecretId=ARN, ResourcePolicy=<that document>) -> expect 400 InvalidParameterException "ResourcePolicy exceeds size limit."; observe 400 ValidationException quoting the document prefix.
4. GetResourcePolicy -> still the in-range policy (effect absent, passes).

Cause 3:
1. In Region H: CreateSecret(Name=P, SecretString="x"); in a second Region A: CreateSecret(Name=S, SecretString="x") (S never replicated). Give P and S a shared name prefix.
2. In H: ReplicateSecretToRegions(SecretId=P, AddReplicaRegions=[{Region: A}]); poll DescribeSecret until the A entry is InSync.
3. In A: ListSecrets(Filters=[{"Key":"name","Values":[<shared prefix>]},{"Key":"primary-region","Values":[H]}]) -> only P (control, passes).
4. In A: ListSecrets(Filters=[{"Key":"name","Values":[<shared prefix>]},{"Key":"primary-region","Values":[A]}]) -> expect []; observe S listed.

Cause 4:
1. CreateSecret(Name=N, SecretString="x") -> ARN; the secret has no replica Regions.
2. StopReplicationToReplica(SecretId=ARN) -> expect 400 InvalidParameterException "Operation not permitted on a primary/standalone secret."; observe the message "Operation not permitted on a primary secret. Call must be made in replica secret's region."
3. ReplicateSecretToRegions(SecretId=ARN, AddReplicaRegions=[{Region: <second Region>}]); poll to InSync, then StopReplicationToReplica(SecretId=ARN) -> expect 400 InvalidParameterException "Operation not permitted on a primary secret. Call must be made in replica secret's region."
4. In the replica Region: StopReplicationToReplica(SecretId=N) -> succeeds and promotes it; calling it again there -> expect "Operation not permitted on a primary/standalone secret."
