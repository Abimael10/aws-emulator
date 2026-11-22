// SPDX-License-Identifier: CC0-1.0

//! The ENDPOINT slice of this service's state: the endpoint configurations, the
//! endpoints created from them, the inference components hosted on them, and the
//! hosting-capacity reading a create answers before it stores anything.
//!
//! Moved out of [`super`] unchanged, so the parent module stays inside the project's
//! file-length limit and this resource family reads as one unit. Every name is
//! re-exported from [`super`], so a handler keeps naming `crate::state::…`.

use super::*;

/// An endpoint configuration created by `CreateEndpointConfig`, as
/// `DescribeEndpointConfig` replays it and `CreateEndpoint` reads it.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    pub name: String,
    pub arn: String,
    pub production_variants: Vec<ProductionVariant>,
    pub shadow_production_variants: Option<Vec<ProductionVariant>>,
    pub data_capture_config: Option<DataCaptureConfig>,
    pub async_inference_config: Option<AsyncInferenceConfig>,
    pub explainer_config: Option<ExplainerConfig>,
    pub metrics_config: Option<MetricsConfig>,
    pub kms_key_id: Option<String>,
    pub execution_role_arn: Option<String>,
    pub vpc_config: Option<VpcConfig>,
    pub enable_network_isolation: Option<bool>,
    pub tags: Vec<Tag>,
    pub creation_time: SystemTime,
}

/// An endpoint created by `CreateEndpoint`, as `DescribeEndpoint` replays it.
///
/// `in_service_at` is the instant the clock owes the `Creating` -> `InService`
/// transition; it is stored, never slept on, and materialised by whichever reporting
/// call first runs at or after it.
///
/// `instance_usage` is how many instances of each ML instance type this endpoint holds,
/// snapshotted from the configuration it was created from (like its variant summaries,
/// and for the same reason: a later configuration does not retroactively change what is
/// deployed). It is what the per-instance-type hosting quota counts against
/// ([`ComAmazonawsSagemakerSagemakerState::endpoint_instances_in_use`]), so deleting the
/// endpoint releases exactly what creating it took.
///
/// `pending_update` is the deployment `UpdateEndpoint` asked for and the clock has not
/// completed yet; while it is set the endpoint still reports the configuration that is
/// serving, and the new one is what [`Endpoint::settle_status`] swaps in when the
/// deployment falls due. `last_deployment_config` is the deployment configuration of the
/// most recent deployment the endpoint was given one for.
///
/// `deployment_failure` is why the fleet this endpoint was created for cannot come up at
/// all — resolved against the model data its variants' models name
/// ([`model_data_deployment_failure`]) — and it is the `FailureReason` the endpoint settles
/// `Failed` with when the clock reaches the instant its outcome was owed. `None` is a
/// deployment with nothing standing in its way, which settles `InService`.
///
/// `deployment_artifacts` is the model data that deployment DOWNLOADS, snapshotted by the
/// create like every other property of the fleet it stands up, so the instant the fleet is
/// due to serve can be resolved against it ([`resolve_due_endpoint_deployment`]).
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub name: String,
    pub arn: String,
    pub config_name: String,
    pub status: String,
    pub failure_reason: Option<String>,
    pub production_variants: Vec<ProductionVariantSummary>,
    pub shadow_production_variants: Vec<ProductionVariantSummary>,
    pub data_capture_config: Option<DataCaptureConfig>,
    pub async_inference_config: Option<AsyncInferenceConfig>,
    pub explainer_config: Option<ExplainerConfig>,
    pub metrics_config: Option<MetricsConfig>,
    pub tags: Vec<Tag>,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub in_service_at: SystemTime,
    pub instance_usage: BTreeMap<String, i64>,
    pub pending_update: Option<EndpointUpdate>,
    pub last_deployment_config: Option<DeploymentConfig>,
    pub deployment_failure: Option<String>,
    pub deployment_artifacts: Vec<DeployedArtifact>,
}

