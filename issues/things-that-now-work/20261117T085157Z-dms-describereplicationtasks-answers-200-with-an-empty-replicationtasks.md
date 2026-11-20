Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dms.s3-full-load-migration
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: dms.s3-full-load-migration|_task#66|IndexError|return dms.describe_replication_tasks(
Issue-Commit: 9c148a0c90ec643ae575e6ff5f800092be1af807

# DMS DescribeReplicationTasks answers 200 with an empty ReplicationTasks list where AWS refuses with 400 ResourceNotFoundFault when Filters name a task that does not exist

> _Note: this issue was identified against the code base at commit `9c148a0c90ec643ae575e6ff5f800092be1af807`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReplicationTasks

## Description
One net-new root cause in the DMS describe path, grounded in behavioral check dms.s3-full-load-migration. The check now gets all the way through the migration itself (the replication instance reaches 'available', the full-load task reaches 'ready', runs, stops with 'Stop Reason FULL_LOAD_ONLY_FINISHED', the LOAD00000001.csv object lands in the target bucket and DescribeTableStatistics reports the completed table — the behavior fixed issue 20261105T072850Z-dms-a-started-full-load-task-never-leaves covered) and now fails in its teardown, on the describe that is supposed to confirm the deleted task is gone. Related open issue: 20261109T161403Z-dms-replication-task-lifecycle-never-completes-task-settings describes the same describe-after-delete gap from the WIRE corpus as its Cause 4, but with the opposite symptom (there the task was still PRESENT in a 200 because its delete had been refused) and it carries only wire replay keys, not this behavioral check id, so it does not cover this check.

Cause 1 of 1: DescribeReplicationTasks whose Filters name a replication task the store does not hold answers HTTP 200 with an empty ReplicationTasks list instead of refusing the request. Failing check ids: dms.s3-full-load-migration.
AWS: Smithy com.amazonaws.databasemigrationservice#DescribeReplicationTasks models exactly one error, ResourceNotFoundFault (HTTP 400, documented 'The resource could not be found'), and its Filters member documents 'replication-task-arn | replication-task-id | migration-type | endpoint-arn | replication-instance-arn'. A describe whose filter value names a replication task that does not exist (never created, or deleted and reclaimed) is REFUSED with that error — it is not an empty successful page. The requirement is the status (400) and the error code (ResourceNotFoundFault); the message is DMS's own fixed 'resource could not be found' wording for a replication task, whose only variable part is the filter value taken from the request (the task ARN/identifier the caller passed) — recorded behavior only for the exact sentence, since this behavioral check asserts the Code and not the text, so do not read any one run's ARN as part of the template.
Emulator: services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/describe_replication_tasks.rs builds `matching` by filtering `store.ordered_tasks()` through `matches_filters` and then unconditionally returns `Ok(DescribeReplicationTasksOutput { replication_tasks: Some(...) })` — there is no not-found branch anywhere in the handler, so a filter that selects nothing yields HTTP 200 with `ReplicationTasks: []`. Replay output: after DeleteReplicationTask the check polls `describe_replication_tasks(Filters=[{'Name':'replication-task-arn','Values':[task_arn]}])['ReplicationTasks'][0]` and raises `IndexError: list index out of range` (check_dms_s3_full_load_migration.py line 83, reached from the teardown `_gone(...)` at line 406) — i.e. the emulator returned a 200 with zero tasks where the check's not-found handling expects a ClientError with Code ResourceNotFoundFault.
Why: the documented way to confirm a teardown finished is to poll the describe until it is refused with ResourceNotFoundFault. A client doing that against this emulator never sees a refusal; it sees a successful response whose list is empty, so code that indexes the first element crashes, code that catches ResourceNotFoundFault to decide 'gone' loops or misreports the resource as still present, and the same filtered describe can no longer distinguish 'this task does not exist' from 'this task exists but matched no other filter'.
Fix: the DescribeReplicationTasks handler's filter/not-found path — when the request's Filters name a specific replication task (replication-task-arn / replication-task-id) that the service state does not hold, return the modeled ResourceNotFoundFault (the handler already has the variant wired as DescribeReplicationTasksErrorModeled::ResourceNotFoundFault) instead of an empty successful page; a filter that merely narrows across existing tasks (migration-type, endpoint-arn, replication-instance-arn) keeps answering 200. The sibling gap on DescribeReplicationInstances is described by open issue 20261109T161403Z (its Cause 5) and is not part of this issue.
Traces: dms.s3-full-load-migration (teardown, after DeleteReplicationTask). Code anchors: services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/describe_replication_tasks.rs lines 169-192 (no not-found branch), lines 89-93 (the modeled ResourceNotFoundFault variant), lines 223-237 (matches_filters), line 73 (the modeled error, HTTP 400).

