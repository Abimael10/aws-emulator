// SPDX-License-Identifier: CC0-1.0

//! How an UPDATE of an image pipeline is held.
//!
//! `UpdateImagePipeline` "does not support selective updates. The request replaces the
//! pipeline's entire configuration, so include every setting that you want to keep. Any
//! optional property that you omit is removed or reset to its default" — so an update does
//! not merge into the stored record, it REPLACES it: everything the request describes is
//! written as submitted, every optional property it omits is cleared or set to the value
//! the service documents for it, and the members an update request cannot carry (the name
//! the ARN was derived from, the pipeline's own tags, the instant it was created) are what
//! the pipeline keeps.
//!
//! One member is neither of those: the platform is "inherited from the recipe that the
//! pipeline uses", so a replacement that names another recipe reports the platform of THAT
//! recipe, read the same way the create read it.
//!
//! Beside the record it carries the two things the operation's own `clientToken` needs,
//! kept apart from the create's: an update is a different operation, so a token spent on
//! one never answers for the other ([`UPDATE_IMAGE_PIPELINE_OPERATION`]).
//!
//! The two renderings at the bottom are shared with the CREATE's fingerprint: the two
//! requests carry the same `schedule` and `loggingConfiguration` members, so one rendering
//! of each serves both and the two cannot disagree about what makes two requests different.

use super::*;

use crate::ops::types::UpdateImagePipelineInput;

/// See [`CREATE_IMAGE_PIPELINE_OPERATION`]. An update carries a `clientToken` of its own,
/// so its receipts are filed under its own name and never read the create's: the same
/// token may create a pipeline and then update it.
pub const UPDATE_IMAGE_PIPELINE_OPERATION: &str = "UpdateImagePipeline";

impl ComAmazonawsImagebuilderImagebuilderState {
    /// Replace the stored pipeline with `record` and file the receipt for the update that
    /// wrote it.
    ///
    /// The key is the record's own ARN, which an update never changes — the ARN carries
    /// the normalized name and an update cannot carry a name — so the replacement takes
    /// the place of what it replaced rather than adding a second entry.
    pub fn replace_image_pipeline(
        &mut self,
        record: ImagePipelineRecord,
        client_token: &str,
        fingerprint: String,
        request_id: String,
    ) {
        self.file_create_receipt(
            UPDATE_IMAGE_PIPELINE_OPERATION,
            &record.region,
            client_token,
            fingerprint,
            request_id,
            record.arn.clone(),
        );
        self.image_pipelines.insert(record.arn.clone(), record);
    }

    /// The receipt `client_token` already earned from `UpdateImagePipeline` in
    /// `request_region`, if any.
    #[must_use]
    pub fn image_pipeline_update_receipt(
        &self,
        request_region: &str,
        client_token: &str,
    ) -> Option<&CreateReceipt> {
        self.create_receipt(
            UPDATE_IMAGE_PIPELINE_OPERATION,
            request_region,
            client_token,
        )
    }
}

impl ImagePipelineRecord {
    /// Whether a request naming these two recipe references names a recipe of the same
    /// KIND as the one this pipeline currently builds.
    ///
    /// "The recipe must be the same type, image or container, as the pipeline's current
    /// recipe" (`UpdateImagePipeline`): a pipeline holds exactly one of the two
    /// references, so the kinds agree when the request fills the same one the record
    /// holds.
    #[must_use]
    pub fn admits_recipe_kind(
        &self,
        image_recipe_arn: Option<&str>,
        container_recipe_arn: Option<&str>,
    ) -> bool {
        self.image_recipe_arn.is_some() == image_recipe_arn.is_some()
            && self.container_recipe_arn.is_some() == container_recipe_arn.is_some()
    }
}

/// Why an update naming a recipe of the other kind than the pipeline's own is refused.
///
/// The rule is `UpdateImagePipeline`'s own: "The recipe must be the same type, image or
/// container, as the pipeline's current recipe."
#[must_use]
pub fn recipe_kind_mismatch_message() -> String {
    "The recipe must be the same type, image or container, as the pipeline's current \
     recipe."
        .to_string()
}