/// The deployment an `UpdateEndpoint` started and the clock has not completed yet: the
/// configuration being deployed, snapshotted the way a create snapshots the one it
/// deploys, plus the instants the deployment started and is owed complete.
///
/// The variants are the NEW configuration's, with the properties the request asked to
/// retain already folded in ([`retained_variants`]), so settling the deployment is a
/// swap and needs nothing of the request again.
#[derive(Debug, Clone)]
pub struct EndpointUpdate {
    pub config_name: String,
    pub production_variants: Vec<ProductionVariant>,
    pub shadow_production_variants: Option<Vec<ProductionVariant>>,
    /// The images each PRODUCTION variant this deployment stands up deploys, keyed by
    /// variant name and snapshotted with the variants themselves
    /// ([`deployed_variant_images`]), so the fleet that starts serving reports the images the
    /// deployment resolved.
    pub deployed_images: BTreeMap<String, Vec<crate::ops::types::DeployedImage>>,
    /// The same for the SHADOW variants, kept apart from the production ones: a variant name
    /// identifies a variant within ONE fleet, so a shadow variant sharing a name with a
    /// production variant hosts its own model and deploys its own images.
    pub shadow_deployed_images: BTreeMap<String, Vec<crate::ops::types::DeployedImage>>,
    pub data_capture_config: Option<DataCaptureConfig>,
    pub async_inference_config: Option<AsyncInferenceConfig>,
    pub explainer_config: Option<ExplainerConfig>,
    pub metrics_config: Option<MetricsConfig>,
    pub started_at: SystemTime,
    pub in_service_at: SystemTime,
}

impl EndpointUpdate {
    /// Every variant the deployment stands up: the production ones and the shadow ones,
    /// which run on the same fleet's capacity.
    pub fn variants(&self) -> impl Iterator<Item = &ProductionVariant> {
        self.production_variants
            .iter()
            .chain(self.shadow_production_variants.iter().flatten())
    }

    /// How many instances of each ML instance type this deployment takes; see
    /// [`instance_demand`].
    pub fn instance_demand(&self) -> BTreeMap<String, i64> {
        instance_demand(self.variants())
    }
}

/// Which variant properties an update RETAINS from the fleet that is serving, rather than
/// taking from the configuration being deployed.
///
/// `VariantPropertyType`'s two members that name a retained property are the instance
/// count and the variant weight (its third, `DataCaptureConfig`, the model documents as
/// "Not currently supported", and an endpoint retains no capture settings across a
/// deployment — so naming it excludes nothing).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetainedProperties {
    pub instance_count: bool,
    pub weight: bool,
}

/// `VariantPropertyType`'s member naming the variant instance count
/// (`smithy_query(query="shape",shape="VariantPropertyType")`).
pub const VARIANT_PROPERTY_DESIRED_INSTANCE_COUNT: &str = "DesiredInstanceCount";

/// `VariantPropertyType`'s member naming the variant weight.
pub const VARIANT_PROPERTY_DESIRED_WEIGHT: &str = "DesiredWeight";

/// What an update retains, from the request's `RetainAllVariantProperties` and the
/// property types it excluded from that retention.
///
/// Nothing is retained unless the request asked for all variant properties to be retained
/// (the model documents the default as `false`), and a property type the request listed as
/// excluded is taken from the new configuration after all — which is what
/// `ExcludeRetainedVariantProperties` is documented to override.
pub fn retained_properties(retain_all: bool, excluded: &[&str]) -> RetainedProperties {
    RetainedProperties {
        instance_count: retain_all
            && !excluded.contains(&VARIANT_PROPERTY_DESIRED_INSTANCE_COUNT),
        weight: retain_all && !excluded.contains(&VARIANT_PROPERTY_DESIRED_WEIGHT),
    }
}

/// The variants a deployment will run: the configuration's own, with each property
/// `retained` names taken from the variant of the same name that is serving now.
///
/// A variant the serving fleet does not carry has nothing to retain, so it is deployed
/// exactly as the configuration declares it.
pub fn retained_variants(
    config_variants: &[ProductionVariant],
    serving: &[ProductionVariantSummary],
    retained: RetainedProperties,
) -> Vec<ProductionVariant> {
    config_variants
        .iter()
        .map(|variant| {
            let mut deployed = variant.clone();
            let Some(current) = serving
                .iter()
                .find(|summary| summary.variant_name == variant.variant_name)
            else {
                return deployed;
            };
            if retained.instance_count && current.current_instance_count.is_some() {
                deployed.initial_instance_count = current.current_instance_count;
            }
            if retained.weight && current.current_weight.is_some() {
                deployed.initial_variant_weight = current.current_weight;
            }
            deployed
        })
        .collect()
}

