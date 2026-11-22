// SPDX-License-Identifier: CC0-1.0

//! The `AWS::AppSync::*` types a GraphQL API's estate is declared with — the API itself, the
//! schema it serves, the data sources it reads and writes through, the resolvers and pipeline
//! functions that run against them, and the API keys a caller presents — created in AWS
//! AppSync itself.
//!
//! This is the per-type half of provisioning for those six types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives in
//! its own module beside [`super::appconfig_resources`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! A GraphQL API is reached on its own DATA PLANE: a client POSTs a GraphQL document to the
//! API's endpoint and a subscriber opens a WebSocket against its real-time one, and what
//! answers there is the schema, the resolvers and the data sources AppSync holds for that API.
//! AppSync is the service that holds all of it, and it ASSIGNS the API's id — `CreateGraphqlApi`
//! takes a name and no identifier and answers the `apiId` it minted, the ARN built on it and
//! the two endpoint URLs the API is served at. So a stack whose API exists nowhere publishes an
//! endpoint nothing serves and an id no `GetGraphqlApi` resolves, however complete the stack's
//! own records look; every child of the API is addressed by that minted id, so none of them
//! could be created either. All six types are therefore provisioned here rather than left
//! declared-only, and what each create answers is the resource's identity and attributes
//! ([`crate::state::template::identity_is_service_assigned`] names all six).
//!
//! ## The calls
//!
//! Each type is the call AppSync publishes for it, on the URI that operation's own `@http`
//! binding declares (that model speaks `restJson1`, so every call rides [`call_rest_json`]):
//!
//! * `AWS::AppSync::GraphQLApi` is `CreateGraphqlApi` (`POST /v1/apis`), with the
//!   `EnvironmentVariables` a declaration carries applied by
//!   `PutGraphqlApiEnvironmentVariables` — the one call AppSync publishes for them;
//! * `AWS::AppSync::GraphQLSchema` is `StartSchemaCreation`
//!   (`POST /v1/apis/{apiId}/schemacreation`), which is ASYNCHRONOUS: it answers the schema
//!   `PROCESSING`, so this resource's creation is held open until `GetSchemaCreationStatus`
//!   reports it `SUCCESS` ([`readiness`]);
//! * `AWS::AppSync::DataSource` is `CreateDataSource` (`POST /v1/apis/{apiId}/datasources`);
//! * `AWS::AppSync::Resolver` is `CreateResolver`
//!   (`POST /v1/apis/{apiId}/types/{typeName}/resolvers`);
//! * `AWS::AppSync::FunctionConfiguration` is `CreateFunction`
//!   (`POST /v1/apis/{apiId}/functions`);
//! * `AWS::AppSync::ApiKey` is `CreateApiKey` (`POST /v1/apis/{apiId}/apikeys`).
//!
//! A CHANGED declaration rides that type's own update call, bound to the URI that addresses
//! the resource itself — `UpdateGraphqlApi`, `UpdateDataSource`, `UpdateResolver`,
//! `UpdateFunction` and `UpdateApiKey` — and a changed schema rides `StartSchemaCreation`
//! again, which is the call AppSync publishes for installing one ([`update`]).
//!
//! ## What identifies each resource
//!
//! Each is the handle every later call on the resource takes, and the value that type's `Ref`
//! returns: the API's own ARN, a data source's, a resolver's and a function's ARN, the ARN of
//! an API key under the API that holds it — never the key AppSync generated, which is what a
//! caller PRESENTS — and, for a schema, the API's id with the type's own segment appended
//! (`crate::state::template::appsync_schema_identifier`), AppSync holding one schema per API
//! and addressing it by that API alone.
//!
//! ## The properties
//!
//! Every property these types publish is a member of the owning create under the SAME name
//! with its initial letter lower-cased, nested members included (`UserPoolConfig.UserPoolId`
//! against `userPoolConfig.userPoolId`, `HttpConfig.AuthorizationConfig.AwsIamConfig.\
//! SigningRegion` against `httpConfig.authorizationConfig.awsIamConfig.signingRegion`), which
//! is the walk [`lower_camel_members`] is. Four are not carried that way:
//!
//! * `ApiId` ADDRESSES the call rather than travelling in it: it is the `@httpLabel` of every
//!   create but the API's own, as `TypeName` is of `CreateResolver`;
//! * `DynamoDBConfig` is `dynamodbConfig` — the one member AppSync does not spell as the
//!   property with a lower-cased initial;
//! * `Tags` is the `[{Key, Value}]` list a template writes against the `TagMap`
//!   `CreateGraphqlApi` takes, so it is rendered as that map;
//! * a `Definition`, a `Code` and a mapping template are TEXT the API runs, carried verbatim —
//!   a schema document reaching `StartSchemaCreation` as the base64 its `definition` blob is
//!   serialized as. Each of them is also publishable as an Amazon S3 REFERENCE
//!   (`CodeS3Location` beside `Code`, and so on), which is READ into the property whose text it
//!   is before any call is made ([`super::appsync_s3_documents`]), so a declaration that ships
//!   its code in an object creates exactly what one stating that text inline creates.
//!
//! A declared property no call here carries is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid. A
//! `FunctionConfiguration`'s `MetricsConfig` is the one property a type here publishes that no
//! create models at all, so it is refused the same way ([`FUNCTION_PROPERTIES`]).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{applicable_tags, call_rest_json, percent_encoded, Created, Readiness, Refusal, Removal};
use crate::state::provisioner::{attribute_text, lower_camel_members, tags_as_map, tags_of};
use crate::state::template::{
    appsync_schema_api_id, appsync_schema_identifier, APPSYNC_API_ID_PROPERTY, APPSYNC_API_KEY,
    APPSYNC_ARN_NAMESPACE, APPSYNC_DATA_SOURCE, APPSYNC_FIELD_NAME_PROPERTY,
    APPSYNC_FUNCTION_CONFIGURATION, APPSYNC_GRAPHQL_API, APPSYNC_GRAPHQL_SCHEMA, APPSYNC_RESOLVER,
    APPSYNC_TYPE_NAME_PROPERTY,
};
use crate::state::StoredResource;

/// The SigV4 signing name of the service that owns these types, which is what routes the
/// in-process call to AWS AppSync's own handlers (that model's `sigv4Name`/`endpointPrefix`).
const APPSYNC_SIGNING_NAME: &str = "appsync";

/// The error code AppSync answers for a resource it does not hold (`NotFoundException`, the
/// modelled refusal of its reads, writes and deletes), which is what says a delete has nothing
/// left to remove and a read has nothing to report.
const NOT_FOUND: &str = "NotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// The code a create that answered 2xx without the identifier AppSync assigns is reported
/// under.
const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// How deeply a declared property document is already nested where [`lower_camel_members`] is
/// given it: the STARTING depth of that walk, which is zero for a property document handed to
/// it whole — the walk's own ceiling then bounds how far inside it may go, so a document
/// deeper than that is refused rather than walked.
const PROPERTY_NESTING: usize = 0;

/// Which of the six types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::AppSync::GraphQLApi`.
    GraphqlApi,
    /// `AWS::AppSync::GraphQLSchema`.
    GraphqlSchema,
    /// `AWS::AppSync::DataSource`.
    DataSource,
    /// `AWS::AppSync::Resolver`.
    Resolver,
    /// `AWS::AppSync::FunctionConfiguration`.
    FunctionConfiguration,
    /// `AWS::AppSync::ApiKey`.
    ApiKey,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        APPSYNC_GRAPHQL_API => Some(Kind::GraphqlApi),
        APPSYNC_GRAPHQL_SCHEMA => Some(Kind::GraphqlSchema),
        APPSYNC_DATA_SOURCE => Some(Kind::DataSource),
        APPSYNC_RESOLVER => Some(Kind::Resolver),
        APPSYNC_FUNCTION_CONFIGURATION => Some(Kind::FunctionConfiguration),
        APPSYNC_API_KEY => Some(Kind::ApiKey),
        _ => None,
    }
}

