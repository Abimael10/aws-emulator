Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-maintenance-window-lifecycle
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-maintenance-window-lifecycle ssm-get-ops-summary-filters-and-errors ssm-ops-summary-aggregation ssm-ops-summary-by-status-and-missing-sync ssm-resource-data-sync-not-found-errors ssm-state-manager-association-workflow
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|4xx->2xx|$.Message
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|4xx->2xx|$.__type
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|4xx->2xx|status
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|2xx->4xx|$.WindowId
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|2xx->4xx|$.WindowTargetId
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|2xx->4xx|status
Issue-Key: ssm|DescribeMaintenanceWindows|2xx->2xx|$.WindowIdentities[Name="sim2real-probe-ssm-mw-276e2a18"].Duration
Issue-Key: ssm|DescribeMaintenanceWindows|2xx->2xx|$.WindowIdentities[Name="sim2real-probe-ssm-mw-276e2a18"].NextExecutionTime
Issue-Key: ssm|GetOpsSummary|4xx->4xx|$.Message
Issue-Key: ssm|GetOpsSummary|2xx->2xx|$.Entities[*]
Issue-Key: ssm|ListNodes|4xx->4xx|$.SyncName
Issue-Key: ssm|ListNodes|4xx->4xx|$.__type
Issue-Key: ssm|CreateAssociationBatch|2xx->2xx|$.Successful[*].DocumentVersion
Issue-Commit: f8c3fa03cff68de185dd43b864dedf3dabbebd07

# ssm Ops Center, maintenance window and association reads diverge (7 causes)

> _Note: this issue was identified against the code base at commit `f8c3fa03cff68de185dd43b864dedf3dabbebd07`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterTargetFromMaintenanceWindow

## Description
Seven net-new causes across SSM maintenance windows, Ops Center (GetOpsSummary), resource-data-sync node reads and State Manager batch association creation. Each is an independent defect and all seven must be closed before this issue is resubmitted.

=== Cause 1 of 7: DeregisterTargetFromMaintenanceWindow ignores the safe-deregistration precondition ===
AWS: Smithy: DeregisterTargetFromMaintenanceWindow takes WindowId (required, @pattern ^mw-[0-9a-f]{17}$), WindowTargetId (required, UUID @pattern) and Safe (boolean, no default in the model — the service treats an absent Safe as a safe deregistration); modeled errors DoesNotExistException and TargetInUseException (httpError 400). Recorded behavior: when the named target is still referenced by a task registered in that window, the call is REFUSED with HTTP 400, __type TargetInUseException and Message "This Target cannot be deregistered because it is still referenced in Task: <windowTaskId>". Fixed text: "This Target cannot be deregistered because it is still referenced in Task: "; the trailing value is the referencing maintenance-window task id, read from the emulator's own state.
Emulator: HTTP 200 and the target is removed — no TargetInUseException, no Message, no __type.
Why: the safety interlock that stops an operator orphaning a maintenance-window task is absent. A client that relies on the refusal to detect "this target is still in use" silently destroys the task's targeting instead, and the window then runs (or fails to run) against nothing.
Fix: the DeregisterTargetFromMaintenanceWindow handler — before mutating state, scan the window's registered tasks for references to WindowTargetId and raise TargetInUseException with AWS's message when Safe is not explicitly false.
Traces: ssm-maintenance-window-lifecycle#task#13.

=== Cause 2 of 7: the legitimate DeregisterTargetFromMaintenanceWindow then fails with 400 ===
AWS: on the success path the operation answers HTTP 200 echoing the modeled output members WindowId (^mw-[0-9a-f]{17}$) and WindowTargetId (UUID), after the referencing task has been deregistered (or with Safe=false).
Emulator: HTTP 400, and neither WindowId nor WindowTargetId is serialized. Hypothesis: the earlier unguarded call (Cause 1) already deleted the target, so the later legitimate deregistration finds no such target and refuses — the two causes share the missing interlock but are separate clusters and both must pass.
Why: a lifecycle client cannot complete an orderly teardown: the call that AWS accepts is rejected, so the window and target records are left in a state the client cannot reconcile, and any teardown-then-recreate flow aborts.
Fix: the same handler's state transition and its success serializer — the target must still exist for the properly ordered deregistration, and the 200 response must project WindowId and WindowTargetId.
Traces: ssm-maintenance-window-lifecycle#teardown#16.

