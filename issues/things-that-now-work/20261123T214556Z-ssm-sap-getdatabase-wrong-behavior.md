Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: c80d66a23f690e017421222dc6c5a4f355562a75

# Report Database$Status UNKNOWN while the application's discovery never succeeded

_Filed: 2026-11-23T21:45:56Z_
_Operation: GetDatabase | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `c80d66a23f690e017421222dc6c5a4f355562a75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StoredDbms::status_for` decides `Database$Status` from the application's `ApplicationStatus` alone, and its doc comment states the rule it means to implement: the DBMS status is reported "only from a status in which the service has established what the application runs at all", and for "any other application status (its registration is still under way, or discovery never succeeded) nothing is known about its databases, which is what the model's `UNKNOWN` states".

The code implements that rule by assuming a never-discovered application can only be `REGISTERING` or `FAILED`. A sibling handler breaks that premise: `StopApplication` is deliberately served against an application whose registration is still running or has failed (it applies no `registration_failed()` gate, unlike `StartApplication`), so it moves a never-discovered application to `STOPPING` and then `STOPPED` while carrying the owed `REGISTRATION_FAILED` discovery result forward. From that instant `status_for` falls into its first arm and `GetDatabase` reports the raw DBMS status — `RUNNING`, since a stop that names no `StopConnectedEntity` leaves the DBMS alone — for a database of an application whose discovery never succeeded and for which the service manages nothing. The condition the comment says must read `UNKNOWN` reads `RUNNING` instead, and the application simultaneously reports `Status=STOPPED`.

(`component_status_for` at state.rs:2994-3012 carries the identical premise and the identical parenthetical, but `StoredApplication::components()` returns nothing until discovery completes, so that copy has no observable effect.)

## Evidence

The rule and its implementation — `services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:755-771`:
```
    /// The `DatabaseStatus` a database of an application whose `ApplicationStatus` is
    /// `application_status` reports.
    ///
    /// A database lives in the connected DBMS, so the status it reports is that DBMS's —
    /// but only from a status in which the service has established what the application
    /// runs at all. For any other application status (its registration is still under way,
    /// or discovery never succeeded) nothing is known about its databases, which is what
    /// the model's `UNKNOWN` states.
    pub fn status_for(&self, application_status: &str) -> &str {
        match application_status {
            APPLICATION_STATUS_ACTIVATED
            | APPLICATION_STATUS_STARTING
            | APPLICATION_STATUS_STOPPING
            | APPLICATION_STATUS_STOPPED => self.status.as_str(),
            _ => DATABASE_STATUS_UNKNOWN,
        }
    }
```

The sibling that puts a never-discovered application into one of those four statuses — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/stop_application.rs:304-323` (no `registration_failed()` gate anywhere in `record_stop`):
```
    application.status = crate::state::APPLICATION_STATUS_STOPPING.to_string();
...
            status: crate::state::APPLICATION_STATUS_STOPPED.to_string(),
...
            discovery_status: application.owed_discovery_status().to_string(),
```
while `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/start_application.rs:177-184` does gate the same condition: `if application.registration_failed() { return Err(... failed_registration_message ...) }`.

The read that surfaces it — `services/service-com-amazonaws-ssmsap-ssmsap/src/ops/get_database.rs:205` (`let view = application.database_view(&database, &region);`) and `services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:1401` (`status: Some(self.dbms.status_for(&self.status).to_string()),`).

Test coverage: `tests/all/get_database.rs:687-773` (`the_status_of_a_database_follows_the_dbms_it_lives_in`) states the same rule — "and `UNKNOWN` while the service has established nothing about what the application runs" — but only exercises the `REGISTERING` row (`(Act::Nothing, "just registered", "UNKNOWN")`); no row has a failed discovery. `tests/all/registration_status_message.rs:274-342` pins the reachable sequence (a stop overtaking a registration whose discovery cannot be carried out settles to `Status=STOPPED` with `DiscoveryStatus=REGISTRATION_FAILED`) without reading any database back.

## Steps to Reproduce

1. Mount this service beside the EC2 emulator (the `WithEc2` harness) and `POST /register-application` for a HANA application naming an instance id EC2 holds no record of, with `Credentials: [{DatabaseName: "SYSTEMDB", ...}]` so the registration derives a database.
2. Immediately `POST /stop-application` with that `ApplicationId` (accepted while the registration is still running).
3. Wait out `TRANSITION_SPAN`, then `POST /get-application`: `Status=STOPPED`, `DiscoveryStatus=REGISTRATION_FAILED` — the service discovered nothing for this application.
4. `POST /get-database` with `{"ApplicationId": <id>, "DatabaseId": "SYSTEMDB"}`: `Database.Status` is `RUNNING`.
5. Expected per `StoredDbms::status_for`'s own stated rule ("discovery never succeeded ⇒ ... the model's `UNKNOWN`"): `UNKNOWN`. The same read before the stop (while the application was `FAILED`/`REGISTERING`) does return `UNKNOWN`, so the stop alone changes the answer.
