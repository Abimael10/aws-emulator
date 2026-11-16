// SPDX-License-Identifier: CC0-1.0

//! Resolving the environment an operation ACTS on, as opposed to one it
//! reports.
//!
//! `EnvironmentStatus$Terminated` is "Environment is not running": the record
//! stays in the store so the lifecycle reads can still report it
//! (`DescribeEnvironments` with `IncludeDeleted`, the health and resource
//! reads, the event log), but there is no environment left for a call that acts
//! on a running one to act on — its instances and the rest of the resources it
//! was made of are gone. So such a call is refused exactly as one naming an
//! environment that was never created is: the shared not-found wording
//! ([`missing_environment_message`]), naming the member the caller used and
//! reporting no status.
//!
//! An environment still TERMINATING is a different class and deliberately NOT
//! refused here: it EXISTS — `DescribeEnvironments` reports it without being
//! asked for deleted records — so it is refused for its STATUS
//! ([`environment_invalid_state_message`]) by the operation's own guard, not
//! denied as absent.
//!
//! One place writes that rule, so the operations that act on a running
//! environment cannot disagree about which environments they can see.
//!
//! Split out of [`super`], which holds the store the environments live in and
//! the resolution of the `EnvironmentId`/`EnvironmentName` pair every one of
//! them takes ([`super::ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState::environment_id_for`]).

use super::*;

impl ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState {
    /// The id of the environment named by `EnvironmentId` or `EnvironmentName`
    /// for an operation that ACTS on a running environment, or why no such
    /// environment answers to the pair.
    ///
    /// The pair is resolved exactly as every other operation resolves it
    /// ([`Self::environment_id_for`]); what this adds is that an environment
    /// whose termination has COMPLETED is ABSENT to such a call, for the reason
    /// this module states. Each caller maps
    /// [`EnvironmentRefusal::NoIdentifier`] to the code its own model documents.
    pub fn running_environment_id_for(
        &self,
        environment_id: Option<&str>,
        environment_name: Option<&str>,
    ) -> Result<String, EnvironmentRefusal> {
        let resolved = self.environment_id_for(environment_id, environment_name)?;
        let terminated = self
            .environments
            .get(&resolved)
            .is_none_or(Environment::is_terminated);
        if terminated {
            return Err(EnvironmentRefusal::NotFound(missing_environment_message(
                environment_id,
                environment_name,
            )));
        }
        Ok(resolved)
    }
}
