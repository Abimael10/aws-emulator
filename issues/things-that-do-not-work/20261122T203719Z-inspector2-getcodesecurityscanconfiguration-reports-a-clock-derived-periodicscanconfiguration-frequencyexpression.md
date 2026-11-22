Issue-Source: trace-sweep-divergence
Issue-Trace: inspector2-code-security-scan-configuration-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: inspector2-code-security-scan-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.inspector2#Inspector2
Issue-Key: inspector2|GetCodeSecurityScanConfiguration|2xx->2xx|$.configuration.periodicScanConfiguration.frequencyExpression
Issue-Commit: a7b3a929bdd53afd3c781969eac09fa5fa2267e9

# inspector2 GetCodeSecurityScanConfiguration reports a clock-derived periodicScanConfiguration.frequencyExpression AWS does not report

> _Note: this issue was identified against the code base at commit `a7b3a929bdd53afd3c781969eac09fa5fa2267e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCodeSecurityScanConfiguration

## Description
Single cause (Cause 1 of 1): the code-security scan-configuration store invents a periodic cron schedule for a configuration whose create named a `frequency` but no `frequencyExpression`, and every read of that configuration then reports the invented value.

Cause 1 of 1 — `$.configuration.periodicScanConfiguration.frequencyExpression` is populated on a read where AWS reports no value (fixture `inspector2-code-security-scan-configuration-lifecycle`, step `task#2`).

AWS: Smithy `GetCodeSecurityScanConfiguration` → output member `configuration` (`CodeSecurityScanConfiguration`) → member `periodicScanConfiguration` (`PeriodicScanConfiguration`). That structure has exactly two members, NEITHER required: `frequency` (`PeriodicScanFrequency`, enum `WEEKLY | MONTHLY | NEVER`) and `frequencyExpression` (`FrequencyExpression`, string, `@length` min 1 max 256, documented as "The schedule expression for periodic scans, in cron format"). The `frequency` member's documentation says only "If you don't provide the `frequencyExpression` Amazon Inspector chooses day for the scan to run. If you provide the `frequencyExpression`, the schedule must match the specified `frequency`" — it does NOT state that the chosen day is surfaced back as a `frequencyExpression`, and the model marks the member optional precisely so it can be absent. Recorded behavior only: in the recording, the create at the preceding step names the periodic settings WITHOUT a `frequencyExpression`, and the AWS response replayed at `task#2` carries NO asserted value at `$.configuration.periodicScanConfiguration.frequencyExpression` (the replay reports expected `<shaped-not-asserted>`). Both sides answer HTTP 200 and there is no error code or error message in this divergence, so only the response body differs.

Emulator: the read answers 200 with `$.configuration.periodicScanConfiguration.frequencyExpression` set to a cron expression the emulator synthesized itself — the replay observed `cron(34 20 ? * 1 *)`. That string is not a constant and is not echoed from the request: its minute, hour and day-of-week fields are read off the host wall clock at the instant the configuration was created (minute-of-hour, hour-of-day, and weekday numbered from Sunday for `WEEKLY`; day-of-month capped at 28 for `MONTHLY`). The recorded `cron(34 20 ? * 1 *)` is therefore one run's clock reading, not a requirement — the general defect is that a value is emitted at all, and that whatever is emitted varies with the clock.

Why: a client that round-trips a scan configuration sees a schedule it never asked for. Code that creates a configuration with `frequency: WEEKLY` and then reads it back to render or diff the schedule will show a concrete cron expression against AWS's empty field, so a configuration-drift or idempotency check (create → get → compare to the submitted configuration) reports a spurious difference. Because the invented expression changes with the time of day and day of week the emulator happens to run at, the same test is not even self-consistent between runs: two identical creates minutes apart produce two different schedules, and a client asserting on the read-back value cannot be made to pass deterministically. Clients that then feed the read-back configuration into `UpdateCodeSecurityScanConfiguration` persist a schedule the user never specified.

