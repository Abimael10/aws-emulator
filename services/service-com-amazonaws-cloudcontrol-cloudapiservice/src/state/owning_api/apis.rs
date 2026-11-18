// SPDX-License-Identifier: CC0-1.0

//! `AWS::ApiGatewayV2::Api`, as it lives in the Amazon API Gateway v2 service itself.
//!
//! An api is not a record of this service: `apigatewayv2:CreateApi` creates it,
//! `apigatewayv2:ImportApi` creates one FROM an OpenAPI definition — together with the
//! routes and integrations that document declares — `apigatewayv2:GetApi` reports it,
//! `apigatewayv2:UpdateApi` and `apigatewayv2:ReimportApi` change it and
//! `apigatewayv2:DeleteApi` removes it. A create of this type therefore does work of its
//! own in another service, which is why every answer here is a call on THAT service's own
//! API, dispatched in process ([`in_process_apigatewayv2`]) rather than a write into a
//! store of this one: an api any API created is one this service resolves, and an api that
//! service no longer holds is one it reports as not found.
//!
//! ## The resource model
//!
//! Catalog source: the `AWS::ApiGatewayV2::Api` resource provider schema's `properties`,
//! whose `primaryIdentifier` is `/properties/ApiId` — the identifier API Gateway v2 assigns
//! when it makes the api — and whose `readOnlyProperties` are that identifier and the
//! `ApiEndpoint` it serves the api on ([`crate::state::read_only_properties`] holds them).
//!
//! The model a read reports is exactly what `GetApi` answers under those property names:
//! the api's identity and endpoint, its name and protocol, its selection expressions, its
//! CORS configuration, its description and version, its two switches, its address type and
//! its tags. The properties a create SETS and that read does not report — the definition an
//! api is imported from and how it is read, and the `RouteKey`, `Target` and `CredentialsArn`
//! of a quick-create api — are this type's `writeOnlyProperties`
//! ([`crate::state::write_only_properties`]), so no model this service reports carries one.
//!
//! ## What a refusal carries
//!
//! A call API Gateway v2 refused is reported as that service's own verdict
//! ([`refusal_of`]): the error code it named decides the `HandlerErrorCode` the resource
//! operation request fails with, and the message it worded is the diagnostic. Nothing here
//! re-words it.

use ::serde_json::{Map, Value};

use super::{
    answered_error, count, flag, tag_entries, text, text_list, Progress, Refusal,
    MAX_OWNING_PAGES, TAGS, TAG_KEY, TAG_VALUE,
};

/// The `TypeName` this module stands behind.
pub const TYPE_NAME: &str = "AWS::ApiGatewayV2::Api";

/// The property that carries the type's primary identifier, which its schema's
/// `primaryIdentifier` points at (`/properties/ApiId`): the identifier API Gateway v2
/// assigns when it makes the api.
pub const IDENTIFIER_PROPERTY: &str = "ApiId";

/// The endpoint API Gateway v2 serves the api on, the type's other read-only property.
const API_ENDPOINT: &str = "ApiEndpoint";

/// The api's own name and the protocol it speaks — the two members `CreateApi` requires.
const NAME: &str = "Name";
const PROTOCOL_TYPE: &str = "ProtocolType";

/// The remaining members of the api's configuration a create and an update take.
const API_KEY_SELECTION_EXPRESSION: &str = "ApiKeySelectionExpression";
const ROUTE_SELECTION_EXPRESSION: &str = "RouteSelectionExpression";
const CORS_CONFIGURATION: &str = "CorsConfiguration";
const CREDENTIALS_ARN: &str = "CredentialsArn";
const DESCRIPTION: &str = "Description";
const DISABLE_EXECUTE_API_ENDPOINT: &str = "DisableExecuteApiEndpoint";
const DISABLE_SCHEMA_VALIDATION: &str = "DisableSchemaValidation";
const IP_ADDRESS_TYPE: &str = "IpAddressType";
const ROUTE_KEY: &str = "RouteKey";
const TARGET: &str = "Target";
const VERSION: &str = "Version";

/// The OpenAPI definition an api is IMPORTED from, inline, and how that document is read:
/// the `Body` member `ImportApi` takes, with `BasePath` and `FailOnWarnings` on that
/// operation's own query.
const BODY: &str = "Body";
const BASE_PATH: &str = "BasePath";
const FAIL_ON_WARNINGS: &str = "FailOnWarnings";