=== Cause 3 of 7: DescribeMaintenanceWindows misprojects Duration and truncates NextExecutionTime ===
AWS: Smithy: MaintenanceWindowIdentity.Duration is the window's duration in hours, @range 1..24, and must equal the value the window was created/updated with; NextExecutionTime targets MaintenanceWindowStringDateTime and is documented as ISO-8601 Extended format — the recorded value carries date, time to seconds, fractional seconds and the Z designator (e.g. a full "....T..:..:...SSSZ" form).
Emulator: for the same window it reports a Duration DIFFERENT from the created value (recorded expected 3, emulator answered 2), and a NextExecutionTime truncated to minute precision with no seconds and no fractional part (recorded expected a full second+millisecond ISO-8601 value, emulator answered a "...T..:..Z" form).
Why: Duration is the window's own configuration echoed back — a client that reads it to render or re-apply the window gets a value it never set, and any read-modify-write of the window silently shrinks it. The truncated timestamp is not a parseable ISO-8601 Extended instant at the precision AWS promises, so schedulers computing the next run mis-order or reject it.
Fix: the DescribeMaintenanceWindows response projection (Duration must come from the stored window record) and the maintenance-window timestamp serializer (full ISO-8601 Extended precision). The open issue 20261112T101725Z-ssm-maintenance-windows-diverge-deleted-window-still-readable.md touches NextExecutionTime truncation on other operations but does not own this cluster's replay key and does not cover the Duration misprojection.
Traces: ssm-maintenance-window-lifecycle#task#4.

=== Cause 4 of 7: GetOpsSummary's invalid-filter-key refusal is emulator prose, not AWS's rule ===
AWS: Smithy: GetOpsSummary.Filters is an OpsFilterList @length 1..5; modeled errors include InvalidFilter (httpError 400), InvalidTypeNameException and InvalidAggregatorException. Recorded behavior: a filter Key that is not in TypeName.AttributeName form is refused with HTTP 400 and Message "Invalid filter key <key>. The key format must be TypeName.AttributeName, such as AWS:Application.Name." Fixed text: "Invalid filter key " and ". The key format must be TypeName.AttributeName, such as AWS:Application.Name."; the single variable part is the offending filter key copied from the request.
Emulator: HTTP 400 with Message "The filter name isn't valid. Verify that you entered the correct name and try again."
Why: the message is a requirement, and AWS's message states the RULE (and names the offending key) while the emulator's states nothing actionable. An operator cannot tell which of up to five filter keys was rejected or what form was expected, and suites asserting AWS's wording fail.
Fix: the GetOpsSummary filter validation and its InvalidFilter message formatter.
Traces: ssm-get-ops-summary-filters-and-errors#task#3.

=== Cause 5 of 7: GetOpsSummary returns no Entities for an aggregated query ===
AWS: Smithy: GetOpsSummaryResult.Entities is an OpsEntityList of OpsEntity {Id, Data: OpsEntityItemMap}; the operation is paginated on Entities. Recorded behavior: an aggregated/typed query over AWS:OpsItem answers HTTP 200 with at least one OpsEntity whose Data maps the queried type name to an entity item carrying a Content list (empty Content when nothing matches) — the entity envelope itself is always present.
Emulator: HTTP 200 but Entities[0] is absent entirely (no entity envelope at all).
Why: a client reading an Ops Center summary cannot distinguish "the query ran and matched nothing" from "the service returned no result shape". Dashboards and aggregation consumers that index Entities[0].Data[<typeName>].Content fail on a missing element rather than reading an empty Content list.
Fix: the GetOpsSummary response projection — always emit the OpsEntity envelope(s) for the requested aggregators/result attributes, with an empty Content list when no items match.
Traces: ssm-ops-summary-aggregation#task#2 (also ssm-ops-summary-by-status-and-missing-sync#task#1).

=== Cause 6 of 7: ListNodes misclassifies a missing resource data sync as UnsupportedOperationException ===
AWS: Smithy: ListNodes takes an optional SyncName (@length 1..64); modeled errors include ResourceDataSyncNotFoundException (httpError 400, awsQueryError ResourceDataSyncNotFound) AND UnsupportedOperationException — they are distinct conditions. Recorded behavior: a SyncName naming no resource data sync in the account is refused with HTTP 400, __type ResourceDataSyncNotFoundException, and the error body carries the SyncName member echoing the requested sync name from the request.
Emulator: HTTP 400 with __type UnsupportedOperationException and no SyncName member.
Why: the error code is a requirement and the two codes mean different things — "that sync does not exist, create it" versus "this operation is not supported in this configuration". A client branching on the code takes the wrong recovery path, and losing the SyncName echo removes the only indication of which sync was missing.
Fix: the ListNodes handler's sync resolution and its error classification/serialization (raise ResourceDataSyncNotFoundException with the SyncName member populated from the request).
Traces: ssm-resource-data-sync-not-found-errors#task#2.

