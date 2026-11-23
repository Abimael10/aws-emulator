// SPDX-License-Identifier: CC0-1.0

//! Whether a value submitted to a member that IDENTIFIES the model a job is to run on names
//! a model this account can run that inference on.
//!
//! A job create reads this over its request BEFORE it decides the `iam:PassRole` the request
//! asks for ([`crate::state::job_pass_role_denial`]): the identifier is part of what the
//! request ASKS FOR, so a value that resolves to no model the job could run on is answered as
//! the input fault it is ([`super::unresolved_model`]) rather than as a report about the role,
//! and the pass-role denial is left to a request whose model identifier resolves. The one job
//! create that cannot take it in that order is the customization, whose
//! `ModelCustomizationJobArn` spells the RESOLVED base model before the job id: the resource
//! the pass is decided FOR is not known until that resolution has happened, so there only the
//! identifier's FORM is read in front of the pass
//! ([`super::names_a_customizable_base_model`]).
//!
//! Each family is resolved through the very index that family's own reads resolve an
//! identifier through, so a create and a read never disagree about which identifiers name a
//! model, and WHICH families are asked follows the member's own published pattern rather
//! than one list for every member: a batch inference `modelId` admits the base models, this
//! account's custom models and the inference profiles
//! (`smithy_query(query="shape",shape="ModelId")`), while an evaluation's
//! `modelIdentifier` admits the base models, both profile kinds, this account's Provisioned
//! Throughputs, its imported models, the prompt routers and a registered Amazon SageMaker
//! endpoint (`smithy_query(query="shape",shape="EvaluationBedrockModelIdentifier")`).

use super::*;

impl ComAmazonawsBedrockAmazonbedrockcontrolplaneserviceState {
    /// Whether `identifier` names a model a BATCH INFERENCE job can invoke, resolved against
    /// every family `ModelId` admits ([`INVOCATION_MODEL_RESOURCES`] spells the ARN forms
    /// among them).
    pub fn names_an_invocation_model(
        &mut self,
        identifier: &str,
        region: &str,
        now: SystemTime,
    ) -> bool {
        if self.names_a_base_model(identifier) {
            return true;
        }
        if self.custom_model(identifier, region, now).is_some() {
            return true;
        }
        self.names_an_inference_profile(identifier, region)
    }

    /// Whether `identifier` names a model an EVALUATION job can invoke, resolved against
    /// every family `EvaluationBedrockModelIdentifier` admits
    /// ([`EVALUATION_MODEL_RESOURCES`] spells the ARN forms among them).
    ///
    /// The one spelling that is not an ARN of this service's own namespace is the Amazon
    /// SageMaker `endpoint` ARN, which names a model endpoint only where it has been
    /// REGISTERED with Amazon Bedrock Marketplace — the resource `CreateMarketplaceModelEndpoint`
    /// and `RegisterMarketplaceModelEndpoint` mint, held under that very ARN
    /// ([`marketplace_model_endpoint_arn`]).
    pub fn names_an_evaluation_model(
        &mut self,
        identifier: &str,
        region: &str,
        now: SystemTime,
    ) -> bool {
        if self.names_a_base_model(identifier) {
            return true;
        }
        if self.names_an_inference_profile(identifier, region) {
            return true;
        }
        if self
            .provisioned_model_throughput(identifier, region, now)
            .is_some()
        {
            return true;
        }
        if self.imported_model(identifier, region, now).is_some() {
            return true;
        }
        if self.prompt_router(identifier).is_some()
            || default_prompt_router(identifier, region).is_some()
        {
            return true;
        }
        self.marketplace_model_endpoints.contains_key(identifier)
    }

