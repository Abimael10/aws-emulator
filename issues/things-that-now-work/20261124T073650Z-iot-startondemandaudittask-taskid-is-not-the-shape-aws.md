Issue-Source: trace-sweep-divergence
Issue-Trace: iot-on-demand-audit-task-lifecycle
Issue-Step: task#4
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-on-demand-audit-task-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|StartOnDemandAuditTask|2xx->2xx|$.taskId
Issue-Commit: 3593513fb7d4f5dd39b253d8c8db33155e4b6dbb

# iot StartOnDemandAuditTask taskId is not the shape AWS mints an audit task id in

> _Note: this issue was identified against the code base at commit `3593513fb7d4f5dd39b253d8c8db33155e4b6dbb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartOnDemandAuditTask

## Description
Cause 1 of 1: the identifier StartOnDemandAuditTask answers with is minted in a shape that does not match the shape AWS answers an on-demand Device Defender audit with, so the replay's shape assertion on $.taskId fails even though the call is otherwise correct (200, no other member diverges).

AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): StartOnDemandAuditTask POST /audit/tasks, success code 200, input targetCheckNames (TargetAuditCheckNames, required), output member taskId -> AuditTaskId with @length min 1 max 40 and @pattern ^[a-zA-Z0-9\-]+$; errors InvalidRequestException(400), LimitExceededException(410), ThrottlingException(400), InternalFailureException(500). The modelled pattern/length are the OUTER bound only — they admit hyphens and anything from 1 to 40 characters — so the concrete id shape is recorded behavior only. The recording asserts taskId by SHAPE, not by literal value ('<shaped-not-asserted>'): the literal id of any single run is a generated per-run value and is never the requirement. What IS the requirement is the shape class AWS's recorded id falls in — its width and its character class (which characters may appear, including whether the id is hyphen-separated and whether it is single- or mixed-case) — because that is what a client validating or sizing an audit task id keys on. Hypothesis: the recorded id is the hyphenated, lowercase, 36-character UUID form AWS uses for this service's other generated identifiers (compare the recorded mitigation-action actionId), whereas the emulator's token carries uppercase letters and no hyphen at all; the operative fact from the replay is only that the emulator's token is NOT in the recorded shape class.

Emulator: answers 200 with a taskId of a different shape class: exactly 20 characters drawn uniformly from mixed-case ASCII letters and digits with no hyphen (AUDIT_TASK_ID_LENGTH = 20 and AUDIT_TASK_ID_ALPHABET = A-Za-z0-9 in the iot audit-task state; the handler mints it through mint_audit_task_id/fresh_audit_task_id). The value is genuinely random per call and is persisted and read back consistently by DescribeAuditTask, so uniqueness and stability are fine — only the shape diverges. The replay reports $.taskId as a value_mismatch against the recorded shape: expected '<shaped-not-asserted>', got a 20-character mixed-case alphanumeric token.

Why: a client that round-trips the id opaquely is unaffected, but a client that validates, parses or sizes the id it stores — a column width, a UUID parse of the audit task id, a regex derived from observed AWS ids, or correlation of an audit task id with the same id carried in a Device Defender notification — accepts AWS's id and rejects or truncates the emulator's. Any test asserting the id's shape rather than its literal value passes on AWS and fails here, which is exactly what this step records.

Fix: id generation for audit tasks — the width and alphabet constants and the minting helper behind the StartOnDemandAuditTask handler's output (the iot audit-task state module), so the minted id falls in the shape class AWS's recorded id does while remaining unique per task, stable across DescribeAuditTask, and inside AuditTaskId's modelled @length 1..40 and @pattern ^[a-zA-Z0-9\-]+$. Nothing else about the operation needs to change: the status, the account-configuration precondition, the targeted-check validation and the persisted task record all replay clean.

No open issue covers this. The nearest neighbours are 20261122T223418Z-iot-mitigation-action-actionid-is-a-fixed-constant.md (a different defect — a frozen constant actionId, not a shape divergence, and it carries no StartOnDemandAuditTask replay key), 20261120T170349Z-iot-resource-state-divergences-deleted-scheduled-audit-stays.md and 20261116T040307Z-iot-createscheduledaudit-pattern-refusal-drops-the-offending-value.md (scheduled audits, not audit tasks). The replay reports this cluster with an empty owned_by and no blocking stub.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator up (/__admin/health 200), 169 fixtures replayed, 1120 steps, 875 passed, 210 mismatched, 35 blocked, score 78.1%, 31 divergence clusters. This is the ONLY cluster with an empty owned_by and no stub_path; the other 30 are already owned by open issues.

Cluster: service iot, operation StartOnDemandAuditTask, fixture iot-on-demand-audit-task-lifecycle, step task#4, verdict mismatch, expected_status 200, actual_status 200, affected_count 1.
  detail: $.taskId: value_mismatch (expected '<shaped-not-asserted>', got 'r22Th9eFU9QUjsf3QfJD')
  divergence_key: service=iot|StartOnDemandAuditTask|status=200->200|keys=$.taskId

Neither 'r22Th9eFU9QUjsf3QfJD' nor the recording's own id is a requirement: the harness asserts the member's shape, not its value, and the emulator's token is what fails that assertion. Its observable properties are fixed by the code, not by the run: length exactly 20, character class [A-Za-z0-9], never a hyphen.

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json):
  StartOnDemandAuditTask: http POST /audit/tasks, code 200; input targetCheckNames (required); output taskId -> AuditTaskId (length 1..40, pattern ^[a-zA-Z0-9\-]+$)

Code evidence:
  services/service-com-amazonaws-iot-awsiotservice/src/ops/start_on_demand_audit_task.rs:201 — task_id = crate::state::mint_audit_task_id(&store)
  services/service-com-amazonaws-iot-awsiotservice/src/state/audit_tasks.rs:87 — pub const AUDIT_TASK_ID_LENGTH: usize = 20;
  services/service-com-amazonaws-iot-awsiotservice/src/state/audit_tasks.rs:92 — pub const AUDIT_TASK_ID_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"; (documented in-crate as "with no hyphen in it")
  services/service-com-amazonaws-iot-awsiotservice/src/state/audit_tasks.rs:458-489 — mint_audit_task_id / fresh_audit_task_id draw AUDIT_TASK_ID_LENGTH characters of that alphabet from the system random source
The in-crate comment asserts AWS "hands an audit task an OPAQUE token of this width"; the recorded response contradicts that choice, which is the defect.

## Steps to Reproduce
Boot the emulator from this checkout and drive its iot endpoint (or replay fixture iot-on-demand-audit-task-lifecycle, step task#4).

1. Enable an audit configuration for the account: POST /audit/configuration (UpdateAccountAuditConfiguration) enabling at least one audit check, e.g. LOGGING_DISABLED_CHECK.
2. POST /audit/tasks (StartOnDemandAuditTask) with body {"targetCheckNames":["LOGGING_DISABLED_CHECK"]}.
3. Inspect the 200 response's taskId and compare its SHAPE — not its value — with the shape of the taskId AWS records for the same call: its length, its character class, and whether it is hyphen-separated and single-cased.
   Expected: an id in the shape class AWS mints an audit task id in, within AuditTaskId's @length 1..40 and @pattern ^[a-zA-Z0-9\-]+$.
   Observed: a 20-character hyphen-free mixed-case alphanumeric token (e.g. of the form seen in the replay), which is a different shape class.
4. Repeat step 2: the ids differ from each other and GET /audit/tasks/{taskId} (DescribeAuditTask) resolves each one, confirming that uniqueness and persistence are correct and the shape alone diverges.

Replay: iot-on-demand-audit-task-lifecycle#task#4.
