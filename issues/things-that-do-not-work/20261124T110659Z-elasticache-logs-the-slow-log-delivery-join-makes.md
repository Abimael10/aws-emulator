Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs
Issue-Key: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs|_body#1|Timeout|records = _delivered(logs, log_group, cluster_id, since_ms)
Issue-Commit: 5d5108616900e975d9bd57b5edb751cc3c34fb65

# elasticache__logs: the slow-log delivery join makes no gradable progress — the check is killed at the replay budget before any control-plane status converges (1 cause)

> _Note: this issue was identified against the code base at commit `5d5108616900e975d9bd57b5edb751cc3c34fb65`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
One root cause (Cause 1 of 1). This is the only failing check in this replay that the report binds to neither an open issue nor an unimplemented-operation issue; every other failure in the run is marked OWNED by or ALREADY FILED.

Cause 1 of 1: the ElastiCache slow-log-to-CloudWatch-Logs flow never reaches a gradable observation. The check creates a cache parameter group with slowlog-log-slower-than=0 and slowlog-max-len=1000, creates a CloudWatch Logs destination group, creates a single-node redis cache cluster on cache.t3.micro with LogDeliveryConfigurations=[{LogType 'slow-log', DestinationType 'cloudwatch-logs', DestinationDetails.CloudWatchLogsDetails.LogGroup <group>, LogFormat 'json', Enabled true}], then polls DescribeCacheClusters for CacheClusterStatus 'available' and for the slow-log LogDeliveryConfigurations[].Status to reach 'active', reads the node Endpoint with ShowCacheNodeInfo=true, runs a RESP read against it and reads the delivered record back out of the log group, then disables and re-enables delivery with ApplyImmediately=true. The replay reports only 'Check timed out after 60.0s' with no stack frame and no assertion reached: the emulator produced no terminal state, no delivered record and no refusal for the suite to compare at any step.

AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json): CreateCacheCluster input takes LogDeliveryConfigurations (LogDeliveryConfigurationRequestList) and returns CacheCluster; DescribeCacheClusters returns CacheClusters with CacheClusterStatus, CacheNodes[].Endpoint (Address, Port) and LogDeliveryConfigurations, and is paginated on Marker; ModifyCacheCluster re-writes LogDeliveryConfigurations and takes ApplyImmediately. Shape LogDeliveryConfiguration has members LogType (enum slow-log|engine-log), DestinationType (enum cloudwatch-logs|kinesis-firehose), DestinationDetails, LogFormat (enum text|json), Status (enum LogDeliveryConfigurationStatus: active|enabling|modifying|disabling|error) and Message. On AWS each of those calls answers 200 immediately and the status members advance monotonically to a terminal value a polling client observes: 'creating' then 'available' for CacheClusterStatus, 'enabling' then 'active' for a newly requested slow-log configuration, 'disabling' then absent/non-active after a disable with ApplyImmediately=true, and 'enabling' then 'active' again on re-enable. The delivered record shape is recorded/documented behavior only (https://docs.aws.amazon.com/AmazonElastiCache/latest/dg/Log_Delivery.html): a JSON object carrying CacheClusterId, CacheNodeId, Id, Timestamp, 'Duration (us)', Command, ClientAddress and ClientName, written into the configured log group. No modeled error applies — this flow is a success path, so there is no AWS error code or non-2xx status to match here.

Emulator: the whole sequence yields nothing gradable. The replay's complete diagnostic is 'Check timed out after 60.0s' — unlike the earlier replays of this same check, which did reach specific assertions (the engine refusing SLOWLOG as an unknown command; a delivered record whose ClientAddress was the placeholder '?:0'), this run stops before any of those points, so the emulator never returned a terminal CacheClusterStatus, never drove the slow-log LogDeliveryConfigurations[].Status to a terminal value, and never produced a delivered record. Hypothesis: one of the status waits (CacheClusterStatus to 'available', or the slow-log configuration Status to 'active' after create and to a non-active value after ModifyCacheCluster with ApplyImmediately=true) never reaches its terminal value, or each poll costs the client its full interval, so the waits alone consume the client's budget before the data-plane arm starts.

Why: a client that wires ElastiCache slow-log delivery to CloudWatch Logs can never confirm the wiring. Every poll loop a client writes over DescribeCacheClusters either hangs or advances too slowly for the client's own timeout, so 'still enabling', 'active', and 'no delivery path at all' are indistinguishable — exactly the distinction the enabled/disabled/re-enabled arms of this flow rest on. Automation that gates on a delivery configuration becoming active, and any consumer reading slow-log records out of the destination group, both stall indefinitely.

Fix: the elasticache service's cache-cluster lifecycle and log-delivery components — the CreateCacheCluster and ModifyCacheCluster handlers, and the state backing DescribeCacheClusters' CacheClusterStatus, CacheNodes[].Endpoint and LogDeliveryConfigurations[].Status — must advance those statuses to their terminal values promptly, and the slow-log delivery path must publish engine-recorded entries, with the documented members, into the configured CloudWatch Logs group, so a polling client converges inside a normal client timeout.

Related OPEN issues that do NOT cover this failure (the replay left this check unowned, so none of their replay keys matches this run's signature; they report assertions reached further inside the same check and should be read together with this one): 20261123T163447Z-elasticache-logs-the-slow-log-to-cloudwatch-logs--r2.md (same check, replay key '|_body#1|Timeout|time.sleep(_POLL_S)' — a timeout pinned to a specific poll frame, where this run has no frame at all), 20261118T131546Z-elasticache-logs-elasticache-the-served-cache-engine-answers-slowlog-with.md (SLOWLOG answered '-ERR unknown command'), 20261119T003525Z-elasticache-logs-delivered-slow-log-records-are-synthesised.md (ClientAddress '?:0', empty ClientName), 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait.md and 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md.

Traces: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs (suite report section '### elasticache__logs', verdict [mismatch]).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout. Overall: 922/1114 passed (82.8%); per-service line 'elasticache__logs 1/2 (50%) 1 failed'.

Failing check as the report lists it:

#### [mismatch] elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs
Error: Check timed out after 60.0s

That single line is the complete diagnostic the replay produced — no assertion fired, no AWS error code and no HTTP status were returned, because the run was cut off before the first graded observation. The check is NOT marked 'OWNED by' or 'ALREADY FILED' in the report.

Expected (AWS): CreateCacheCluster returns 200 with CacheCluster echoing the requested CacheClusterId and CacheClusterStatus 'creating'; DescribeCacheClusters then reports CacheClusterStatus 'available', exactly one CacheNodes entry carrying Endpoint.Address and Endpoint.Port, and a LogDeliveryConfigurations entry with LogType 'slow-log', DestinationType 'cloudwatch-logs', DestinationDetails.CloudWatchLogsDetails.LogGroup equal to the requested group, LogFormat 'json' and Status 'active'; a RESP command executed against that endpoint appears in the destination log group as a JSON record carrying CacheClusterId, CacheNodeId, Id, Timestamp, 'Duration (us)', Command, ClientAddress and ClientName; ModifyCacheCluster with ApplyImmediately=true and that configuration disabled drives Status off 'active' and stops the stream; re-enabling returns Status to 'active' and delivery resumes.

Actual (emulator): none of the above is observed — the check is killed at the replay budget with no response graded.

The cluster id, log-group name, parameter-group name, engine version, VPC CIDR, probe key, and the record's Id/Timestamp/'Duration (us)' values, as well as the harness's own 60s budget number, are per-run values and are not part of the requirement. The requirement is that each of these control-plane status members reaches its terminal enum value promptly and that the delivered slow-log record exists in the configured group with the documented members.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1, check elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs.
2. CreateCacheParameterGroup for the redis family reported by DescribeCacheEngineVersions, then ModifyCacheParameterGroup with ParameterNameValues [{slowlog-log-slower-than, '0'}, {slowlog-max-len, '1000'}].
3. CreateLogGroup in CloudWatch Logs named /aws/elasticache/<name>.
4. CreateCacheCluster with Engine 'redis', CacheNodeType cache.t3.micro, NumCacheNodes 1, Port 6379, that parameter group, a cache subnet group, TransitEncryptionEnabled false, and LogDeliveryConfigurations [{LogType 'slow-log', DestinationType 'cloudwatch-logs', DestinationDetails.CloudWatchLogsDetails.LogGroup <that group>, LogFormat 'json', Enabled true}]. Confirm the response echoes CacheClusterId and reports CacheClusterStatus 'creating'.
5. Poll DescribeCacheClusters until CacheClusterStatus is 'available' and the slow-log LogDeliveryConfigurations entry reports Status 'active' with the requested LogGroup and LogFormat 'json'. Observe that neither wait reaches its terminal value within a normal client timeout.
6. DescribeCacheClusters with ShowCacheNodeInfo=true and read CacheNodes[0].Endpoint; run a RESP read (e.g. SET then GETRANGE) against it, then FilterLogEvents on the destination log group for a JSON slow-log record naming that command verb and CacheClusterId.
7. ModifyCacheCluster with ApplyImmediately=true and the slow-log configuration disabled; poll until its Status is no longer 'active' and confirm no new record arrives. Re-enable it and confirm Status returns to 'active' and a later command is delivered again.
8. Re-replay the check: it must reach and grade these assertions instead of being killed at the replay budget.