/// The variant summaries an endpoint reports, derived from the configuration's stored
/// `ProductionVariant`s and the images each variant's model deploys
/// ([`deployed_variant_images`], keyed by variant name).
///
/// `CurrentWeight`/`DesiredWeight` come from `InitialVariantWeight`, whose model doc
/// says "If unspecified, it defaults to 1.0"; the instance counts come from
/// `InitialInstanceCount`, and a serverless variant reports its serverless config
/// instead of a count.
///
/// `CurrentInstanceCount` is "The number of instances associated with the variant"
/// (`TaskCount`, `@range min 0`), which every variant of a created endpoint has a number
/// for: the instances its configuration provisions, and zero for a variant that provisions
/// none — a serverless one holds no instances rather than an unknown number of them.
/// `DesiredInstanceCount` is "The number of instances requested in the
/// `UpdateEndpointWeightsAndCapacities` request", so it is reported only for a variant a
/// count was asked for.
///
/// `DeployedImages` are the inference images deployed on the variant's instances, which are
/// the containers of the model it hosts; a variant hosting no model deploys none, so the
/// member — a `len 1..`-free list that AWS reports per container — stays absent rather than
/// carrying an empty list.
///
/// Stated once here because an endpoint takes its summaries from a configuration both
/// when it is created from one and when a deployment swaps one in.
pub fn variant_summaries(
    variants: &[ProductionVariant],
    deployed: &BTreeMap<String, Vec<crate::ops::types::DeployedImage>>,
) -> Vec<ProductionVariantSummary> {
    variants
        .iter()
        .map(|variant| ProductionVariantSummary {
            variant_name: variant.variant_name.clone(),
            deployed_images: deployed
                .get(&variant.variant_name)
                .filter(|images| !images.is_empty())
                .cloned(),
            current_weight: Some(variant.initial_variant_weight.unwrap_or(1.0)),
            desired_weight: Some(variant.initial_variant_weight.unwrap_or(1.0)),
            current_instance_count: Some(variant.initial_instance_count.unwrap_or(0)),
            desired_instance_count: variant.initial_instance_count,
            instance_pools: None,
            variant_status: None,
            current_serverless_config: variant.serverless_config.clone(),
            desired_serverless_config: variant.serverless_config.clone(),
            managed_instance_scaling: variant.managed_instance_scaling.clone(),
            routing_config: variant.routing_config.clone(),
            capacity_reservation_config: None,
        })
        .collect()
}

/// The images each of `variants` deploys, keyed by variant name: the stored images of the
/// model the variant hosts ([`Model::deployed_images`], resolved by the create that
/// registered that model).
///
/// SNAPSHOTTED by the write that stands the fleet up, like every other property a variant
/// summary reports: a model deleted or re-registered afterwards does not change what an
/// endpoint already serving it reports. A variant naming no model, or one naming a model
/// this account does not hold, deploys no image of its own.
pub fn deployed_variant_images(
    variants: &[ProductionVariant],
    models: &BTreeMap<String, Model>,
) -> BTreeMap<String, Vec<crate::ops::types::DeployedImage>> {
    variants
        .iter()
        .filter_map(|variant| {
            let model = models.get(variant.model_name.as_deref()?)?;
            Some((variant.variant_name.clone(), model.deployed_images.clone()))
        })
        .collect()
}

/// The unit the per-instance-type hosting quotas are counted in: what their limit, the
/// account's utilisation of them and a request's delta are numbers OF, as
/// [`account_quota_exceeded`] states them.
pub const INSTANCES_UNIT: &str = "Instances";

/// The most model artifacts ONE deployment resolves before the fleet it stands up comes up.
///
/// An EMULATOR ceiling, not an AWS limit: the modelled lists admit up to ten production
/// variants (`ProductionVariantList` is `@length 1..10`) of up to fifteen containers each
/// (`ContainerDefinitionList` is `@length 0..15`), and every location is a question to Amazon
/// S3 and to IAM, so the work one create can ask for is bounded whatever the configuration
/// declares.
pub const MAX_DEPLOYMENT_ARTIFACTS: usize = 16;

