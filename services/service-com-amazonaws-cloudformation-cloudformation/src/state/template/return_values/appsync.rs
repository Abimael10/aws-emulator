// SPDX-License-Identifier: CC0-1.0

//! What `Ref` returns for each `AWS::AppSync::*` type, and what `Fn::GetAtt` reads off one.
//!
//! The per-type half of [`super::return_values`] for the AWS AppSync types, in a module of
//! its own so neither file grows past the length a change to this repository may touch, and
//! reached through the one arm that file carries for them.
//!
//! ## What identifies one of these resources
//!
//! AppSync ASSIGNS every one of these identifiers. `CreateGraphqlApi` takes the API's name
//! and no identifier and answers the `apiId` it minted beside the ARN built on it
//! (`arn:<partition>:appsync:<region>:<account>:apis/<apiId>`), and every later call on the
//! API — `CreateDataSource`, `StartSchemaCreation`, `CreateResolver`, and a client's own
//! `GetGraphqlApi` — is addressed by that id. A data source, a resolver and a function are
//! named UNDER that api id, so each one's ARN carries it too, and an API key's id is the key
//! AppSync generated. None of that is a value a naming rule could compute, so what this
//! module produces is the STAND-IN a declaration is resolved against until the create has
//! answered ([`super::identity_is_service_assigned`] names every one of these types, and
//! [`crate::state::provisioning::appsync_resources`] is what creates them and reports the
//! identifiers and endpoints AppSync answered with).
//!
//! ## The endpoints
//!
//! A GraphQL API's `GraphQLUrl` and `RealtimeUrl` — and the `GraphQLDns`/`RealtimeDns` those
//! URLs are built on — are the addresses the API is SERVED at, which only the owning service
//! can produce (it opens the sockets as the API is created). So each of them stands in as the
//! value [`super::service_assigned_document`] computes for a return value only that service
//! has, and the create re-points it at the address the API really answers on.

use super::*;

/// The `AWS::<cloudFormationName>::<Resource>` types of the AWS AppSync estate a GraphQL API
/// is built out of. The middle segment is that model's own
/// `awsService.cloudFormationName` (`AppSync`).
pub const APPSYNC_GRAPHQL_API: &str = "AWS::AppSync::GraphQLApi";
pub const APPSYNC_GRAPHQL_SCHEMA: &str = "AWS::AppSync::GraphQLSchema";
pub const APPSYNC_DATA_SOURCE: &str = "AWS::AppSync::DataSource";
pub const APPSYNC_RESOLVER: &str = "AWS::AppSync::Resolver";
pub const APPSYNC_FUNCTION_CONFIGURATION: &str = "AWS::AppSync::FunctionConfiguration";
pub const APPSYNC_API_KEY: &str = "AWS::AppSync::ApiKey";

/// The `AWS::AppSync::*` types an EVENT API's estate is declared with — the API itself, whose
/// endpoints carry published events, and the channel namespaces a publisher and a subscriber
/// address on it — and the one that links a SOURCE API to a Merged API.
pub const APPSYNC_API: &str = "AWS::AppSync::Api";
pub const APPSYNC_CHANNEL_NAMESPACE: &str = "AWS::AppSync::ChannelNamespace";
pub const APPSYNC_SOURCE_API_ASSOCIATION: &str = "AWS::AppSync::SourceApiAssociation";

/// The property an association names the Merged API it links a source API INTO by, which is
/// also the `@httpLabel` of every call AppSync publishes on one.
pub const APPSYNC_MERGED_API_IDENTIFIER_PROPERTY: &str = "MergedApiIdentifier";

/// The property an association names the SOURCE API it links by, which travels in the create's
/// own body.
pub const APPSYNC_SOURCE_API_IDENTIFIER_PROPERTY: &str = "SourceApiIdentifier";

/// The ARN namespace (and endpoint prefix) AppSync mints its resources under — that model's
/// `awsService.arnNamespace`.
pub const APPSYNC_ARN_NAMESPACE: &str = "appsync";

/// The property every child of a GraphQL API names its API by, which is also the
/// `@httpLabel` of each of their creates.
pub const APPSYNC_API_ID_PROPERTY: &str = "ApiId";

/// The property a resolver and a function name the type they are attached to by.
pub const APPSYNC_TYPE_NAME_PROPERTY: &str = "TypeName";

/// The property a resolver names the field it resolves by.
pub const APPSYNC_FIELD_NAME_PROPERTY: &str = "FieldName";

