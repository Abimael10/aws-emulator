// SPDX-License-Identifier: CC0-1.0

//! Which stored resource still REFERENCES the resource a delete names.
//!
//! A delete of this service is refused while another stored resource depends on what it
//! would remove: `ResourceDependencyException` is the modelled answer for "you have
//! attempted to mutate or delete a resource with a dependency that prohibits this action"
//! (the shape's own documentation), stated in one wording
//! ([`super::resource_dependency_message`], and in the infrastructure configuration's own
//! [`super::configuration_dependency_message`]). So the question each delete asks — does
//! anything still point at this? — is answered here, by the reverse of the reference
//! resolution the forward path uses, so the two cannot disagree about which resource a
//! stored reference reaches:
//!
//! * a COMPONENT is reached by the component list of a recipe of either kind
//!   ([`super::ComAmazonawsImagebuilderImagebuilderState::recipe_depending_on_component`],
//!   [`super::ComAmazonawsImagebuilderImagebuilderState::container_recipe_depending_on_component`]);
//! * a RECIPE of either kind is reached by the recipe reference an image pipeline holds
//!   ([`ComAmazonawsImagebuilderImagebuilderState::pipeline_depending_on_image_recipe`],
//!   [`ComAmazonawsImagebuilderImagebuilderState::pipeline_depending_on_container_recipe`]),
//!   which is the reference each of that pipeline's runs builds
//!   ([`super::ComAmazonawsImagebuilderImagebuilderState::resolve_built_recipe`]);
//! * an INFRASTRUCTURE or DISTRIBUTION configuration is reached by the pipeline that builds
//!   in it or distributes with it
//!   ([`ComAmazonawsImagebuilderImagebuilderState::pipeline_depending_on_infrastructure_configuration`],
//!   [`ComAmazonawsImagebuilderImagebuilderState::pipeline_depending_on_distribution_configuration`]) —
//!   "You can't delete a configuration that an image pipeline still references. The request
//!   fails with `ResourceDependencyException`. Update or delete the referencing pipelines
//!   first" (`DeleteInfrastructureConfiguration`, which `DeleteDistributionConfiguration`
//!   repeats). Neither configuration ARN carries a version, so a pipeline reaches one by
//!   naming it exactly.
//! * a WORKFLOW build version is reached by the workflow list an image pipeline holds
//!   ([`ComAmazonawsImagebuilderImagebuilderState::pipeline_depending_on_workflow`]) — "You
//!   can't delete a workflow build version while an image pipeline references it. The request
//!   fails with ResourceDependencyException" (`DeleteWorkflow`).
//!
//! A reference reaches a resource whenever it MATCHES it, not only when it is the newest
//! match: a pipeline holding a wildcard version builds whichever matching version is newest
//! at the instant it runs, so every version it matches is still in use — the same rule the
//! component dependency check follows.
//!
//! An image BUILD VERSION is not a dependant: a build records the recipe and the workflows it
//! was built from as a snapshot of what it ran, and that record is not a reference the recipe
//! or the workflow has to outlive.

use super::{
    ComAmazonawsImagebuilderImagebuilderState, ImagePipelineRecord, ImageRecipeRecord,
    VersionedReference, WorkflowReference, CONTAINER_RECIPE_ARN_PREFIX, IMAGE_RECIPE_ARN_PREFIX,
};

impl ComAmazonawsImagebuilderImagebuilderState {
    /// The first stored image pipeline whose recipe reference reaches the image recipe `arn`
    /// names, if any does.
    ///
    /// `None` when no pipeline reaches it — and when this store holds no recipe under that
    /// ARN at all, there being nothing for a reference to reach.
    #[must_use]
    pub fn pipeline_depending_on_image_recipe(&self, arn: &str) -> Option<&ImagePipelineRecord> {
        let recipe = self.image_recipes.get(arn)?;
        self.image_pipelines
            .values()
            .find(|pipeline| pipeline_reaches_image_recipe(pipeline, recipe))
    }

    /// The first stored image pipeline whose recipe reference reaches the CONTAINER recipe
    /// `arn` names, if any does — the other half of
    /// [`Self::pipeline_depending_on_image_recipe`], read by the same rule: "The request
    /// fails with `ResourceDependencyException` … if an image pipeline references it"
    /// (`DeleteContainerRecipe`).
    #[must_use]
    pub fn pipeline_depending_on_container_recipe(
        &self,
        arn: &str,
    ) -> Option<&ImagePipelineRecord> {
        let recipe = self.container_recipes.get(arn)?;
        self.image_pipelines.values().find(|pipeline| {
            pipeline
                .container_recipe_arn
                .as_deref()
                .is_some_and(|reference| {
                    reference_reaches(
                        reference,
                        CONTAINER_RECIPE_ARN_PREFIX,
                        &recipe.region,
                        &recipe.name,
                        &recipe.version,
                    )
                })
        })
    }

    /// The first stored image pipeline that builds in the infrastructure configuration `arn`
    /// names, if any does.
    ///
    /// An infrastructure configuration ARN carries no version, so a pipeline reaches one by
    /// naming it exactly: there is no wildcard form for a reference to it to resolve.
    #[must_use]
    pub fn pipeline_depending_on_infrastructure_configuration(
        &self,
        arn: &str,
    ) -> Option<&ImagePipelineRecord> {
        self.image_pipelines
            .values()
            .find(|pipeline| pipeline.infrastructure_configuration_arn == arn)
    }