/// The property a resource is created under, where its type carries one.
const NAME: &str = "Name";

/// The property carrying a resource's tags, which reaches the API's create as a map.
const TAGS: &str = "Tags";

/// The member that tag map travels as.
const TAGS_MEMBER: &str = "tags";

/// The property carrying the environment variables of an API, which no create takes.
const ENVIRONMENT_VARIABLES: &str = "EnvironmentVariables";

/// The property carrying the schema document a GraphQL API serves.
const DEFINITION: &str = "Definition";

/// Every property `AWS::AppSync::GraphQLApi` publishes. A declared property this list does
/// not carry is refused rather than dropped.
const GRAPHQL_API_PROPERTIES: &[&str] = &[
    "AdditionalAuthenticationProviders",
    "ApiType",
    AUTHENTICATION_TYPE,
    "EnhancedMetricsConfig",
    ENVIRONMENT_VARIABLES,
    "IntrospectionConfig",
    "LambdaAuthorizerConfig",
    "LogConfig",
    "MergedApiExecutionRoleArn",
    NAME,
    "OpenIDConnectConfig",
    "OwnerContact",
    "QueryDepthLimit",
    "ResolverCountLimit",
    TAGS,
    "UserPoolConfig",
    "Visibility",
    "XrayEnabled",
];

/// The property naming how callers of the API authenticate, which `CreateGraphqlApi` requires.
const AUTHENTICATION_TYPE: &str = "AuthenticationType";

/// The properties of an API that travel as the create's own members, each the property with
/// its initial letter lower-cased.
const GRAPHQL_API_MEMBERS: &[&str] = &[
    "AdditionalAuthenticationProviders",
    "ApiType",
    "EnhancedMetricsConfig",
    "IntrospectionConfig",
    "LambdaAuthorizerConfig",
    "LogConfig",
    "MergedApiExecutionRoleArn",
    "OpenIDConnectConfig",
    "OwnerContact",
    "UserPoolConfig",
    "Visibility",
];

/// The properties of an API that travel as `UpdateGraphqlApi`'s own members: the create's,
/// minus the two that call models no member for — the KIND of API and where it is reachable
/// from, which are fixed when AppSync makes it, so a declaration restating either asks for a
/// different API (`crate::state::template::replacement_properties` names them) and such a
/// change never reaches the update at all.
const GRAPHQL_API_UPDATE_MEMBERS: &[&str] = &[
    "AdditionalAuthenticationProviders",
    "EnhancedMetricsConfig",
    "IntrospectionConfig",
    "LambdaAuthorizerConfig",
    "LogConfig",
    "MergedApiExecutionRoleArn",
    "OpenIDConnectConfig",
    "OwnerContact",
    "UserPoolConfig",
];

/// The members of an API AppSync models as a NUMBER rather than as text (`QueryDepthLimit`
/// `@range(min: 0, max: 75)` and `ResolverCountLimit` `@range(min: 0, max: 10000)`), so a
/// declaration that writes one as the text a parameter reference resolves to is sent as the
/// number it spells.
const GRAPHQL_API_NUMBERS: &[&str] = &["QueryDepthLimit", "ResolverCountLimit"];

/// The members of an API AppSync models as a BOOLEAN, read the same way.
const GRAPHQL_API_FLAGS: &[&str] = &["XrayEnabled"];

/// Every property `AWS::AppSync::GraphQLSchema` publishes.
const GRAPHQL_SCHEMA_PROPERTIES: &[&str] =
    &[APPSYNC_API_ID_PROPERTY, DEFINITION, "DefinitionS3Location"];

/// Every property `AWS::AppSync::DataSource` publishes.
const DATA_SOURCE_PROPERTIES: &[&str] = &[
    APPSYNC_API_ID_PROPERTY,
    "Description",
    DYNAMODB_CONFIG,
    "ElasticsearchConfig",
    "EventBridgeConfig",
    "HttpConfig",
    "LambdaConfig",
    "MetricsConfig",
    NAME,
    "OpenSearchServiceConfig",
    "RelationalDatabaseConfig",
    "ServiceRoleArn",
    "Type",
];

/// The one property of a data source AppSync does not spell as the property with a lower-cased
/// initial: the type publishes `DynamoDBConfig` and `CreateDataSource` takes `dynamodbConfig`.
const DYNAMODB_CONFIG: &str = "DynamoDBConfig";

/// The member `DynamoDBConfig` travels as.
const DYNAMODB_CONFIG_MEMBER: &str = "dynamodbConfig";

/// The properties of a data source that travel as the create's own members, each the property
/// with its initial letter lower-cased.
const DATA_SOURCE_MEMBERS: &[&str] = &[
    "Description",
    "ElasticsearchConfig",
    "EventBridgeConfig",
    "HttpConfig",
    "LambdaConfig",
    "MetricsConfig",
    "OpenSearchServiceConfig",
    "RelationalDatabaseConfig",
    "ServiceRoleArn",
    "Type",
];

/// Every property `AWS::AppSync::Resolver` publishes.
const RESOLVER_PROPERTIES: &[&str] = &[
    APPSYNC_API_ID_PROPERTY,
    "CachingConfig",
    "Code",
    "CodeS3Location",
    "DataSourceName",
    APPSYNC_FIELD_NAME_PROPERTY,
    "Kind",
    "MaxBatchSize",
    "MetricsConfig",
    "PipelineConfig",
    "RequestMappingTemplate",
    "RequestMappingTemplateS3Location",
    "ResponseMappingTemplate",
    "ResponseMappingTemplateS3Location",
    "Runtime",
    "SyncConfig",
    APPSYNC_TYPE_NAME_PROPERTY,
];

/// The properties of a resolver that travel as the create's own members.
const RESOLVER_MEMBERS: &[&str] = &[
    "CachingConfig",
    "Code",
    "DataSourceName",
    APPSYNC_FIELD_NAME_PROPERTY,
    "Kind",
    "MetricsConfig",
    "PipelineConfig",
    "RequestMappingTemplate",
    "ResponseMappingTemplate",
    "Runtime",
    "SyncConfig",
];

/// The properties of a resolver that travel as `UpdateResolver`'s own members: the create's,
/// minus the FIELD name — which that call takes as a URI label rather than in its body.
const RESOLVER_UPDATE_MEMBERS: &[&str] = &[
    "CachingConfig",
    "Code",
    "DataSourceName",
    "Kind",
    "MetricsConfig",
    "PipelineConfig",
    "RequestMappingTemplate",
    "ResponseMappingTemplate",
    "Runtime",
    "SyncConfig",
];

/// Every property of `AWS::AppSync::FunctionConfiguration` this module APPLIES.
///
/// The type publishes one more, `MetricsConfig`, which is deliberately not here:
/// `CreateFunction` models no `metricsConfig` member (a data source's and a resolver's creates
/// do), so a declaration stating it is refused by the one check that refuses an unapplied
/// property ([`refuse_unapplied_properties`]) rather than created without it.
const FUNCTION_PROPERTIES: &[&str] = &[
    APPSYNC_API_ID_PROPERTY,
    "Code",
    "CodeS3Location",
    "DataSourceName",
    "Description",
    "FunctionVersion",
    "MaxBatchSize",
    NAME,
    "RequestMappingTemplate",
    "RequestMappingTemplateS3Location",
    "ResponseMappingTemplate",
    "ResponseMappingTemplateS3Location",
    "Runtime",
    "SyncConfig",
];

