// SPDX-License-Identifier: CC0-1.0

//! `AWS::DynamoDB::Table`: the table a stack declares, created in Amazon DynamoDB itself.
//!
//! This is the per-type half of provisioning for one resource type, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::lambda_function`] for the reason that module records —
//! that file is far past the length a change to this repository may touch — and is reached
//! through the same seam ([`super::Owner`]).
//!
//! ## Why the table is really created
//!
//! A stack's output is the hand-off between a deployment and the code that consumes it, and
//! what it hands over are the resource's own identifiers: "Ref returns the table name", and
//! `Fn::GetAtt` reads the table's `Arn` and its `StreamArn`. The stream ARN is the one
//! Amazon DynamoDB MINTED when the table's `StreamSpecification` enabled a stream — no
//! naming rule computes it, and it is the handle every DynamoDB Streams consumer
//! (`DescribeStream`, `GetShardIterator`, `GetRecords`) is called with — so the only way an
//! output over it can carry a value a client can use is for the table to exist in DynamoDB
//! and for its own answer to be what the attribute reports. That is what this arm does: the
//! create is `CreateTable`, and the attributes published for the resource are read off the
//! `TableDescription` it answered.
//!
//! ## The properties
//!
//! The type's properties are `CreateTable`'s own members, named identically on both sides,
//! with four exceptions:
//!
//! * `TableName` ADDRESSES the call rather than travelling beside the others: "Ref returns
//!   the table name", and that name is CloudFormation's to decide
//!   ([`super::super::template`]), so the create carries the resource's physical id as
//!   `TableName` and the table a client then calls `DescribeTable` with is the one the stack
//!   publishes;
//! * `SSESpecification` spells its flag `SSEEnabled`, while the API's own member is
//!   `Enabled` ([`sse_specification`]);
//! * `ResourcePolicy` is an object carrying a `PolicyDocument` in a template, while the
//!   API's `ResourcePolicy` is that document as TEXT ([`resource_policy`]);
//! * `StreamSpecification` states what the stream CAPTURES and nothing else — declaring the
//!   property is what asks for a stream — while the API's own specification carries a
//!   REQUIRED `StreamEnabled` beside the view type ([`stream_specification`]).
//!
//! A property whose shape or spelling differs that way is TRANSLATED into the API's member
//! rather than passed through: a member a template never states is a member the owning
//! service refuses the call for, which fails a resource whose declaration is valid
//! CloudFormation. The translation serves BOTH calls: `UpdateTable` takes
//! `SSESpecification` and `StreamSpecification` as members of its own, so a deployment that
//! changes one of them is applied to the table that already exists rather than reported as a
//! change with no in-place path ([`update_request`], [`stream_steps`]).
//!
//! Every other property of the type is a DIFFERENT DynamoDB call made on the table after it
//! exists (`UpdateTimeToLive`, `UpdateContinuousBackups`, `UpdateContributorInsights`, the
//! Kinesis destination calls) or an import rather than a create
//! (`ImportSourceSpecification`). The calls are MADE, by [`settings`], on the table this
//! arm has just created, and the resource is reported created only once DynamoDB's own read
//! reports each of those settings in force ([`readiness`]) — a table configured with less
//! than its declaration asked for is the shallow success this arm exists to avoid, and a
//! declaration of one of them that this service refuses fails the resource rather than
//! leaving the property dropped. `ImportSourceSpecification` is not a property applied to a
//! created table at all but a different create, so a declaration carrying THAT is still
//! refused ([`refuse_unapplied_properties`]).
//!
//! `CreateTable` answers a table whose `TableStatus` is `CREATING` — "You can perform read
//! and write operations only on an ACTIVE table" — so the resource's creation IS held open on
//! that window ([`readiness`]): a table in it takes none of the calls the stack published it
//! for, and a client acting on the stack's output the instant the stack completes is answered
//! as if the table were not there. What ALSO holds the resource open is a declared SETTING
//! that service has not put into force yet: the declaration is not applied while it is still
//! being applied, so a client that reads the setting back the instant the stack completes is
//! answered what the template declared.
//!
//! ## What a drift check reads back
//!
//! The same reads answer what the table IS NOW ([`actual_configuration`]): `DescribeTable`
//! for the table itself and the owning read of each declared setting, each reported under the
//! property the TYPE declares it as. So a drift check tells a table whose properties were
//! changed out of band (reported MODIFIED, with the differing properties) from one that is no
//! longer there (reported DELETED, which only that service answering it holds no such table
//! establishes).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    call_json_rpc, key_value_tag_list, Created, Readiness, Refusal, Removal, AWS_JSON_1_0,
};
use crate::state::provisioner::tags_of;
use crate::state::StoredResource;

/// The properties DynamoDB applies through a call of its own on the table that already
/// exists, and reports through a read of its own.
mod settings;

/// The resource type this module provisions.
pub(crate) const DYNAMODB_TABLE: &str = "AWS::DynamoDB::Table";

/// The SigV4 signing name of the service that owns it, which is what routes the in-process
/// call to Amazon DynamoDB's own handlers (`awsService.endpointPrefix`/`sigv4Name`).
const DYNAMODB_SIGNING_NAME: &str = "dynamodb";

/// The service shape an awsJson `X-Amz-Target` names the operation under
/// (`com.amazonaws.dynamodb#DynamoDB_20120810`).
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The error code DynamoDB answers for a table it does not hold
/// (`ResourceNotFoundException`), which is what says a delete has nothing left to remove and
/// a read has no table to report.
const TABLE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The property that names the table, which addresses every call this arm makes.
const TABLE_NAME_PROPERTY: &str = "TableName";

/// The property carrying the table's own tag set, merged with the ones CloudFormation
/// propagates ([`tags_of`]).
const TAGS_PROPERTY: &str = "Tags";

/// The property carrying a resource-based policy, whose spelling differs between the
/// template and the API ([`resource_policy`]).
const RESOURCE_POLICY_PROPERTY: &str = "ResourcePolicy";

/// The property carrying the server-side-encryption settings, whose flag is spelled
/// differently on the two sides ([`sse_specification`]).
const SSE_SPECIFICATION_PROPERTY: &str = "SSESpecification";

/// The flag `AWS::DynamoDB::Table`'s `SSESpecification` spells, and the one the API's own
/// `SSESpecification` spells for the same thing.
const SSE_ENABLED_PROPERTY: &str = "SSEEnabled";
const SSE_ENABLED_MEMBER: &str = "Enabled";

/// The member a template's `ResourcePolicy` carries the policy document under.
const POLICY_DOCUMENT_PROPERTY: &str = "PolicyDocument";

/// The tag-key namespace DynamoDB reserves, compared in lower case: a tag write carrying
/// one is refused outright, whoever makes it ([`storable_tags`]).
const RESERVED_TAG_PREFIX: &str = "aws:";

/// The properties that ARE `CreateTable`'s own body members under the same name, so the
/// declared value travels into the request as written.
///
/// Source: `CreateTable`'s member list
/// (`smithy_query(query="operation",operation="CreateTable")`), intersected with the
/// properties `AWS::DynamoDB::Table` publishes.
const CREATE_MEMBERS: &[&str] = &[
    "AttributeDefinitions",
    BILLING_MODE_PROPERTY,
    "DeletionProtectionEnabled",
    "GlobalSecondaryIndexes",
    "KeySchema",
    "LocalSecondaryIndexes",
    ON_DEMAND_THROUGHPUT_PROPERTY,
    PROVISIONED_THROUGHPUT_PROPERTY,
    "TableClass",
    "WarmThroughput",
];

/// The properties a declaration states its keys and the attributes behind them in: the
/// table's own key schema, the definitions every key attribute has to be declared in, and the
/// two index lists, each of whose entries carries a key schema of its own.
///
/// Source: `CreateTable`'s own members
/// (`smithy_query(query="operation",operation="CreateTable")`), which the type publishes under
/// the same names ([`CREATE_MEMBERS`]).
const KEY_SCHEMA_PROPERTY: &str = "KeySchema";
const ATTRIBUTE_DEFINITIONS_PROPERTY: &str = "AttributeDefinitions";
const INDEX_PROPERTIES: &[&str] = &["GlobalSecondaryIndexes", "LocalSecondaryIndexes"];

/// The member each element of either list names one attribute in (`KeySchemaElement$AttributeName`,
/// `AttributeDefinition$AttributeName`).
const ATTRIBUTE_NAME_MEMBER: &str = "AttributeName";

/// The property carrying the table's DynamoDB Streams settings, whose SHAPE differs between
/// the template and the API ([`stream_specification`]).
const STREAM_SPECIFICATION_PROPERTY: &str = "StreamSpecification";

/// The member the API's own `StreamSpecification` states the stream's existence in, which is
/// REQUIRED there (`smithy_query(query="shape",shape="StreamSpecification")`), and the member
/// stating what the stream captures, which both sides spell the same way.
const STREAM_ENABLED_MEMBER: &str = "StreamEnabled";
const STREAM_VIEW_TYPE_MEMBER: &str = "StreamViewType";

/// The members of a declared `StreamSpecification` this arm applies: what the stream
/// captures, and the API's own flag, which a declaration stating it keeps.
///
/// The type's own specification also carries a `ResourcePolicy` over the STREAM, which this
/// arm puts into force nowhere, so a declaration carrying it is refused rather than dropped
/// ([`refuse_unapplied_properties`]).
const STREAM_SPECIFICATION_MEMBERS: &[&str] = &[STREAM_ENABLED_MEMBER, STREAM_VIEW_TYPE_MEMBER];

/// The members `UpdateTable` takes that a declaration can change on a table that already
/// exists, under the same name on both sides.
///
/// Source: `UpdateTable`'s member list
/// (`smithy_query(query="operation",operation="UpdateTable")`). A property outside this set
/// has no in-place path — `KeySchema` and the index lists are not members of `UpdateTable`
/// at all — so a deployment that changes one is reported as not applied rather than as a
/// success that dropped the caller's change ([`update`]).
const UPDATE_MEMBERS: &[&str] = &[
    BILLING_MODE_PROPERTY,
    "DeletionProtectionEnabled",
    ON_DEMAND_THROUGHPUT_PROPERTY,
    PROVISIONED_THROUGHPUT_PROPERTY,
    "TableClass",
    "WarmThroughput",
];

