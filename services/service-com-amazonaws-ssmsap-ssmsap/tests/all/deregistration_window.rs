// SPDX-License-Identifier: CC0-1.0

//! What a deregistration leaves behind — nothing, and no window — driven over the wire.
//!
//! Every case goes through the real router (the harness `tests/all/application_lifecycle.rs`
//! owns), so the restJson1 routing, the boundary constraint walk, the handler and the
//! response serialization all run exactly as they do in the emulator binary. Nothing
//! reaches into `crate::state` to assert a behaviour: the ONE read of the store here is
//! the id of the record the deregistration files, which no response hands back, and every
//! assertion about that record is then made over the wire through `GetOperation`.
//!
//! RULE under test: a `DeregisterApplication` that is accepted ends the registration at
//! that instant — the application resolves for no later call, and what was held FOR it
//! (its tags, the configuration check runs that inspected it) stops resolving with it — so
//! the very next call naming that id answers the condition that the account has no such
//! registered application, each operation on the error family its own model declares
//! (HTTP 400 `ValidationException` where no not-found error is modelled, HTTP 404
//! `ResourceNotFoundException` where one is), and the id is free to register again. The
//! operation RECORDS of calls already made survive, since they are the history
//! `GetOperation` resolves by id.
//!
//! No assertion here waits for the clock: there is no instant between the deregistration
//! being accepted and the application being gone, which is exactly what the rows pin.
//!
//! The rows span what that rule generalises over: the `ApplicationId` forms the modelled
//! pattern admits, the application type, whether the registration carried tags, and the
//! state the application is in when it is deregistered — still registering (a change
//! owed), activated, started, and stopped.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use service_com_amazonaws_ssmsap_ssmsap::state::OPERATION_TYPE_DEREGISTER_APPLICATION;

use crate::application_lifecycle::{tag_pairs, valid_registration, Emulator, Reply, REGION};
use crate::configuration_checks::{started, string_field};
use crate::resource_tags::list_tags_in_region;
use crate::validation_message_rendering::{message, VALIDATION_EXCEPTION};

/// The account every emulated ARN carries, and the account a lookup runs in, from the
/// core's single source of truth.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The modelled error code the calls that DO declare a not-found error answer an
/// application that is not registered with (`smithy_query(query="operation",
/// operation="StartApplication")`: `ResourceNotFoundException` `@httpError 404`).
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The fixed sentence the ARN-addressed tagging routes refuse an ARN that names no
/// resource of this account and Region with, on their own not-found class.
const NOT_FOUND_CODE: &str = service_com_amazonaws_ssmsap_ssmsap::scaffolded::NOT_FOUND_CODE;

/// `ApplicationStatus` values the rows drive the application through
/// (`smithy_query(query="shape", shape="ApplicationStatus")`).
const ACTIVATED: &str = "ACTIVATED";
const STOPPED: &str = "STOPPED";

/// How long a poll for a status the clock owes may go on before the case fails. Generous
/// by design: the loop waits for the CONDITION, and only a store that never settles can
/// run it out.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// How this service states that the account holds no such registered application: the
/// wording the calls that model NO not-found error answer the condition with, fixed text
/// around the account the lookup ran in and the id the request carried.
///
/// Spelled out here rather than taken from the crate, so the wording is pinned
/// independently of the one place the handlers build it in.
fn no_registered_application(id: &str) -> String {
    format!("No registered application found for account: {ACCOUNT}, and application: {id}")
}

/// How the calls that DO model a not-found error state the same condition: fixed text
/// around the id the request carried. Spelled out here for the same reason.
fn application_does_not_exist(id: &str) -> String {
    format!("Application {id} doesn't exist.")
}

/// How the application-scoped listing of operation records states it. Spelled out here
/// for the same reason.
fn the_application_does_not_exist(id: &str) -> String {
    format!("The application '{id}' does not exist")
}