=== Cause 7 of 7: CreateAssociationBatch omits DocumentVersion from each Successful entry ===
AWS: Smithy: CreateAssociationBatchResult.Successful is an AssociationDescriptionList; AssociationDescription.DocumentVersion has @pattern ^([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)$. Recorded behavior: for a batch entry that does not pin a version, the created association is described with DocumentVersion '$DEFAULT' — the member is always present on the 200 path.
Emulator: HTTP 200 and Successful[0].DocumentVersion is absent.
Why: a client that reads back the association it just created cannot tell which document version State Manager bound, and code that round-trips the description into UpdateAssociation loses the version pin entirely. The default must be materialized, not omitted.
Fix: the CreateAssociationBatch handler's association-record construction / AssociationDescription projection — default DocumentVersion to '$DEFAULT' when the entry omits it, and serialize it. (The open issue 20261105T101017Z-ssm-state-manager-associations-refusal-wording-response-projection.md owns the same omission on CreateAssociation/DescribeAssociation; it does not carry this cluster's replay key and CreateAssociationBatch is not covered by it.)
Traces: ssm-state-manager-association-workflow#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 850 steps, score 62.4%). Seven net-new ssm clusters, none carrying stub_path or owned_by:

1) DeregisterTargetFromMaintenanceWindow — ssm-maintenance-window-lifecycle#task#13: expected status 400, got 200; $.__type missing (expected 'TargetInUseException'); $.Message missing (expected 'This Target cannot be deregistered because it is still referenced in Task: {{window_task_id}}').
2) DeregisterTargetFromMaintenanceWindow — ssm-maintenance-window-lifecycle#teardown#16: expected status 200, got 400; $.WindowId missing; $.WindowTargetId missing.
3) DescribeMaintenanceWindows — ssm-maintenance-window-lifecycle#task#4: 200->200; WindowIdentities[...].Duration value_mismatch (expected 3, got 2); WindowIdentities[...].NextExecutionTime value_mismatch (expected a full ISO-8601 Extended value with seconds and milliseconds + 'Z', got a minute-precision '...T06:01Z' form).
4) GetOpsSummary — ssm-get-ops-summary-filters-and-errors#task#3: 400->400; $.Message value_mismatch: expected 'Invalid filter key AccountId. The key format must be TypeName.AttributeName, such as AWS:Application.Name.', got "The filter name isn't valid. Verify that you entered the correct name and try again."
5) GetOpsSummary — ssm-ops-summary-aggregation#task#2 (cluster also covers ssm-ops-summary-by-status-and-missing-sync#task#1): 200->200; $.Entities[0] missing (expected {'Data': {'AWS:OpsItem': {'Content': []}}}, got None).
6) ListNodes — ssm-resource-data-sync-not-found-errors#task#2: 400->400; $.__type value_mismatch (expected 'ResourceDataSyncNotFoundException', got 'UnsupportedOperationException'); $.SyncName missing (expected the requested sync name, got None).
7) CreateAssociationBatch — ssm-state-manager-association-workflow#task#11: 200->200; $.Successful[0].DocumentVersion missing (expected '$DEFAULT', got None).

Model evidence (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): DeregisterTargetFromMaintenanceWindow errors DoesNotExistException + TargetInUseException, output WindowId/WindowTargetId; MaintenanceWindowIdentity.Duration @range 1..24, NextExecutionTime -> MaintenanceWindowStringDateTime (ISO-8601 Extended); GetOpsSummary errors InvalidFilter/InvalidTypeNameException/InvalidAggregatorException/ResourceDataSyncNotFoundException, output Entities -> OpsEntityList of OpsEntity{Id,Data}; ListNodes errors ResourceDataSyncNotFoundException and UnsupportedOperationException (distinct); AssociationDescription.DocumentVersion @pattern ^([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)$.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: replay ssm-maintenance-window-lifecycle step task#13 — DeregisterTargetFromMaintenanceWindow for a target still referenced by a registered window task; expect 400 TargetInUseException with AWS's Message, observe 200.
4. Cause 2: replay the same fixture's step teardown#16 — the properly ordered DeregisterTargetFromMaintenanceWindow; expect 200 echoing WindowId and WindowTargetId, observe 400.
5. Cause 3: replay the same fixture's step task#4 — DescribeMaintenanceWindows; compare the window's Duration against the value it was created with and NextExecutionTime's precision.
6. Cause 4: replay ssm-get-ops-summary-filters-and-errors step task#3 — GetOpsSummary with a filter Key not in TypeName.AttributeName form; compare $.Message.
7. Cause 5: replay ssm-ops-summary-aggregation step task#2 (and ssm-ops-summary-by-status-and-missing-sync#task#1) — GetOpsSummary with an AWS:OpsItem aggregator; expect an Entities[0] envelope.
8. Cause 6: replay ssm-resource-data-sync-not-found-errors step task#2 — ListNodes with a SyncName that names no sync; expect 400 ResourceDataSyncNotFoundException carrying SyncName.
9. Cause 7: replay ssm-state-manager-association-workflow step task#11 — CreateAssociationBatch with an entry that omits DocumentVersion; expect Successful[0].DocumentVersion '$DEFAULT'.