/// The properties of a function that travel as the create's own members.
const FUNCTION_MEMBERS: &[&str] = &[
    "Code",
    "DataSourceName",
    "Description",
    "FunctionVersion",
    "RequestMappingTemplate",
    "ResponseMappingTemplate",
    "Runtime",
    "SyncConfig",
];

/// The member of a resolver and of a function AppSync models as a NUMBER (`MaxBatchSize`,
/// `@range(min: 0, max: 2000)`).
const BATCH_SIZE_NUMBERS: &[&str] = &["MaxBatchSize"];

/// Every property `AWS::AppSync::ApiKey` publishes.
const API_KEY_PROPERTIES: &[&str] = &[
    APPSYNC_API_ID_PROPERTY,
    API_KEY_ID,
    "Description",
    API_KEY_EXPIRES,
];

/// The property naming an API key's own identifier, which `CreateApiKey` does not take: that
/// call generates the key, so a declaration stating one asks for a key this emulator cannot
/// make.
const API_KEY_ID: &str = "ApiKeyId";

/// The member an API key's expiry travels as, which AppSync models as a `Long` (seconds since
/// the epoch).
const API_KEY_EXPIRES: &str = "Expires";

/// Create the resource one of these types declares, in AWS AppSync itself — so a client that
/// deployed the stack reads every one of them back through that service's own reads, and the
/// endpoint the stack published really serves the schema and the resolvers the template
/// declared.
///
/// A document the declaration holds in Amazon S3 rather than in the template is READ first and
/// created from as the property whose text it is ([`super::appsync_s3_documents`]), so the
/// create is made with exactly what a declaration stating that text inline would make it with.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, resource, properties_of(kind))?;
    let fetched = super::appsync_s3_documents::fetched(
        ctx,
        region,
        &resource.properties,
        super::appsync_s3_documents::GRAPHQL_ESTATE_DOCUMENTS,
    )
    .await?
    .map(|properties| StoredResource {
        properties,
        ..resource.clone()
    });
    let resource = fetched.as_ref().unwrap_or(resource);
    match kind {
        Kind::GraphqlApi => create_graphql_api(ctx, region, resource, propagated).await,
        Kind::GraphqlSchema => start_schema_creation(ctx, region, resource).await,
        Kind::DataSource => create_data_source(ctx, region, resource).await,
        Kind::Resolver => create_resolver(ctx, region, resource).await,
        Kind::FunctionConfiguration => create_function(ctx, region, resource).await,
        Kind::ApiKey => create_api_key(ctx, region, resource).await,
    }
}

/// Create the GraphQL API an `AWS::AppSync::GraphQLApi` declares, under the name the
/// declaration asked for.
///
/// AppSync assigns the API's identity: the call takes no identifier and answers the `apiId` it
/// minted, the ARN built on it — which is what this type's `Ref` returns — and the `uris` and
/// `dns` records of the two endpoints the API is now SERVED at, which no naming rule could
/// compute. All of them become the resource's own ([`api_attributes`]).
async fn create_graphql_api(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let mut request = Map::new();
    request.insert(
        "name".to_string(),
        Value::String(required(resource, properties, NAME)?),
    );
    request.insert(
        "authenticationType".to_string(),
        Value::String(required(resource, properties, AUTHENTICATION_TYPE)?),
    );
    carry(&mut request, properties, GRAPHQL_API_MEMBERS)?;
    carry_numbers(&mut request, properties, resource, GRAPHQL_API_NUMBERS)?;
    carry_flags(&mut request, properties, resource, GRAPHQL_API_FLAGS)?;
    let tags = tags_as_map(&tags_of(properties, &applicable_tags(propagated)));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_appsync(ctx, region, "POST", APIS_URI, Some(&Value::Object(request))).await?;
    let api = answered.get("graphqlApi").cloned().unwrap_or(Value::Null);
    let api_id = minted(&api, "apiId", "CreateGraphqlApi")?;
    let arn = minted(&api, "arn", "CreateGraphqlApi")?;
    // The variables an API holds are written by a call of their own, so they are applied to
    // the API this create just made rather than dropped. A put that service refuses leaves the
    // API configured with less than the declaration asked for, so the API this call made is
    // TAKEN AWAY again before the refusal is reported: the deployment records no physical id
    // for a failed create, so nothing else could delete it, and an API left behind under an id
    // the stack never recorded is one nothing can clean up.
    if let Some(variables) = stated(properties, ENVIRONMENT_VARIABLES) {
        let request = serde_json::json!({ "environmentVariables": variables });
        if let Err(refusal) = call_appsync(
            ctx,
            region,
            "PUT",
            &format!("{}/environmentVariables", api_uri(&api_id)),
            Some(&request),
        )
        .await
        {
            let _ = call_appsync(ctx, region, "DELETE", &api_uri(&api_id), None).await;
            return Err(refusal);
        }
    }
    Ok(Created {
        physical_resource_id: Some(arn.clone()),
        attributes: api_attributes(&api, &api_id, &arn),
    })
}

/// The attributes a GraphQL API publishes, read off what its own create answered: the id
/// AppSync minted, the ARN built on it, and the hostname and URL of each of the two endpoints
/// the API is served at (`uris`/`dns`, keyed `GRAPHQL` and `REALTIME`).
fn api_attributes(api: &Value, api_id: &str, arn: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    attributes.insert("ApiId".to_string(), api_id.to_string());
    attributes.insert("Arn".to_string(), arn.to_string());
    for (attribute, member, endpoint) in [
        ("GraphQLDns", "dns", GRAPHQL_ENDPOINT),
        ("GraphQLUrl", "uris", GRAPHQL_ENDPOINT),
        ("RealtimeDns", "dns", REALTIME_ENDPOINT),
        ("RealtimeUrl", "uris", REALTIME_ENDPOINT),
    ] {
        if let Some(value) = api
            .get(member)
            .and_then(|records| records.get(endpoint))
            .and_then(Value::as_str)
        {
            attributes.insert(attribute.to_string(), value.to_string());
        }
    }
    attributes
}

/// The key AppSync gives the GraphQL (HTTPS) endpoint in an API's `uris` and `dns` records.
const GRAPHQL_ENDPOINT: &str = "GRAPHQL";

/// The key it gives the real-time (WebSocket) endpoint in the same records.
const REALTIME_ENDPOINT: &str = "REALTIME";

/// Install the schema an `AWS::AppSync::GraphQLSchema` declares on the API it names.
///
/// "Adds a new schema to your GraphQL API. This operation is asynchronous" — the call answers
/// the schema `PROCESSING`, so the resource's creation is held open until that service reports
/// the creation finished ([`readiness`]). AppSync holds one schema per API and addresses it by
/// that API alone, so what identifies this resource is that API's id with the type's own
/// segment appended ([`appsync_schema_identifier`]).
async fn start_schema_creation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let api_id = required(resource, properties, APPSYNC_API_ID_PROPERTY)?;
    let definition = required(resource, properties, DEFINITION)?;
    // `StartSchemaCreation$definition` is a blob, which `restJson1` carries as base64.
    let request = serde_json::json!({
        "definition": ::aws_smithy_types::base64::encode(definition.as_bytes()),
    });
    call_appsync(
        ctx,
        region,
        "POST",
        &schema_creation_uri(&api_id),
        Some(&request),
    )
    .await?;
    Ok(Created {
        physical_resource_id: Some(appsync_schema_identifier(&api_id)),
        attributes: BTreeMap::new(),
    })
}