/// One call that names an application by its `ApplicationId`, and the refusal it answers
/// for an id that names no registered application.
///
/// Which error family each call carries is its MODEL's: a call that declares a
/// `ResourceNotFoundException` reports the condition as that 404, and one that declares
/// none reports it as the 400 `ValidationException` it does declare
/// (`smithy_query(query="operation", operation="GetApplication,GetComponent,ListOperations,DeregisterApplication")`
/// list no 404).
struct Refusal {
    /// The restJson1 path the model binds the operation to.
    path: &'static str,
    /// The smallest body the call accepts for the application the case names.
    body: fn(&str) -> Value,
    status: u16,
    code: &'static str,
    /// How this service words the condition for the id the request carried.
    message: fn(&str) -> String,
}

/// Every call this suite asks for the deregistered id: the read the recording shows
/// refused, the mutating calls on the other error family, and the application-scoped
/// listings.
const REFUSALS: &[Refusal] = &[
    Refusal {
        path: "/get-application",
        body: |id| json!({"ApplicationId": id}),
        status: 400,
        code: VALIDATION_EXCEPTION,
        message: no_registered_application,
    },
    Refusal {
        // Deregistering a deregistered id is refused: the first call already ended that
        // registration, so the second one names no registered application.
        path: "/deregister-application",
        body: |id| json!({"ApplicationId": id}),
        status: 400,
        code: VALIDATION_EXCEPTION,
        message: no_registered_application,
    },
    Refusal {
        // `ComponentId` is modelled `^[\w\d-]+$`; the application is what fails to
        // resolve here, before any component is looked for.
        path: "/get-component",
        body: |id| json!({"ApplicationId": id, "ComponentId": "HDB-HANA"}),
        status: 400,
        code: VALIDATION_EXCEPTION,
        message: no_registered_application,
    },
    Refusal {
        path: "/list-operations",
        body: |id| json!({"ApplicationId": id}),
        status: 400,
        code: VALIDATION_EXCEPTION,
        message: the_application_does_not_exist,
    },
    Refusal {
        path: "/start-application",
        body: |id| json!({"ApplicationId": id}),
        status: 404,
        code: RESOURCE_NOT_FOUND_EXCEPTION,
        message: application_does_not_exist,
    },
    Refusal {
        path: "/stop-application",
        body: |id| json!({"ApplicationId": id}),
        status: 404,
        code: RESOURCE_NOT_FOUND_EXCEPTION,
        message: application_does_not_exist,
    },
    Refusal {
        path: "/start-application-refresh",
        body: |id| json!({"ApplicationId": id}),
        status: 404,
        code: RESOURCE_NOT_FOUND_EXCEPTION,
        message: application_does_not_exist,
    },
    Refusal {
        // `UpdateApplicationSettings` needs a setting to change, or it refuses the empty
        // request before it resolves anything.
        path: "/update-application-settings",
        body: |id| {
            json!({
                "ApplicationId": id,
                "CredentialsToAddOrUpdate": [{
                    "DatabaseName": "TNT1",
                    "CredentialType": "ADMIN",
                    "SecretId": "sap/tnt1/admin",
                }],
            })
        },
        status: 404,
        code: RESOURCE_NOT_FOUND_EXCEPTION,
        message: application_does_not_exist,
    },
    Refusal {
        path: "/start-configuration-checks",
        body: |id| json!({"ApplicationId": id}),
        status: 404,
        code: RESOURCE_NOT_FOUND_EXCEPTION,
        message: application_does_not_exist,
    },
    Refusal {
        path: "/list-configuration-check-operations",
        body: |id| json!({"ApplicationId": id}),
        status: 404,
        code: RESOURCE_NOT_FOUND_EXCEPTION,
        message: application_does_not_exist,
    },
];

/// What the application has been through by the time it is deregistered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// Nothing: the registration is still settling, so a change was already owed when
    /// the deregistration was made.
    StillRegistering,
    /// The registration has settled, so the application is activated and no change is
    /// owed.
    Activated,
    /// A settled `StartApplication` on top of that.
    Started,
    /// A settled `StopApplication` on top of that, so the application is stopped.
    Stopped,
}

