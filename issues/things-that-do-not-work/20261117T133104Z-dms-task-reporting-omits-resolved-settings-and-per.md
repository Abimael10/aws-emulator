Issue-Source: trace-sweep-divergence
Issue-Trace: dms-s3-full-load-task-migrates-csv-rows
Issue-Step: task#22
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dms-s3-full-load-task-migrates-csv-rows
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: dms|DescribeReplicationTasks|2xx->2xx|$.ReplicationTasks[*].ReplicationTaskStats.FreshStartDate
Issue-Key: dms|DescribeReplicationTasks|2xx->2xx|$.ReplicationTasks[*].ReplicationTaskStats.FullLoadFinishDate
Issue-Key: dms|DescribeReplicationTasks|2xx->2xx|$.ReplicationTasks[*].ReplicationTaskStats.FullLoadStartDate
Issue-Key: dms|DescribeReplicationTasks|2xx->2xx|$.ReplicationTasks[*].ReplicationTaskStats.StartDate
Issue-Key: dms|DescribeReplicationTasks|2xx->2xx|$.ReplicationTasks[*].ReplicationTaskStats.StopDate
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].AppliedDdls
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].AppliedDeletes
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].AppliedInserts
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].AppliedUpdates
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].FullLoadEndTime
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].FullLoadStartTime
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].LastUpdateTime
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].ResyncProgress
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].ResyncRowsAttempted
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].ResyncRowsFailed
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].ResyncRowsSucceeded
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].ResyncState
Issue-Key: dms|DescribeTableStatistics|2xx->2xx|$.TableStatistics[*].ValidationState
Issue-Related: 20261109T161403Z-dms-replication-task-lifecycle-never-completes-task-settings.md
Issue-Commit: 36e91b0a54420c8643974af3d3528aa1f799565b

# DMS task reporting omits resolved settings and per-table load counters

