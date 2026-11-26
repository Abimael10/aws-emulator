// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer: how a value the modelled member it is
//! bound to cannot be read from is refused.
//!
//! Two refusals happen before a handler runs, and they are not the same error class:
//!
//! * a value whose JSON class the modelled type of the member it is bound to cannot be read
//!   from — a quoted literal where the model declares an integer, a flag where it declares
//!   a string — never becomes a value at all. Nothing was converted and no rule of the model
//!   was reached, so AWS answers it at the DESERIALIZATION layer: the protocol-level
//!   [`SERIALIZATION_EXCEPTION`] (HTTP 400), whose sentence states the two facts the caller
//!   has to reconcile — the CLASS of JSON value it sent and the modelled type that value
//!   could not become ([`coercion_sentence`]);
//! * a value that WAS read and then broke a rule the model declares (an absent `@required`
//!   member, a `@length`/`@range`/`@pattern`/enum violation) is a CONSTRAINT refusal, which
//!   this service answers in AWS's validation envelope or in the operation's own modelled
//!   `InvalidInputException` (`crate::service`).
//!
//! The framework renders BOTH as `ValidationException` carrying the codec's own prose (see
//! `vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code), under a LOWERCASE `message` member, so a client branching on the error
//! code to decide whether to fix its serializer or fix a value takes the wrong branch and
//! finds the reason under a member a serialization refusal does not use. The vendored tree is
//! read-only from a service crate, so the separation is made here, at the one seam a service
//! has before dispatch ([`crate::service::EmulatedService::override_operation`]), as
//! `services/service-com-amazonaws-codebuild-codebuild-20161006` and
//! `services/service-com-amazonaws-dynamodb-dynamodb-20120810` do for their own request
//! layers.
//!
//! Nothing about the model is written down here. Which members a body may carry, and what
//! each one is read from, come off the operation's own input schema ([`input_schema`], which
//! resolves the routing key to the input shape the model binds to that operation); WHETHER a
//! value can be read into the member it is bound to, the NAME its class is stated under and
//! the SENTENCE itself are the workspace's one statement of those questions
//! (`service_common::json_wire_values`), so this layer cannot disagree with the request
//! layers of the other services that answer them. What this file decides is only which
//! modelled types this service states the sentence for ([`named_type`]).
//!
//! Two deliberate limits:
//!
//! * only the body's own TOP level is read, as a member `Schema` in this framework carries
//!   its target's kind but not its target's schema, so a value nested inside a list, a map or
//!   a structure is left to the codec exactly as it is today;
//! * only a member whose modelled type this protocol carries in exactly ONE JSON form is
//!   answered here ([`named_type`]) — everything else keeps the codec's own answer.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use serde_json::Value;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Service,
};

