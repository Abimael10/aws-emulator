// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Clean Rooms ML's own control plane: the training dataset
//! an `AWS::CleanRoomsML::TrainingDataset` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! A training dataset is the metadata that points at a Glue table: `CreateTrainingDataset`
//! takes the dataset's name, the role Clean Rooms ML reads the data with and the datasets
//! themselves, takes no identifier at all, and answers the `trainingDatasetArn` that service
//! minted — the value this type's `Ref` returns and the handle `GetTrainingDataset` and
//! `DeleteTrainingDataset` each address one by. So the ARN that create answered is the
//! resource's physical id, replacing the stand-in the naming rules computed. The STATUS the
//! dataset carries is that service's too, and the create does not report it: it is read off that
//! service's own report of the dataset the create just made
//! ([`TRAINING_DATASET_STATUS_ATTRIBUTE`]).
//!
//! The type's properties ARE that call's own members once each initial is lower-cased
//! ([`camel_cased`], which walks the nested `TrainingData` the same way: a dataset's `Type` is
//! that member's `type`, its `InputConfig.DataSource.GlueDataSource.DatabaseName` is
//! `inputConfig.dataSource.glueDataSource.databaseName`, and so on down to a column's
//! `ColumnTypes`). The one property that does not travel that way is `Tags`, which the type
//! publishes as the usual `[{Key, Value}]` list while that call takes a tag MAP
//! ([`tags_as_map`]).
//!
//! There is no UPDATE arm: Clean Rooms ML publishes no call that changes a training dataset
//! that exists, so every property of the type is create-only
//! ([`super::template::replacement_properties`]) — its `Tags` included, since no arm calls that
//! service's own tagging API for this type yet — and a declaration restating one replaces the
//! resource instead.

use super::*;

/// Clean Rooms ML's signing name, from its model (`sigv4Name: cleanrooms-ml`), which speaks
/// `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI its own
/// operation's `@http` binding declares, naming the operation so a refusal is reported the way
/// that service's own SDK raises it.
pub(super) const CLEANROOMSML_SIGNING_NAME: &str = "cleanrooms-ml";

/// The collection `CreateTrainingDataset` is bound to (`POST /training-dataset`). ONE dataset
/// is addressed under it by the ARN that service minted
/// (`GET /training-dataset/{trainingDatasetArn}`,
/// `DELETE /training-dataset/{trainingDatasetArn}`).
const TRAINING_DATASET_URI: &str = "/training-dataset";

/// The operations this module calls, as Clean Rooms ML's own model names them.
const CREATE_TRAINING_DATASET: &str = "CreateTrainingDataset";
const GET_TRAINING_DATASET: &str = "GetTrainingDataset";
const DELETE_TRAINING_DATASET: &str = "DeleteTrainingDataset";

/// The error code Clean Rooms ML answers for a training dataset it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read and its delete).
const CLEANROOMSML_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The properties `CreateTrainingDataset` takes as the member of the same name with a
/// lower-cased initial, which is every property of the type but its `Tags`
/// ([`tags_of`] reads those).
const TRAINING_DATASET_PROPERTIES: &[&str] = &["Description", "Name", "RoleArn", "TrainingData"];

/// The member the created dataset's ARN is answered in, which is also the member every later
/// call about one takes it as.
const TRAINING_DATASET_ARN_MEMBER: &str = "trainingDatasetArn";

/// The member the create takes the tag map as.
const TRAINING_DATASET_TAGS_MEMBER: &str = "tags";

/// The member `GetTrainingDataset` reports the dataset's status in, and the attribute the type
/// publishes it as.
///
/// It is a value only Clean Rooms ML can report — `TrainingDatasetStatus` is that service's own
/// enum, and the create answers nothing but the ARN — so it is read off that service's own
/// report of the dataset the create just made, never computed here, exactly as an
/// `AWS::Braket::SpendingLimit`'s `CreatedAt` is ([`super::braket_spending_limits`]).
const TRAINING_DATASET_STATUS_MEMBER: &str = "status";
const TRAINING_DATASET_STATUS_ATTRIBUTE: &str = "Status";

/// The members `GetTrainingDataset` reports under the properties the type declares them as:
/// the dataset's name, the role it reads its data with and its description.
///
/// Its `trainingData` is NOT among them: that service reports the datasets as the shape its own
/// model spells in lower camel case, so comparing it against the declaration would need the
/// reverse of the re-casing the create does, and a mapping this module would have to invent is
/// worse than no evidence — the position an `AWS::Glue::TableOptimizer`'s configuration is in
/// ([`super::glue_table_optimizers`]). A property the actual document does not carry is not
/// evidence of anything ([`super::drift`]).
const REPORTED_TRAINING_DATASET: &[(&str, &str)] = &[
    ("Description", "description"),
    ("Name", "name"),
    ("RoleArn", "roleArn"),
];

