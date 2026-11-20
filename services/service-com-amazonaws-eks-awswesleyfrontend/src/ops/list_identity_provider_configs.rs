// SPDX-License-Identifier: CC0-1.0

//! @op ListIdentityProviderConfigs
//!
//! IMPLEMENTED. `ListIdentityProviderConfigs` pages the identity-provider
//! configurations `AssociateIdentityProviderConfig` wrote onto the cluster's record in
//! the shared engine's store ([`crate::state::ClusterRecord::identity_provider_configs`]),
//! as the `(type, name)` pairs that ADDRESS them — the same pair
//! `DescribeIdentityProviderConfig` resolves one by, so the listing a caller walks and
//! the reads it then makes cannot disagree. It keeps no copy of its own and computes
//! nothing per request beyond materialising the status moves the clock already owes.
//!
//! ORDER: `clusterName` is this operation's `@httpLabel`, so the cluster is resolved
//! first and a name this Region does not hold is the service's 404 naming that cluster,
//! whatever else the request carries; a cluster `RegisterCluster` connected has no
//! Amazon EKS control plane to authenticate users to, so the listing does not apply to
//! it at all. A `nextToken` this listing did not mint names no page to resume from and
//! is the bad parameter it is, never an empty page.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ListIdentityProviderConfigs`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json")
//! ```
//!
//! Lists the identity provider configurations for your cluster.
//!
//! ## Input
//!
//! * `clusterName` (String) **required** — The name of your cluster.
//! * `maxResults` (ListIdentityProviderConfigsRequestMaxResults, range 1..100) — The maximum
//!   number of results, returned in paginated output. You receive maxResults in a single
//!   page, along with a nextToken response element. You can see the remaining results of the
//!   initial request by sending another request with the returned nextToken value. This value
//!   can be between 1 and 100. If you don't use this parameter, 100 results and a nextToken
//!   value, if applicable, are returned.
//! * `nextToken` (String) — The nextToken value returned from a previous paginated request,
//!   where maxResults was used and the results exceeded the value of that parameter.
//!   Pagination continues from the end of the previous results that returned the nextToken
//!   value. This value is null when there are no more results to return. This token should be
//!   treated as an opaque identifier that is used only to retrieve the next items in a list
//!   and not for other programmatic purposes.
//!
//! ## Output
//!
//! * `identityProviderConfigs` (IdentityProviderConfigs) — The identity provider
//!   configurations for the cluster.
//! * `nextToken` (String) — The nextToken value to include in a future
//!   ListIdentityProviderConfigsResponse request. When the results of a
//!   ListIdentityProviderConfigsResponse request exceed maxResults, you can use this value to
//!   retrieve the next page of results. This value is null when there are no more results to
//!   return. This token should be treated as an opaque identifier that is used only to
//!   retrieve the next items in a list and not for other programmatic purposes.
//!
//! ## Errors
//!
//! * `ClientException` (HTTP 400) — These errors are usually caused by a client action.
//!   Actions can include using an action or resource on behalf of an IAM principal that
//!   doesn't have permissions to use the action or resource or specifying an identifier that
//!   is not valid.
//! * `InvalidParameterException` (HTTP 400) — The specified parameter is invalid. Review the
//!   available parameters for the API request.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource could not be found. You
//!   can view your available clusters with ListClusters. You can view your available managed
//!   node groups with ListNodegroups. Amazon EKS clusters and node groups are Amazon Web
//!   Services Region specific.
//! * `ServerException` (HTTP 500) — These errors are usually caused by a server-side issue.
//! * `ServiceUnavailableException` (HTTP 503) — The service is unavailable. Back off and
//!   retry the operation.