## Evidence
Behavioral suite simparity-behavioral-aws-v1: total 2/3 passed, dms 2/3 (67%), 1 failed. The emulator is serving — the failing check drove the entire DMS control plane and data plane successfully (subnet group, replication instance to 'available', both s3 endpoints, task to 'ready', start, stop with FULL_LOAD_ONLY_FINISHED, target object, table statistics) before failing in teardown — so this is a fidelity divergence, not a bootstrap gap.

[error] dms.s3-full-load-migration
  Error: File "check_dms_s3_full_load_migration.py", line 406, in <lambda>
    _gone(lambda: _task(dms, task_arn), budget_s=_cap(ctx, 180), what=f"replication task {task_id}")
  File "check_dms_s3_full_load_migration.py", line 83, in _task
    return dms.describe_replication_tasks(Filters=[{'Name': 'replication-task-arn', 'Values': [task_arn]}])['ReplicationTasks'][0]
  IndexError: list index out of range

Sequence: DeleteReplicationTask(ReplicationTaskArn=<task arn>) is accepted, then the check calls DescribeReplicationTasks filtered on that same ARN to wait for the task to be absent. Its not-found handling (_gone / _GONE_CODES) accepts either a describe helper returning None or a botocore ClientError whose Error.Code is 'ResourceNotFoundFault'.

Expected (AWS): HTTP 400 with error code ResourceNotFoundFault — DMS's fixed 'resource could not be found' text for a replication task, with the filter value from the request as its only variable part.
Actual (emulator): HTTP 200 with body {"ReplicationTasks": []} (no error, no Marker), so the client's first-element access raises IndexError and no not-found code is ever observed.

Model consulted: smithy_query query=operation operation=DescribeReplicationTasks on services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/smithy-model/com-amazonaws-databasemigrationservice-amazondmsv20160101.json → errors: [ResourceNotFoundFault, errorKind client, httpError 400]; output members Marker + ReplicationTasks (ReplicationTaskList); input Filters/MaxRecords/Marker/WithoutSettings.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay behavioral suite simparity-behavioral-aws-v1, check dms.s3-full-load-migration: it fails with IndexError in its teardown wait.
3. Shorter repro against DMS alone (no setup needed for the core divergence):
   a. Call DescribeReplicationTasks(Filters=[{'Name':'replication-task-arn','Values':['arn:aws:dms:us-east-1:123456789012:task:DOESNOTEXIST']}]). The emulator answers HTTP 200 with ReplicationTasks: []; AWS refuses with HTTP 400 ResourceNotFoundFault.
   b. Same with Filters=[{'Name':'replication-task-id','Values':['no-such-task']}] — identical empty 200.
4. Full lifecycle repro: create a replication subnet group over two subnets in two AZs, CreateReplicationInstance and wait for 'available', CreateEndpoint twice (source/target, EngineName 's3'), CreateReplicationTask with MigrationType='full-load', wait for Status 'ready', StartReplicationTask, wait for Status 'stopped', then DeleteReplicationTask and poll DescribeReplicationTasks filtered on that task ARN: every poll returns HTTP 200 with an empty ReplicationTasks list, so a client waiting for ResourceNotFoundFault (or indexing ReplicationTasks[0]) never completes teardown.