/// One model artifact a deployment has to DOWNLOAD to bring a fleet up: the Amazon S3
/// location a variant's model holds its model data at, and the execution role that model is
/// read as.
#[derive(Debug, Clone)]
pub struct DeployedArtifact {
    /// The model the variant hosts, which is what a failure about this download names.
    pub model_name: String,
    /// `CreateModel$ExecutionRoleArn`: "the ARN of the IAM role that SageMaker can assume to
    /// access model artifacts and docker image for deployment".
    pub role_arn: String,
    /// The model data location, as the model's container names it.
    pub uri: String,
}

/// Every model artifact the fleet of `variants` has to download, in the order the
/// configuration lists the variants and each model names its locations
/// ([`model_data_locations`]), bounded by [`MAX_DEPLOYMENT_ARTIFACTS`].
///
/// A model deployed out of the REGISTRY carries no model data of its own: its artifact is
/// where the model package's inference specification names it ([`served_artifact_url`]), which
/// is the location such a deployment downloads, so that is what is resolved for it.
///
/// A variant naming no model, a model this account does not hold and a model whose containers
/// name no model data at all each have nothing to download: there is no artifact of theirs for
/// the deployment to fetch, so none is reported here. A model that assumes no execution role
/// has one all the same — whether the object is THERE is not a question about the role.
#[must_use]
pub fn deployed_model_artifacts<'variants>(
    store: &ComAmazonawsSagemakerSagemakerState,
    variants: impl IntoIterator<Item = &'variants ProductionVariant>,
) -> Vec<DeployedArtifact> {
    variants
        .into_iter()
        .filter_map(|variant| store.models.get(variant.model_name.as_deref()?))
        .flat_map(|model| {
            let role_arn = model.execution_role_arn.clone().unwrap_or_default();
            let mut locations =
                model_data_locations(model.primary_container.as_ref(), model.containers.as_deref());
            if locations.is_empty() {
                locations.extend(served_artifact_url(store, model));
            }
            locations.into_iter().map(move |uri| DeployedArtifact {
                model_name: model.name.clone(),
                role_arn: role_arn.clone(),
                uri,
            })
        })
        .take(MAX_DEPLOYMENT_ARTIFACTS)
        .collect()
}

/// Resolve the model data of an endpoint whose CREATE's outcome falls due on this call, so the
/// outcome it settles is the one the deployment's own download reaches.
///
/// The fleet comes up at the instant the clock owes the endpoint, and what has to be there at
/// that instant is the model data — "SageMaker uses Amazon Web Services Security Token Service
/// to download model artifacts from the S3 path you provided" (`CreateEndpoint`) — so a
/// location that went away between the call that asked for the endpoint and the instant it was
/// due to serve fails the deployment exactly as one that was already gone. The artifacts are
/// the ones the CREATE snapshotted ([`Endpoint::deployment_artifacts`]), so a model deleted or
/// re-registered afterwards does not change what this deployment downloads.
///
/// Changes nothing for an endpoint whose create is not due on this call, one already settled,
/// one with nothing to download and one whose create already resolved a failure: the question
/// is asked once per deployment, and only when its outcome is being materialised. The read
/// itself is Amazon S3's, so no store guard is held across it.
pub async fn resolve_due_endpoint_deployment(
    state: &::engine::ServiceState<ComAmazonawsSagemakerSagemakerState>,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    endpoint_name: &str,
    now: SystemTime,
) {
    let artifacts = {
        let store = state.lock();
        let Some(endpoint) = store.endpoints.get(endpoint_name) else {
            return;
        };
        if endpoint.status != STATUS_CREATING
            || now < endpoint.in_service_at
            || endpoint.deployment_failure.is_some()
            || endpoint.deployment_artifacts.is_empty()
        {
            return;
        }
        endpoint.deployment_artifacts.clone()
    };
    let Some(reason) = model_data_deployment_failure(ctx, region, &artifacts).await else {
        return;
    };
    let mut store = state.lock();
    if let Some(endpoint) = store.endpoints.get_mut(endpoint_name) {
        // Still the same create, still unsettled: a deployment another call materialised in
        // the meantime keeps the outcome that call reached.
        if endpoint.status == STATUS_CREATING && endpoint.deployment_failure.is_none() {
            endpoint.deployment_failure = Some(reason);
        }
    }
}