/// The property stating the table's read/write capacity mode, and the two modes it names.
///
/// A declaration that states NO mode leaves the table in the provisioned one — "if you set
/// BillingMode as `PROVISIONED`, you must specify this property. If you set BillingMode as
/// `PAY_PER_REQUEST`, you cannot specify this property"
/// (`smithy_query(query="doc",shape="CreateTableInput",member="ProvisionedThroughput")`),
/// which is the mode the owning service reads a declaration carrying a provisioned capacity
/// and no mode in — so that is the mode a declaration RESOLVES to when it states none
/// ([`capacity_mode`]).
const BILLING_MODE_PROPERTY: &str = "BillingMode";
const PROVISIONED_CAPACITY_MODE: &str = "PROVISIONED";
const ON_DEMAND_CAPACITY_MODE: &str = "PAY_PER_REQUEST";

/// The two properties stating a capacity figure, each belonging to ONE capacity mode: the
/// provisioned pair a `PROVISIONED` table is charged for, and the ceiling an on-demand table
/// has ("sets the maximum number of read and write units for the specified on-demand table"
/// — `smithy_query(query="doc",shape="OnDemandThroughput")`).
///
/// Which mode each belongs to is what [`applies_in_capacity_mode`] answers.
const PROVISIONED_THROUGHPUT_PROPERTY: &str = "ProvisionedThroughput";
const ON_DEMAND_THROUGHPUT_PROPERTY: &str = "OnDemandThroughput";

/// The member `CreateTable` and `UpdateTable` answer the table's description under.
const TABLE_DESCRIPTION_MEMBER: &str = "TableDescription";

/// The member a read answers the table's description under (`DescribeTable$Table`).
const TABLE_MEMBER: &str = "Table";

/// The `TableDescription` members the table's published `Fn::GetAtt` attributes are read
/// off, each paired with the attribute name the type publishes it as.
///
/// Source: the type's own "Return values" section — `Fn::GetAtt` reads `Arn` and
/// `StreamArn` off an `AWS::DynamoDB::Table` — and `TableDescription`'s own members
/// (`smithy_query(query="shape",shape="TableDescription")`), which is where each value
/// comes from. `StreamArn` is reported only for a table that HAS a stream: a table whose
/// declaration enabled none has no stream ARN, and an attribute nothing minted is one a
/// client must never be handed.
const PUBLISHED_ATTRIBUTES: &[(&str, &str)] = &[
    ("Arn", "TableArn"),
    ("StreamArn", "LatestStreamArn"),
];

/// The member a description reports the table's lifecycle state in, read by
/// [`await_settled`] and [`creation_window`].
const TABLE_STATUS_MEMBER: &str = "TableStatus";

/// The attribute the type publishes the table's ARN as, and the `TableDescription` member it
/// is read off — the pair [`PUBLISHED_ATTRIBUTES`] states, named here because DynamoDB's
/// TAGGING calls address a resource by that ARN ([`retag`]).
const ARN_ATTRIBUTE: &str = "Arn";
const TABLE_ARN_MEMBER: &str = "TableArn";

/// The members DynamoDB's tagging calls take: the ARN that addresses the resource
/// (`TagResource`/`UntagResource`/`ListTagsOfResource`'s `ResourceArn`), the keys a removal
/// states (`UntagResource$TagKeys`) and the cursor a listing hands out
/// (`ListTagsOfResource$NextToken`); the tags themselves travel under the same `Tags` the type
/// declares them as ([`TAGS_PROPERTY`]).
///
/// Source: those operations' member lists
/// (`smithy_query(query="operation",operation="TagResource,UntagResource,ListTagsOfResource")`).
const RESOURCE_ARN_MEMBER: &str = "ResourceArn";
const TAG_KEYS_MEMBER: &str = "TagKeys";
const NEXT_TOKEN_MEMBER: &str = "NextToken";

/// The members one reported tag is made of (`Tag$Key`, `Tag$Value`).
const TAG_KEY_MEMBER: &str = "Key";
const TAG_VALUE_MEMBER: &str = "Value";

/// How many pages of a table's tags one reconciliation reads: `ListTagsOfResource` is
/// paginated, and a table carries at most a bounded tag set, so the whole set is reached in a
/// bounded number of pages and a cursor that never ends cannot hold a deployment open.
const MAX_TAG_PAGES: usize = 16;

/// Create the table an `AWS::DynamoDB::Table` declares, in Amazon DynamoDB itself — so a
/// client that deployed the stack reads it back with `dynamodb describe-table`, writes items
/// to it, and reaches the stream its `StreamSpecification` enabled through the DynamoDB
/// Streams endpoint.
///
/// `Err` is DynamoDB's own refusal of the create, which is what the resource fails with: a
/// declaration that names no key schema, a key schema whose attributes are not defined, a
/// name already taken, a member this emulator does not store yet.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type)?;
    // ...and the one rule the handler holds the declaration's own two key properties to,
    // before the owning service is called at all ([`refuse_undefined_key_attributes`]).
    refuse_undefined_key_attributes(&resource.properties)?;
    let mut request = Map::new();
    // The table is created under the identifier CloudFormation decided for the resource,
    // which is what `Ref` resolves to and what a client calls DynamoDB with.
    request.insert(
        TABLE_NAME_PROPERTY.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    for member in CREATE_MEMBERS {
        if let Some(value) = declared_in(&resource.properties, member) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    if let Some(specification) = stream_specification(&resource.properties) {
        request.insert(STREAM_SPECIFICATION_PROPERTY.to_string(), specification);
    }
    if let Some(specification) = sse_specification(&resource.properties) {
        request.insert(SSE_SPECIFICATION_PROPERTY.to_string(), specification);
    }
    if let Some(document) = resource_policy(&resource.properties) {
        request.insert(RESOURCE_POLICY_PROPERTY.to_string(), Value::String(document));
    }
    let tags = storable_tags(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_PROPERTY.to_string(), tag_list(&tags));
    }
    // Nothing is in force on a table that does not exist yet, so every declared setting is
    // applied from off — which is the state `CreateTable` leaves each of them in, since that
    // call carries none of them. No such change has to be reported as having no in-place
    // path, so the answer here is the calls themselves ([`settings::calls`]).
    let setting_calls = settings::calls(None, &resource.properties).unwrap_or_default();
    let answered = call_json_rpc(
        ctx,
        DYNAMODB_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{DYNAMODB_TARGET_PREFIX}.CreateTable"),
        &Value::Object(request),
    )
    .await?;
    if !setting_calls.is_empty() {
        // Each of these calls changes the CONFIGURATION of the table, which DynamoDB refuses
        // while the table is still being created, so they are made once that service reports
        // it settled ([`await_settled`]) — exactly as a change to the table itself is.
        await_settled(ctx, region, &resource.physical_resource_id).await;
        if let Err(refusal) =
            settings::apply(ctx, region, &resource.physical_resource_id, &setting_calls).await
        {
            // A table configured with less than its declaration asked for is not a resource
            // the stack may report as created, so the table is removed again before the
            // refusal is reported — exactly as a function whose own configuration calls were
            // refused is ([`super::lambda_function::create`]).
            let _ = delete_table(ctx, region, &resource.physical_resource_id).await;
            return Err(refusal);
        }
    }
    Ok(Created::attributes(published_attributes(
        answered.get(TABLE_DESCRIPTION_MEMBER),
    )))
}

/// Bring the table's tag set back into step with the one CloudFormation gives it, through
/// DynamoDB's own `TagResource` and `UntagResource`.
///
/// `CreateTable`'s `Tags` member applies to the table that call creates and to no table that
/// already exists — and a table the stack ADOPTED was created by no call of this service's at
/// all — so this is where the set a table is to end up carrying is reconciled: a stack
/// operation's `Tags` are propagated to the resources in the stack, and a deployment can state
/// them differently with no declaration changing.
///
/// Idempotent: a table whose tags already agree with the set has nothing called about it, and
/// only the keys that really differ travel. The keys in the namespace DynamoDB reserves are
/// not a caller's to write ([`storable_tags`]), so CloudFormation's own records of the stack
/// reach neither call.
pub(super) async fn retag(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    // DynamoDB's tagging calls address a resource by ARN: the one the record publishes, or
    // the one that service reports for the table right now — never a value this builds, so a
    // resource whose attributes were not re-pointed still has its tags reconciled rather than
    // silently left as they were. A table that service holds none of has no tags to reconcile,
    // which the deployment's own reads are what report.
    let arn = match resource.attributes.get(ARN_ATTRIBUTE) {
        Some(arn) if !arn.is_empty() => arn.clone(),
        _ => {
            let reported = describe(ctx, region, &resource.physical_resource_id).await?;
            match reported
                .as_ref()
                .and_then(|description| description.get(TABLE_ARN_MEMBER))
                .and_then(Value::as_str)
            {
                Some(arn) => arn.to_string(),
                None => return Ok(()),
            }
        }
    };
    let wanted = storable_tags(declared, propagated);
    let held = held_tags(ctx, region, &arn).await?;
    // A key the table carries that the set no longer states is removed; the two calls are
    // made in that order, so a set stating fewer tags than the table has leaves exactly the
    // stated ones. A key in the namespace DynamoDB reserves is in neither call: that service
    // refuses the WHOLE request on one, so a reserved key the table carries from elsewhere
    // would turn a tag reconciliation into a failed resource rather than being taken off.
    let removed: Vec<Value> = held
        .iter()
        .filter(|(key, _)| !key.to_ascii_lowercase().starts_with(RESERVED_TAG_PREFIX))
        .filter(|(key, _)| !wanted.iter().any(|(stated, _)| stated == key))
        .map(|(key, _)| Value::String(key.clone()))
        .collect();
    if !removed.is_empty() {
        let mut request = Map::new();
        request.insert(RESOURCE_ARN_MEMBER.to_string(), Value::String(arn.clone()));
        request.insert(TAG_KEYS_MEMBER.to_string(), Value::Array(removed));
        call_json_rpc(
            ctx,
            DYNAMODB_SIGNING_NAME,
            region,
            AWS_JSON_1_0,
            &format!("{DYNAMODB_TARGET_PREFIX}.UntagResource"),
            &Value::Object(request),
        )
        .await?;
    }
    let added: Vec<(String, String)> = wanted
        .into_iter()
        .filter(|(key, value)| {
            !held
                .iter()
                .any(|(held_key, held_value)| held_key == key && held_value == value)
        })
        .collect();
    if !added.is_empty() {
        let mut request = Map::new();
        request.insert(RESOURCE_ARN_MEMBER.to_string(), Value::String(arn));
        request.insert(TAGS_PROPERTY.to_string(), tag_list(&added));
        call_json_rpc(
            ctx,
            DYNAMODB_SIGNING_NAME,
            region,
            AWS_JSON_1_0,
            &format!("{DYNAMODB_TARGET_PREFIX}.TagResource"),
            &Value::Object(request),
        )
        .await?;
    }
    Ok(())
}