/// Create the data source an `AWS::AppSync::DataSource` declares, under the API its `ApiId`
/// names — so a resolver of that API really has a backend to run against.
///
/// `Ref` returns the data source's ARN, which `CreateDataSource` answers under
/// `dataSource.dataSourceArn`: it carries the API's minted id, so only that service can report
/// it.
async fn create_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let api_id = required(resource, properties, APPSYNC_API_ID_PROPERTY)?;
    let name = required(resource, properties, NAME)?;
    let mut request = Map::new();
    request.insert("name".to_string(), Value::String(name.clone()));
    carry(&mut request, properties, DATA_SOURCE_MEMBERS)?;
    carry_renamed(&mut request, properties, DYNAMODB_CONFIG, DYNAMODB_CONFIG_MEMBER)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &format!("{}/datasources", api_uri(&api_id)),
        Some(&Value::Object(request)),
    )
    .await?;
    let data_source = answered.get("dataSource").cloned().unwrap_or(Value::Null);
    let arn = minted(&data_source, "dataSourceArn", "CreateDataSource")?;
    let mut attributes = BTreeMap::new();
    attributes.insert("DataSourceArn".to_string(), arn.clone());
    attributes.insert("Name".to_string(), name);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Attach the resolver an `AWS::AppSync::Resolver` declares to the field it names, on the API
/// its `ApiId` names — so a query of that field is really resolved by what the template
/// declared.
///
/// `Ref` returns the resolver's ARN, which `CreateResolver` answers under
/// `resolver.resolverArn`.
async fn create_resolver(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let api_id = required(resource, properties, APPSYNC_API_ID_PROPERTY)?;
    let type_name = required(resource, properties, APPSYNC_TYPE_NAME_PROPERTY)?;
    let field_name = required(resource, properties, APPSYNC_FIELD_NAME_PROPERTY)?;
    let mut request = Map::new();
    carry(&mut request, properties, RESOLVER_MEMBERS)?;
    carry_numbers(&mut request, properties, resource, BATCH_SIZE_NUMBERS)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &format!("{}/resolvers", type_uri(&api_id, &type_name)),
        Some(&Value::Object(request)),
    )
    .await?;
    let resolver = answered.get("resolver").cloned().unwrap_or(Value::Null);
    let arn = minted(&resolver, "resolverArn", "CreateResolver")?;
    let mut attributes = BTreeMap::new();
    attributes.insert("FieldName".to_string(), field_name);
    attributes.insert("ResolverArn".to_string(), arn.clone());
    attributes.insert("TypeName".to_string(), type_name);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Create the pipeline function an `AWS::AppSync::FunctionConfiguration` declares, on the API
/// its `ApiId` names.
///
/// AppSync assigns the function's identity: `CreateFunction` answers the `functionId` it minted
/// and the `functionArn` built on it, which is what this type's `Ref` returns and what a
/// pipeline resolver names the function by.
async fn create_function(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let api_id = required(resource, properties, APPSYNC_API_ID_PROPERTY)?;
    let name = required(resource, properties, NAME)?;
    let mut request = Map::new();
    request.insert("name".to_string(), Value::String(name.clone()));
    carry(&mut request, properties, FUNCTION_MEMBERS)?;
    carry_numbers(&mut request, properties, resource, BATCH_SIZE_NUMBERS)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &format!("{}/functions", api_uri(&api_id)),
        Some(&Value::Object(request)),
    )
    .await?;
    let function = answered
        .get("functionConfiguration")
        .cloned()
        .unwrap_or(Value::Null);
    let arn = minted(&function, "functionArn", "CreateFunction")?;
    let function_id = minted(&function, "functionId", "CreateFunction")?;
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "DataSourceName".to_string(),
        stated_text(properties, "DataSourceName"),
    );
    attributes.insert("FunctionArn".to_string(), arn.clone());
    attributes.insert("FunctionId".to_string(), function_id);
    attributes.insert("Name".to_string(), name);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Create the API key an `AWS::AppSync::ApiKey` declares, on the API its `ApiId` names — so a
/// caller presenting the key the stack published is really admitted by that API.
///
/// AppSync GENERATES the key: `CreateApiKey` takes a description and an expiry and answers the
/// `apiKey.id` it made, which is both the key's own id and the value a caller sends in
/// `x-api-key`. What IDENTIFIES the resource is the ARN built on that id under the API that
/// holds it, which is what this type's `Ref` returns.
async fn create_api_key(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let api_id = required(resource, properties, APPSYNC_API_ID_PROPERTY)?;
    refuse_declared_key_id(properties)?;
    let mut request = Map::new();
    carry(&mut request, properties, &["Description"])?;
    carry_numbers(&mut request, properties, resource, &[API_KEY_EXPIRES])?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &format!("{}/apikeys", api_uri(&api_id)),
        Some(&Value::Object(request)),
    )
    .await?;
    let key = answered.get("apiKey").cloned().unwrap_or(Value::Null);
    let key_id = minted(&key, "id", "CreateApiKey")?;
    let mut attributes = BTreeMap::new();
    attributes.insert("ApiKey".to_string(), key_id.clone());
    attributes.insert(API_KEY_ID.to_string(), key_id.clone());
    // An API key is addressed under the API that holds it, as `DeleteApiKey`'s own URI
    // (`/v1/apis/{apiId}/apikeys/{id}`) addresses it, so its ARN is that path.
    let arn = key_arn(region, &api_id, &key_id);
    attributes.insert("Arn".to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// The ARN of one API key, built on the API that holds it and the key's own id — the path
/// `DeleteApiKey`'s own URI (`/v1/apis/{apiId}/apikeys/{id}`) addresses the key at.
fn key_arn(region: &str, api_id: &str, key_id: &str) -> String {
    ::emulator_server::emulator_arn(
        APPSYNC_ARN_NAMESPACE,
        region,
        &format!("apis/{api_id}/apikeys/{key_id}"),
    )
}

/// The refusal for a declaration that states the key's own identifier: AppSync GENERATES a
/// key, so neither its create nor its update takes one, and a declaration naming one asks for
/// a key this emulator cannot make.
fn refuse_declared_key_id(properties: &Value) -> Result<(), Refusal> {
    if stated(properties, API_KEY_ID).is_none() {
        return Ok(());
    }
    Err(Refusal {
        code: NOT_IMPLEMENTED.to_string(),
        message: format!(
            "AppSync generates an API key's own identifier, so this emulator does not create a \
             {APPSYNC_API_KEY} under the {API_KEY_ID} property a declaration states"
        ),
    })
}

/// Whether AppSync reports the created resource ready for a client to use.
///
/// Five of the six EXIST from the moment their create answers: that service reports no state an
/// API, a data source, a resolver, a function or an API key is still coming up in. A SCHEMA is
/// the exception — `StartSchemaCreation` "is asynchronous. Use GetSchemaCreationStatus to
/// determine when it has completed" — so that arm reads the status back, and a creation AppSync
/// reports `FAILED` fails the resource with the detail it gave rather than waiting the window
/// out.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    if kind != Kind::GraphqlSchema {
        return Ok(Readiness::Ready);
    }
    let answered = call_appsync(
        ctx,
        region,
        "GET",
        &schema_creation_uri(appsync_schema_api_id(physical_resource_id)),
        None,
    )
    .await?;
    let status = answered
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let details = answered
        .get("details")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match status.as_str() {
        SCHEMA_SUCCESS | SCHEMA_ACTIVE => Ok(Readiness::Ready),
        SCHEMA_FAILED => Ok(Readiness::Failed(details)),
        _ => Ok(Readiness::Pending(status)),
    }
}

/// The `SchemaStatus` AppSync reports a schema it has installed in.
const SCHEMA_SUCCESS: &str = "SUCCESS";

/// The `SchemaStatus` of a schema that is installed and in use, which is the other terminal
/// state that enum carries beside [`SCHEMA_SUCCESS`].
const SCHEMA_ACTIVE: &str = "ACTIVE";

/// The `SchemaStatus` of a creation AppSync could not carry out.
const SCHEMA_FAILED: &str = "FAILED";

