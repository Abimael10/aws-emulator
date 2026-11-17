// SPDX-License-Identifier: CC0-1.0

//! @op PutDeploymentParameter
//!
//! Writes one deployment parameter — its TEXT into a secret in AWS Secrets Manager,
//! its identity into [`crate::state`] — and reports it back from what was stored.
//!
//! The value is NOT this service's to hold: `deploymentParameter` is "the deployment
//! parameter targeted to the acceptor of an agreement for which to create the AWS
//! Secret Manager resource" and its `secretString` is "the text to encrypt and store
//! in the secret", so the write this operation performs is a Secrets Manager secret,
//! which is what lets a Quick Launch CloudFormation template resolve the value as a
//! Secrets Manager dynamic reference. [`crate::state::deployment_secret`] is that
//! half of the write: a create puts the text into a new secret named for the
//! resource this operation minted, a repeat write for the same identity stakes
//! another VERSION of that same secret, and the deletion `expirationDate` makes due
//! takes the secret with the record. A text Secrets Manager would not take is this
//! write FAILING (HTTP 500) and nothing is recorded for it: a deployment parameter
//! whose value no caller can read is not a deployment parameter.
//!
//! Because that write awaits, it cannot be made while this service's store guard is
//! held, so a call decides what to write under the guard, writes the secret, then
//! commits.
//!
//! A deployment parameter is keyed on (catalog, productId, agreementId,
//! deploymentParameter.name): the model's own doc for
//! `DeploymentParameterInput$name` says the name "is the identifier on which
//! deployment parameters are keyed for a given buyer and product. If this name
//! matches an existing deployment parameter, this request will update the existing
//! resource." So a second call for the same identity UPDATES in place and reports
//! the SAME `deploymentParameterId` and `resourceArn`; it never mints a second one.
//!
//! `tags` is applied on the CREATE only — `PutDeploymentParameterRequest$tags`
//! documents that "Tags will only be applied for create operations, and they'll be
//! ignored if the resource already exists" — and the response's `tags` is the
//! resource's stored tag map, which `TagResource` and `UntagResource` also write.
//!
//! That response member is the model's CONSTRAINED copy of the tag map, while the
//! same stored set is also reported through the copy the model leaves
//! unconstrained (`ListTagsForResourceResponse$tags` targets `Tags`:
//! `smithy_query(query="shape",shape="Tags,TagsMap")`), so the two members the
//! model gives one resource's tags state different things about what a tag may
//! hold. What they agree on is WHOSE tags they report, and that is what is
//! reported here: the set the record holds at the instant the call is served,
//! entry for entry, the same set `ListTagsForResource` answers with. A key or
//! value AWS's documented tag character set admits but the constrained copy does
//! not — one carrying a space, one of letters outside ASCII, an empty value —
//! reaches the record through `TagResource`, whose member targets the
//! unconstrained map, and is reported here unchanged: the constrained copy's
//! `@pattern` and `@length` bind the tag set a REQUEST may carry through a member
//! that targets it, which is why this operation refuses such a set on the way IN,
//! and dropping those entries from the response would report a tag set the
//! resource does not have and make the two reporting members disagree about one
//! record.
//!
//! `expirationDate` is the parameter's DEADLINE: it is "the date when deployment
//! parameters expire and are scheduled for deletion", so once it passes the
//! parameter is gone ([`crate::state::ComAmazonawsMarketplacedeploymentAwsmpdeploymentparametersserviceState::purge_expired`],
//! applied here before this request resolves anything) and a call for the same
//! identity creates a NEW resource rather than updating the expired one. It is
//! also OPTIONAL, so an update that omits it leaves the stored date alone: only
//! the members a request carries are applied. On the wire it is an RFC 3339
//! date-time string — the member is `@timestampFormat("date-time")`, and
//! [`super::types::PutDeploymentParameterInput`]'s schema carries that format — so
//! a value in any other JSON form is refused before this handler runs.
//!
//! A write is TARGETED: the operation "is targeted by `catalog` and
//! `agreementId`" for the product its URI names, and that target — an agreement
//! of the account covering that product ([`crate::state::ProvisionedAgreement`])
//! — is created out of band of this API: in AWS an agreement appears when a buyer
//! accepts a seller's offer, and this model declares no operation that creates a
//! product or an agreement. So the target is a PRECONDITION this operation
//! RESOLVES and never one the write establishes: a request whose
//! (catalog, productId, agreementId) the store does not hold is the modelled
//! `ResourceNotFoundException` (HTTP 404) naming the segment that did not resolve
//! — the resource the request's URI PATH addresses (`productId` inside `catalog`)
//! before the agreement its BODY names
//! ([`crate::state::ComAmazonawsMarketplacedeploymentAwsmpdeploymentparametersserviceState::unresolved_target`]),
//! with nothing written. The targets themselves get into the store out of band of
//! any request, from the fixture set a fresh store holds
//! ([`crate::state::DEFAULT_AGREEMENTS`]) or from
//! [`crate::state::seed_agreements`].
//!
//! `catalog` is checked FIRST, and against the set of catalogs that exist
//! ([`crate::state::SUPPORTED_CATALOGS`]): the model documents the member as a
//! fixed value, so a syntactically valid name outside that set names no catalog
//! to write inside, and the request is the modelled `ValidationException`
//! (HTTP 400) with nothing written — so a bad catalog stays a 400 and never
//! becomes the target check's 404.
//!
//! The request never reaches this handler at all when the front door refuses it:
//! a `@sensitive` payload past its modelled maximum length is answered with HTTP
//! 403 `ForbiddenException` in [`crate::service`], ahead of the modelled walk.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `PutDeploymentParameter::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `PutDeploymentParameter`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice/smithy-model/com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice.json")
//! ```
//!
//! Creates or updates a deployment parameter and is targeted by catalog and agreementId.
//!
//! ## Input
//!
//! * `agreementId` (ResourceId, len 1..64, pattern ^[A-Za-z0-9_/-]+$) **required** — The
//!   unique identifier of the agreement.
//! * `catalog` (Catalog, len 1..64, pattern ^[a-zA-Z_-]+$) **required** — The catalog related
//!   to the request. Fixed value: AWSMarketplace
//! * `clientToken` (ClientToken, len 32..64, pattern ^[a-zA-Z0-9/_+=.:@-]+$,
//!   @idempotencyToken) — The idempotency token for deployment parameters. A unique
//!   identifier for the new version. This field is not required if you're calling using an
//!   AWS SDK. Otherwise, a clientToken must be provided with the request.
//! * `deploymentParameter` (DeploymentParameterInput) **required** — The deployment parameter
//!   targeted to the acceptor of an agreement for which to create the AWS Secret Manager
//!   resource.
//! * `expirationDate` (Timestamp) — The date when deployment parameters expire and are
//!   scheduled for deletion.
//! * `productId` (ResourceId, len 1..64, pattern ^[A-Za-z0-9_/-]+$) **required** — The
//!   product for which AWS Marketplace will save secrets for the buyer’s account.
//! * `tags` (TagsMap, len ..50) — A map of key-value pairs, where each pair represents a tag
//!   saved to the resource. Tags will only be applied for create operations, and they'll be
//!   ignored if the resource already exists.
//!
//! ## Output
//!
//! * `agreementId` (ResourceId, len 1..64, pattern ^[A-Za-z0-9_/-]+$) **required** — The
//!   unique identifier of the agreement.
//! * `deploymentParameterId` (DeploymentParameterResourceIdentifier, len 1..32, pattern
//!   ^dp-[a-zA-Z0-9]+$) **required** — The unique identifier of the deployment parameter.
//! * `resourceArn` (ResourceArn, len 1..2048, pattern ^[a-zA-Z0-9:*/-]+$) **required** — The
//!   Amazon Resource Name (ARN) associated with the deployment parameter resource you want to
//!   create or update.
//! * `tags` (TagsMap, len ..50) — A map of key-value pairs, where each pair represents a tag
//!   saved to the resource. Tags will only be applied for create operations, and they'll be
//!   ignored if the resource already exists.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — You do not have sufficient access to perform this
//!   action.
//! * `ConflictException` (HTTP 409) — The request configuration has conflicts. For details,
//!   see the accompanying error message.
//! * `InternalServerException` (HTTP 500) — There was an internal service exception.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource wasn't found.
//! * `ServiceQuotaExceededException` (HTTP 402) — The maximum number of requests per account
//!   has been exceeded.
//! * `ThrottlingException` (HTTP 429) — Too many requests.
//! * `ValidationException` (HTTP 400) — An error occurred during validation.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `PutDeploymentParameterError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-marketplacedeployment-awsmpdeploymentparametersservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `PutDeploymentParameterError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.marketplacedeployment#PutDeploymentParameter")]
#[derive(Debug)]
pub enum PutDeploymentParameterErrorModeled {
    AccessDeniedException(AccessDeniedException),
    ConflictException(ConflictException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceQuotaExceededException(ServiceQuotaExceededException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::put_deployment_parameter::PutDeploymentParameterError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type PutDeploymentParameterError = crate::scaffolded::Scaffolded<PutDeploymentParameterErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<PutDeploymentParameterErrorModeled> for PutDeploymentParameterError {
    fn from(modeled: PutDeploymentParameterErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `PutDeploymentParameter` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct PutDeploymentParameter<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> PutDeploymentParameter<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for PutDeploymentParameter<Eng>
{
    type Input = PutDeploymentParameterInput;
    type Output = PutDeploymentParameterOutput;
    type Error = PutDeploymentParameterError;

    const NAME: &'static str = "PutDeploymentParameter";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::PUT_DEPLOYMENT_PARAMETER_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::PUT_DEPLOYMENT_PARAMETER_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The CATALOG first, before any state is touched: `catalog` is an
        // allowed-value member — the model documents it as "the catalog related
        // to the request. Fixed value: AWSMarketplace" — so a value outside
        // `crate::state::SUPPORTED_CATALOGS` names no catalog at all, however
        // well-formed it is. `Catalog`'s own `length 1..64` / `^[a-zA-Z_-]+$`
        // (refused by the centralized validate step ahead of this handler) is the
        // SYNTAX of a catalog name; this is whether the name denotes one. It is a
        // modelled `ValidationException` (HTTP 400), and it is answered BEFORE
        // the product/agreement target is touched: there is no catalog to write a
        // deployment parameter inside, so nothing is stored and no target is
        // recorded for it.
        if !crate::state::is_supported_catalog(&input.catalog) {
            return Err(PutDeploymentParameterErrorModeled::ValidationException(
                crate::state::unsupported_catalog(),
            )
            .into());
        }

        // The TAGS next, still before any state is touched: the modelled `tags`
        // member is judged on the INPUT, so the refusal does not depend on whether
        // this call would create the parameter (and apply `tags`) or update one that
        // exists (where the member documents that `tags` is ignored). A key in the
        // namespace AWS reserves for its own generated tags, a key or value outside
        // its length bound, and a key outside the character class this member's own
        // `TagsMap` states are all bad requests, answered here rather than at the
        // merge below — which runs the same judgement, so no stored record can
        // disagree with it. Nothing is stored either way.
        //
        // A set past the NUMBER of tags one resource holds keeps the tagging front
        // end's own identity rather than this service's modelled error
        // (`crate::state::TagRefusal`), which is why the refusal is lifted through
        // the channel instead of being wrapped in a modelled variant here.
        if let Some(tags) = input.tags.as_ref() {
            if let Err(why) =
                crate::state::validate_tags_member(tags, crate::state::TagConstraints::TagsMap)
            {
                return Err(why.into_error(PutDeploymentParameterErrorModeled::ValidationException));
            }
        }

        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(PutDeploymentParameterErrorModeled::InternalServerException(
                crate::state::store_unavailable(),
            )
            .into());
        };

        let region = crate::state::request_region(ctx.parts());
        let description = crate::state::deployment_secret::secret_description(
            &input.catalog,
            &input.product_id,
            &input.agreement_id,
            &input.deployment_parameter.name,
        );

        // The write has TWO halves — the Secrets Manager secret the text goes into,
        // which belongs to that service, and this service's own record — and the
        // secret cannot be written while this store's guard is held, because that
        // call awaits. So each pass decides what to write against the store as it
        // stands, writes the secret, and then commits; a commit the store no longer
        // admits (the identity was taken, or the record the pass resolved is gone)
        // starts another pass against what the store holds now, which is the same
        // answer the serialised order would have given. The passes are BOUNDED:
        // overtaken that many times, the request is the modelled `ConflictException`
        // rather than a loop.
        for _ in 0..COMMIT_PASSES {
            let plan = {
                // The store first, then the clock: this call is served at the
                // instant it reaches the store, so the expiry it materialises is
                // read off the clock once the lock is held, never from before the
                // wait for it.
                let mut store = state.lock();
                let now = ::std::time::SystemTime::now();
                // The expiry the clock has already made due, BEFORE this request
                // resolves anything: `expirationDate` is "the date when deployment
                // parameters expire and are scheduled for deletion", so a parameter
                // past its date is gone whether or not anybody called, and this
                // write must not update it. With it purged, a request for that
                // identity finds no record and mints a fresh resource — which is
                // what AWS answers once the old one has been deleted.
                store.purge_expired(now);
                plan_write(&mut store, &region, &input)?
            };

            // The deletions that purge made due, carried out now the guard is
            // released: an expired parameter's text must not outlive it, and
            // Secrets Manager is the side that owns it.
            crate::state::deployment_secret::release_expired_secrets(
                &*self.engine,
                ctx,
                &region,
            )
            .await;

            let plan = match plan {
                // The `@idempotencyToken` replay: the write already happened, and
                // its secret already holds its text, so nothing is written again.
                Plan::Replay(output) => return Ok(output),
                plan => plan,
            };

            // The VALUE, written where it belongs: `secretString` is "the text to
            // encrypt and store in the secret" and `deploymentParameter` is the
            // parameter "for which to create the AWS Secret Manager resource", so
            // this is the write this operation performs. A value that could not be
            // stored is this write failing — the deployment parameter IS that
            // secret — so nothing is recorded for it.
            let secret_arn = match crate::state::deployment_secret::store_secret(
                ctx,
                &region,
                plan.secret_name(),
                &description,
                plan.recorded_secret(),
                &input.deployment_parameter.secret_string,
            )
            .await
            {
                Ok(secret_arn) => secret_arn,
                Err(detail) => {
                    return Err(
                        PutDeploymentParameterErrorModeled::InternalServerException(
                            crate::state::deployment_secret::secret_write_failed(&detail),
                        )
                        .into(),
                    )
                }
            };

            // The record, committed against the store as it stands now. The
            // instants it stamps are sampled here rather than in the pass above:
            // the record is written when the commit reaches the store, so writes
            // the lock serialises stamp it in the order they actually wrote it.
            let committed = {
                let mut store = state.lock();
                let now = ::std::time::SystemTime::now();
                commit_write(&mut store, now, &plan, &input, secret_arn)?
            };
            if let Some(output) = committed {
                return Ok(output);
            }
        }
        Err(PutDeploymentParameterErrorModeled::ConflictException(
            crate::state::concurrent_write_conflict(
                &input.product_id,
                &input.deployment_parameter.name,
            ),
        )
        .into())
    }
}

/// How many times one request decides-writes-commits before it is answered as a
/// conflict.
///
/// A pass is started again only when ANOTHER write of the same identity landed
/// between this one's decision and its commit, and the second pass is made against
/// that write's own record — so one retry is what the condition needs, and the
/// bound is what keeps a request that keeps being overtaken from looping.
const COMMIT_PASSES: usize = 2;

/// What one pass of this operation is going to do, decided under the store guard.
enum Plan {
    /// The `@idempotencyToken` names a write already made: that write's answer,
    /// read out of the record it left.
    Replay(PutDeploymentParameterOutput),
    /// The identity the store holds no record for: mint the resource, create the
    /// secret for it, and insert the record.
    Mint {
        /// The identity the record is keyed on.
        key: crate::state::ParameterKey,
        /// The `dp-…` id minted for it.
        deployment_parameter_id: String,
        /// The ARN minted for it.
        resource_arn: String,
        /// The name the secret holding its text is created under.
        secret_name: String,
        /// The tag map the create applies, already merged and judged.
        tags: ::std::collections::BTreeMap<String, String>,
    },
    /// The identity the store already holds: stake another version of that
    /// record's own secret and rewrite the record in place.
    Rewrite {
        /// The record's ARN, which this pass resolved it by.
        resource_arn: String,
        /// Its secret's name, kept so a secret that is no longer there can be
        /// created again under the name it had.
        secret_name: String,
        /// The ARN of the secret this record's text is held in.
        secret_arn: String,
    },
}

impl Plan {
    /// The name the secret this pass writes is held under.
    fn secret_name(&self) -> &str {
        match self {
            Plan::Replay(_) => "",
            Plan::Mint { secret_name, .. } | Plan::Rewrite { secret_name, .. } => secret_name,
        }
    }

    /// The secret this service has ALREADY recorded for the parameter, when it has
    /// one: a write for it stakes a new version of that secret rather than creating
    /// a second one.
    fn recorded_secret(&self) -> Option<&str> {
        match self {
            Plan::Replay(_) | Plan::Mint { .. } => None,
            Plan::Rewrite { secret_arn, .. } => Some(secret_arn),
        }
    }
}

/// Decide what this request writes, against the store as it stands.
///
/// Everything here is a judgement on the request and on stored state, and nothing
/// here writes anything the caller can observe: the only state it changes is the
/// `dp-…` id counter a mint draws from, which hands out one id per mint and never
/// reuses one.
///
/// The order is the order the service applies it:
///
/// 1. the write's TARGET. `PutDeploymentParameter` "creates or updates a deployment
///    parameter and is targeted by catalog and agreementId" for the product the URI
///    addresses, and both the product and the agreement that target names are
///    created out of band of this API — nothing this model declares creates a
///    product or an agreement. So the target is a precondition this operation
///    RESOLVES: a request whose triple the store does not hold is the modelled
///    `ResourceNotFoundException` (HTTP 404), blamed on the segment that did not
///    resolve — the resource the request's URI PATH addresses (`productId` inside
///    `catalog`) first, then the agreement its BODY names
///    (`crate::state::ComAmazonawsMarketplacedeploymentAwsmpdeploymentparametersserviceState::unresolved_target`
///    owns that order) — and nothing is written for it.
///
///    The modelled input constraints (every `@pattern`, every `@length`) are
///    already refused with `ValidationException` (HTTP 400) by the centralized
///    validate step that runs before the handler — except an oversized `@sensitive`
///    payload, which the front door refuses with HTTP 403 `ForbiddenException`
///    ahead of that step, and a `tags` map past the number of tags one resource
///    holds, which the tagging front end refuses with its own HTTP 400
///    `BadRequestException` (both in `crate::service`);
/// 2. the `clientToken` replay. It is the `@idempotencyToken`: a token already
///    bound to a write replays that write rather than performing it again, and the
///    SAME token aimed at a DIFFERENT parameter is a conflicting request. Resolved
///    AFTER the target, deliberately: the target is a judgement on THIS request's
///    (catalog, productId, agreementId), so it is answered before the request is
///    matched against an earlier write. The two cannot disagree — a target already
///    held stays held (`crate::state::MAX_PROVISIONED_TARGETS` refuses a new target
///    rather than evicting one), so the target of a write this token replays still
///    resolves;
/// 3. the identity index, which says whether this call mints or rewrites.
fn plan_write(
    store: &mut crate::state::ComAmazonawsMarketplacedeploymentAwsmpdeploymentparametersserviceState,
    region: &str,
    input: &PutDeploymentParameterInput,
) -> Result<Plan, PutDeploymentParameterError> {
    if let Some(member) =
        store.unresolved_target(&input.catalog, &input.product_id, &input.agreement_id)
    {
        return Err(PutDeploymentParameterErrorModeled::ResourceNotFoundException(
            crate::state::target_not_found(member),
        )
        .into());
    }

    // The identity this request names, resolved ONCE: a deployment parameter is
    // keyed on (catalog, productId, agreementId, deploymentParameter.name)
    // (`crate::state::ParameterKey`), and every judgement below that asks "which
    // parameter is this request about?" — the `clientToken` replay check and the
    // identity-index lookup — compares this one value, so the two cannot come to
    // judge different identities.
    let key = parameter_key(input);

    if let Some(token) = input.client_token.as_deref() {
        if let Some(arn) = store.client_tokens.get(token).cloned() {
            if let Some(record) = store.parameters.get(&arn) {
                if record.key() == key {
                    return Ok(Plan::Replay(output_of(record)));
                }
                return Err(PutDeploymentParameterErrorModeled::ConflictException(
                    crate::state::client_token_conflict(token, &record.deployment_parameter_id),
                )
                .into());
            }
        }
    }

    match store.by_name.get(&key).cloned() {
        // The parameter already exists: this call rewrites its content in place,
        // and its text goes into the secret that record already names.
        Some(resource_arn) => {
            let Some(record) = store.parameters.get(&resource_arn) else {
                // The two indexes are written together, so this cannot happen; a
                // missing record is a store fault, not a client error.
                return Err(store_inconsistent());
            };
            Ok(Plan::Rewrite {
                resource_arn,
                secret_name: record.secret_name.clone(),
                secret_arn: record.secret_arn.clone(),
            })
        }
        None => {
            // This operation's `tags` member targets the modelled CONSTRAINED tag
            // map, so its keys are held to that map's key character class as well as
            // to the tagging limits every merge keeps (the handler's walk has
            // already refused a value outside the map's value shape).
            let tags = match crate::state::merge_tags(
                &::std::collections::BTreeMap::new(),
                &input.tags.clone().unwrap_or_default(),
                crate::state::TagConstraints::TagsMap,
            ) {
                Ok(tags) => tags,
                Err(why) => {
                    return Err(
                        why.into_error(PutDeploymentParameterErrorModeled::ValidationException)
                    )
                }
            };
            if !store.has_capacity() {
                return Err(
                    PutDeploymentParameterErrorModeled::ServiceQuotaExceededException(
                        crate::state::parameter_quota_exceeded(store.parameters.len()),
                    )
                    .into(),
                );
            }
            let deployment_parameter_id = store.mint_id();
            let resource_arn =
                crate::state::deployment_parameter_arn(region, &deployment_parameter_id);
            let secret_name =
                crate::state::deployment_secret::deployment_parameter_secret_name(
                    &deployment_parameter_id,
                );
            Ok(Plan::Mint {
                key,
                deployment_parameter_id,
                resource_arn,
                secret_name,
                tags,
            })
        }
    }
}

/// Commit the write `plan` describes, now that its secret holds the request's text,
/// and report the answer read out of the stored record.
///
/// `None` means the store no longer admits this commit — another write of the same
/// identity landed while this one was writing its secret — so the caller decides the
/// request again against what the store holds now. The secret this pass wrote is
/// queued for deletion in that case, so a pass that is started again leaves nothing
/// standing: the record that won the identity names its own secret.
fn commit_write(
    store: &mut crate::state::ComAmazonawsMarketplacedeploymentAwsmpdeploymentparametersserviceState,
    now: ::std::time::SystemTime,
    plan: &Plan,
    input: &PutDeploymentParameterInput,
    secret_arn: String,
) -> Result<Option<PutDeploymentParameterOutput>, PutDeploymentParameterError> {
    let resource_arn = match plan {
        // Decided and answered in the pass that read it; nothing to commit.
        Plan::Replay(_) => return Ok(None),
        Plan::Mint {
            key,
            deployment_parameter_id,
            resource_arn,
            secret_name,
            tags,
        } => {
            if store.by_name.contains_key(key) {
                store.queue_expired_secret(secret_arn);
                return Ok(None);
            }
            if !store.has_capacity() {
                store.queue_expired_secret(secret_arn);
                return Err(
                    PutDeploymentParameterErrorModeled::ServiceQuotaExceededException(
                        crate::state::parameter_quota_exceeded(store.parameters.len()),
                    )
                    .into(),
                );
            }
            store.parameters.insert(
                resource_arn.clone(),
                crate::state::DeploymentParameterRecord {
                    resource_arn: resource_arn.clone(),
                    deployment_parameter_id: deployment_parameter_id.clone(),
                    catalog: input.catalog.clone(),
                    product_id: input.product_id.clone(),
                    agreement_id: input.agreement_id.clone(),
                    name: input.deployment_parameter.name.clone(),
                    secret_name: secret_name.clone(),
                    secret_arn,
                    expiration_date: input.expiration_date,
                    tags: tags.clone(),
                    created_at: now,
                    updated_at: now,
                },
            );
            store.by_name.insert(key.clone(), resource_arn.clone());
            resource_arn.clone()
        }
        Plan::Rewrite {
            resource_arn,
            secret_name,
            ..
        } => {
            let Some(record) = store.parameters.get_mut(resource_arn) else {
                // The record this pass resolved is gone — its `expirationDate` fell
                // due, or another call took the identity — so this commit is not the
                // one to make: the identity is decided again, and the secret this
                // pass wrote goes with the record that no longer holds it.
                store.queue_expired_secret(secret_arn.clone());
                return Ok(None);
            };
            // The secret the record names is the one Secrets Manager answered with
            // for this write, which is the secret it already had unless that secret
            // had to be created again under the same name.
            record.secret_name = secret_name.clone();
            record.secret_arn = secret_arn;
            // `expirationDate` is OPTIONAL, so an update that omits it says nothing
            // about expiry and leaves the stored date as it was: only the members the
            // request carries are applied, and an absent member is not an instruction
            // to clear persisted state. A request that does name a date replaces it.
            if let Some(expiration_date) = input.expiration_date {
                record.expiration_date = Some(expiration_date);
            }
            record.mark_written(now);
            resource_arn.clone()
        }
    };

    if let Some(token) = input.client_token.clone() {
        store.remember_client_token(token, resource_arn.clone());
    }

    let Some(record) = store.parameters.get(&resource_arn) else {
        return Err(store_inconsistent());
    };
    Ok(Some(output_of(record)))
}

/// The identity a request names: (catalog, productId, agreementId,
/// deploymentParameter.name), as [`crate::state::ParameterKey`] defines it.
fn parameter_key(input: &PutDeploymentParameterInput) -> crate::state::ParameterKey {
    (
        input.catalog.clone(),
        input.product_id.clone(),
        input.agreement_id.clone(),
        input.deployment_parameter.name.clone(),
    )
}

/// The modelled `InternalServerException` for the two indexes of one record
/// disagreeing, which they are written together to prevent.
fn store_inconsistent() -> PutDeploymentParameterError {
    PutDeploymentParameterErrorModeled::InternalServerException(InternalServerException {
        message: "the deployment-parameter store is inconsistent".to_string(),
    })
    .into()
}

/// The response for `record`, read entirely out of the stored record — its tag
/// map included, reported as stored for the reason the module header gives.
fn output_of(record: &crate::state::DeploymentParameterRecord) -> PutDeploymentParameterOutput {
    PutDeploymentParameterOutput {
        resource_arn: record.resource_arn.clone(),
        agreement_id: record.agreement_id.clone(),
        deployment_parameter_id: record.deployment_parameter_id.clone(),
        tags: Some(record.tags_map()),
    }
}
