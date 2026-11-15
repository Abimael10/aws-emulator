// SPDX-License-Identifier: CC0-1.0

//! Resolving the identifier of an application, an environment or a configuration profile.
//!
//! One concern, in one file: every member of this service's model that ADDRESSES one of
//! those three resources is typed to that resource's own name shape — `ApplicationId` and
//! `EnvironmentId` target `Name` (`@length` 1..64), `ConfigurationProfileId` targets
//! `LongName` (1..128) — and NONE of them carries the `Id` pattern (`^[a-z0-9]{4,7}$`)
//! that the same operations' OUTPUT members carry
//! (`smithy_query(query="operation",operation="GetApplication,GetEnvironment,GetConfigurationProfile")`).
//! So such a member accepts either the logical id the create minted or the name the create
//! supplied, and every operation taking one resolves it the same way: by id first, then by
//! exact name within the scope that holds the resource.
//!
//! The lookups here are that resolution, layered over the by-id lookups the store already
//! had: each answers the stored record (or its id) for whichever form the caller wrote, so
//! an operation reports the resolved `Id` members and refuses only an identifier that is
//! neither an id nor a name.

use super::*;

impl Application {
    /// The id of the environment `identifier` names, by id or by name, for a caller that
    /// goes on to MUTATE the record and so cannot hold the borrow
    /// [`Application::resolve_environment`] hands back.
    pub fn resolve_environment_id(&self, identifier: &str) -> Option<String> {
        self.resolve_environment(identifier)
            .map(|environment| environment.id.clone())
    }

    /// The environment `identifier` names, by id or by name, for a caller that goes on to
    /// write to it. See [`Application::resolve_environment`].
    pub fn resolve_environment_mut(&mut self, identifier: &str) -> Option<&mut Environment> {
        let environment_id = self.resolve_environment_id(identifier)?;
        self.environments.get_mut(&environment_id)
    }

    /// Forget the environment `identifier` names, by id or by name, answering the record
    /// that was removed. The removal itself is [`Application::delete_environment`], which
    /// is what the deployments the environment held move with.
    pub fn delete_resolved_environment(&mut self, identifier: &str) -> Option<Environment> {
        let environment_id = self.resolve_environment_id(identifier)?;
        self.delete_environment(&environment_id)
    }

    /// The id of the configuration profile `identifier` names, by id or by name. See
    /// [`Application::resolve_environment_id`].
    pub fn resolve_profile_id(&self, identifier: &str) -> Option<String> {
        self.resolve_profile(identifier)
            .map(|profile| profile.id.clone())
    }

    /// The configuration profile `identifier` names, by id or by name, for a caller that
    /// goes on to write to it. See [`Application::resolve_profile`].
    pub fn resolve_profile_mut(
        &mut self,
        identifier: &str,
    ) -> Option<&mut ConfigurationProfile> {
        let profile_id = self.resolve_profile_id(identifier)?;
        self.profiles.get_mut(&profile_id)
    }
}

impl ComAmazonawsAppconfigAmazonappconfigState {
    /// The application `identifier` names, by id or by name, for a caller that goes on to
    /// write to the record or to one nested under it. See
    /// [`ComAmazonawsAppconfigAmazonappconfigState::resolve_application`].
    pub fn resolve_application_mut(&mut self, identifier: &str) -> Option<&mut Application> {
        let application_id = self.resolve_application_id(identifier)?;
        self.applications.get_mut(&application_id)
    }

    /// Forget the application `identifier` names, by id or by name, answering the record
    /// that was removed. See
    /// [`ComAmazonawsAppconfigAmazonappconfigState::remove_application`].
    pub fn remove_resolved_application(&mut self, identifier: &str) -> Option<Application> {
        let application_id = self.resolve_application_id(identifier)?;
        self.remove_application(&application_id)
    }

    /// The configuration profile the pair of identifiers addresses, each resolved by id or
    /// by name — the profile within the application that the first identifier resolved to.
    ///
    /// ONE lookup for the whole path, like
    /// [`ComAmazonawsAppconfigAmazonappconfigState::configuration_profile`] it layers over,
    /// so an operation that acts on the profile cannot report the two levels differently.
    pub fn resolve_configuration_profile(
        &self,
        application_identifier: &str,
        configuration_profile_identifier: &str,
    ) -> Option<&ConfigurationProfile> {
        self.resolve_application(application_identifier)?
            .resolve_profile(configuration_profile_identifier)
    }

    /// [`ComAmazonawsAppconfigAmazonappconfigState::resolve_configuration_profile`], for a
    /// caller that goes on to write to the profile.
    pub fn resolve_configuration_profile_mut(
        &mut self,
        application_identifier: &str,
        configuration_profile_identifier: &str,
    ) -> Option<&mut ConfigurationProfile> {
        let application_id = self.resolve_application_id(application_identifier)?;
        self.applications
            .get_mut(&application_id)?
            .resolve_profile_mut(configuration_profile_identifier)
    }

    /// Forget the configuration profile the pair of identifiers addresses, each resolved by
    /// id or by name, answering the record that was removed. See
    /// [`ComAmazonawsAppconfigAmazonappconfigState::remove_configuration_profile`].
    pub fn remove_resolved_configuration_profile(
        &mut self,
        application_identifier: &str,
        configuration_profile_identifier: &str,
    ) -> Option<ConfigurationProfile> {
        let application_id = self.resolve_application_id(application_identifier)?;
        let application = self.applications.get_mut(&application_id)?;
        let profile_id = application.resolve_profile_id(configuration_profile_identifier)?;
        application.profiles.remove(&profile_id)
    }

    /// The id of the environment the pair of identifiers addresses, each resolved by id or
    /// by name: the environment within the application that `application_identifier`
    /// resolved to.
    pub fn resolve_environment_id(
        &self,
        application_identifier: &str,
        environment_identifier: &str,
    ) -> Option<String> {
        self.resolve_application(application_identifier)?
            .resolve_environment_id(environment_identifier)
    }

    /// How a refusal NAMES the application an identifier addressed: the logical id it
    /// resolved to, or the identifier as the request spelled it when it resolved to
    /// nothing.
    ///
    /// Every refusal of this service reports a resolved level by its id and an unresolved
    /// one as it was written, so an operation whose whole path is ONE lookup — the ones
    /// answering [`configuration_profile_not_found_exception`] — words the application level
    /// exactly as an operation that resolves the levels one at a time does.
    pub fn application_label(&self, application_identifier: &str) -> String {
        self.resolve_application_id(application_identifier)
            .unwrap_or_else(|| application_identifier.to_string())
    }
}