/// The tags DynamoDB holds for the resource `arn`, as the `(key, value)` pairs its own
/// `ListTagsOfResource` reports them in.
///
/// Every page is read ([`MAX_TAG_PAGES`]): the reconciliation above is about the WHOLE set the
/// resource carries, so a tag on a later page is one a removal would otherwise miss.
async fn held_tags(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    arn: &str,
) -> Result<Vec<(String, String)>, Refusal> {
    let mut held: Vec<(String, String)> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_TAG_PAGES {
        let mut request = Map::new();
        request.insert(
            RESOURCE_ARN_MEMBER.to_string(),
            Value::String(arn.to_string()),
        );
        if let Some(token) = &cursor {
            request.insert(NEXT_TOKEN_MEMBER.to_string(), Value::String(token.clone()));
        }
        let answered = call_json_rpc(
            ctx,
            DYNAMODB_SIGNING_NAME,
            region,
            AWS_JSON_1_0,
            &format!("{DYNAMODB_TARGET_PREFIX}.ListTagsOfResource"),
            &Value::Object(request),
        )
        .await?;
        if let Some(Value::Array(tags)) = answered.get(TAGS_PROPERTY) {
            for tag in tags {
                let key = tag.get(TAG_KEY_MEMBER).and_then(Value::as_str);
                let value = tag.get(TAG_VALUE_MEMBER).and_then(Value::as_str);
                if let (Some(key), Some(value)) = (key, value) {
                    held.push((key.to_string(), value.to_string()));
                }
            }
        }
        cursor = answered
            .get(NEXT_TOKEN_MEMBER)
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(ToString::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(held)
}

/// Whether DynamoDB reports the table this create or change made ready.
///
/// The TABLE itself is waited for: `CreateTable` answers a table whose `TableStatus` is
/// `CREATING`, and "you can perform read and write operations only on an ACTIVE table", so a
/// table in that window takes none of the calls its declaring stack published it for — a
/// client handed the table's name or ARN by a completed stack is answered as if the table
/// were not there. That is the same case Amazon EC2's `pending` is for an instance
/// ([`super::ec2_instance::readiness`]): the stack holds the resource's creation open until
/// the owning service reports the resource out of its own creation window, so a client that
/// reads the stack as complete finds a table it can really read and write. `UPDATING` is
/// that window for a CHANGE: the declaration is not in force while that service is still
/// putting it there.
///
/// Also waited for is a SETTING the DECLARATION asked for that this arm applied through a
/// call of its own ([`settings`]): until that service's own read reports the setting in
/// force, the configuration the declaration asked for is not the configuration the table
/// has, and a resource reported created is one whose whole declared configuration a client
/// reads back. A declaration that asked for none of those settings is nothing to wait for,
/// and nothing is asked about them. `declared` is `None` where the records do not say what
/// the resource was declared with, and every setting of the type is then asked about, since
/// nothing says which of them this resource's creation owes.
///
/// A read DynamoDB refuses, and a table it reports none of, say nothing about either
/// question — one unanswered read is no evidence about the table — so neither holds the
/// resource open, exactly as [`settings::settling`] treats a refused read.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Readiness {
    if let Some(status) = creation_window(ctx, region, physical_resource_id).await {
        return Readiness::Pending(status);
    }
    match settings::settling(ctx, region, physical_resource_id, declared).await {
        Some(status) => Readiness::Pending(status),
        None => Readiness::Ready,
    }
}

/// The `TableStatus` DynamoDB reports for a table it has not finished creating or changing
/// ([`in_change_window`]), or `None` where it reports the table out of both windows — or
/// reports nothing this can read.
async fn creation_window(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Option<String> {
    let description = describe(ctx, region, physical_resource_id).await.ok()??;
    let status = description.get(TABLE_STATUS_MEMBER)?.as_str()?;
    in_change_window(status).then(|| status.to_string())
}

/// Apply a CHANGED declaration to the table the stack already has, through DynamoDB's own
/// `UpdateTable`.
///
/// Only the members that really CHANGED are sent: `UpdateTable` refuses a
/// `StreamSpecification` that enables a stream the table already has, so re-stating an
/// unchanged member would turn a change to one property into a refusal of the whole
/// deployment.
///
/// The table's stream is the one member whose change can need more than one call
/// ([`stream_steps`]), since that same refusal means no call changes what an ENABLED stream
/// captures; each call is made on a settled table, and the attributes published for the
/// resource are read off the LAST answer, so an output over the stream ARN follows the stream
/// the change left the table with.
///
/// `Ok(None)` where the change is to a property `UpdateTable` does not take
/// ([`UPDATE_MEMBERS`]), or to one it takes and the declaration DROPPED with no value for the
/// call to state ([`update_request`]): the table keeps the configuration in force and the
/// deployment reports the change as not applied, rather than as a success that dropped it.
///
/// The table's NAME is its identity, and a declaration that changes it is a different
/// resource: [`super::super::template`] computes a new physical id for it, so the deployment
/// replaces the table rather than reaching here.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type)?;
    // The settings that are calls of their own: what takes each from what the table has to
    // what this declaration asks for, or `None` where one of those changes has no call of
    // this service to express it — which is reported as not applied, exactly as a change to a
    // property `UpdateTable` does not take is.
    let Some(setting_calls) = settings::calls(Some(&resource.properties), declared) else {
        return Ok(None);
    };
    // What takes the table's STREAM from the specification in force to the declared one,
    // which travels as its own call rather than as a member of the request below.
    let streams = stream_steps(&resource.properties, declared);
    // The members of `UpdateTable` this declaration moves, or `None` where one of those
    // changes has no in-place path.
    let Some(request) = update_request(&resource.properties, declared) else {
        return Ok(None);
    };
    if changed_outside_update_members(&resource.properties, declared) {
        return Ok(None);
    }
    if request.is_empty() && setting_calls.is_empty() && streams.is_empty() {
        // Nothing `UpdateTable` takes changed, no setting of its own changed, and nothing
        // outside either did, so the table already has the declared configuration.
        return Ok(Some(Created::default()));
    }
    // Every change below is made once DynamoDB reports the table out of the window it
    // refuses one in ([`await_settled`]), exactly as the delete is.
    await_settled(ctx, region, &resource.physical_resource_id).await;
    // The settings first: each of their calls leaves the TABLE itself untouched, while
    // `UpdateTable` leaves it in the window this service refuses a further change in.
    settings::apply(ctx, region, &resource.physical_resource_id, &setting_calls).await?;
    let mut answered = None;
    for specification in streams {
        let mut body = Map::new();
        body.insert(STREAM_SPECIFICATION_PROPERTY.to_string(), specification);
        answered = Some(update_table(ctx, region, &resource.physical_resource_id, body).await?);
    }
    if !request.is_empty() {
        answered = Some(update_table(ctx, region, &resource.physical_resource_id, request).await?);
    }
    let Some(answered) = answered else {
        // Only settings changed, and they are applied: the table itself keeps the
        // configuration in force.
        return Ok(Some(Created::default()));
    };
    Ok(Some(Created::attributes(published_attributes(
        answered.get(TABLE_DESCRIPTION_MEMBER),
    ))))
}

/// The `UpdateTable` body that takes the table from the configuration `in_force` to the one
/// `declared` asks for, beside the member that addresses the call — or `None` where one of
/// those changes has no in-place path, which the deployment reports as not applied.
///
/// Every property of the type that `UpdateTable` takes as a member of its own is carried
/// here, under the member's own spelling where the two sides differ: the ones named
/// identically ([`UPDATE_MEMBERS`]), and the encryption settings, whose flag the template and
/// the API spell differently ([`sse_change`]). The table's stream is a member of the same
/// call whose change can need more than one of them, so it travels as its own step
/// ([`stream_steps`]).
///
/// Only the members that really CHANGED are carried: `UpdateTable` refuses a request that
/// re-states a value the table already has, so re-stating an unchanged member would turn a
/// change to one property into a refusal of the whole deployment.
///
/// A member the declaration DROPPED is not a member `UpdateTable` can take back to its
/// default — there is no "unset" on that call — so such a change has no in-place path, with
/// ONE exception: a member the capacity mode the declaration asks for does not take at all
/// ([`applies_in_capacity_mode`]) is retired BY that mode change, so there is nothing left
/// for the call to state and the drop is part of the change rather than a change with no
/// path. A declaration that drops the capacity figure of the mode it LEAVES — the
/// provisioned pair as the table goes on-demand, the on-demand ceiling as it goes
/// provisioned — is therefore applied in place.
///
/// The capacity mode itself travels as the mode the declaration RESOLVES to
/// ([`capacity_mode`]) rather than as the member as written, so a declaration that drops
/// `BillingMode` asks for the mode a table carrying none is in instead of leaving the change
/// unexpressed.
fn update_request(in_force: &Value, declared: &Value) -> Option<Map<String, Value>> {
    let mode = capacity_mode(declared);
    let mut request = Map::new();
    for member in UPDATE_MEMBERS {
        if *member == BILLING_MODE_PROPERTY {
            // Carried below as the mode the declaration resolves to, which a declaration
            // that states no mode states as well.
            continue;
        }
        let asked = declared_in(declared, member);
        if asked == declared_in(in_force, member) {
            continue;
        }
        let Some(value) = asked else {
            if !applies_in_capacity_mode(member, mode) {
                continue;
            }
            return None;
        };
        request.insert((*member).to_string(), value.clone());
    }
    if mode != capacity_mode(in_force) {
        request.insert(
            BILLING_MODE_PROPERTY.to_string(),
            Value::String(mode.to_string()),
        );
    }
    // The server-side-encryption settings are a member of the same call, under the API's own
    // spelling of the flag ([`sse_change`]), so a declaration that turns encryption on, moves
    // it to another key, or asks for the Amazon Web Services owned key again is applied in
    // place rather than reported as a change with no path.
    if let Some(specification) = sse_change(in_force, declared) {
        request.insert(SSE_SPECIFICATION_PROPERTY.to_string(), specification);
    }
    Some(request)
}

/// The read/write capacity mode a declaration puts the table in: the mode it states, or the
/// provisioned one for a declaration that states none ([`BILLING_MODE_PROPERTY`]).
fn capacity_mode(properties: &Value) -> &str {
    declared_in(properties, BILLING_MODE_PROPERTY)
        .and_then(Value::as_str)
        .unwrap_or(PROVISIONED_CAPACITY_MODE)
}

/// Whether the property `member` is one the capacity mode `mode` takes at all.
///
/// The two capacity figures each belong to one mode: the provisioned pair is the capacity a
/// `PROVISIONED` table is charged for and "you cannot specify this property" for a
/// `PAY_PER_REQUEST` one, while the on-demand ceiling is "the maximum number of read and
/// write units for the specified on-demand table" ([`PROVISIONED_THROUGHPUT_PROPERTY`],
/// [`ON_DEMAND_THROUGHPUT_PROPERTY`]). Every other member of the call is about the table
/// whatever its mode.
///
/// A declaration that STATES a figure the mode it asks for does not take is a contradiction
/// of the owning service's own rule, not a drop: it travels into the call and that service
/// refuses it, which fails the resource rather than being dropped from a success.
fn applies_in_capacity_mode(member: &str, mode: &str) -> bool {
    match member {
        PROVISIONED_THROUGHPUT_PROPERTY => mode == PROVISIONED_CAPACITY_MODE,
        ON_DEMAND_THROUGHPUT_PROPERTY => mode == ON_DEMAND_CAPACITY_MODE,
        _ => true,
    }
}

/// One `UpdateTable` on the table `physical_resource_id`, carrying `request` beside the member
/// that addresses it, made once DynamoDB reports the table out of the window it refuses a
/// change in — which is the window the PREVIOUS call of a multi-call change left it in.
async fn update_table(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    mut request: Map<String, Value>,
) -> Result<Value, Refusal> {
    await_settled(ctx, region, physical_resource_id).await;
    request.insert(
        TABLE_NAME_PROPERTY.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    call_json_rpc(
        ctx,
        DYNAMODB_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{DYNAMODB_TARGET_PREFIX}.UpdateTable"),
        &Value::Object(request),
    )
    .await
}

/// Delete the table a stack created, addressed by the name its physical id carries.
///
/// The delete is made once DynamoDB reports the table out of the window it refuses a delete
/// in ([`await_settled`]): "tried to delete a table currently in the CREATING state" is a
/// `ResourceInUseException`, so a deployment deleting a table its own create had just made
/// would fail a resource it was perfectly able to delete.
///
/// A table DynamoDB no longer holds is already in the state the delete asks for, judged on
/// the CODE and never on a message, which carries the request's own table name.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    delete_table(ctx, region, &resource.physical_resource_id).await
}

/// Delete the table `table_name`: what the stack's own deletion owes, and what a create
/// whose declared settings this service refused undoes before it reports that refusal.
async fn delete_table(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    table_name: &str,
) -> Result<(), Refusal> {
    await_settled(ctx, region, table_name).await;
    let request = addressed(table_name);
    match call_json_rpc(
        ctx,
        DYNAMODB_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{DYNAMODB_TARGET_PREFIX}.DeleteTable"),
        &request,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == TABLE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether DynamoDB has finished removing the table a stack deleted.
///
/// `DeleteTable` is accepted, not carried out: "DynamoDB immediately returns a response with
/// a `TableStatus` of `DELETING`", and until that service has taken the table away every
/// `DescribeTable` still answers for it — so the removal is READ from DynamoDB rather than
/// assumed, exactly as the create's own window is ([`readiness`]). `Removal::Gone` is the
/// table that service no longer holds, judged on the CODE and never on a message
/// ([`holds_no_table`]); a table it still reports is one still being removed, reported in the
/// status it reports it in. CloudFormation does not report a resource `DELETE_COMPLETE` while
/// the service that owns it still holds it, so a client that reads the table straight after
/// the stack's deletion is refused by DynamoDB rather than served a table the stack said was
/// gone.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let request = addressed(physical_resource_id);
    match call_json_rpc(
        ctx,
        DYNAMODB_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{DYNAMODB_TARGET_PREFIX}.DescribeTable"),
        &request,
    )
    .await
    {
        Ok(answered) => Ok(Removal::Pending(
            value_at(&answered, &[TABLE_MEMBER, TABLE_STATUS_MEMBER])
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )),
        Err(refusal) if holds_no_table(&refusal) => Ok(Removal::Gone),
        Err(refusal) => Err(refusal),
    }
}

/// The error codes DynamoDB answers for a table it NO LONGER HOLDS: the control-plane code
/// every table call of that service answers ([`TABLE_NOT_FOUND`]), and the one the reads
/// that report a table's own settings model instead
/// (`DescribeContinuousBackups`/`DescribeTimeToLive`'s `TableNotFoundException`).
const TABLE_GONE_CODES: &[&str] = &[TABLE_NOT_FOUND, "TableNotFoundException"];

/// Whether a refusal is DynamoDB answering that it holds no such table, judged on the CODE
/// and never on a message, which carries the request's own table name.
fn holds_no_table(refusal: &Refusal) -> bool {
    TABLE_GONE_CODES.contains(&refusal.code.as_str())
}

/// The `TableDescription` members a drift check reports under the property of the SAME name:
/// what `DescribeTable` answers for each of them is what `AWS::DynamoDB::Table` declares
/// under that spelling, so the two sides of the comparison name the same configuration.
///
/// Source: `TableDescription`'s own members
/// (`smithy_query(query="shape",shape="TableDescription")`), intersected with the properties
/// the type publishes. A description member whose value the type spells differently is
/// translated instead ([`SUMMARISED_PROPERTIES`], [`actual_configuration`]).
///
/// `AttributeDefinitions` is deliberately NOT among them: DynamoDB reports the definitions in
/// an order of its own rather than the declared one, and the comparison walks a list element
/// by element, so comparing the two would read that order as a changed configuration. Nor is
/// `TableName`: it is the resource's own IDENTITY, which the read this answer comes from was
/// ADDRESSED by, and the comparison leaves that property out for exactly that reason
/// ([`super::super::drift`]).
const REPORTED_MEMBERS: &[&str] = &[
    "DeletionProtectionEnabled",
    "GlobalSecondaryIndexes",
    "KeySchema",
    "LocalSecondaryIndexes",
    "OnDemandThroughput",
    "ProvisionedThroughput",
    "WarmThroughput",
];

/// The properties the description reports INSIDE a summary of its own: the property the type
/// declares, and the path its value sits at in the description.
///
/// Source: `TableDescription$TableThroughputModeSummary` ("the read/write throughput mode of
/// the table"), `$BillingModeSummary` ("the details for the read/write capacity mode", whose
/// own note says a table that has never been on-demand reports none) and
/// `$TableClassSummary` ("details of the table class"). The mode is read from the newer
/// summary first, and from the older one for an answer that carries only that; the FIRST path
/// an answer carries is the one reported, so one property is never reported twice.
const SUMMARISED_PROPERTIES: &[(&str, &[&str])] = &[
    (
        "BillingMode",
        &["TableThroughputModeSummary", "TableThroughputMode"],
    ),
    ("BillingMode", &["BillingModeSummary", "BillingMode"]),
    ("TableClass", &["TableClassSummary", "TableClass"]),
];

/// The table's configuration as Amazon DynamoDB reports it, for drift detection: the
/// properties of `AWS::DynamoDB::Table` that service answers for, each under the name the
/// TYPE declares it as. A table that service no longer holds was deleted (`None`).
///
/// Two reads stand behind it, which is how the type's properties are configured in the first
/// place ([`create`]): `DescribeTable` for the table itself, and the read that owns each
/// SETTING the declaration carries ([`settings::reported_configuration`]) — a property no
/// member of `DescribeTable` carries is one no `DescribeTable` answer can report.
///
/// A property neither read reports is left out rather than guessed at: the comparison takes
/// an absent property as no evidence about the resource, which is what a service reporting
/// the configuration it models rather than every property a template may declare means. Left
/// out for that reason are the table's tag set and its resource-based policy, neither of
/// which `DescribeTable` carries, and `SSESpecification`, which the description spells under
/// other member names (`SSEDescription`) and whose key it names by the ARN that service
/// resolved, where a declaration may state an alias or a key id.
///
/// Only the members a declaration STATES are compared, so a member the reported value carries
/// beside them — the flag the API's own `StreamSpecification` requires, the counters a
/// throughput description carries — says nothing about the resource; a member the declaration
/// states and the service reports no value for is the configuration genuinely no longer
/// carrying it.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(description) = describe(ctx, region, &resource.physical_resource_id).await? else {
        return Ok(None);
    };
    let mut reported = Map::new();
    for property in REPORTED_MEMBERS {
        if let Some(value) = declared_in(&description, property) {
            reported.insert((*property).to_string(), value.clone());
        }
    }
    for (property, path) in SUMMARISED_PROPERTIES {
        if reported.contains_key(*property) {
            continue;
        }
        if let Some(value) = value_at(&description, path) {
            reported.insert((*property).to_string(), value.clone());
        }
    }
    // "The current DynamoDB Streams configuration for the table", which the read reports only
    // for a table whose stream is ON: a table it reports none for has no stream, which the
    // API's own specification spells as the flag turned off — the same form a declaration
    // asking for no stream reaches that service as ([`stream_specification`]).
    reported.insert(
        STREAM_SPECIFICATION_PROPERTY.to_string(),
        declared_in(&description, STREAM_SPECIFICATION_PROPERTY)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ STREAM_ENABLED_MEMBER: false })),
    );
    // The settings that are reads of this service's own, for the properties the declaration
    // carries. A table that is gone by the time one of them is asked is the same answer the
    // read above gives for one.
    let Some(settings) = settings::reported_configuration(
        ctx,
        region,
        &resource.physical_resource_id,
        &resource.properties,
    )
    .await?
    else {
        return Ok(None);
    };
    reported.extend(settings);
    Ok(Some(Value::Object(reported)))
}