/// Why the deployment of `artifacts` cannot come up — the endpoint's `FailureReason` — or
/// `None` when every one of them can be read as the role its own model assumes.
///
/// "If any of the models hosted at this endpoint get model data from an Amazon S3 location,
/// SageMaker uses Amazon Web Services Security Token Service to download model artifacts from
/// the S3 path you provided" (`CreateEndpoint`): the download happens while the endpoint is
/// brought up, so a location that holds no object, or one the role may not read, is a fleet
/// that never serves. Both are the DOWNLOAD failing, which is the one sentence a deployment
/// reports them with ([`failed_to_download_model_data`]) — it names the location and both of
/// the things that have to hold for the download to succeed.
///
/// Asked through the same reading the hosting creates ask ([`first_unreadable_location_as_role`])
/// and awaited with NO store guard held; only what Amazon S3 and IAM ANSWERED fails a
/// deployment.
pub async fn model_data_deployment_failure(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    artifacts: &[DeployedArtifact],
) -> Option<String> {
    for artifact in artifacts {
        let unreadable = first_unreadable_location_as_role(
            ctx,
            region,
            &artifact.role_arn,
            [artifact.uri.as_str()],
        )
        .await;
        if let Some(unreadable) = unreadable {
            return Some(failed_to_download_model_data(unreadable.uri()));
        }
    }
    None
}

/// The published name of the quota that bounds how many instances of ONE ML instance type
/// an account may run behind its endpoints, as a refusal states it.
///
/// Service Quotas names the entry `<instance type> for endpoint usage` (it is listed under
/// that name for this service, beside the notebook-instance and training entries of the
/// same shape), so the instance type is part of the quota's NAME rather than prose around
/// it: a client reads which type it has no room for straight off the message.
pub fn endpoint_usage_quota_name(instance_type: &str) -> String {
    format!("{instance_type} for endpoint usage")
}

/// The instance FAMILIES whose published default per-type instance quota is zero: the
/// newest accelerated generations (the `p4`/`p5`/`p6` GPU families and the Trainium
/// ones), which an account has to be granted capacity for before it can run anything on
/// them.
///
/// Keyed on the FAMILY — the middle segment of an `ml.<family>.<size>` instance type — so
/// a size AWS adds to one of these families needs no edit here, and so the rule is one
/// about generations rather than a list of every type. Every other family's default is a
/// non-zero number of instances ([`DEFAULT_ENDPOINT_USAGE_QUOTA`] for hosting,
/// [`crate::state::DEFAULT_CLUSTER_USAGE_QUOTA`] for a HyperPod cluster). Read through
/// [`has_zero_default_instance_quota`], which both quotas share.
const ZERO_DEFAULT_HOSTING_FAMILIES: [&str; 11] = [
    "p4d",
    "p4de",
    "p5",
    "p5e",
    "p5en",
    "p6-b200",
    "p6-b300",
    "p6e-gb200",
    "trn1",
    "trn1n",
    "trn2",
];

/// How many instances of ONE type an account may host, for a type whose published default
/// is not zero.
///
/// This emulator models neither Service Quotas nor an account's adjustments, so — as with
/// [`MODEL_QUOTA`] — this is the emulator's OWN bound rather than a claim about any
/// account's limit for a particular type: far above any deployment an emulator serves, and
/// what stops one request asking for an unbounded number of instances.
pub const DEFAULT_ENDPOINT_USAGE_QUOTA: i64 = 1_000;

/// Whether AWS publishes a ZERO default quota for the instances of `instance_type`'s
/// family, so an account has to be granted capacity before it can run any.
///
/// Keyed on the FAMILY — the middle segment of an `ml.<family>.<size>` instance type —
/// stated once here because the same families carry the zero default for EVERY per-type
/// instance quota this service publishes: the endpoint-usage one
/// ([`endpoint_usage_quota`]) and the HyperPod cluster-usage one
/// ([`crate::state::cluster_usage_quota`]).
#[must_use]
pub fn has_zero_default_instance_quota(instance_type: &str) -> bool {
    // `ml.<family>.<size>`: the family is the second segment, and a value that is not in
    // that form names no family of the zero-default generations.
    let family = instance_type.split('.').nth(1).unwrap_or_default();
    ZERO_DEFAULT_HOSTING_FAMILIES.contains(&family)
}

