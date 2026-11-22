// SPDX-License-Identifier: CC0-1.0

//! The members an INTEGRATION response carries on the wire beside the ones its modelled
//! output shape declares.
//!
//! CloudWatch Logs answers the integration create and the two integration reads with more
//! members than their output shapes model: the same integration appears a second time under
//! the shorter member names `name`, `status` and `type`; a read of one integration carries an
//! `integration` record and a listing an `integrations` list of them; and the create carries
//! the `integrationConfig` it accepted. A client built against that wire dereferences those
//! members, so a response that omits them hands it `null` where the service hands it a value
//! — an empty list included.
//!
//! None of them is a second source of truth: each is DERIVED from the modelled members of the
//! same response, which the operation's handler computed from the store. So this layer adds
//! nothing a handler did not already answer, and an integration cannot be reported one way by
//! a documented member and another way by its alias. It is applied to the three operations
//! that carry such members and to no other response, and only to a SUCCESS: a refusal's body
//! words an error and carries no integration at all.
//!
//! It sits in the dispatch every request of this service is served through
//! ([`super::EmulatedService::override_operation`]), rather than inside the three handlers,
//! because what it adds is a property of the WIRE SHAPE of a response and not of any one
//! operation's logic — the handlers answer the modelled output shape, which is the only shape
//! they can name.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_types::body::SdkBody;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The read of ONE integration, whose response carries the `integration` record and the
/// three aliases.
const GET_INTEGRATION: &str = "GetIntegration";

/// The listing, whose response carries the `integrations` list and the three aliases on every
/// summary.
const LIST_INTEGRATIONS: &str = "ListIntegrations";

/// The create, whose response carries the `integrationConfig` it accepted.
const PUT_INTEGRATION: &str = "PutIntegration";

/// The lowest HTTP status that is a failure, and so the lowest one answered by a body that
/// words a refusal rather than an integration.
const FIRST_FAILURE_STATUS: u16 = 400;

/// The modelled member each alias repeats, as `(the member the model declares, the name the
/// wire also carries it under)`.
///
/// Source: `IntegrationSummary` and `GetIntegrationResponse`, whose members are
/// `integrationName`, `integrationType` and `integrationStatus`
/// (`smithy_query(query="shape",shape="IntegrationSummary")`).
const ALIASED_MEMBERS: &[(&str, &str)] = &[
    ("integrationName", "name"),
    ("integrationStatus", "status"),
    ("integrationType", "type"),
];

/// The member an integration's configuration occupies inside an `integration` record, one
/// row per value of the model's `IntegrationType` enum — which declares the single value
/// `OPENSEARCH` (`smithy_query(query="shape",shape="IntegrationType")`) — spelled the way the
/// wire spells that type's configuration member.
const CONFIG_MEMBER_OF_TYPE: &[(&str, &str)] = &[("OPENSEARCH", "openSearchIntegrationConfig")];

/// The member a response carries the accepted configuration under.
const INTEGRATION_CONFIG: &str = "integrationConfig";

/// The member a read of one integration carries the integration record under.
const INTEGRATION: &str = "integration";

/// The member a listing carries the integration records under.
const INTEGRATIONS: &str = "integrations";

/// The member a listing carries the modelled summaries under
/// (`ListIntegrationsResponse$integrationSummaries`).
const INTEGRATION_SUMMARIES: &str = "integrationSummaries";

/// The member an integration's type is modelled under, which decides the configuration
/// member of its `integration` record ([`CONFIG_MEMBER_OF_TYPE`]).
const INTEGRATION_TYPE: &str = "integrationType";

/// The member an integration's name is modelled under.
const INTEGRATION_NAME: &str = "integrationName";

/// The member a read of one integration reports its OpenSearch Service resources under
/// (`GetIntegrationResponse$integrationDetails`), and the union variant an OPENSEARCH
/// integration's resources arrive in.
const INTEGRATION_DETAILS: &str = "integrationDetails";
/// See [`INTEGRATION_DETAILS`].
const OPENSEARCH_INTEGRATION_DETAILS: &str = "openSearchIntegrationDetails";

/// The resource whose own NAME the wire carries beside its ARN
/// (`OpenSearchIntegrationDetails$collection`).
const COLLECTION: &str = "collection";

