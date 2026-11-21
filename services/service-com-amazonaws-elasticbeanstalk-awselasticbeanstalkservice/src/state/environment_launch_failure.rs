// SPDX-License-Identifier: CC0-1.0

//! Why a launch this service ACCEPTED can still fail, and what the environment
//! then reports.
//!
//! An environment runs on Amazon EC2 instances, and those instances are given
//! the IAM INSTANCE PROFILE the environment's configuration associates with
//! them (`aws:autoscaling:launchconfiguration` / `IamInstanceProfile`, "instance
//! profile that gives the environment's instances the permissions they need").
//! A launch has to have one, and handing a role to a resource it creates is
//! something a service does on the CALLER's behalf, which IAM admits only to a
//! caller allowed to pass that role — so a launch whose configuration
//! associates no instance profile, or whose caller may not pass the one it
//! does, cannot be provisioned, however well-formed the request that asked for
//! it was.
//!
//! That refusal is NOT the call's answer: the launch is accepted, the
//! environment is recorded, and the failure is reported where a client reads
//! the progress of a launch — the environment's own event stream and the status
//! it settles in. So such an environment records the refusal as an `ERROR`
//! event and the clock takes it to `Terminated` ("Environment is not running")
//! instead of to `Ready`, rather than reporting a healthy environment nothing
//! could have provisioned.
//!
//! Both launches of this service go through here ([`super::environment_launch`]
//! builds the record they share): the one `CreateEnvironment` performs from the
//! request's own configuration — resolved through the published option
//! catalogue, so a configuration that associates no profile is launched with
//! that option's published default — and the one `ComposeEnvironments` performs
//! from an environment manifest, whose "name of each environment and other
//! required information" is the whole of what configures the environment it
//! composes: a manifest that associates no instance profile leaves that launch
//! with none at all.
//!
//! WHO may pass a role is IAM's decision, taken against the caller's policies
//! in [`crate::service::authorization`]; what is held here is the instance
//! profile a launch needs, the role it passes for it, and what the environment
//! then reports.

use super::*;

/// The IAM instance profile the instances of an environment whose configuration
/// names none are launched with: the published DEFAULT of
/// `aws:autoscaling:launchconfiguration` / `IamInstanceProfile`, which is the
/// value the catalogue this service publishes reports for that option
/// ([`super::configuration_option_catalog`]) — stated here and read there, so
/// the profile a launch passes and the default a client reads cannot differ.
pub const DEFAULT_INSTANCE_PROFILE: &str = "aws-elasticbeanstalk-ec2-role";

/// The event the clock records when a launch that could not be provisioned
/// reaches its terminal status: Elastic Beanstalk's own statement that the
/// environment failed to launch. WHOLLY FIXED text — the diagnosis is the
/// separate event recorded when the launch failed, which names what refused it.
pub const LAUNCH_FAILED_EVENT: &str = "Failed to launch environment.";

/// The IAM instance profile these configuration options ASSOCIATE with the
/// environment's instances, and `None` for a configuration that associates
/// none: one that does not set
/// `aws:autoscaling:launchconfiguration` / `IamInstanceProfile` at all, and one
/// whose setting carries no value or only blanks, which associates no profile
/// either (the reading [`configuration_advisory_messages`] advises on).
pub fn associated_instance_profile(settings: &[ConfigurationOptionSetting]) -> Option<String> {
    option_value(
        settings,
        NAMESPACE_LAUNCH_CONFIGURATION,
        OPTION_IAM_INSTANCE_PROFILE,
    )
    .map(str::trim)
    .filter(|profile| !profile.is_empty())
    .map(str::to_string)
}

/// The IAM instance profile the instances of an environment with these
/// configuration options are launched with: the one they associate, and
/// [`DEFAULT_INSTANCE_PROFILE`] — that option's own published default — for a
/// configuration that associates none.
pub fn launch_instance_profile(settings: &[ConfigurationOptionSetting]) -> String {
    associated_instance_profile(settings).unwrap_or_else(|| DEFAULT_INSTANCE_PROFILE.to_string())
}