/// One value inside a description, at the path of member names `path` names, or `None` where
/// the description carries no such value.
///
/// The one walk of a nested answer this arm has, shared with the settings half
/// ([`settings`]), which reads each of its own answers the same way.
pub(super) fn value_at<'v>(description: &'v Value, path: &[&str]) -> Option<&'v Value> {
    let mut at = description;
    for member in path {
        at = declared_in(at, member)?;
    }
    Some(at)
}

/// The `TableStatus` values DynamoDB refuses a further change of the table in: "attempt to
/// recreate an existing table, or tried to delete a table currently in the CREATING state"
/// is its `ResourceInUseException`, and the same holds while an earlier change is still
/// being applied. They are the same windows a table serves no item call in ("You can perform
/// read and write operations only on an ACTIVE table"), which is why [`readiness`] holds the
/// resource open on them as well.
const TABLE_STATUS_CREATING: &str = "CREATING";
const TABLE_STATUS_UPDATING: &str = "UPDATING";

/// Whether `status` is one of those windows — the ONE reading of a `TableStatus` in this
/// module, so the wait before a further call ([`await_settled`]) and the readiness of a
/// created or changed resource ([`creation_window`]) cannot come to disagree about which
/// statuses they are.
fn in_change_window(status: &str) -> bool {
    status == TABLE_STATUS_CREATING || status == TABLE_STATUS_UPDATING
}

