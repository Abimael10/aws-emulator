Issue-Source: trace-sweep-divergence
Issue-Trace: datasync-s3-to-s3-task-execution-copies-object
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: datasync-s3-to-s3-task-execution-copies-object datasync-task-lifecycle-and-tags
Issue-Shape-Id: com.amazonaws.datasync#FmrsService
Issue-Key: datasync|DescribeTaskExecution|2xx->2xx|$.EndTime
Issue-Key: datasync|DescribeTaskExecution|2xx->2xx|$.LaunchTime
Issue-Key: datasync|DescribeTaskExecution|2xx->2xx|$.StartTime
Issue-Key: datasync|DescribeTask|2xx->2xx|$.Excludes[*]
Issue-Key: datasync|DescribeTask|2xx->2xx|$.Includes[*]
Issue-Related: 20261029T180920Z-datasync-task-reads-never-settle-to-available-omit.md
Issue-Commit: 960085fede7475f864d055ec106450d0cb96a6ce

# DataSync task read-back diverges: UpdateTask edits dropped and task-execution timestamps mis-shaped (2 causes)

> _Note: this issue was identified against the code base at commit `960085fede7475f864d055ec106450d0cb96a6ce`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTaskExecution

## Description
TWO root causes on the DataSync read-back surface. Both answer HTTP 200 on both sides (expected_status 200 / actual_status 200) — the divergence is in the response body a client reads after it has changed or run a task. Model consulted: services/service-com-amazonaws-datasync-fmrsservice/smithy-model/com-amazonaws-datasync-fmrsservice.json (DescribeTaskExecution output StartTime/LaunchTime/EndTime : Time (timestamp); DescribeTask output Name/Options/Includes/Excludes/SourceNetworkInterfaceArns/DestinationNetworkInterfaceArns; UpdateTask input TaskArn (required), Name, Options, Includes, Excludes, Schedule, ManifestConfig, TaskReportConfig, output empty, errors InternalException(500)/InvalidRequestException(400)).

=== Cause 1 of 2: DescribeTaskExecution's StartTime, LaunchTime and EndTime are not the timestamps DataSync reports for an execution ===
AWS: DescribeTaskExecution reports StartTime ("the time that DataSync sends the request to start the task execution"), LaunchTime ("the time that the task execution actually begins") and EndTime ("the time that the transfer task ends"), all of the modeled `Time` timestamp shape, serialized by awsJson1_1 as epoch seconds. The model's own member documentation states the relation between two of them: "For non-queued tasks, LaunchTime and StartTime are typically the same. For queued tasks, LaunchTime is typically later than StartTime because previously queued tasks must finish running before newer tasks can begin." The recorded response carries all three members, and the fixture asserts them BY SHAPE rather than by literal value (expected '<shaped-not-asserted>'), precisely so that a per-run wall-clock value is not a requirement — so this divergence is not a clock-value difference: the values the emulator emits fail the recorded shape of these members.
Emulator: all three members are reported as the raw host clock in f64 epoch seconds, carrying sub-millisecond (microsecond) fractional digits — the replay reports $.StartTime, $.LaunchTime and $.EndTime each as value_mismatch against the recorded shape, with values of the form 1795506132.7008011 / 1795506133.000801 / 1795506133.9008012 (seven fractional digits). They are derived from one SystemTime::now() through a fixed synthetic phase schedule: LaunchTime = StartTime + one transition span, EndTime = StartTime + four transition spans, so for an execution that was never queued LaunchTime is reported strictly later than StartTime rather than the same instant the model documents. Hypothesis: the shape assertion fails on both counts — the sub-millisecond precision (DataSync reports these epoch-second timestamps at millisecond granularity, which is what the recording shows) and the unconditional LaunchTime != StartTime offset for a non-queued execution.
Why: a client that reports or charts an execution's timeline, or computes a transfer duration from StartTime/EndTime, reads timestamps whose format and whose inter-member relation differ from the service's: code that formats an epoch-seconds timestamp assuming at most millisecond precision mis-renders or truncates, and logic that treats LaunchTime > StartTime as "this execution spent time QUEUED" wrongly concludes every execution was queued.
Fix: the DescribeTaskExecution handler's timestamp projection and the shared timestamp conversion it uses — services/service-com-amazonaws-datasync-fmrsservice/src/ops/describe_task_execution.rs lines 324/355-356 (start_time/launch_time/end_time via crate::state::as_timestamp) and services/service-com-amazonaws-datasync-fmrsservice/src/state.rs lines 1069-1075 (as_timestamp -> DateTime::from_secs_f64 of the full host clock) plus the phase schedule at state.rs lines 608-618 (ExecutionSchedule::starting_at, LaunchTime = start + TRANSITION_SPAN), so that an execution that was never queued launches at the instant it started and every reported Time member carries the granularity DataSync reports.
Traces: datasync-s3-to-s3-task-execution-copies-object#task#11.

