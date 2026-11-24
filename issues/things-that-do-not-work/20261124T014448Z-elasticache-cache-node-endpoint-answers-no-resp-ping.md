Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments|_ready_prober#10|TimeoutError|raise TimeoutError(f"a RESP connection to {subject} at {host}:{port}: the run budget ended first; not graded")
Issue-Key: elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure|_ready_prober#18|TimeoutError|raise TimeoutError(f"a RESP connection to {subject} at {host}:{port}: the run budget ended first; not graded")
Issue-Commit: dd0dfa661ba492304c3b4ddf1bd869b805afe51f

# ElastiCache cache-node endpoint answers no RESP PING: a non-TLS single-node redis cluster that reports 'available' with a CacheNodes[].Endpoint never accepts a RESP connection, so both data-plane checks die in the readiness probe

> _Note: this issue was identified against the code base at commit `dd0dfa661ba492304c3b4ddf1bd869b805afe51f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheClusters

## Description
Cause 1 of 1: a single-node Redis cache cluster created with Port=6379, TransitEncryptionEnabled=false, a cache subnet group over the run's private subnets and a security group that admits the private CIDR on 6379, reaches CacheClusterStatus 'available' and publishes CacheNodes[0].Endpoint{Address,Port}, but nothing at that advertised address:port ever answers a RESP PING. Failing check ids: elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments, elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure.

AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json): Endpoint is documented "Represents the information required for client programs to connect to a cache node. This value is read-only", Endpoint$Address "The DNS hostname of the cache node", Endpoint$Port "The port number that the cache engine is listening on"; CacheNode$Endpoint "The hostname for connecting to this cache node"; CreateCacheCluster$Port "The port number on which each of the cache nodes accepts connections". Recorded behavior: once DescribeCacheClusters reports CacheClusterStatus 'available' and ShowCacheNodeInfo=true returns a CacheNodes[0].Endpoint, a TCP connect to that Address:Port from inside the cluster's VPC (with an admitting security-group ingress rule) succeeds and speaks RESP2 — PING answers +PONG, INCR on a fresh key answers the integer reply :1, GET answers the bulk string $<len>\r\n<bytes>\r\n, OBJECT ENCODING on an INCR-built key answers $3\r\nint\r\n, and INFO memory/stats publish maxmemory_policy and evicted_keys lines.

Emulator: the cluster does provision (CreateCacheCluster answers CacheClusterStatus 'creating', the cluster settles to 'available', CacheNodes[0].Endpoint is populated and maps to a reachable-looking address:port), but the readiness prober's repeated RESP PING to that endpoint never returns ok across every attempt it can fit in the remaining per-check budget, and the check dies in _ready_prober with "TimeoutError: a RESP connection to cache cluster <cluster-id> at <host>:<port>: the run budget ended first; not graded". No INCR, GET, STRLEN, INCRBY, OBJECT ENCODING, INFO or DBSIZE assertion is ever reached, so neither single-key INCR atomicity nor maxmemory-policy eviction is observable. The host/port/cluster-id in that message are per-run values; the requirement is that whatever Address/Port the cluster projection publishes is one the emulator listens on and serves RESP from. Hypothesis (the replay cannot separate these): either the cache data plane is not served for a plaintext (TransitEncryptionEnabled=false) cluster created with an explicit Port, or the published Endpoint.Port is not the port the served engine actually listens on — the emulator is already known to ignore the requested cache Port (open issue 20261118T043247Z-elasticache-cluster-and-replication-group-projections-omit-always.md), which would make the published endpoint unconnectable even though an engine is listening elsewhere. A secondary contributor to the budget exhaustion is the settling path (services/service-com-amazonaws-elasticache-amazonelasticachev9/src/state.rs:468, CLUSTER_TRANSITION = 3 s): each control-plane status wait costs the client its whole 15 s poll interval before the probe gets any time at all; that mechanism is already filed (cause 3 of 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait.md and 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md) and is NOT re-filed here.

Related open issues do not cover these two check ids: 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait.md reports an endpoint that accepts no TCP, but through a VPC-attached Lambda probe failing with "a Lambda function in this emulator has no host network to reach" — here the probe runs on the in-VPC runner itself and gets no RESP answer at all; 20261119T170526Z-elasticache-cache-node-data-plane-ignores-its-security.md reports the OPPOSITE symptom (the endpoint keeps accepting RESP connections after ingress is revoked), i.e. some clusters' endpoints do serve RESP; 20261119T112538Z-elasticache-3-net-new-causes-the-served-cache.md reports a served engine answering specific command families with "unknown command". None of them carries elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments or elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure.

Why: a client that provisions a cache cluster, waits for 'available' and connects to the endpoint DescribeCacheClusters publishes cannot store or read a single key. Every behaviour that only the engine can show — per-key atomicity of INCR (no lost updates), the decimal-string/int encoding of a counter, and maxmemory-policy eviction under memory pressure — is unobservable, and the client cannot distinguish "my security group is wrong" from "this endpoint is not served".

Fix: the ElastiCache cache-cluster data plane and the endpoint projection in the elasticache crate — serve RESP on exactly the Address and Port the CacheCluster/CacheNodes projection publishes (including a cluster created with an explicit CreateCacheCluster$Port and TransitEncryptionEnabled=false), and have the endpoint become connectable no later than the moment DescribeCacheClusters reports CacheClusterStatus 'available' with that endpoint.

Traces: elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments, elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: 151/194 passed; elasticache 132/161 (29 failed). These are the only two elasticache failures the report binds to no open issue and does not mark ALREADY FILED.

1) elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments [error]:
  File "check_elasticache_elasticache_concurrent_incr_on_one_key_loses_no_increments.py", line 820, in run
    probe = _ready_prober(ctx, host, port, tls=False, subject=f"cache cluster {cluster_id}")
  File "...", line 548, in _ready_prober
    raise TimeoutError(f"a RESP connection to {subject} at {host}:{port}: the run budget ended first; not graded")
  TimeoutError: a RESP connection to cache cluster sim2real-behav-ecinc-245f2336388e at 127.0.153.133:39301: the run budget ended first; not graded

2) elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure [error]:
  File "check_elasticache_elasticache_redis_maxmemory_policy_evicts_under_pressure.py", line 909, in run
    probe = _ready_prober(ctx, host, port, tls=False, subject=f"cache cluster {cluster_id}")
  File "...", line 561, in _ready_prober
    raise TimeoutError(...)
  TimeoutError: a RESP connection to cache cluster sim2real-behav-ecevict-e41eed390718 at 127.0.136.231:35047: the run budget ended first; not graded

The prober (check source) loops `prober.resp(host, port, [["PING"]], tls=False)` every 2 s and returns as soon as the reply is ok; it only raises after every attempt in its window failed. It reached the probe at all, which means the control plane answered: CreateCacheCluster returned CacheClusterStatus 'creating', _wait_cluster_available saw 'available', and _resolved_endpoint read a non-empty CacheNodes[0].Endpoint{Address,Port} (an empty endpoint would have raised the DATAPLANE_UNIMPLEMENTED 'advertises no connectable endpoint' assertion instead). So the divergence is strictly that the advertised endpoint answers no RESP PING.

Expected after the probe (never reached): 400 concurrent INCRs over 8 connections return exactly the sequence 1..400, GET returns $3\r\n400\r\n, STRLEN 3, INCRBY 100 returns :500, OBJECT ENCODING returns $3\r\nint\r\n; and for the eviction check INFO memory reports maxmemory_policy=allkeys-lru, INFO stats reports an evicted_keys line, and writing past maxmemory keeps accepting SET while DBSIZE drops.

Model evidence: Endpoint{Address: 'The DNS hostname of the cache node', Port: 'The port number that the cache engine is listening on'}, doc 'Represents the information required for client programs to connect to a cache node'; CreateCacheCluster$Port 'The port number on which each of the cache nodes accepts connections'. Checkout evidence for the secondary wait cost: services/service-com-amazonaws-elasticache-amazonelasticachev9/src/state.rs:468 `pub const CLUSTER_TRANSITION: Duration = Duration::from_secs(3);` with the module note that nothing sleeps and the due instant is materialised by whichever later call reads it.

## Steps to Reproduce
1. Boot the emulator from this checkout and run suite simparity-behavioral-aws-v1.
2. Create a VPC with two private subnets and a security group admitting the VPC CIDR on TCP 6379; CreateCacheSubnetGroup over those subnets.
3. CreateCacheCluster(CacheClusterId=<id>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, Port=6379, CacheSubnetGroupName=<sng>, SecurityGroupIds=[<sg>], TransitEncryptionEnabled=False). Confirm the response reports CacheClusterStatus 'creating'.
4. Poll DescribeCacheClusters(CacheClusterId=<id>) until CacheClusterStatus == 'available', then DescribeCacheClusters(CacheClusterId=<id>, ShowCacheNodeInfo=True) and read CacheNodes[0].Endpoint.{Address,Port}.
5. From inside that VPC, TCP-connect to that Address:Port and send the RESP2 command PING (*1\r\n$4\r\nPING\r\n). Expect +PONG; observe that no attempt over the whole readiness window gets a reply.
6. Then send INCR on a fresh key (expect :1), GET/STRLEN/INCRBY/OBJECT ENCODING, and INFO memory / INFO stats on a cluster bound to a parameter group with maxmemory-policy=allkeys-lru (expect maxmemory_policy and evicted_keys lines).
7. Re-replay elasticache.elasticache-concurrent-incr-on-one-key-loses-no-increments and elasticache.elasticache-redis-maxmemory-policy-evicts-under-pressure: both must get past _ready_prober and reach their own RESP assertions.
