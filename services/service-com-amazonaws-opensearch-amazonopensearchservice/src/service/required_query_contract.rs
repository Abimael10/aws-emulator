// SPDX-License-Identifier: CC0-1.0

//! The request this service answers BEFORE its routing table can name an operation: one
//! whose method and path address an operation of this API but which omits a member that
//! operation requires in the QUERY STRING.
//!
//! RULE: a request whose method and path name an operation this API serves IS that
//! operation's request. A member the operation requires in the query string and the
//! request does not carry is a statement about the REQUEST — refused with the modelled
//! `ValidationException` (HTTP 400) in the sentence this service states an absent required
//! member with, naming the member under the name the request carries it — and never an
//! answer that the operation does not exist.
//!
//! WHY it is answered here: a `@required` `@httpQuery` member is part of the ROUTE the
//! framework matches, so a candidate route whose required query binding is absent from the
//! request is not selected at all (`route_lookup`,
//! `vendor/smithy-schema-server/src/rest_xml.rs`, shared by this model's `restJson1`
//! protocol — that rule is how an S3-class model tells several operations apart on one
//! method and URI template). With no route selected the request reaches the
//! unknown-operation tail, which answers 501 `NotImplemented` for a REST envelope: a
//! server-fault class that tells a client the API does not exist rather than that its own
//! request is incomplete, and one a retry policy keeps retrying. That tail is the
//! framework's, so the only seam this service has for such a request is
//! [`smithy_server_runtime::Service::override_routing_error`], which is where this module
//! is asked.
//!
//! It is also what a read of ONE resource whose required `@httpLabel` rendered EMPTY
//! becomes: an empty label leaves the request target at the collection path the label
//! follows, which is the path of the collection route, and that route is the one requiring
//! the member that names the set to report. Such a read therefore earns the collection
//! route's own refusal rather than the answer a request naming no operation gets.
//!
//! NOTHING about a route is written down here. Which requests an operation addresses is
//! read off its generated input schema by the shared reader every service with such a
//! route uses ([`::service_common::addresses_modeled_http_route`]), so this layer and the
//! framework's routing table cannot disagree about which requests the table already
//! resolves; WHICH member the request left out is named by the codec dispatch would have
//! read the request with — the generated input carries the model's `@required` members as
//! non-optional fields, so the deserializer that cannot build it names the absent one; and
//! the NAME the refusal states is that member's own `@httpQuery` name, read off the same
//! schema. The table below holds no URI, no method and no member name.
//!
//! Disambiguation is not at stake: each operation below binds a method and path of its
//! own, so a request matching one of them names that operation whatever its query string
//! carries.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::SerdeError;
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::path;
use aws_smithy_schema_server::{ProtocolError, Violation, ViolationKind};
use smithy_server_runtime::{
    DeserializableStruct, ErasedServiceDefinition, Refusal, ServerHttpProtocol,
};

use crate::ops::types::{
    GetDomainMaintenanceStatusInput, ListMigrationsInput, ListTagsInput,
    GET_DOMAIN_MAINTENANCE_STATUS_INPUT_SCHEMA, LIST_MIGRATIONS_INPUT_SCHEMA,
    LIST_TAGS_INPUT_SCHEMA,
};

/// Reads the request one candidate operation was sent to and names the member the model
/// requires and the request left out, or `None` when it left out none.
///
/// One reader per candidate rather than one generic function, because the `@required`
/// members live on the generated input TYPE — a required member is a non-optional field of
/// it — and that type is named at the call site.
type MissingMemberReader = fn(&dyn ServerHttpProtocol, &RtRequest) -> Option<String>;