/// The error code a value no modelled member could be read from is refused with.
///
/// It is the PROTOCOL's error rather than one of this service's modelled shapes: the request
/// never became an input, so no operation ran to answer with an error of its own. The
/// framework already names this code for an awsJson body it cannot process at all
/// (`pre_dispatch.rs`'s catch-all arm), which is the same refusal one unreadable member
/// earns.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The status such a refusal carries: the caller sent something that cannot be read, so it is
/// a client error — the same 400 every refusal of this service's own input carries.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// Whether this layer refuses `req`: its body carries a value the modelled member it is bound
/// to cannot be read from.
pub(super) fn refuses(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> bool {
    sentence(definition, req).is_some()
}

/// The sentence refusing `req`, when a value its body carries cannot be read into the modelled
/// type of the member it is bound to.
///
/// `None` is "nothing for THIS layer to refuse", which covers several outcomes deliberately:
/// every value of the body reads; the body is not a JSON object, so no member of it can be
/// named (the framework's own rendering of an unreadable body stands); or the request names no
/// operation this service serves, which is the router's routing failure to answer.
fn sentence(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> Option<String> {
    let key = definition.protocol().routing_key(req).ok()?;
    let Some(input) = input_schema(key) else {
        // An operation this service SERVES whose input [`INPUT_SCHEMAS`] does not name keeps
        // the codec's own refusal, which is the divergence this layer exists to remove — so
        // it is said out loud rather than passed over silently. (The routing table is not
        // enumerable from outside the framework, so this is the one place the gap can be
        // noticed.)
        if definition.routing_table().get(key).is_some() {
            ::tracing::warn!(
                operation = key,
                "this service serves an operation whose modelled input this layer cannot \
                 resolve; a value its body carries that no modelled member can be read from \
                 keeps the codec's own refusal"
            );
        }
        return None;
    };
    unreadable_member(input, req.body().bytes().unwrap_or_default())
}

/// The first member of `body` whose JSON class the modelled type of the member of `input` it is
/// bound to cannot be read from, as the sentence refusing it.
///
/// A key naming no member of the input is ignored, exactly as the codec ignores it. "First"
/// is the body's own order — the order the caller wrote its members in, which is the order the
/// codec would have read them in, so the member named is the one the caller has to fix first.
/// (This workspace's JSON reader preserves that order; a reader that did not would name the
/// lowest wire name instead, which is still one deterministic member of the same body.)
fn unreadable_member(input: &Schema, body: &[u8]) -> Option<String> {
    let Ok(Value::Object(members)) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    members
        .iter()
        .filter_map(|(key, held)| {
            Some((
                ::service_common::json_wire_values::member_by_wire_name(input, key)?,
                held,
            ))
        })
        .find_map(|(member, held)| {
            let named = named_type(member.shape_type())?;
            (!::service_common::json_wire_values::reads_as(member.shape_type(), held)).then(|| {
                ::service_common::json_wire_values::unconverted_message(
                    ::service_common::json_wire_values::json_literal_class(held),
                    named,
                )
            })
        })
}

// ---------------------------------------------------------------------------
// The words the refusal states
// ---------------------------------------------------------------------------
//
// Both halves are the shared ones
// (`service_common::json_wire_values::unconverted_message` over `json_literal_class`): the
// sentence names the CLASS of value sent rather than the value — so a caller is never told
// its own value back, and one wording answers every value of that class — and states the
// modelled type bare, with no article and no member path, which is the form recorded for this
// refusal. The class naming is the one that states a boolean and a fraction by their LITERAL,
// as every service of this same front end whose recordings reach those classes records them
// (`services/service-com-amazonaws-emr-elasticmapreduce` and
// `services/service-com-amazonaws-dynamodb-dynamodb-20120810`); the recording this layer is
// built from is a quoted literal bound to an integer member, whose class (`STRING_VALUE`) is
// the same under either naming, so it does not discriminate them.
//
// WHICH modelled types this service states the sentence for is the one thing decided here.
//
// WHICH VALUES it states it for is the shared readability question's
// (`service_common::json_wire_values::reads_as`), so one of them is worth naming: a whole
// number whose magnitude the member's integral WIDTH does not hold reads as no value of that
// member either, and is refused by its class like every other value nothing could be read
// from. That is the same answer the sibling service of this front end states for it
// (`services/service-com-amazonaws-dynamodb-dynamodb-20120810/src/service/request_layer.rs`,
// whose own rows assert `NUMBER_VALUE cannot be converted to Integer` for a number past that
// width) rather than a second rule invented here.

/// The modelled type a refusal names for a member whose target is `target`, and `None` for a
/// target this layer states nothing about — which keeps the codec's own answer.
///
/// Named: the types this protocol carries in exactly ONE JSON form, so a value of any other
/// class was converted to nothing at all. The name is the type's own Smithy (and Java) name,
/// which is what the sentence states.
///
/// Not named, deliberately:
///
/// * `Float` and `Double` — this protocol carries a floating-point member's non-finite values
///   (`NaN`, `Infinity`, `-Infinity`) as STRINGS, so a quoted literal bound to one is readable
///   and a layer that refused it would refuse a request the codec accepts;
/// * `Blob` and `Timestamp` — read from more than one form too (base64 text, and either an
///   epoch number or a `@timestampFormat` string), and what a refusal of one states is the
///   FORM rather than the type;
/// * `Document`, which every JSON value reads into, and the aggregates (`List`, `Map`,
///   `Structure`, `Union`), where no conversion is attempted at all: AWS states those in other
///   words than this sentence, and no recording of this service shows them.
fn named_type(target: ShapeType) -> Option<&'static str> {
    Some(match target {
        ShapeType::Boolean => "Boolean",
        ShapeType::Byte => "Byte",
        ShapeType::Short => "Short",
        ShapeType::Integer => "Integer",
        ShapeType::Long => "Long",
        ShapeType::String => "String",
        _ => return None,
    })
}

/// How the model names the INPUT shape of an operation: this API spells it either way, so
/// both are resolved ([`input_schema`]). `GetTables` is served by `GetTablesRequest` and
/// `CheckSchemaVersionValidity` by `CheckSchemaVersionValidityInput`.
const INPUT_SHAPE_SUFFIXES: [&str; 2] = ["Request", "Input"];

/// The input schema of the operation `operation` names, or `None` when this service serves no
/// operation by that name.
///
/// Resolved off the schemas themselves ([`INPUT_SCHEMAS`]) rather than from a second table of
/// names written here, so every operation this service serves is reachable by the name its
/// routing key carries — the ones whose handlers are still scaffolded included — and the only
/// thing stated per operation is its schema.
fn input_schema(operation: &str) -> Option<&'static Schema> {
    INPUT_SCHEMAS.iter().copied().find(|input| {
        let shape = input.shape_id().shape_name();
        INPUT_SHAPE_SUFFIXES
            .iter()
            .any(|suffix| shape.strip_suffix(suffix) == Some(operation))
    })
}

