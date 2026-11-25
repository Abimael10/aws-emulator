Issue-Source: trace-sweep-divergence
Issue-Trace: inspector2-code-security-scan-configuration-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: inspector2-code-security-scan-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.inspector2#Inspector2
Issue-Key: inspector2|GetCodeSecurityScanConfiguration|2xx->2xx|$.configuration.periodicScanConfiguration.frequencyExpression
Issue-Commit: c8c1a6886de773856b080208b347842cb2a65b75

# inspector2 GetCodeSecurityScanConfiguration reports a synthesized periodic frequencyExpression the recorded response does not carry

> _Note: this issue was identified against the code base at commit `c8c1a6886de773856b080208b347842cb2a65b75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCodeSecurityScanConfiguration

## Description
Cause 1 of 1: the code-security scan configuration read back by GetCodeSecurityScanConfiguration carries a periodic schedule expression at $.configuration.periodicScanConfiguration.frequencyExpression that does not agree with the recorded AWS response for the same lifecycle step (create a configuration, then read it back).

AWS: Smithy (services/service-com-amazonaws-inspector2-inspector2/smithy-model/com-amazonaws-inspector2-inspector2.json) models GetCodeSecurityScanConfiguration (POST /codesecurity/scan-configuration/get, HTTP 200) with output member `configuration` of shape CodeSecurityScanConfiguration, whose `periodicScanConfiguration` is a PeriodicScanConfiguration with members `frequency` (enum PeriodicScanFrequency: WEEKLY | MONTHLY | NEVER) and `frequencyExpression` (FrequencyExpression, @length 1..256, documented only as "The schedule expression for periodic scans, in cron format"). The `frequency` member documents: "If you don't provide the frequencyExpression Amazon Inspector chooses day for the scan to run. If you provide the frequencyExpression, the schedule must match the specified frequency." The model states nothing further about what the read reports when the caller supplied no expression, so for the reported value itself this is recorded behavior only: at this step the recorded response's value at that path is a service-chosen one the fixture does not assert literally, and the emulator's value is not an accepted realization of that recorded slot. Status is 200 on both sides and no error code is involved; only this one member diverges.

Emulator: the replayed step answers 200 with $.configuration.periodicScanConfiguration.frequencyExpression = 'cron(0 0 ? * WED *)' where the recorded step asserts a shaped (non-literal) value, and the replay reports `value_mismatch` for exactly that path (divergence key service=inspector2|GetCodeSecurityScanConfiguration|status=200->200|keys=$.configuration.periodicScanConfiguration.frequencyExpression). The value is manufactured on the create path, not read from the request: crate::state::stored_code_security_scan_configuration() fills `frequency_expression` from crate::state::chosen_frequency_expression() whenever the stored periodic settings name a frequency but no expression, and that helper builds the string from a fixed time-of-day (CHOSEN_SCHEDULE_TIME_OF_DAY = 0 0) plus a day index hashed off the configuration id minted into the ARN (SCHEDULE_DAY_SEED / chosen_schedule_day_index), so a WEEKLY configuration always reads back as a weekly cron naming one hashed day abbreviation and a MONTHLY one as a day-of-month cron. Get itself only echoes the stored record. Hypothesis: the recorded service either omits `frequencyExpression` from the read when the caller supplied none, or words the chosen expression differently from the emulator's `cron(<minute> <hour> ? * <DAY> *)` template — the recording does not expose the literal, so the emulator's locally invented template is what cannot be justified.

Why: a client that reads a configuration back and then round-trips it (for example feeding the read result into UpdateCodeSecurityScanConfiguration, or diffing desired state against actual in infrastructure tooling) sees a periodic schedule it never set and that AWS does not report in that form. Against AWS that client makes no change; against the emulator it either sees a spurious drift on every read or writes back an expression AWS would have to validate against `frequency`. Equality checks and golden-file assertions over the configuration document fail on a member the caller left unset.

Fix: the read's state projection — the chosen-expression synthesis invoked from the create/store path (the inspector2 service state helpers `stored_code_security_scan_configuration` / `chosen_frequency_expression`) and, by consequence, what the GetCodeSecurityScanConfiguration handler echoes for `configuration`. The stored record should reproduce what the service reports for a configuration created with `frequency` alone rather than filling the member from a locally invented cron template keyed off the minted ARN. No open issue covers this: the open inspector2 issues concern four control-plane reads answering NotImplemented, the ListFindingAggregations ValidationException message, the ResetEncryptionKey stub, and a cloudformation resource-type refusal.

Traces: inspector2-code-security-scan-configuration-lifecycle#task#2 (one affected step in the cluster).

## Evidence
Replay of fixture inspector2-code-security-scan-configuration-lifecycle, step task#2 (GetCodeSecurityScanConfiguration), verdict mismatch:
- expected status 200, actual status 200
- detail: $.configuration.periodicScanConfiguration.frequencyExpression: value_mismatch (expected '<shaped-not-asserted>', got 'cron(0 0 ? * WED *)')
- divergence_key: service=inspector2|GetCodeSecurityScanConfiguration|status=200->200|keys=$.configuration.periodicScanConfiguration.frequencyExpression
- affected: inspector2-code-security-scan-configuration-lifecycle#task#2
Expected: the read reports the periodic scan settings as the recorded service reports them for a configuration created with a frequency and no expression (the recorded value at that path is service-chosen and not asserted literally by the fixture).
Actual: the read reports frequencyExpression = 'cron(0 0 ? * WED *)', synthesized by crate::state::chosen_frequency_expression (fixed 0 0 time-of-day, day abbreviation from SCAN_DAYS_OF_WEEK selected by a hash of the configuration id in the minted ARN).

## Steps to Reproduce
1. Boot the emulator built from this checkout and point an inspector2 client at it.
2. Replay fixture inspector2-code-security-scan-configuration-lifecycle: step task#1 calls CreateCodeSecurityScanConfiguration (POST /codesecurity/scan-configuration/create) with a `configuration.periodicScanConfiguration` naming a `frequency` and no `frequencyExpression`.
3. Step task#2 calls GetCodeSecurityScanConfiguration (POST /codesecurity/scan-configuration/get) with the ARN returned by step 2.
4. Compare $.configuration.periodicScanConfiguration of the 200 response against the recorded response: the emulator adds frequencyExpression = 'cron(0 0 ? * WED *)', which the recorded step does not carry as that value, and replay_wire_corpus reports value_mismatch on that path.