/// The member the collection's ARN is modelled under (`OpenSearchCollection$collectionArn`).
const COLLECTION_ARN: &str = "collectionArn";

/// The member the wire carries the collection's own name under, which that structure does
/// not model.
const COLLECTION_NAME: &str = "collectionName";

/// `document` with the members `operation`'s wire response also carries, or `None` for an
/// operation that carries none and for a body that is not a JSON object — which leaves every
/// other response of this service byte for byte as its serializer wrote it.
///
/// The operation is matched BEFORE the body is read, so a response of any other operation —
/// a page of log events among them — is never parsed at all.
fn with_wire_members(operation: &str, document: &str) -> Option<String> {
    let add: fn(&mut Map<String, Value>) = match operation {
        GET_INTEGRATION => add_integration_record,
        LIST_INTEGRATIONS => add_integration_records,
        PUT_INTEGRATION => add_accepted_config,
        _ => return None,
    };
    let mut document: Map<String, Value> = ::serde_json::from_str(document).ok()?;
    add(&mut document);
    ::serde_json::to_string(&document).ok()
}

/// Repeat each modelled member of one reported integration under the name the wire also
/// carries it under ([`ALIASED_MEMBERS`]). A member the response does not carry gets no
/// alias: an alias says what its own member says, including that it is absent.
fn alias_members(integration: &mut Map<String, Value>) {
    for (modelled, alias) in ALIASED_MEMBERS {
        if let Some(value) = integration.get(*modelled).cloned() {
            integration.insert((*alias).to_string(), value);
        }
    }
}

/// The `integration` record describing the integration `reported` reports: its name, its
/// type and the configuration member of that type.
///
/// The configuration itself is NOT echoed — no member of the create's `resourceConfig` is
/// read back by an integration read — so the type's configuration member is an empty
/// structure, which is what the wire carries.
fn integration_record(reported: &Map<String, Value>) -> Value {
    let mut record = Map::new();
    let config = reported
        .get(INTEGRATION_TYPE)
        .and_then(Value::as_str)
        .and_then(|integration_type| {
            CONFIG_MEMBER_OF_TYPE
                .iter()
                .find(|(declared, _member)| *declared == integration_type)
        })
        .map_or_else(Map::new, |(_declared, member)| {
            let mut config = Map::new();
            config.insert((*member).to_string(), Value::Object(Map::new()));
            config
        });
    record.insert(INTEGRATION_CONFIG.to_string(), Value::Object(config));
    for member in [INTEGRATION_NAME, INTEGRATION_TYPE] {
        if let Some(value) = reported.get(member).cloned() {
            record.insert(member.to_string(), value);
        }
    }
    Value::Object(record)
}

/// The members a read of ONE integration carries: the `integration` record, the aliases,
/// and the collection's own name.
fn add_integration_record(document: &mut Map<String, Value>) {
    let record = integration_record(document);
    document.insert(INTEGRATION.to_string(), record);
    alias_members(document);
    name_the_collection(document);
}

/// Name the serverless collection a read reports after the resource its own ARN names: the
/// last segment of `collectionArn`, which is the ONE derivation of that name — the ARN the
/// record built is the source of truth, so the name cannot name a second collection. A
/// response carrying no collection, or one with no ARN, gets no name.
fn name_the_collection(document: &mut Map<String, Value>) {
    let Some(collection) = document
        .get_mut(INTEGRATION_DETAILS)
        .and_then(|details| details.get_mut(OPENSEARCH_INTEGRATION_DETAILS))
        .and_then(|details| details.get_mut(COLLECTION))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let named = collection
        .get(COLLECTION_ARN)
        .and_then(Value::as_str)
        .and_then(|arn| arn.rsplit(['/', ':']).next())
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    if let Some(named) = named {
        collection.insert(COLLECTION_NAME.to_string(), Value::String(named));
    }
}

