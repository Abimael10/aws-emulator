Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs
Issue-Key: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs|_resp_int#1|AssertionError|assert raw.startswith(b":") and raw.endswith(b"\r\n"), (
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# elasticache: the served cache engine answers SLOWLOG with "unknown command", so slow-log recording and its delivery to CloudWatch Logs can never be observed

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
One root cause (Cause 1 of 1), grounded in the one failing elasticache__logs check the replay report binds to no open issue.

Cause 1 of 1: the cache engine the emulator serves on a cache cluster's endpoint does not implement the SLOWLOG command family, so the engine never records a slow-log entry and the slow-log LogDeliveryConfiguration the control plane accepted has nothing to deliver.

AWS: recorded behavior only for the RESP wire (no Smithy shape governs engine commands; the Smithy surface involved is CreateCacheCluster/CreateReplicationGroup's LogDeliveryConfigurations with LogType 'slow-log', DestinationType 'cloudwatch-logs', LogFormat 'json', and ModifyCacheParameterGroup setting the parameters slowlog-log-slower-than and slowlog-max-len). On a Redis OSS / Valkey ElastiCache node the SLOWLOG subcommands are available (unlike CONFIG, which ElastiCache restricts): `SLOWLOG LEN` answers a RESP integer reply `:<n>\r\n`, `SLOWLOG GET` answers an array of entries, and with slowlog-log-slower-than=0 every executed command — including a sub-millisecond ECHO — is recorded, so SLOWLOG LEN rises above 0. Those recorded entries are what the slow-log delivery configuration then publishes into the configured CloudWatch Logs group.
Emulator: the cluster's endpoint accepts the RESP session and answers other commands, but `SLOWLOG LEN` is refused with the Redis-shaped unknown-command error `-ERR unknown command 'slowlog', with args beginning with: \r\n`. The fixed text of that refusal is Redis's own template for a verb the server does not implement; the variable part is the verb echoed from the request. The parameter writes (slowlog-log-slower-than=0, slowlog-max-len=1000) were accepted by the control plane and the cluster reports an ACTIVE slow-log delivery configuration, yet the engine exposes no slow-log at all.
Why: a client that configures slow-log delivery gets 200s from every control-plane call and an 'active' delivery status, then finds that the engine denies the only command by which the slow log can be read or proven to be recording. Every diagnosis path built on SLOWLOG (ElastiCache's documented way to find slow commands) fails with 'unknown command', and a broken delivery pipeline is indistinguishable from an engine that records nothing.
Fix: the emulator's ElastiCache data plane — the RESP command dispatcher that backs the cache endpoint — must implement the SLOWLOG family (LEN, GET, RESET, HELP) and record entries according to the cluster's effective slowlog-log-slower-than / slowlog-max-len parameter values; the slow-log LogDeliveryConfiguration path in the elasticache state/log-delivery component is what then publishes those entries to the configured CloudWatch Logs group.
Related open issues, neither of which carries this failure signature (they report earlier blockers of the same check: the in-VPC Lambda probe's runtime and networking): 20261111T005655Z-elasticache-logs-the-slow-log-to-cloudwatch-logs.md and 20261105T090541Z-elasticache-logs-the-slow-log-delivery-check-cannot.md. The sibling check elasticache__logs.slow-log-delivery-to-missing-log-group-rejected is owned by 20261108T145640Z-elasticache-logs-elasticache-a-slow-log-logdeliveryconfiguration-naming-a-cloudwatch.md (a different cause). The probe now reaches the engine from outside the VPC, so this command-surface gap is what the check hits next.
Traces: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs (_await_engine_recording -> _resp_int, check lines 1162/1017).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: elasticache__logs 0/2 (1 of the 2 failures owned by an existing issue).

elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs [mismatch]
check_elasticache__logs_slow_log_delivery_lands_the_engine_command_in_cloudwatch_logs.py:1162 in _await_engine_recording -> `last = _resp_int(replies[1], source=prober.label, cmd="SLOWLOG LEN")` -> :1017
AssertionError: [runner-outside-vpc(tls_mode=None)] SLOWLOG LEN must return a RESP integer reply (:<n>\r\n), got b"-ERR unknown command 'slowlog', with args beginning with: \r\n"

Expected (AWS): the pipeline [["ECHO","sim2real-slowlog-probe"],["SLOWLOG","LEN"]] answers a bulk string for ECHO and a RESP integer `:<n>\r\n` for SLOWLOG LEN, with n >= 1 once slowlog-log-slower-than=0 has taken effect; the subsequent GETRANGE is then found in the cluster's CloudWatch Logs slow-log stream.
Actual (emulator): SLOWLOG is an unknown command (`-ERR unknown command 'slowlog', ...`), so no slow-log entry exists, nothing can be delivered, and the check dies before any log read.
The prober label, cluster id, log-group name, key names and parameter-group name are per-run values and are not part of the requirement; the availability of SLOWLOG and the RESP integer reply shape are.

## Steps to Reproduce
1. Boot the emulator from this checkout and run suite simparity-behavioral-aws-v1.
2. CreateCacheParameterGroup (redis/valkey family) and ModifyCacheParameterGroup with ParameterNameValues [{slowlog-log-slower-than, "0"}, {slowlog-max-len, "1000"}].
3. CreateLogGroup in CloudWatch Logs; CreateCacheCluster (cache.t3.micro, that parameter group) with LogDeliveryConfigurations [{LogType: 'slow-log', DestinationType: 'cloudwatch-logs', DestinationDetails.CloudWatchLogsDetails.LogGroup: <that group>, LogFormat: 'json'}]; wait for 'available' and the delivery configuration's Status 'active'.
4. Open a RESP session to the cluster endpoint and send `ECHO probe` then `SLOWLOG LEN`.
   Expect (AWS): `:<n>\r\n` with n >= 1.
   Observe: `-ERR unknown command 'slowlog', with args beginning with: \r\n`.
5. Run a marker command (e.g. GETRANGE) and FilterLogEvents on the log group; expect a slow-log record naming it.
6. Re-replay elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs; it must reach its assertions about the log group's contents rather than failing on SLOWLOG.
