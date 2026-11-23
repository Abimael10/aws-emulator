Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs
Issue-Key: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs|_body#1|Timeout|time.sleep(_POLL_S)
Issue-Commit: a5a3c6bd63789eb42e1283a455a4e127273ca01a

# elasticache__logs: the slow-log-to-CloudWatch-Logs join never converges — the check is killed by the replay budget before any delivered record can be read (1 cause)

> _Note: this issue was identified against the code base at commit `a5a3c6bd63789eb42e1283a455a4e127273ca01a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
One root cause (Cause 1 of 1), grounded in the single failing elasticache__logs check that the replay report binds to no open issue and to no unimplemented-operation issue.

Cause 1 of 1: the ElastiCache slow-log delivery flow — create parameter group, create a single-node Redis/Valkey cache cluster carrying a slow-log LogDeliveryConfiguration, reach 'available' with that configuration 'active', run an engine command, then read the delivered record out of the destination CloudWatch Logs group, disable delivery and re-enable it — makes no observable progress inside the replay budget. The check is terminated by the harness ('Check timed out after 60.0s') with no assertion reached at all, so the emulator never produces an answer the suite can grade for any step of the join.

AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9): CreateCacheCluster takes LogDeliveryConfigurations (LogDeliveryConfigurationRequestList) and returns CacheCluster; DescribeCacheClusters returns CacheClusters with CacheClusterStatus, CacheNodes[].Endpoint and LogDeliveryConfigurations, and ModifyCacheCluster re-writes LogDeliveryConfigurations (ApplyImmediately is required for a log-delivery change). The record shape delivered to CloudWatch Logs is recorded/documented behavior only (https://docs.aws.amazon.com/AmazonElastiCache/latest/dg/Log_Delivery.html): CacheClusterId, CacheNodeId, Id, Timestamp, 'Duration (us)', Command, ClientAddress, ClientName. On AWS every one of those calls answers immediately with a 200 and a status the client can poll to a terminal value, and the engine endpoint answers RESP commands; the only slow part on AWS is real provisioning and the periodic slow-log pull, both of which a client observes by polling a status that does change.

Emulator: every step of that sequence together makes no gradable progress in the replay budget — the check is cut off at the harness timeout rather than failing an assertion, so the emulator returned no terminal state, no endpoint-backed slow-log record and no refusal for the suite to compare. Note that two earlier replays of this same check DID reach specific assertions (SLOWLOG refused as an unknown command; the delivered record's ClientAddress being the placeholder '?:0'), so this run's signature is a net-new failure mode: the flow now stops short of those points entirely. The only diagnostic the replay emits is the timeout; Hypothesis: one of the status waits (DescribeCacheClusters CacheClusterStatus reaching 'available', or the slow-log LogDeliveryConfiguration Status reaching 'active'/inactive after ModifyCacheCluster with ApplyImmediately=true) never reaches its terminal value, or each such wait costs a client its whole poll interval, so the sequence of waits alone exhausts the budget.

Why: a client that wires ElastiCache slow-log delivery to CloudWatch Logs can never confirm the wiring. Every poll loop a client writes over DescribeCacheClusters (wait for 'available', wait for the delivery configuration to go active, wait for it to go inactive after a disable, wait for it to go active again) either hangs or advances so slowly that the client's own timeout fires, and the documented delivered record is never observable. An operator cannot tell a disabled delivery configuration apart from a delivery path that does not exist, which is exactly the distinction the negative/positive arms of this flow rest on.

Fix: the elasticache service's cache-cluster lifecycle and log-delivery components — the CreateCacheCluster / ModifyCacheCluster handlers and the state that backs DescribeCacheClusters' CacheClusterStatus and LogDeliveryConfigurations[].Status — must drive those statuses to their terminal values promptly and the slow-log delivery path must publish engine-recorded entries into the configured log group, so a polling client converges inside a normal client timeout. File and line citations belong under Traces.

Related OPEN issues that do NOT cover this failure (the replay left this check unowned, so neither of their replay keys matches this run's signature; both report assertions reached further into the same check and should be read together with this one): 20261118T131546Z-elasticache-logs-elasticache-the-served-cache-engine-answers-slowlog-with.md (SLOWLOG answered with '-ERR unknown command') and 20261119T003525Z-elasticache-logs-delivered-slow-log-records-are-synthesised.md (ClientAddress '?:0', empty ClientName). Also open and adjacent, but about other flows: 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait.md and 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md.

Traces: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs (suite report section '### elasticache__logs', verdict [mismatch]).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: per-service line 'elasticache__logs 1/2 (50%) 1 failed'.

#### [mismatch] elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs
Error: Check timed out after 60.0s

This is the complete diagnostic the replay produced: no assertion fired, no AWS error code and no HTTP status were returned for the suite to compare, because the run was cut off before the first graded observation.

Expected (AWS): the sequence completes with gradable answers at each step — CreateCacheCluster returns CacheCluster with CacheClusterId echoed and CacheClusterStatus 'creating'; DescribeCacheClusters subsequently reports CacheClusterStatus 'available' with one CacheNodes entry carrying an Endpoint (Address, Port) and a LogDeliveryConfigurations entry with LogType 'slow-log', DestinationType 'cloudwatch-logs', DestinationDetails.CloudWatchLogsDetails.LogGroup equal to the requested group, LogFormat 'json' and Status 'active'; a command run against that endpoint appears in the destination log group as a JSON record carrying CacheClusterId, CacheNodeId, Id, Timestamp, 'Duration (us)', Command, ClientAddress, ClientName; ModifyCacheCluster with ApplyImmediately=true and the configuration disabled drives Status away from 'active' and stops the stream; re-enabling returns Status to 'active' and delivery resumes.

Actual (emulator): none of the above is reached — the check is killed at the replay budget.

The cluster id, log-group name, parameter-group name, VPC CIDR, key names, record Id/Timestamp/Duration and the harness's own 60s budget value are per-run values and are not part of the requirement; the requirement is that each of these control-plane statuses reaches its terminal value and that the delivered record exists with the documented fields, promptly enough for a polling client to observe.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1, check elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs.
2. CreateCacheParameterGroup for the redis/valkey family reported by DescribeCacheEngineVersions, then ModifyCacheParameterGroup with ParameterNameValues [{slowlog-log-slower-than, '0'}, {slowlog-max-len, <n>}].
3. CreateLogGroup in CloudWatch Logs (/aws/elasticache/<name>).
4. CreateCacheCluster (Engine redis, cache.t3.micro, NumCacheNodes 1, that parameter group and a cache subnet group) with LogDeliveryConfigurations [{LogType 'slow-log', DestinationType 'cloudwatch-logs', DestinationDetails.CloudWatchLogsDetails.LogGroup <that group>, LogFormat 'json', Enabled true}].
5. Poll DescribeCacheClusters for CacheClusterStatus 'available' and for the slow-log LogDeliveryConfiguration to reach Status 'active'; then DescribeCacheClusters with ShowCacheNodeInfo=true for the node Endpoint. Observe whether each poll reaches its terminal value within a normal client timeout — it does not inside the replay budget.
6. Run a RESP command against that endpoint and FilterLogEvents on the destination log group for a slow-log record naming it.
7. ModifyCacheCluster with ApplyImmediately=true and the slow-log configuration disabled; poll until the configuration is no longer 'active'; confirm no new record arrives. Re-enable it and confirm delivery resumes.
8. Re-replay the check: it must reach and grade these assertions instead of being killed at the budget.