/// Create the training dataset an `AWS::CleanRoomsML::TrainingDataset` declares, so a client
/// that deployed the stack reads the dataset back through Clean Rooms ML's own
/// `GetTrainingDataset` and finds it in `ListTrainingDatasets` rather than a
/// `ResourceNotFoundException`.
///
/// The dataset is recorded in the service that OWNS it, so everything the declaration states is
/// resolved by Clean Rooms ML exactly as it is for a client's own call: a name that account and
/// Region already holds a dataset under, and a Glue table whose Amazon S3 location holds
/// nothing to train on, are that service's refusals, which fail the resource and roll the stack
/// back rather than publishing an ARN for a dataset nothing holds.
///
/// The identity is Clean Rooms ML's: the ARN it minted becomes this resource's physical id —
/// the primary identifier of the type, and the handle every training-dataset API takes — and it
/// is reported back beside the status that service holds the dataset in.
pub(super) async fn create_training_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut declared = Map::new();
    for property in TRAINING_DATASET_PROPERTIES {
        if let Some(value) = properties.get(*property) {
            declared.insert((*property).to_string(), value.clone());
        }
    }
    let mut request = match camel_cased(&Value::Object(declared), 0)? {
        Value::Object(members) => members,
        // `camel_cased` answers an object for an object, so nothing reaches this; a request
        // stating no member is refused by that operation's own validation rather than by a
        // panic here.
        _ => Map::new(),
    };
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(
            TRAINING_DATASET_TAGS_MEMBER.to_string(),
            Value::Object(tags),
        );
    }
    let answered = call_rest_json_named(
        ctx,
        CLEANROOMSML_SIGNING_NAME,
        region,
        CREATE_TRAINING_DATASET,
        "POST",
        TRAINING_DATASET_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = reported_text(&answered, TRAINING_DATASET_ARN_MEMBER).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call Clean Rooms ML with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Clean Rooms ML control plane answered a created training dataset \
                      without an ARN"
                .to_string(),
        });
    }
    let mut attributes = BTreeMap::new();
    attributes.insert(
        template::CLEANROOMSML_TRAINING_DATASET_ARN_ATTRIBUTE.to_string(),
        arn.clone(),
    );
    // The status the dataset carries is that service's: read off its own report of the dataset
    // the create just made, never decided here. A report carrying none contributes no attribute
    // rather than an invented one.
    if let Some(status) = reported_status(ctx, region, &arn).await? {
        attributes.insert(TRAINING_DATASET_STATUS_ATTRIBUTE.to_string(), status);
    }
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// The status Clean Rooms ML reports for the training dataset at `training_dataset_arn`, or
/// `None` where its report carries none.
async fn reported_status(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    training_dataset_arn: &str,
) -> Result<Option<String>, Refusal> {
    let answered = call_rest_json_named(
        ctx,
        CLEANROOMSML_SIGNING_NAME,
        region,
        GET_TRAINING_DATASET,
        "GET",
        &training_dataset_uri(training_dataset_arn),
        None,
    )
    .await?;
    Ok(reported_text(&answered, TRAINING_DATASET_STATUS_MEMBER))
}

/// Delete the training dataset a stack created, by the ARN it was created under — this
/// resource's physical id. One that service no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message that carries the
/// request's own values) is already in the state the delete asks for.
pub(super) async fn delete_training_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        CLEANROOMSML_SIGNING_NAME,
        region,
        DELETE_TRAINING_DATASET,
        "DELETE",
        &training_dataset_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == CLEANROOMSML_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The training dataset's configuration as Clean Rooms ML reports it, for drift detection: its
/// own read's answer, under the properties the type declares those members as
/// ([`REPORTED_TRAINING_DATASET`]). A dataset that service no longer holds
/// (`ResourceNotFoundException`) was deleted, which is what `None` reports.
pub(super) async fn describe_training_dataset(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let answered = match call_rest_json_named(
        ctx,
        CLEANROOMSML_SIGNING_NAME,
        region,
        GET_TRAINING_DATASET,
        "GET",
        &training_dataset_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == CLEANROOMSML_RESOURCE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(Some(reported_as_properties(
        &answered,
        REPORTED_TRAINING_DATASET,
    )))
}

/// The URI ONE training dataset is addressed at: the collection, then the
/// `trainingDatasetArn` `@httpLabel` escaped as a single path segment ([`percent_encoded`]) so
/// an ARN's own `:` and `/` address one dataset rather than another path.
fn training_dataset_uri(training_dataset_arn: &str) -> String {
    format!(
        "{TRAINING_DATASET_URI}/{}",
        percent_encoded(training_dataset_arn)
    )
}