Fix: the configuration's state layer, on the create path — `stored_code_security_scan_configuration` must store the periodic settings as sent instead of filling the absent `frequencyExpression` in, and `chosen_frequency_expression` (the clock-derived cron synthesizer it calls) should not be consulted for a create that named no expression. `GetCodeSecurityScanConfiguration`'s handler is a pure read of that stored record and needs no change beyond the store reporting what was sent. An expression the caller DID supply must still be kept and echoed verbatim.

Traces: fixture `inspector2-code-security-scan-configuration-lifecycle`, step `task#2` (`GetCodeSecurityScanConfiguration`, 200→200, key `$.configuration.periodicScanConfiguration.frequencyExpression`).

No open inspector2 issue covers this: the three existing inspector2 issues concern unimplemented control-plane reads, the `ListFindingAggregations` ValidationException message, and the `ResetEncryptionKey` stub; the cloudformation__inspector2 issue concerns `CreateStack` resource types.

## Evidence
Replay divergence (verdict `mismatch`, cluster key `service=inspector2|GetCodeSecurityScanConfiguration|status=200->200|keys=$.configuration.periodicScanConfiguration.frequencyExpression`):

  fixture_id: inspector2-code-security-scan-configuration-lifecycle
  step_key:   task#2
  operation:  GetCodeSecurityScanConfiguration
  expected_status: 200   actual_status: 200
  detail: $.configuration.periodicScanConfiguration.frequencyExpression: value_mismatch (expected '<shaped-not-asserted>', got 'cron(34 20 ? * 1 *)')

Expected (AWS, recorded): the read's `configuration.periodicScanConfiguration` carries no value at `frequencyExpression` — the member is optional in `PeriodicScanConfiguration` and the recording asserts nothing there for a configuration created without one.

Actual (emulator): `configuration.periodicScanConfiguration.frequencyExpression` is present and set to a cron expression derived from the emulator host's wall clock. Code path:

  services/service-com-amazonaws-inspector2-inspector2/src/ops/create_code_security_scan_configuration.rs:219-222
      configuration: crate::state::stored_code_security_scan_configuration(input.configuration, now)
  services/service-com-amazonaws-inspector2-inspector2/src/state.rs:718-731
      stored_code_security_scan_configuration — fills the absent frequency_expression via chosen_frequency_expression(frequency, created_at)
  services/service-com-amazonaws-inspector2-inspector2/src/state.rs:683-710
      chosen_frequency_expression — "WEEKLY" => cron({minute} {hour} ? * {weekday-from-sunday} *), "MONTHLY" => cron({minute} {hour} {day-of-month<=28} * ? *), both read off the creation instant
  services/service-com-amazonaws-inspector2-inspector2/src/ops/get_code_security_scan_configuration.rs:197
      configuration: Some(stored.configuration.clone()) — the read echoes the stored (invented) schedule

The observed `34`/`20`/`1` in `cron(34 20 ? * 1 *)` are that run's minute, hour and weekday; they are not part of any requirement. The requirement is that no `frequencyExpression` is reported when the create supplied none.

## Steps to Reproduce
1. Start the emulator from this checkout and point a client at its inspector2 endpoint.
2. Replay fixture `inspector2-code-security-scan-configuration-lifecycle`. Step `task#1` creates a code security scan configuration whose `configuration.periodicScanConfiguration` names a `frequency` and NO `frequencyExpression` (POST /codesecurity/scan-configuration/create).
3. Step `task#2` calls `GetCodeSecurityScanConfiguration` with the returned `scanConfigurationArn` (POST /codesecurity/scan-configuration/get).
4. Inspect the 200 response body at `$.configuration.periodicScanConfiguration`. The emulator returns a `frequencyExpression` (a `cron(...)` string whose minute/hour/day fields follow the host clock); the recorded AWS response has no value at that path.
5. Repeat step 2-3 at a different time of day or on a different weekday: the emulator's `frequencyExpression` changes, confirming the value is derived from the wall clock rather than from the request.