/// How long [`await_settled`] waits between asking DynamoDB whether the table is out of
/// those windows, and how many times it asks.
///
/// The interval is short enough that a settled table is noticed about as soon as it settles,
/// and the budget covers the span DynamoDB itself holds a new table `CREATING` for several
/// times over, so a loaded machine does not turn the wait into a failure. A wait that runs
/// out is not a failure of its own: what fails then is the call that needed the table
/// settled, reported as DynamoDB's own refusal of it.
const SETTLED_CHECK_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(25);
const SETTLED_CHECKS: usize = 200;

/// Wait until DynamoDB reports the table out of the windows it refuses a change in
/// ([`TABLE_STATUS_CREATING`], [`TABLE_STATUS_UPDATING`]), which is what the next call on it
/// needs.
///
/// Returns as soon as the table is settled, and gives up after [`SETTLED_CHECKS`] checks. A
/// refusal of the READ ends the wait the same way: it says nothing about whether the table
/// is settled, and the call that follows is what reports DynamoDB's answer.
async fn await_settled(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) {
    for _ in 0..SETTLED_CHECKS {
        let Ok(described) = describe(ctx, region, physical_resource_id).await else {
            return;
        };
        let Some(description) = described else {
            // DynamoDB holds no such table, so there is nothing to wait for.
            return;
        };
        let status = description
            .get(TABLE_STATUS_MEMBER)
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !in_change_window(status) {
            return;
        }
        ::tokio::time::sleep(SETTLED_CHECK_INTERVAL).await;
    }
}

/// The table's description as DynamoDB reports it, or `None` where that service holds no
/// such table.
async fn describe(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = addressed(physical_resource_id);
    match call_json_rpc(
        ctx,
        DYNAMODB_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{DYNAMODB_TARGET_PREFIX}.DescribeTable"),
        &request,
    )
    .await
    {
        Ok(answered) => Ok(answered.get(TABLE_MEMBER).cloned()),
        Err(refusal) if refusal.code == TABLE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// A request that only ADDRESSES one table: the single `TableName` member `DeleteTable` and
/// `DescribeTable` take.
fn addressed(physical_resource_id: &str) -> Value {
    let mut request = Map::new();
    request.insert(
        TABLE_NAME_PROPERTY.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    Value::Object(request)
}

/// The `Fn::GetAtt` attributes the type publishes, read off the `TableDescription` DynamoDB
/// answered ([`PUBLISHED_ATTRIBUTES`]).
///
/// An attribute the description does not carry is LEFT OUT rather than reported empty: a
/// table whose declaration enabled no stream has no stream ARN, and the resource keeps
/// whatever the naming rules computed for the names it does publish.
fn published_attributes(description: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(description) = description else {
        return attributes;
    };
    for (attribute, member) in PUBLISHED_ATTRIBUTES {
        if let Some(value) = description
            .get(*member)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            attributes.insert((*attribute).to_string(), value.to_string());
        }
    }
    attributes
}

/// One declared property, or `None` where the declaration states none or states it null.
fn declared_in<'p>(properties: &'p Value, property: &str) -> Option<&'p Value> {
    properties
        .get(property)
        .filter(|value| !value.is_null())
}

/// The API's `StreamSpecification` for the property the template declares, or `None` where it
/// declares none.
///
/// The type publishes `StreamViewType` and a `ResourcePolicy` for the stream, and no flag of
/// its own: declaring the property IS asking for a stream, while the API's own structure
/// REQUIRES `StreamEnabled` ("Member must not be null" is how `CreateTable` refuses a request
/// that leaves it out). So the flag the declaration means — enabled — is what CloudFormation
/// supplies, a declaration that states the flag itself keeps the value it states, and the view
/// type travels as written for DynamoDB to judge against its own enum.
fn stream_specification(properties: &Value) -> Option<Value> {
    let declared = declared_in(properties, STREAM_SPECIFICATION_PROPERTY)?;
    let Value::Object(members) = declared else {
        // Not an object: handed over as written, for DynamoDB to judge.
        return Some(declared.clone());
    };
    let mut specification = Map::new();
    specification.insert(
        STREAM_ENABLED_MEMBER.to_string(),
        members
            .get(STREAM_ENABLED_MEMBER)
            .cloned()
            .unwrap_or(Value::Bool(true)),
    );
    if let Some(view_type) = members.get(STREAM_VIEW_TYPE_MEMBER) {
        specification.insert(STREAM_VIEW_TYPE_MEMBER.to_string(), view_type.clone());
    }
    Some(Value::Object(specification))
}

/// The API's `SSESpecification` for the property the template declares, or `None` where it
/// declares none.
///
/// The two sides spell the flag differently — the template's property is `SSEEnabled` and
/// the API's member is `Enabled` — so it is renamed; `SSEType` and `KMSMasterKeyId` are
/// spelled the same way on both and travel as written.
fn sse_specification(properties: &Value) -> Option<Value> {
    let declared = declared_in(properties, SSE_SPECIFICATION_PROPERTY)?;
    let Value::Object(members) = declared else {
        // Not an object: handed over as written, for DynamoDB to judge.
        return Some(declared.clone());
    };
    let mut specification = Map::new();
    for (property, value) in members {
        let member = if property == SSE_ENABLED_PROPERTY {
            SSE_ENABLED_MEMBER
        } else {
            property.as_str()
        };
        specification.insert(member.to_string(), value.clone());
    }
    Some(Value::Object(specification))
}

/// Whether a specification asks for a stream: every one that does not state the API's flag
/// FALSE does, since declaring the property is what asks for one.
fn asks_for_a_stream(specification: &Value) -> bool {
    specification.get(STREAM_ENABLED_MEMBER) != Some(&Value::Bool(false))
}

/// Whether a specification asks for the table to be encrypted with a KMS key: the API's own
/// flag stated TRUE.
///
/// "If enabled (true), server-side encryption type is set to `KMS` … If disabled (false) or
/// not specified, server-side encryption is set to Amazon Web Services owned key"
/// (`smithy_query(query="doc",shape="SSESpecification",member="Enabled")`), so a
/// specification that states the flag otherwise — and a declaration that states no
/// specification at all — asks for the owned key.
fn asks_for_encryption(specification: Option<&Value>) -> bool {
    specification.and_then(|specification| specification.get(SSE_ENABLED_MEMBER))
        == Some(&Value::Bool(true))
}

/// The `SSESpecification` that takes the table's server-side encryption from the one in force
/// to the declared one, or `None` where the table already has what the declaration asks for.
///
/// `SSESpecification` IS a member of `UpdateTable`
/// (`smithy_query(query="operation",operation="UpdateTable")`), under the API's own spelling
/// of the flag ([`sse_specification`]), so a deployment that changes the property is applied
/// in place: the key a declaration names travels into the call as written, and a declaration
/// the deployment DROPPED asks for the Amazon Web Services owned key, which the same member
/// expresses as the flag turned off.
///
/// A declaration that asks for the owned key over a table that already has it is nothing to
/// call, exactly as an unchanged specification is: only the member that really CHANGED is
/// carried.
fn sse_change(in_force: &Value, declared: &Value) -> Option<Value> {
    let before = sse_specification(in_force);
    let after = sse_specification(declared);
    if before == after {
        return None;
    }
    if !asks_for_encryption(after.as_ref()) {
        // The owned key: asked for by the flag turned off, and already in force on a table
        // whose own specification asks for no KMS key either.
        return asks_for_encryption(before.as_ref())
            .then(|| serde_json::json!({ SSE_ENABLED_MEMBER: false }));
    }
    after
}