/// The operations of this service whose model binds a `@required` member to the QUERY
/// STRING, as `(the operation's generated input schema, its reader)`.
///
/// Source: the model itself. `smithy_query(query="operation",operation="<Op>",
/// model_path="services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json")`
/// reports a `required` `httpQuery` member for exactly these three — `ListMigrations`
/// (`applicationId`), `GetDomainMaintenanceStatus` (`maintenanceId`) and `ListTags`
/// (`arn`); every other operation of this model requires only members bound to the path or
/// carried in the body, and the route of such a request matches without this layer. The
/// ROW is the schema, so the method, the URI pattern and the member names stay the
/// model's: an entry cannot fall out of step with it.
const ROUTES_WITH_REQUIRED_QUERY: &[(&Schema, MissingMemberReader)] = &[
    (LIST_MIGRATIONS_INPUT_SCHEMA, list_migrations_missing_member),
    (
        GET_DOMAIN_MAINTENANCE_STATUS_INPUT_SCHEMA,
        get_domain_maintenance_status_missing_member,
    ),
    (LIST_TAGS_INPUT_SCHEMA, list_tags_missing_member),
];

/// See [`ROUTES_WITH_REQUIRED_QUERY`].
fn list_migrations_missing_member(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    missing_member::<ListMigrationsInput>(protocol, LIST_MIGRATIONS_INPUT_SCHEMA, req)
}

/// See [`ROUTES_WITH_REQUIRED_QUERY`].
fn get_domain_maintenance_status_missing_member(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    missing_member::<GetDomainMaintenanceStatusInput>(
        protocol,
        GET_DOMAIN_MAINTENANCE_STATUS_INPUT_SCHEMA,
        req,
    )
}

/// See [`ROUTES_WITH_REQUIRED_QUERY`].
fn list_tags_missing_member(protocol: &dyn ServerHttpProtocol, req: &RtRequest) -> Option<String> {
    missing_member::<ListTagsInput>(protocol, LIST_TAGS_INPUT_SCHEMA, req)
}

/// The member `T` requires and this request left out, read by the codec the dispatch
/// sequence would have read the request with.
///
/// The generated input carries the model's `@required` members as non-optional fields, so a
/// request that omits one cannot be built at all and the deserializer reports it as
/// `SerdeError::MissingMember`, naming the member it stopped at — under the model's own
/// spelling of it or under the lowerCamel path a violation of it is reported at, which
/// [`carried_as`] accepts either of. Any other read failure is not this module's question —
/// a value that broke a constraint is the boundary's report, not an absent member — and
/// leaves the request to the answer it had.
///
/// Each route above requires exactly one member in the query string, so the member the read
/// stopped at is the whole of what the request left out.
fn missing_member<T: DeserializableStruct>(
    protocol: &dyn ServerHttpProtocol,
    schema: &'static Schema,
    req: &RtRequest,
) -> Option<String> {
    let mut deserializer = protocol.deserialize_input(req, schema).ok()?;
    match T::deserialize(&mut *deserializer) {
        Err(SerdeError::MissingMember { member_name }) => Some(member_name),
        Ok(_) | Err(_) => None,
    }
}

/// The name the REQUEST carries `member_name` under: that member's own `@httpQuery` name,
/// read off `schema`.
///
/// The refusal names the member a client can act on — the parameter it did not send — and
/// for a member bound to the query string that is the query name the model declares for it,
/// which is the only spelling of it the client ever sees. A member the model binds anywhere
/// else keeps the name it is reported under, which is what the validating walk states every
/// other member under.
///
/// `member_name` is matched against the model name AND against the path spelling a
/// violation of that member carries, because the name a reporting layer hands over is
/// either — the same comparison `::service_common::validation_message` makes when it reads
/// a member off a schema by the path a violation names it at.
fn carried_as(schema: &Schema, member_name: &str) -> String {
    schema
        .members()
        .iter()
        .find(|member| {
            member.member_name().is_some_and(|declared| {
                declared == member_name || path::member(declared) == member_name
            })
        })
        .and_then(|member| member.http_query())
        .map(|query| query.value().to_owned())
        .unwrap_or_else(|| member_name.to_owned())
}

/// The candidate operation this request addresses, as `(its input schema, its reader)`.
fn addressed(req: &RtRequest) -> Option<(&'static Schema, MissingMemberReader)> {
    ROUTES_WITH_REQUIRED_QUERY
        .iter()
        .find(|(schema, _)| ::service_common::addresses_modeled_http_route(req, schema))
        .map(|(schema, reader)| (*schema, *reader))
}

