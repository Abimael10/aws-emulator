Issue-Source: trace-sweep-divergence
Issue-Trace: inspector2-code-security-scan-configuration-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: inspector2-code-security-scan-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.inspector2#Inspector2
Issue-Key: inspector2|GetCodeSecurityScanConfiguration|2xx->2xx|$.configuration.periodicScanConfiguration.frequencyExpression
Issue-Commit: e014230fb9b53a8f1acc538e4bd0b4dbf598fc56

# inspector2 GetCodeSecurityScanConfiguration returns a fabricated periodicScanConfiguration.frequencyExpression AWS never set

> _Note: this issue was identified against the code base at commit `e014230fb9b53a8f1acc538e4bd0b4dbf598fc56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCodeSecurityScanConfiguration

## Description
ONE cause: the emulator invents a periodic cron schedule for a code security scan configuration that was created with a `frequency` but NO `frequencyExpression`, stores it, and then reports it back on every read — so the read of a stored configuration carries a member the real service does not return.

Cause 1 of 1: GetCodeSecurityScanConfiguration echoes a schedule expression the create never supplied.
AWS: Smithy: GetCodeSecurityScanConfiguration (POST /codesecurity/scan-configuration/get, success 200) returns `configuration` targeting CodeSecurityScanConfiguration, whose `periodicScanConfiguration` targets PeriodicScanConfiguration with members `frequency` (enum PeriodicScanFrequency: WEEKLY | MONTHLY | NEVER) and `frequencyExpression` (FrequencyExpression, @length min 1 max 256, "The schedule expression for periodic scans, in cron format."). Neither member is required. The `frequency` member's documentation says only that "If you don't provide the frequencyExpression Amazon Inspector chooses day for the scan to run" and that a provided expression "must match the specified frequency" — it does NOT say the chosen day is surfaced as a `frequencyExpression` in the response. Recorded behavior only: at this step the configuration was created with a periodic `frequency` and no `frequencyExpression`, and the recorded 200 response from GetCodeSecurityScanConfiguration asserts NO value at `$.configuration.periodicScanConfiguration.frequencyExpression` — AWS renders the periodic settings as they were submitted and leaves the expression out when the caller did not provide one. Hypothesis: the day Inspector picks is internal scheduling state and is not projected into the configuration document the API returns.
Emulator: answers 200 (the status is correct) but the response body carries `$.configuration.periodicScanConfiguration.frequencyExpression` = `cron(0 0 ? * 1 *)`, a value nothing in the request established. The value is synthesized at CREATE time, not at read time: `crate::state::stored_code_security_scan_configuration` fills `periodic.frequency_expression` from `crate::state::chosen_frequency_expression(frequency)`, which maps WEEKLY -> "cron(0 0 ? * 1 *)" and MONTHLY -> "cron(0 0 1 * ? *)"; the stored record is then rendered verbatim by the GetCodeSecurityScanConfiguration handler (`configuration: Some(stored.configuration.clone())`). So the fabricated expression is persisted and every read of that configuration repeats it; the same stored value also leaks into any other read or update round-trip of the same record.
Why: a client that reads back a configuration it created cannot distinguish "I set a cron schedule" from "I set only a frequency". Configuration-diffing and infrastructure-as-code reconciliation see a `frequencyExpression` in the live state that is absent from the desired state, so they report permanent drift and keep re-issuing updates; a client that copies the read-back configuration into an update now pins a schedule the operator never chose. The specific cron text is also entirely the emulator's invention — it is not a value AWS ever returned for this input — so any assertion written against the emulator's answer is wrong against AWS.
Fix: the configuration-storage helper in the inspector2 crate's state module — `stored_code_security_scan_configuration` must store the periodic settings exactly as submitted and must not default `frequency_expression` from `frequency`; `chosen_frequency_expression` has no AWS-observable behavior to implement and should not feed the stored/rendered configuration. The GetCodeSecurityScanConfiguration handler itself is correct in rendering the stored record as-is, so no change is needed there once the store stops being populated with the invented value. Note the open issue 20261029T171819Z-inspector2-constraint-violation-validationexception-messages-add-the-rejected.md also cites this fixture, but it covers only the 400 ValidationException message wording on CreateCodeSecurityScanConfiguration (task#3) and does not cover this 200-response body divergence.
Traces: inspector2-code-security-scan-configuration-lifecycle#task#2.

## Evidence
Cause 1 — fixture inspector2-code-security-scan-configuration-lifecycle step task#2, GetCodeSecurityScanConfiguration: expected status 200, actual status 200 — the status and error channel agree and only the response body diverges.

Divergent key, as the replay reports it:
  $.configuration.periodicScanConfiguration.frequencyExpression: value_mismatch (expected '<shaped-not-asserted>', got 'cron(0 0 ? * 1 *)')

Expected: the recorded AWS 200 response asserts no value at that path — the member is not part of the configuration AWS returns for a periodic configuration created with a `frequency` and no `frequencyExpression`.
Actual: the emulator's 200 response carries that member with the cron expression `cron(0 0 ? * 1 *)`.

The fabricated value is not request- or state-derived: it is a literal in the inspector2 crate's state module (`chosen_frequency_expression`: WEEKLY -> "cron(0 0 ? * 1 *)", MONTHLY -> "cron(0 0 1 * ? *)", NEVER -> None), applied by `stored_code_security_scan_configuration` on the create path and rendered unchanged by the GetCodeSecurityScanConfiguration handler. The requirement is structural — the member is absent unless the caller supplied it — not the particular cron text of this recording.

Divergence key: service=inspector2|GetCodeSecurityScanConfiguration|status=200->200|keys=$.configuration.periodicScanConfiguration.frequencyExpression (cluster size 1).

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator.
2. POST /codesecurity/scan-configuration/create (CreateCodeSecurityScanConfiguration) with a valid `name`, a `level`, and a `configuration` whose `ruleSetCategories` are valid and whose `periodicScanConfiguration` names a `frequency` (e.g. WEEKLY) and NO `frequencyExpression` — fixture inspector2-code-security-scan-configuration-lifecycle, step task#1. Keep the returned `scanConfigurationArn`.
3. POST /codesecurity/scan-configuration/get (GetCodeSecurityScanConfiguration) with that `scanConfigurationArn` — fixture inspector2-code-security-scan-configuration-lifecycle, step task#2.
4. Inspect the 200 body at $.configuration.periodicScanConfiguration: the emulator returns `frequencyExpression` = "cron(0 0 ? * 1 *)" (and "cron(0 0 1 * ? *)" if the create named MONTHLY), where the recorded AWS response carries no `frequencyExpression` at all.
5. Repeat the GET: the invented expression is stable across reads, confirming it was persisted by the create rather than computed per read.