    /// The first stored image pipeline that distributes with the distribution configuration
    /// `arn` names, if any does — read exactly as
    /// [`Self::pipeline_depending_on_infrastructure_configuration`] is, for the same reason.
    #[must_use]
    pub fn pipeline_depending_on_distribution_configuration(
        &self,
        arn: &str,
    ) -> Option<&ImagePipelineRecord> {
        self.image_pipelines
            .values()
            .find(|pipeline| pipeline.distribution_configuration_arn.as_deref() == Some(arn))
    }

    /// The first stored image pipeline whose workflow list reaches the workflow build version
    /// `arn` names, if any does.
    ///
    /// The reverse of the resolution a pipeline's own workflow references pass
    /// ([`super::ComAmazonawsImagebuilderImagebuilderState::resolve_workflow`]), read through
    /// the one reading of a workflow ARN this crate has ([`WorkflowReference`]) — so the
    /// stage, the Region, the name and the version nodes are compared exactly as the forward
    /// path compares them, and a reference carrying trailing `x` wildcards reaches every
    /// build it admits rather than only the newest.
    ///
    /// `None` when no pipeline reaches it, and when this store holds no workflow under that
    /// ARN at all, there being nothing for a reference to reach. The Region needs no
    /// argument: a reference reaches a record only when the two name the same Region, so a
    /// pipeline of another Region can reach no workflow of this one.
    #[must_use]
    pub fn pipeline_depending_on_workflow(&self, arn: &str) -> Option<&ImagePipelineRecord> {
        let workflow = self.workflows.get(arn)?;
        self.image_pipelines.values().find(|pipeline| {
            pipeline
                .workflows
                .iter()
                .flatten()
                .filter_map(|configuration| {
                    WorkflowReference::parse(&configuration.workflow_arn)
                })
                .any(|reference| reference.matches(workflow))
        })
    }
}

/// Whether the recipe reference `pipeline` holds reaches the stored image recipe `recipe`.
///
/// A pipeline names exactly one recipe, of either kind; one naming a CONTAINER recipe holds
/// no image-recipe reference, so it reaches no image recipe.
fn pipeline_reaches_image_recipe(
    pipeline: &ImagePipelineRecord,
    recipe: &ImageRecipeRecord,
) -> bool {
    pipeline
        .image_recipe_arn
        .as_deref()
        .is_some_and(|reference| {
            reference_reaches(
                reference,
                IMAGE_RECIPE_ARN_PREFIX,
                &recipe.region,
                &recipe.name,
                &recipe.version,
            )
        })
}

/// Whether the versioned `reference` reaches the stored record of `region`, `name` and
/// `version` under `resource_prefix`.
///
/// The ONE reading of a reference to a versioned resource ([`VersionedReference`]), so a
/// dependency check admits exactly the records the resolution a build performs would have
/// reached: the same Region, the same normalized name, no build number (a recipe reference
/// names none), and a version the reference's nodes admit — a wildcard node admitting every
/// value.
fn reference_reaches(
    reference: &str,
    resource_prefix: &str,
    region: &str,
    name: &str,
    version: &str,
) -> bool {
    VersionedReference::parse(reference, resource_prefix).is_some_and(|named| {
        named.region == region
            && named.name == super::normalized_name(name)
            && named.build_version.is_none()
            && super::version_admits(&named.version, version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a reference reaches a stored record when it names the same Region and
    /// normalized name and its version nodes admit the stored version — a wildcard node
    /// admitting any value.
    #[test]
    fn a_reference_reaches_every_version_its_nodes_admit() {
        // (case, the reference, the stored region, name and version, whether it reaches)
        let rows: &[(&str, &str, &str, &str, &str, bool)] = &[
            (
                "the exact version",
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/build-recipe/1.0.0",
                "us-west-2",
                "Build Recipe",
                "1.0.0",
                true,
            ),
            (
                "a patch wildcard over the stored patch",
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/build-recipe/1.0.x",
                "us-west-2",
                "build-recipe",
                "1.0.7",
                true,
            ),
            (
                "a minor wildcard over the stored minor",
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/build-recipe/1.x.x",
                "us-west-2",
                "build-recipe",
                "1.4.0",
                true,
            ),
            (
                "another version of the same name",
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/build-recipe/1.0.0",
                "us-west-2",
                "build-recipe",
                "2.0.0",
                false,
            ),
            (
                "another name",
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/other-recipe/1.0.0",
                "us-west-2",
                "build-recipe",
                "1.0.0",
                false,
            ),
            (
                "another Region",
                "arn:aws:imagebuilder:eu-west-1:000000000000:image-recipe/build-recipe/1.0.0",
                "us-west-2",
                "build-recipe",
                "1.0.0",
                false,
            ),
            (
                "a value that is not a recipe ARN at all",
                "build-recipe/1.0.0",
                "us-west-2",
                "build-recipe",
                "1.0.0",
                false,
            ),
        ];
        for (case, reference, region, name, version, expected) in rows {
            let actual =
                reference_reaches(reference, IMAGE_RECIPE_ARN_PREFIX, region, name, version);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