/// The same definition held as an Amazon S3 OBJECT instead. The object has to be FETCHED
/// from S3 before any api can be imported from it, which nothing here does, so a model
/// stating it is refused rather than accepted and dropped.
const BODY_S3_LOCATION: &str = "BodyS3Location";

/// The members of [`CORS_CONFIGURATION`], as the type's schema spells them.
const CORS_ALLOW_CREDENTIALS: &str = "AllowCredentials";
const CORS_ALLOW_HEADERS: &str = "AllowHeaders";
const CORS_ALLOW_METHODS: &str = "AllowMethods";
const CORS_ALLOW_ORIGINS: &str = "AllowOrigins";
const CORS_EXPOSE_HEADERS: &str = "ExposeHeaders";
const CORS_MAX_AGE: &str = "MaxAge";

/// Whether `property` is one of this type's the OWNING service COMPUTES.
///
/// The schema's `readOnlyProperties` are held in this crate's one catalogue of them
/// ([`crate::state::read_only_properties`]), so this module restates none of them; that
/// catalogue leaves the primary identifier out, because a patch that would change it is
/// refused as the create-only property it is, and it is added back here — a desired state
/// may not ask for the identifier either.
fn read_only_properties() -> Vec<String> {
    let mut computed = crate::state::read_only_properties(TYPE_NAME, IDENTIFIER_PROPERTY);
    computed.push(IDENTIFIER_PROPERTY.to_string());
    computed
}

/// The api `identifier` names in `region`, as API Gateway v2's own read reports it, or
/// `None` when that service holds no api under it.
pub async fn read(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Option<Value> {
    held(&in_process_apigatewayv2(ctx, region), identifier).await
}

/// The api `identifier` names, as the resource model this service reports — or `None` for
/// one that service does not hold.
async fn held(
    client: &::aws_sdk_apigatewayv2::Client,
    identifier: &str,
) -> Option<Value> {
    let reported = client.get_api().api_id(identifier).send().await.ok()?;
    Some(model(&Reported::of_read(&reported)))
}

/// The apis API Gateway v2 holds in `region`, as `(primary identifier, resource model)` in
/// identifier order: at most `limit` of them, and only those whose identifier sorts AFTER
/// `after`.
///
/// `GetApis` reports them in its own order and one bounded page at a time, while a resource
/// listing of this service pages in IDENTIFIER order, so the pages are walked and the
/// window kept as they go: what is held is never more than the page this listing is about
/// to report. A page that service did not answer ends the walk, so what a listing reports is
/// the window it could read of that service's own inventory.
pub async fn list(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    after: Option<&str>,
    limit: usize,
) -> Vec<(String, Value)> {
    let client = in_process_apigatewayv2(ctx, region);
    let mut window: Vec<(String, Value)> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_OWNING_PAGES {
        let Ok(page) = client.get_apis().set_next_token(next.clone()).send().await else {
            break;
        };
        for api in page.items.clone().unwrap_or_default() {
            let reported = Reported::of_summary(&api);
            let Some(identifier) = reported.api_id.clone() else {
                continue;
            };
            if after.is_none_or(|after| identifier.as_str() > after) {
                window.push((identifier, model(&reported)));
            }
        }
        window.sort_by(|(left, _), (right, _)| left.cmp(right));
        window.truncate(limit);
        next = page.next_token;
        if next.is_none() {
            break;
        }
    }
    window
}

/// Create the api the resource model `properties` describes, in `region`, and answer the
/// `ApiId` API Gateway v2 assigned it — the primary identifier the resource then answers to.
///
/// A model carrying a DEFINITION is a different call: the definition IS the api — its name,
/// its protocol, its routes and their integrations all come out of the document — so it is
/// made with `ImportApi` rather than with `CreateApi` ([`DesiredApi::imported`] is what the
/// type's own rule for such a model admits).
pub async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
) -> Result<String, Refusal> {
    let desired = DesiredApi::read(properties, None)?;
    let client = in_process_apigatewayv2(ctx, region);
    if let Some(body) = desired.imported()? {
        return client
            .import_api()
            .body(body)
            .set_basepath(desired.base_path.clone())
            .set_fail_on_warnings(desired.fail_on_warnings)
            .send()
            .await
            .map_err(refusal_of)?
            .api_id
            .ok_or_else(|| Refusal::rejected(NO_API_ID.to_string()));
    }
    // "Name" and "ProtocolType" are the two members `CreateApi` requires, so a model
    // stating neither a definition nor them describes no api that could be created.
    let (Some(name), Some(protocol_type)) = (desired.name.clone(), desired.protocol_type.clone())
    else {
        return Err(Refusal::invalid(format!(
            "{NAME} and {PROTOCOL_TYPE} are required to create a resource of type {TYPE_NAME}"
        )));
    };
    let mut request = client
        .create_api()
        .name(name)
        .protocol_type(::aws_sdk_apigatewayv2::types::ProtocolType::from(
            protocol_type.as_str(),
        ))
        .set_api_key_selection_expression(desired.api_key_selection_expression.clone())
        .set_route_selection_expression(desired.route_selection_expression.clone())
        .set_cors_configuration(cors_of(desired.cors_configuration.as_ref())?)
        .set_credentials_arn(desired.credentials_arn.clone())
        .set_description(desired.description.clone())
        .set_disable_execute_api_endpoint(desired.disable_execute_api_endpoint)
        .set_disable_schema_validation(desired.disable_schema_validation)
        .set_ip_address_type(
            desired
                .ip_address_type
                .as_deref()
                .map(::aws_sdk_apigatewayv2::types::IpAddressType::from),
        )
        .set_route_key(desired.route_key.clone())
        .set_target(desired.target.clone())
        .set_version(desired.version.clone());
    for (key, value) in &desired.tags {
        request = request.tags(key, value);
    }
    request
        .send()
        .await
        .map_err(refusal_of)?
        .api_id
        .ok_or_else(|| Refusal::rejected(NO_API_ID.to_string()))
}

