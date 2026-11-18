// SPDX-License-Identifier: CC0-1.0

//! The refusals MediaConvert words as ENTIRELY FIXED text.
//!
//! The RULE under test: where the sentence AWS answers for an error class is all fixed
//! text, the emulator answers those bytes and nothing else — no part of the request and no
//! part of the store appears in it, so every request that earns the refusal is answered
//! with the SAME sentence. Three doors are covered here, each with the sentence AWS is
//! recorded answering:
//!
//! * `DeleteQueue` on a queue that still holds a job in `SUBMITTED` or `PROGRESSING` —
//!   the modelled `ConflictException` (409), "The service couldn't complete your request
//!   because there is a conflict with the current state of the resource" — states the
//!   conflict and the remediation, and names neither the queue nor how many jobs it holds;
//! * `UpdateJobTemplate` on a built-in (`SYSTEM`) job template — the modelled
//!   `ForbiddenException` (403), with `JobTemplate$Type`'s "System or built-in job
//!   templates can't be modified or deleted by the user" — states the class of resource it
//!   will not update, and names neither the template nor the member it arrived in;
//! * `ListVersions` from an account that does not have access to Job engine version
//!   selection — the modelled `ForbiddenException` (403), "You don't have permissions for
//!   this action with the credentials you sent" — states the capability the account lacks
//!   and how to ask for it, and names no part of the request at all.
//!
//! Each sentence is spelled out below rather than imported from the crate, so a wrong
//! constant in the implementation cannot make a test pass.
//!
//! The rows span what each rule generalises over: for the queue delete, the NAME of the
//! queue, HOW MANY jobs hold it and in which statuses, and whether the queue is paused;
//! for the template update, WHICH built-in template is addressed (read out of the store's
//! own catalogue, so no row is pinned to the first one listed) and WHICH members the
//! update carries; for the version listing, WHAT the request asks of the catalogue — no
//! query at all, each published bound of `maxResults`, and a `nextToken` — because the
//! capability is the account's and nothing in the request bears on it. Each door's
//! positive control is asserted beside its refusals: the delete succeeds once no job holds
//! the queue, a CUSTOM template takes the same update, and the same listings are served
//! once the account has the capability.

use serde_json::{json, Value};

use super::job_engine_versions::{grant_version_selection, list_versions};
use super::job_lifecycle::{settings as job_settings, Emulator, Reply, ROLE};

/// The sentence MediaConvert answers a `DeleteQueue` of a queue that is in use with.
const QUEUE_IN_USE: &str = "The queue is currently in use and cannot be deleted. To \
     resolve: Stop submitting any other requests to modify this queue, and cancel any \
     remaining jobs that are in a SUBMITTED or PROGRESSING state in this queue. Then, \
     resubmit your request.";

/// The sentence MediaConvert answers an `UpdateJobTemplate` of a built-in template with.
const SYSTEM_TEMPLATE_UPDATE: &str = "You are not allowed to update system job templates.";

/// The sentence MediaConvert answers a `ListVersions` from an account that does not have
/// access to Job engine version selection with.
const NO_VERSION_SELECTION: &str = "Your account does not have access to MediaConvert \
     version selection. For more information, or to request access, contact AWS Support.";

/// One queue the delete is refused for: its name, how many jobs are submitted to it, and
/// whether the queue is paused while they wait.
struct Busy {
    case: &'static str,
    queue: &'static str,
    jobs: usize,
    paused: bool,
}

/// The rows span the queue-delete class along what the rule generalises over: the queue's
/// name (a plain one, one carrying separators, a long one), how many jobs hold it, and
/// whether those jobs are waiting in an ACTIVE or a PAUSED queue.
const BUSY: &[Busy] = &[
    Busy {
        case: "one job in a plainly named queue",
        queue: "busy",
        jobs: 1,
        paused: false,
    },
    Busy {
        case: "several jobs in a queue whose name carries separators",
        queue: "sim2real-mc-queue.b6c10d0c02ce",
        jobs: 3,
        paused: false,
    },
    Busy {
        case: "one job waiting in a PAUSED queue",
        queue: "held-queue",
        jobs: 1,
        paused: true,
    },
    Busy {
        case: "many jobs in a long queue name",
        queue: "a-queue-name-that-is-considerably-longer-than-the-others-here",
        jobs: 5,
        paused: false,
    },
];

/// `DELETE /2017-08-29/queues/{name}`.
async fn delete_queue(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .send("DELETE", &format!("/2017-08-29/queues/{name}"), None)
        .await
}