/// Every operation input of this service, as the schema the framework reads a request into.
///
/// Listed in full rather than for the operations one recording happens to cover, because the
/// rule above is a property of the PROTOCOL and not of any one operation: a request to any of
/// them carrying a value its modelled member cannot be read from is the same refusal. The
/// framework's type-erased operation surface carries no input schema
/// (`vendor/smithy-server-runtime/src/service.rs`), so the schemas are named here, one line
/// each, exactly as
/// `services/service-com-amazonaws-codebuild-codebuild-20161006/src/service.rs` names its
/// own.
const INPUT_SCHEMAS: &[&Schema] = &[
    crate::ops::types::ASSOCIATE_GLOSSARY_TERMS_INPUT_SCHEMA,
    crate::ops::types::BATCH_CREATE_PARTITION_INPUT_SCHEMA,
    crate::ops::types::BATCH_DELETE_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::BATCH_DELETE_PARTITION_INPUT_SCHEMA,
    crate::ops::types::BATCH_DELETE_TABLE_INPUT_SCHEMA,
    crate::ops::types::BATCH_DELETE_TABLE_VERSION_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_BLUEPRINTS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_CRAWLERS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_CUSTOM_ENTITY_TYPES_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_DATA_QUALITY_RESULT_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_DATA_QUALITY_RESULT_V2_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_DATA_QUALITY_RULESET_EVALUATION_RUN_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_DEV_ENDPOINTS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_ITERABLE_FORMS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_JOBS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_PARTITION_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_TABLE_OPTIMIZER_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_TRIGGERS_INPUT_SCHEMA,
    crate::ops::types::BATCH_GET_WORKFLOWS_INPUT_SCHEMA,
    crate::ops::types::BATCH_PUT_DATA_QUALITY_STATISTIC_ANNOTATION_INPUT_SCHEMA,
    crate::ops::types::BATCH_STOP_JOB_RUN_INPUT_SCHEMA,
    crate::ops::types::BATCH_UPDATE_PARTITION_INPUT_SCHEMA,
    crate::ops::types::CANCEL_DATA_QUALITY_RULE_RECOMMENDATION_RUN_INPUT_SCHEMA,
    crate::ops::types::CANCEL_DATA_QUALITY_RULESET_EVALUATION_RUN_INPUT_SCHEMA,
    crate::ops::types::CANCEL_ML_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::CANCEL_STATEMENT_INPUT_SCHEMA,
    crate::ops::types::CHECK_SCHEMA_VERSION_VALIDITY_INPUT_SCHEMA,
    crate::ops::types::CREATE_BLUEPRINT_INPUT_SCHEMA,
    crate::ops::types::CREATE_CATALOG_INPUT_SCHEMA,
    crate::ops::types::CREATE_CLASSIFIER_INPUT_SCHEMA,
    crate::ops::types::CREATE_COLUMN_STATISTICS_TASK_SETTINGS_INPUT_SCHEMA,
    crate::ops::types::CREATE_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::CREATE_CRAWLER_INPUT_SCHEMA,
    crate::ops::types::CREATE_CUSTOM_ENTITY_TYPE_INPUT_SCHEMA,
    crate::ops::types::CREATE_DATA_QUALITY_RULESET_INPUT_SCHEMA,
    crate::ops::types::CREATE_DATABASE_INPUT_SCHEMA,
    crate::ops::types::CREATE_DEV_ENDPOINT_INPUT_SCHEMA,
    crate::ops::types::CREATE_GLOSSARY_INPUT_SCHEMA,
    crate::ops::types::CREATE_GLOSSARY_TERM_INPUT_SCHEMA,
    crate::ops::types::CREATE_GLUE_IDENTITY_CENTER_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::CREATE_INTEGRATION_INPUT_SCHEMA,
    crate::ops::types::CREATE_INTEGRATION_RESOURCE_PROPERTY_INPUT_SCHEMA,
    crate::ops::types::CREATE_INTEGRATION_TABLE_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::CREATE_JOB_INPUT_SCHEMA,
    crate::ops::types::CREATE_ML_TRANSFORM_INPUT_SCHEMA,
    crate::ops::types::CREATE_PARTITION_INPUT_SCHEMA,
    crate::ops::types::CREATE_PARTITION_INDEX_INPUT_SCHEMA,
    crate::ops::types::CREATE_REGISTRY_INPUT_SCHEMA,
    crate::ops::types::CREATE_SCHEMA_INPUT_SCHEMA,
    crate::ops::types::CREATE_SCRIPT_INPUT_SCHEMA,
    crate::ops::types::CREATE_SECURITY_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::CREATE_SESSION_INPUT_SCHEMA,
    crate::ops::types::CREATE_TABLE_INPUT_SCHEMA,
    crate::ops::types::CREATE_TABLE_OPTIMIZER_INPUT_SCHEMA,
    crate::ops::types::CREATE_TRIGGER_INPUT_SCHEMA,
    crate::ops::types::CREATE_USAGE_PROFILE_INPUT_SCHEMA,
    crate::ops::types::CREATE_USER_DEFINED_FUNCTION_INPUT_SCHEMA,
    crate::ops::types::CREATE_WORKFLOW_INPUT_SCHEMA,
    crate::ops::types::DELETE_ASSET_INPUT_SCHEMA,
    crate::ops::types::DELETE_ASSET_TYPE_INPUT_SCHEMA,
    crate::ops::types::DELETE_ATTACHMENT_INPUT_SCHEMA,
    crate::ops::types::DELETE_BLUEPRINT_INPUT_SCHEMA,
    crate::ops::types::DELETE_CATALOG_INPUT_SCHEMA,
    crate::ops::types::DELETE_CLASSIFIER_INPUT_SCHEMA,
    crate::ops::types::DELETE_COLUMN_STATISTICS_FOR_PARTITION_INPUT_SCHEMA,
    crate::ops::types::DELETE_COLUMN_STATISTICS_FOR_TABLE_INPUT_SCHEMA,
    crate::ops::types::DELETE_COLUMN_STATISTICS_TASK_SETTINGS_INPUT_SCHEMA,
    crate::ops::types::DELETE_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::DELETE_CONNECTION_TYPE_INPUT_SCHEMA,
    crate::ops::types::DELETE_CRAWLER_INPUT_SCHEMA,
    crate::ops::types::DELETE_CUSTOM_ENTITY_TYPE_INPUT_SCHEMA,
    crate::ops::types::DELETE_DATA_QUALITY_RULESET_INPUT_SCHEMA,
    crate::ops::types::DELETE_DATABASE_INPUT_SCHEMA,
    crate::ops::types::DELETE_DEV_ENDPOINT_INPUT_SCHEMA,
    crate::ops::types::DELETE_FORM_TYPE_INPUT_SCHEMA,
    crate::ops::types::DELETE_GLOSSARY_INPUT_SCHEMA,
    crate::ops::types::DELETE_GLOSSARY_TERM_INPUT_SCHEMA,
    crate::ops::types::DELETE_GLUE_IDENTITY_CENTER_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::DELETE_INTEGRATION_INPUT_SCHEMA,
    crate::ops::types::DELETE_INTEGRATION_RESOURCE_PROPERTY_INPUT_SCHEMA,
    crate::ops::types::DELETE_INTEGRATION_TABLE_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::DELETE_JOB_INPUT_SCHEMA,
    crate::ops::types::DELETE_ML_TRANSFORM_INPUT_SCHEMA,
    crate::ops::types::DELETE_PARTITION_INPUT_SCHEMA,
    crate::ops::types::DELETE_PARTITION_INDEX_INPUT_SCHEMA,
    crate::ops::types::DELETE_REGISTRY_INPUT_SCHEMA,
    crate::ops::types::DELETE_RESOURCE_POLICY_INPUT_SCHEMA,
    crate::ops::types::DELETE_SCHEMA_INPUT_SCHEMA,
    crate::ops::types::DELETE_SCHEMA_VERSIONS_INPUT_SCHEMA,
    crate::ops::types::DELETE_SECURITY_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::DELETE_SESSION_INPUT_SCHEMA,
    crate::ops::types::DELETE_TABLE_INPUT_SCHEMA,
    crate::ops::types::DELETE_TABLE_OPTIMIZER_INPUT_SCHEMA,
    crate::ops::types::DELETE_TABLE_VERSION_INPUT_SCHEMA,
    crate::ops::types::DELETE_TRIGGER_INPUT_SCHEMA,
    crate::ops::types::DELETE_USAGE_PROFILE_INPUT_SCHEMA,
    crate::ops::types::DELETE_USER_DEFINED_FUNCTION_INPUT_SCHEMA,
    crate::ops::types::DELETE_WORKFLOW_INPUT_SCHEMA,
    crate::ops::types::DESCRIBE_CONNECTION_TYPE_INPUT_SCHEMA,
    crate::ops::types::DESCRIBE_ENTITY_INPUT_SCHEMA,
    crate::ops::types::DESCRIBE_INBOUND_INTEGRATIONS_INPUT_SCHEMA,
    crate::ops::types::DESCRIBE_INTEGRATIONS_INPUT_SCHEMA,
    crate::ops::types::DISASSOCIATE_GLOSSARY_TERMS_INPUT_SCHEMA,
    crate::ops::types::GET_ASSET_INPUT_SCHEMA,
    crate::ops::types::GET_ASSET_TYPE_INPUT_SCHEMA,
    crate::ops::types::GET_BLUEPRINT_INPUT_SCHEMA,
    crate::ops::types::GET_BLUEPRINT_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_BLUEPRINT_RUNS_INPUT_SCHEMA,
    crate::ops::types::GET_CATALOG_INPUT_SCHEMA,
    crate::ops::types::GET_CATALOG_IMPORT_STATUS_INPUT_SCHEMA,
    crate::ops::types::GET_CATALOGS_INPUT_SCHEMA,
    crate::ops::types::GET_CLASSIFIER_INPUT_SCHEMA,
    crate::ops::types::GET_CLASSIFIERS_INPUT_SCHEMA,
    crate::ops::types::GET_COLUMN_STATISTICS_FOR_PARTITION_INPUT_SCHEMA,
    crate::ops::types::GET_COLUMN_STATISTICS_FOR_TABLE_INPUT_SCHEMA,
    crate::ops::types::GET_COLUMN_STATISTICS_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_COLUMN_STATISTICS_TASK_RUNS_INPUT_SCHEMA,
    crate::ops::types::GET_COLUMN_STATISTICS_TASK_SETTINGS_INPUT_SCHEMA,
    crate::ops::types::GET_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::GET_CONNECTIONS_INPUT_SCHEMA,
    crate::ops::types::GET_CRAWLER_INPUT_SCHEMA,
    crate::ops::types::GET_CRAWLER_METRICS_INPUT_SCHEMA,
    crate::ops::types::GET_CRAWLERS_INPUT_SCHEMA,
    crate::ops::types::GET_CUSTOM_ENTITY_TYPE_INPUT_SCHEMA,
    crate::ops::types::GET_DASHBOARD_URL_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_CATALOG_ENCRYPTION_SETTINGS_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_CATALOG_EXPORT_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_MODEL_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_MODEL_RESULT_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_RESULT_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_RESULT_V2_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_RULE_RECOMMENDATION_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_RULESET_INPUT_SCHEMA,
    crate::ops::types::GET_DATA_QUALITY_RULESET_EVALUATION_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_DATABASE_INPUT_SCHEMA,
    crate::ops::types::GET_DATABASES_INPUT_SCHEMA,
    crate::ops::types::GET_DATAFLOW_GRAPH_INPUT_SCHEMA,
    crate::ops::types::GET_DEV_ENDPOINT_INPUT_SCHEMA,
    crate::ops::types::GET_DEV_ENDPOINTS_INPUT_SCHEMA,
    crate::ops::types::GET_ENTITY_RECORDS_INPUT_SCHEMA,
    crate::ops::types::GET_FORM_TYPE_INPUT_SCHEMA,
    crate::ops::types::GET_GLOSSARY_INPUT_SCHEMA,
    crate::ops::types::GET_GLOSSARY_TERM_INPUT_SCHEMA,
    crate::ops::types::GET_GLUE_IDENTITY_CENTER_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::GET_INTEGRATION_RESOURCE_PROPERTY_INPUT_SCHEMA,
    crate::ops::types::GET_INTEGRATION_TABLE_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::GET_JOB_INPUT_SCHEMA,
    crate::ops::types::GET_JOB_BOOKMARK_INPUT_SCHEMA,
    crate::ops::types::GET_JOB_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_JOB_RUNS_INPUT_SCHEMA,
    crate::ops::types::GET_JOBS_INPUT_SCHEMA,
    crate::ops::types::GET_MAPPING_INPUT_SCHEMA,
    crate::ops::types::GET_MATERIALIZED_VIEW_REFRESH_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_ML_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_ML_TASK_RUNS_INPUT_SCHEMA,
    crate::ops::types::GET_ML_TRANSFORM_INPUT_SCHEMA,
    crate::ops::types::GET_ML_TRANSFORMS_INPUT_SCHEMA,
    crate::ops::types::GET_PARTITION_INPUT_SCHEMA,
    crate::ops::types::GET_PARTITION_INDEXES_INPUT_SCHEMA,
    crate::ops::types::GET_PARTITIONS_INPUT_SCHEMA,
    crate::ops::types::GET_PLAN_INPUT_SCHEMA,
    crate::ops::types::GET_REGISTRY_INPUT_SCHEMA,
    crate::ops::types::GET_RESOURCE_POLICIES_INPUT_SCHEMA,
    crate::ops::types::GET_RESOURCE_POLICY_INPUT_SCHEMA,
    crate::ops::types::GET_SCHEMA_INPUT_SCHEMA,
    crate::ops::types::GET_SCHEMA_BY_DEFINITION_INPUT_SCHEMA,
    crate::ops::types::GET_SCHEMA_VERSION_INPUT_SCHEMA,
    crate::ops::types::GET_SCHEMA_VERSIONS_DIFF_INPUT_SCHEMA,
    crate::ops::types::GET_SECURITY_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::GET_SECURITY_CONFIGURATIONS_INPUT_SCHEMA,
    crate::ops::types::GET_SESSION_INPUT_SCHEMA,
    crate::ops::types::GET_SESSION_ENDPOINT_INPUT_SCHEMA,
    crate::ops::types::GET_STATEMENT_INPUT_SCHEMA,
    crate::ops::types::GET_SYSTEM_LOGS_FOR_JOB_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_SYSTEM_LOGS_FOR_SESSION_INPUT_SCHEMA,
    crate::ops::types::GET_TABLE_INPUT_SCHEMA,
    crate::ops::types::GET_TABLE_OPTIMIZER_INPUT_SCHEMA,
    crate::ops::types::GET_TABLE_VERSION_INPUT_SCHEMA,
    crate::ops::types::GET_TABLE_VERSIONS_INPUT_SCHEMA,
    crate::ops::types::GET_TABLES_INPUT_SCHEMA,
    crate::ops::types::GET_TAGS_INPUT_SCHEMA,
    crate::ops::types::GET_TRIGGER_INPUT_SCHEMA,
    crate::ops::types::GET_TRIGGERS_INPUT_SCHEMA,
    crate::ops::types::GET_UNFILTERED_PARTITION_METADATA_INPUT_SCHEMA,
    crate::ops::types::GET_UNFILTERED_PARTITIONS_METADATA_INPUT_SCHEMA,
    crate::ops::types::GET_UNFILTERED_TABLE_METADATA_INPUT_SCHEMA,
    crate::ops::types::GET_USAGE_PROFILE_INPUT_SCHEMA,
    crate::ops::types::GET_USER_DEFINED_FUNCTION_INPUT_SCHEMA,
    crate::ops::types::GET_USER_DEFINED_FUNCTIONS_INPUT_SCHEMA,
    crate::ops::types::GET_WORKFLOW_INPUT_SCHEMA,
    crate::ops::types::GET_WORKFLOW_RUN_INPUT_SCHEMA,
    crate::ops::types::GET_WORKFLOW_RUN_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::GET_WORKFLOW_RUNS_INPUT_SCHEMA,
    crate::ops::types::IMPORT_CATALOG_TO_GLUE_INPUT_SCHEMA,
    crate::ops::types::LIST_ASSET_TYPES_INPUT_SCHEMA,
    crate::ops::types::LIST_BLUEPRINTS_INPUT_SCHEMA,
    crate::ops::types::LIST_COLUMN_STATISTICS_TASK_RUNS_INPUT_SCHEMA,
    crate::ops::types::LIST_CONNECTION_TYPES_INPUT_SCHEMA,
    crate::ops::types::LIST_CRAWLERS_INPUT_SCHEMA,
    crate::ops::types::LIST_CRAWLS_INPUT_SCHEMA,
    crate::ops::types::LIST_CUSTOM_ENTITY_TYPES_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_RESULTS_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_RESULTS_V2_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_RULE_RECOMMENDATION_RUNS_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_RULESET_EVALUATION_RUNS_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_RULESETS_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_STATISTIC_ANNOTATIONS_INPUT_SCHEMA,
    crate::ops::types::LIST_DATA_QUALITY_STATISTICS_INPUT_SCHEMA,
    crate::ops::types::LIST_DEV_ENDPOINTS_INPUT_SCHEMA,
    crate::ops::types::LIST_ENTITIES_INPUT_SCHEMA,
    crate::ops::types::LIST_FORM_TYPES_INPUT_SCHEMA,
    crate::ops::types::LIST_GLOSSARIES_INPUT_SCHEMA,
    crate::ops::types::LIST_GLOSSARY_TERMS_INPUT_SCHEMA,
    crate::ops::types::LIST_INTEGRATION_RESOURCE_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::LIST_INTEGRATION_TABLE_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::LIST_ITERABLE_FORMS_INPUT_SCHEMA,
    crate::ops::types::LIST_JOBS_INPUT_SCHEMA,
    crate::ops::types::LIST_MATERIALIZED_VIEW_REFRESH_TASK_RUNS_INPUT_SCHEMA,
    crate::ops::types::LIST_ML_TRANSFORMS_INPUT_SCHEMA,
    crate::ops::types::LIST_REGISTRIES_INPUT_SCHEMA,
    crate::ops::types::LIST_SCHEMA_VERSIONS_INPUT_SCHEMA,
    crate::ops::types::LIST_SCHEMAS_INPUT_SCHEMA,
    crate::ops::types::LIST_SESSIONS_INPUT_SCHEMA,
    crate::ops::types::LIST_STATEMENTS_INPUT_SCHEMA,
    crate::ops::types::LIST_TABLE_OPTIMIZER_RUNS_INPUT_SCHEMA,
    crate::ops::types::LIST_TRIGGERS_INPUT_SCHEMA,
    crate::ops::types::LIST_USAGE_PROFILES_INPUT_SCHEMA,
    crate::ops::types::LIST_WORKFLOWS_INPUT_SCHEMA,
    crate::ops::types::MODIFY_INTEGRATION_INPUT_SCHEMA,
    crate::ops::types::PUBLISH_DATA_QUALITY_RESULT_V2_INPUT_SCHEMA,
    crate::ops::types::PUT_ASSET_INPUT_SCHEMA,
    crate::ops::types::PUT_ASSET_TYPE_INPUT_SCHEMA,
    crate::ops::types::PUT_ATTACHMENT_INPUT_SCHEMA,
    crate::ops::types::PUT_DATA_CATALOG_ENCRYPTION_SETTINGS_INPUT_SCHEMA,
    crate::ops::types::PUT_DATA_CATALOG_EXPORT_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::PUT_DATA_QUALITY_PROFILE_ANNOTATION_INPUT_SCHEMA,
    crate::ops::types::PUT_FORM_TYPE_INPUT_SCHEMA,
    crate::ops::types::PUT_RESOURCE_POLICY_INPUT_SCHEMA,
    crate::ops::types::PUT_SCHEMA_VERSION_METADATA_INPUT_SCHEMA,
    crate::ops::types::PUT_WORKFLOW_RUN_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::QUERY_SCHEMA_VERSION_METADATA_INPUT_SCHEMA,
    crate::ops::types::REGISTER_CONNECTION_TYPE_INPUT_SCHEMA,
    crate::ops::types::REGISTER_SCHEMA_VERSION_INPUT_SCHEMA,
    crate::ops::types::REMOVE_SCHEMA_VERSION_METADATA_INPUT_SCHEMA,
    crate::ops::types::RESET_JOB_BOOKMARK_INPUT_SCHEMA,
    crate::ops::types::RESUME_WORKFLOW_RUN_INPUT_SCHEMA,
    crate::ops::types::RUN_STATEMENT_INPUT_SCHEMA,
    crate::ops::types::SEARCH_ASSETS_INPUT_SCHEMA,
    crate::ops::types::SEARCH_TABLES_INPUT_SCHEMA,
    crate::ops::types::START_BLUEPRINT_RUN_INPUT_SCHEMA,
    crate::ops::types::START_COLUMN_STATISTICS_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::START_COLUMN_STATISTICS_TASK_RUN_SCHEDULE_INPUT_SCHEMA,
    crate::ops::types::START_CRAWLER_INPUT_SCHEMA,
    crate::ops::types::START_CRAWLER_SCHEDULE_INPUT_SCHEMA,
    crate::ops::types::START_DATA_QUALITY_RULE_RECOMMENDATION_RUN_INPUT_SCHEMA,
    crate::ops::types::START_DATA_QUALITY_RULESET_EVALUATION_RUN_INPUT_SCHEMA,
    crate::ops::types::START_EXPORT_LABELS_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::START_IMPORT_LABELS_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::START_JOB_RUN_INPUT_SCHEMA,
    crate::ops::types::START_MATERIALIZED_VIEW_REFRESH_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::START_ML_EVALUATION_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::START_ML_LABELING_SET_GENERATION_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::START_TRIGGER_INPUT_SCHEMA,
    crate::ops::types::START_WORKFLOW_RUN_INPUT_SCHEMA,
    crate::ops::types::STOP_COLUMN_STATISTICS_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::STOP_COLUMN_STATISTICS_TASK_RUN_SCHEDULE_INPUT_SCHEMA,
    crate::ops::types::STOP_CRAWLER_INPUT_SCHEMA,
    crate::ops::types::STOP_CRAWLER_SCHEDULE_INPUT_SCHEMA,
    crate::ops::types::STOP_MATERIALIZED_VIEW_REFRESH_TASK_RUN_INPUT_SCHEMA,
    crate::ops::types::STOP_SESSION_INPUT_SCHEMA,
    crate::ops::types::STOP_TRIGGER_INPUT_SCHEMA,
    crate::ops::types::STOP_WORKFLOW_RUN_INPUT_SCHEMA,
    crate::ops::types::TAG_RESOURCE_INPUT_SCHEMA,
    crate::ops::types::TEST_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::UNTAG_RESOURCE_INPUT_SCHEMA,
    crate::ops::types::UPDATE_ASSET_INPUT_SCHEMA,
    crate::ops::types::UPDATE_BLUEPRINT_INPUT_SCHEMA,
    crate::ops::types::UPDATE_CATALOG_INPUT_SCHEMA,
    crate::ops::types::UPDATE_CLASSIFIER_INPUT_SCHEMA,
    crate::ops::types::UPDATE_COLUMN_STATISTICS_FOR_PARTITION_INPUT_SCHEMA,
    crate::ops::types::UPDATE_COLUMN_STATISTICS_FOR_TABLE_INPUT_SCHEMA,
    crate::ops::types::UPDATE_COLUMN_STATISTICS_TASK_SETTINGS_INPUT_SCHEMA,
    crate::ops::types::UPDATE_CONNECTION_INPUT_SCHEMA,
    crate::ops::types::UPDATE_CRAWLER_INPUT_SCHEMA,
    crate::ops::types::UPDATE_CRAWLER_SCHEDULE_INPUT_SCHEMA,
    crate::ops::types::UPDATE_DATA_QUALITY_RULESET_INPUT_SCHEMA,
    crate::ops::types::UPDATE_DATABASE_INPUT_SCHEMA,
    crate::ops::types::UPDATE_DEV_ENDPOINT_INPUT_SCHEMA,
    crate::ops::types::UPDATE_GLOSSARY_INPUT_SCHEMA,
    crate::ops::types::UPDATE_GLOSSARY_TERM_INPUT_SCHEMA,
    crate::ops::types::UPDATE_GLUE_IDENTITY_CENTER_CONFIGURATION_INPUT_SCHEMA,
    crate::ops::types::UPDATE_INTEGRATION_RESOURCE_PROPERTY_INPUT_SCHEMA,
    crate::ops::types::UPDATE_INTEGRATION_TABLE_PROPERTIES_INPUT_SCHEMA,
    crate::ops::types::UPDATE_JOB_INPUT_SCHEMA,
    crate::ops::types::UPDATE_JOB_FROM_SOURCE_CONTROL_INPUT_SCHEMA,
    crate::ops::types::UPDATE_ML_TRANSFORM_INPUT_SCHEMA,
    crate::ops::types::UPDATE_PARTITION_INPUT_SCHEMA,
    crate::ops::types::UPDATE_REGISTRY_INPUT_SCHEMA,
    crate::ops::types::UPDATE_SCHEMA_INPUT_SCHEMA,
    crate::ops::types::UPDATE_SOURCE_CONTROL_FROM_JOB_INPUT_SCHEMA,
    crate::ops::types::UPDATE_TABLE_INPUT_SCHEMA,
    crate::ops::types::UPDATE_TABLE_OPTIMIZER_INPUT_SCHEMA,
    crate::ops::types::UPDATE_TRIGGER_INPUT_SCHEMA,
    crate::ops::types::UPDATE_USAGE_PROFILE_INPUT_SCHEMA,
    crate::ops::types::UPDATE_USER_DEFINED_FUNCTION_INPUT_SCHEMA,
    crate::ops::types::UPDATE_WORKFLOW_INPUT_SCHEMA,
];

