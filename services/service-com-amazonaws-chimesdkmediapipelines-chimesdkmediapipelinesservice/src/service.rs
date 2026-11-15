// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-chimesdkmediapipelines-chimesdkmediapipelinesservice` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Five opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.

use smithy_server_runtime::{
    http, ErasedOperation, Operation, Refusal, Refused, Service,
};
use std::collections::HashMap;
use std::sync::Arc;

pub mod authorization;

/// This service's own `aws.api#service$endpointPrefix`, the name that identifies it
/// among the six services that sign as `chime`.
///
/// Source: `smithy_query(query="service_info", model_path="services/\
/// service-com-amazonaws-chimesdkmediapipelines-chimesdkmediapipelinesservice/\
/// smithy-model/com-amazonaws-chimesdkmediapipelines-chimesdkmediapipelinesservice.json")`
/// — `awsService.endpointPrefix`.
const OWN_ENDPOINT_PREFIX: &str = "media-pipelines-chime";

/// The `com-amazonaws-chimesdkmediapipelines-chimesdkmediapipelinesservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// One [`authorization::Gated`] per operation this service publishes, keyed by the
    /// routing key the model names that operation by ([`Operation::NAME`], the same key the
    /// generated routing table uses). Built once here, never per request; each entry carries
    /// the operation's TYPE and a handle on the one shared engine, so this is a table of
    /// names rather than a second set of operations.
    gated: HashMap<&'static str, Box<dyn ErasedOperation>>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            gated: gated_table::<Eng>(&engine),
            entry: crate::ServiceDefinition::new(engine),
        }
    }
}

