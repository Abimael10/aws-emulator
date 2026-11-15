// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the AWS Glue Data Catalog's TABLE OPTIMIZER: the
//! optimizer an `AWS::Glue::TableOptimizer` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! An optimizer is a resource OF a catalog table rather than a named resource of its own:
//! `CreateTableOptimizer` takes no identifier and takes the catalog, the database, the table
//! and the optimizer's own `Type` — the four values `GetTableOptimizer` and
//! `DeleteTableOptimizer` each address one by — so those four ARE the resource's identity
//! ([`super::template::composite_identity_properties`]) and the physical id of one is them
//! joined, which is what the type's `Ref` returns.
//!
//! The type's properties are that call's own members, named identically on both sides, with
//! ONE difference: the optimizer's configuration is a structure whose members AWS Glue's own
//! model spells in lower camel case (`roleArn`, `enabled`, `vpcConfiguration`,
//! `retentionConfiguration`, `orphanFileDeletionConfiguration`,
//! `smithy_query(query="shape", shape="TableOptimizerConfiguration")`) while the template
//! declares them in upper camel case, so that one property is re-cased on the way out
//! ([`lower_camel_members`]) and everything else travels as declared.
//!
//! There is no UPDATE arm: the one call that reconfigures an optimizer that exists
//! (`UpdateTableOptimizer`) is still one of this emulator's scaffold stubs, so an arm built
//! on it could only fail the deployment it is meant to carry — the position an
//! `AWS::Glue::Job`'s tags are in (`create_glue_job`). A changed declaration is therefore
//! reported as not applied, which is what a type with no update arm is documented to do
//! ([`super`]), and a declaration restating any part of the identity replaces the resource
//! instead ([`super::template::replacement_properties`]).

use super::*;

/// The property an `AWS::Glue::TableOptimizer` states the optimizer's configuration in,
/// which is the `TableOptimizerConfiguration` member `CreateTableOptimizer` takes it as.
const TABLE_OPTIMIZER_CONFIGURATION_PROPERTY: &str = "TableOptimizerConfiguration";

/// The property that states WHICH optimizer of the table the resource is, which is part of
/// its identity: `CreateTableOptimizer`, `GetTableOptimizer` and `DeleteTableOptimizer` each
/// take it as the request's own `Type`.
const TABLE_OPTIMIZER_TYPE_PROPERTY: &str = "Type";

/// The property the TABLE the optimizer belongs to is named in, which every call about an
/// optimizer takes as its own `TableName` — unlike the two catalog types beside it, which
/// state an entity's name inside the input structure their own calls take it in
/// (`GLUE_INPUT_NAME_MEMBER`).
const TABLE_OPTIMIZER_TABLE_NAME_PROPERTY: &str = "TableName";

/// The members every call about ONE optimizer takes — the four that identify it
/// (`smithy_query(query="operation", operation="CreateTableOptimizer,GetTableOptimizer,\
/// DeleteTableOptimizer")`), each spelled identically as the type's own property.
const TABLE_OPTIMIZER_ADDRESS: &[&str] = &[
    GLUE_CATALOG_ID_PROPERTY,
    GLUE_DATABASE_NAME_PROPERTY,
    TABLE_OPTIMIZER_TABLE_NAME_PROPERTY,
    TABLE_OPTIMIZER_TYPE_PROPERTY,
];

/// The member of `GetTableOptimizer`'s answer that carries the optimizer itself, whose own
/// `type` is the `Type` the declaration states.
const REPORTED_TABLE_OPTIMIZER: &str = "TableOptimizer";
const REPORTED_TABLE_OPTIMIZER_TYPE: &str = "type";