/// What a create answers with when API Gateway v2 reported no identifier for the api it
/// made: nothing names the resource, so the request fails rather than reporting a success
/// with no identifier.
const NO_API_ID: &str = "API Gateway v2 reported no ApiId for the api it created";

/// Apply the resource model `properties` to the api `identifier` names in `region`.
///
/// `UpdateApi` (`PATCH /v2/apis/{ApiId}`) is the one call that changes an api that exists,
/// and a model carrying a DEFINITION is applied by reading that definition over the api
/// instead: `ReimportApi` "puts an Api resource" under the same `ApiId`, so a changed
/// definition changes THIS api rather than making a second one.
pub async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
    properties: &Value,
) -> Result<(), Refusal> {
    let client = in_process_apigatewayv2(ctx, region);
    let held = held(&client, identifier).await.ok_or(Refusal::NotFound)?;
    let desired = DesiredApi::read(properties, Some(&held))?;
    if let Some(body) = desired.imported()? {
        client
            .reimport_api()
            .api_id(identifier)
            .body(body)
            .set_basepath(desired.base_path.clone())
            .set_fail_on_warnings(desired.fail_on_warnings)
            .send()
            .await
            .map_err(refusal_of)?;
        return Ok(());
    }
    client
        .update_api()
        .api_id(identifier)
        .set_name(desired.name.clone())
        .set_api_key_selection_expression(desired.api_key_selection_expression.clone())
        .set_route_selection_expression(desired.route_selection_expression.clone())
        .set_cors_configuration(cors_of(desired.cors_configuration.as_ref())?)
        .set_credentials_arn(desired.credentials_arn.clone())
        .set_description(desired.description.clone())
        .set_disable_execute_api_endpoint(desired.disable_execute_api_endpoint)
        .set_disable_schema_validation(desired.disable_schema_validation)
        .set_ip_address_type(
            desired
                .ip_address_type
                .as_deref()
                .map(::aws_sdk_apigatewayv2::types::IpAddressType::from),
        )
        .set_route_key(desired.route_key.clone())
        .set_target(desired.target.clone())
        .set_version(desired.version.clone())
        .send()
        .await
        .map_err(refusal_of)?;
    Ok(())
}

/// Delete the api `identifier` names in `region`.
pub async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<(), Refusal> {
    in_process_apigatewayv2(ctx, region)
        .delete_api()
        .api_id(identifier)
        .send()
        .await
        .map_err(refusal_of)
        .map(|_| ())
}