/// The diagnosis a launch whose configuration associates NO IAM instance
/// profile fails with: Elastic Beanstalk's own statement that the environment
/// must have one, naming the configuration option that would carry it and the
/// account role it is taken from otherwise.
///
/// FIXED text apart from those two names, which are this service's own
/// constants rather than values from the request — so the sentence names the
/// option a caller sets to fix it and the role it may create instead.
pub fn missing_instance_profile_diagnosis() -> String {
    format!(
        "Environment must have instance profile associated with it. Please specify profile name \
         in configuration option '{NAMESPACE_LAUNCH_CONFIGURATION}/{OPTION_IAM_INSTANCE_PROFILE}' \
         or add role with name '{DEFAULT_INSTANCE_PROFILE}' to this account"
    )
}

/// The ARN of the IAM role a launch passes to its instances for the instance
/// profile `instance_profile` — the resource `iam:PassRole` is decided on.
///
/// The option takes the instance profile's NAME or its ARN. An
/// `instance-profile/<name>` ARN names the PROFILE, and what is passed is the
/// role that profile carries, so the decision is taken on the role of that
/// name — the way an account's Elastic Beanstalk instance profile holds the
/// role it is named after. Any other ARN is the role itself, as written, and a
/// bare name is resolved in this account's own IAM namespace, where every role
/// of the account lives.
pub fn instance_profile_role_arn(instance_profile: &str) -> String {
    if let Some(profile_name) = ::service_common::iam_role::instance_profile_name(instance_profile)
    {
        return ::engine::emulator_arn("iam", "", &format!("role/{profile_name}"));
    }
    if instance_profile.starts_with("arn:") {
        return instance_profile.to_string();
    }
    ::engine::emulator_arn("iam", "", &format!("role/{instance_profile}"))
}

impl Environment {
    /// Report this freshly launched environment as one that could not be
    /// provisioned: the move the clock owes it is no longer the one to `Ready`
    /// but the one to `Terminated` ("Environment is not running"), recording
    /// that the environment failed to launch.
    ///
    /// The environment keeps the status it is in — a launch that has just been
    /// accepted is `Launching` ("Environment is in the process of initial
    /// deployment"), which is what the accepting call answers — and the failure
    /// becomes observable when the clock makes it due, exactly as a successful
    /// launch's completion does.
    pub fn fail_launch(&mut self, request_id: &str) {
        self.pending_status = Some(PendingStatus {
            due_at: Instant::now() + STATUS_TRANSITION_SPAN,
            due_date: due_date(),
            status: STATUS_TERMINATED.to_string(),
            health: HEALTH_GREY.to_string(),
            // An environment that is not running reports no health status at
            // all, as one the clock has terminated reports none.
            health_status: None,
            abortable_operation_in_progress: false,
            request_id: request_id.to_string(),
            completion_message: LAUNCH_FAILED_EVENT.to_string(),
            completion_severity: SEVERITY_ERROR,
        });
    }
}

impl ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState {
    /// Record that the launch of the environment `environment_id` cannot be
    /// provisioned, for the `diagnosis` that refused it: the environment
    /// carries that diagnosis as an `ERROR` event and is taken to its terminal
    /// status instead of to `Ready` ([`Environment::fail_launch`]).
    ///
    /// `diagnosis` is the refusal as the service that stated it words it — the
    /// authorization failure IAM answers a role it will not let this caller
    /// pass with, or this service's own statement that the environment has no
    /// instance profile at all — so the event stream carries WHY the launch
    /// failed and not only that it did.
    pub fn fail_environment_launch(
        &mut self,
        environment_id: &str,
        diagnosis: impl Into<String>,
        request_id: &str,
    ) {
        let Some(environment) = self.environments.get_mut(environment_id) else {
            return;
        };
        environment.fail_launch(request_id);
        let subject = environment.event_subject();
        self.record_event(request_id, SEVERITY_ERROR, diagnosis, subject);
    }
}
