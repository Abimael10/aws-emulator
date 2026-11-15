// SPDX-License-Identifier: CC0-1.0

//! The container-instance capabilities a task-definition revision requires, derived from
//! the stored revision and reported through `TaskDefinition$requiresAttributes` ("The
//! container instance attributes required by your task").
//!
//! Every feature a revision declares that only a container instance publishing a
//! capability can serve contributes that capability's attribute, and the list is reported
//! in the order Amazon ECS derives it: the capabilities each CONTAINER's own features need,
//! container by container and feature by feature, then the ones the revision as a whole
//! needs — the task role's, the execution role's image pull, and the task-networking pair
//! last. A capability two features need is reported ONCE, where it was first derived.

use super::*;

/// The capability a container instance publishes when its agent can authenticate an image
/// pull against Amazon ECR, as the agent publishes it (Amazon ECS Developer Guide, the
/// `com.amazonaws.ecs.capability.ecr-auth` attribute): a revision whose containers run an
/// image a registry serves only to an authenticated principal
/// ([`needs_execution_role_to_pull`]) cannot be placed on an instance without it.
pub const ATTRIBUTE_ECR_AUTH: &str = "com.amazonaws.ecs.capability.ecr-auth";

/// The capability an agent publishes when it can make that pull AS the task execution role
/// (`ecs.capability.execution-role-ecr-pull`) — required by a revision that both runs such
/// an image and names the role the pull is made as
/// (`TaskDefinition$executionRoleArn`: "the task execution role that grants the Amazon ECS
/// container agent permission to make Amazon Web Services API calls on your behalf").
pub const ATTRIBUTE_EXECUTION_ROLE_ECR_PULL: &str = "ecs.capability.execution-role-ecr-pull";

/// The Docker remote-API version that carries log-driver OPTIONS, as the agent publishes
/// its API versions (`com.amazonaws.ecs.capability.docker-remote-api.<version>`): a
/// container whose `logConfiguration` passes `options` to its driver needs an instance
/// speaking it, beside the version that carries the driver itself
/// ([`ATTRIBUTE_DOCKER_REMOTE_API_1_18`]).
pub const ATTRIBUTE_DOCKER_REMOTE_API_1_19: &str =
    "com.amazonaws.ecs.capability.docker-remote-api.1.19";

/// Whether `value_from` names a Secrets Manager secret rather than a Systems Manager
/// parameter. The `Secret$valueFrom` doc: "the full ARN of the Secrets Manager secret or
/// the full ARN or name of the Systems Manager Parameter Store parameter", so the
/// service that holds it is the ARN's own service segment.
pub(super) fn is_secrets_manager_reference(value_from: &str) -> bool {
    value_from
        .split(':')
        .nth(2)
        .is_some_and(|service| service == "secretsmanager")
}

/// Append `name` to `out` unless the list already requires that capability: one attribute
/// is reported once, at the position the feature that first needed it put it.
fn require(out: &mut Vec<String>, name: String) {
    if !out.contains(&name) {
        out.push(name);
    }
}