#[tokio::test]
async fn a_queue_in_use_is_refused_with_the_fixed_in_use_sentence() {
    for row in BUSY {
        let case = row.case;
        // Jobs stay SUBMITTED for the whole case, so no row races the clock.
        let emulator = Emulator::new();
        let mut body = json!({ "name": row.queue });
        if row.paused {
            body["status"] = json!("PAUSED");
        }
        let created = emulator.create_queue(body).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: CreateQueue answered {} with body {}",
            created.status, created.body
        );

        let mut ids = Vec::new();
        for index in 0..row.jobs {
            let job = emulator
                .create_job(json!({
                    "role": ROLE,
                    "queue": row.queue,
                    "settings": job_settings(
                        "s3://example-bucket/in/a.mp4",
                        &format!("_{index}"),
                    ),
                }))
                .await;
            assert_eq!(
                job.status, 201,
                "case {case:?}: CreateJob #{index} answered {} with body {}",
                job.status, job.body
            );
            ids.push(job.string("/job/id"));
        }

        let refused = delete_queue(&emulator, row.queue).await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected HTTP 409, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ConflictException"),
            "case {case:?}: expected ConflictException, got {:?} with body {}",
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.string("/message"),
            QUEUE_IN_USE,
            "case {case:?}: expected the fixed in-use sentence {QUEUE_IN_USE:?}, got {}",
            refused.body
        );

        // The refusal left the queue and its jobs exactly as they were, and once no job
        // holds the queue the same delete is accepted — so the sentence above is the only
        // thing this rule changes.
        let read = emulator.get_queue(row.queue).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the refused delete must leave the queue readable, got {} with \
             body {}",
            read.status, read.body
        );
        for id in &ids {
            let canceled = emulator.cancel_job(id).await;
            assert_eq!(
                canceled.status, 202,
                "case {case:?}: CancelJob {id} answered {} with body {}",
                canceled.status, canceled.body
            );
        }
        let deleted = delete_queue(&emulator, row.queue).await;
        assert_eq!(
            deleted.status, 202,
            "case {case:?}: with no job holding it the queue must delete, got {} with body \
             {}",
            deleted.status, deleted.body
        );
    }
}

/// Which members one row of the template-update table sends.
struct Update {
    case: &'static str,
    body: fn() -> Value,
}

/// The rows span the members an `UpdateJobTemplate` can carry, because the refusal is
/// decided by the resource addressed and not by what the request asks to change.
const UPDATES: &[Update] = &[
    Update {
        case: "a description alone",
        body: || json!({ "description": "sim2real-attempt" }),
    },
    Update {
        case: "a category and a description",
        body: || json!({ "category": "mine", "description": "sim2real-attempt" }),
    },
    Update {
        case: "a priority",
        body: || json!({ "priority": 7 }),
    },
    Update {
        case: "no member at all",
        body: || json!({}),
    },
    Update {
        case: "a whole settings graph",
        body: || json!({ "settings": super::job_template_crud::settings(1, 1) }),
    },
];