/// The `Message` member of the serialization refusal's shape.
///
/// Spelled `Message`: this service speaks `awsJson1_1`, whose codec takes the wire key from
/// the member NAME, and the protocol-level serialization refusal carries its sentence under
/// that capitalized key — like this service's own modelled errors and unlike the framework's
/// refusal body, whose member is the lowercase `message`. A client reading a serialization
/// refusal finds the sentence under no other name.
static SERIALIZATION_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#SerializationException$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    "Message",
    0,
)
.with_xml_name("Message")
.with_json_name("Message");

/// Schema for the serialization refusal's shape.
///
/// In the `smithy.framework` namespace for the same reason as the scaffold's synthetic shapes
/// ([`crate::scaffolded`]): this shape is in no service's model, so it must not collide with a
/// modelled one. The wire `__type` is the code handed to the protocol's own error serializer,
/// not this id.
static SERIALIZATION_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#SerializationException",
        "smithy.framework",
        "SerializationException",
    ),
    ShapeType::Structure,
    &[&SERIALIZATION_MESSAGE],
);

/// The body of a serialization refusal: the one sentence it states.
#[derive(Debug, Clone)]
struct Unreadable {
    /// The sentence — `service_common::json_wire_values::unconverted_message`.
    message: String,
}

impl SerializableStruct for Unreadable {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&SERIALIZATION_MESSAGE, self.message.as_str())?;
        Ok(())
    }
}