/// The salt the stand-in API id is derived under, so an API's id and any other identifier
/// derived from the same name are different digits ([`derived_digits`]).
const APPSYNC_API_ID_SALT: &str = "appsync/api-id";

/// How many characters an AppSync GraphQL API id carries: the ids that service mints are a
/// fixed-length run of lower-case alphanumerics, and a stand-in of the same length is what a
/// declaration reads until the create answers the real one.
const APPSYNC_API_ID_CHARS: usize = 26;

/// Whether `resource_type` is one of the AppSync types this module carries.
pub(super) fn is_appsync_type(resource_type: &str) -> bool {
    matches!(
        resource_type,
        APPSYNC_GRAPHQL_API
            | APPSYNC_GRAPHQL_SCHEMA
            | APPSYNC_DATA_SOURCE
            | APPSYNC_RESOLVER
            | APPSYNC_FUNCTION_CONFIGURATION
            | APPSYNC_API_KEY
            | APPSYNC_API
            | APPSYNC_CHANNEL_NAMESPACE
            | APPSYNC_SOURCE_API_ASSOCIATION
    )
}

/// The physical resource id and the attributes a resource of one of these types reports,
/// as [`super::return_values`]'s own arm answers them.
pub(super) fn appsync_return_values(
    resource_type: &str,
    name: &str,
    properties: &Value,
    scope: &Scope<'_>,
    budget: &mut Budget,
) -> (String, BTreeMap<String, String>) {
    let region = scope.stack.region;
    let mut attributes = BTreeMap::new();
    match resource_type {
        // The API itself: `Ref` returns the API's ARN, and `Fn::GetAtt` reads the id AppSync
        // minted, that same ARN, and the two endpoints the API is served on.
        APPSYNC_GRAPHQL_API => {
            let api_id = stand_in_api_id(name);
            let arn = api_arn(region, &api_id);
            attributes.insert("ApiId".to_string(), api_id);
            attributes.insert("Arn".to_string(), arn.clone());
            for attribute in ["GraphQLDns", "GraphQLUrl", "RealtimeDns", "RealtimeUrl"] {
                attributes.insert(
                    attribute.to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (arn, attributes)
        }
        // The schema of one API. AppSync holds one schema per API and addresses it by that
        // API alone (`StartSchemaCreation`/`GetIntrospectionSchema` take an `apiId` and
        // nothing else), so what identifies the resource is the API it belongs to, and it
        // publishes no attribute of its own.
        APPSYNC_GRAPHQL_SCHEMA => (declared_api_id(properties, scope, budget), attributes),
        // A data source: `Ref` returns its ARN, and `Fn::GetAtt` reads that ARN and the name
        // it was created under.
        APPSYNC_DATA_SOURCE => {
            let arn = child_arn(
                region,
                properties,
                scope,
                budget,
                &format!("datasources/{name}"),
            );
            attributes.insert("DataSourceArn".to_string(), arn.clone());
            attributes.insert("Name".to_string(), name.to_string());
            (arn, attributes)
        }
        // A resolver: `Ref` returns its ARN, and `Fn::GetAtt` reads that ARN beside the type
        // and the field it resolves — which are what the declaration itself states and what
        // AppSync addresses the resolver by.
        APPSYNC_RESOLVER => {
            let type_name = declared_text(properties, scope, budget, APPSYNC_TYPE_NAME_PROPERTY);
            let field_name = declared_text(properties, scope, budget, APPSYNC_FIELD_NAME_PROPERTY);
            let arn = child_arn(
                region,
                properties,
                scope,
                budget,
                &format!("types/{type_name}/resolvers/{field_name}"),
            );
            attributes.insert("FieldName".to_string(), field_name);
            attributes.insert("ResolverArn".to_string(), arn.clone());
            attributes.insert("TypeName".to_string(), type_name);
            (arn, attributes)
        }
        // A function of a pipeline resolver: `Ref` returns its ARN, and `Fn::GetAtt` reads
        // that ARN, the function id AppSync minted, the data source it runs against and the
        // name it was created under.
        APPSYNC_FUNCTION_CONFIGURATION => {
            let function_id = stand_in_api_id(name);
            let arn = child_arn(
                region,
                properties,
                scope,
                budget,
                &format!("functions/{function_id}"),
            );
            attributes.insert(
                "DataSourceName".to_string(),
                declared_text(properties, scope, budget, "DataSourceName"),
            );
            attributes.insert("FunctionArn".to_string(), arn.clone());
            attributes.insert("FunctionId".to_string(), function_id);
            attributes.insert("Name".to_string(), name.to_string());
            (arn, attributes)
        }
        // An API key: the key AppSync generated is both the resource's identifier and the
        // value a caller presents in `x-api-key`, so `ApiKey` and `ApiKeyId` are that one
        // value, and the ARN is built on it under the API that holds it.
        APPSYNC_API_KEY => {
            let key_id = stand_in_api_id(name);
            let arn = child_arn(
                region,
                properties,
                scope,
                budget,
                &format!("apikeys/{key_id}"),
            );
            attributes.insert("ApiKey".to_string(), key_id.clone());
            attributes.insert("ApiKeyId".to_string(), key_id.clone());
            attributes.insert("Arn".to_string(), arn);
            (key_id, attributes)
        }
        // An Event API: `Ref` returns the `Api` ID, and `Fn::GetAtt` reads that id, the ARN
        // AppSync built on it and the two DNS records the API is SERVED at — one per endpoint,
        // which only the owning service can produce (it opens the sockets as the API is made).
        APPSYNC_API => {
            let api_id = stand_in_api_id(name);
            attributes.insert("ApiArn".to_string(), api_arn(region, &api_id));
            attributes.insert("ApiId".to_string(), api_id.clone());
            for attribute in [DNS_HTTP_ATTRIBUTE, DNS_REALTIME_ATTRIBUTE] {
                attributes.insert(
                    attribute.to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (api_id, attributes)
        }
        // A channel namespace of an Event API: `Ref` returns the namespace's ARN, which is
        // the only value that type publishes, built under the API its `ApiId` names and the
        // path `GetChannelNamespace` addresses it at.
        APPSYNC_CHANNEL_NAMESPACE => {
            let arn = child_arn(
                region,
                properties,
                scope,
                budget,
                &format!("channelNamespace/{name}"),
            );
            attributes.insert("ChannelNamespaceArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // The link of a source API into a Merged API: AppSync generates the association's own
        // id, `Ref` returns the ARN built on it under the Merged API that holds it, and
        // `Fn::GetAtt` reads that pair beside the id and the ARN of each side of the link and
        // the state the link is in — which only that service reports.
        APPSYNC_SOURCE_API_ASSOCIATION => {
            let association_id = stand_in_api_id(name);
            let merged_api_id = api_id_in(&declared_text(
                properties,
                scope,
                budget,
                APPSYNC_MERGED_API_IDENTIFIER_PROPERTY,
            ));
            let source_api_id = api_id_in(&declared_text(
                properties,
                scope,
                budget,
                APPSYNC_SOURCE_API_IDENTIFIER_PROPERTY,
            ));
            let arn = ::emulator_server::emulator_arn(
                APPSYNC_ARN_NAMESPACE,
                region,
                &format!("apis/{merged_api_id}/sourceApiAssociations/{association_id}"),
            );
            attributes.insert("AssociationArn".to_string(), arn.clone());
            attributes.insert("AssociationId".to_string(), association_id);
            attributes.insert("MergedApiArn".to_string(), api_arn(region, &merged_api_id));
            attributes.insert("MergedApiId".to_string(), merged_api_id);
            attributes.insert("SourceApiArn".to_string(), api_arn(region, &source_api_id));
            attributes.insert(
                "SourceApiAssociationStatus".to_string(),
                service_assigned_document(name, "SourceApiAssociationStatus"),
            );
            attributes.insert("SourceApiId".to_string(), source_api_id);
            (arn, attributes)
        }
        // No other type reaches here: the one arm that calls this names exactly the nine
        // above ([`is_appsync_type`]), and a type outside them keeps the physical name the
        // naming rules computed, as every type this catalogue does not carry does.
        _ => (name.to_string(), attributes),
    }
}

/// The attribute an Event API publishes its HTTP endpoint's DNS record as, and the one it
/// publishes its real-time endpoint's as — each the `Dns` record of that endpoint, read off
/// the map `Api$dns` carries them in (keyed `HTTP` and `REALTIME`).
pub(crate) const DNS_HTTP_ATTRIBUTE: &str = "Dns.Http";
pub(crate) const DNS_REALTIME_ATTRIBUTE: &str = "Dns.Realtime";

/// The API id an `@httpLabel` of an AppSync call takes, read off either form an identifier is
/// stated in: "In most cases, Merged APIs (especially in your account) only require the API ID
/// value or ARN of the merged API" (`AssociateSourceGraphqlApi$mergedApiIdentifier`), so the
/// id is the last segment of an ARN and the whole value otherwise.
fn api_id_in(identifier: &str) -> String {
    identifier
        .rsplit('/')
        .next()
        .unwrap_or(identifier)
        .to_string()
}

/// The NAMES of the attributes a resource of one of these types publishes — the other half
/// of the catalogue above, read without a resource ([`super::published_attribute_names`]).
pub(super) fn published_attributes(resource_type: &str) -> &'static [&'static str] {
    match resource_type {
        APPSYNC_GRAPHQL_API => &[
            "ApiId",
            "Arn",
            "GraphQLDns",
            "GraphQLUrl",
            "RealtimeDns",
            "RealtimeUrl",
        ],
        APPSYNC_DATA_SOURCE => &["DataSourceArn", "Name"],
        APPSYNC_RESOLVER => &["FieldName", "ResolverArn", "TypeName"],
        APPSYNC_FUNCTION_CONFIGURATION => {
            &["DataSourceName", "FunctionArn", "FunctionId", "Name"]
        }
        APPSYNC_API_KEY => &["ApiKey", "ApiKeyId", "Arn"],
        APPSYNC_API => &["ApiArn", "ApiId", DNS_HTTP_ATTRIBUTE, DNS_REALTIME_ATTRIBUTE],
        APPSYNC_CHANNEL_NAMESPACE => &["ChannelNamespaceArn"],
        APPSYNC_SOURCE_API_ASSOCIATION => &[
            "AssociationArn",
            "AssociationId",
            "MergedApiArn",
            "MergedApiId",
            "SourceApiArn",
            "SourceApiAssociationStatus",
            "SourceApiId",
        ],
        // A schema publishes no attribute of its own.
        _ => &[],
    }
}

/// The ARN of one GraphQL API, which is what that API's `Ref` returns.
fn api_arn(region: &str, api_id: &str) -> String {
    ::emulator_server::emulator_arn(APPSYNC_ARN_NAMESPACE, region, &format!("apis/{api_id}"))
}

/// The ARN of a resource that lives UNDER an API, built on the API the declaration names and
/// the path AppSync's own URI addresses the resource at (`/v1/apis/{apiId}/datasources/{name}`,
/// `/v1/apis/{apiId}/types/{typeName}/resolvers/{fieldName}`).
fn child_arn(
    region: &str,
    properties: &Value,
    scope: &Scope<'_>,
    budget: &mut Budget,
    path: &str,
) -> String {
    let api_id = declared_api_id(properties, scope, budget);
    ::emulator_server::emulator_arn(
        APPSYNC_ARN_NAMESPACE,
        region,
        &format!("apis/{api_id}/{path}"),
    )
}

/// The API id a declaration names, as the text it resolves to.
///
/// A declaration that names the API with an `Fn::GetAtt` on an API of the same template
/// resolves to the stand-in that API carries until its create has answered, and to the id
/// AppSync minted once it has ([`super::resolve_dependent_identities`]'s rounds and the
/// re-pointing the deployment does). A declaration naming nothing resolvable yields the empty
/// text, which no AppSync call resolves — that request is refused by the owning service rather
/// than created under an API nothing named.
fn declared_api_id(properties: &Value, scope: &Scope<'_>, budget: &mut Budget) -> String {
    declared_text(properties, scope, budget, APPSYNC_API_ID_PROPERTY)
}

/// One declared property as the text it resolves to, or the empty text where the declaration
/// states none.
fn declared_text(
    properties: &Value,
    scope: &Scope<'_>,
    budget: &mut Budget,
    property: &str,
) -> String {
    properties
        .get(property)
        .and_then(|value| text(value, scope, 0, budget))
        .unwrap_or_default()
}

/// The stand-in an identifier AppSync MINTS is resolved to before that service has been asked
/// for it: [`APPSYNC_API_ID_CHARS`] characters derived from the resource's own name, which is
/// stable across reads and across a later deployment of the same stack.
fn stand_in_api_id(name: &str) -> String {
    derived_digits(
        APPSYNC_API_ID_SALT,
        name,
        APPSYNC_API_ID_CHARS,
        Alphabet::Lower,
    )
}