#[allow(unused_imports)]
use super::types::*;
use crate::state::{
    self, DEFAULT_LIST_PAGE_SIZE, IDENTITY_PROVIDER_CONFIGS_LISTING,
    IDENTITY_PROVIDER_CONFIG_TYPE_OIDC,
};
use std::time::SystemTime;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListIdentityProviderConfigsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-eks-awswesleyfrontend.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListIdentityProviderConfigsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.eks#ListIdentityProviderConfigs")]
#[derive(Debug)]
pub enum ListIdentityProviderConfigsErrorModeled {
    ClientException(ClientException),
    InvalidParameterException(InvalidParameterException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServerException(ServerException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_identity_provider_configs::ListIdentityProviderConfigsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListIdentityProviderConfigsError = crate::scaffolded::Scaffolded<ListIdentityProviderConfigsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListIdentityProviderConfigsErrorModeled> for ListIdentityProviderConfigsError {
    fn from(modeled: ListIdentityProviderConfigsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListIdentityProviderConfigs` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListIdentityProviderConfigs<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListIdentityProviderConfigs<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListIdentityProviderConfigs<Eng>
{
    type Input = ListIdentityProviderConfigsInput;
    type Output = ListIdentityProviderConfigsOutput;
    type Error = ListIdentityProviderConfigsError;

    const NAME: &'static str = "ListIdentityProviderConfigs";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_IDENTITY_PROVIDER_CONFIGS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_IDENTITY_PROVIDER_CONFIGS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let region = state::request_region(ctx.parts());
        let store = crate::state::service_state(&*self.engine).ok_or_else(|| {
            ListIdentityProviderConfigsError::from(
                ListIdentityProviderConfigsErrorModeled::ServerException(ServerException {
                    cluster_name: Some(input.cluster_name.clone()),
                    message: Some("the eks emulator store is unavailable".to_string()),
                    ..Default::default()
                }),
            )
        })?;

        // `maxResults` is modelled `@range {min: 1, max: 100}`, so a value outside that
        // range is refused by the validating walk before this handler runs. Only the
        // ABSENT case gets a value here — "if you don't use this parameter, 100 results
        // and a nextToken value, if applicable, are returned" — and the floor of one is
        // what that bound already guarantees, written out so a page size of zero cannot
        // answer an empty page.
        let page_size = input
            .max_results
            .unwrap_or(DEFAULT_LIST_PAGE_SIZE)
            .max(1) as usize;

        let mut guard = store.lock();
        // The only write a reporting operation may make: the `CREATING` -> `ACTIVE`
        // moves were due whether or not anybody called.
        guard.materialize_due_transitions(SystemTime::now());

        // THE PARENT RESOURCE FIRST: a configuration is a sub-resource of its cluster,
        // and a cluster name identifies a cluster only within one Region — so a listing
        // of a cluster this Region does not hold is that 404, naming the cluster it
        // could not resolve, whatever else the request carries.
        let Some(cluster) = guard
            .clusters
            .get(&(region.clone(), input.cluster_name.clone()))
        else {
            return Err(
                ListIdentityProviderConfigsErrorModeled::ResourceNotFoundException(
                    crate::state::cluster_not_found(&input.cluster_name),
                )
                .into(),
            );
        };
        // An identity-provider configuration authenticates users to an Amazon EKS
        // control plane's API server, and a cluster `RegisterCluster` connected has no
        // such control plane: the operation does not apply to it, which is a refusal
        // rather than an empty listing — the listing is not something this service keeps
        // for that cluster at all. The same gate the reads of one keep.
        if cluster.is_connected() {
            return Err(ListIdentityProviderConfigsErrorModeled::ClientException(
                crate::state::connected_cluster_refusal(&input.cluster_name),
            )
            .into());
        }

        // A `nextToken` names the configuration to resume AFTER. One this listing did
        // not issue is a bad parameter, not an empty page.
        let resume_after = match input.next_token.as_deref() {
            None => None,
            Some(token) => Some(
                state::decode_scoped_page_token(IDENTITY_PROVIDER_CONFIGS_LISTING, token)
                    .ok_or_else(|| {
                        ListIdentityProviderConfigsError::from(
                            ListIdentityProviderConfigsErrorModeled::InvalidParameterException(
                                InvalidParameterException {
                                    cluster_name: Some(input.cluster_name.clone()),
                                    message: Some(
                                        state::INVALID_NEXT_TOKEN_MESSAGE.to_string(),
                                    ),
                                    ..Default::default()
                                },
                            ),
                        )
                    })?,
            ),
        };

        // The configurations are stored under their names, so the map's own order is
        // the one a page resumes through.
        let mut remaining = cluster
            .identity_provider_configs
            .keys()
            .filter(|name| {
                resume_after
                    .as_deref()
                    .is_none_or(|after| name.as_str() > after)
            });
        let identity_provider_configs: Vec<IdentityProviderConfig> = remaining
            .by_ref()
            .take(page_size)
            .map(|name| IdentityProviderConfig {
                // "The only type available is `oidc`"
                // (`IdentityProviderConfig$type`), which is the half of the pair that
                // says WHICH kind of provider each listed configuration is.
                r#type: IDENTITY_PROVIDER_CONFIG_TYPE_OIDC.to_string(),
                name: name.clone(),
            })
            .collect();
        // A token is returned only when something is left to resume from, so the last
        // page omits it: "this value is null when there are no more results to return".
        let next_token = match (remaining.next(), identity_provider_configs.last()) {
            (Some(_), Some(last)) => Some(state::encode_scoped_page_token(
                IDENTITY_PROVIDER_CONFIGS_LISTING,
                &last.name,
            )),
            _ => None,
        };

        Ok(ListIdentityProviderConfigsOutput {
            identity_provider_configs: Some(identity_provider_configs),
            next_token,
        })
    }
}