/// The account's quota, in instances, for hosting `instance_type` behind an endpoint.
///
/// Zero for a family AWS publishes a zero default for ([`ZERO_DEFAULT_HOSTING_FAMILIES`]),
/// so an endpoint on one is refused until the account is granted capacity;
/// [`DEFAULT_ENDPOINT_USAGE_QUOTA`] for every other type.
pub fn endpoint_usage_quota(instance_type: &str) -> i64 {
    if has_zero_default_instance_quota(instance_type) {
        return 0;
    }
    DEFAULT_ENDPOINT_USAGE_QUOTA
}

/// How many instances of each ML instance type `variants` ask for: one entry per type, the
/// instance counts of the variants naming it added up.
///
/// A variant that names an instance type runs at least one instance of it, so a variant
/// with no `InitialInstanceCount` counts as one; a variant that names no single type at all
/// — a serverless one, or one that lists a pool of alternatives for the service to choose
/// from — holds no instances of any one type and contributes nothing.
pub fn instance_demand<'variants>(
    variants: impl IntoIterator<Item = &'variants ProductionVariant>,
) -> BTreeMap<String, i64> {
    let mut demand: BTreeMap<String, i64> = BTreeMap::new();
    for variant in variants {
        let Some(instance_type) = variant.instance_type.as_deref() else {
            continue;
        };
        *demand.entry(instance_type.to_string()).or_default() += variant_instance_count(variant);
    }
    demand
}

/// How many instances of its own type one variant runs: its `InitialInstanceCount`, and at
/// least one, because a variant that names an instance type runs an instance of it.
///
/// The one normalisation both readings of a variant's capacity share, so what a quota
/// counts for a type ([`instances_of_type`]) cannot disagree with what the whole demand
/// ([`instance_demand`]) adds up.
fn variant_instance_count(variant: &ProductionVariant) -> i64 {
    i64::from(variant.initial_instance_count.unwrap_or(1)).max(1)
}

/// How many instances of `instance_type` `variants` ask for: [`instance_demand`]'s reading
/// of ONE type, without building the map of every type.
pub fn instances_of_type<'variants>(
    variants: impl IntoIterator<Item = &'variants ProductionVariant>,
    instance_type: &str,
) -> i64 {
    variants
        .into_iter()
        .filter(|variant| variant.instance_type.as_deref() == Some(instance_type))
        .map(variant_instance_count)
        .sum()
}

/// Materialise the `Creating` -> `InService` transition the clock already owes a
/// resource that carries one.
///
/// This is the one write a reporting operation may make: the transition was due at
/// `in_service_at` whether or not anybody called, so the observer makes no difference to
/// the result. `last_modified_time` moves to the instant the transition was due, not to
/// "now", so the value is stable across later reads.
///
/// Answers whether the transition happened on this call, so the caller can record it.
pub(super) fn settle_creating(
    status: &mut String,
    last_modified_time: &mut SystemTime,
    in_service_at: SystemTime,
    now: SystemTime,
) -> bool {
    if status == STATUS_CREATING && now >= in_service_at {
        *status = STATUS_IN_SERVICE.to_string();
        *last_modified_time = in_service_at;
        return true;
    }
    false
}