/// The container-instance attributes ONE container of a revision requires, appended to
/// `out`. `has_execution_role` is whether the REVISION carries an `executionRoleArn`,
/// which some container features need beside themselves. See
/// [`TaskDefinitionRecord::requires_attributes`] for the rule.
pub(super) fn container_required_attributes(
    container: &ContainerDefinition,
    has_execution_role: bool,
    out: &mut Vec<String>,
) {
    if let Some(logging) = container.log_configuration.as_ref() {
        // Configuring a log driver at all needs the Docker remote-API version that
        // carries a log configuration, and passing the driver its own `options` the
        // version that carries those ([`ATTRIBUTE_DOCKER_REMOTE_API_1_19`]) — so a
        // container that names a driver and nothing else needs only the first.
        require(out, ATTRIBUTE_DOCKER_REMOTE_API_1_18.to_string());
        require(
            out,
            format!("{ATTRIBUTE_LOGGING_DRIVER_PREFIX}{}", logging.log_driver),
        );
        if logging
            .options
            .as_ref()
            .is_some_and(|options| !options.is_empty())
        {
            require(out, ATTRIBUTE_DOCKER_REMOTE_API_1_19.to_string());
        }
        if has_execution_role && logging.log_driver == LOG_DRIVER_AWSLOGS {
            require(out, ATTRIBUTE_EXECUTION_ROLE_AWSLOGS.to_string());
        }
        for secret in logging.secret_options.iter().flatten() {
            require(
                out,
                if is_secrets_manager_reference(&secret.value_from) {
                    ATTRIBUTE_SECRETS_ASM_LOG_DRIVER
                } else {
                    ATTRIBUTE_SECRETS_SSM_LOG_DRIVER
                }
                .to_string(),
            );
        }
    }
    // An image a registry serves only to an authenticated principal is fetched with
    // credentials the agent has to obtain for it, which is its own capability.
    if container
        .image
        .as_deref()
        .is_some_and(|image| needs_execution_role_to_pull(::std::iter::once(image)))
    {
        require(out, ATTRIBUTE_ECR_AUTH.to_string());
    }
    for secret in container.secrets.iter().flatten() {
        require(
            out,
            if is_secrets_manager_reference(&secret.value_from) {
                ATTRIBUTE_SECRETS_ASM_ENVIRONMENT
            } else {
                ATTRIBUTE_SECRETS_SSM_ENVIRONMENT
            }
            .to_string(),
        );
    }
    for file in container.environment_files.iter().flatten() {
        if file.r#type.eq_ignore_ascii_case("s3") {
            require(out, ATTRIBUTE_ENV_FILES_S3.to_string());
        }
    }
    if container.privileged == Some(true) {
        require(out, ATTRIBUTE_PRIVILEGED_CONTAINER.to_string());
    }
    if let Some(firelens) = container.firelens_configuration.as_ref() {
        require(out, format!("{ATTRIBUTE_FIRELENS_PREFIX}{}", firelens.r#type));
    }
}

impl TaskDefinitionRecord {
    /// The container-instance attributes a task of this revision requires —
    /// `TaskDefinition$requiresAttributes`: "The container instance attributes required
    /// by your task."
    ///
    /// DERIVED from the stored revision: every feature it declares that only a container
    /// instance publishing a capability can serve contributes that capability's
    /// attribute — a container's `logConfiguration` needs the Docker remote-API version
    /// that carries one, the attribute of the driver it names, the version that carries
    /// driver `options` when it passes any ([`ATTRIBUTE_DOCKER_REMOTE_API_1_19`]) and,
    /// for the `awslogs` driver on a revision that also carries an `executionRoleArn`,
    /// [`ATTRIBUTE_EXECUTION_ROLE_AWSLOGS`]; an image no registry serves unauthenticated
    /// needs [`ATTRIBUTE_ECR_AUTH`]; a `secrets` or `secretOptions` reference needs the
    /// attribute of the secret store it names; an S3 `environmentFiles` entry needs
    /// [`ATTRIBUTE_ENV_FILES_S3`]; a privileged container needs
    /// [`ATTRIBUTE_PRIVILEGED_CONTAINER`]; a `firelensConfiguration` needs the log-router
    /// capability of the router kind it names ([`ATTRIBUTE_FIRELENS_PREFIX`]); a
    /// `taskRoleArn` needs [`ATTRIBUTE_TASK_IAM_ROLE`]; such an image TOGETHER with an
    /// `executionRoleArn` needs [`ATTRIBUTE_EXECUTION_ROLE_ECR_PULL`], the pull made as
    /// that role; and the `awsvpc` network mode needs [`ATTRIBUTE_TASK_ENI`] with the
    /// Docker remote-API version that carries task networking. Each name is FIXED text
    /// completed only by the feature it is derived from, never by the account, the family
    /// or the revision. The names are the Amazon ECS container-agent capabilities, which
    /// is what an instance registers them as, so
    /// `ListAttributes`/`DescribeContainerInstances` and this list speak the same
    /// vocabulary.
    ///
    /// Reported in the order the derivation takes them (see the module docs) and
    /// deduplicated, so a revision reads back one entry per capability. A revision that
    /// declares no such feature requires nothing and the member stays ABSENT (`None`)
    /// rather than reporting an empty list.
    pub fn requires_attributes(&self) -> Option<Vec<Attribute>> {
        let mut names = Vec::new();
        let has_execution_role = self.execution_role_arn.is_some();
        for container in &self.container_definitions {
            container_required_attributes(container, has_execution_role, &mut names);
        }
        // A revision that names a TASK role has the agent serve that role's credentials to
        // its containers, which is its own capability ([`ATTRIBUTE_TASK_IAM_ROLE`]) — so a
        // revision that names none requires nothing of the kind.
        if self.task_role_arn.is_some() {
            require(&mut names, ATTRIBUTE_TASK_IAM_ROLE.to_string());
        }
        // The pull of such an image is made AS the execution role where the revision names
        // one, which is a capability of its own beside authenticating the pull at all.
        if has_execution_role && self.pull_needs_authentication() {
            require(&mut names, ATTRIBUTE_EXECUTION_ROLE_ECR_PULL.to_string());
        }
        if self.network_mode == NETWORK_MODE_AWSVPC {
            require(&mut names, ATTRIBUTE_DOCKER_REMOTE_API_1_18.to_string());
            require(&mut names, ATTRIBUTE_TASK_ENI.to_string());
        }
        if names.is_empty() {
            return None;
        }
        Some(
            names
                .into_iter()
                .map(|name| Attribute {
                    name,
                    ..Attribute::default()
                })
                .collect(),
        )
    }

    /// Whether an image this revision's containers run is one a registry serves only to an
    /// authenticated principal ([`needs_execution_role_to_pull`]), whichever role the
    /// revision names for the pull.
    ///
    /// The one question two of this revision's derived answers put: WHICH capabilities the
    /// pull needs ([`TaskDefinitionRecord::requires_attributes`]) and whether a launch type
    /// with no container instance to fall back on can run the revision at all
    /// ([`TaskDefinitionRecord::pull_needs_execution_role`]).
    pub fn pull_needs_authentication(&self) -> bool {
        needs_execution_role_to_pull(
            self.container_definitions
                .iter()
                .filter_map(|container| container.image.as_deref()),
        )
    }
}