/// Create the table optimizer an `AWS::Glue::TableOptimizer` declares, so a client that
/// deployed the stack and then calls `glue GetTableOptimizer` for that table and type reads
/// the optimizer back rather than an `EntityNotFoundException`.
///
/// The optimizer is registered in the service that OWNS it, so everything the declaration
/// states is resolved by AWS Glue exactly as it is for a client's own call: a database or a
/// table the Data Catalog does not hold, a table no optimizer could belong to, and a type the
/// table already carries an optimizer of are all that service's refusals, which fail the
/// resource and roll the stack back rather than publishing an optimizer nothing holds.
///
/// The type publishes no `Fn::GetAtt` attribute and no `Tags` property: an optimizer is not a
/// taggable resource — AWS Glue holds tags on the catalog entities its own tagging calls name
/// — so nothing is propagated to one and the identity in force is the one the stack already
/// computed from the declaration.
pub(super) async fn create_glue_table_optimizer(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let mut request = table_optimizer_address(&resource.properties);
    if let Some(configuration) = resource
        .properties
        .get(TABLE_OPTIMIZER_CONFIGURATION_PROPERTY)
    {
        request.insert(
            TABLE_OPTIMIZER_CONFIGURATION_PROPERTY.to_string(),
            lower_camel_members(configuration, 0)?,
        );
    }
    call_glue(
        ctx,
        region,
        "CreateTableOptimizer",
        &Value::Object(request),
    )
    .await?;
    Ok(Created::default())
}

/// Delete the optimizer a stack created, addressed by the four values AWS Glue keys one by —
/// which its declaration carries and which are this resource's physical id.
///
/// An optimizer that service no longer holds (`EntityNotFoundException`, judged on the CODE
/// and never on a message that carries the request's own names) is already in the state the
/// delete asks for — as is one whose table or database is already gone, which answers the
/// same code.
pub(super) async fn delete_glue_table_optimizer(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let request = Value::Object(table_optimizer_address(&resource.properties));
    match call_glue(ctx, region, "DeleteTableOptimizer", &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == GLUE_ENTITY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The optimizer as AWS Glue reports it, for drift detection: its own read's answer, with
/// the table the optimizer belongs to and the optimizer's own type reported under the
/// properties the type declares them as. An optimizer that service no longer holds
/// (`EntityNotFoundException`) was deleted.
///
/// Only those four are reported, because a property the actual document does not carry is not
/// evidence of anything ([`super::drift`]): the configuration is reported as the
/// `TableOptimizerConfiguration` SHAPE, whose members AWS Glue spells in its own lower camel
/// case, so comparing it against the declaration would need the reverse of the re-casing the
/// create does, and a mapping this module would have to invent is worse than no evidence —
/// the position a GuardDuty detector's reported data sources are in (`describe_detector`).
pub(super) async fn describe_glue_table_optimizer(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let request = Value::Object(table_optimizer_address(&resource.properties));
    let answer = match call_glue(ctx, region, "GetTableOptimizer", &request).await {
        Ok(answer) => answer,
        Err(refusal) if refusal.code == GLUE_ENTITY_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let mut actual = Map::new();
    for property in TABLE_OPTIMIZER_ADDRESS
        .iter()
        .filter(|property| **property != TABLE_OPTIMIZER_TYPE_PROPERTY)
    {
        if let Some(reported) = answer.get(*property) {
            actual.insert((*property).to_string(), reported.clone());
        }
    }
    if let Some(reported) = answer
        .get(REPORTED_TABLE_OPTIMIZER)
        .and_then(|optimizer| optimizer.get(REPORTED_TABLE_OPTIMIZER_TYPE))
    {
        actual.insert(
            TABLE_OPTIMIZER_TYPE_PROPERTY.to_string(),
            reported.clone(),
        );
    }
    Ok(Some(Value::Object(actual)))
}

/// The four members that address ONE optimizer, read off a declaration: the catalog, the
/// database, the table and the optimizer's own type, each sent only where the declaration
/// states it — a member it leaves out is that member's own documented default in AWS Glue, or
/// the omission that service's own validation of the required member judges.
fn table_optimizer_address(declared: &Value) -> Map<String, Value> {
    let mut request = Map::new();
    for property in TABLE_OPTIMIZER_ADDRESS {
        if let Some(text) = declared_text(declared, property) {
            request.insert((*property).to_string(), Value::String(text));
        }
    }
    request
}