impl Endpoint {
    /// Materialise the transition the clock already owes this endpoint: the
    /// `Creating` -> `InService` one of a create (see [`settle_creating`]), or the
    /// `Updating` -> `InService` one of a deployment, which swaps the configuration being
    /// deployed in for the one that was serving.
    ///
    /// A create whose fleet cannot come up at all settles `Failed` with the reason it
    /// carries instead of `InService`: "If any of the models hosted at this endpoint get
    /// model data from an Amazon S3 location, SageMaker uses Amazon Web Services Security
    /// Token Service to download model artifacts from the S3 path you provided"
    /// (`CreateEndpoint`), so a deployment whose model data cannot be downloaded has no
    /// model to serve. The outcome was owed at the same instant either way — the condition
    /// was resolved by the create, not by this reader.
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        if settle_creating(
            &mut self.status,
            &mut self.last_modified_time,
            self.in_service_at,
            now,
        ) {
            if let Some(reason) = self.deployment_failure.clone() {
                self.status = STATUS_FAILED.to_string();
                self.failure_reason = Some(reason);
            }
            return true;
        }
        self.settle_update(now)
    }

    /// The deployment of `config` this endpoint WOULD run, retaining the variant
    /// properties `retained` names from the fleet that is serving now.
    ///
    /// Computes and changes nothing, so a caller can read what the deployment would take
    /// — its [`EndpointUpdate::instance_demand`] — and refuse it before anything is
    /// stored. The configuration is SNAPSHOTTED into the result, the way a create
    /// snapshots the one it deploys: a later `CreateEndpointConfig` does not change what
    /// this deployment stands up.
    pub fn planned_update(
        &self,
        config: &EndpointConfig,
        retained: RetainedProperties,
        now: SystemTime,
        models: &BTreeMap<String, Model>,
    ) -> EndpointUpdate {
        let production_variants = retained_variants(
            &config.production_variants,
            &self.production_variants,
            retained,
        );
        let shadow_production_variants = config.shadow_production_variants.as_deref().map(
            |variants| retained_variants(variants, &self.shadow_production_variants, retained),
        );
        // The images the deployment deploys are snapshotted with the variants, from the
        // models registered NOW: a model re-registered while the deployment is in flight
        // does not change what this deployment stands up. One map per fleet, because a
        // variant name identifies a variant inside its own fleet.
        let deployed_images = deployed_variant_images(&production_variants, models);
        let shadow_deployed_images = shadow_production_variants
            .as_deref()
            .map(|variants| deployed_variant_images(variants, models))
            .unwrap_or_default();
        EndpointUpdate {
            config_name: config.name.clone(),
            production_variants,
            shadow_production_variants,
            deployed_images,
            shadow_deployed_images,
            data_capture_config: config.data_capture_config.clone(),
            async_inference_config: config.async_inference_config.clone(),
            explainer_config: config.explainer_config.clone(),
            metrics_config: config.metrics_config.clone(),
            started_at: now,
            in_service_at: now + ENDPOINT_UPDATED_AFTER,
        }
    }

    /// Start `update`: the endpoint goes `Updating` and keeps reporting the configuration
    /// that is serving until the deployment is owed complete, which is
    /// [`ENDPOINT_UPDATED_AFTER`] after the call that asked for it.
    pub fn begin_update(&mut self, update: EndpointUpdate) {
        self.status = STATUS_UPDATING.to_string();
        self.last_modified_time = update.started_at;
        self.pending_update = Some(update);
    }

    /// Materialise the deployment the clock already owes: the new fleet is serving, the
    /// old one is gone, and the endpoint reports the configuration that was deployed.
    ///
    /// The same single write a reporting operation may make as [`settle_creating`]: the
    /// deployment was due at its stored instant whether or not anybody called, and
    /// `last_modified_time` moves to that instant rather than to "now", so the value is
    /// stable across later reads.
    fn settle_update(&mut self, now: SystemTime) -> bool {
        let Some(update) = self
            .pending_update
            .take_if(|update| now >= update.in_service_at)
        else {
            return false;
        };
        self.instance_usage = update.instance_demand();
        self.production_variants =
            variant_summaries(&update.production_variants, &update.deployed_images);
        self.shadow_production_variants = update
            .shadow_production_variants
            .as_deref()
            .map(|variants| variant_summaries(variants, &update.shadow_deployed_images))
            .unwrap_or_default();
        self.config_name = update.config_name;
        self.data_capture_config = update.data_capture_config;
        self.async_inference_config = update.async_inference_config;
        self.explainer_config = update.explainer_config;
        self.metrics_config = update.metrics_config;
        self.status = STATUS_IN_SERVICE.to_string();
        self.last_modified_time = update.in_service_at;
        true
    }

    /// How many instances of `instance_type` this endpoint holds: the fleet that is
    /// serving, plus the one a deployment in flight is standing up — SageMaker keeps the
    /// old fleet until the new one is serving, so an endpoint part-way through a
    /// deployment holds both.
    pub fn instances_of(&self, instance_type: &str) -> i64 {
        let serving = self.instance_usage.get(instance_type).copied().unwrap_or(0);
        let deploying = self
            .pending_update
            .as_ref()
            .map(|update| instances_of_type(update.variants(), instance_type))
            .unwrap_or(0);
        serving + deploying
    }
}