/// The `StreamSpecification` of each `UpdateTable` that takes the table's stream from the
/// specification in force to the declared one, in the order the calls must be made.
///
/// A stream's identity is its ARN, and `UpdateTable` refuses a specification that enables a
/// stream on "a table that already has a stream", so NO call changes what an enabled stream
/// captures: a declaration that changes the view type is applied by turning the stream in
/// force off and a stream capturing what it now asks for on. The table's stream ARN after
/// such a change is therefore a new one, and an `Fn::GetAtt` over it follows the stream the
/// table now has.
///
/// A declaration that states no specification at all asks for no stream, which `UpdateTable`
/// expresses as the flag turned off; a table that has no stream is already in the state such
/// a declaration asks for, so nothing is called for it.
fn stream_steps(in_force: &Value, declared: &Value) -> Vec<Value> {
    let before = stream_specification(in_force);
    let after = stream_specification(declared);
    if before == after {
        return Vec::new();
    }
    let enabled = before.as_ref().is_some_and(asks_for_a_stream);
    let mut turn_off = Map::new();
    turn_off.insert(STREAM_ENABLED_MEMBER.to_string(), Value::Bool(false));
    let turn_off = Value::Object(turn_off);
    match after {
        Some(specification) if asks_for_a_stream(&specification) => {
            if enabled {
                vec![turn_off, specification]
            } else {
                vec![specification]
            }
        }
        _ if enabled => vec![turn_off],
        _ => Vec::new(),
    }
}

