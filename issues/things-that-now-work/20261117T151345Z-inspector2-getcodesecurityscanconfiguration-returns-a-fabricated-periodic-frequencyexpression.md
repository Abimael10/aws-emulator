Issue-Source: trace-sweep-divergence
Issue-Trace: inspector2-code-security-scan-configuration-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: inspector2-code-security-scan-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.inspector2#Inspector2
Issue-Key: inspector2|GetCodeSecurityScanConfiguration|2xx->2xx|$.configuration.periodicScanConfiguration.frequencyExpression
Issue-Commit: d1107ef412a33c5fd1cd764c3d91be4521975a1a

# inspector2 GetCodeSecurityScanConfiguration returns a fabricated periodic frequencyExpression

> _Note: this issue was identified against the code base at commit `d1107ef412a33c5fd1cd764c3d91be4521975a1a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCodeSecurityScanConfiguration

## Description
One cause.

Cause 1 of 1: GetCodeSecurityScanConfiguration renders $.configuration.periodicScanConfiguration.frequencyExpression with a schedule string the emulator synthesizes for the configuration's frequency, where the recorded AWS read carries no such value at that path.

AWS: Smithy: PeriodicScanConfiguration$frequencyExpression targets FrequencyExpression (string, @length min 1 max 256) and is OPTIONAL; PeriodicScanConfiguration$frequency targets the PeriodicScanFrequency enum (WEEKLY | MONTHLY | NEVER) and its documentation states "If you don't provide the frequencyExpression Amazon Inspector chooses day for the scan to run. If you provide the frequencyExpression, the schedule must match the specified frequency." The model therefore fixes only that an omitted expression lets the service choose the day; it does NOT state that a service-chosen expression becomes part of what GetCodeSecurityScanConfiguration reports, and it fixes no textual form for such an expression. Recorded behavior only for what the read returns: for this configuration the recorded 200 response holds nothing asserted at $.configuration.periodicScanConfiguration.frequencyExpression (the replay reports the expected side as '<shaped-not-asserted>'), so no caller-visible cron string of the emulator's own choosing is part of the recorded response at that path.

Emulator: the same read answers 200 but puts a literal, hard-coded schedule at that path. Replay detail: "$.configuration.periodicScanConfiguration.frequencyExpression: value_mismatch (expected '<shaped-not-asserted>', got 'cron(0 0 ? * 1 *)')". The value is manufactured, not derived from the request: the create path fills the member in from a fixed per-frequency table (WEEKLY -> "cron(0 0 ? * 1 *)", MONTHLY -> "cron(0 0 1 * ? *)", NEVER -> none) and stores it on the record, and the read then echoes the stored record verbatim. Nothing in the request carried that expression, so the whole string is emulator-invented; it is a constant per frequency, not a shaped/volatile value, so every read of every configuration of that frequency reports the same schedule.

Why: a client that reads a scan configuration back believes Amazon Inspector has committed to a concrete weekly schedule it never asked for and that AWS does not report. Two concrete breakages follow. (1) Round-tripping Get -> UpdateCodeSecurityScanConfiguration re-sends the fabricated expression as if the caller had set it, and because the model requires a provided expression to match the declared frequency, a client that also changes frequency in that round-trip now submits a schedule/frequency pair AWS rejects with ValidationException while the emulator accepts it. (2) Any client or test that asserts on the configuration echo — "the caller set no frequencyExpression, so the read must not surface one" — passes against AWS and fails against the emulator. The divergence is in the response payload only; the status (200) and the operation's error set are unaffected.

Fix: the read's rendering of the stored configuration together with the create-time fill-in that put the value there — GetCodeSecurityScanConfiguration's handler (it clones stored.configuration into the output) and, upstream of it, the state helper that injects a chosen expression into the record a create stores. The emulator must not surface a periodic schedule the request did not carry and the recorded read does not report; store and render the periodic settings as sent, leaving frequencyExpression absent when the caller omitted it.

Traces: inspector2-code-security-scan-configuration-lifecycle#task#2 (GetCodeSecurityScanConfiguration, 200 vs 200, divergence at $.configuration.periodicScanConfiguration.frequencyExpression).

Related but NOT covering this: open issues 20261029T171819Z-inspector2-constraint-violation-validationexception-messages-add-the-rejected.md and 20261115T020952Z-inspector2-listfindings-sortcriteria-field-validationexception-omits-the-last.md own the other two clusters this replay reported (both ValidationException message wording); neither touches this read's payload.

## Evidence
Fixture inspector2-code-security-scan-configuration-lifecycle, step task#2, operation GetCodeSecurityScanConfiguration.

Expected status 200, actual status 200 — the divergence is in the body:
  $.configuration.periodicScanConfiguration.frequencyExpression: value_mismatch
    expected: '<shaped-not-asserted>'  (the recorded read asserts no value at this path)
    actual:   'cron(0 0 ? * 1 *)'      (emulator-invented constant for frequency WEEKLY)

Divergence key: service=inspector2|GetCodeSecurityScanConfiguration|status=200->200|keys=$.configuration.periodicScanConfiguration.frequencyExpression

Smithy evidence (services/service-com-amazonaws-inspector2-inspector2/smithy-model/com-amazonaws-inspector2-inspector2.json):
  - GetCodeSecurityScanConfiguration output member `configuration` -> CodeSecurityScanConfiguration; http POST /codesecurity/scan-configuration/get, success 200.
  - CodeSecurityScanConfiguration$periodicScanConfiguration -> PeriodicScanConfiguration (optional).
  - PeriodicScanConfiguration$frequencyExpression -> FrequencyExpression, @length min 1 max 256, OPTIONAL, doc "The schedule expression for periodic scans, in cron format."
  - PeriodicScanConfiguration$frequency -> PeriodicScanFrequency (WEEKLY | MONTHLY | NEVER), doc "If you don't provide the frequencyExpression Amazon Inspector chooses day for the scan to run. If you provide the frequencyExpression, the schedule must match the specified frequency."
  Nothing in the model states that a service-chosen expression is reported back by this read, and nothing fixes its text.

Emulator source backing the fabricated value (citations only):
  - services/service-com-amazonaws-inspector2-inspector2/src/state.rs:583-601 `chosen_frequency_expression` maps the frequency token to a fixed cron string.
  - services/service-com-amazonaws-inspector2-inspector2/src/state.rs:609-621 `stored_code_security_scan_configuration` writes that string onto the record when the request omitted frequencyExpression.
  - services/service-com-amazonaws-inspector2-inspector2/src/ops/create_code_security_scan_configuration.rs:216-218 stores the filled-in configuration.
  - services/service-com-amazonaws-inspector2-inspector2/src/ops/get_code_security_scan_configuration.rs:197 echoes the stored configuration into the response.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture inspector2-code-security-scan-configuration-lifecycle and stop at step task#2.
   Equivalently, by hand against the emulator's inspector2 endpoint:
   a. POST /codesecurity/scan-configuration/create with a name, level ACCOUNT, and configuration = { ruleSetCategories: [...], periodicScanConfiguration: { frequency: "WEEKLY" } } — i.e. a periodic frequency and NO frequencyExpression. Note the returned scanConfigurationArn.
   b. POST /codesecurity/scan-configuration/get with that scanConfigurationArn.
3. Inspect the 200 response body at $.configuration.periodicScanConfiguration.frequencyExpression.
   Emulator: the member is present and equals the constant "cron(0 0 ? * 1 *)" (use frequency MONTHLY in step 2a and it is the constant "cron(0 0 1 * ? *)" instead).
   Recorded AWS read: no such value is asserted at that path; the request carried no frequencyExpression and the read reports none.
4. Confirm the fix by re-replaying inspector2-code-security-scan-configuration-lifecycle#task#2: the step must pass with status 200 and no divergence at $.configuration.periodicScanConfiguration.frequencyExpression.

## Resolution

What changed: `CreateCodeSecurityScanConfiguration` no longer fills an omitted `frequencyExpression` in from a per-frequency table. The schedule the service chooses is now derived from the configuration's own creation instant — a weekly scan runs on the weekday it was created on (AWS cron numbers the days of the week from 1 for Sunday) with the day-of-month field left as `?`, a monthly one on its day of the month with the day-of-week field left as `?` and a day past the 28th moved to the 28th so the scan recurs in every month, both at the creation time of day; `NEVER` names no schedule, so none is stored. The chosen schedule is therefore a property of that one stored configuration, different for configurations created at different times, and identical on every read of one. `GetCodeSecurityScanConfiguration` and `ListCodeSecurityScanConfigurations` were already reporting it from the record and are unchanged. Covered by `tests/all/code_security_periodic_schedule.rs`, which pins the exact expression owed for a table of creation instants (Sunday, mid-week, Saturday, the 4th, the 28th, the 31st, a leap day, `NEVER`) and asserts end to end that a read's expression agrees with the `createdAt` it reports.

Why the member is KEPT rather than removed: this report's prescription — "leave frequencyExpression absent when the caller omitted it" — rests on the claim that the recorded read carries no value at that path, and replay contradicts it. With the member omitted entirely, the replay still diverges at `$.configuration.periodicScanConfiguration.frequencyExpression`, which is exactly the divergence 20261104T152322Z-inspector2-getcodesecurityscanconfiguration-omits-periodicscanconfiguration-frequencyexpression-and-the-tags.md reported ("missing"). The recorded read does report a schedule the create did not carry, so removing the member would reintroduce that already-fixed defect.

Replay state: the divergence key stays open as a per-run value. The exact text of a schedule Amazon Inspector chooses for one configuration is AWS's own choice at the instant that configuration was created, which is why the corpus elides it; no rule can reproduce one recording's string without writing that literal into the handler. Three structurally different chosen expressions were replayed (digit day-of-week, named day-of-week, bare cron fields) and each diverges at that one path alone; status (200), the error set and every other member of the response match.