/// The operation this service serves for a request carrying a value no modelled member of the
/// operation's input could be read from.
///
/// An [`ErasedOperation`] rather than a response built in `Service::override_operation`,
/// because that seam hands the router a BORROWED operation. It holds no state: the refusal is
/// re-derived from the same request the router passes back into [`ErasedOperation::run`], so
/// one value serves every such request.
pub(super) struct UnreadableValues;

/// The one value of [`UnreadableValues`] this service hands back.
pub(super) static UNREADABLE_VALUES: UnreadableValues = UnreadableValues;

impl ErasedOperation for UnreadableValues {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        let started = ::std::time::Instant::now();
        let definition = svc.definition();
        // The same decision `Service::override_operation` made, re-derived from the same
        // request: this operation is reached only when it answered `Some`, so the refusal is
        // found again. Were it somehow not, the request is served exactly as the generated
        // routing table would serve it rather than refused in words nothing decided.
        let Some(message) = sentence(definition, req) else {
            return match definition
                .protocol()
                .routing_key(req)
                .ok()
                .and_then(|key| definition.routing_table().get(key))
            {
                Some(op) => op.run(svc, req, ctx),
                None => Box::pin(std::future::ready(definition.routing_error_response(req))),
            };
        };
        let refused = definition
            .protocol()
            .serialize_error(
                &Unreadable {
                    message: message.clone(),
                },
                &SERIALIZATION_SCHEMA,
                SERIALIZATION_EXCEPTION,
                SERIALIZATION_EXCEPTION_STATUS,
            )
            // The protocol writes one string member into its own envelope, so this cannot
            // fail; a codec that somehow could not is answered in the envelope the framework
            // renders a malformed request in, at the same status.
            .unwrap_or_else(|_| {
                definition.framework_error_response(
                    ::aws_smithy_schema_server::ProtocolError::Malformed(message),
                    req,
                )
            });
        // The row the framework sequence would have recorded for this request, so a refused
        // one is not a hole in the telemetry series — through the one writer of such a row in
        // this service (`crate::service::record_refusal`, which its pre-handler wrapper's own
        // refusals go through too).
        let operation = definition.protocol().routing_key(req).ok();
        if let Some(operation) = operation {
            super::record_refusal(
                ctx,
                operation,
                started.elapsed(),
                SERIALIZATION_EXCEPTION_STATUS,
                SERIALIZATION_EXCEPTION,
            );
        }
        // Reported exactly as every other response of this service is: the request it answered
        // named on it (`service_common::query_error_envelope::with_request_id_header`, the one
        // writer of that report) and, for the operations a different front end serves, that
        // front end's own media type — a refusal of the request's VALUES is still a response of
        // the surface the request addressed, so it carries that surface's envelope
        // ([`super::with_rest_json_media_type`], which writes the request id the same way).
        let reported = match operation {
            Some(operation) if super::REST_JSON_MEDIA_TYPE_OPERATIONS.contains(&operation) => {
                super::with_rest_json_media_type(refused)
            }
            _ => ::service_common::query_error_envelope::with_request_id_header(refused),
        };
        Box::pin(std::future::ready(reported))
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", so a request this
        // layer refuses is still authenticated exactly as the operation it addressed is: an
        // unreadable value is no reason to answer an unsigned caller.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every operation this service serves is reachable by the name its routing key
    /// carries, because every input shape of this model is named for its operation with one of
    /// [`INPUT_SHAPE_SUFFIXES`].
    ///
    /// Asserted over the whole list rather than a sample, so a schema that could never be
    /// resolved — and whose operation would therefore silently keep the codec's refusal — is a
    /// failure here rather than a gap on the wire.
    #[test]
    fn every_listed_input_is_named_for_its_operation() {
        for input in INPUT_SCHEMAS {
            let shape = input.shape_id().shape_name();
            let operation = INPUT_SHAPE_SUFFIXES
                .iter()
                .find_map(|suffix| shape.strip_suffix(suffix));
            let Some(operation) = operation else {
                panic!(
                    "case {shape:?}: every input shape must be named <Operation> plus one of \
                     {INPUT_SHAPE_SUFFIXES:?}"
                );
            };
            assert_eq!(
                input_schema(operation).map(|resolved| resolved.shape_id().shape_name()),
                Some(shape),
                "case {operation:?}: expected to resolve {shape:?}, got {:?}",
                input_schema(operation).map(|resolved| resolved.shape_id().shape_name())
            );
        }
    }

