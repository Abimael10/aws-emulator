// SPDX-License-Identifier: CC0-1.0

//! Which stored resource still REFERENCES the resource a delete names.
//!
//! A delete of this service is refused while another stored resource depends on what it
//! would remove: `ResourceDependencyException` is the modelled answer for "you have
//! attempted to mutate or delete a resource with a dependency that prohibits this action"
//! (the shape's own documentation), stated in one wording
//! ([`super::resource_dependency_message`]). So the question each delete asks — does
//! anything still point at this? — is answered here, by the reverse of the reference
//! resolution the forward path uses, so the two cannot disagree about which resource a
//! stored reference reaches:
//!
//! * a COMPONENT is reached by the component list of a recipe of either kind
//!   ([`super::ComAmazonawsImagebuilderImagebuilderState::recipe_depending_on_component`],
//!   [`super::ComAmazonawsImagebuilderImagebuilderState::container_recipe_depending_on_component`]);
//! * an IMAGE RECIPE is reached by the recipe reference an image pipeline holds
//!   ([`ComAmazonawsImagebuilderImagebuilderState::pipeline_depending_on_image_recipe`]),
//!   which is the reference each of that pipeline's runs builds
//!   ([`super::ComAmazonawsImagebuilderImagebuilderState::resolve_built_recipe`]).
//!
//! A reference reaches a resource whenever it MATCHES it, not only when it is the newest
//! match: a pipeline holding a wildcard version builds whichever matching version is newest
//! at the instant it runs, so every version it matches is still in use — the same rule the
//! component dependency check follows.
//!
//! An image BUILD VERSION is not a dependant: a build records the recipe it was built from
//! as a snapshot of what it ran, and that record is not a reference the recipe has to
//! outlive.

use super::{
    ComAmazonawsImagebuilderImagebuilderState, ImagePipelineRecord, ImageRecipeRecord,
    VersionedReference, IMAGE_RECIPE_ARN_PREFIX,
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
}

/// Whether the recipe reference `pipeline` holds reaches the stored image recipe `recipe`.
///
/// A pipeline names exactly one recipe, of either kind; one naming a CONTAINER recipe holds
/// no image-recipe reference, so it reaches no image recipe. The reference is read as every
/// other versioned reference of this service is ([`VersionedReference`]), so a wildcard
/// version node reaches each recipe it admits.
fn pipeline_reaches_image_recipe(
    pipeline: &ImagePipelineRecord,
    recipe: &ImageRecipeRecord,
) -> bool {
    pipeline
        .image_recipe_arn
        .as_deref()
        .and_then(|reference| VersionedReference::parse(reference, IMAGE_RECIPE_ARN_PREFIX))
        .is_some_and(|reference| {
            reference.region == recipe.region
                && reference.name == super::normalized_name(&recipe.name)
                && reference.build_version.is_none()
                && super::version_admits(&reference.version, &recipe.version)
        })
}
