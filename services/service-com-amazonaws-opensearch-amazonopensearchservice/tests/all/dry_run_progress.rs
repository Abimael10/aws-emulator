// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeDryRunProgress` and the verbose dry run
//! `UpdateDomainConfig` starts for it to report on.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a
//! client sees. The harness, the baseline `CreateDomain` body and the table of option
//! containers come from [`super::domain_lifecycle`]; this suite adds no second copy of
//! them.
//!
//! The rule under test: a pre-update dry run is a RECORD the call that asked for it
//! mints on the domain, addressed by its own `DryRunId` — so the read that reports its
//! progress answers from that record (the run the request names, or the domain's most
//! recent one when it names none), and a domain or an id that has no such record is
//! refused under the operation's own modelled not-found rather than answered with a
//! progress record for an analysis nothing performed. Which analysis is recorded is
//! what `DryRunMode` asks for: `Verbose` "runs an additional check to validate the
//! changes you're making", which is the one this operation reports the progress of,
//! while `Basic` "only returns the type of deployment" in the update's own answer.
//!
//! So the assertions span a SET of inputs: every option container in the shared change
//! table (a cluster structure, an EBS structure, a policy string, two maps, an enum, a
//! snapshot structure and a software-update structure), both documented dry run modes
//! and a request naming none, both values of `LoadDryRunConfig` and a request naming
//! none, histories of one, two and four runs on one domain, the run addressed by id and
//! the run left implicit, both sides of the analysis span, and five ways for a request
//! to name no run at all.
//!
//! A dry run is also TERMINAL from the call that minted it and is superseded by a change
//! the domain really applies, which
//! [`a_dry_run_is_terminal_from_the_start_and_an_applied_change_supersedes_it`] asserts
//! over three of those option containers.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    change_rows, create_baseline, error_code, settled_domain, string_member, Answer, Emulator,
    CHANGE_LOG_GROUP, PREFIX, RECORDED_NOT_FOUND_STATUS,
};

/// A well-formed `GUID` that names no dry run: the id space is 128 bits wide and the
/// store mints v4 UUIDs, so the nil-shaped id below is never one of them.
const ABSENT_DRY_RUN_ID: &str = "00000000-0000-4000-8000-000000000000";

/// `DryRunProgressStatus$DryRunStatus` for a run whose pre-update check found the
/// proposed configuration applicable. The member is a free `String` in the model, so the
/// value set is the service's own: lowercase, and terminal at `succeeded` for a check
/// that found no validation failure.
const SUCCEEDED: &str = "succeeded";

/// `GET /2021-01-01/opensearch/domain/{DomainName}/dryRun`, with the `dryRunId` and
/// `loadDryRunConfig` query parameters when they are given.
async fn dry_run_progress(
    emulator: &Emulator,
    name: &str,
    dry_run_id: Option<&str>,
    load_config: Option<bool>,
) -> Answer {
    let mut uri = format!("{PREFIX}/opensearch/domain/{name}/dryRun");
    let mut separator = '?';
    if let Some(dry_run_id) = dry_run_id {
        uri.push(separator);
        uri.push_str(&format!("dryRunId={dry_run_id}"));
        separator = '&';
    }
    if let Some(load_config) = load_config {
        uri.push(separator);
        uri.push_str(&format!("loadDryRunConfig={load_config}"));
    }
    emulator.call("GET", &uri, None).await
}