> _Note: this issue was identified against the code base at commit `36e91b0a54420c8643974af3d3528aa1f799565b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReplicationTasks

## Description
Two net-new wire divergences from fixture dms-s3-full-load-task-migrates-csv-rows, service 'dms' (emulator_up=true, bootstrap_signal=false, so these are fidelity divergences, not a bootstrap gap). Neither cluster carries stub_path or owned_by. They share one fault: the records a replication task's reporting operations project omit documented response members, so a task that HAS run reports less about itself than AWS does. Three other clusters of this fixture (task#21 StartReplicationTask, teardown#27 DescribeReplicationTasks, teardown#31 DescribeReplicationInstances) are OWNED by open issue 20261109T161403Z-dms-replication-task-lifecycle-never-completes-task-settings.md and are not part of this issue; that issue's replay pointer does NOT carry the two steps below, so they are filed here with their own fixture steps.

Cause 1 of 2: DescribeReplicationTasks reports a task with no ReplicationTaskSettings member, because a task created without (or with a partial) settings document is never resolved against DMS's default task settings. Fixture dms-s3-full-load-task-migrates-csv-rows step task#22 (DescribeReplicationTasks).
AWS: Smithy ReplicationTask$ReplicationTaskSettings (String) — 'The settings for the replication task'; CreateReplicationTask$ReplicationTaskSettings is OPTIONAL, and DescribeReplicationTasks drops the member only when the request carries WithoutSettings=true. DMS resolves an omitted or partial settings document against its service defaults at create time and thereafter reports the fully populated JSON on every ReplicationTask it returns. The recording shows that document's shape: a JSON object whose top-level keys include Logging (EnableLogging/EnableLogContext false, CloudWatchLogGroup/CloudWatchLogStream null, and one LogComponents entry per DMS log component — DATA_STRUCTURE, COMMUNICATION, IO, COMMON, FILE_FACTORY, FILE_TRANSFER, REST_SERVER, ADDONS, TARGET_LOAD, TARGET_APPLY, SOURCE_UNLOAD, SOURCE_CAPTURE, TRANSFORMATION, SORTER, TASK_MANAGER, TABLES_MANAGER, METADATA_MANAGER, PERFORMANCE, VALIDATOR_EXT — each at Severity LOGGER_SEVERITY_DEFAULT), StreamBufferSettings, ErrorBehavior and the remaining default sections. The individual numbers in one recording are the service defaults, not per-run values.
Emulator: the settings are stored as exactly what the caller passed (src/state.rs line 908, `replication_task_settings: Option<String>`) and projected straight back out of the single ReplicationTaskRecord::to_shape (src/state.rs lines 950-954), so when the caller omits them the member is absent from every response. Replay reports '$.ReplicationTasks[0].ReplicationTaskSettings: missing'.
Why: a client that reads back a task's effective settings — the documented way to discover what logging, error behavior and stream buffering the task will run with, and the input to a ModifyReplicationTask round-trip — gets no member at all, so a JSON parse of ReplicationTaskSettings fails or silently yields nothing and a settings round-trip wipes the defaults.
Fix: the CreateReplicationTask handler's record construction — resolve the caller's omitted/partial ReplicationTaskSettings against a default task-settings document held in the service's state module and store the resolved document — so the one to_shape projection reports it on every operation that returns a ReplicationTask.
Secondary observation on the same step: the harness also flags $.ReplicationTasks[0].ReplicationTaskCreationDate, ReplicationTaskStartDate and the ReplicationTaskStats date members (FreshStartDate, FullLoadStartDate, FullLoadFinishDate, StartDate, StopDate) as value_mismatch with expected '<shaped-not-asserted>' against the bare epoch floats the emulator emits from `aws_smithy_types::DateTime::from(SystemTime)`. The recording redacts those values, so this is recorded behavior only — Hypothesis: the emulator's TStamp serialization for these members does not match the serialization the recording carries. A per-run clock value is not itself a requirement; the absent settings document is the filable part of this cluster.
Traces: dms-s3-full-load-task-migrates-csv-rows#task#22. Code anchors: services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/state.rs lines 907-908 and 942-979; src/ops/create_replication_task.rs (the task-record construction).

Cause 2 of 2: DescribeTableStatistics reports each table of a finished full load with the applied-change counters, the resync accounting and the validation/resync state members ALL absent, where AWS reports each of them. Fixture step task#23 (DescribeTableStatistics).
AWS: Smithy TableStatistics models AppliedInserts / AppliedDeletes / AppliedUpdates / AppliedDdls (LongOptional), ResyncRowsAttempted / ResyncRowsSucceeded / ResyncRowsFailed (LongOptional), ResyncProgress (DoubleOptional), ResyncState and ValidationState (String). ResyncState's doc publishes the value list beginning 'Not enabled – Resync is not enabled for the table in the migration task'; ValidationState's publishes 'Not enabled – Validation isn't enabled for the ...' for a task with validation off. For a full-load-only task that applied no change-data-capture rows and has neither validation nor resync enabled, DMS reports the counters as 0 (ResyncProgress 0.0) and both state members as the fixed text 'Not enabled' — the member set is reported, not omitted. The recorded entry carries exactly that: AppliedDdls/AppliedDeletes/AppliedInserts/AppliedUpdates 0, ResyncRowsAttempted/Failed/Succeeded 0, ResyncProgress 0.0, ResyncState 'Not enabled', ValidationState 'Not enabled'.
Emulator: every one of those thirteen members is reported as absent — replay reports 'missing (expected 0, got None)' for the four Applied* counters and the three Resync* counters, 'missing (expected 0.0, got None)' for ResyncProgress, and 'missing (expected \'Not enabled\', got None)' for ResyncState and ValidationState. The DescribeTableStatistics handler reports whatever the task run's per-table accounting builds (src/ops/describe_table_statistics.rs lines 198-218, `task.table_statistics()`), and that projection populates only the load-side members.
Why: a client reading per-table migration accounting — the documented way to tell whether a table was validated or resynced and how many changes were applied to the target — cannot distinguish 'zero applied changes / not enabled' from 'not reported', so any dashboard or completeness check over TableStatistics either crashes on the absent member or reports the table as unaccounted.
Fix: the task-run per-table statistics projection the DescribeTableStatistics handler reads (`ReplicationTaskRecord::table_statistics`, in the service's state/task-run module): report the applied-change counters and the resync counters as 0 for a run that applied none, ResyncProgress as 0.0, and ResyncState / ValidationState as 'Not enabled' when the task enables neither — rather than leaving each member None.
Secondary observation: the same step flags FullLoadStartTime, FullLoadEndTime and LastUpdateTime as value_mismatch against '<shaped-not-asserted>' with the emulator's bare epoch floats; as in Cause 1 that is a timestamp-serialization hypothesis over redacted per-run values, not the filable part.
Traces: dms-s3-full-load-task-migrates-csv-rows#task#23. Code anchors: src/ops/describe_table_statistics.rs lines 198-218; the ReplicationTaskRecord run/statistics projection in src/state.rs (ReplicationTaskRecord, lines 927-979).

## Evidence
Replay of trace set simparity-wire-aws-v1: 14 fixtures replayed, 180 steps (75 passed, 102 mismatched, 0 errored, 3 blocked), score 41.7%, divergence_count 8. emulator_up=true, bootstrap_signal=false, emulator_detail: '/__admin/health answered 200 and the :37587 data listener accepts'. Both clusters are service 'dms', fixture dms-s3-full-load-task-migrates-csv-rows, stub_path empty and owned_by empty.

[mismatch] task#22 DescribeReplicationTasks — expected 200, got 200, body diverges:
  $.ReplicationTasks[0].ReplicationTaskSettings: missing. Expected: the resolved DMS default task-settings JSON document — {"Logging":{"EnableLogging":false,"EnableLogContext":false,"LogComponents":[{"Severity":"LOGGER_SEVERITY_DEFAULT","Id":"DATA_STRUCTURE"}, ... one entry per DMS log component ... ],"CloudWatchLogGroup":null,"CloudWatchLogStream":null},"StreamBufferSettings":{"StreamBufferCount":3,"CtrlStreamBufferSizeInMB":5,"StreamBufferSizeInMB":8},"ErrorBehavior":{...}, ...}. Actual: member absent.
  $.ReplicationTasks[0].ReplicationTaskCreationDate: value_mismatch (expected '<shaped-not-asserted>', got a bare epoch float); ReplicationTaskStartDate and ReplicationTaskStats.{FreshStartDate,FullLoadStartDate,FullLoadFinishDate,StartDate,StopDate} likewise flagged.
  divergence_key: service=dms|DescribeReplicationTasks|status=200->200|keys=$.ReplicationTasks[0].ReplicationTaskCreationDate,$.ReplicationTasks[0].ReplicationTaskSettings,$.ReplicationTasks[0].ReplicationTaskStartDate,$.ReplicationTasks[0].ReplicationTaskStats.FreshStartDate,$.ReplicationTasks[0].ReplicationTaskStats.FullLoadFinishDate,$.ReplicationTasks[0].ReplicationTaskStats.FullLoadStartDate,$.ReplicationTasks[0].ReplicationTaskStats.StartDate,$.ReplicationTasks[0].ReplicationTaskStats.StopDate

[mismatch] task#23 DescribeTableStatistics — expected 200, got 200, body diverges on TableStatistics[0]:
  AppliedDdls: missing (expected 0); AppliedDeletes: missing (expected 0); AppliedInserts: missing (expected 0); AppliedUpdates: missing (expected 0).
  ResyncRowsAttempted: missing (expected 0); ResyncRowsFailed: missing (expected 0); ResyncRowsSucceeded: missing (expected 0); ResyncProgress: missing (expected 0.0).
  ResyncState: missing (expected 'Not enabled'); ValidationState: missing (expected 'Not enabled').
  FullLoadStartTime / FullLoadEndTime / LastUpdateTime: value_mismatch (expected '<shaped-not-asserted>', got bare epoch floats).
  divergence_key: service=dms|DescribeTableStatistics|status=200->200|keys=$.TableStatistics[0].AppliedDdls,$.TableStatistics[0].AppliedDeletes,$.TableStatistics[0].AppliedInserts,$.TableStatistics[0].AppliedUpdates,$.TableStatistics[0].FullLoadEndTime,$.TableStatistics[0].FullLoadStartTime,$.TableStatistics[0].LastUpdateTime,$.TableStatistics[0].ResyncProgress,$.TableStatistics[0].ResyncRowsAttempted,$.TableStatistics[0].ResyncRowsFailed,$.TableStatistics[0].ResyncRowsSucceeded,$.TableStatistics[0].ResyncState,$.TableStatistics[0].ValidationState

Model consulted: smithy_query query=operation operation=DescribeTableStatistics and query=shape shape=TableStatistics on services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/smithy-model/com-amazonaws-databasemigrationservice-amazondmsv20160101.json — TableStatistics models AppliedInserts/AppliedDeletes/AppliedUpdates/AppliedDdls and ResyncRowsAttempted/Succeeded/Failed as LongOptional, ResyncProgress as DoubleOptional, ResyncState and ValidationState as String whose docs publish the 'Not enabled' value; ReplicationTask$ReplicationTaskSettings is the settings document DescribeReplicationTasks reports unless WithoutSettings=true.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay wire fixture dms-s3-full-load-task-migrates-csv-rows from trace set simparity-wire-aws-v1; steps task#22 and task#23 diverge.
3. Shorter repro against DMS alone:
   a. CreateReplicationSubnetGroup over two subnets in two AZs, CreateReplicationInstance and wait for ReplicationInstanceStatus 'available'.
   b. CreateEndpoint twice (a source and an s3 target).
   c. CreateReplicationTask with MigrationType='full-load', one selection rule and NO ReplicationTaskSettings; wait for Status 'ready'.
   d. StartReplicationTask(StartReplicationTaskType='start-replication'), then DescribeReplicationTasks for that task: the reported ReplicationTask carries no ReplicationTaskSettings member, where AWS reports the resolved default task-settings document (Cause 1).
   e. DescribeTableStatistics(ReplicationTaskArn=<task arn>) once the full load has run: each TableStatistics entry omits AppliedInserts/AppliedDeletes/AppliedUpdates/AppliedDdls, ResyncRowsAttempted/Succeeded/Failed, ResyncProgress, ResyncState and ValidationState, where AWS reports the counters as 0 (ResyncProgress 0.0) and both state members as 'Not enabled' (Cause 2).
