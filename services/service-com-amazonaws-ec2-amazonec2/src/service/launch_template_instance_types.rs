// SPDX-License-Identifier: CC0-1.0

//! Why this front door does NOT judge a launch template's INSTANCE TYPE at the boundary, and
//! where that value is judged instead.
//!
//! Every other member of this model that names an instance type is held to the modelled
//! `InstanceType` value set by the dispatch-boundary constraint walk, and a value outside it
//! is refused in EC2's own words before any handler runs ([`super::parameter_refusals`]). The
//! launch parameters a LAUNCH TEMPLATE holds are the exception, because a template is a set of
//! launch PARAMETERS rather than a launch: "the error codes and error messages that are
//! returned for the parameters or parameter combinations that are not valid when a new launch
//! template or new version of a launch template is created" (`ValidationWarning`) is the
//! channel such a parameter is answered on, and neither write binds an error for it — so the
//! write stores the type, answers 200 and reports it as a finding
//! ([`crate::state::launch_template_warning`]), while the LAUNCH that tries to use it is what
//! refuses it ([`crate::ops::run_instances`]).
//!
//! That exception is declared where the shape is, as the `skip_validation` on
//! [`crate::ops::types::RequestLaunchTemplateData`], which drops the constraint traits of that
//! one member and of nothing else. This module holds no logic: it is where a reader of this
//! front door finds the reason a refusal that would otherwise be worded here is deliberately
//! absent, and the test below pins BOTH halves of it against the generated schemas, so a
//! regenerated shape that lost the exception — or one that lost a sibling's value set — fails
//! here rather than silently changing what this service refuses.

#[cfg(test)]
mod tests {
    use crate::ops::types::{
        CreateLaunchTemplateInput, CreateLaunchTemplateVersionInput, RequestLaunchTemplateData,
        CREATE_LAUNCH_TEMPLATE_INPUT_SCHEMA, CREATE_LAUNCH_TEMPLATE_VERSION_INPUT_SCHEMA,
    };

    /// A type the published catalog does not hold, in the form a client sends one.
    const UNPUBLISHED_INSTANCE_TYPE: &str = "sim2real.invalid-type";

    /// A value outside the `ShutdownBehavior` catalog, for the sibling member that IS still
    /// judged at the boundary — read off the member's own value set rather than guessed
    /// (`smithy_query(query="shape",shape="ShutdownBehavior")` declares `[stop, terminate]`).
    const UNDECLARED_SHUTDOWN_BEHAVIOR: &str = "explode";

    /// The launch parameters a row sends, naming `instance_type` and `shutdown_behavior`.
    fn launch_parameters(
        instance_type: Option<&str>,
        shutdown_behavior: Option<&str>,
    ) -> RequestLaunchTemplateData {
        RequestLaunchTemplateData {
            image_id: Some("ami-0123456789abcdef0".to_string()),
            instance_type: instance_type.map(str::to_string),
            instance_initiated_shutdown_behavior: shutdown_behavior.map(str::to_string),
            ..RequestLaunchTemplateData::default()
        }
    }

    /// The members the boundary walk reports a violation on, for both writes: each write
    /// carries the same launch parameters, so the two can never be judged differently.
    fn refused_members(data: &RequestLaunchTemplateData) -> Vec<String> {
        let create = CreateLaunchTemplateInput {
            launch_template_name: "probe".to_string(),
            launch_template_data: data.clone(),
            ..CreateLaunchTemplateInput::default()
        };
        let version = CreateLaunchTemplateVersionInput {
            launch_template_name: Some("probe".to_string()),
            launch_template_data: data.clone(),
            ..CreateLaunchTemplateVersionInput::default()
        };
        let walk = |violations: Vec<::aws_smithy_schema_server::Violation>| -> Vec<String> {
            violations
                .into_iter()
                .map(|violation| violation.path)
                .collect()
        };
        let created = walk(::aws_smithy_schema_server::constraints::validate(
            &create,
            CREATE_LAUNCH_TEMPLATE_INPUT_SCHEMA,
        ));
        let versioned = walk(::aws_smithy_schema_server::constraints::validate(
            &version,
            CREATE_LAUNCH_TEMPLATE_VERSION_INPUT_SCHEMA,
        ));
        assert_eq!(
            created, versioned,
            "both launch-template writes carry the same launch parameters, so the boundary \
             must report the same members: got {created:?} from the create and {versioned:?} \
             from the version"
        );
        created
    }

    /// The boundary leaves a launch template's instance type alone and keeps judging every
    /// sibling member of the same parameters.
    #[test]
    fn only_the_instance_type_of_a_launch_templates_parameters_escapes_the_boundary() {
        // (case, the instance type, the shutdown behaviour, whether the walk must refuse it)
        let rows: &[(&str, Option<&str>, Option<&str>, bool)] = &[
            ("a published type", Some("t3.micro"), None, false),
            (
                "a type the catalog does not hold",
                Some(UNPUBLISHED_INSTANCE_TYPE),
                None,
                false,
            ),
            (
                "a published type in another case",
                Some("T3.MICRO"),
                None,
                false,
            ),
            ("no instance type at all", None, None, false),
            (
                "a sibling member outside its own value set",
                Some("t3.micro"),
                Some(UNDECLARED_SHUTDOWN_BEHAVIOR),
                true,
            ),
            (
                "that sibling beside an unknown type: the sibling still decides",
                Some(UNPUBLISHED_INSTANCE_TYPE),
                Some(UNDECLARED_SHUTDOWN_BEHAVIOR),
                true,
            ),
            (
                "the sibling's own declared value",
                Some(UNPUBLISHED_INSTANCE_TYPE),
                Some("terminate"),
                false,
            ),
        ];

        for (case, instance_type, shutdown_behavior, refused) in rows {
            let members = refused_members(&launch_parameters(*instance_type, *shutdown_behavior));
            assert_eq!(
                !members.is_empty(),
                *refused,
                "case {case:?}: expected the boundary to refuse these parameters = {refused}, \
                 got the violations {members:?}"
            );
            assert!(
                !members
                    .iter()
                    .any(|member| member.ends_with("InstanceType")),
                "case {case:?}: a launch template's instance type is reported as a finding on \
                 a 200, never refused at the boundary, got the violations {members:?}"
            );
        }
    }
}
