// SPDX-License-Identifier: CC0-1.0

//! The operation a request ADDRESSED when the REST route lookup matched it to no operation
//! at all, only because a `@required` query-bound member of that route is absent.
//!
//! RULE: a request whose method and path are the ones an operation of this service binds IS
//! that operation's request. A member that operation requires in the query string and the
//! request does not carry is a statement about the REQUEST — this service's modelled
//! `ValidationException` (HTTP 400) naming the absent member, exactly as a required member
//! absent from a BODY is already answered — and never a report that the operation is not
//! implemented.
//!
//! WHY it has to be answered here: every `@required` `@httpQuery` / `@httpHeader` member is
//! part of the ROUTE the framework matches, so a candidate route whose required binding the
//! request does not carry is not selected at all (`route_lookup`,
//! `vendor/smithy-schema-server/src/rest_xml.rs`, which this model's `restJson1` protocol
//! routes through). That rule exists to tell several operations apart on ONE method + URI
//! template by the shape of the request; it picks BETWEEN operations and cannot un-name the
//! only operation a route has. With no route selected the request falls to the
//! unknown-operation tail, which for a REST envelope is HTTP 501 `NotImplemented` — the
//! claim that nobody has written the operation yet ([`crate::scaffolded`], whose doc says
//! that claim must not answer a bad request), which is also a class an SDK retry policy
//! keeps retrying.
//!
//! Naming the operation from [`smithy_server_runtime::Service::override_operation`] hands
//! such a request to the operation it addressed, so the centralized request walk's own
//! boundary refuses it: the generated input carries the model's `@required` members as
//! non-optional fields, so the codec that cannot build one reports the member it stopped at
//! and the framework words it in this service's envelope (HTTP 400, `ValidationException`,
//! `1 validation error detected: Value null at '<member>' failed to satisfy constraint:
//! Member must not be null`). The handler never runs, so a refused request writes nothing.
//!
//! WHICH answer the request then earns is the model's: this model declares `tagKeys` both
//! `@required` and a list of at least one key, so a removal naming none is outside what it
//! admits and the boundary refuses it. A service whose own recording shows the real endpoint
//! ANSWERING such a request instead — reading the absent repeated parameter as the empty list
//! it travels as, and removing nothing — serves it rather than refusing it, and does so by
//! filling the member before the operation runs (`service-com-amazonaws-greengrassv2-greengrassv2`
//! does exactly that). Nothing recorded for THIS service says it answers, so the modelled
//! refusal stands; this module is where that would be revisited if a recording ever says
//! otherwise.
//!
//! NOTHING about a route is written down here. Which requests an operation addresses is read
//! off that operation's own generated input schema — which carries its `@http` method and
//! URI pattern — by the shared reader every service with such a route uses
//! ([`::service_common::addresses_modeled_http_route`]), and the operation is then taken
//! from the generated routing table under its own `Operation::NAME`, so this layer and that
//! table cannot disagree about which requests the table already resolves.

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema::Schema;
use smithy_server_runtime::{ErasedOperation, ErasedServiceDefinition, Operation};

/// The WIRED operations of this service whose model binds a `@required` member to the query
/// string or to a header, as `(the operation's name, its generated input schema)`.
///
/// Source: the model. `smithy_query(query="shape",shape="UntagResourceInput",
/// model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`
/// reports `tagKeys` as `required` with `httpQuery: "tagKeys"`, and it is the only such
/// member on any operation this crate wires: every other operation of this model requires
/// only members bound to the path or carried in the body, and the route of such a request
/// matches without this layer. The one operation whose model does require headers,
/// `StartMedicalScribeListeningSession`, is routed ahead of the table by its own method and
/// path ([`super::super::service`]), so its route is never withheld either.
///
/// The row holds no method, no URI pattern and no parameter name: those come off the schema
/// and out of the routing table, so an entry cannot fall out of step with the model. An
/// operation the model adds to this class is one row.
const ROUTES_WITH_REQUIRED_QUERY: &[(&str, &Schema)] = &[(
    <crate::ops::untag_resource::UntagResource<()> as Operation>::NAME,
    crate::ops::types::UNTAG_RESOURCE_INPUT_SCHEMA,
)];