/// One application to deregister.
#[derive(Debug)]
struct Case {
    label: &'static str,
    /// The `ApplicationId`, spanning the forms the modelled `^[\w\d\.-]+$` admits.
    id: &'static str,
    /// An `ApplicationType` value (`smithy_query(query="shape",
    /// shape="ApplicationType")`: `HANA` | `SAP_ABAP`).
    app_type: &'static str,
    tags: &'static [(&'static str, &'static str)],
    before: Before,
}

/// The rows: the state the application is in when it is deregistered, across both
/// application types, tagged and untagged, with ids of different forms.
const CASES: &[Case] = &[
    Case {
        label: "an activated, tagged HANA application",
        id: "HDB",
        app_type: "HANA",
        tags: &[("env", "dev")],
        before: Before::Activated,
    },
    Case {
        label: "an application whose registration is still settling",
        id: "a",
        app_type: "HANA",
        tags: &[],
        before: Before::StillRegistering,
    },
    Case {
        label: "a started SAP_ABAP application with a dotted id",
        id: "abap.prod_01",
        app_type: "SAP_ABAP",
        tags: &[("owner", "sap-team"), ("tier", "prod")],
        before: Before::Started,
    },
    Case {
        label: "a stopped application",
        id: "HDB-TEST",
        app_type: "HANA",
        tags: &[],
        before: Before::Stopped,
    },
];

impl Emulator {
    /// Register `case`'s application and return the `OperationId` the call handed out.
    async fn register_case(&self, case: &Case) -> String {
        self.register_application(case.label, case.id, case.app_type, case.tags)
            .await
    }

    /// Register one application and return the `OperationId` the call handed out.
    async fn register_application(
        &self,
        label: &str,
        id: &str,
        app_type: &str,
        tags: &[(&str, &str)],
    ) -> String {
        let mut body = match valid_registration(id, app_type) {
            Value::Object(body) => body,
            other => panic!("case {label:?}: a registration body is an object, got {other:?}"),
        };
        if !tags.is_empty() {
            let mut rendered = Map::new();
            for (key, value) in tags {
                rendered.insert((*key).to_string(), json!(value));
            }
            body.insert("Tags".into(), Value::Object(rendered));
        }
        let reply = self.send("/register-application", Value::Object(body)).await;
        assert_eq!(
            reply.status, 200,
            "case {label:?}: expected 200 from RegisterApplication, got {reply:?}"
        );
        operation_id(label, &reply)
    }

    /// The ids of the `DEREGISTER_APPLICATION` records filed against `application_id` in
    /// [`REGION`].
    ///
    /// `DeregisterApplicationOutput` carries no `OperationId`
    /// (`smithy_query(query="shape", shape="DeregisterApplicationOutput")`: no members),
    /// so this is the one record of this slice whose id no response hands out — hence the
    /// store read. Everything asserted about the record itself goes over the wire.
    fn deregister_record_ids(&self, application_id: &str) -> Vec<String> {
        let mut ids = Vec::new();
        self.with_state(|state| {
            for (id, record) in &state.region(REGION).operations {
                if record.op_type == OPERATION_TYPE_DEREGISTER_APPLICATION
                    && record.resource_id == application_id
                {
                    ids.push(id.clone());
                }
            }
        });
        ids
    }