    /// Whether `identifier` names a base model a NEW job can run on: one the published
    /// catalogue OFFERS, by its bare id or by its `foundation-model` ARN
    /// ([`FoundationModelCatalog::resolve_base_model`]).
    ///
    /// A model this service recognises and offers NO LONGER is offered under nothing
    /// ([`is_past_end_of_life`]): there is no model for a job submitted now to run on, so
    /// such an identifier names nothing this create can act on and is refused as the input it
    /// is. RECORDED behaviour — the corpus records both batch-job creates refusing exactly
    /// such an identifier with the fixed invalid-model-identifier sentence — and it is the
    /// reading `ops/create_model_customization_job.rs` already takes of the base model a
    /// customization runs on, which resolves against the offered catalogue alone. An
    /// operation about a model's own LIFECYCLE rather than about work to run on it answers
    /// differently ([`unoffered_foundation_model`] for the catalogue reads, and a Provisioned
    /// Throughput purchase for the throughput a retired model is offered under).
    fn names_a_base_model(&self, identifier: &str) -> bool {
        matches!(
            self.foundation_models.resolve_base_model(identifier),
            BaseModelIdentity::Offered(_)
        )
    }

    /// Whether `identifier` names an inference profile: one this account created
    /// ([`Self::inference_profile_arn_of`]) or one Amazon Bedrock publishes in `region`
    /// ([`published_inference_profile`]) — the two kinds `GetInferenceProfile` resolves the
    /// same member against, by the profile's ARN or by its bare id.
    fn names_an_inference_profile(&self, identifier: &str, region: &str) -> bool {
        self.inference_profile_arn_of(identifier, region).is_some()
            || published_inference_profile(identifier, region).is_some()
    }
}

/// Every value an evaluation's `inferenceConfig` submits to a member that IDENTIFIES a model
/// Amazon Bedrock is to invoke for the job.
///
/// The `models` configuration names them directly: each `bedrockModel` carries the
/// `modelIdentifier` of the model whose responses are scored. A
/// `precomputedInferenceSource` carries a LABEL for responses the caller computed elsewhere
/// ("a label that identifies a model used in a model evaluation job where you provide your
/// own inference response data",
/// `smithy_query(query="doc",shape="EvaluationPrecomputedInferenceSource",member="inferenceSourceIdentifier")`),
/// so it identifies no model for this service to resolve.
///
/// A `ragConfigs` configuration names the knowledge base the responses are retrieved from,
/// and — where it asks for RESPONSE GENERATION — the `modelArn` of "the foundation model or
/// inference profile used to generate responses"
/// (`smithy_query(query="doc",shape="KnowledgeBaseRetrieveAndGenerateConfiguration",member="modelArn")`),
/// which identifies a model the same way. A retrieval-only configuration generates no
/// response, so it names none, and a precomputed RAG source brings its own responses for the
/// reason a precomputed inference source does.
#[must_use]
pub fn evaluation_model_identifiers(config: &EvaluationInferenceConfig) -> Vec<&str> {
    match config {
        EvaluationInferenceConfig::Models(models) => models
            .iter()
            .filter_map(|model| match model {
                EvaluationModelConfig::bedrockModel(model) => {
                    Some(model.model_identifier.as_str())
                }
                EvaluationModelConfig::precomputedInferenceSource(_) => None,
            })
            .collect(),
        EvaluationInferenceConfig::ragConfigs(configs) => configs
            .iter()
            .filter_map(|config| match config {
                RAGConfig::knowledgeBaseConfig(KnowledgeBaseConfig::retrieveAndGenerateConfig(
                    generate,
                )) => Some(generate),
                RAGConfig::knowledgeBaseConfig(KnowledgeBaseConfig::retrieveConfig(_))
                | RAGConfig::precomputedRagSourceConfig(_) => None,
            })
            .flat_map(|generate| {
                generate
                    .knowledge_base_configuration
                    .iter()
                    .map(|configured| configured.model_arn.as_str())
                    .chain(
                        generate
                            .external_sources_configuration
                            .iter()
                            .map(|configured| configured.model_arn.as_str()),
                    )
            })
            .collect(),
    }
}
