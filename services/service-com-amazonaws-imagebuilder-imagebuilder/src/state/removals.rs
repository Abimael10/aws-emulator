// SPDX-License-Identifier: CC0-1.0

//! How a resource this store holds is FORGOTTEN.
//!
//! The other half of each delete: once nothing still references the resource
//! ([`super::dependents`]), the record is taken out of the store it was kept in and the
//! resource policy held beside it goes with it
//! ([`ComAmazonawsImagebuilderImagebuilderState::resource_policies`]), which is keyed by
//! that resource's own ARN — a policy outliving its resource would be reported for the next
//! resource created under the same ARN.
//!
//! Each removal answers the record it removed, so the response names what this call took
//! out of the store rather than echoing the request, and answers `None` for an ARN this
//! Region holds nothing under: a regional endpoint reaches only its own Region's resources,
//! which every lookup here enforces.

use super::{
    ComAmazonawsImagebuilderImagebuilderState, ContainerRecipeRecord,
    DistributionConfigurationRecord, ImagePipelineRecord, InfrastructureConfigurationRecord,
    WorkflowRecord,
};

impl ComAmazonawsImagebuilderImagebuilderState {
    /// Forget the image pipeline `arn` names, when it belongs to `request_region`.
    ///
    /// "Images that the pipeline created aren't deleted - remove those separately with
    /// `DeleteImage`. You can delete a pipeline while a build that it started is still
    /// running. The build continues independently"
    /// (`smithy_query(query="doc",shape="DeleteImagePipeline")`), so the build versions it
    /// queued stay exactly as they are.
    pub fn remove_image_pipeline(
        &mut self,
        arn: &str,
        request_region: &str,
    ) -> Option<ImagePipelineRecord> {
        self.image_pipeline(arn, request_region)?;
        self.resource_policies.remove(arn);
        self.image_pipelines.remove(arn)
    }

    /// Forget the infrastructure configuration `arn` names, when it belongs to
    /// `request_region`.
    pub fn remove_infrastructure_configuration(
        &mut self,
        arn: &str,
        request_region: &str,
    ) -> Option<InfrastructureConfigurationRecord> {
        self.infrastructure_configuration(arn, request_region)?;
        self.resource_policies.remove(arn);
        self.infrastructure_configurations.remove(arn)
    }

    /// Forget the distribution configuration `arn` names, when it belongs to
    /// `request_region`.
    pub fn remove_distribution_configuration(
        &mut self,
        arn: &str,
        request_region: &str,
    ) -> Option<DistributionConfigurationRecord> {
        self.distribution_configuration(arn, request_region)?;
        self.resource_policies.remove(arn);
        self.distribution_configurations.remove(arn)
    }

    /// Forget the container recipe `arn` names, when it belongs to `request_region`.
    pub fn remove_container_recipe(
        &mut self,
        arn: &str,
        request_region: &str,
    ) -> Option<ContainerRecipeRecord> {
        self.container_recipe(arn, request_region)?;
        self.resource_policies.remove(arn);
        self.container_recipes.remove(arn)
    }

    /// Forget the workflow BUILD VERSION `arn` names, when it belongs to `request_region`.
    ///
    /// One build version is one resource of this service, so a delete takes out exactly the
    /// build the ARN names and leaves every other build of the same name and version where
    /// it is — the same reading the component build version's own removal follows.
    pub fn remove_workflow(&mut self, arn: &str, request_region: &str) -> Option<WorkflowRecord> {
        self.workflow(arn, request_region)?;
        self.resource_policies.remove(arn);
        self.workflows.remove(arn)
    }
}
