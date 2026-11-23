Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-serverless-cache-iam-user-endpoint-discovery
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-serverless-cache-iam-user-endpoint-discovery
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeServerlessCaches|2xx->2xx|$.DescribeServerlessCachesResponse.DescribeServerlessCachesResult.ServerlessCaches.member.Endpoint.Port
Issue-Key: elasticache|DescribeServerlessCaches|2xx->2xx|$.DescribeServerlessCachesResponse.DescribeServerlessCachesResult.ServerlessCaches.member.ReaderEndpoint.Port
Issue-Commit: 33ea92ed4461881d8f7ae2a85609594a4bd6a1fc

# ElastiCache serverless cache publishes per-run listener ports as Endpoint.Port and ReaderEndpoint.Port instead of the engine's serverless ports

> _Note: this issue was identified against the code base at commit `33ea92ed4461881d8f7ae2a85609594a4bd6a1fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServerlessCaches

## Description
One cause, one fault: the serverless-cache endpoint projection publishes whatever ephemeral port the emulator's own data listener happened to be bound to for this run, instead of the fixed ports an ElastiCache serverless cache always serves. The two published endpoints also stop being distinguishable: AWS's reader endpoint differs from the primary endpoint only by its port, and the emulator emits two unrelated high ports instead.

Cause 1 of 1: DescribeServerlessCaches reports non-standard Endpoint.Port and ReaderEndpoint.Port for an available serverless cache.
AWS: Smithy com.amazonaws.elasticache#ServerlessCache models Endpoint and ReaderEndpoint, both com.amazonaws.elasticache#Endpoint {Address: String "The DNS hostname of the cache node", Port: Integer "The port number that the cache engine is listening on"}; the model itself states no port value, so this is recorded behavior only. The recording for an available Valkey/Redis OSS serverless cache carries Endpoint.Port = 6379 (the engine's primary/write port) and ReaderEndpoint.Port = 6380 (the companion read port), i.e. two FIXED engine ports that are a property of serverless ElastiCache, not of the account, the run or the cache's name. Only the two Address values are per-environment; the two port numbers are constants, and the reader port is exactly the primary port + 1.
Emulator: both ports are the emulator's own per-run listener ports — unrelated ephemeral high ports, one per endpoint, with no fixed value and no 6379/6380 relationship between them (replay: Endpoint.Port expected '6379', got a high ephemeral port; ReaderEndpoint.Port expected '6380', got a different high ephemeral port). Status is 200 on both sides; only these two values diverge.
Why: a client that performs endpoint discovery — reads ServerlessCache.Endpoint/ReaderEndpoint from the API and hands the address:port to a Valkey/Redis client, or asserts that the serverless cache answers on the standard cache port — gets a port that is not the engine's and that changes on every boot, so configuration captured from one run is wrong on the next. Worse, because the primary and reader ports are independent random values, the write/read split can no longer be recognised by the documented 6379/6380 convention: a client that derives the reader endpoint from the primary one (or validates that the pair differs only in port) computes an endpoint that does not exist.
Fix: the serverless-cache endpoint derivation in the elasticache crate — the serverless-cache state should store the engine's serverless port pair for the cache's engine (6379 primary / 6380 reader for Valkey and Redis OSS) and the DescribeServerlessCaches / CreateServerlessCache / ModifyServerlessCache ServerlessCache projection should serialize those, not the address of whatever listener the emulator bound this run. If a dialable local port is still needed for the data plane, it belongs behind the address, not in the published Port.
Traces: elasticache-serverless-cache-iam-user-endpoint-discovery#task#11.

Related open issues that do NOT cover this step: 20261118T043247Z-elasticache-cluster-and-replication-group-projections-omit-always (CacheCluster/ReplicationGroup node-group ports echoing a requested Port — different operations, different fixtures), 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait (reachability of the advertised cache endpoint, not the published serverless port values) and 20261114T214625Z-elasticache-serverless-snapshot-restore-round-trip-still-never (a convergence/budget finding on serverless snapshot-restore). Replay reported owned_by empty and stub_path empty for this cluster, so no open issue carries its replay key.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator_up true, /__admin/health 200, data listener accepting; steps_total 333, steps_passed 288, divergence_count 27).

Cluster: service=elasticache, operation=DescribeServerlessCaches, verdict=mismatch, expected_status=200, actual_status=200, stub_path empty, owned_by empty, affected_count=1.
fixture_id=elasticache-serverless-cache-iam-user-endpoint-discovery, step_key=task#11.

Detail as reported:
  $.DescribeServerlessCachesResponse.DescribeServerlessCachesResult.ServerlessCaches.member.Endpoint.Port: value_mismatch (expected '6379', got '45885')
  $.DescribeServerlessCachesResponse.DescribeServerlessCachesResult.ServerlessCaches.member.ReaderEndpoint.Port: value_mismatch (expected '6380', got '45685')

Expected vs actual, stated as the requirement: Endpoint.Port MUST be the engine's serverless primary port (6379 for Valkey/Redis OSS) and ReaderEndpoint.Port MUST be its reader port (6380). The concrete numbers 45885 and 45685 are this run's listener ports and are not themselves the requirement — the requirement is that the two published ports are the engine's fixed pair and are stable across boots, with the reader port one above the primary. Everything else in the response matched, including both endpoint Addresses and all other ServerlessCache members.

## Steps to Reproduce
1. Clone the repo and boot the emulator, then replay the bound wire corpus (replay_wire_corpus).
2. Fixture elasticache-serverless-cache-iam-user-endpoint-discovery: create an IAM-authenticated user/user group and a serverless cache, wait for the cache to become available, then at step task#11 call DescribeServerlessCaches for that cache.
3. Read ServerlessCaches.member.Endpoint.Port and ServerlessCaches.member.ReaderEndpoint.Port from the response and compare with the recording: AWS returns 6379 and 6380; the emulator returns two unrelated ephemeral ports.
4. Reboot the emulator and repeat step 3: the two values change again, confirming they are per-run listener ports rather than the engine's serverless ports.