/// A deterministic rendering of the settings one `UpdateImagePipeline` request carries, so
/// a retry of it can be told from a different request reusing its `clientToken`.
///
/// `clientToken` is left out, for the reason
/// [`infrastructure_configuration_request_fingerprint`] leaves it out: it is the key this
/// rendering is compared under. The ARN is part of it — the same token aimed at a
/// different pipeline is a different request.
#[must_use]
pub fn image_pipeline_update_fingerprint(input: &UpdateImagePipelineInput) -> String {
    let mut fields: BTreeMap<&'static str, String> = BTreeMap::new();
    fields.insert(
        "imagePipelineArn",
        text(Some(input.image_pipeline_arn.as_str())),
    );
    fields.insert("description", text(input.description.as_deref()));
    fields.insert("imageRecipeArn", text(input.image_recipe_arn.as_deref()));
    fields.insert(
        "containerRecipeArn",
        text(input.container_recipe_arn.as_deref()),
    );
    fields.insert(
        "infrastructureConfigurationArn",
        text(Some(input.infrastructure_configuration_arn.as_str())),
    );
    fields.insert(
        "distributionConfigurationArn",
        text(input.distribution_configuration_arn.as_deref()),
    );
    fields.insert("status", text(input.status.as_deref()));
    fields.insert("executionRole", text(input.execution_role.as_deref()));
    fields.insert(
        "enhancedImageMetadataEnabled",
        flag(input.enhanced_image_metadata_enabled),
    );
    fields.insert("imageTags", tag_map(input.image_tags.as_ref()));
    fields.insert(
        "imageTestsConfiguration",
        image_tests_fingerprint(input.image_tests_configuration.as_ref()),
    );
    fields.insert(
        "imageScanningConfiguration",
        image_scanning_fingerprint(input.image_scanning_configuration.as_ref()),
    );
    fields.insert("schedule", schedule_fingerprint(input.schedule.as_ref()));
    fields.insert(
        "loggingConfiguration",
        pipeline_logging_fingerprint(input.logging_configuration.as_ref()),
    );
    fields.insert("workflows", workflows_fingerprint(input.workflows.as_deref()));
    fields
        .iter()
        .map(|(member, rendered)| format!("{member}={rendered};"))
        .collect()
}

/// The `schedule` member, as both pipeline fingerprints render it: the expression, the
/// timezone, the start condition and the failure count its auto-disable policy carries.
pub(crate) fn schedule_fingerprint(schedule: Option<&Schedule>) -> String {
    schedule.map_or_else(
        || ABSENT.to_string(),
        |schedule| {
            group(&[
                text(schedule.schedule_expression.as_deref()),
                text(schedule.timezone.as_deref()),
                text(schedule.pipeline_execution_start_condition.as_deref()),
                schedule.auto_disable_policy.as_ref().map_or_else(
                    || ABSENT.to_string(),
                    |policy| number(Some(policy.failure_count)),
                ),
            ])
        },
    )
}