/// Whether AppSync has finished removing a deleted resource.
///
/// Nothing is asked: each of `DeleteGraphqlApi`, `DeleteDataSource`, `DeleteResolver`,
/// `DeleteFunction` and `DeleteApiKey` takes the resource away as it answers, and that service
/// reports no state a removed one is still being deleted in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has, through the call AppSync
/// publishes for changing one.
///
/// Each type has exactly one: `UpdateGraphqlApi`, `UpdateDataSource`, `UpdateResolver`,
/// `UpdateFunction` and `UpdateApiKey`, each bound to the URI that addresses the resource
/// itself, and each taking the same members its create takes minus the ones the URI binds. A
/// SCHEMA is the one whose change AppSync applies through the call that created it —
/// `StartSchemaCreation` "adds a new schema to your GraphQL API", and the document a
/// declaration states is the whole type system — so a changed definition is installed by
/// starting that creation again on the API the resource already names.
///
/// `Ok(None)` is a declaration that restates a value the resource's IDENTITY is built from
/// ([`identity_moved`]): that asks for a different resource, which the deployment replaces
/// rather than changing in place.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, resource, properties_of(kind))?;
    // A changed declaration states its documents the same two ways a create's does, so a
    // reference it holds is read and applied as the property whose text it is.
    let fetched = super::appsync_s3_documents::fetched(
        ctx,
        region,
        declared,
        super::appsync_s3_documents::GRAPHQL_ESTATE_DOCUMENTS,
    )
    .await?;
    let declared = fetched.as_ref().unwrap_or(declared);
    if identity_moved(kind, resource, declared) {
        return Ok(None);
    }
    match kind {
        Kind::GraphqlApi => update_graphql_api(ctx, region, resource, declared).await,
        Kind::GraphqlSchema => install_schema(ctx, region, resource, declared).await,
        Kind::DataSource => update_data_source(ctx, region, resource, declared).await,
        Kind::Resolver => update_resolver(ctx, region, resource, declared).await,
        Kind::FunctionConfiguration => update_function(ctx, region, resource, declared).await,
        Kind::ApiKey => update_api_key(ctx, region, resource, declared).await,
    }
    .map(Some)
}

/// Whether a declaration states one of the values the resource's identity is built from
/// differently from the resource the stack has: the API every child lives under, the NAME a
/// data source is addressed by, and the type and field a resolver is attached to. None of
/// those is a member of the owning update call — each is a URI label of it — so a declaration
/// restating one asks for a different resource
/// (`crate::state::template::replacing_properties` is the one reading of that, and what makes
/// the deployment replace such a resource before it reaches here).
fn identity_moved(kind: Kind, resource: &StoredResource, declared: &Value) -> bool {
    let held = &resource.properties;
    // A schema's identity is the API it belongs to, which is what its physical id carries.
    if kind == Kind::GraphqlSchema {
        return stated_text(declared, APPSYNC_API_ID_PROPERTY)
            != appsync_schema_api_id(&resource.physical_resource_id);
    }
    let bound: &[&str] = match kind {
        Kind::GraphqlApi => &[],
        Kind::DataSource => &[APPSYNC_API_ID_PROPERTY, NAME],
        Kind::Resolver => &[
            APPSYNC_API_ID_PROPERTY,
            APPSYNC_TYPE_NAME_PROPERTY,
            APPSYNC_FIELD_NAME_PROPERTY,
        ],
        Kind::FunctionConfiguration | Kind::ApiKey => &[APPSYNC_API_ID_PROPERTY],
        Kind::GraphqlSchema => &[],
    };
    bound
        .iter()
        .any(|property| stated_text(declared, property) != stated_text(held, property))
}

/// Apply a changed declaration of the API itself, through `UpdateGraphqlApi` — which takes the
/// API's whole configuration (its `name` and `authenticationType` are both required members),
/// so a changed declaration is applied by putting it, and the variables it holds by the one
/// call AppSync publishes for them.
async fn update_graphql_api(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let api_id = api_id_of(resource)?;
    let mut request = Map::new();
    request.insert(
        "name".to_string(),
        Value::String(required(resource, declared, NAME)?),
    );
    request.insert(
        "authenticationType".to_string(),
        Value::String(required(resource, declared, AUTHENTICATION_TYPE)?),
    );
    carry(&mut request, declared, GRAPHQL_API_UPDATE_MEMBERS)?;
    carry_numbers(&mut request, declared, resource, GRAPHQL_API_NUMBERS)?;
    carry_flags(&mut request, declared, resource, GRAPHQL_API_FLAGS)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &api_uri(&api_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let api = answered.get("graphqlApi").cloned().unwrap_or(Value::Null);
    let arn = minted(&api, "arn", "UpdateGraphqlApi")?;
    // The variables an API holds are written by a call of their own, so a change to them is
    // applied to the API this update just changed rather than dropped. That call takes the
    // WHOLE map, so a declaration that no longer states any is applied by putting an empty
    // one — and an API neither revision states variables for is left alone.
    let stated_now = stated(declared, ENVIRONMENT_VARIABLES);
    if stated_now.is_some() || stated(&resource.properties, ENVIRONMENT_VARIABLES).is_some() {
        let variables = stated_now.unwrap_or_else(|| Value::Object(Map::new()));
        let request = serde_json::json!({ "environmentVariables": variables });
        call_appsync(
            ctx,
            region,
            "PUT",
            &format!("{}/environmentVariables", api_uri(&api_id)),
            Some(&request),
        )
        .await?;
    }
    Ok(Created {
        physical_resource_id: Some(arn.clone()),
        attributes: api_attributes(&api, &api_id, &arn),
    })
}

/// Install the schema a changed `AWS::AppSync::GraphQLSchema` declares, on the API the resource
/// already names — the same asynchronous creation its own create starts, so the resource is
/// held open until AppSync reports it installed ([`readiness`]).
async fn install_schema(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let definition = required(resource, declared, DEFINITION)?;
    let request = serde_json::json!({
        "definition": ::aws_smithy_types::base64::encode(definition.as_bytes()),
    });
    call_appsync(
        ctx,
        region,
        "POST",
        &schema_creation_uri(appsync_schema_api_id(&resource.physical_resource_id)),
        Some(&request),
    )
    .await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// Apply a changed declaration of a data source, through `UpdateDataSource` — which addresses
/// the data source by the API and the name it was created under and takes the rest of what its
/// create takes.
async fn update_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let name = required(resource, declared, NAME)?;
    let mut request = Map::new();
    carry(&mut request, declared, DATA_SOURCE_MEMBERS)?;
    carry_renamed(&mut request, declared, DYNAMODB_CONFIG, DYNAMODB_CONFIG_MEMBER)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &format!(
            "{}/datasources/{}",
            api_uri(&api_id_of(resource)?),
            percent_encoded(&name)
        ),
        Some(&Value::Object(request)),
    )
    .await?;
    let data_source = answered.get("dataSource").cloned().unwrap_or(Value::Null);
    let arn = minted(&data_source, "dataSourceArn", "UpdateDataSource")?;
    let mut attributes = BTreeMap::new();
    attributes.insert("DataSourceArn".to_string(), arn.clone());
    attributes.insert("Name".to_string(), name);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration of a resolver, through `UpdateResolver` — which addresses the
/// resolver by the API, the type and the field it is attached to and takes the rest of what its
/// create takes.
async fn update_resolver(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let type_name = required(resource, declared, APPSYNC_TYPE_NAME_PROPERTY)?;
    let field_name = required(resource, declared, APPSYNC_FIELD_NAME_PROPERTY)?;
    let mut request = Map::new();
    carry(&mut request, declared, RESOLVER_UPDATE_MEMBERS)?;
    carry_numbers(&mut request, declared, resource, BATCH_SIZE_NUMBERS)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &format!(
            "{}/resolvers/{}",
            type_uri(&api_id_of(resource)?, &type_name),
            percent_encoded(&field_name)
        ),
        Some(&Value::Object(request)),
    )
    .await?;
    let resolver = answered.get("resolver").cloned().unwrap_or(Value::Null);
    let arn = minted(&resolver, "resolverArn", "UpdateResolver")?;
    let mut attributes = BTreeMap::new();
    attributes.insert("FieldName".to_string(), field_name);
    attributes.insert("ResolverArn".to_string(), arn.clone());
    attributes.insert("TypeName".to_string(), type_name);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration of a pipeline function, through `UpdateFunction` — which
/// addresses the function by the API and the `functionId` AppSync minted and takes a new name
/// beside the rest of what its create takes, so a renamed function is the SAME function under
/// another name.
async fn update_function(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let name = required(resource, declared, NAME)?;
    let mut request = Map::new();
    request.insert("name".to_string(), Value::String(name.clone()));
    carry(&mut request, declared, FUNCTION_MEMBERS)?;
    carry_numbers(&mut request, declared, resource, BATCH_SIZE_NUMBERS)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &function_uri(resource)?,
        Some(&Value::Object(request)),
    )
    .await?;
    let function = answered
        .get("functionConfiguration")
        .cloned()
        .unwrap_or(Value::Null);
    let arn = minted(&function, "functionArn", "UpdateFunction")?;
    let function_id = minted(&function, "functionId", "UpdateFunction")?;
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "DataSourceName".to_string(),
        stated_text(declared, "DataSourceName"),
    );
    attributes.insert("FunctionArn".to_string(), arn.clone());
    attributes.insert("FunctionId".to_string(), function_id);
    attributes.insert("Name".to_string(), name);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration of an API key, through `UpdateApiKey` — which addresses the key
