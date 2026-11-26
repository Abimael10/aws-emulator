Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 692424277e1d35e583945ee4447dd18f53ca6040

# A finished configuration check is evaluated against the configuration as it stands when it is first POLLED, not at its own EndTime

_Filed: 2026-11-26T18:59:58Z_
_Operation: GetConfigurationCheckOperation | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `692424277e1d35e583945ee4447dd18f53ca6040`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A configuration check run is filed `INPROGRESS` with its terminal instant stored on the record (`terminal_due_at = now + TRANSITION_SPAN`), and its rules are evaluated inside `RegionState::settle`. The crate states twice that the evaluation belongs to the RUN's own instant and not to the read that observes it, and the record it writes stamps `EndTime` with `terminal_due_at`.

But `settle(now)` materialises EVERY application change due at or before `now` FIRST (the `for application in self.applications.values_mut()` loop), and only afterwards resolves the application the run was started for and evaluates the rules against it. `now` is the instant of the first request at or after the run's due instant, which can be arbitrarily later. So any application change that fell due BETWEEN the run's `terminal_due_at` and the moment somebody first polls is applied before the rules run, and the finished run reports rule results — and `RuleStatusCounts`, and the `SubCheckResult`/`RuleResult` records `ListSubCheckResults` and `ListSubCheckRuleResults` page over — computed from a configuration that did not exist at the `EndTime` the same record reports.

Nothing stops that window from existing: `UpdateApplicationSettings` files its change with `due_at = now + TRANSITION_SPAN` and its only conflict gate is `application_control_available`, which looks at `StoredApplication::pending` alone. A configuration check run sets no `pending` on the application, so a settings update started just after a check start is accepted, and its credentials/`DatabaseArn` rewrite (which is exactly what the `HanaDatabaseRegistered`, `SystemDatabaseRegistered`, `DatabaseCredentialsPresent` and `CredentialSecretsNamed` rules read) settles after the check was due to finish.

The observable consequence is that the SAME sequence of API calls yields DIFFERENT results for the same run depending on when the client first polls it: poll between the two due instants and the run freezes the old configuration; poll after both and the same run reports the new one. That is precisely what this crate's stored-due-instant design exists to prevent everywhere else ("The due instant is STORED on the record and materialised by the first request at or after it ... nothing ever sleeps", state.rs:384-385).

## Evidence

The rule the crate states for this evaluation — services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:2028-2032:
```
// A configuration check run reaches its terminal status at its own instant too,
// and what it reports then is the EVALUATION of its check's rules against the
// application it was started for ([`ConfigurationCheckEntry::evaluate`]) — so the
// rules are evaluated here, at the instant the run was due to finish whether or
// not anybody polled it, and never by the read that observes the record.
```
and src/state.rs:2044-2046:
```
// The application is resolved here rather than held on the run, so the rules
// judge the configuration as it stands when the run ends.
let evaluated = match self.applications.get(&application_id) {
```
The code under those comments judges the application as it stands at `now`, because the SAME `settle` pass has already applied every change due at or before `now` — src/state.rs:2013-2023:
```
for application in self.applications.values_mut() {
    let region = arn_region(&application.arn).to_string();
    ...
    application.settle(now);
```
while the record it then writes claims it ended at its own due instant — src/state/configuration_checks.rs:883-890:
```
                self.sub_check_results = results;
...
        self.end_time = Some(self.terminal_due_at);
```
and src/state/configuration_checks.rs:871-872 repeats the claim: "The caller computes it only for a run [`Self::terminal_due`] holds for, because the rules are evaluated by the run and not by the read that observes it."

The window is reachable because the only gate `UpdateApplicationSettings` applies is on `StoredApplication::pending`, which a check run never sets — src/ops/update_application_settings.rs:210-214 and 296-303:
```
        crate::state::application_control_available(
            &application,
            crate::state::ApplicationControl::SettingsUpdate,
        )
...
        application.pending = Some(crate::state::PendingTransition {
            due_at: now + crate::state::TRANSITION_SPAN,
            change: crate::state::PendingChange::Settings(...)
```
(The reverse direction IS gated: `StartConfigurationChecks` refuses while a change is owed — src/ops/start_configuration_checks.rs:199-203 — and tests/all/configuration_checks_in_flight.rs pins that direction only: "RULE under test: a call that starts asynchronous work against an application is refused with the modelled `ConflictException` (HTTP 409) while a change the clock still owes that application is outstanding". No test exercises a settings update started AFTER a check run: tests/all/configuration_check_rules.rs contains no `update-application-settings` call.)

## Steps to Reproduce

1. RegisterApplication a HANA application with `Sid`, `SapInstanceNumber` and ONE credential whose `DatabaseName` is `HDB`; wait one TRANSITION_SPAN so it settles ACTIVATED / DiscoveryStatus SUCCESS.
2. Call StartConfigurationChecks with `ConfigurationCheckIds: ["SAP_CHECK_01"]` and keep the returned run id R. R is INPROGRESS with its terminal instant one TRANSITION_SPAN away.
3. Immediately (within that span) call UpdateApplicationSettings with `CredentialsToAddOrUpdate` naming a SECOND database (e.g. `DatabaseName: "TNT1"`). It is accepted — the application has no pending change, and no gate considers the in-flight check run — and its change falls due one TRANSITION_SPAN later, i.e. AFTER R's terminal instant.
4. Wait until both due instants have passed, then call GetConfigurationCheckOperation(R) and ListSubCheckResults/ListSubCheckRuleResults(R): the run reports SUCCESS with `EndTime` at its own due instant, but the `HanaDatabaseRegistered` rule result reads "2 SAP HANA database(s)" with `DatabaseCount` 2 — a configuration that only came into being after that EndTime.
5. Repeat steps 1-3, but this time call GetConfigurationCheckOperation(R) in the window AFTER R's due instant and BEFORE the settings update's: the same run freezes `DatabaseCount` 1 and keeps it on every later read. Identical call sequences, different stored results, decided by when the caller first polled.