/// Where API Gateway v2's own lifecycle for the api `identifier` names has got to, for a
/// request that is following it.
///
/// That service publishes no lifecycle STATUS for an api: a call it answered has already
/// carried the work out, so what confirms the operation is the api's own read — the api is
/// there for a create or an update, and gone for a delete. A read that disagrees with what
/// the operation asked for is the work still being owed, which the next call reads again.
pub async fn progress(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
    operation: &str,
) -> Progress {
    let reported = read(ctx, region, identifier).await;
    if operation == crate::state::OPERATION_DELETE {
        return match reported {
            Some(_) => Progress::Running,
            None => Progress::Settled(None),
        };
    }
    match reported {
        Some(model) => Progress::Settled(Some(model)),
        None => Progress::Failed(Refusal::NotFound),
    }
}

/// The members of an api's record this service reads, whichever call reported it: `GetApi`
/// answers one api and `GetApis` the account's apis, and both report the same api under the
/// same member names.
#[derive(Debug, Default)]
struct Reported {
    api_id: Option<String>,
    api_endpoint: Option<String>,
    api_key_selection_expression: Option<String>,
    cors_configuration: Option<::aws_sdk_apigatewayv2::types::Cors>,
    description: Option<String>,
    disable_execute_api_endpoint: Option<bool>,
    disable_schema_validation: Option<bool>,
    ip_address_type: Option<String>,
    name: Option<String>,
    protocol_type: Option<String>,
    route_selection_expression: Option<String>,
    tags: Vec<(String, String)>,
    version: Option<String>,
}

impl Reported {
    /// The api one `GetApi` answered.
    fn of_read(reported: &::aws_sdk_apigatewayv2::operation::get_api::GetApiOutput) -> Self {
        Self {
            api_id: reported.api_id.clone(),
            api_endpoint: reported.api_endpoint.clone(),
            api_key_selection_expression: reported.api_key_selection_expression.clone(),
            cors_configuration: reported.cors_configuration.clone(),
            description: reported.description.clone(),
            disable_execute_api_endpoint: reported.disable_execute_api_endpoint,
            disable_schema_validation: reported.disable_schema_validation,
            ip_address_type: reported
                .ip_address_type
                .as_ref()
                .map(|value| value.as_str().to_string()),
            name: reported.name.clone(),
            protocol_type: reported
                .protocol_type
                .as_ref()
                .map(|value| value.as_str().to_string()),
            route_selection_expression: reported.route_selection_expression.clone(),
            tags: tag_pairs(reported.tags.as_ref()),
            version: reported.version.clone(),
        }
    }

    /// One api of a `GetApis` page.
    fn of_summary(reported: &::aws_sdk_apigatewayv2::types::Api) -> Self {
        Self {
            api_id: reported.api_id.clone(),
            api_endpoint: reported.api_endpoint.clone(),
            api_key_selection_expression: reported.api_key_selection_expression.clone(),
            cors_configuration: reported.cors_configuration.clone(),
            description: reported.description.clone(),
            disable_execute_api_endpoint: reported.disable_execute_api_endpoint,
            disable_schema_validation: reported.disable_schema_validation,
            ip_address_type: reported
                .ip_address_type
                .as_ref()
                .map(|value| value.as_str().to_string()),
            name: reported.name.clone(),
            protocol_type: reported
                .protocol_type
                .as_ref()
                .map(|value| value.as_str().to_string()),
            route_selection_expression: reported.route_selection_expression.clone(),
            tags: tag_pairs(reported.tags.as_ref()),
            version: reported.version.clone(),
        }
    }
}