/// by the API and the key's own id and takes the description and the expiry, the two members a
/// declaration states beside them. The key itself is NOT reissued: it is the value AppSync
/// generated, which this call leaves as it is.
async fn update_api_key(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    refuse_declared_key_id(declared)?;
    let mut request = Map::new();
    carry(&mut request, declared, &["Description"])?;
    carry_numbers(&mut request, declared, resource, &[API_KEY_EXPIRES])?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &api_key_uri(resource)?,
        Some(&Value::Object(request)),
    )
    .await?;
    let key = answered.get("apiKey").cloned().unwrap_or(Value::Null);
    let key_id = minted(&key, "id", "UpdateApiKey")?;
    let arn = key_arn(region, &api_id_of(resource)?, &key_id);
    let mut attributes = BTreeMap::new();
    attributes.insert("ApiKey".to_string(), key_id.clone());
    attributes.insert(API_KEY_ID.to_string(), key_id);
    attributes.insert("Arn".to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Delete the resource a stack created, addressed exactly as its own read is.
///
/// A resource AppSync no longer holds is already in the state the delete asks for, judged on
/// the CODE and never on a message — which is also what deleting the API that held it leaves
/// behind, since an API owns its schema, its data sources, its resolvers, its functions and its
/// keys. A SCHEMA is taken away by no call at all: AppSync publishes none for one, so deleting
/// the API is what removes it.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let Some(uri) = addressed(kind, resource)? else {
        return Ok(());
    };
    match call_appsync(ctx, region, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as AppSync reports it, for drift detection: that resource's own
/// read, under this type's property names ([`reported_properties`]).
///
/// `None` is a resource AppSync no longer holds, which is a deleted one. A SCHEMA and an API
/// KEY report nothing here: AppSync publishes no read of one schema's document in the shape a
/// declaration states it, and none of a single key at all — so the check is reported as one
/// this emulator does not make rather than as a difference nobody made.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let (uri, member) = match kind {
        Kind::GraphqlApi => (api_uri(&api_id_of(resource)?), "graphqlApi"),
        Kind::DataSource => (
            format!(
                "{}/datasources/{}",
                api_uri(&api_id_of(resource)?),
                percent_encoded(&required(resource, &resource.properties, NAME)?)
            ),
            "dataSource",
        ),
        Kind::Resolver => (
            resolver_uri(resource)?,
            "resolver",
        ),
        Kind::FunctionConfiguration => (function_uri(resource)?, "functionConfiguration"),
        Kind::GraphqlSchema | Kind::ApiKey => {
            return Err(Refusal {
                code: NOT_IMPLEMENTED.to_string(),
                message: format!(
                    "this emulator does not read the configuration of a {} back for drift \
                     detection",
                    resource.resource_type
                ),
            })
        }
    };
    let answered = match call_appsync(ctx, region, "GET", &uri, None).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let read = answered.get(member).cloned().unwrap_or(Value::Null);
    let mut held = Map::new();
    for property in reported_properties(kind) {
        if let Some(value) = read.get(lower_initial(property)) {
            held.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(held)))
}

/// The collection every GraphQL API of the account lives in, which `CreateGraphqlApi` is bound
/// to.
const APIS_URI: &str = "/v1/apis";

/// The URI that addresses ONE API: the `@httpLabel` every call on it binds.
fn api_uri(api_id: &str) -> String {
    format!("{APIS_URI}/{}", percent_encoded(api_id))
}

/// The URI the schema of one API is created and read at.
fn schema_creation_uri(api_id: &str) -> String {
    format!("{}/schemacreation", api_uri(api_id))
}

/// The URI that addresses one TYPE of an API, which a resolver hangs off.
fn type_uri(api_id: &str, type_name: &str) -> String {
    format!(
        "{}/types/{}",
        api_uri(api_id),
        percent_encoded(type_name)
    )
}

/// The URI that addresses one resolver, as `GetResolver` and `DeleteResolver` bind it.
fn resolver_uri(resource: &StoredResource) -> Result<String, Refusal> {
    let properties = &resource.properties;
    let api_id = api_id_of(resource)?;
    let type_name = required(resource, properties, APPSYNC_TYPE_NAME_PROPERTY)?;
    let field_name = required(resource, properties, APPSYNC_FIELD_NAME_PROPERTY)?;
    Ok(format!(
        "{}/resolvers/{}",
        type_uri(&api_id, &type_name),
        percent_encoded(&field_name)
    ))
}

/// The URI that addresses one pipeline function, which AppSync keys by the `functionId` it
/// minted — the attribute the create recorded on the resource.
fn function_uri(resource: &StoredResource) -> Result<String, Refusal> {
    let function_id = resource
        .attributes
        .get("FunctionId")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "the {} this stack created carries no FunctionId, so AppSync cannot be asked \
                 about it",
                resource.resource_type
            ),
        })?;
    Ok(format!(
        "{}/functions/{}",
        api_uri(&api_id_of(resource)?),
        percent_encoded(function_id)
    ))
}

/// The URI the DELETE of one resource of `kind` is bound to, or `None` for a resource AppSync
/// publishes no delete for.
fn addressed(kind: Kind, resource: &StoredResource) -> Result<Option<String>, Refusal> {
    let uri = match kind {
        Kind::GraphqlApi => api_uri(&api_id_of(resource)?),
        // AppSync publishes no call that removes a schema from an API: the schema goes with
        // the API, so the stack's own deletion of the API is what takes it away.
        Kind::GraphqlSchema => return Ok(None),
        Kind::DataSource => format!(
            "{}/datasources/{}",
            api_uri(&api_id_of(resource)?),
            percent_encoded(&required(resource, &resource.properties, NAME)?)
        ),
        Kind::Resolver => resolver_uri(resource)?,
        Kind::FunctionConfiguration => function_uri(resource)?,
        Kind::ApiKey => api_key_uri(resource)?,
    };
    Ok(Some(uri))
}