/// The members a LISTING carries: the aliases on every summary, and the `integrations` list
/// holding one record per summary — EMPTY when the account holds none, which is an answer
/// rather than an absence.
fn add_integration_records(document: &mut Map<String, Value>) {
    let mut records: Vec<Value> = Vec::new();
    if let Some(summaries) = document
        .get_mut(INTEGRATION_SUMMARIES)
        .and_then(Value::as_array_mut)
    {
        for summary in summaries.iter_mut() {
            if let Some(summary) = summary.as_object_mut() {
                alias_members(summary);
                records.push(integration_record(summary));
            }
        }
    }
    document.insert(INTEGRATIONS.to_string(), Value::Array(records));
}

/// The member the CREATE carries: the configuration it accepted, as a structure of its own.
/// The create echoes no member of it back, so the structure is empty.
fn add_accepted_config(document: &mut Map<String, Value>) {
    document.insert(INTEGRATION_CONFIG.to_string(), Value::Object(Map::new()));
}

/// The response as it is sent: the one `operation` serialized, plus the members its wire
/// response also carries ([`with_wire_members`]).
///
/// A failure, a body that is not a JSON object and an operation that carries no such member
/// are returned untouched, and the advertised length is corrected for the one that is
/// rewritten.
fn with_wire_members_applied(operation: &str, mut response: RtResponse) -> RtResponse {
    if response.status().as_u16() >= FIRST_FAILURE_STATUS {
        return response;
    }
    let rewritten = {
        // The body of a modelled success is built in memory by its serializer, so the bytes
        // are readable here; a streamed body carries no integration and is left alone.
        let Some(bytes) = response.body().bytes() else {
            return response;
        };
        let Ok(text) = ::std::str::from_utf8(bytes) else {
            return response;
        };
        match with_wire_members(operation, text) {
            Some(rewritten) => rewritten,
            None => return response,
        }
    };
    if response.headers().get("content-length").is_some() {
        let length = rewritten.len().to_string();
        let _ = response.headers_mut().try_insert("content-length", length);
    }
    *response.body_mut() = SdkBody::from(rewritten);
    response
}

/// Serves the dispatch this layer wraps and adds the members the served operation's wire
/// response carries beyond its modelled output shape ([`with_wire_members_applied`]).
///
/// The operation is the one the request itself names, read the way the routing table reads
/// it, so a response is only ever augmented as the operation that produced it.
pub struct WithWireMembers<O> {
    operation: O,
}

impl<O: ErasedOperation> WithWireMembers<O> {
    /// Wrap `operation`, adding the wire members to whatever it answers.
    pub fn new(operation: O) -> Self {
        Self { operation }
    }
}