/// The tag set an api reports, as `(key, value)` pairs in key order.
fn tag_pairs(tags: Option<&::std::collections::HashMap<String, String>>) -> Vec<(String, String)> {
    let ordered: ::std::collections::BTreeMap<&String, &String> =
        tags.into_iter().flatten().collect();
    ordered
        .into_iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// One api as the resource model this service reports: the schema properties the read
/// ANSWERS, and only those the api HAS — the type's write-only properties are part of no
/// model this service reports, and a property the read carries nothing for is absent rather
/// than reported empty.
fn model(reported: &Reported) -> Value {
    let mut model = Map::new();
    for (property, value) in [
        (IDENTIFIER_PROPERTY, reported.api_id.clone()),
        (API_ENDPOINT, reported.api_endpoint.clone()),
        (NAME, reported.name.clone()),
        (PROTOCOL_TYPE, reported.protocol_type.clone()),
        (
            API_KEY_SELECTION_EXPRESSION,
            reported.api_key_selection_expression.clone(),
        ),
        (
            ROUTE_SELECTION_EXPRESSION,
            reported.route_selection_expression.clone(),
        ),
        (DESCRIPTION, reported.description.clone()),
        (IP_ADDRESS_TYPE, reported.ip_address_type.clone()),
        (VERSION, reported.version.clone()),
    ] {
        if let Some(value) = value {
            model.insert(property.to_string(), Value::from(value));
        }
    }
    for (property, value) in [
        (
            DISABLE_EXECUTE_API_ENDPOINT,
            reported.disable_execute_api_endpoint,
        ),
        (DISABLE_SCHEMA_VALIDATION, reported.disable_schema_validation),
    ] {
        if let Some(value) = value {
            model.insert(property.to_string(), Value::from(value));
        }
    }
    if let Some(cors) = &reported.cors_configuration {
        model.insert(CORS_CONFIGURATION.to_string(), cors_model(cors));
    }
    if !reported.tags.is_empty() {
        model.insert(
            TAGS.to_string(),
            Value::Array(
                reported
                    .tags
                    .iter()
                    .map(|(key, value)| {
                        Value::Object(Map::from_iter([
                            (TAG_KEY.to_string(), Value::from(key.clone())),
                            (TAG_VALUE.to_string(), Value::from(value.clone())),
                        ]))
                    })
                    .collect(),
            ),
        );
    }
    Value::Object(model)
}

/// The api's CORS configuration as the schema's `CorsConfiguration` carries it: the members
/// the api HOLDS, each under the schema's own spelling.
fn cors_model(cors: &::aws_sdk_apigatewayv2::types::Cors) -> Value {
    let mut rendered = Map::new();
    if let Some(allowed) = cors.allow_credentials {
        rendered.insert(CORS_ALLOW_CREDENTIALS.to_string(), Value::from(allowed));
    }
    for (member, values) in [
        (CORS_ALLOW_HEADERS, cors.allow_headers.as_ref()),
        (CORS_ALLOW_METHODS, cors.allow_methods.as_ref()),
        (CORS_ALLOW_ORIGINS, cors.allow_origins.as_ref()),
        (CORS_EXPOSE_HEADERS, cors.expose_headers.as_ref()),
    ] {
        if let Some(values) = values {
            rendered.insert(
                member.to_string(),
                Value::Array(values.iter().map(|value| Value::from(value.clone())).collect()),
            );
        }
    }
    if let Some(max_age) = cors.max_age {
        rendered.insert(CORS_MAX_AGE.to_string(), Value::from(max_age));
    }
    Value::Object(rendered)
}

/// What a resource model asks an api to be: the members API Gateway v2's own calls take,
/// each read off the model's properties.
#[derive(Debug, Default)]
struct DesiredApi {
    name: Option<String>,
    protocol_type: Option<String>,
    api_key_selection_expression: Option<String>,
    route_selection_expression: Option<String>,
    cors_configuration: Option<Value>,
    credentials_arn: Option<String>,
    description: Option<String>,
    disable_execute_api_endpoint: Option<bool>,
    disable_schema_validation: Option<bool>,
    ip_address_type: Option<String>,
    route_key: Option<String>,
    target: Option<String>,
    version: Option<String>,
    tags: Vec<(String, String)>,
    body: Option<Value>,
    body_s3_location: Option<Value>,
    base_path: Option<String>,
    fail_on_warnings: Option<bool>,
}

impl DesiredApi {
    /// Read `properties` as the model of an api — or state why API Gateway v2 would not take
    /// it.
    ///
    /// `held` is the model the api ALREADY reports, for a model applied to an api that
    /// exists, and `None` for one that describes an api to create. The type's read-only
    /// properties are the owning service's to compute: a model may carry one — a read
    /// reports it, so a model a client round-trips carries it — but only with the value the
    /// api already holds, and a create, where there is no api to hold one, may carry none at
    /// all.
    fn read(properties: &Value, held: Option<&Value>) -> Result<Self, Refusal> {
        let Some(model) = properties.as_object() else {
            return Err(Refusal::invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self::default();
        let computed = read_only_properties();
        for (property, value) in model {
            if computed.contains(property) {
                if held.and_then(|held| held.get(property)) != Some(value) {
                    return Err(Refusal::invalid(format!(
                        "{property} is a read-only property of {TYPE_NAME}"
                    )));
                }
                continue;
            }
            match property.as_str() {
                NAME => desired.name = Some(text(property, value)?),
                PROTOCOL_TYPE => desired.protocol_type = Some(text(property, value)?),
                API_KEY_SELECTION_EXPRESSION => {
                    desired.api_key_selection_expression = Some(text(property, value)?);
                }
                ROUTE_SELECTION_EXPRESSION => {
                    desired.route_selection_expression = Some(text(property, value)?);
                }
                CORS_CONFIGURATION => desired.cors_configuration = Some(value.clone()),
                CREDENTIALS_ARN => desired.credentials_arn = Some(text(property, value)?),
                DESCRIPTION => desired.description = Some(text(property, value)?),
                DISABLE_EXECUTE_API_ENDPOINT => {
                    desired.disable_execute_api_endpoint = Some(flag(property, value)?);
                }
                DISABLE_SCHEMA_VALIDATION => {
                    desired.disable_schema_validation = Some(flag(property, value)?);
                }
                IP_ADDRESS_TYPE => desired.ip_address_type = Some(text(property, value)?),
                ROUTE_KEY => desired.route_key = Some(text(property, value)?),
                TARGET => desired.target = Some(text(property, value)?),
                VERSION => desired.version = Some(text(property, value)?),
                TAGS => desired.tags = tag_entries(value)?,
                BODY => desired.body = Some(value.clone()),
                BODY_S3_LOCATION => desired.body_s3_location = Some(value.clone()),
                BASE_PATH => desired.base_path = Some(text(property, value)?),
                FAIL_ON_WARNINGS => desired.fail_on_warnings = Some(flag(property, value)?),
                other => {
                    return Err(Refusal::invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        // An api's PROTOCOL is settled when API Gateway v2 makes it — no member of
        // `UpdateApi` carries it — so a model applied to an api that exists may name it only
        // as the protocol that api already speaks.
        if let Some(held) = held {
            let stated = model.get(PROTOCOL_TYPE);
            if stated.is_some() && stated != held.get(PROTOCOL_TYPE) {
                return Err(Refusal::invalid(format!(
                    "{PROTOCOL_TYPE} of a {TYPE_NAME} that exists is what a read of it reports"
                )));
            }
            // The api's TAGS are not settled that way: that service CHANGES them, through
            // tagging calls of its own that no patch of an api makes — so a model that asks
            // for a different tag set asks for work no handler here performs, which is
            // refused as that rather than as a property that cannot change.
            let stated = model.get(TAGS);
            if stated.is_some() && stated != held.get(TAGS) {
                return Err(Refusal::invalid(format!(
                    "{TAGS} is a property of {TYPE_NAME} this emulator's handler does not \
                     apply to an api that exists"
                )));
            }
        }
        Ok(desired)
    }

    /// The DEFINITION this model asks the api to be imported from, as the text the call that
    /// reads it takes — and `None` for a model that carries none.
    ///
    /// "To import an HTTP API, you must specify a Body or BodyS3Location", and this type's
    /// own rule is that a model carrying a definition states none of the members only
    /// `CreateApi` and `UpdateApi` take ("If you specify a Body or BodyS3Location, don't
    /// specify CorsConfiguration, CredentialsArn, DisableExecuteApiEndpoint, Name,
    /// ProtocolType, RouteKey, RouteSelectionExpression, Tags, Target, or Version"), so one
    /// that does is REFUSED rather than imported with that member silently dropped. A
    /// definition held in Amazon S3 is refused for a reason of its own: the object has to be
    /// fetched before anything can be imported from it, and nothing here fetches it.
    fn imported(&self) -> Result<Option<String>, Refusal> {
        if self.body_s3_location.is_some() {
            return Err(Refusal::invalid(format!(
                "{BODY_S3_LOCATION} is a property of {TYPE_NAME} this emulator's handler does \
                 not apply"
            )));
        }
        let Some(body) = &self.body else {
            return Ok(None);
        };
        for (property, stated) in [
            (NAME, self.name.is_some()),
            (PROTOCOL_TYPE, self.protocol_type.is_some()),
            (CORS_CONFIGURATION, self.cors_configuration.is_some()),
            (CREDENTIALS_ARN, self.credentials_arn.is_some()),
            (
                DISABLE_EXECUTE_API_ENDPOINT,
                self.disable_execute_api_endpoint.is_some(),
            ),
            (ROUTE_KEY, self.route_key.is_some()),
            (
                ROUTE_SELECTION_EXPRESSION,
                self.route_selection_expression.is_some(),
            ),
            (TARGET, self.target.is_some()),
            (VERSION, self.version.is_some()),
            (TAGS, !self.tags.is_empty()),
        ] {
            if stated {
                return Err(Refusal::invalid(format!(
                    "{TYPE_NAME} states a {BODY}, so the api is imported from that definition, \
                     and {property} is no member of that import"
                )));
            }
        }
        Ok(Some(definition_text(body)))
    }
}

/// The definition a model carries as the TEXT the call that reads it takes: an inline
/// document is declared as the JSON object it is, and a model that states it as text already
/// carries that text.
fn definition_text(body: &Value) -> String {
    match body {
        Value::String(text) => text.clone(),
        document => document.to_string(),
    }
}

/// The CORS configuration a model states, as the `Cors` structure API Gateway v2's calls
/// take — or why it is not one.
fn cors_of(
    stated: Option<&Value>,
) -> Result<Option<::aws_sdk_apigatewayv2::types::Cors>, Refusal> {
    let Some(stated) = stated else {
        return Ok(None);
    };
    let Some(members) = stated.as_object() else {
        return Err(Refusal::invalid(format!(
            "{CORS_CONFIGURATION} must be an object of its members"
        )));
    };
    let mut cors = ::aws_sdk_apigatewayv2::types::Cors::builder();
    for (member, value) in members {
        match member.as_str() {
            CORS_ALLOW_CREDENTIALS => cors = cors.allow_credentials(flag(member, value)?),
            CORS_ALLOW_HEADERS => cors = cors.set_allow_headers(Some(text_list(member, value)?)),
            CORS_ALLOW_METHODS => cors = cors.set_allow_methods(Some(text_list(member, value)?)),
            CORS_ALLOW_ORIGINS => cors = cors.set_allow_origins(Some(text_list(member, value)?)),
            CORS_EXPOSE_HEADERS => {
                cors = cors.set_expose_headers(Some(text_list(member, value)?));
            }
            CORS_MAX_AGE => cors = cors.max_age(count(member, value)?),
            other => {
                return Err(Refusal::invalid(format!(
                    "{other} is not a member of {CORS_CONFIGURATION}"
                )));
            }
        }
    }
    Ok(Some(cors.build()))
}

/// A call API Gateway v2 refused, as the refusal this service reports: the error code that
/// service NAMED decides which handler verdict the resource operation request fails with,
/// and the message it worded is the diagnostic, passed on rather than re-worded.
fn refusal_of<E>(
    error: ::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> Refusal
where
    ::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >: ::aws_smithy_types::error::metadata::ProvideErrorMetadata + ::std::fmt::Display,
{
    let (code, detail) = answered_error(error);
    match code.as_str() {
        // The modelled 404 of every read, update and delete of an api: that service holds no
        // such api.
        "NotFoundException" => Refusal::NotFound,
        // "The requested operation would cause a conflict with the current state of a
        // service resource", which for a create is the resource already being there.
        "ConflictException" => Refusal::already_exists(detail),
        // "The client is sending more than the allowed number of requests per unit of time
        // or the number of resources exceeds the limit."
        "TooManyRequestsException" => Refusal::at_capacity(detail),
        // That service's own answer to a request it will not take at all, which for this
        // type is a fault in the resource model the request carried.
        "BadRequestException" => Refusal::invalid(detail),
        _ => Refusal::rejected(detail),
    }
}

/// An API Gateway v2 client whose transport is the framework's IN-PROCESS callback into this
/// same emulator — the one way this crate reaches the service that owns this type.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// dispatches to the service mounted beside this one INSIDE this process. No socket is
/// opened, no host name is resolved, nothing outside this process is reachable through it,
/// and nothing a request carries can steer it elsewhere — the endpoint URL exists because
/// the SDK requires one, and the only part of it a caller contributes is the Region token
/// its own credential scope named, which `crate::state::request_region` has already narrowed
/// to the characters a Region name is made of. The credentials are the emulator's own; the
/// caller's `Authorization` header is never forwarded.
fn in_process_apigatewayv2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_apigatewayv2::Client {
    let config = ::aws_sdk_apigatewayv2::Config::builder()
        .behavior_version(::aws_sdk_apigatewayv2::config::BehaviorVersion::latest())
        .region(::aws_sdk_apigatewayv2::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://apigateway.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        // The transport is in-process: nothing to retry, time out or watch for a stalled
        // stream, and each would need a sleep implementation this client is not given.
        .retry_config(::aws_sdk_apigatewayv2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_apigatewayv2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_apigatewayv2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_apigatewayv2::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_apigatewayv2::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type's read-only properties are the ones no desired state may ask for, and the
    /// primary identifier is one of them: a create that names any of them describes an api
    /// API Gateway v2 would not make.
    #[test]
    fn a_create_may_not_name_a_read_only_property() {
        let computed = read_only_properties();
        assert!(
            computed.contains(&IDENTIFIER_PROPERTY.to_string()),
            "expected the type's primary identifier to be one the owning service computes, \
             got {computed:?}"
        );
        for property in &computed {
            let model = ::serde_json::json!({
                NAME: "an-api", PROTOCOL_TYPE: "HTTP", property.as_str(): "anything",
            });
            let refusal = DesiredApi::read(&model, None).err();
            assert_eq!(
                refusal,
                Some(Refusal::invalid(format!(
                    "{property} is a read-only property of {TYPE_NAME}"
                ))),
                "property {property:?}: expected a create naming it to be refused, got \
                 {refusal:?}"
            );
        }
    }

    /// ...and a model a client round-trips carries them with the values the api reports,
    /// which asks for nothing: such a model is taken, and what it asks of API Gateway v2 is
    /// the writable members it carries.
    #[test]
    fn an_update_takes_the_read_only_values_the_read_reported() {
        let held = ::serde_json::json!({
            "ApiId": "a1b2c3d4e5",
            "ApiEndpoint": "https://a1b2c3d4e5.execute-api.us-east-1.amazonaws.com",
            "Name": "an-api",
            "ProtocolType": "HTTP",
        });
        let mut desired = held.clone();
        desired["Name"] = ::serde_json::json!("a-renamed-api");
        let read = DesiredApi::read(&desired, Some(&held))
            .expect("a model carrying the values the read reported");
        assert_eq!(
            read.name.as_deref(),
            Some("a-renamed-api"),
            "expected the patched name to be what the update asks for, got {read:?}"
        );
    }

    /// A property the type's schema does not declare asks for something no handler applies,
    /// which is refused rather than accepted and dropped.
    #[test]
    fn a_property_the_type_does_not_declare_is_refused() {
        let model = ::serde_json::json!({ "Name": "an-api", "ThisPropertyDoesNotExist": 1 });
        let refusal = DesiredApi::read(&model, None).err();
        assert_eq!(
            refusal,
            Some(Refusal::invalid(format!(
                "ThisPropertyDoesNotExist is not a property of {TYPE_NAME}"
            ))),
            "expected an undeclared property to be refused, got {refusal:?}"
        );
    }

    /// A model carrying a DEFINITION is imported, and the import takes none of the members
    /// only the create takes — so a model stating one of them beside it is refused.
    #[test]
    fn a_definition_is_imported_and_admits_no_create_member_beside_it() {
        let document = ::serde_json::json!({ "openapi": "3.0.1" });
        let imported = DesiredApi::read(&::serde_json::json!({ BODY: document.clone() }), None)
            .expect("a model carrying only a definition")
            .imported()
            .expect("a definition the import reads");
        assert_eq!(
            imported.as_deref(),
            Some(document.to_string().as_str()),
            "expected the definition to travel as the text the import reads, got {imported:?}"
        );
        let beside = DesiredApi::read(
            &::serde_json::json!({ BODY: document, NAME: "an-api" }),
            None,
        )
        .expect("a model this reader takes")
        .imported()
        .err();
        assert_eq!(
            beside,
            Some(Refusal::invalid(format!(
                "{TYPE_NAME} states a {BODY}, so the api is imported from that definition, and \
                 {NAME} is no member of that import"
            ))),
            "expected a create member beside the definition to be refused, got {beside:?}"
        );
    }
}