/// The `loggingConfiguration` member of a PIPELINE, as both pipeline fingerprints render
/// it: the two log group names it names.
pub(crate) fn pipeline_logging_fingerprint(
    logging: Option<&PipelineLoggingConfiguration>,
) -> String {
    logging.map_or_else(
        || ABSENT.to_string(),
        |logging| {
            group(&[
                text(logging.image_log_group_name.as_deref()),
                text(logging.pipeline_log_group_name.as_deref()),
            ])
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One stored pipeline holding `image_recipe_arn` and `container_recipe_arn`, which is
    /// all the recipe-kind rule reads.
    fn pipeline(
        image_recipe_arn: Option<&str>,
        container_recipe_arn: Option<&str>,
    ) -> ImagePipelineRecord {
        ImagePipelineRecord {
            arn: "arn:aws:imagebuilder:us-west-2:000000000000:image-pipeline/p".to_string(),
            region: "us-west-2".to_string(),
            name: "p".to_string(),
            description: None,
            platform: None,
            enhanced_image_metadata_enabled: true,
            image_recipe_arn: image_recipe_arn.map(str::to_string),
            container_recipe_arn: container_recipe_arn.map(str::to_string),
            infrastructure_configuration_arn:
                "arn:aws:imagebuilder:us-west-2:000000000000:infrastructure-configuration/c"
                    .to_string(),
            distribution_configuration_arn: None,
            image_tests_configuration: ImageTestsConfiguration {
                image_tests_enabled: Some(DEFAULT_IMAGE_TESTS_ENABLED),
                timeout_minutes: Some(DEFAULT_IMAGE_TESTS_TIMEOUT_MINUTES),
            },
            schedule: None,
            status: PIPELINE_STATUS_ENABLED.to_string(),
            tags: None,
            image_tags: None,
            image_scanning_configuration: None,
            execution_role: None,
            workflows: None,
            logging_configuration: None,
            date_created: "2026-01-01T00:00:00Z".to_string(),
            date_updated: None,
        }
    }

    /// One row of the recipe-kind table: the case, the two references the pipeline holds,
    /// the two the request submits, and whether the replacement is admitted.
    type RecipeKindRow = (
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        bool,
    );

    /// One row of the fingerprint table: the case and how that row alters the request.
    type AlteredRequestRow = (&'static str, fn(&mut UpdateImagePipelineInput));

    /// RULE: a replacement's recipe is admitted only when it fills the same one of the two
    /// references the stored pipeline holds.
    #[test]
    fn a_replacement_keeps_the_pipelines_recipe_kind() {
        const IMAGE: &str = "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/r/1.0.0";
        const CONTAINER: &str =
            "arn:aws:imagebuilder:us-west-2:000000000000:container-recipe/r/1.0.0";
        let rows: &[RecipeKindRow] = &[
            (
                "an image pipeline given another image recipe",
                Some(IMAGE),
                None,
                Some(IMAGE),
                None,
                true,
            ),
            (
                "an image pipeline given a container recipe",
                Some(IMAGE),
                None,
                None,
                Some(CONTAINER),
                false,
            ),
            (
                "a container pipeline given another container recipe",
                None,
                Some(CONTAINER),
                None,
                Some(CONTAINER),
                true,
            ),
            (
                "a container pipeline given an image recipe",
                None,
                Some(CONTAINER),
                Some(IMAGE),
                None,
                false,
            ),
        ];
        for (case, held_image, held_container, submitted_image, submitted_container, expected) in
            rows
        {
            let actual = pipeline(*held_image, *held_container)
                .admits_recipe_kind(*submitted_image, *submitted_container);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: two requests differing in any setting render differently, and two requests
    /// carrying the same settings render alike — which is what tells a retry from a
    /// different request reusing one token.
    #[test]
    fn the_update_fingerprint_separates_requests_that_differ() {
        let base = UpdateImagePipelineInput {
            image_pipeline_arn: "arn:aws:imagebuilder:us-west-2:000000000000:image-pipeline/p"
                .to_string(),
            infrastructure_configuration_arn:
                "arn:aws:imagebuilder:us-west-2:000000000000:infrastructure-configuration/c"
                    .to_string(),
            image_recipe_arn: Some(
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/r/1.0.0".to_string(),
            ),
            client_token: "token".to_string(),
            ..UpdateImagePipelineInput::default()
        };
        let rendered = image_pipeline_update_fingerprint(&base);

        let mut retried = base.clone();
        // The token is the key the rendering is compared under, so it is not part of it.
        retried.client_token = "another-token".to_string();
        let actual = image_pipeline_update_fingerprint(&retried);
        assert_eq!(
            actual, rendered,
            "case \"the same settings under another token\": expected {rendered:?}, got \
             {actual:?}"
        );

        // (case, how the request differs)
        let rows: &[AlteredRequestRow] = &[
            ("another status", |input| {
                input.status = Some("DISABLED".to_string());
            }),
            ("another description", |input| {
                input.description = Some("a pipeline".to_string());
            }),
            ("another pipeline", |input| {
                input.image_pipeline_arn =
                    "arn:aws:imagebuilder:us-west-2:000000000000:image-pipeline/other".to_string();
            }),
            ("a cleared recipe reference", |input| {
                input.image_recipe_arn = None;
            }),
            ("a schedule", |input| {
                input.schedule = Some(Schedule {
                    schedule_expression: Some("cron(0 0 * * ? *)".to_string()),
                    timezone: None,
                    pipeline_execution_start_condition: None,
                    auto_disable_policy: None,
                });
            }),
            ("a logging configuration", |input| {
                input.logging_configuration = Some(PipelineLoggingConfiguration {
                    image_log_group_name: Some("/aws/imagebuilder/image".to_string()),
                    pipeline_log_group_name: None,
                });
            }),
            ("tests turned off", |input| {
                input.image_tests_configuration = Some(ImageTestsConfiguration {
                    image_tests_enabled: Some(false),
                    timeout_minutes: None,
                });
            }),
        ];
        for (case, alter) in rows {
            let mut altered = base.clone();
            alter(&mut altered);
            let actual = image_pipeline_update_fingerprint(&altered);
            assert_ne!(
                actual, rendered,
                "case {case:?}: expected a rendering other than {rendered:?}, got {actual:?}"
            );
        }
    }
}
