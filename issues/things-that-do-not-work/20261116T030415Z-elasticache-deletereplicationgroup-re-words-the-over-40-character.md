Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-replication-group-lifecycle-redis
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-replication-group-lifecycle-redis
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DeleteReplicationGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# elasticache DeleteReplicationGroup re-words the over-40-character ReplicationGroupId refusal

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteReplicationGroup

## Description
One net-new root cause in the elasticache service, grounded in the single wire divergence this run's replay report binds to no open issue and does not mark ALREADY FILED (25 of the run's 26 clusters carry an `owned_by`).

Cause 1 of 1: the ReplicationGroupId length refusal of DeleteReplicationGroup is emulator-authored instead of AWS's sentence.

AWS: Smithy: DeleteReplicationGroup input member ReplicationGroupId (target String, required); the operation's modeled client errors include InvalidParameterValueException (httpError 400, awsQueryError code `InvalidParameterValue`). The Smithy model carries NO @length bound on the identifier, so the 40-character maximum and the refusal text are recorded behavior only: AWS refuses a ReplicationGroupId longer than 40 characters with HTTP 400, a Query-protocol ErrorResponse whose Error.Message is the FIXED sentence `Replication Group ID cannot be longer than 40 characters.` The sentence has NO variable part — it does not interpolate the offending id, the member name or the actual length; the only request-derived input to the rule is the identifier's length, which selects the refusal.

Emulator: the same request is refused with the same HTTP 400 and the same error code, but Error.Message is `The parameter ReplicationGroupId is not a valid identifier because it is longer than 40 characters.` — a different, locally invented sentence that names the member (`ReplicationGroupId`) and recasts the rule as an identifier-validity statement. The status and code are correct; only the message diverges.

Why: the error code InvalidParameterValue is shared by every request-validation refusal this operation can raise (bad FinalSnapshotIdentifier, bad RetainPrimaryCluster combination, bad identifier characters, over-long identifier), so the MESSAGE is the only thing distinguishing them on the wire. A client that branches on the refusal text — a teardown/retry helper that must tell "the id you passed is too long, truncate it" from another InvalidParameterValue, a log parser, or a test asserting the service's documented message — matches nothing here and falls through to its generic failure path. A caller that trims its id to AWS's limit on seeing AWS's sentence never does so against the emulator.

Fix: the request-validation path of the DeleteReplicationGroup handler (the ReplicationGroupId length check that composes the InvalidParameterValue refusal) must emit AWS's fixed sentence verbatim rather than a handler-local phrasing. Note the validation is shared across the replication-group operations, so the same string likely feeds every one of them; the requirement is AWS's wording, not the emulator's.

Related open issue, which does NOT cover this cause: 20261116T010037Z-elasticache-replication-group-delete-not-found-refusals-the.md bundles six behavioral causes on DeleteReplicationGroup — its not-found text, its InvalidReplicationGroupState state-gate wording, the `deleting` window, shard endpoint identity and the auth-token rotate flow. None of its six causes is the InvalidParameterValue identifier-length refusal, and it carries behavioral check ids rather than this fixture step's replay key, so this divergence would not be re-replayed by closing it.

Traces: elasticache-replication-group-lifecycle-redis#task#11.

## Evidence
Replay of wire trace set simparity-wire-aws-v1 against this checkout: 27 fixtures replayed, 246 steps (202 passed, 44 mismatched, 0 errored, 0 blocked), score 82.1%, 26 divergence clusters, emulator_up true. This is the only cluster with an empty `owned_by` and an empty `stub_path`.

Cluster (verdict: mismatch), service elasticache, operation DeleteReplicationGroup, fixture `elasticache-replication-group-lifecycle-redis`, step `task#11`, affected_count 1:
  expected_status: 400
  actual_status:   400
  detail: $.ErrorResponse.Error.Message: value_mismatch
    expected 'Replication Group ID cannot be longer than 40 characters.'
    got      'The parameter ReplicationGroupId is not a valid identifier because it is longer than 40 characters.'

Fixed vs variable parts: the recorded AWS message is entirely fixed text — 'Replication Group ID cannot be longer than 40 characters.' — with no value taken from the request or from emulator state, so nothing in it is per-run. The emulator's replacement text is fixed too; it simply differs. No other JSON path under $.ErrorResponse diverged, so the HTTP status (400) and the Query-protocol error code (InvalidParameterValue) already match and only Error.Message is wrong.

Model evidence (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json, via smithy_query query=operation): DeleteReplicationGroup input {ReplicationGroupId: String (required, clientOptional), RetainPrimaryCluster: BooleanOptional, FinalSnapshotIdentifier: String}; errors include InvalidParameterValueException -> httpError 400, awsQueryError code 'InvalidParameterValue'. The model declares no @length constraint on ReplicationGroupId, hence the 40-character rule and its wording are classified here as recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an ElastiCache client (Query protocol, API version 2015-02-02) at its data listener.
2. Call DeleteReplicationGroup with a ReplicationGroupId longer than 40 characters, e.g. a 41+ character identifier of legal characters (lowercase letters, digits and hyphens, starting with a letter). The id need not exist — the length refusal precedes the existence lookup.
3. Expect HTTP 400 with a Query ErrorResponse: Error.Code 'InvalidParameterValue' and Error.Message exactly 'Replication Group ID cannot be longer than 40 characters.'
4. Observe HTTP 400 and Error.Code 'InvalidParameterValue' (correct) but Error.Message 'The parameter ReplicationGroupId is not a valid identifier because it is longer than 40 characters.'
5. To confirm a fix, re-replay the wire fixture `elasticache-replication-group-lifecycle-redis`, step `task#11`.