/// The policy document a declared `ResourcePolicy` carries, as the TEXT `CreateTable`'s own
/// `ResourcePolicy` member takes.
///
/// "ResourcePolicy … PolicyDocument" is an object in a template, while the API member is
/// "an Amazon Web Services resource-based policy document in JSON format" — a string. A
/// document already written as text travels as written, so a template that states the policy
/// as a JSON string is not re-encoded.
fn resource_policy(properties: &Value) -> Option<String> {
    let declared = declared_in(properties, RESOURCE_POLICY_PROPERTY)?;
    let document = declared.get(POLICY_DOCUMENT_PROPERTY).unwrap_or(declared);
    match document {
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// The tags of a declared table that DynamoDB will STORE: the ones the declaration asks for
/// and the ones CloudFormation propagates ([`tags_of`]), less every key in the namespace
/// DynamoDB reserves.
///
/// A key in the reserved `aws:` namespace "cannot be assigned by a caller" is DynamoDB's own
/// rule for every caller of its tag-taking calls — it refuses the whole request on one such
/// key — so the three records CloudFormation keeps about a resource it provisioned (the
/// stack's name, its id and the resource's logical id) are left in CloudFormation's own
/// records rather than sent to a call that would refuse the table outright. Every other tag
/// travels, which is what a stack's tags being "propagated to the resources that
/// CloudFormation creates" means here. It is the same split
/// [`super::lambda_function`] records for AWS Lambda, which reserves the same namespace.
fn storable_tags(properties: &Value, propagated: &[(String, String)]) -> Vec<(String, String)> {
    tags_of(properties, propagated)
        .into_iter()
        .filter(|(key, _)| !key.to_ascii_lowercase().starts_with(RESERVED_TAG_PREFIX))
        .collect()
}

/// The tag set as `CreateTable`'s own `Tags` list: "an array of key-value pairs", each a
/// `Tag` with a `Key` and a `Value` — the form [`super::key_value_tag_list`] renders for
/// every provider whose owning model spells it that way.
fn tag_list(tags: &[(String, String)]) -> Value {
    key_value_tag_list(tags)
}

/// Whether the declaration changes a property that `UpdateTable` does not take, which is
/// what [`update`] reports as having no in-place path.
fn changed_outside_update_members(in_force: &Value, declared: &Value) -> bool {
    let mut names: Vec<&str> = Vec::new();
    for properties in [in_force, declared] {
        if let Value::Object(members) = properties {
            for property in members.keys() {
                if !UPDATE_MEMBERS.contains(&property.as_str())
                    && !names.contains(&property.as_str())
                {
                    names.push(property.as_str());
                }
            }
        }
    }
    names
        .into_iter()
        // The tags of a resource already created are reconciled by `retag`, not by an
        // update, so a change to them alone is not a configuration change with no path.
        .filter(|property| *property != TAGS_PROPERTY)
        // A setting DynamoDB applies through a call of its own has an in-place path of its
        // own ([`settings`]), so a change to one is not a change `UpdateTable` had to carry.
        .filter(|property| !settings::is_a_setting(property))
        // The stream configuration IS a member `UpdateTable` takes — under the API's own
        // spelling of it ([`stream_specification`]) — so [`update`] carries a change to it.
        .filter(|property| *property != STREAM_SPECIFICATION_PROPERTY)
        // So are the server-side-encryption settings, under the API's own spelling of their
        // flag ([`sse_change`]).
        .filter(|property| *property != SSE_SPECIFICATION_PROPERTY)
        .any(|property| declared_in(declared, property) != declared_in(in_force, property))
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a table from being reported created with less than its
/// declaration asked for.
///
/// What is left is `ImportSourceSpecification`: it is not a property applied to a table this
/// arm created but a different create — `ImportTable` rather than `CreateTable` — so a
/// declaration carrying it asks for a table nothing here makes, filed as its own gap rather
/// than dropped from a create that answered success. Every other property of the type is
/// either a member of the create or a call of this service's own on the table that exists,
/// which [`settings`] makes.
///
/// A property this arm TRANSLATES is checked member by member for the same reason: the
/// translation carries the members it names ([`STREAM_SPECIFICATION_MEMBERS`]), so one it does
/// not name would be dropped from a create that answered success.
fn refuse_unapplied_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Value::Object(members) = properties else {
        return Ok(());
    };
    for property in members.keys() {
        if applies(property) {
            continue;
        }
        return Err(unapplied(property, resource_type));
    }
    // The stream's own resource-based policy is a sub-resource of the STREAM rather than a
    // member of either table call, so a declaration asking for one asks for something this
    // arm does not put: refused, for the reason every unapplied property above is. The two
    // members the API's own structure carries are applied ([`stream_specification`]).
    if let Some(Value::Object(specification)) =
        declared_in(properties, STREAM_SPECIFICATION_PROPERTY)
    {
        for member in specification.keys() {
            if !STREAM_SPECIFICATION_MEMBERS.contains(&member.as_str()) {
                return Err(unapplied(
                    &format!("{STREAM_SPECIFICATION_PROPERTY}.{member}"),
                    resource_type,
                ));
            }
        }
    }
    Ok(())
}

/// The refusal itself, worded over the declared property it names.
fn unapplied(property: &str, resource_type: &str) -> Refusal {
    Refusal {
        code: "NotImplemented".to_string(),
        message: format!("this emulator does not apply the {property} property of {resource_type}"),
    }
}

/// The code a declaration the resource HANDLER itself refuses is reported under: the fault is
/// in the declaration, which is the class every validation refusal falls in
/// ([`super::HANDLER_ERROR_INVALID_REQUEST`]).
const DECLARATION_REFUSED: &str = "ValidationException";

/// The sentence the handler refuses a declaration whose key schema names an attribute the
/// declaration does not define with.
///
/// Entirely FIXED text: it states the rule and names neither the attribute nor the index. The
/// handler holds the declaration to the rule itself, before it calls the owning service at
/// all, so what a resource's failed event reports is which rule the template broke — not the
/// index-oriented `ValidationException` DynamoDB words `CreateTable`'s own refusal with, which
/// names an index a template declaring none does not have.
const UNDEFINED_KEY_ATTRIBUTE: &str =
    "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions";

/// Hold the declaration to the one rule that relates its two key properties: every attribute
/// a `KeySchema` element names — the table's own, and every index's — has to be declared in
/// `AttributeDefinitions`.
///
/// Checked over EVERY key schema the declaration carries, because the rule is about the
/// declaration as a whole: `AttributeDefinitions` is the one list of the attributes the table
/// has types for, and an index key is an attribute of the same table.
fn refuse_undefined_key_attributes(properties: &Value) -> Result<(), Refusal> {
    let defined = attribute_names(declared_in(properties, ATTRIBUTE_DEFINITIONS_PROPERTY));
    let mut referenced = attribute_names(declared_in(properties, KEY_SCHEMA_PROPERTY));
    for property in INDEX_PROPERTIES {
        if let Some(Value::Array(indexes)) = declared_in(properties, property) {
            for index in indexes {
                referenced.extend(attribute_names(declared_in(index, KEY_SCHEMA_PROPERTY)));
            }
        }
    }
    if referenced.iter().all(|name| defined.contains(name)) {
        return Ok(());
    }
    Err(Refusal {
        code: DECLARATION_REFUSED.to_string(),
        message: UNDEFINED_KEY_ATTRIBUTE.to_string(),
    })
}

/// The `AttributeName` every element of a declared key schema or definition list names, in
/// declaration order.
///
/// A declaration that states no list, states it as something other than a list, or whose
/// element names no attribute contributes nothing: what such a declaration is refused for is
/// the owning service's to say, and this rule is only about the names the two lists do carry.
fn attribute_names(declared: Option<&Value>) -> Vec<&str> {
    match declared {
        Some(Value::Array(elements)) => elements
            .iter()
            .filter_map(|element| {
                element
                    .get(ATTRIBUTE_NAME_MEMBER)
                    .and_then(Value::as_str)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether this arm applies the property `name`: a body member of the create, the one that
/// addresses it, the tag set, one of the three whose shape or spelling differs between the two
/// sides, or one of the settings DynamoDB applies through a call of its own ([`settings`]).
fn applies(name: &str) -> bool {
    CREATE_MEMBERS.contains(&name)
        || settings::is_a_setting(name)
        || [
            TABLE_NAME_PROPERTY,
            TAGS_PROPERTY,
            RESOURCE_POLICY_PROPERTY,
            SSE_SPECIFICATION_PROPERTY,
            STREAM_SPECIFICATION_PROPERTY,
        ]
        .contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource record of this type, as the naming rules leave one.
    fn declared(properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Table".to_string(),
            resource_type: DYNAMODB_TABLE.to_string(),
            physical_resource_id: "stack-Table-ABC123".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// Every member `UpdateTable` is sent is also a property the create applies, so one
    /// declaration cannot mean two different things on the two calls.
    #[test]
    fn every_updated_member_is_an_applied_property() {
        for member in UPDATE_MEMBERS {
            assert!(
                applies(member),
                "case {member:?}: expected the updated member to be an applied property of \
                 {DYNAMODB_TABLE}, got none"
            );
        }
    }

    /// One row of [`the_published_attributes_are_the_services_own_answer`]: the case's
    /// name, the `TableDescription` DynamoDB answered, and the attributes it publishes.
    type AttributeRow = (&'static str, Value, &'static [(&'static str, &'static str)]);

    /// The attributes the type publishes are read off the description the create answered,
    /// and an attribute the description does not carry is left out rather than reported
    /// empty.
    #[test]
    fn the_published_attributes_are_the_services_own_answer() {
        let cases: &[AttributeRow] = &[
            (
                "a table with a stream",
                serde_json::json!({
                    "TableArn": "arn:aws:dynamodb:us-east-1:000000000000:table/orders",
                    "LatestStreamArn":
                        "arn:aws:dynamodb:us-east-1:000000000000:table/orders/stream/\
                         2026-10-23T11:08:02.000",
                }),
                &[
                    (
                        "Arn",
                        "arn:aws:dynamodb:us-east-1:000000000000:table/orders",
                    ),
                    (
                        "StreamArn",
                        "arn:aws:dynamodb:us-east-1:000000000000:table/orders/stream/\
                         2026-10-23T11:08:02.000",
                    ),
                ],
            ),
            (
                "a table with no stream: no StreamArn at all",
                serde_json::json!({
                    "TableArn": "arn:aws:dynamodb:us-east-1:000000000000:table/plain",
                }),
                &[(
                    "Arn",
                    "arn:aws:dynamodb:us-east-1:000000000000:table/plain",
                )],
            ),
            (
                "an empty stream ARN is no ARN",
                serde_json::json!({
                    "TableArn": "arn:aws:dynamodb:us-east-1:000000000000:table/plain",
                    "LatestStreamArn": "",
                }),
                &[(
                    "Arn",
                    "arn:aws:dynamodb:us-east-1:000000000000:table/plain",
                )],
            ),
        ];
        for (case, description, expected) in cases {
            let published = published_attributes(Some(description));
            let wanted: BTreeMap<String, String> = expected
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                published, wanted,
                "case {case:?}: expected the published attributes {wanted:?}, got {published:?}"
            );
        }
    }

    /// A property the arm does not apply is refused rather than dropped, and every property
    /// it does apply is admitted.
    #[test]
    fn an_unapplied_property_is_refused() {
        // An import is not a property applied to a table this arm created but a different
        // create, so a declaration carrying it is refused.
        let refused: &[&str] = &["ImportSourceSpecification"];
        for property in refused {
            let mut properties = Map::new();
            properties.insert(
                "KeySchema".to_string(),
                serde_json::json!([{ "AttributeName": "pk", "KeyType": "HASH" }]),
            );
            properties.insert((*property).to_string(), Value::Object(Map::new()));
            let resource = declared(Value::Object(properties));
            let answer =
                refuse_unapplied_properties(&resource.properties, &resource.resource_type);
            assert!(
                answer.is_err(),
                "case {property:?}: expected a refusal naming the unapplied property, got \
                 {answer:?}"
            );
        }
        let applied = declared(serde_json::json!({
            "TableName": "orders",
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": { "StreamEnabled": true,
                                     "StreamViewType": "NEW_AND_OLD_IMAGES" },
            "TimeToLiveSpecification": { "AttributeName": "expiresAt", "Enabled": true },
            "PointInTimeRecoverySpecification": { "PointInTimeRecoveryEnabled": true },
            "ContributorInsightsSpecification": { "Enabled": true },
            "KinesisStreamSpecification": {
                "StreamArn": "arn:aws:kinesis:us-east-1:000000000000:stream/changes" },
            "Tags": [{ "Key": "Tier", "Value": "gold" }],
        }));
        let answer = refuse_unapplied_properties(&applied.properties, &applied.resource_type);
        assert!(
            answer.is_ok(),
            "case {:?}: expected every applied property to be admitted, got {answer:?}",
            "a fully declared table"
        );
    }

    /// The template's `SSEEnabled` reaches the API as its own `Enabled` member, and the
    /// other two members travel as written.
    #[test]
    fn the_encryption_flag_is_renamed_for_the_api() {
        let specification = sse_specification(&serde_json::json!({
            "SSESpecification": { "SSEEnabled": true, "SSEType": "KMS",
                                  "KMSMasterKeyId": "alias/aws/dynamodb" },
        }));
        assert_eq!(
            specification,
            Some(serde_json::json!({
                "Enabled": true, "SSEType": "KMS", "KMSMasterKeyId": "alias/aws/dynamodb",
            })),
            "case {:?}: expected the API's own member names, got {specification:?}",
            "a declared SSESpecification"
        );
    }

    /// A declared `ResourcePolicy` reaches the API as the policy document's TEXT, whichever
    /// of the two spellings the template used.
    #[test]
    fn the_resource_policy_reaches_the_api_as_text() {
        let cases: &[(&str, Value)] = &[
            (
                "a PolicyDocument object",
                serde_json::json!({
                    "ResourcePolicy": { "PolicyDocument": { "Version": "2012-10-17" } },
                }),
            ),
            (
                "a PolicyDocument already written as text",
                serde_json::json!({
                    "ResourcePolicy": { "PolicyDocument": "{\"Version\":\"2012-10-17\"}" },
                }),
            ),
        ];
        for (case, properties) in cases {
            let document = resource_policy(properties);
            assert_eq!(
                document.as_deref(),
                Some("{\"Version\":\"2012-10-17\"}"),
                "case {case:?}: expected the policy document as text, got {document:?}"
            );
        }
    }

    /// The type's own `StreamSpecification` reaches the API with the flag that specification
    /// requires, whatever the template states, and a declaration that states the flag keeps
    /// the value it states.
    #[test]
    fn the_declared_stream_reaches_the_api_with_its_required_flag() {
        // The rows differ along what the rule generalises over: the type's own form (the view
        // type alone, in each of the values the model declares), the API's own spelling of
        // both flag values, and a declaration that states no specification at all.
        let cases: &[(&str, Value, Option<Value>)] = &[
            (
                "the type's own form: the view type alone",
                serde_json::json!({ "StreamSpecification": {
                    "StreamViewType": "NEW_AND_OLD_IMAGES" } }),
                Some(serde_json::json!({ "StreamEnabled": true,
                                         "StreamViewType": "NEW_AND_OLD_IMAGES" })),
            ),
            (
                "the type's own form, capturing keys only",
                serde_json::json!({ "StreamSpecification": { "StreamViewType": "KEYS_ONLY" } }),
                Some(serde_json::json!({ "StreamEnabled": true,
                                         "StreamViewType": "KEYS_ONLY" })),
            ),
            (
                "the API's own spelling travels as written",
                serde_json::json!({ "StreamSpecification": { "StreamEnabled": true,
                                                             "StreamViewType": "NEW_IMAGE" } }),
                Some(serde_json::json!({ "StreamEnabled": true,
                                         "StreamViewType": "NEW_IMAGE" })),
            ),
            (
                "a specification that turns the stream off asks for no stream",
                serde_json::json!({ "StreamSpecification": { "StreamEnabled": false } }),
                Some(serde_json::json!({ "StreamEnabled": false })),
            ),
            (
                "no specification at all: no member on the call",
                serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" }),
                None,
            ),
        ];
        for (case, properties, expected) in cases {
            let specification = stream_specification(properties);
            assert_eq!(
                specification.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the API's own specification {expected:?}, got \
                 {specification:?}"
            );
        }
    }

    /// A `StreamSpecification.ResourcePolicy` is a policy over the STREAM that this arm puts
    /// into force nowhere, so a declaration carrying it is refused rather than dropped.
    #[test]
    fn an_unapplied_stream_member_is_refused() {
        let carried = declared(serde_json::json!({
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "StreamSpecification": { "StreamViewType": "KEYS_ONLY",
                                     "ResourcePolicy": { "PolicyDocument": {} } },
        }));
        let answer = refuse_unapplied_properties(&carried.properties, &carried.resource_type);
        let message = answer.as_ref().err().map(|refusal| refusal.message.clone());
        assert_eq!(
            message.as_deref(),
            Some(
                "this emulator does not apply the StreamSpecification.ResourcePolicy property \
                 of AWS::DynamoDB::Table"
            ),
            "case {:?}: expected a refusal naming the member, got {answer:?}",
            "a stream resource policy"
        );
    }

    /// What takes the table's stream from the specification in force to the declared one: a
    /// change of VIEW TYPE has no single-call path, so it is the one that needs two.
    #[test]
    fn a_view_type_change_is_applied_as_a_disable_and_an_enable() {
        let off = serde_json::json!({ "StreamEnabled": false });
        let cases: &[(&str, Value, Value, Vec<Value>)] = &[
            (
                "the view type changed: the stream in force off, then the declared one on",
                serde_json::json!({ "StreamSpecification": {
                    "StreamViewType": "NEW_AND_OLD_IMAGES" } }),
                serde_json::json!({ "StreamSpecification": { "StreamViewType": "KEYS_ONLY" } }),
                vec![
                    off.clone(),
                    serde_json::json!({ "StreamEnabled": true, "StreamViewType": "KEYS_ONLY" }),
                ],
            ),
            (
                "the same view type: nothing to call",
                serde_json::json!({ "StreamSpecification": { "StreamViewType": "NEW_IMAGE" } }),
                serde_json::json!({ "StreamSpecification": { "StreamViewType": "NEW_IMAGE" } }),
                Vec::new(),
            ),
            (
                "a stream added to a table that had none: one enable",
                serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" }),
                serde_json::json!({ "BillingMode": "PAY_PER_REQUEST",
                                    "StreamSpecification": { "StreamViewType": "OLD_IMAGE" } }),
                vec![serde_json::json!({ "StreamEnabled": true,
                                         "StreamViewType": "OLD_IMAGE" })],
            ),
            (
                "the specification dropped: the stream in force is turned off",
                serde_json::json!({ "StreamSpecification": { "StreamViewType": "OLD_IMAGE" } }),
                serde_json::json!({}),
                vec![off.clone()],
            ),
            (
                "a table with no stream, declared with none: nothing to call",
                serde_json::json!({}),
                serde_json::json!({ "StreamSpecification": { "StreamEnabled": false } }),
                Vec::new(),
            ),
        ];
        for (case, in_force, declared_now, expected) in cases {
            let steps = stream_steps(in_force, declared_now);
            assert_eq!(
                &steps, expected,
                "case {case:?}: expected the calls {expected:?}, got {steps:?}"
            );
        }
    }

    /// A change to a property `UpdateTable` does not take is reported as having no in-place
    /// path, while a change to one it does take is not.
    #[test]
    fn a_change_outside_the_update_members_has_no_in_place_path() {
        let cases: &[(&str, Value, Value, bool)] = &[
            (
                "the capacity mode changed: UpdateTable takes it",
                serde_json::json!({ "BillingMode": "PROVISIONED" }),
                serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" }),
                false,
            ),
            (
                "the key schema changed: no member of UpdateTable",
                serde_json::json!({ "KeySchema": [{ "AttributeName": "pk",
                                                    "KeyType": "HASH" }] }),
                serde_json::json!({ "KeySchema": [{ "AttributeName": "id",
                                                    "KeyType": "HASH" }] }),
                true,
            ),
            (
                "only the tags changed: reconciled by the re-tagging pass",
                serde_json::json!({ "Tags": [{ "Key": "Tier", "Value": "gold" }] }),
                serde_json::json!({ "Tags": [{ "Key": "Tier", "Value": "silver" }] }),
                false,
            ),
            (
                "a property was dropped altogether",
                serde_json::json!({ "LocalSecondaryIndexes": [] }),
                serde_json::json!({}),
                true,
            ),
            (
                "the encryption settings changed: UpdateTable takes them",
                serde_json::json!({ "SSESpecification": { "SSEEnabled": true } }),
                serde_json::json!({ "SSESpecification": { "SSEEnabled": false } }),
                false,
            ),
            (
                "the encryption settings dropped: the same member expresses the owned key",
                serde_json::json!({ "SSESpecification": { "SSEEnabled": true } }),
                serde_json::json!({}),
                false,
            ),
        ];
        for (case, in_force, declared_now, expected) in cases {
            let outside = changed_outside_update_members(in_force, declared_now);
            assert_eq!(
                outside, *expected,
                "case {case:?}: expected a change outside UpdateTable's members to be \
                 {expected:?}, got {outside:?}"
            );
        }
    }

    /// The pair of read/write capacity figures a provisioned declaration carries.
    fn capacity(read: u64, write: u64) -> Value {
        serde_json::json!({ "ReadCapacityUnits": read, "WriteCapacityUnits": write })
    }

    /// The `UpdateTable` body a changed declaration asks for: the capacity mode it RESOLVES
    /// to, the figures the new mode takes, and nothing for a figure that mode does not take
    /// at all — which is what makes a capacity-mode move an in-place change rather than one
    /// with no path.
    #[test]
    fn a_capacity_mode_move_is_one_in_place_update() {
        let provisioned = serde_json::json!({ "BillingMode": "PROVISIONED",
                                              "ProvisionedThroughput": capacity(5, 5) });
        let on_demand = serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" });
        let cases: &[(&str, Value, Value, Option<Value>)] = &[
            (
                "provisioned to on-demand: the pair the new mode does not take is dropped",
                provisioned.clone(),
                on_demand.clone(),
                Some(serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" })),
            ),
            (
                "the same move stated without a mode in force: PROVISIONED is the default",
                serde_json::json!({ "ProvisionedThroughput": capacity(1, 1) }),
                on_demand.clone(),
                Some(serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" })),
            ),
            (
                "on-demand to provisioned: the new mode's own figures travel with it",
                on_demand.clone(),
                provisioned.clone(),
                Some(serde_json::json!({ "BillingMode": "PROVISIONED",
                                         "ProvisionedThroughput": capacity(5, 5) })),
            ),
            (
                "the mode member dropped: the declaration resolves to the provisioned mode",
                on_demand.clone(),
                serde_json::json!({ "ProvisionedThroughput": capacity(2, 3) }),
                Some(serde_json::json!({ "BillingMode": "PROVISIONED",
                                         "ProvisionedThroughput": capacity(2, 3) })),
            ),
            (
                "the on-demand ceiling dropped with the mode that has one",
                serde_json::json!({ "BillingMode": "PAY_PER_REQUEST",
                                    "OnDemandThroughput": { "MaxReadRequestUnits": 10 } }),
                provisioned.clone(),
                Some(serde_json::json!({ "BillingMode": "PROVISIONED",
                                         "ProvisionedThroughput": capacity(5, 5) })),
            ),
            (
                "a ceiling added as the table goes on-demand",
                provisioned.clone(),
                serde_json::json!({ "BillingMode": "PAY_PER_REQUEST",
                                    "OnDemandThroughput": { "MaxWriteRequestUnits": 20 } }),
                Some(serde_json::json!({ "BillingMode": "PAY_PER_REQUEST",
                                         "OnDemandThroughput": {
                                             "MaxWriteRequestUnits": 20 } })),
            ),
            (
                "the mode member dropped for the mode already in force: nothing to call",
                provisioned.clone(),
                serde_json::json!({ "ProvisionedThroughput": capacity(5, 5) }),
                Some(serde_json::json!({})),
            ),
            (
                "a figure the mode in force takes, dropped with the mode kept: no path",
                provisioned.clone(),
                serde_json::json!({ "BillingMode": "PROVISIONED" }),
                None,
            ),
            (
                "a flag dropped, which no mode retires: no path",
                serde_json::json!({ "DeletionProtectionEnabled": true }),
                serde_json::json!({}),
                None,
            ),
        ];
        for (case, in_force, declared_now, expected) in cases {
            let request = update_request(in_force, declared_now).map(Value::Object);
            assert_eq!(
                request.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the UpdateTable body {expected:?}, got {request:?}"
            );
        }
    }

    /// The `SSESpecification` an in-place update carries: the declared one as the API spells
    /// it, the flag turned off where the declaration asks for the Amazon Web Services owned
    /// key, and nothing at all where the table already has what the declaration asks for.
    ///
    /// The rows differ along what the rule generalises over: which way the flag moves, whether
    /// the declaration states the specification or DROPS it, a key identifier changed while
    /// encryption stays on, and both spellings of "the owned key" on either side.
    #[test]
    fn the_encryption_settings_are_carried_by_the_in_place_update() {
        let on = serde_json::json!({ "SSESpecification": { "SSEEnabled": true } });
        let off = serde_json::json!({ "SSESpecification": { "SSEEnabled": false } });
        let none = serde_json::json!({ "BillingMode": "PAY_PER_REQUEST" });
        let cases: &[(&str, Value, Value, Option<Value>)] = &[
            (
                "encryption turned off: the API's own flag, turned off",
                on.clone(),
                off.clone(),
                Some(serde_json::json!({ "Enabled": false })),
            ),
            (
                "the specification dropped: the owned key, asked for the same way",
                on.clone(),
                none.clone(),
                Some(serde_json::json!({ "Enabled": false })),
            ),
            (
                "encryption turned on: the declared specification, renamed",
                off.clone(),
                on.clone(),
                Some(serde_json::json!({ "Enabled": true })),
            ),
            (
                "encryption turned on over a named key: the key travels as written",
                none.clone(),
                serde_json::json!({ "SSESpecification": {
                    "SSEEnabled": true, "SSEType": "KMS",
                    "KMSMasterKeyId": "alias/orders-table" } }),
                Some(serde_json::json!({
                    "Enabled": true, "SSEType": "KMS",
                    "KMSMasterKeyId": "alias/orders-table" })),
            ),
            (
                "the key moved while encryption stays on",
                serde_json::json!({ "SSESpecification": {
                    "SSEEnabled": true, "KMSMasterKeyId": "alias/aws/dynamodb" } }),
                serde_json::json!({ "SSESpecification": {
                    "SSEEnabled": true, "KMSMasterKeyId": "alias/orders-table" } }),
                Some(serde_json::json!({
                    "Enabled": true, "KMSMasterKeyId": "alias/orders-table" })),
            ),
            (
                "the same specification: nothing to carry",
                on.clone(),
                on.clone(),
                None,
            ),
            (
                "the owned key, spelled two ways: already in force, nothing to carry",
                off.clone(),
                none.clone(),
                None,
            ),
            (
                "a table with no specification, declared with the flag off: nothing to carry",
                none.clone(),
                off.clone(),
                None,
            ),
        ];
        for (case, in_force, declared_now, expected) in cases {
            let specification = sse_change(in_force, declared_now);
            assert_eq!(
                specification.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the SSESpecification {expected:?}, got \
                 {specification:?}"
            );
            // And the same specification is what the call's body carries, which is what makes
            // the change an in-place update rather than one with no path.
            let request = update_request(in_force, declared_now)
                .expect("the encryption settings have an in-place path");
            assert_eq!(
                request.get(SSE_SPECIFICATION_PROPERTY),
                expected.as_ref(),
                "case {case:?}: expected the UpdateTable body to carry {expected:?}, got \
                 {request:?}"
            );
        }
    }
}