/// The refusal a request that omits a required query member earns, worded as every other
/// refusal of this service's boundary is, or `None` when the request omits none — which
/// leaves it to the answer a request naming no operation earns.
///
/// The violation is the same one the validating walk would have reported had the route been
/// selected (`MissingRequiredMember` at that member's path), and it is worded through the
/// one place this service words a boundary refusal
/// ([`crate::service::validation::aws_validation_refusal`], the seam
/// [`smithy_server_runtime::Service::override_refusal`] delegates to), so a member refused
/// here and a member the walk refuses are indistinguishable on the wire: HTTP 400, the
/// `ValidationException` code, and AWS's `N validation error(s) detected: Value null at
/// '<member>' failed to satisfy constraint: Member must not be null` sentence. The status
/// and the envelope stay with the framework's refusal channel, which owns them.
pub(crate) fn refusal_response(
    definition: &dyn ErasedServiceDefinition,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<RtResponse> {
    let (schema, reader) = addressed(req)?;
    let member = reader(protocol, req)?;
    let violations = vec![Violation {
        path: path::member(&carried_as(schema, &member)),
        kind: ViolationKind::MissingRequiredMember,
        value: None,
        sensitive: false,
    }];
    let chosen = super::validation::aws_validation_refusal(&Refusal::Invalid(violations.clone()));
    Some(definition.refusal_response(ProtocolError::Invalid(violations), chosen, req))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::aws_smithy_types::body::SdkBody;

    /// A request to `method` `uri` with no body.
    fn request(method: &str, uri: &str) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_method(method).expect("the method must be valid");
        req.set_uri(uri).expect("the uri must be valid");
        req
    }

    /// RULE: the refusal names a query-bound member under the name the REQUEST carries it,
    /// whichever spelling the reporting layer handed over — the model's own name for the
    /// member, or the lowerCamel path a violation of it is reported at.
    #[test]
    fn a_query_bound_member_is_named_by_its_query_parameter() {
        for (case, reported) in [
            ("the model's name for the member", "ARN"),
            ("the path a violation of it names it at", "aRN"),
        ] {
            let carried = carried_as(LIST_TAGS_INPUT_SCHEMA, reported);
            assert_eq!(
                carried, "arn",
                "case {case:?}: expected the query parameter the member is carried as, got \
                 {carried:?}"
            );
        }
    }

    /// RULE: only a method and path one of these operations binds is claimed — the
    /// collection path itself, and that path with the empty last segment an unset
    /// `@httpLabel` leaves behind.
    #[test]
    fn only_a_modeled_route_of_a_candidate_operation_is_claimed() {
        for (case, method, uri, expected) in [
            (
                "the migrations collection",
                "GET",
                "/2021-01-01/opensearch/app-migrations",
                true,
            ),
            (
                "the migrations collection with an empty last segment",
                "GET",
                "/2021-01-01/opensearch/app-migrations/",
                true,
            ),
            (
                "the migrations collection with a query string",
                "GET",
                "/2021-01-01/opensearch/app-migrations?maxResults=5",
                true,
            ),
            (
                "the tags of one resource",
                "GET",
                "/2021-01-01/tags",
                true,
            ),
            (
                "one domain's maintenance status",
                "GET",
                "/2021-01-01/opensearch/domain/some-domain/domainMaintenance",
                true,
            ),
            (
                "a migration under the collection",
                "GET",
                "/2021-01-01/opensearch/app-migrations/one-migration",
                false,
            ),
            (
                "the same path under another method",
                "POST",
                "/2021-01-01/opensearch/app-migrations",
                false,
            ),
            (
                "a path no route of this model binds",
                "GET",
                "/2021-01-01/opensearch/app-migrations/one/two",
                false,
            ),
            (
                "another collection of this model",
                "GET",
                "/2021-01-01/opensearch/application",
                false,
            ),
        ] {
            assert_eq!(
                addressed(&request(method, uri)).is_some(),
                expected,
                "case {case:?}: expected {expected} for {method} {uri}"
            );
        }
    }
}
