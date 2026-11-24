// SPDX-License-Identifier: CC0-1.0

//! The ONE member list of `EnvironmentDescription`, for the three structures the
//! scaffold emits that shape as.
//!
//! The model binds `EnvironmentDescription` as the output of `CreateEnvironment`,
//! `UpdateEnvironment` and `TerminateEnvironment` and as the element of
//! `DescribeEnvironments`/`ComposeEnvironments`' list, and the scaffold emits a
//! DISTINCT Rust struct per operation for it. The WIRE carries two members the
//! on-disk model does not declare, so none of those structs can be derived with
//! `#[smithy_macros::smithy(shape = ...)]` any more (see
//! `types::unmodelled_member_wire` for why a derive admits exactly the model's
//! member list). The macro below states the member list ONCE and emits a struct
//! and its codec from it, so the three structs cannot drift apart and the
//! unmodelled members are carried by each of them.
//!
//! The two unmodelled members, in the order they are appended after the model's
//! own:
//!
//! * `MinCapacityEnabled` — whether a minimum-capacity configuration is in force
//!   for the environment. RECORDED BEHAVIOUR: the real service emits the boolean
//!   for every described environment, `false` for one with no such
//!   configuration.
//! * `Alerts` — the alerts raised against the environment. RECORDED BEHAVIOUR:
//!   the real service emits the list element for every described environment,
//!   empty for an environment with none. The on-disk model declares neither the
//!   member nor an element shape for it, and the recording carries no element, so
//!   the list is declared over the only rendering an element could have on the
//!   awsQuery wire — text.
//!
//! Which values each carries is [`crate::state::environment_description`]'s, read
//! off the stored environment like every other member; this module owns the WIRE
//! shape alone.

/// Declare one of the generated `EnvironmentDescription` structures and the
/// schema, serializer and deserializer for it.
///
/// `$ty` is the struct name the scaffold gave that operation's output and
/// `$schema_static` the `<STRUCT>_SCHEMA` static the operation's `impl Operation`
/// names (the same name the derive would have emitted). The member list below is
/// the model's own, in the order
/// `smithy_query(query="shape", shape="EnvironmentDescription")` lists it, with
/// the unmodelled two appended last.
macro_rules! eb_environment_description {
    ($ty:ident, $schema_static:ident) => {
        #[derive(Debug, Clone, Default)]
        pub struct $ty {
            pub environment_name: Option<String>,
            pub environment_id: Option<String>,
            pub application_name: Option<String>,
            pub version_label: Option<String>,
            pub solution_stack_name: Option<String>,
            pub platform_arn: Option<String>,
            pub template_name: Option<String>,
            pub description: Option<String>,
            pub endpoint_url: Option<String>,
            pub cname: Option<String>,
            pub date_created: Option<::aws_smithy_types::DateTime>,
            pub date_updated: Option<::aws_smithy_types::DateTime>,
            pub status: Option<String>,
            pub abortable_operation_in_progress: Option<bool>,
            pub health: Option<String>,
            pub health_status: Option<String>,
            pub resources: Option<EnvironmentResourcesDescription>,
            pub tier: Option<EnvironmentTier>,
            pub environment_links: Option<Vec<EnvironmentLink>>,
            pub environment_arn: Option<String>,
            pub operations_role: Option<String>,
            /// Unmodelled; see this module's header.
            pub min_capacity_enabled: Option<bool>,
            /// Unmodelled; see this module's header.
            pub alerts: Option<Vec<String>>,
        }

        eb_schema_and_codec!(
            $ty,
            $schema_static,
            "com.amazonaws.elasticbeanstalk#EnvironmentDescription",
            "com.amazonaws.elasticbeanstalk",
            "EnvironmentDescription",
            (
                M_ENVIRONMENT_NAME,
                environment_name,
                "EnvironmentName",
                0,
                opt,
                string
            ),
            (
                M_ENVIRONMENT_ID,
                environment_id,
                "EnvironmentId",
                1,
                opt,
                string
            ),
            (
                M_APPLICATION_NAME,
                application_name,
                "ApplicationName",
                2,
                opt,
                string
            ),
            (
                M_VERSION_LABEL,
                version_label,
                "VersionLabel",
                3,
                opt,
                string
            ),
            (
                M_SOLUTION_STACK_NAME,
                solution_stack_name,
                "SolutionStackName",
                4,
                opt,
                string
            ),
            (M_PLATFORM_ARN, platform_arn, "PlatformArn", 5, opt, string),
            (
                M_TEMPLATE_NAME,
                template_name,
                "TemplateName",
                6,
                opt,
                string
            ),
            (M_DESCRIPTION, description, "Description", 7, opt, string),
            (M_ENDPOINT_URL, endpoint_url, "EndpointURL", 8, opt, string),
            (M_CNAME, cname, "CNAME", 9, opt, string),
            (
                M_DATE_CREATED,
                date_created,
                "DateCreated",
                10,
                opt,
                date_time
            ),
            (
                M_DATE_UPDATED,
                date_updated,
                "DateUpdated",
                11,
                opt,
                date_time
            ),
            (M_STATUS, status, "Status", 12, opt, string),
            (
                M_ABORTABLE_OPERATION_IN_PROGRESS,
                abortable_operation_in_progress,
                "AbortableOperationInProgress",
                13,
                opt,
                boolean
            ),
            (M_HEALTH, health, "Health", 14, opt, string),
            (
                M_HEALTH_STATUS,
                health_status,
                "HealthStatus",
                15,
                opt,
                string
            ),
            (
                M_RESOURCES,
                resources,
                "Resources",
                16,
                opt,
                struct_of(EnvironmentResourcesDescription)
            ),
            (M_TIER, tier, "Tier", 17, opt, struct_of(EnvironmentTier)),
            (
                M_ENVIRONMENT_LINKS,
                environment_links,
                "EnvironmentLinks",
                18,
                opt,
                list_of_struct(EnvironmentLink)
            ),
            (
                M_ENVIRONMENT_ARN,
                environment_arn,
                "EnvironmentArn",
                19,
                opt,
                string
            ),
            (
                M_OPERATIONS_ROLE,
                operations_role,
                "OperationsRole",
                20,
                opt,
                string
            ),
            (
                M_MIN_CAPACITY_ENABLED,
                min_capacity_enabled,
                "MinCapacityEnabled",
                21,
                opt,
                boolean
            ),
            (M_ALERTS, alerts, "Alerts", 22, opt, list_of_string),
        );
    };
}