/// The URI that addresses one API key, as `UpdateApiKey` and `DeleteApiKey` bind it: the API
/// that holds the key, and the key's own id — which is the attribute the create recorded, the
/// resource's physical id being the ARN built on it.
fn api_key_uri(resource: &StoredResource) -> Result<String, Refusal> {
    let key_id = resource
        .attributes
        .get(API_KEY_ID)
        .filter(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| last_segment(&resource.physical_resource_id).to_string());
    if key_id.is_empty() {
        return Err(Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "the {} this stack created carries no {API_KEY_ID}, so AppSync cannot be asked \
                 about it",
                resource.resource_type
            ),
        });
    }
    Ok(format!(
        "{}/apikeys/{}",
        api_uri(&api_id_of(resource)?),
        percent_encoded(&key_id)
    ))
}

/// The last `/`-separated segment of an identifier, which is the key id an API key's ARN ends
/// in.
fn last_segment(identifier: &str) -> &str {
    identifier.rsplit('/').next().unwrap_or(identifier)
}

/// The API id one of these resources belongs to: the one its declaration names, and for the
/// API itself the id its own create answered ([`api_attributes`] records it as `ApiId`, and the
/// physical id is the ARN that id is built into).
fn api_id_of(resource: &StoredResource) -> Result<String, Refusal> {
    if let Some(api_id) = resource
        .attributes
        .get("ApiId")
        .filter(|api_id| !api_id.is_empty())
    {
        return Ok(api_id.clone());
    }
    required(resource, &resource.properties, APPSYNC_API_ID_PROPERTY)
}

/// Every property the type of `kind` publishes, for the one check that refuses a declared
/// property no call this module makes carries.
fn properties_of(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::GraphqlApi => GRAPHQL_API_PROPERTIES,
        Kind::GraphqlSchema => GRAPHQL_SCHEMA_PROPERTIES,
        Kind::DataSource => DATA_SOURCE_PROPERTIES,
        Kind::Resolver => RESOLVER_PROPERTIES,
        Kind::FunctionConfiguration => FUNCTION_PROPERTIES,
        Kind::ApiKey => API_KEY_PROPERTIES,
    }
}

/// The members of a resource's own read that are COMPARED against its declaration, each named
/// identically on both sides but for the lower-cased initial AppSync spells its members with.
/// A member that is part of the resource's identity — the ARNs and ids the read answers — is
/// left out: it is not something a declaration states.
fn reported_properties(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::GraphqlApi => &[
            NAME,
            AUTHENTICATION_TYPE,
            "ApiType",
            "Visibility",
            "OwnerContact",
            "XrayEnabled",
        ],
        Kind::DataSource => &["Name", "Type", "Description", "ServiceRoleArn"],
        Kind::Resolver => &[
            APPSYNC_TYPE_NAME_PROPERTY,
            APPSYNC_FIELD_NAME_PROPERTY,
            "DataSourceName",
            "Kind",
            "RequestMappingTemplate",
            "ResponseMappingTemplate",
        ],
        Kind::FunctionConfiguration => &[
            NAME,
            "DataSourceName",
            "Description",
            "FunctionVersion",
            "RequestMappingTemplate",
            "ResponseMappingTemplate",
        ],
        Kind::GraphqlSchema | Kind::ApiKey => &[],
    }
}

/// Copy the declared properties `carried` onto the request under the member names the owning
/// create spells them with — the property with its initial letter lower-cased, inside and out
/// ([`lower_camel_members`]). A property the declaration does not state is left out rather
/// than sent empty, so AppSync's own validation judges the request it would judge for a
/// client's own call.
fn carry(
    request: &mut Map<String, Value>,
    properties: &Value,
    carried: &[&str],
) -> Result<(), Refusal> {
    for property in carried {
        if let Some(value) = stated(properties, property) {
            request.insert(
                lower_initial(property),
                lower_camel_members(&value, PROPERTY_NESTING)?,
            );
        }
    }
    Ok(())
}

/// [`carry`] for the ONE property whose member is not the property with a lower-cased initial.
fn carry_renamed(
    request: &mut Map<String, Value>,
    properties: &Value,
    property: &str,
    member: &str,
) -> Result<(), Refusal> {
    if let Some(value) = stated(properties, property) {
        request.insert(
            member.to_string(),
            lower_camel_members(&value, PROPERTY_NESTING)?,
        );
    }
    Ok(())
}

/// [`carry`] for the members AppSync models as a NUMBER: a declaration that writes one as the
/// text a parameter reference resolves to is sent as the number it spells, and a value that is
/// no number at all is refused here — naming the property — rather than sent as a request that
/// member cannot carry.
fn carry_numbers(
    request: &mut Map<String, Value>,
    properties: &Value,
    resource: &StoredResource,
    members: &[&str],
) -> Result<(), Refusal> {
    for property in members {
        let Some(value) = stated(properties, property) else {
            continue;
        };
        let number = declared_number(&value).ok_or_else(|| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "the {property} property of {} is a number, and {} is not one",
                resource.resource_type,
                attribute_text(&value)
            ),
        })?;
        request.insert(lower_initial(property), number);
    }
    Ok(())
}

/// [`carry_numbers`] for the members AppSync models as a BOOLEAN, read the same way: `true` and
/// `false` in either JSON's own spelling or the text a parameter resolves to.
fn carry_flags(
    request: &mut Map<String, Value>,
    properties: &Value,
    resource: &StoredResource,
    members: &[&str],
) -> Result<(), Refusal> {
    for property in members {
        let Some(value) = stated(properties, property) else {
            continue;
        };
        let flag = match &value {
            Value::Bool(flag) => Some(*flag),
            Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "the {property} property of {} is a boolean, and {} is not one",
                resource.resource_type,
                attribute_text(&value)
            ),
        })?;
        request.insert(lower_initial(property), Value::Bool(flag));
    }
    Ok(())
}

/// One declared value as the NUMBER it spells, in either JSON's own spelling or the text a
/// parameter reference resolves to; `None` for a value that is no number at all.
fn declared_number(value: &Value) -> Option<Value> {
    match value {
        Value::Number(_) => Some(value.clone()),
        Value::String(text) => {
            let text = text.trim();
            if let Ok(integer) = text.parse::<i64>() {
                return Some(Value::from(integer));
            }
            text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number)
        }
        _ => None,
    }
}

/// One property as the declaration states it, or `None` where it states none.
fn stated(properties: &Value, property: &str) -> Option<Value> {
    properties
        .get(property)
        .filter(|value| !value.is_null())
        .cloned()
}

/// One property as the TEXT it carries, or the empty text where the declaration states none.
fn stated_text(properties: &Value, property: &str) -> String {
    stated(properties, property)
        .as_ref()
        .map(attribute_text)
        .unwrap_or_default()
}

/// One property a call cannot be made without, as the text it carries, or the refusal for a
/// declaration that states none.
fn required(
    resource: &StoredResource,
    properties: &Value,
    property: &str,
) -> Result<String, Refusal> {
    match stated(properties, property).as_ref().map(attribute_text) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!("{} requires the {property} property", resource.resource_type),
        }),
    }
}

/// The member name a property travels under: the property with its initial letter lower-cased,
/// which is how AppSync's model spells every member these types publish as a property.
fn lower_initial(property: &str) -> String {
    let mut characters = property.chars();
    match characters.next() {
        Some(initial) => initial.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The refusal for a declared property no call this module makes carries, naming which property
/// it was: refusing is what keeps a resource from being reported as created — or as updated —
/// with less than its declaration asked for. `properties` is the document the deployment
/// DECLARES, which on an update is not the one the record carries.
fn refuse_unapplied_properties(
    properties: &Value,
    resource: &StoredResource,
    published: &[&str],
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if published.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    Ok(())
}

/// One identifier AppSync MINTED, read off the record its create answered.
///
/// A create that answered 2xx WITHOUT it identifies no resource, so the resource is reported as
/// failed rather than recorded under an identifier nothing answers to.
fn minted(record: &Value, member: &str, operation: &str) -> Result<String, Refusal> {
    record
        .get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!("AppSync answered {operation} without a {member}"),
        })
}