    /// Look at `GetApplication` until the application reports `wanted`.
    ///
    /// The emulator never sleeps: a change the clock owes is materialised by the first
    /// request at or after its due instant, so this looks AGAIN rather than waiting a
    /// fixed time, and fails only if the status never arrives.
    async fn await_status(&self, label: &str, id: &str, wanted: &str) {
        let started_at = Instant::now();
        loop {
            let reply = self
                .send("/get-application", json!({"ApplicationId": id}))
                .await;
            if reply.status == 200 && reply.app_str("Status") == wanted {
                return;
            }
            assert!(
                started_at.elapsed() < POLL_DEADLINE,
                "case {label:?}: expected {id:?} to report Status {wanted}, got {reply:?}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }
}

/// The `OperationId` a reply handed out.
fn operation_id(label: &str, reply: &Reply) -> String {
    reply
        .member("OperationId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {label:?}: expected an OperationId, got {reply:?}"))
        .to_string()
}

/// Every `ApplicationSummary` id a listing reported.
fn listed(reply: &Reply) -> Vec<String> {
    reply
        .member("Applications")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("Id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ARN the registration of `id` mints in [`REGION`].
fn expected_arn(id: &str) -> String {
    format!("arn:aws:ssm-sap:{REGION}:{ACCOUNT}:application/{id}")
}

#[tokio::test]
async fn a_deregistration_takes_the_application_out_of_every_path_at_once() {
    for case in CASES {
        let label = case.label;
        let emulator = Emulator::new();
        // A second application, deregistered by nothing, proves the removal is scoped to
        // the application the call names.
        let survivor = format!("{}-keep", case.id);
        let mut earlier_operations = vec![emulator.register_case(case).await];
        emulator
            .register_application(label, &survivor, case.app_type, &[])
            .await;

        match case.before {
            Before::StillRegistering => {}
            Before::Activated => emulator.await_status(label, case.id, ACTIVATED).await,
            Before::Started => {
                emulator.await_status(label, case.id, ACTIVATED).await;
                let started = emulator
                    .send("/start-application", json!({"ApplicationId": case.id}))
                    .await;
                assert_eq!(
                    started.status, 200,
                    "case {label:?}: expected 200 from StartApplication, got {started:?}"
                );
                earlier_operations.push(operation_id(label, &started));
                emulator.await_status(label, case.id, ACTIVATED).await;
            }
            Before::Stopped => {
                emulator.await_status(label, case.id, ACTIVATED).await;
                let stopped = emulator
                    .send("/stop-application", json!({"ApplicationId": case.id}))
                    .await;
                assert_eq!(
                    stopped.status, 200,
                    "case {label:?}: expected 200 from StopApplication, got {stopped:?}"
                );
                earlier_operations.push(operation_id(label, &stopped));
                emulator.await_status(label, case.id, STOPPED).await;
            }
        }

        // What is held FOR the registration while it exists: its tags, and a
        // configuration check run that inspected it. A run can only be started against an
        // application whose registration has completed, so the row that deregisters one
        // still registering starts none.
        let tags_before = list_tags_in_region(&emulator, REGION, &expected_arn(case.id)).await;
        assert_eq!(
            tags_before.status, 200,
            "case {label:?}: a registered application's tags resolve, got {tags_before:?}"
        );
        let expected_tags: BTreeMap<String, String> = case
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        // `ListTagsForResource` reports its map in the member the model names it with
        // (`smithy_query(query="shape", shape="ListTagsForResourceOutput")`: `tags`).
        assert_eq!(
            tag_pairs(tags_before.member("tags")),
            expected_tags,
            "case {label:?}: the tags its registration carried are the ones held, got \
             {tags_before:?}"
        );
        let mut check_runs: Vec<String> = Vec::new();
        if case.before != Before::StillRegistering {
            let checks = emulator
                .start_checks(json!({"ApplicationId": case.id}))
                .await;
            assert_eq!(
                checks.status, 200,
                "case {label:?}: expected 200 from StartConfigurationChecks, got {checks:?}"
            );
            check_runs = started(&checks)
                .iter()
                .map(|operation| string_field(operation, "Id"))
                .collect();
            assert!(
                !check_runs.is_empty(),
                "case {label:?}: a start files at least one run, got {checks:?}"
            );
        }

        let deregistered = emulator
            .send("/deregister-application", json!({"ApplicationId": case.id}))
            .await;
        assert_eq!(
            deregistered.status, 200,
            "case {label:?}: expected 200 from DeregisterApplication, got {deregistered:?}"
        );
        assert!(
            deregistered.member("Application").is_none()
                && deregistered.member("OperationId").is_none(),
            "case {label:?}: DeregisterApplicationOutput models no member, got {deregistered:?}"
        );

        // From here on NOTHING waits for the clock: the registration ended when the call
        // was accepted, so the next call already sees it gone.
        for refusal in REFUSALS {
            let path = refusal.path;
            let reply = emulator.send(path, (refusal.body)(case.id)).await;
            assert_eq!(
                (reply.status, reply.code()),
                (refusal.status, refusal.code),
                "case {label:?}: expected {} {} from {path} for a deregistered id, got \
                 {reply:?}",
                refusal.status,
                refusal.code
            );
            let expected = (refusal.message)(case.id);
            assert_eq!(
                message(&reply),
                expected,
                "case {label:?}: expected {path} to state {expected:?}, got {:?}",
                message(&reply)
            );
            assert!(
                reply.member("Application").is_none() && reply.member("OperationId").is_none(),
                "case {label:?}: a refusal from {path} reports nothing for the application, \
                 got {reply:?}"
            );
        }

        let listing = emulator.send("/list-applications", json!({})).await;
        assert_eq!(
            listed(&listing),
            vec![survivor.clone()],
            "case {label:?}: only the other application remains listed, got {listing:?}"
        );

        // What was held FOR the registration went with it, at the same instant.
        let tags_after = list_tags_in_region(&emulator, REGION, &expected_arn(case.id)).await;
        assert_eq!(
            (tags_after.status, tags_after.code()),
            (404, NOT_FOUND_CODE),
            "case {label:?}: its ARN names nothing once it is deregistered, got {tags_after:?}"
        );
        for run in &check_runs {
            let reply = emulator.get_check(run).await;
            assert_eq!(
                (reply.status, reply.code()),
                (400, VALIDATION_EXCEPTION),
                "case {label:?}: the run {run:?} that inspected it stops resolving, got \
                 {reply:?}"
            );
        }

        // The other application is untouched by any of it.
        let other = emulator
            .send("/get-application", json!({"ApplicationId": survivor}))
            .await;
        assert_eq!(
            other.status, 200,
            "case {label:?}: the application nobody deregistered still resolves, got {other:?}"
        );

        // The record of the deregistration is filed like every other operation's, and the
        // records of the calls made before it are history the removal does not destroy.
        let records = emulator.deregister_record_ids(case.id);
        assert_eq!(
            records.len(),
            1,
            "case {label:?}: the deregistration files exactly one record, got {records:?}"
        );
        let record = emulator
            .send("/get-operation", json!({"OperationId": records[0]}))
            .await;
        assert_eq!(
            record.status, 200,
            "case {label:?}: the deregistration's own record resolves, got {record:?}"
        );
        assert_eq!(
            (
                record.op_str("Type"),
                record.op_str("ResourceId"),
                record.op_str("ResourceArn"),
            ),
            (
                OPERATION_TYPE_DEREGISTER_APPLICATION,
                case.id,
                expected_arn(case.id).as_str(),
            ),
            "case {label:?}: the record must describe the deregistration it was filed for, \
             got {record:?}"
        );
        for earlier in &earlier_operations {
            let reply = emulator
                .send("/get-operation", json!({"OperationId": earlier}))
                .await;
            assert_eq!(
                reply.status, 200,
                "case {label:?}: an OperationId handed out before the deregistration still \
                 resolves, got {reply:?}"
            );
            assert_eq!(
                reply.op_str("ResourceId"),
                case.id,
                "case {label:?}: that record still names the application it was filed against, \
                 got {reply:?}"
            );
        }

        // The id is free again at once, and the re-registration is a new record.
        let reregistered = emulator
            .register_application(label, case.id, case.app_type, &[])
            .await;
        assert!(
            !reregistered.is_empty() && !earlier_operations.contains(&reregistered),
            "case {label:?}: the re-registration files its own record, got {reregistered:?}"
        );
    }
}