/// The operation `req` addressed, when the model's own routing key names none and `req`
/// nevertheless addressed the route of an operation above — or `None` for every other
/// request, which keeps the answer it already had.
///
/// Asked from [`smithy_server_runtime::Service::override_operation`]. The routing key is
/// consulted FIRST and a request it names an operation for is left alone, so nothing that
/// routes today routes differently: this reads only the requests that would otherwise be
/// answered as naming no operation at all.
pub(crate) fn addressed_operation<'a>(
    definition: &'a dyn ErasedServiceDefinition,
    req: &RtRequest,
) -> Option<&'a dyn ErasedOperation> {
    if definition
        .wire_protocol(req)
        .protocol()
        .routing_key(req)
        .is_ok()
    {
        return None;
    }
    let (operation, _) = ROUTES_WITH_REQUIRED_QUERY
        .iter()
        .find(|(_, schema)| ::service_common::addresses_modeled_http_route(req, schema))?;
    definition.routing_table().get(operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::aws_smithy_types::body::SdkBody;

    /// A request to `method` `uri` carrying no body.
    fn request(method: &str, uri: &str) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_method(method).expect("a valid method");
        req.set_uri(uri).expect("a valid uri");
        req
    }

    /// Whether `req` addresses the route of one operation of the table — the question
    /// [`addressed_operation`] asks once the routing key has named nothing.
    fn addresses_a_required_query_route(req: &RtRequest) -> bool {
        ROUTES_WITH_REQUIRED_QUERY
            .iter()
            .any(|(_, schema)| ::service_common::addresses_modeled_http_route(req, schema))
    }

    /// RULE: the route is read off the model, so exactly the method and the path shape the
    /// `@http` trait binds are addressed — one label segment, whatever the query string, and
    /// no other method, depth or path.
    #[test]
    fn only_the_modelled_method_and_path_of_the_route_are_addressed() {
        let arn = "arn%3Aaws%3Ahealth-agent%3Aus-west-2%3A000000000000%3Adomain%2Fdom-aaaaaaaaaaaaaaaa";
        for (case, method, uri, expected) in [
            ("the tagging URI with no query string", "DELETE", format!("/tags/{arn}")),
            ("the tagging URI with an empty query string", "DELETE", format!("/tags/{arn}?")),
            (
                "the tagging URI carrying another parameter",
                "DELETE",
                format!("/tags/{arn}?resourceArn=env"),
            ),
            (
                "a parameter whose name only starts with the bound one",
                "DELETE",
                format!("/tags/{arn}?tagKeysExtra=env"),
            ),
            // The bound parameter supplied: still the route of this operation, which is
            // what makes the routing-key test above the thing that decides.
            ("the bound parameter supplied", "DELETE", format!("/tags/{arn}?tagKeys=env")),
            // An absolute-form target, which is the form an AWS SDK sends.
            (
                "an absolute-form request target",
                "DELETE",
                format!("https://localhost:9297/tags/{arn}"),
            ),
        ]
        .into_iter()
        .map(|(case, method, uri)| (case, method, uri, true))
        .chain(
            [
                ("the collection path with no label", "DELETE", "/tags".to_string()),
                ("a segment past the route's path", "DELETE", format!("/tags/{arn}/extra")),
                ("a path no route of this model binds", "DELETE", "/nope".to_string()),
                ("the reader of the same URI", "GET", format!("/tags/{arn}")),
                ("the writer of the same URI", "POST", format!("/tags/{arn}")),
            ]
            .into_iter()
            .map(|(case, method, uri)| (case, method, uri, false)),
        ) {
            let addressed = addresses_a_required_query_route(&request(method, &uri));
            assert_eq!(
                addressed, expected,
                "case {case:?} ({method} {uri}): expected addressed={expected}, got \
                 {addressed}"
            );
        }
    }
}