/// Call one AWS AppSync operation on the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding states.
async fn call_appsync(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json(ctx, APPSYNC_SIGNING_NAME, region, method, uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `StoredResource` of one of these types, as a deployment hands it to the arms above.
    fn declared(resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Subject".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: "abcdefghijklmnopqrstuvwxyz".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// The six type names this module owns are exactly the ones it claims, and nothing else: a
    /// near miss of one is another type's, which this module must not answer for.
    #[test]
    fn the_owned_types_are_the_six_appsync_types() {
        let cases: &[(&str, Option<Kind>)] = &[
            (APPSYNC_GRAPHQL_API, Some(Kind::GraphqlApi)),
            (APPSYNC_GRAPHQL_SCHEMA, Some(Kind::GraphqlSchema)),
            (APPSYNC_DATA_SOURCE, Some(Kind::DataSource)),
            (APPSYNC_RESOLVER, Some(Kind::Resolver)),
            (
                APPSYNC_FUNCTION_CONFIGURATION,
                Some(Kind::FunctionConfiguration),
            ),
            (APPSYNC_API_KEY, Some(Kind::ApiKey)),
            ("AWS::AppSync::Api", None),
            ("AWS::AppSync::ChannelNamespace", None),
            ("AWS::AppSync::DomainName", None),
            ("AWS::SQS::Queue", None),
            ("", None),
        ];
        for (resource_type, expected) in cases {
            let actual = kind_of(resource_type);
            assert_eq!(
                actual, *expected,
                "case {resource_type:?}: expected kind_of {expected:?}, got {actual:?}"
            );
        }
    }

    /// Each type's delete addresses the resource exactly as its own operation's `@http` binding
    /// does — and a SCHEMA is addressed by no delete at all, because AppSync publishes none.
    #[test]
    fn a_resource_is_addressed_as_its_own_operation_binds_it() {
        /// The resource of `resource_type` a create has already answered for: the API id
        /// AppSync minted is the attribute that create recorded.
        fn created(resource_type: &str, properties: Value, attributes: &[(&str, &str)]) -> StoredResource {
            let mut resource = declared(resource_type, properties);
            for (name, value) in attributes {
                resource
                    .attributes
                    .insert((*name).to_string(), (*value).to_string());
            }
            resource
        }
        let cases: &[(&str, Kind, StoredResource, Option<&str>)] = &[
            (
                "an api",
                Kind::GraphqlApi,
                created(
                    APPSYNC_GRAPHQL_API,
                    serde_json::json!({ "Name": "shop" }),
                    &[("ApiId", "theapiid")],
                ),
                Some("/v1/apis/theapiid"),
            ),
            (
                "a schema",
                Kind::GraphqlSchema,
                declared(
                    APPSYNC_GRAPHQL_SCHEMA,
                    serde_json::json!({ "ApiId": "theapiid" }),
                ),
                None,
            ),
            (
                "a data source",
                Kind::DataSource,
                declared(
                    APPSYNC_DATA_SOURCE,
                    serde_json::json!({ "ApiId": "theapiid", "Name": "Orders" }),
                ),
                Some("/v1/apis/theapiid/datasources/Orders"),
            ),
            (
                "a resolver",
                Kind::Resolver,
                declared(
                    APPSYNC_RESOLVER,
                    serde_json::json!({
                        "ApiId": "theapiid",
                        "TypeName": "Mutation",
                        "FieldName": "addOrder"
                    }),
                ),
                Some("/v1/apis/theapiid/types/Mutation/resolvers/addOrder"),
            ),
            (
                "a function",
                Kind::FunctionConfiguration,
                created(
                    APPSYNC_FUNCTION_CONFIGURATION,
                    serde_json::json!({ "ApiId": "theapiid", "Name": "Enrich" }),
                    &[("FunctionId", "fn-1")],
                ),
                Some("/v1/apis/theapiid/functions/fn-1"),
            ),
            (
                "an api key",
                Kind::ApiKey,
                declared(APPSYNC_API_KEY, serde_json::json!({ "ApiId": "theapiid" })),
                Some("/v1/apis/theapiid/apikeys/abcdefghijklmnopqrstuvwxyz"),
            ),
        ];
        for (case, kind, resource, expected) in cases {
            let actual = addressed(*kind, resource)
                .unwrap_or_else(|refusal| panic!("case {case:?}: expected a URI, got {refusal}"));
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected the delete to be bound to {expected:?}, got {actual:?}"
            );
        }
    }

    /// A number a template writes as text is the same number as one written as JSON's own, and
    /// a value that is no number at all is none.
    #[test]
    fn a_number_is_read_in_either_spelling() {
        let cases: &[(&str, Value, Option<Value>)] = &[
            ("a JSON integer", serde_json::json!(75), Some(serde_json::json!(75))),
            ("the text of one", serde_json::json!("75"), Some(serde_json::json!(75))),
            ("text with spaces", serde_json::json!(" 0 "), Some(serde_json::json!(0))),
            ("a word", serde_json::json!("deep"), None),
            ("a boolean", serde_json::json!(true), None),
        ];
        for (case, value, expected) in cases {
            let actual = declared_number(value);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected declared_number {expected:?}, got {actual:?}"
            );
        }
    }

    /// A property no call this module makes carries is refused, naming it, so nothing is
    /// reported created with less than the declaration asked for — and every property each type
    /// publishes and applies is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let resource = declared(APPSYNC_GRAPHQL_API, Value::Null);
        let published = serde_json::json!({
            "Name": "shop",
            "AuthenticationType": "AMAZON_COGNITO_USER_POOLS",
            "UserPoolConfig": { "UserPoolId": "us-east-1_abc", "DefaultAction": "DENY" },
            "LogConfig": { "FieldLogLevel": "ERROR" },
            "XrayEnabled": true,
            "QueryDepthLimit": 4,
            "Tags": []
        });
        assert!(
            refuse_unapplied_properties(&published, &resource, GRAPHQL_API_PROPERTIES).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        // (case, the declaration, the type's applied set, the property the refusal must name)
        let refused: &[(&str, Value, &[&str], &str)] = &[
            (
                "a property no call carries",
                serde_json::json!({ "Name": "shop", "NotAProperty": "x" }),
                GRAPHQL_API_PROPERTIES,
                "NotAProperty",
            ),
            (
                "a function's MetricsConfig, which CreateFunction models no member for",
                serde_json::json!({
                    "ApiId": "theapiid",
                    "Name": "Enrich",
                    "DataSourceName": "RemoteSource",
                    "MetricsConfig": "ENABLED"
                }),
                FUNCTION_PROPERTIES,
                "MetricsConfig",
            ),
            (
                "a property no call carries on an api key",
                serde_json::json!({ "ApiId": "theapiid", "NotAProperty": "x" }),
                API_KEY_PROPERTIES,
                "NotAProperty",
            ),
        ];
        for (case, declaration, applied, named) in refused {
            let refusal = refuse_unapplied_properties(declaration, &resource, applied)
                .expect_err(&format!("case {case:?}: expected a refusal"));
            assert!(
                refusal.message.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {:?}",
                refusal.message
            );
        }
        // ...and a function declaration stating only properties this module applies is accepted.
        let applied = serde_json::json!({
            "ApiId": "theapiid",
            "Name": "Enrich",
            "DataSourceName": "RemoteSource",
            "FunctionVersion": "2018-05-29",
            "MaxBatchSize": 10
        });
        assert!(
            refuse_unapplied_properties(&applied, &resource, FUNCTION_PROPERTIES).is_ok(),
            "case \"a function's applied properties\": expected them to be applied, got a refusal"
        );
    }
}
