Issue-Source: trace-sweep-divergence
Issue-Trace: inspector2-code-security-scan-configuration-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: inspector2-code-security-scan-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.inspector2#Inspector2
Issue-Key: inspector2|GetCodeSecurityScanConfiguration|2xx->2xx|$.configuration.periodicScanConfiguration.frequencyExpression
Issue-Commit: 4e68013acb17007a4b50a9cc9c0932294de10e0a

# inspector2 code security scan configuration reports a wall-clock-derived frequencyExpression for a service-chosen periodic schedule

> _Note: this issue was identified against the code base at commit `4e68013acb17007a4b50a9cc9c0932294de10e0a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCodeSecurityScanConfiguration

## Description
ONE cause.

Cause 1 of 1: GetCodeSecurityScanConfiguration returns, inside `configuration.periodicScanConfiguration`, a `frequencyExpression` that the emulator synthesizes from the wall-clock instant the configuration happened to be created at, so the value it reports is not the schedule the recorded service reports for the frequency that was asked for.

AWS: Smithy: GetCodeSecurityScanConfiguration, POST /codesecurity/scan-configuration/get, input {scanConfigurationArn (required, ScanConfigurationArn pattern ^arn:(aws[a-zA-Z-]*)?:inspector2:...:owner/(\d{12}|o-[a-z0-9]{10,32})/codesecurity-configuration/[a-f0-9-]{36}$)}, output {scanConfigurationArn, name, configuration: CodeSecurityScanConfiguration, level, scopeSettings, createdAt, lastUpdatedAt, tags}; the configuration member `periodicScanConfiguration` targets PeriodicScanConfiguration{frequency: PeriodicScanFrequency (enum), frequencyExpression: FrequencyExpression @length 1..256}. The model documents frequencyExpression as "The schedule expression for periodic scans, in cron format", and documents on `frequency`: "If you don't provide the frequencyExpression Amazon Inspector chooses day for the scan to run. If you provide the frequencyExpression, the schedule must match the specified frequency." So the ONLY part of a service-chosen schedule the model licenses the service to pick is the DAY the scan runs on; the remaining cron fields are not described as derived from anything the caller did, and in particular not from the moment the create call landed. Recorded behavior: on the read-back of a configuration created in task#1, the fixture asserts this member by SHAPE rather than by literal value (expected side is the placeholder '<shaped-not-asserted>'), i.e. a cron expression of the recorded form for the requested frequency, whose day field is the service's own choice. The read itself is a 200 and no error code or message is in question here — only the value of this one member.

Emulator: answers 200 but with $.configuration.periodicScanConfiguration.frequencyExpression = a cron expression whose minute and hour fields are the minute and hour of the creation instant, and whose day field is the weekday (or day of month) of the creation instant: the replay observed 'cron(30 14 ? * 3 *)', i.e. 14:30 UTC on weekday 3, which is simply when the replay's create ran. The synthesis is in the inspector2 state helper `chosen_frequency_expression(frequency, created_at)` (services/service-com-amazonaws-inspector2-inspector2/src/state.rs), called from `stored_code_security_scan_configuration` on the create path and then echoed verbatim by this read.

Why: a client that reads back a periodic code security scan configuration gets a schedule that changes on every run of the same program — minute, hour and day all move with the clock — so nothing downstream can assert, snapshot, diff or template on the configuration it just created. Infrastructure-as-code and drift-detection flows that compare a desired periodicScanConfiguration against the service's reported one see a permanent, spurious difference, and a test that pins the schedule AWS chose cannot pass. Hypothesis: AWS fixes the time-of-day fields of a service-chosen schedule and varies only the day, which is what the model's "chooses day for the scan to run" wording describes; deriving minute and hour from the create timestamp is the specific part the emulator adds on its own.

Fix: the periodic-schedule fill-in in the inspector2 crate's state, i.e. `chosen_frequency_expression` and its caller `stored_code_security_scan_configuration` on the CreateCodeSecurityScanConfiguration path, must stop reading the creation clock for the minute and hour fields and emit the fixed time-of-day the service uses for a chosen schedule, leaving only the day field service-chosen; an expression the caller supplied must continue to be stored and echoed unchanged. GetCodeSecurityScanConfiguration's handler needs no change beyond echoing what is stored. Note on neighbouring open issues: the open inspector2 issues (the four-stub behavioral bundle, the ListFindingAggregations enum-message issue, and the ResetEncryptionKey stub) concern other operations and do not cover this member.

Traces: inspector2-code-security-scan-configuration-lifecycle#task#2 (GetCodeSecurityScanConfiguration read-back of the configuration created earlier in the same fixture).

## Evidence
Replay of fixture `inspector2-code-security-scan-configuration-lifecycle`, step `task#2`, operation GetCodeSecurityScanConfiguration, verdict `mismatch`.

Expected status 200; actual status 200 — the status and error channel agree, the divergence is a response-member value.

Detail reported by the replay: `$.configuration.periodicScanConfiguration.frequencyExpression: value_mismatch (expected '<shaped-not-asserted>', got 'cron(30 14 ? * 3 *)')`.

Expected: a cron `frequencyExpression` of the form the recorded service reports for the frequency the create asked for — the fixture asserts the member's shape because the day the service picks is its own choice, so the fixed cron fields are the requirement and the day field is not asserted. Actual: `cron(30 14 ? * 3 *)`, whose minute (30), hour (14) and day-of-week (3) are all read off the wall-clock instant at which the replay's CreateCodeSecurityScanConfiguration ran, so the same program produces a different expression on every run.

Code path: services/service-com-amazonaws-inspector2-inspector2/src/state.rs — `chosen_frequency_expression` formats `cron({minute} {hour} ? * {day_of_week} *)` for WEEKLY and `cron({minute} {hour} {day_of_month} * ? *)` for MONTHLY from `DateTime::<Utc>::from(created_at)`; `stored_code_security_scan_configuration` applies it when the request named a frequency with no expression; services/service-com-amazonaws-inspector2-inspector2/src/ops/get_code_security_scan_configuration.rs line 197 returns `stored.configuration.clone()` unchanged.

Divergence key: `service=inspector2|GetCodeSecurityScanConfiguration|status=200->200|keys=$.configuration.periodicScanConfiguration.frequencyExpression`. Cluster size 1 (affected: inspector2-code-security-scan-configuration-lifecycle#task#2). No `stub_path` and no `owned_by`: the operation is implemented and no open issue carries this replay key.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus `simparity-wire-aws-v1`.
2. Replay fixture `inspector2-code-security-scan-configuration-lifecycle`. Step `task#1` calls CreateCodeSecurityScanConfiguration (POST /codesecurity/scan-configuration/create) with a `configuration.periodicScanConfiguration` that names a `frequency` and NO `frequencyExpression`.
3. Step `task#2` calls GetCodeSecurityScanConfiguration (POST /codesecurity/scan-configuration/get) with the `scanConfigurationArn` returned by step `task#1`.
4. Inspect `$.configuration.periodicScanConfiguration.frequencyExpression` in the 200 response. Observe a cron expression whose minute, hour and day fields are those of the moment step 2 executed (the replay saw `cron(30 14 ? * 3 *)`), instead of the recorded service-chosen schedule whose time-of-day fields are fixed.
5. Re-run the same two steps a few minutes later, or on a different weekday: the reported `frequencyExpression` changes, which confirms the value is derived from the creation clock rather than from the requested frequency.
6. Re-replay `inspector2-code-security-scan-configuration-lifecycle#task#2` to confirm a fix.