=== Cause 2 of 2: UpdateTask accepts nothing — the task reads back with its pre-update Name, filters and Options ===
AWS: UpdateTask mutates the stored task (input members Name, Options, Includes, Excludes, Schedule, ManifestConfig, TaskReportConfig over a required TaskArn; empty output), and the next DescribeTask reports the NEW values: the new Name, the new Includes/Excludes filter rules, and each Options member the update supplied. In the recording the task is renamed, given Includes=[{FilterType: SIMPLE_PATTERN, Value: ...}] and Excludes=[{FilterType: SIMPLE_PATTERN, Value: ...}], and Options with Atime/Mtime/Uid/Gid/PosixPermissions/ObjectTags/VerifyMode/SecurityDescriptorCopyFlags set to NONE, OverwriteMode NEVER, TaskQueueing DISABLED and a finite BytesPerSecond cap — and DescribeTask echoes exactly what was set.
Emulator: UpdateTask is still an unimplemented scaffold (services/service-com-amazonaws-datasync-fmrsservice/src/ops/update_task.rs carries the SCAFFOLDED marker and returns Scaffolded::not_implemented), so no edit is ever stored, and the DescribeTask that follows it returns the task exactly as created: $.Name is the pre-update name (expected the renamed value), $.Includes[0] and $.Excludes[0] are missing entirely (expected the supplied SIMPLE_PATTERN rules), and every Options member is back at its modeled default — $.Options.Atime 'BEST_EFFORT' (expected 'NONE'), $.Options.Mtime 'PRESERVE' ('NONE'), $.Options.Uid/$.Options.Gid 'INT_VALUE' ('NONE'), $.Options.PosixPermissions 'PRESERVE' ('NONE'), $.Options.ObjectTags 'PRESERVE' ('NONE'), $.Options.VerifyMode 'POINT_IN_TIME_CONSISTENT' ('NONE'), $.Options.OverwriteMode 'ALWAYS' ('NEVER'), $.Options.TaskQueueing 'ENABLED' ('DISABLED'), $.Options.BytesPerSecond -1 (expected the requested cap), $.Options.SecurityDescriptorCopyFlags 'OWNER_DACL' ('NONE'). The same cluster also shows $.SourceNetworkInterfaceArns and $.DestinationNetworkInterfaceArns missing where the recording has them present as empty arrays, and $.Status 'CREATING' where the recording has 'AVAILABLE'.
Why: a client reconfigures a task and then reads it back to confirm — the documented pattern for changing filters, disabling task queueing or applying a bandwidth cap. Against the emulator the reconfiguration is invisible: the task keeps reporting (and, for the emulated transfer, keeps using) the configuration it was created with, so a test or provisioner that asserts the new configuration fails while one that merely checks the call succeeded is silently wrong.
Fix: the UpdateTask handler and the task state it writes (services/service-com-amazonaws-datasync-fmrsservice/src/ops/update_task.rs, over crate::state::Task at state.rs lines 431-458) — apply Name, Includes, Excludes, Schedule, ManifestConfig, TaskReportConfig and every supplied Options member (overlaying them on the stored options the way crate::state::overlay_options already does for CreateTask) so DescribeTask/ListTasks report them, refusing an unknown TaskArn with the service's existing InvalidRequestException wording (state::task_not_found: message "Task <task-id> is not found.", errorCode and datasyncErrorCode "SyncSetNotFound"). Names, ARNs and the byte cap quoted above are the recording's request values filling the template, not requirements.
Traces: datasync-task-lifecycle-and-tags#task#9.

Related open issues, which the replay does not record as owning either cluster above: 20261029T180920Z-datasync-task-reads-never-settle-to-available-omit.md covers the never-settling Status and the omitted SourceNetworkInterfaceArns/DestinationNetworkInterfaceArns lists that travel in Cause 2's cluster (and describes UpdateTask persistence in its own Cause 4); the UpdateTask 501 stub itself is tracked by the scaffold's own not-implemented issue. Cause 1 is covered nowhere.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up=true, score_pct=87.4, steps_total=143, steps_passed=125, steps_mismatched=18). Two verdict=mismatch clusters, neither carrying stub_path or owned_by:

1) datasync-s3-to-s3-task-execution-copies-object#task#11, DescribeTaskExecution, expected 200 / actual 200 — Cause 1:
   $.EndTime: value_mismatch (expected '<shaped-not-asserted>', got 1795506133.9008012)
   $.LaunchTime: value_mismatch (expected '<shaped-not-asserted>', got 1795506133.000801)
   $.StartTime: value_mismatch (expected '<shaped-not-asserted>', got 1795506132.7008011)
   divergence_key: service=datasync|DescribeTaskExecution|status=200->200|keys=$.EndTime,$.LaunchTime,$.StartTime
   (LaunchTime - StartTime = 0.300 s and EndTime - StartTime = 1.200 s exactly — the synthetic one-TRANSITION_SPAN-per-phase schedule, not instants the execution observed; the recording asserts these members by shape, so no per-run clock value is being required here.)

2) datasync-task-lifecycle-and-tags#task#9, DescribeTask after UpdateTask, expected 200 / actual 200 — Cause 2:
   $.Name: expected the updated name ('...-v2'), got the pre-update name
   $.Includes[0]: missing (expected {'FilterType': 'SIMPLE_PATTERN', 'Value': '/data'})
   $.Excludes[0]: missing (expected {'FilterType': 'SIMPLE_PATTERN', 'Value': '*.tmp|/scratch'})
   $.Options.Atime: expected 'NONE', got 'BEST_EFFORT'; $.Options.Mtime: expected 'NONE', got 'PRESERVE'
   $.Options.Uid / $.Options.Gid: expected 'NONE', got 'INT_VALUE'
   $.Options.PosixPermissions: expected 'NONE', got 'PRESERVE'
   $.Options.ObjectTags: expected 'NONE', got 'PRESERVE'
   $.Options.VerifyMode: expected 'NONE', got 'POINT_IN_TIME_CONSISTENT'
   $.Options.OverwriteMode: expected 'NEVER', got 'ALWAYS'; $.Options.TaskQueueing: expected 'DISABLED', got 'ENABLED'
   $.Options.BytesPerSecond: expected the requested cap (2097152), got -1 (no limit)
   $.Options.SecurityDescriptorCopyFlags: expected 'NONE', got 'OWNER_DACL'
   $.SourceNetworkInterfaceArns / $.DestinationNetworkInterfaceArns: missing (expected [])
   divergence_key: service=datasync|DescribeTask|status=200->200|keys=$.DestinationNetworkInterfaceArns,$.Excludes[0],$.Includes[0],$.Name,$.Options.*,$.SourceNetworkInterfaceArns

Code read from this checkout: src/ops/update_task.rs:188 returns Scaffolded::not_implemented("UpdateTask") (file still carries the '//! SCAFFOLDED, NOT IMPLEMENTED' marker); src/state.rs:1070-1075 as_timestamp = DateTime::from_secs_f64(now since epoch); src/state.rs:610-617 ExecutionSchedule::starting_at offsets launched_at/finished_at by 1 and 4 x TRANSITION_SPAN (300 ms, state.rs:64); src/ops/describe_task_execution.rs:324,355-356 project those instants into StartTime/LaunchTime/EndTime. Model members and docs quoted above come from smithy_query on services/service-com-amazonaws-datasync-fmrsservice/smithy-model/com-amazonaws-datasync-fmrsservice.json.

## Steps to Reproduce
Boot the emulator from this checkout and replay the simparity-wire-aws-v1 datasync fixtures.

Cause 1: create two S3 locations, CreateTask, StartTaskExecution, wait for the execution to reach SUCCESS, then DescribeTaskExecution. Observe StartTime/LaunchTime/EndTime reported as epoch seconds with microsecond fractional digits, and LaunchTime a fixed 300 ms after StartTime even though the execution was never queued. Replay: datasync-s3-to-s3-task-execution-copies-object#task#11

Cause 2: create two S3 locations and a task, call UpdateTask on it with a new Name, Includes=[{SIMPLE_PATTERN,'/data'}], Excludes=[{SIMPLE_PATTERN,'*.tmp|/scratch'}] and Options{Atime:NONE, Mtime:NONE, Uid:NONE, Gid:NONE, PosixPermissions:NONE, ObjectTags:NONE, VerifyMode:NONE, SecurityDescriptorCopyFlags:NONE, OverwriteMode:NEVER, TaskQueueing:DISABLED, BytesPerSecond:<cap>}, then DescribeTask the same TaskArn. Observe the pre-update Name, no Includes/Excludes at all and every Options member back at its modeled default. Replay: datasync-task-lifecycle-and-tags#task#9