/// The [`authorization::Gated`] wrapper for every operation this service publishes
/// (`smithy_query(query="list_operations")`, all 31 of them).
///
/// Authorization is a property of the request's identity, the action it names and the
/// resource it addresses rather than of what a handler goes on to do, so it is taken here
/// for all of them at once — implemented or still scaffolded, and whatever error set the
/// operation itself declares. A stub answers 501 for a request whose caller IAM permits and
/// the refusal otherwise, which is the order the real service evaluates them in; leaving an
/// operation out would serve an unauthorized caller, the very divergence this removes.
fn gated_table<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> HashMap<&'static str, Box<dyn ErasedOperation>> {
    let mut wrappers: HashMap<&'static str, Box<dyn ErasedOperation>> = HashMap::new();
    macro_rules! register {
        ($($module:ident :: $operation:ident),+ $(,)?) => {
            $(
                wrappers.insert(
                    <crate::ops::$module::$operation<Eng> as Operation>::NAME,
                    Box::new(authorization::Gated::<
                        crate::ops::$module::$operation<Eng>,
                        Eng,
                    >::new(Arc::clone(engine))) as Box<dyn ErasedOperation>,
                );
            )+
        };
    }
    register!(
        create_media_capture_pipeline::CreateMediaCapturePipeline,
        create_media_concatenation_pipeline::CreateMediaConcatenationPipeline,
        create_media_insights_pipeline::CreateMediaInsightsPipeline,
        create_media_insights_pipeline_configuration::CreateMediaInsightsPipelineConfiguration,
        create_media_live_connector_pipeline::CreateMediaLiveConnectorPipeline,
        create_media_pipeline_kinesis_video_stream_pool::CreateMediaPipelineKinesisVideoStreamPool,
        create_media_stream_pipeline::CreateMediaStreamPipeline,
        delete_media_capture_pipeline::DeleteMediaCapturePipeline,
        delete_media_insights_pipeline_configuration::DeleteMediaInsightsPipelineConfiguration,
        delete_media_pipeline::DeleteMediaPipeline,
        delete_media_pipeline_kinesis_video_stream_pool::DeleteMediaPipelineKinesisVideoStreamPool,
        get_media_capture_pipeline::GetMediaCapturePipeline,
        get_media_insights_pipeline_configuration::GetMediaInsightsPipelineConfiguration,
        get_media_pipeline::GetMediaPipeline,
        get_media_pipeline_kinesis_video_stream_pool::GetMediaPipelineKinesisVideoStreamPool,
        get_speaker_search_task::GetSpeakerSearchTask,
        get_voice_tone_analysis_task::GetVoiceToneAnalysisTask,
        list_media_capture_pipelines::ListMediaCapturePipelines,
        list_media_insights_pipeline_configurations::ListMediaInsightsPipelineConfigurations,
        list_media_pipeline_kinesis_video_stream_pools::ListMediaPipelineKinesisVideoStreamPools,
        list_media_pipelines::ListMediaPipelines,
        list_tags_for_resource::ListTagsForResource,
        start_speaker_search_task::StartSpeakerSearchTask,
        start_voice_tone_analysis_task::StartVoiceToneAnalysisTask,
        stop_speaker_search_task::StopSpeakerSearchTask,
        stop_voice_tone_analysis_task::StopVoiceToneAnalysisTask,
        tag_resource::TagResource,
        untag_resource::UntagResource,
        update_media_insights_pipeline_configuration::UpdateMediaInsightsPipelineConfiguration,
        update_media_insights_pipeline_status::UpdateMediaInsightsPipelineStatus,
        update_media_pipeline_kinesis_video_stream_pool::UpdateMediaPipelineKinesisVideoStreamPool,
    );
    wrappers
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // This service signs as `chime` — a signing name it SHARES with the five other
        // Amazon Chime services in this workspace, each of which serves its OWN endpoint
        // (`chime`, `identity-chime`, `meetings-chime`, `messaging-chime`,
        // `voice-chime`). The router resolves a request's routing key from the SigV4
        // credential scope first, so all six see the key `chime`: claiming on the signing
        // name alone makes every one of them a claimant for every one of the others'
        // requests, and the router — which refuses rather than guesses — answers
        // `500 AmbiguousService` for ALL of it, including this service's own
        // `/sdk-media-pipelines` traffic.
        //
        // So the claim is keyed on what identifies ONE service: the endpoint the request
        // addresses, and failing that this model's own URI path space. That rule, and the
        // readers for the path spaces the family shares, live once in `service-common`.
        //
        // Every path space this model binds is its own except `/tags`, which five of the
        // six publish identically: a request that names no endpoint is served by the
        // family's declared tagging owner, and this service serves that space at its OWN
        // endpoint from the same shared ARN-keyed tag directory, so both answer alike —
        // see `chime_family`. There is therefore no shared space left for this service to
        // claim on a request that names no endpoint.
        service_common::chime_family::claims_chime_family_request(
            parts,
            OWN_ENDPOINT_PREFIX,
            crate::ROUTE_PATH_SEGMENTS,
            |_| false,
        )
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // NOT a re-route: the key is the one the generated table's own protocol resolves,
        // and the operation it names is the one served — wrapped in
        // [`authorization::Gated`] so IAM's decision about the action is taken BEFORE the
        // handler runs, as AWS takes it ahead of the service. Every operation this service
        // publishes is in that table; a key the model does not name resolves to `None`
        // here, so the router's routing-error path is untouched.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.gated.get(key).map(::std::convert::AsRef::as_ref)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // A value one of this model's own constraints does not admit is refused at the
        // REQUEST BOUNDARY, ahead of any handler, and that layer is not this service: it is
        // the common front end AWS puts in front of a restJson1 service, which answers
        // `400 ValidationException` carrying the envelope
        // `N validation error(s) detected: <clause>; <clause>` over one clause per
        // violation. So a boundary refusal is NOT reworded into one of this service's own
        // modelled errors: `BadRequestException` is what a HANDLER of this service answers
        // for a rule the model cannot express, and the two stay distinguishable for a
        // client that branches on the code.
        //
        // Only the `@pattern` RENDERING is chosen here, and this service's dialect keeps the
        // expression's OPENING `^` and drops the `$` that closes it: a Smithy `@pattern` is
        // a find, so the trailing anchor is how the model makes the match total at that end
        // rather than part of the grammar a caller has to satisfy, while everything else the
        // model wrote is quoted as declared. That is the rendering this service's refusals
        // are recorded stating, and the shared home owns the rule
        // (`service_common::validation::unanchored_at_the_close`); the CHECK still runs the
        // model's pattern as written. Everything else — which refusals are restated, the
        // code, the envelope, the member paths and every other requirement — is the shared
        // `service_common::validation_message::aws_validation_refusal_with`'s and the
        // framework's, so this service cannot word one of them differently.
        //
        // An authentication refusal keeps the framework's wording: it is not an
        // input-constraint failure and is not this service's to restate.
        ::service_common::validation_message::aws_validation_refusal_with(
            refusal,
            ::service_common::validation::unanchored_at_the_close,
        )
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
        let _ = req;
        None
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the operations REGISTERED behind the authorization gate are exactly the ones
    /// [`authorization::GATED_OPERATIONS`] names — which this crate's own suite holds to the
    /// set the MODEL binds to the service shape.
    ///
    /// Without this, the `register!` list above could drop an operation while the constant
    /// still said it was judged, and that operation would serve an unauthorized caller. The
    /// table is built over a stand-in engine type carrying no state: this asserts which KEYS
    /// are registered, and no request is served.
    #[test]
    fn every_gated_operation_is_registered_behind_the_authorization_gate() {
        let registered = gated_table::<()>(&Arc::new(()));
        let mut keys: Vec<&str> = registered.keys().copied().collect();
        keys.sort_unstable();
        let mut gated: Vec<&str> = authorization::GATED_OPERATIONS.to_vec();
        gated.sort_unstable();
        assert_eq!(
            keys, gated,
            "case \"the registered table\": expected exactly the gated operations, got \
             {keys:?} against {gated:?}"
        );
    }
}