/// The whole body of an accepted progress call.
async fn progress_body(
    emulator: &Emulator,
    name: &str,
    dry_run_id: Option<&str>,
    load_config: Option<bool>,
) -> Value {
    let answer = dry_run_progress(emulator, name, dry_run_id, load_config).await;
    assert_eq!(
        answer.status, 200,
        "case DescribeDryRunProgress({name:?}, {dry_run_id:?}, {load_config:?}): expected 200, \
         got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// Start a verbose dry run of `change` on `name`, and answer the whole update body.
async fn start_verbose_dry_run(emulator: &Emulator, name: &str, change: Value) -> Value {
    let mut body = change;
    body["DryRun"] = json!(true);
    body["DryRunMode"] = json!("Verbose");
    let answer = emulator.update_config(name, body.clone()).await;
    assert_eq!(
        answer.status, 200,
        "case UpdateDomainConfig({name:?}, {body:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// Assert `value` satisfies `GUID`'s own constraint: `@length(min: 36, max: 36)` with
/// `@pattern("^\p{XDigit}{8}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{4}-\p{XDigit}{12}$")`.
fn assert_guid(value: &str, case: &str) {
    let groups: Vec<&str> = value.split('-').collect();
    let widths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    assert_eq!(
        widths,
        vec![8, 4, 4, 4, 12],
        "case {case}: expected hex groups of 8-4-4-4-12, got {value:?} with {widths:?}"
    );
    assert!(
        groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit())),
        "case {case}: every GUID group is hexadecimal, got {value:?}"
    );
}

/// Assert `value` is an instant written as date-time, which is how a member modelled a
/// string carries one — parsed with the SDK's own reader rather than matched by shape.
fn assert_timestamp_text(value: &str, case: &str) {
    assert!(
        ::aws_smithy_types::DateTime::from_str(
            value,
            ::aws_smithy_types::date_time::Format::DateTime,
        )
        .is_ok(),
        "case {case}: expected an instant written as date-time, got {value:?}"
    );
}

#[tokio::test]
async fn a_verbose_dry_run_is_reported_from_the_record_the_update_minted() {
    let emulator = Emulator::with_log_groups();
    // The log-publishing row of the shared table publishes to a log group, which has to
    // exist before an option enabling it is accepted — a dry run is judged by the same
    // rules as the change it reports on.
    emulator.created_log_group(CHANGE_LOG_GROUP).await;
    let rows = change_rows();

    // One domain per row, all created up front so the waits below overlap: a dry run
    // against a domain still provisioning cannot predict a deployment type, which is
    // the update path's own rule and is asserted where it belongs.
    for index in 0..rows.len() {
        create_baseline(&emulator, &format!("verbose-{index}")).await;
    }
    for index in 0..rows.len() {
        settled_domain(&emulator, &format!("verbose-{index}")).await;
    }

    for (index, (option, value, deployment)) in rows.into_iter().enumerate() {
        let name = format!("verbose-{index}");
        let case = format!("DescribeDryRunProgress({option})");

        let started = start_verbose_dry_run(&emulator, &name, json!({ option: value.clone() })).await;
        let started_status = &started["DryRunProgressStatus"];
        let dry_run_id = string_member(started_status, "DryRunId", &case);
        assert_guid(&dry_run_id, &case);
        assert_eq!(
            started_status["DryRunStatus"].as_str(),
            Some(SUCCEEDED),
            "case {case}: the check ran in the call that asked for it, so the run it minted is \
             terminal at {SUCCEEDED}, got {started:?}"
        );

        // The read reports the run the update minted, by the id it handed back and with
        // the request naming none at all.
        for named in [None, Some(dry_run_id.as_str())] {
            let body = progress_body(&emulator, &name, named, None).await;
            let status = &body["DryRunProgressStatus"];
            assert_eq!(
                status["DryRunId"].as_str(),
                Some(dry_run_id.as_str()),
                "case {case} ({named:?}): expected the run the update started, got {body:?}"
            );
            assert_eq!(
                status["DryRunStatus"].as_str(),
                Some(SUCCEEDED),
                "case {case} ({named:?}): expected the same terminal {SUCCEEDED} the update \
                 reported, got {body:?}"
            );
            assert_eq!(
                status["ValidationFailures"].as_array().map(Vec::len),
                Some(0),
                "case {case} ({named:?}): a configuration this service refuses never reaches a \
                 dry run, so the run reports no failure, got {body:?}"
            );
            assert_eq!(
                body["DryRunResults"]["DeploymentType"].as_str(),
                Some(deployment),
                "case {case} ({named:?}): expected DeploymentType {deployment:?}, got {body:?}"
            );
            // "Whether to include the configuration of the dry run in the response": a
            // request that does not ask for it is not answered with it.
            assert!(
                body["DryRunConfig"].is_null(),
                "case {case} ({named:?}): the planned configuration was not asked for, got \
                 {body:?}"
            );
        }

        let creation_date = string_member(
            &progress_body(&emulator, &name, None, None).await["DryRunProgressStatus"],
            "CreationDate",
            &case,
        );
        assert_timestamp_text(&creation_date, &case);

        // "The configuration specifies the updates that you're planning to make on the
        // domain": the planned configuration carries the change, and the domain itself
        // still carries none of it.
        let loaded = progress_body(&emulator, &name, None, Some(true)).await;
        let planned = &loaded["DryRunConfig"];
        assert_eq!(
            planned[option],
            super::domain_lifecycle::completed(option, &value),
            "case {case}: expected the planned {option} to be {value:?}, got {planned:?}"
        );
        assert_eq!(
            planned["DomainName"].as_str(),
            Some(name.as_str()),
            "case {case}: the plan is for this domain, got {planned:?}"
        );
        let live = emulator.describe(&name).await;
        assert_ne!(
            live.body["DomainStatus"][option], value,
            "case {case}: a dry run applies nothing, so the domain must not carry {option}, got \
             {:?}",
            live.body["DomainStatus"]
        );

        // The record does not move: a run whose check has already concluded reports the
        // instant it was created as both its `CreationDate` and its `UpdateDate`, and a
        // second read answers exactly what the first did.
        let reported = progress_body(&emulator, &name, Some(&dry_run_id), None).await;
        let status = &reported["DryRunProgressStatus"];
        let update_date = string_member(status, "UpdateDate", &case);
        assert_timestamp_text(&update_date, &case);
        assert_eq!(
            (
                status["DryRunId"].as_str(),
                status["DryRunStatus"].as_str(),
                status["CreationDate"].as_str(),
                status["UpdateDate"].as_str(),
            ),
            (
                Some(dry_run_id.as_str()),
                Some(SUCCEEDED),
                Some(creation_date.as_str()),
                Some(creation_date.as_str()),
            ),
            "case {case}: the run is terminal and nothing has moved it since it was created, got \
             {reported:?}"
        );
        assert_eq!(
            reported["DryRunResults"]["DeploymentType"].as_str(),
            Some(deployment),
            "case {case}: the run reports the analysis it performed, got {reported:?}"
        );

        // A read does not move the record: the second answer is the first.
        let again = progress_body(&emulator, &name, Some(&dry_run_id), None).await;
        assert_eq!(
            again, reported,
            "case {case}: a read must not change the run: {reported:?} then {again:?}"
        );
    }
}

/// RULE: the pre-update check a verbose dry run performs runs INSIDE the call that asks
/// for it, so the run is terminal from the moment its record exists: its `DryRunStatus`
/// is the service's terminal value for the failures that check recorded — `succeeded`
/// where it recorded none — and that is the value the update's own answer carries and the
/// value every read after it answers, with no wait anywhere.
#[tokio::test]
async fn a_verbose_dry_run_reports_its_terminal_status_with_no_wait() {
    let emulator = Emulator::with_log_groups();
    // The log-publishing row of the shared table publishes to a log group, which has to
    // exist before an option enabling it is accepted.
    emulator.created_log_group(CHANGE_LOG_GROUP).await;
    let rows = change_rows();

    // One domain per option container, all created up front so the waits below overlap.
    for index in 0..rows.len() {
        create_baseline(&emulator, &format!("terminal-{index}")).await;
    }
    for index in 0..rows.len() {
        settled_domain(&emulator, &format!("terminal-{index}")).await;
    }

    for (index, (option, value, _)) in rows.into_iter().enumerate() {
        let name = format!("terminal-{index}");
        let case = format!("DescribeDryRunProgress({option}, no wait)");
        let started = start_verbose_dry_run(&emulator, &name, json!({ option: value })).await;
        let minted = &started["DryRunProgressStatus"];
        let dry_run_id = string_member(minted, "DryRunId", &case);
        let minted_status = minted["DryRunStatus"].as_str().map(str::to_owned);

        // Read straight after that call, three looks over, by the id it handed back and
        // with the request naming none: nothing waits, so a client that never polls still
        // reads a terminal status — the one the update answered with.
        for look in 0..3 {
            for named in [Some(dry_run_id.as_str()), None] {
                let body = progress_body(&emulator, &name, named, None).await;
                let status = &body["DryRunProgressStatus"];
                assert_eq!(
                    (
                        status["DryRunStatus"].as_str(),
                        status["ValidationFailures"].as_array().map(Vec::len),
                    ),
                    (Some(SUCCEEDED), Some(0)),
                    "case {case} (look {look}, {named:?}): a check that recorded no validation \
                     failure leaves the run terminal at {SUCCEEDED}, got {body:?}"
                );
                assert_eq!(
                    status["DryRunStatus"].as_str().map(str::to_owned),
                    minted_status,
                    "case {case} (look {look}, {named:?}): the read and the call that minted the \
                     run cannot report it differently, got {body:?} after {started:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn every_dry_run_keeps_its_own_id_and_the_latest_is_the_implicit_one() {
    let emulator = Emulator::new();
    // Spans the length of a domain's dry-run history: one, two and four runs.
    for runs in [1usize, 2, 4] {
        let name = format!("history-{runs}");
        create_baseline(&emulator, &name).await;
        settled_domain(&emulator, &name).await;

        // (dry run id, the option that run analysed) in the order they were started.
        let mut history: Vec<(String, &'static str, &'static str)> = Vec::new();
        for (option, value, deployment) in change_rows().into_iter().take(runs) {
            let case = format!("{name}: {option}");
            let started =
                start_verbose_dry_run(&emulator, &name, json!({ option: value.clone() })).await;
            history.push((
                string_member(&started["DryRunProgressStatus"], "DryRunId", &case),
                option,
                deployment,
            ));
        }

        let ids: Vec<&String> = history.iter().map(|(id, _, _)| id).collect();
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            ids.len(),
            "case {name}: every dry run has its own id, got {ids:?}"
        );

        // Each run stays addressable by its own id, reporting the analysis IT performed
        // and not another run's.
        for (dry_run_id, option, deployment) in &history {
            let case = format!("{name}: by id {dry_run_id}");
            let body = progress_body(&emulator, &name, Some(dry_run_id), Some(true)).await;
            assert_eq!(
                body["DryRunProgressStatus"]["DryRunId"].as_str(),
                Some(dry_run_id.as_str()),
                "case {case}: expected the run that was asked for, got {body:?}"
            );
            assert_eq!(
                body["DryRunResults"]["DeploymentType"].as_str(),
                Some(*deployment),
                "case {case}: expected the deployment kind of {option}, got {body:?}"
            );
            let expected = change_rows()
                .into_iter()
                .find(|(candidate, _, _)| candidate == option)
                .map(|(_, value, _)| super::domain_lifecycle::completed(option, &value))
                .unwrap_or(Value::Null);
            assert_eq!(
                body["DryRunConfig"][option], expected,
                "case {case}: expected the configuration {option} was analysed with, got {:?}",
                body["DryRunConfig"]
            );
        }

        // A call naming no `DryRunId` reports the most recent run.
        let (latest, _, _) = history
            .last()
            .expect("every row starts at least one dry run");
        let implicit = progress_body(&emulator, &name, None, None).await;
        assert_eq!(
            implicit["DryRunProgressStatus"]["DryRunId"].as_str(),
            Some(latest.as_str()),
            "case {name}: a call naming no DryRunId reports the most recent run, got {implicit:?}"
        );
    }
}

#[tokio::test]
async fn an_absent_domain_or_dry_run_is_the_modelled_not_found() {
    let emulator = Emulator::new();

    // A domain that holds one run, so another domain's id can be asked for against it.
    create_baseline(&emulator, "present-domain").await;
    create_baseline(&emulator, "other-domain").await;
    // And one that never had a dry run asked for at all.
    create_baseline(&emulator, "no-run-domain").await;
    for name in ["present-domain", "other-domain", "no-run-domain"] {
        settled_domain(&emulator, name).await;
    }

    let known = string_member(
        &start_verbose_dry_run(
            &emulator,
            "present-domain",
            json!({ "SnapshotOptions": { "AutomatedSnapshotStartHour": 5 } }),
        )
        .await["DryRunProgressStatus"],
        "DryRunId",
        "present-domain",
    );
    let other = string_member(
        &start_verbose_dry_run(
            &emulator,
            "other-domain",
            json!({ "SnapshotOptions": { "AutomatedSnapshotStartHour": 6 } }),
        )
        .await["DryRunProgressStatus"],
        "DryRunId",
        "other-domain",
    );
    let deleted = emulator
        .call(
            "DELETE",
            &format!("{PREFIX}/opensearch/domain/other-domain"),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case DeleteDomain(other-domain): expected 200, got {} with {:?}",
        deleted.status, deleted.body
    );

    // Rows: a domain that was never created (with and without an id), a domain that was
    // deleted, a domain on which no dry run was ever started — the recorded case — and
    // a well-formed id another domain's run carries. The third column is what the
    // refusal has to name: the id a request that addresses one asks about, and the
    // domain for a request that addresses none or names no domain at all.
    for (name, dry_run_id, names) in [
        ("absent-domain", None, "absent-domain"),
        ("absent-domain", Some(ABSENT_DRY_RUN_ID), "absent-domain"),
        ("other-domain", None, "other-domain"),
        ("no-run-domain", None, "no-run-domain"),
        ("no-run-domain", Some(ABSENT_DRY_RUN_ID), ABSENT_DRY_RUN_ID),
        ("present-domain", Some(ABSENT_DRY_RUN_ID), ABSENT_DRY_RUN_ID),
        ("present-domain", Some(other.as_str()), other.as_str()),
    ] {
        let answer = dry_run_progress(&emulator, name, dry_run_id, None).await;
        assert_eq!(
            answer.status, RECORDED_NOT_FOUND_STATUS,
            "case DescribeDryRunProgress({name:?}, {dry_run_id:?}): expected \
             {RECORDED_NOT_FOUND_STATUS}, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case DescribeDryRunProgress({name:?}, {dry_run_id:?}): expected \
             ResourceNotFoundException, got {:?}",
            answer.body
        );
        // The refusal names what was asked for: the id, where the request addresses one
        // of a domain that exists, and otherwise the domain itself.
        assert!(
            answer.body["message"]
                .as_str()
                .is_some_and(|message| message.contains(names)),
            "case DescribeDryRunProgress({name:?}, {dry_run_id:?}): the refusal names {names:?}, \
             got {:?}",
            answer.body
        );
    }

    // The run that does exist is still reported: a refusal for one id does not disturb
    // the record.
    let body = progress_body(&emulator, "present-domain", Some(&known), None).await;
    assert_eq!(
        body["DryRunProgressStatus"]["DryRunId"].as_str(),
        Some(known.as_str()),
        "case DescribeDryRunProgress(present-domain, known): expected the stored run, got {body:?}"
    );
}

#[tokio::test]
async fn only_a_verbose_dry_run_leaves_a_run_to_report_on() {
    let emulator = Emulator::new();

    // (case, the `DryRunMode` the update names, whether a run is then reportable)
    let rows: [(&str, Option<&str>, bool); 3] = [
        ("Verbose", Some("Verbose"), true),
        ("Basic", Some("Basic"), false),
        ("no mode at all", None, false),
    ];

    for (index, (case, mode, reportable)) in rows.into_iter().enumerate() {
        let name = format!("mode-{index}");
        create_baseline(&emulator, &name).await;
        settled_domain(&emulator, &name).await;

        let mut body = json!({
            "ClusterConfig": { "InstanceType": "r6g.xlarge.search", "InstanceCount": 4 },
            "DryRun": true,
        });
        if let Some(mode) = mode {
            body["DryRunMode"] = json!(mode);
        }
        let answer = emulator.update_config(&name, body.clone()).await;
        assert_eq!(
            answer.status, 200,
            "case UpdateDomainConfig({case}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        // Whichever mode was asked for, the deployment kind the change would need is
        // the update's own answer.
        assert_eq!(
            answer.body["DryRunResults"]["DeploymentType"].as_str(),
            Some("Blue/Green"),
            "case UpdateDomainConfig({case}): expected the deployment kind of the change, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body["DryRunProgressStatus"].is_null(),
            !reportable,
            "case UpdateDomainConfig({case}): expected a progress status iff an analysis was \
             started, got {:?}",
            answer.body
        );

        let progress = dry_run_progress(&emulator, &name, None, None).await;
        if reportable {
            assert_eq!(
                progress.status, 200,
                "case DescribeDryRunProgress({case}): expected 200, got {} with {:?}",
                progress.status, progress.body
            );
            assert_eq!(
                progress.body["DryRunProgressStatus"]["DryRunId"],
                answer.body["DryRunProgressStatus"]["DryRunId"],
                "case DescribeDryRunProgress({case}): expected the run the update minted, got {:?}",
                progress.body
            );
        } else {
            assert_eq!(
                progress.status, RECORDED_NOT_FOUND_STATUS,
                "case DescribeDryRunProgress({case}): a mode that starts no analysis leaves \
                 nothing to report, got {} with {:?}",
                progress.status, progress.body
            );
            assert_eq!(
                error_code(&progress),
                "ResourceNotFoundException",
                "case DescribeDryRunProgress({case}): expected ResourceNotFoundException, got {:?}",
                progress.body
            );
        }
    }
}

#[tokio::test]
async fn a_malformed_dry_run_id_is_refused_by_the_modelled_constraint() {
    let emulator = Emulator::new();
    let name = "constrained-domain";
    create_baseline(&emulator, name).await;

    // `GUID` is `@length(min: 36, max: 36)` with the 8-4-4-4-12 hex pattern: each
    // published bound and one past it, plus one row per character class the pattern
    // refuses.
    for dry_run_id in [
        // 35 characters: one short of the bound.
        "00000000-0000-4000-8000-00000000000",
        // 37 characters: one past it.
        "00000000-0000-4000-8000-0000000000000",
        // 36 characters, but not hexadecimal.
        "zzzzzzzz-0000-4000-8000-000000000000",
        // 36 characters, hexadecimal, but not grouped 8-4-4-4-12.
        "000000000-000-4000-8000-000000000000",
    ] {
        let answer = dry_run_progress(&emulator, name, Some(dry_run_id), None).await;
        assert_eq!(
            answer.status, 400,
            "case DescribeDryRunProgress(dryRunId={dry_run_id:?}): expected 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ValidationException",
            "case DescribeDryRunProgress(dryRunId={dry_run_id:?}): expected ValidationException, \
             got {:?}",
            answer.body
        );
    }

    // The bound itself is accepted: 36 hexadecimal characters in the published groups
    // reach the handler, which answers for the run they name.
    let answer = dry_run_progress(&emulator, name, Some(ABSENT_DRY_RUN_ID), None).await;
    assert_eq!(
        answer.status, RECORDED_NOT_FOUND_STATUS,
        "case DescribeDryRunProgress(dryRunId={ABSENT_DRY_RUN_ID:?}): a well-formed id reaches \
         the handler, got {} with {:?}",
        answer.status, answer.body
    );
}

/// RULE: a verbose dry run's pre-update check runs in the call that asks for it, so the
/// record that call mints is TERMINAL from the start — a client reads the service's
/// terminal `DryRunStatus` on its first look, and no later read moves it; and a
/// configuration change the domain really applies supersedes the analyses it was holding,
/// so a request naming one of them afterwards is the operation's own not-found refusal —
/// while a dry run asked for after that change is reportable again.
#[tokio::test]
async fn a_dry_run_is_terminal_from_the_start_and_an_applied_change_supersedes_it() {
    let emulator = Emulator::new();
    // Three of the shared change table's option containers, spanning both deployment
    // kinds and three forms of value: two structures and an enum string.
    let rows: Vec<(&'static str, Value, &'static str)> = change_rows()
        .into_iter()
        .filter(|(option, _, _)| {
            matches!(*option, "ClusterConfig" | "SnapshotOptions" | "IPAddressType")
        })
        .collect();

    for (index, (option, value, deployment)) in rows.into_iter().enumerate() {
        let name = format!("settles-{index}");
        create_baseline(&emulator, &name).await;
        settled_domain(&emulator, &name).await;

        let case = format!("DescribeDryRunProgress({option}, terminal)");
        let started = start_verbose_dry_run(&emulator, &name, json!({ option: value.clone() })).await;
        let dry_run_id = string_member(&started["DryRunProgressStatus"], "DryRunId", &case);
        assert_eq!(
            started["DryRunProgressStatus"]["DryRunStatus"].as_str(),
            Some(SUCCEEDED),
            "case {case}: the call that ran the check reports its conclusion, got {started:?}"
        );

        // The first read a client can make already answers that conclusion, and reading
        // it again answers the same.
        let concluded = progress_body(&emulator, &name, Some(&dry_run_id), None).await;
        let status = &concluded["DryRunProgressStatus"];
        assert_eq!(
            (
                status["DryRunStatus"].as_str(),
                status["DryRunId"].as_str(),
                concluded["DryRunResults"]["DeploymentType"].as_str(),
            ),
            (
                Some(SUCCEEDED),
                Some(dry_run_id.as_str()),
                Some(deployment),
            ),
            "case {case}: expected the same run terminal at {SUCCEEDED}, got {concluded:?}"
        );
        let again = progress_body(&emulator, &name, Some(&dry_run_id), None).await;
        assert_eq!(
            again, concluded,
            "case {case}: the terminal run stays as it is: {concluded:?} then {again:?}"
        );

        // The change the run analysed, this time APPLIED.
        let applied = emulator
            .update_config(&name, json!({ option: value.clone() }))
            .await;
        assert_eq!(
            applied.status, 200,
            "case UpdateDomainConfig({option}, applied): expected 200, got {} with {:?}",
            applied.status, applied.body
        );

        // The run the change superseded addresses nothing any more — by its own id, and
        // as the domain's latest run.
        let case = format!("DescribeDryRunProgress({option}, superseded)");
        for named in [Some(dry_run_id.as_str()), None] {
            let answer = dry_run_progress(&emulator, &name, named, None).await;
            assert_eq!(
                (answer.status, error_code(&answer).as_str()),
                (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
                "case {case} ({named:?}): expected the not-found refusal, got {} with {:?}",
                answer.status,
                answer.body
            );
        }
        let refused = dry_run_progress(&emulator, &name, Some(&dry_run_id), None).await;
        assert_eq!(
            refused.body["message"].as_str(),
            Some(format!("No Dry Run found for the provided DryRunId: {dry_run_id}").as_str()),
            "case {case}: expected the service's sentence for an id that addresses no dry run, \
             got {:?}",
            refused.body
        );

        // And the domain still takes a new analysis: the change closed the runs it
        // superseded, not the capability.
        settled_domain(&emulator, &name).await;
        let again = start_verbose_dry_run(&emulator, &name, json!({ option: value.clone() })).await;
        let next_id = string_member(&again["DryRunProgressStatus"], "DryRunId", &case);
        assert_ne!(
            next_id, dry_run_id,
            "case {case}: a run started after the change is its own, got {again:?}"
        );
        let reported = progress_body(&emulator, &name, Some(&next_id), None).await;
        assert_eq!(
            reported["DryRunProgressStatus"]["DryRunId"].as_str(),
            Some(next_id.as_str()),
            "case {case}: expected the run started after the change to be reportable, got \
             {reported:?}"
        );
    }
}