/// The built-in templates this account holds, newest listing first — read from
/// `ListJobTemplates(listBy=SYSTEM)` rather than hard-coded, so the rows cover whatever
/// the catalogue publishes.
async fn system_templates(emulator: &Emulator) -> Vec<String> {
    let listed = emulator.list_job_templates("listBy=SYSTEM&maxResults=20").await;
    assert_eq!(
        listed.status, 200,
        "setup: ListJobTemplates(listBy=SYSTEM) answered {} with body {}",
        listed.status, listed.body
    );
    listed
        .at("/jobTemplates")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|template| template["name"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn updating_a_system_job_template_is_refused_with_the_fixed_sentence() {
    let emulator = Emulator::new();
    let templates = system_templates(&emulator).await;
    assert!(
        templates.len() > 1,
        "setup: expected several built-in templates to span, got {templates:?}"
    );

    // Every built-in template the catalogue publishes, against each shape of update: the
    // rule is about the resource, so it holds for every pairing.
    for name in &templates {
        let before = emulator.get_job_template(name).await;
        assert_eq!(
            before.status, 200,
            "case {name:?}: GetJobTemplate answered {} with body {}",
            before.status, before.body
        );
        for row in UPDATES {
            let case = format!("{name}/{}", row.case);
            let refused = emulator.update_job_template(name, (row.body)()).await;
            assert_eq!(
                refused.status, 403,
                "case {case:?}: expected HTTP 403, got {} with body {}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_type.as_deref(),
                Some("ForbiddenException"),
                "case {case:?}: expected ForbiddenException, got {:?} with body {}",
                refused.error_type,
                refused.body
            );
            assert_eq!(
                refused.string("/message"),
                SYSTEM_TEMPLATE_UPDATE,
                "case {case:?}: expected the fixed sentence {SYSTEM_TEMPLATE_UPDATE:?}, got \
                 {}",
                refused.body
            );
            // Nothing the refused update carried reached the stored template.
            let after = emulator.get_job_template(name).await;
            assert_eq!(
                after.template(),
                before.template(),
                "case {case:?}: the refused update must leave the template as it was, got \
                 {} after {}",
                after.body,
                before.body
            );
        }
    }

    // The other side of the rule: a template the CALLER created takes every one of those
    // updates, so the refusal is about the resource and not about the call.
    let created = emulator
        .create_job_template(json!({
            "name": "mine",
            "settings": super::job_template_crud::settings(1, 1),
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateJobTemplate answered {} with body {}",
        created.status, created.body
    );
    for row in UPDATES {
        let served = emulator.update_job_template("mine", (row.body)()).await;
        assert_eq!(
            served.status, 200,
            "case mine/{}: a created template takes the update, got {} with body {}",
            row.case, served.status, served.body
        );
    }
}

/// One request the version listing is driven with, and what the account's capability
/// makes of it.
struct VersionsRequest {
    case: &'static str,
    /// The query string the listing is asked with, apart from the capability.
    query: &'static str,
    /// The status the SAME request answers once the account has been granted the
    /// capability: 200 for a listing the service serves, 400 for the one carrying a token
    /// this service never issued — which is what shows the capability is decided ahead of
    /// the token rather than after it.
    granted_status: u16,
}

/// The rows span what a request can ask of the catalogue, because none of it bears on the
/// account's capability: no query at all, each published bound of `maxResults` and a size
/// between them, and a `nextToken`.
const VERSIONS_REQUESTS: &[VersionsRequest] = &[
    VersionsRequest {
        case: "no query at all",
        query: "",
        granted_status: 200,
    },
    VersionsRequest {
        case: "the bottom of the maxResults range",
        query: "maxResults=1",
        granted_status: 200,
    },
    VersionsRequest {
        case: "a page size inside the maxResults range",
        query: "maxResults=3",
        granted_status: 200,
    },
    VersionsRequest {
        case: "the top of the maxResults range",
        query: "maxResults=20",
        granted_status: 200,
    },
    VersionsRequest {
        case: "a nextToken this service never issued",
        query: "nextToken=not-a-token",
        granted_status: 400,
    },
];

#[tokio::test]
async fn listing_versions_without_the_capability_is_refused_with_the_fixed_sentence() {
    // A fresh account: nobody has been granted it Job engine version selection, which is
    // the state of every account the grant has not been made for.
    let emulator = Emulator::new();

    for row in VERSIONS_REQUESTS {
        let case = row.case;
        let refused = list_versions(&emulator, row.query).await;
        assert_eq!(
            refused.status, 403,
            "case {case:?}: expected HTTP 403, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ForbiddenException"),
            "case {case:?}: expected ForbiddenException, got {:?} with body {}",
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.string("/message"),
            NO_VERSION_SELECTION,
            "case {case:?}: expected the fixed sentence {NO_VERSION_SELECTION:?}, got {}",
            refused.body
        );
        assert!(
            refused.at("/versions").is_null() && refused.at("/nextToken").is_null(),
            "case {case:?}: a refused listing must report no version and no cursor, got {}",
            refused.body
        );
    }

    // The other side of the rule: the SAME account, once the capability has been granted
    // out of band, is served every one of those listings — and the request carrying a
    // token this service never issued is now refused for the TOKEN, which is what shows
    // the capability is read ahead of it.
    grant_version_selection(&emulator);
    for row in VERSIONS_REQUESTS {
        let case = row.case;
        let reply = list_versions(&emulator, row.query).await;
        assert_eq!(
            reply.status, row.granted_status,
            "case {case:?}, granted: expected HTTP {}, got {} with body {}",
            row.granted_status, reply.status, reply.body
        );
        if row.granted_status == 200 {
            assert!(
                reply
                    .at("/versions")
                    .as_array()
                    .is_some_and(|versions| !versions.is_empty()),
                "case {case:?}, granted: expected the published catalogue, got {}",
                reply.body
            );
        } else {
            assert_eq!(
                reply.error_type.as_deref(),
                Some("BadRequestException"),
                "case {case:?}, granted: expected BadRequestException, got {:?} with body {}",
                reply.error_type,
                reply.body
            );
        }
    }

    // Nothing the refusals touched: the listing is a read either way, so two listings of
    // the granted account answer the same bytes.
    let first = list_versions(&emulator, "").await;
    let again = list_versions(&emulator, "").await;
    assert_eq!(
        again.body, first.body,
        "a repeated listing must answer the same, got {} then {}",
        again.body, first.body
    );
}
