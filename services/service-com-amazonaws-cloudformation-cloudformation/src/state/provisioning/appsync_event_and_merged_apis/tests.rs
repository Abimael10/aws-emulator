// SPDX-License-Identifier: CC0-1.0

//! What the three types' arms carry, address and read, held to the calls AppSync publishes.

use super::*;

/// Every type this module owns resolves to its own kind, and a type it does not own to none —
/// which is what keeps the provisioning algorithm from sending another service's type here.
#[test]
fn each_owned_type_resolves_to_its_kind() {
    let rows: &[(&str, Option<Kind>)] = &[
        (APPSYNC_API, Some(Kind::EventApi)),
        (APPSYNC_CHANNEL_NAMESPACE, Some(Kind::ChannelNamespace)),
        (
            APPSYNC_SOURCE_API_ASSOCIATION,
            Some(Kind::SourceApiAssociation),
        ),
        // The GraphQL API estate is another module's, and so is a custom domain.
        ("AWS::AppSync::GraphQLApi", None),
        ("AWS::AppSync::DomainName", None),
        ("AWS::NotAService::NotAType", None),
    ];
    for (resource_type, expected) in rows {
        let actual = kind_of(resource_type);
        assert_eq!(
            actual, *expected,
            "case {resource_type:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The declared properties reach each create under that call's own member names: the property
/// with its initial letter lower-cased, nested members included — and a property the
/// declaration does not state is left out rather than sent empty.
#[test]
fn the_declared_properties_are_carried_under_the_api_member_names() {
    // (case, the properties carried, the declaration, what the request body must carry).
    let rows: Vec<(&str, &[&str], Value, Value)> = vec![
        (
            "an Event API's whole configuration",
            API_MEMBERS,
            serde_json::json!({
                "Name": "an-event-api",
                "OwnerContact": "platform",
                "EventConfig": {
                    "AuthProviders": [{"AuthType": "API_KEY"}],
                    "ConnectionAuthModes": [{"AuthType": "API_KEY"}],
                    "DefaultPublishAuthModes": [{"AuthType": "API_KEY"}],
                    "DefaultSubscribeAuthModes": [{"AuthType": "API_KEY"}]
                }
            }),
            serde_json::json!({
                "ownerContact": "platform",
                "eventConfig": {
                    "authProviders": [{"authType": "API_KEY"}],
                    "connectionAuthModes": [{"authType": "API_KEY"}],
                    "defaultPublishAuthModes": [{"authType": "API_KEY"}],
                    "defaultSubscribeAuthModes": [{"authType": "API_KEY"}]
                }
            }),
        ),
        (
            "an Event API stating only what its create requires",
            API_MEMBERS,
            serde_json::json!({
                "Name": "an-event-api",
                "EventConfig": {"AuthProviders": [{"AuthType": "API_KEY"}]}
            }),
            serde_json::json!({
                "eventConfig": {"authProviders": [{"authType": "API_KEY"}]}
            }),
        ),
        (
            "a channel namespace's handlers, down to the integration's data source",
            CHANNEL_NAMESPACE_MEMBERS,
            serde_json::json!({
                "ApiId": "anapiid",
                "Name": "default",
                "CodeHandlers": "export function onPublish(ctx) { return ctx.events }",
                "PublishAuthModes": [{"AuthType": "API_KEY"}],
                "HandlerConfigs": {
                    "OnPublish": {
                        "Behavior": "DIRECT",
                        "Integration": {"DataSourceName": "a-data-source"}
                    }
                }
            }),
            serde_json::json!({
                "codeHandlers": "export function onPublish(ctx) { return ctx.events }",
                "publishAuthModes": [{"authType": "API_KEY"}],
                "handlerConfigs": {
                    "onPublish": {
                        "behavior": "DIRECT",
                        "integration": {"dataSourceName": "a-data-source"}
                    }
                }
            }),
        ),
        (
            "an association's description and merge configuration",
            ASSOCIATION_MEMBERS,
            serde_json::json!({
                "MergedApiIdentifier": "amergedapiid",
                "SourceApiIdentifier": "asourceapiid",
                "Description": "the link",
                "SourceApiAssociationConfig": {"MergeType": "AUTO_MERGE"}
            }),
            serde_json::json!({
                "description": "the link",
                "sourceApiAssociationConfig": {"mergeType": "AUTO_MERGE"}
            }),
        ),
    ];
    for (case, carried, declared, expected) in rows {
        let mut request = Map::new();
        carry(&mut request, &declared, carried)
            .unwrap_or_else(|refusal| panic!("case {case:?}: expected a request, got {refusal}"));
        let actual = Value::Object(request);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected}, got {actual}"
        );
    }
}

/// Each URI is the model's own binding, with every identifier written as ONE path segment — so
/// a name carrying a character that is structure in a URI cannot address another operation.
#[test]
fn the_uris_are_the_models_own_bindings() {
    let rows: &[(&str, String, &str)] = &[
        (
            "the Event API collection",
            EVENT_APIS_URI.to_string(),
            "/v2/apis",
        ),
        (
            "one Event API",
            event_api_uri("anapiid"),
            "/v2/apis/anapiid",
        ),
        (
            "the channel namespaces of one Event API",
            channel_namespaces_uri("anapiid"),
            "/v2/apis/anapiid/channelNamespaces",
        ),
        (
            "one channel namespace",
            channel_namespace_uri("anapiid", "default"),
            "/v2/apis/anapiid/channelNamespaces/default",
        ),
        (
            "a namespace name carrying URI structure",
            channel_namespace_uri("anapiid", "a/b"),
            "/v2/apis/anapiid/channelNamespaces/a%2Fb",
        ),
        (
            "the source API associations of one Merged API",
            source_api_associations_uri("amergedapiid"),
            "/v1/mergedApis/amergedapiid/sourceApiAssociations",
        ),
        (
            "a Merged API named by its ARN",
            source_api_associations_uri("arn:aws:appsync:us-east-1:000000000000:apis/amergedapiid"),
            "/v1/mergedApis/arn%3Aaws%3Aappsync%3Aus-east-1%3A000000000000%3Aapis%2Famergedapiid\
             /sourceApiAssociations",
        ),
    ];
    for (case, actual, expected) in rows {
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// An association is addressed by the pair its own ARN carries, and a physical id of any other
/// shape identifies no association at all rather than being pointed at a guessed one.
#[test]
fn an_association_is_addressed_by_the_pair_its_arn_carries() {
    let addressed = association_uri(
        "arn:aws:appsync:us-east-1:000000000000:apis/amergedapiid/sourceApiAssociations/anid",
    )
    .unwrap_or_else(|refusal| panic!("case \"an association ARN\": expected a URI, got {refusal}"));
    let expected = "/v1/mergedApis/amergedapiid/sourceApiAssociations/anid";
    assert_eq!(
        addressed, expected,
        "case \"an association ARN\": expected {expected:?}, got {addressed:?}"
    );
    for (case, identifier) in [
        ("an API's own ARN", "arn:aws:appsync:us-east-1:000000000000:apis/anapiid"),
        ("a bare association id", "anid"),
        (
            "an ARN naming another kind of child",
            "arn:aws:appsync:us-east-1:000000000000:apis/anapiid/datasources/a-source",
        ),
        (
            "an ARN with no association id",
            "arn:aws:appsync:us-east-1:000000000000:apis/anapiid/sourceApiAssociations/",
        ),
        ("the empty id", ""),
    ] {
        let actual = association_uri(identifier);
        assert!(
            actual.is_err(),
            "case {case:?}: expected a refusal, got {actual:?}"
        );
    }
}

/// The attributes an association publishes are the ones AppSync's own record of the link
/// carries, and a member that record does not carry contributes no attribute.
#[test]
fn an_associations_attributes_are_read_off_the_service_record() {
    let association = serde_json::json!({
        "associationArn":
            "arn:aws:appsync:us-east-1:000000000000:apis/amergedapiid/sourceApiAssociations/anid",
        "associationId": "anid",
        "mergedApiArn": "arn:aws:appsync:us-east-1:000000000000:apis/amergedapiid",
        "mergedApiId": "amergedapiid",
        "sourceApiArn": "arn:aws:appsync:us-east-1:000000000000:apis/asourceapiid",
        "sourceApiId": "asourceapiid",
        "sourceApiAssociationStatus": "MERGE_SUCCESS",
        "description": "the link"
    });
    let attributes = association_attributes(&association);
    for (attribute, member) in ASSOCIATION_ATTRIBUTES {
        let expected = association.get(member).and_then(Value::as_str);
        let actual = attributes.get(*attribute).map(String::as_str);
        assert_eq!(
            actual, expected,
            "case {attribute:?}: expected {expected:?}, got {actual:?}"
        );
    }
    // A record that identifies nothing publishes no attribute, rather than empty ones.
    let empty = association_attributes(&serde_json::json!({"description": "the link"}));
    assert!(
        empty.is_empty(),
        "case \"a record carrying no identifier\": expected no attributes, got {empty:?}"
    );
}