    /// RULE: the refusal is about the first member of the body the modelled input cannot read,
    /// and names the CLASS of value sent with the modelled type it could not become; a key the
    /// input declares no member for is ignored exactly as the codec ignores it.
    ///
    /// The rows differ along what the rule generalises over: the class of value sent, the
    /// modelled type it was bound to, and whether that pair reads at all.
    #[test]
    fn the_refusal_names_the_class_and_the_modelled_type() {
        let tables = input_schema("GetTables").expect("GetTables is served");
        // (case, the body, the sentence the refusal states)
        let rows: [(&str, &str, Option<&str>); 12] = [
            (
                "a quoted literal bound to an integer member",
                r#"{"DatabaseName":"d","MaxResults":"STRING_VALUE"}"#,
                Some("STRING_VALUE cannot be converted to Integer"),
            ),
            (
                "a true literal bound to an integer member",
                r#"{"DatabaseName":"d","MaxResults":true}"#,
                Some("TRUE_VALUE cannot be converted to Integer"),
            ),
            (
                "a false literal bound to an integer member",
                r#"{"DatabaseName":"d","MaxResults":false}"#,
                Some("FALSE_VALUE cannot be converted to Integer"),
            ),
            (
                "a fraction bound to an integer member",
                r#"{"DatabaseName":"d","MaxResults":7.5}"#,
                Some("DECIMAL_VALUE cannot be converted to Integer"),
            ),
            (
                "a whole number past the member's width",
                r#"{"DatabaseName":"d","MaxResults":2147483648}"#,
                Some("NUMBER_VALUE cannot be converted to Integer"),
            ),
            (
                "a number bound to a string member",
                r#"{"DatabaseName":7}"#,
                Some("NUMBER_VALUE cannot be converted to String"),
            ),
            (
                "a quoted literal bound to a boolean member",
                r#"{"DatabaseName":"d","IncludeStatusDetails":"true"}"#,
                Some("STRING_VALUE cannot be converted to Boolean"),
            ),
            (
                "every value reads",
                r#"{"DatabaseName":"d","MaxResults":10,"IncludeStatusDetails":true}"#,
                None,
            ),
            (
                "an explicit null is no value of the wrong class",
                r#"{"DatabaseName":"d","MaxResults":null}"#,
                None,
            ),
            (
                "a key the input declares no member for",
                r#"{"DatabaseName":"d","NotAMember":"STRING_VALUE"}"#,
                None,
            ),
            (
                "two unreadable values: the one the body states first is named",
                r#"{"MaxResults":"STRING_VALUE","DatabaseName":7}"#,
                Some("STRING_VALUE cannot be converted to Integer"),
            ),
            ("a body that is no JSON object names no member", "[]", None),
        ];
        for (case, body, expected) in rows {
            let stated = unreadable_member(tables, body.as_bytes());
            assert_eq!(
                stated.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: a target this protocol reads in more than one JSON form, and one no conversion is
    /// attempted for, is left to the codec; the single-form scalars are named by their own
    /// modelled type.
    #[test]
    fn only_a_single_form_target_is_named() {
        // (the modelled type of the member, the name a refusal states for it)
        let rows: [(ShapeType, Option<&str>); 12] = [
            (ShapeType::Boolean, Some("Boolean")),
            (ShapeType::Byte, Some("Byte")),
            (ShapeType::Short, Some("Short")),
            (ShapeType::Integer, Some("Integer")),
            (ShapeType::Long, Some("Long")),
            (ShapeType::String, Some("String")),
            (ShapeType::Double, None),
            (ShapeType::Float, None),
            (ShapeType::Timestamp, None),
            (ShapeType::Blob, None),
            (ShapeType::Document, None),
            (ShapeType::List, None),
        ];
        for (target, named) in rows {
            assert_eq!(
                named_type(target),
                named,
                "case {target:?}: expected {named:?}, got {:?}",
                named_type(target)
            );
        }
    }
}