/// How long after `UpdateEndpoint` the clock owes the `Updating` -> `InService`
/// transition, on the same terms as [`ENDPOINT_IN_SERVICE_AFTER`]: a caller that
/// describes the endpoint straight after asking for the deployment still observes
/// `Updating` with the old configuration serving, and one that waits observes `InService`
/// with the new one. The span itself is that one value, so the two cannot drift apart.
pub const ENDPOINT_UPDATED_AFTER: Duration = ENDPOINT_IN_SERVICE_AFTER;

/// How long after `CreateInferenceComponent` the clock owes that component the
/// `Creating` -> `InService` transition, on the same terms as
/// [`ENDPOINT_IN_SERVICE_AFTER`]: a caller that describes the component straight after
/// creating it still observes `Creating` with no copies running, and one that waits
/// observes `InService` with the copies it asked for. Compressed, never zero.
///
/// The span is measured from the later of the create and the instant the hosting
/// endpoint goes `InService`, because a component's copies run on that endpoint's
/// capacity and cannot be placed before it is serving.
pub const INFERENCE_COMPONENT_IN_SERVICE_AFTER: Duration = Duration::from_millis(300);

/// See [`MODEL_QUOTA`]: the same bound on the inference-component store, which
/// `CreateInferenceComponent` answers `ResourceLimitExceeded` past and
/// `DeleteInferenceComponent` releases a slot of.
pub const INFERENCE_COMPONENT_QUOTA: usize = 1_000;

/// An inference component created by `CreateInferenceComponent`, as
/// `DescribeInferenceComponent` replays it.
///
/// The endpoint it is hosted on is snapshotted by name AND by ARN, so the component
/// keeps reporting the endpoint it was deployed to even after that endpoint is deleted.
/// Exactly one of `specification` / `specifications` is ever set: the model doc for
/// `Specifications` says a caller uses "either this parameter or the singular
/// Specification parameter, but not both".
///
/// `in_service_at` is the instant the clock owes the `Creating` -> `InService`
/// transition; it is stored, never slept on, and materialised by whichever reporting
/// call first runs at or after it.
#[derive(Debug, Clone)]
pub struct InferenceComponent {
    pub name: String,
    pub arn: String,
    pub endpoint_name: String,
    pub endpoint_arn: String,
    pub variant_name: Option<String>,
    pub specification: Option<crate::ops::types::InferenceComponentSpecification>,
    pub specifications: Option<Vec<crate::ops::types::InferenceComponentSpecification>>,
    /// The image each container of its specifications deploys, keyed by the path the
    /// specification named it as and resolved by the create that stored this component
    /// ([`crate::state::deployed_image`]), so every read replays one resolution.
    pub deployed_images: BTreeMap<String, crate::ops::types::DeployedImage>,
    pub runtime_config: Option<crate::ops::types::InferenceComponentRuntimeConfig>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub tags: Vec<Tag>,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub in_service_at: SystemTime,
}

impl InferenceComponent {
    /// Materialise the `Creating` -> `InService` transition the clock already owes; see
    /// [`settle_creating`].
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        settle_creating(
            &mut self.status,
            &mut self.last_modified_time,
            self.in_service_at,
            now,
        )
    }

    /// The specifications this component deploys, whether the caller sent the singular
    /// `Specification` or the `Specifications` list: one slice both readers walk.
    ///
    /// A stored record carries at most ONE of the two members —
    /// `CreateInferenceComponent` refuses a request that sends both — so the
    /// both-populated pair is not a case this has to choose between.
    pub fn specifications(&self) -> &[crate::ops::types::InferenceComponentSpecification] {
        match (&self.specification, &self.specifications) {
            (Some(single), None) => ::std::slice::from_ref(single),
            (None, Some(list)) => list.as_slice(),
            _ => &[],
        }
    }
}
