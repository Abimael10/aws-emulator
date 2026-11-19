Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.elasticache-lua-eval-atomicity-and-script-cache elasticache.elasticache-streams-xautoclaim-min-idle-threshold-selects-pending-entries elasticache.cache-cluster-duplicate-id
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.elasticache-lua-eval-atomicity-and-script-cache|_resp_bulk#9|AssertionError|assert raw.startswith(b"$"), (
Issue-Key: elasticache.elasticache-streams-xautoclaim-min-idle-threshold-selects-pending-entries|_resp_bulk#10|AssertionError|assert raw.startswith(b"$"), (
Issue-Key: elasticache.cache-cluster-duplicate-id|run#9|AssertionError|assert refused == (
Issue-Commit: b448d9011382618e887f073932b66b1aa6876ff6

# ElastiCache: 3 net-new causes — the served cache engine has no EVAL/SCRIPT or stream (XADD) command family, and DeleteCacheCluster's creating-state refusal is the emulator's own sentence

> _Note: this issue was identified against the code base at commit `b448d9011382618e887f073932b66b1aa6876ff6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteCacheCluster

## Description
Three net-new root causes in the elasticache emulator, grounded in the three failing elasticache checks the replay report binds to no open issue (not marked ALREADY FILED and not OWNED by any open issue). A fixer must close all THREE before submitting.

Cause 1 of 3: the cache engine served on a cache cluster's endpoint does not implement the Lua scripting family (SCRIPT LOAD / SCRIPT EXISTS / EVAL / EVALSHA).
  AWS: recorded behavior only for the RESP wire — no Smithy shape governs engine commands (the control-plane surface involved is CreateCacheCluster/CreateReplicationGroup, which advertise the node Endpoint the client then connects to). On a Redis OSS / Valkey ElastiCache node the scripting family is available: `SCRIPT LOAD <source>` answers a RESP bulk string `$40\r\n<40-hex-sha1>\r\n` whose payload is the SHA-1 of the script body, `EVALSHA <sha> <numkeys> ...` then executes the cached script atomically, and `SCRIPT EXISTS <sha>` answers an array of integers. The reply TYPE (bulk string) and the sha being the hash of the submitted source are the requirement; the particular sha and script text come from the request.
  Emulator: the endpoint accepts the RESP session and answers other commands, but `SCRIPT LOAD` is refused with the Redis-shaped unknown-verb error `-ERR unknown command 'script', with args beginning with: \r\n`. Fixed text is Redis's own unknown-command template; the single variable part is the verb echoed from the request.
  Why: a client that uses server-side Lua (the documented way to get atomic multi-key read-modify-write on ElastiCache) gets a 200 from every control-plane call, an 'available' cluster and a connectable endpoint, and then an 'unknown command' for EVAL/EVALSHA — so script-cache behavior and the atomicity guarantee it exists for can never be exercised, and an emulator-backed test of such a client passes nothing.
  Fix: the emulator's ElastiCache data plane — the RESP command dispatcher backing the cache endpoint — must implement SCRIPT (LOAD/EXISTS/FLUSH) plus EVAL/EVALSHA with a per-node script cache keyed by SHA-1 of the source, executing the body atomically against the node's keyspace.
  Related open issue, which does NOT carry this signature (it reports the same dispatcher missing a DIFFERENT family, SLOWLOG): 20261118T131546Z-elasticache-logs-elasticache-the-served-cache-engine-answers-slowlog-with.md.
  Traces: elasticache.elasticache-lua-eval-atomicity-and-script-cache (run -> _resp_bulk, check lines 1715/1415).

Cause 2 of 3: the same dispatcher does not implement the stream family (XADD, and with it XREADGROUP / XPENDING / XAUTOCLAIM).
  AWS: recorded behavior only for the RESP wire. On a Redis OSS / Valkey ElastiCache node `XADD <key> * <field> <value>` answers a RESP bulk string holding the generated entry id `$<len>\r\n<ms>-<seq>\r\n`; consumer groups (XGROUP CREATE, XREADGROUP), XPENDING and XAUTOCLAIM then operate on that stream, and XAUTOCLAIM selects exactly the pending entries whose idle time is at or above the requested min-idle-time. The reply type and the id's `<ms>-<seq>` shape are the requirement; the id values themselves are per-run.
  Emulator: `XADD` is refused with `-ERR unknown command 'xadd', with args beginning with: \r\n` (same unknown-verb template as Cause 1, variable part is the verb from the request), so no stream can be created and every downstream consumer-group/claim assertion is unreachable.
  Why: a client using ElastiCache streams for work queues sees a healthy cluster and a connectable endpoint but cannot append a single entry; min-idle-time based redelivery — the behavior the whole consumer-group design depends on — is unobservable against the emulator.
  Fix: the ElastiCache data plane's RESP command dispatcher must implement the stream type and its commands (XADD, XLEN, XRANGE, XGROUP, XREADGROUP, XACK, XPENDING, XCLAIM, XAUTOCLAIM) with per-entry idle tracking in the node's keyspace.
  Traces: elasticache.elasticache-streams-xautoclaim-min-idle-threshold-selects-pending-entries (run -> _resp_bulk, check lines 1758/1335).

Cause 3 of 3: DeleteCacheCluster on a cluster that is still `creating` refuses with the emulator's own sentence instead of AWS's state-gate text.
  AWS: Smithy DeleteCacheCluster error InvalidCacheClusterStateFault, awsQueryError code `InvalidCacheClusterState`, HTTP 400. AWS answers `Can only delete cache clusters with state in: available, failed, incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster <cache-cluster-id> has state: <state>.` — the fixed text is the whole sentence pair including the permitted-state list `available, failed, incompatible-parameters, incompatible-network, restore-failed, recovering`; the two variable parts are the requested CacheClusterId and the cluster's current status from the emulator's state (`creating` here).
  Emulator: same code (`InvalidCacheClusterState`) but the message is `Cache cluster <cache-cluster-id> is in <state> state, not available.` — it never names the set of states from which a delete IS allowed.
  Why: a client (or an operator's error handler) matching AWS's sentence cannot tell this refusal apart from any other invalid-state refusal, and loses the actionable part of the message — which states permit the delete. Deletion-retry logic keyed on AWS's text never fires.
  Fix: DeleteCacheCluster's state gate in the operation handler (services/service-com-amazonaws-elasticache-amazonelasticachev9/src/ops/delete_cache_cluster.rs) must emit the permitted-state-list message rather than the shared 'not available' sentence.
  Related open issue, which does NOT cover this operation (it clusters TestFailover, ModifyReplicationGroupShardConfiguration, StartMigration and CreateServerlessCache): 20261109T151249Z-elasticache-state-gate-and-limit-refusals-answer-one.md.
  Traces: elasticache.cache-cluster-duplicate-id (run, check line 91; the CacheClusterAlreadyExists leg of the same check passes).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: elasticache 126/161 (78%), 35 failures, of which 32 are owned by open issues or already filed for unimplemented stubs. The three below are the net-new ones.

Cause 1 — [mismatch] elasticache.elasticache-lua-eval-atomicity-and-script-cache
check_elasticache_elasticache_lua_eval_atomicity_and_script_cache.py:1715 `got_sha = _resp_bulk(loaded[0], source=src, cmd="SCRIPT LOAD")` -> :1415
AssertionError: [runner-outside-vpc] SCRIPT LOAD must return a RESP bulk string ($<len>\r\n<bytes>\r\n), got b"-ERR unknown command 'script', with args beginning with: \r\n"
Expected (AWS): a bulk string carrying the 40-hex SHA-1 of the submitted script, after which EVALSHA runs it atomically.

Cause 2 — [mismatch] elasticache.elasticache-streams-xautoclaim-min-idle-threshold-selects-pending-entries
check_elasticache_elasticache_streams_xautoclaim_min_idle_threshold_selects_pending_entries.py:1758 `auto_ids = [_resp_bulk(r, source=src, cmd="XADD") for r in call(add_cmds)]` -> :1335
AssertionError: [runner-outside-vpc] XADD must return a RESP bulk string ($<len>\r\n<bytes>\r\n), got b"-ERR unknown command 'xadd', with args beginning with: \r\n"
Expected (AWS): a bulk string entry id of the form <ms>-<seq>; the stream then supports XGROUP/XREADGROUP/XPENDING/XAUTOCLAIM.

Cause 3 — [mismatch] elasticache.cache-cluster-duplicate-id
check_elasticache_cache_cluster_duplicate_id.py:91
AssertionError: expected InvalidCacheClusterState with its exact message, got ('InvalidCacheClusterState', 'Cache cluster <cache-cluster-id> is in creating state, not available.')
Expected (AWS), HTTP 400 / InvalidCacheClusterState: 'Can only delete cache clusters with state in: available, failed, incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster <cache-cluster-id> has state: creating.'

In all three, the prober label, cluster ids, generated script shas, stream entry ids, host and port are per-run values and are not part of the requirement; the reply types, command availability, HTTP status, error code and the message templates are.

## Steps to Reproduce
Cause 1: CreateCacheCluster(Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, CacheSubnetGroupName=<group>); wait CacheClusterStatus='available'; read the node Endpoint from DescribeCacheClusters(ShowCacheNodeInfo=True); open a RESP session to it and send `SCRIPT LOAD "return redis.call('SET', KEYS[1], ARGV[1])"`. Expect a RESP bulk string with the 40-hex SHA-1; observe `-ERR unknown command 'script', ...`. Then EVALSHA that sha and expect the script to run atomically.

Cause 2: same cluster and endpoint; send `XADD <key> * f v`. Expect a bulk-string entry id `<ms>-<seq>`; observe `-ERR unknown command 'xadd', ...`. Then XGROUP CREATE / XREADGROUP / XAUTOCLAIM <key> <group> <consumer> <min-idle-ms> 0 and expect only the pending entries idle at least min-idle-ms to be claimed.

Cause 3: CreateCacheCluster(CacheClusterId=<id>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, CacheSubnetGroupName=<group>); while DescribeCacheClusters still reports CacheClusterStatus='creating', call DeleteCacheCluster(CacheClusterId=<id>). Expect HTTP 400, Error.Code 'InvalidCacheClusterState', Error.Message 'Can only delete cache clusters with state in: available, failed, incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster <id> has state: creating.'; observe 'Cache cluster <id> is in creating state, not available.'

Re-replay elasticache.elasticache-lua-eval-atomicity-and-script-cache, elasticache.elasticache-streams-xautoclaim-min-idle-threshold-selects-pending-entries and elasticache.cache-cluster-duplicate-id to confirm the fix.