impl<O: ErasedOperation> ErasedOperation for WithWireMembers<O> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let response = self.operation.run(svc, req, ctx).await;
            match svc.definition().protocol().routing_key(req) {
                Ok(operation) => with_wire_members_applied(operation, response),
                Err(_) => response,
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The wrapped dispatch's own answer: wrapping must not change which schemes sign an
        // operation.
        self.operation.auth_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every alias repeats the modelled member it is the other name of, an
    /// `integration` record names the integration the response reports, and a listing
    /// carries one record per summary — an empty list where it reports none.
    #[test]
    fn each_added_member_repeats_what_the_modelled_members_say() {
        // (case, the operation, the serialized output, the document that must be sent)
        let cases = [
            (
                "a read of one integration",
                GET_INTEGRATION,
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationType": "OPENSEARCH",
                    "integrationStatus": "ACTIVE",
                }),
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationType": "OPENSEARCH",
                    "integrationStatus": "ACTIVE",
                    "integration": {
                        "integrationConfig": {"openSearchIntegrationConfig": {}},
                        "integrationName": "dash",
                        "integrationType": "OPENSEARCH",
                    },
                    "name": "dash",
                    "status": "ACTIVE",
                    "type": "OPENSEARCH",
                }),
            ),
            (
                "a read of an integration still provisioning",
                GET_INTEGRATION,
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationType": "OPENSEARCH",
                    "integrationStatus": "PROVISIONING",
                }),
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationType": "OPENSEARCH",
                    "integrationStatus": "PROVISIONING",
                    "integration": {
                        "integrationConfig": {"openSearchIntegrationConfig": {}},
                        "integrationName": "dash",
                        "integrationType": "OPENSEARCH",
                    },
                    "name": "dash",
                    "status": "PROVISIONING",
                    "type": "OPENSEARCH",
                }),
            ),
            (
                "a read reporting the integration's collection",
                GET_INTEGRATION,
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationType": "OPENSEARCH",
                    "integrationStatus": "ACTIVE",
                    "integrationDetails": {"openSearchIntegrationDetails": {
                        "collection": {
                            "collectionArn":
                                "arn:aws:aoss:eu-west-2:000000000000:collection/dash-collection",
                        },
                    }},
                }),
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationType": "OPENSEARCH",
                    "integrationStatus": "ACTIVE",
                    "integrationDetails": {"openSearchIntegrationDetails": {
                        "collection": {
                            "collectionArn":
                                "arn:aws:aoss:eu-west-2:000000000000:collection/dash-collection",
                            "collectionName": "dash-collection",
                        },
                    }},
                    "integration": {
                        "integrationConfig": {"openSearchIntegrationConfig": {}},
                        "integrationName": "dash",
                        "integrationType": "OPENSEARCH",
                    },
                    "name": "dash",
                    "status": "ACTIVE",
                    "type": "OPENSEARCH",
                }),
            ),
            (
                "a listing that reports none",
                LIST_INTEGRATIONS,
                ::serde_json::json!({"integrationSummaries": []}),
                ::serde_json::json!({"integrationSummaries": [], "integrations": []}),
            ),
            (
                "a listing that reports two",
                LIST_INTEGRATIONS,
                ::serde_json::json!({"integrationSummaries": [
                    {
                        "integrationName": "first",
                        "integrationType": "OPENSEARCH",
                        "integrationStatus": "ACTIVE",
                    },
                    {
                        "integrationName": "second",
                        "integrationType": "OPENSEARCH",
                        "integrationStatus": "PROVISIONING",
                    },
                ]}),
                ::serde_json::json!({
                    "integrationSummaries": [
                        {
                            "integrationName": "first",
                            "integrationType": "OPENSEARCH",
                            "integrationStatus": "ACTIVE",
                            "name": "first",
                            "status": "ACTIVE",
                            "type": "OPENSEARCH",
                        },
                        {
                            "integrationName": "second",
                            "integrationType": "OPENSEARCH",
                            "integrationStatus": "PROVISIONING",
                            "name": "second",
                            "status": "PROVISIONING",
                            "type": "OPENSEARCH",
                        },
                    ],
                    "integrations": [
                        {
                            "integrationConfig": {"openSearchIntegrationConfig": {}},
                            "integrationName": "first",
                            "integrationType": "OPENSEARCH",
                        },
                        {
                            "integrationConfig": {"openSearchIntegrationConfig": {}},
                            "integrationName": "second",
                            "integrationType": "OPENSEARCH",
                        },
                    ],
                }),
            ),
            (
                "a create",
                PUT_INTEGRATION,
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationStatus": "PROVISIONING",
                }),
                ::serde_json::json!({
                    "integrationName": "dash",
                    "integrationStatus": "PROVISIONING",
                    "integrationConfig": {},
                }),
            ),
        ];
        for (case, operation, serialized, expected) in cases {
            let sent = with_wire_members(operation, &serialized.to_string())
                .unwrap_or_else(|| panic!("case {case:?}: {operation} carries wire members"));
            let sent: Value = ::serde_json::from_str(&sent).expect("the sent body is JSON");
            assert_eq!(
                sent, expected,
                "case {case:?}: expected {expected}, got {sent}"
            );
        }
    }

    /// RULE: a response of any other operation is left exactly as its serializer wrote it,
    /// and so is a body that is not a JSON object.
    #[test]
    fn every_other_response_is_left_alone() {
        for (case, operation, document) in [
            ("another read", "GetDelivery", "{\"delivery\":{\"id\":\"abc\"}}"),
            ("the delete of an integration", "DeleteIntegration", "{}"),
            ("a body that is not an object", GET_INTEGRATION, "[]"),
            ("a body that is not JSON at all", LIST_INTEGRATIONS, "not json"),
        ] {
            assert_eq!(
                with_wire_members(operation, document),
                None,
                "case {case:?}: expected no rewrite of {document:?}"
            );
        }
    }
}
