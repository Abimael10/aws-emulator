// SPDX-License-Identifier: CC0-1.0

//! The SERVICE's OWN write into a thing's reserved `$package` named shadow: the package
//! version reporting a finished IoT job performs.
//!
//! An account turns the reporting on through the IoT control plane
//! (`iot:UpdatePackageConfiguration$versionUpdateByJobsConfig`, "Configuration to manage
//! IoT Job's package version reporting. If configured, Jobs updates the thing's reserved
//! named shadow with the package version information up on successful job completion"),
//! and a job names what it installs
//! (`iot:CreateJobRequest$destinationPackageVersions`, "The package version Amazon
//! Resource Names (ARNs) that are installed on the device when the job successfully
//! completes"). Nothing a CLIENT calls writes that shadow: the service writes it when a
//! targeted thing's job execution reaches `SUCCEEDED`.
//!
//! The write lives in THIS crate because this is the crate that owns the shadow store —
//! the same store `UpdateThingShadow` writes and `GetThingShadow` reads — so what the
//! service reports and what a device reads back are one record, merged by the one
//! [`super::Shadow::apply`] every shadow write goes through and announced on the shadow's
//! own reserved topics like any other accepted update. The plane that moves an execution
//! to its terminal status calls [`report_job_package_versions`] once that status is
//! recorded.

use ::serde_json::{Map, Value};

/// The named shadow a thing's software package versions are reported into.
///
/// AWS IoT reserves it for exactly that feature, which is why this service serves it to
/// clients as well ([`super::SERVED_RESERVED_SHADOW_NAMES`]) rather than refusing the
/// reserved namespace outright.
pub const PACKAGE_SHADOW_NAME: &str = "$package";

/// The member a reported package entry carries the installed version under:
/// `state.reported.<packageName>.version`.
const VERSION_MEMBER: &str = "version";

/// Report the package versions a finished job installed on `thing_name`, when the
/// account has package-version reporting by jobs turned on.
///
/// Answers nothing: the plane that drove the execution to its terminal status has already
/// answered its own caller, and this report is the SERVICE's own act rather than part of
/// that answer — a report nothing was configured for, and one a store ceiling refused
/// (logged where that is decided), are both invisible to the device either way.
/// Everything it needs comes from state
/// the two IoT planes already hold: whether the reporting is on, from the control plane's
/// package configuration; which packages the job installs, from the SHARED job record's
/// `destinationPackageVersions`; and the package and version names, read out of each of
/// those ARNs
/// ([`service_common::iot_jobs::package_version_from_arn`]). A job that names none, an
/// account that has not turned the reporting on, and an ARN that names no package version
/// each report nothing — none of them is a failure of the execution update that triggered
/// this.
pub fn report_job_package_versions<Eng: Send + Sync + 'static>(
    engine: &Eng,
    thing_name: &str,
    job_id: &str,
) {
    // "If configured, Jobs updates the thing's reserved named shadow …": the account's
    // own configuration decides whether the reporting happens at all, and it lives in
    // the control plane's store — read, never written, from here.
    let configured = ::service_com_amazonaws_iot_awsiotservice::state::service_state(engine)
        .is_some_and(|iot| iot.lock().package_configuration.version_update_by_jobs_enabled);
    if !configured {
        return;
    }

    // The job's package association, from the SHARED jobs store both planes read.
    let Some(jobs) = ::service_com_amazonaws_iot_awsiotservice::state::job_store(engine) else {
        return;
    };
    let installed: Vec<String> = {
        let registry = jobs.lock();
        match registry.job(job_id) {
            Some(job) => job.destination_package_versions.clone(),
            None => Vec::new(),
        }
    };

    let mut reported = Map::new();
    for arn in &installed {
        // "the shadow document's state.reported gains an entry keyed by the package
        // name whose version member is the version the ARN names": the names are the
        // ARN's, so a job and the shadow it reports into cannot name different packages.
        if let Some((package_name, version_name)) =
            ::service_common::iot_jobs::package_version_from_arn(arn)
        {
            reported.insert(
                package_name.to_string(),
                Value::Object(Map::from_iter([(
                    VERSION_MEMBER.to_string(),
                    Value::from(version_name),
                )])),
            );
        }
    }
    if reported.is_empty() {
        return;
    }

    merge_reported_state(
        engine,
        thing_name,
        PACKAGE_SHADOW_NAME,
        &Value::Object(reported),
    );
}

/// Merge `reported` into the `reported` section of `thing_name`'s `shadow_name` shadow as
/// the SERVICE, and announce the accepted update on that shadow's reserved topics.
///
/// The write itself is the ONE every shadow write goes through
/// ([`super::shadow_writes::apply_shadow_write`]), so the service's own report is held to
/// exactly the ceilings a client's `UpdateThingShadow` is held to and merges by the same
/// rules; the announcement is the one a client's accepted update makes
/// ([`super::shadow_topics::announce_update`]). A shadow the service wrote is therefore
/// indistinguishable from one a device reported — the same record, read back by the same
/// `GetThingShadow`.
///
/// A write a store ceiling refuses leaves the stored shadow exactly as it was and is
/// LOGGED here: this call answers no client, so a refusal has no response to travel in,
/// and a report that was dropped would otherwise leave no trace at all.
fn merge_reported_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
    thing_name: &str,
    shadow_name: &str,
    reported: &Value,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let key = super::shadow_key(thing_name, Some(shadow_name));
    let mut guard = state.lock();
    let written = match super::shadow_writes::apply_shadow_write(
        &mut guard,
        &key,
        None,
        Some(reported),
        super::now_secs(),
    ) {
        Ok(written) => written,
        Err(refusal) => {
            ::tracing::warn!(
                thing_name,
                shadow_name,
                ?refusal,
                "the package version report was not stored"
            );
            return;
        }
    };
    // This service's store is released before the broker's is taken: the announcement is
    // a publication, and no store is held across the fan-out.
    drop(guard);

    super::shadow_topics::announce_update(
        engine,
        thing_name,
        Some(shadow_name),
        &super::shadow_topics::AcceptedUpdate {
            accepted: &written.stored_document,
            previous: written.previous.as_ref(),
            previous_version: written.previous_version,
            current: &written.current,
            version: written.version,
            timestamp_secs: written.timestamp_secs,
            // The service's own write answers no caller, so there is no correlation
            // token to echo on it.
            client_token: None,
        },
    );
}
