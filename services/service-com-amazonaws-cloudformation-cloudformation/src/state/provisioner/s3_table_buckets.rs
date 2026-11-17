// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon S3 Tables: the `AWS::S3Tables::TableBucket` a
//! stack creates there, and the `AWS::S3Tables::Namespace` it creates INSIDE one. One file
//! of [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! This is the THIRD bucket variant, beside Amazon S3's own bucket
//! ([`super::data_and_storage`]) and the Amazon S3 Vectors vector bucket
//! ([`super::network_and_delivery`]), and the three differ in exactly one thing that matters
//! to a client reading one out of a stack: the identifier their owning service accepts. S3
//! takes a bucket NAME, while S3 Tables and S3 Vectors each key their bucket by its own ARN
//! — which is why this type's `Ref` and physical resource id are that ARN
//! ([`super::template::return_values`]) and why every call below addresses the bucket with
//! it.

use super::*;

/// Amazon S3 Tables' signing name, from its model (`sigv4Name: s3tables`, service shape
/// `S3TableBuckets`), which speaks `restJson1` — so its calls ride on [`call_rest_json`],
/// each on the URI its own operation's `@http` binding declares.
pub(super) const S3TABLES_SIGNING_NAME: &str = "s3tables";

/// The URI `CreateTableBucket` is bound to (`PUT /buckets`), from that model's `@http`
/// trait.
pub(super) const S3TABLES_BUCKETS_URI: &str = "/buckets";

/// The error code S3 Tables answers for a table bucket it does not hold
/// (`NotFoundException`, the modelled 404 of its reads and its deletes).
pub(super) const S3TABLES_NOT_FOUND: &str = "NotFoundException";

/// Every property `AWS::S3Tables::TableBucket` publishes that this arm carries to Amazon S3
/// Tables: the bucket's name, which is `CreateTableBucket`'s own `name` member, the
/// encryption settings that call's `encryptionConfiguration` takes, the tag list its `tags`
/// map takes, and the unreferenced-file-removal maintenance settings, which are no member of
/// that create at all but a `PutTableBucketMaintenanceConfiguration` of their own
/// ([`put_unreferenced_file_removal`]).
///
/// A declaration naming anything else is REFUSED ([`refuse_unmapped_properties`]) rather than
/// deployed with the property dropped, so a stack never reports a table bucket configured
/// differently from its declaration.
pub(super) const S3TABLES_TABLE_BUCKET_PROPERTIES: &[&str] = &[
    "TableBucketName",
    "EncryptionConfiguration",
    "Tags",
    S3TABLES_UNREFERENCED_FILE_REMOVAL,
];

/// The property `AWS::S3Tables::TableBucket` publishes a table bucket's unreferenced-file
/// removal settings under, which is the one property of the type that reaches Amazon S3
/// Tables through a call of its OWN rather than through the create's members.
pub(super) const S3TABLES_UNREFERENCED_FILE_REMOVAL: &str = "UnreferencedFileRemoval";

/// The maintenance TASK those settings configure, as `TableBucketMaintenanceType` spells its
/// one value — the `{type}` label of `PutTableBucketMaintenanceConfiguration`'s URI
/// (`PUT /buckets/{tableBucketARN}/maintenance/{type}`) and the key the read answers the
/// task's configuration under.
const S3TABLES_UNREFERENCED_FILE_REMOVAL_TYPE: &str = "icebergUnreferencedFileRemoval";

/// The segment that put's URI carries between the bucket and the task.
const S3TABLES_MAINTENANCE_SEGMENT: &str = "maintenance";

/// The operation that call is, as Amazon S3 Tables' own model names it.
const S3TABLES_PUT_MAINTENANCE_CONFIGURATION: &str = "PutTableBucketMaintenanceConfiguration";

/// The two day counts of that property against the members
/// `IcebergUnreferencedFileRemovalSettings` spells them with: CloudFormation publishes
/// `UnreferencedDays` and `NoncurrentDays`, while that structure's own members are
/// `unreferencedDays` and `nonCurrentDays`
/// (`smithy_query(query="shape", shape="IcebergUnreferencedFileRemovalSettings")` on that
/// service's model).
const S3TABLES_UNREFERENCED_FILE_REMOVAL_DAYS: &[(&str, &str)] = &[
    ("UnreferencedDays", "unreferencedDays"),
    ("NoncurrentDays", "nonCurrentDays"),
];

/// The property that declaration states the task's switch as, and the member
/// `TableBucketMaintenanceConfigurationValue` takes it as, beside the member that structure's
/// settings travel in.
const S3TABLES_MAINTENANCE_STATUS: &str = "Status";
const S3TABLES_MAINTENANCE_STATUS_MEMBER: &str = "status";
const S3TABLES_MAINTENANCE_SETTINGS_MEMBER: &str = "settings";

/// The member that put takes the whole configuration as (`value`, `@required`).
const S3TABLES_MAINTENANCE_VALUE_MEMBER: &str = "value";

/// The values `MaintenanceStatus` takes on the wire, from that model's own enum
/// (`smithy_query(query="shape", shape="MaintenanceStatus")`: `enabled`, `disabled`).
///
/// `AWS::S3Tables::TableBucket` publishes the same switch with the upper-case initial
/// CloudFormation spells an enum value with, so a declared status is carried as the value the
/// OWNING model names ([`maintenance_status`]).
const S3TABLES_MAINTENANCE_STATUS_VALUES: &[&str] = &["enabled", "disabled"];

/// The members of an `AWS::S3Tables::TableBucket`'s `EncryptionConfiguration` as
/// `CreateTableBucket` spells them: that type publishes the algorithm and the key with the
/// upper-case initial and acronym CloudFormation spells members with, while the API's own
/// `EncryptionConfiguration` has `sseAlgorithm` and `kmsKeyArn`
/// (`smithy_query(query="shape", shape="EncryptionConfiguration")` on that service's model).
pub(super) const S3TABLES_ENCRYPTION_MEMBERS: &[(&str, &str)] = &[
    ("SSEAlgorithm", "sseAlgorithm"),
    ("KMSKeyArn", "kmsKeyArn"),
];

/// Create the table bucket an `AWS::S3Tables::TableBucket` declares, so a client that
/// deployed the stack and then calls `s3tables GetTableBucket` with the ARN the stack
/// publishes reads the bucket back rather than a refusal.
///
/// `Ref` of this type returns the bucket's ARN, which is this resource's physical id, and
/// such an ARN is a total function of the Region and the bucket's NAME — so the bucket is
/// created under the name CloudFormation decided ([`table_bucket_name`]) and the ARN S3
/// Tables answers with is the one the stack already computed. It is reported back as the
/// resource's `TableBucketARN` attribute all the same, so the handle the stack publishes is
/// the one that service really minted.
///
/// The type's remaining properties are that create's own members: the
/// `EncryptionConfiguration`, whose two members S3 Tables spells in lower camel case
/// ([`S3TABLES_ENCRYPTION_MEMBERS`]), and the tags — the ones the declaration asks for plus
/// the ones CloudFormation gives the resource ([`tags_of`]), as the MAP that request takes,
/// so `s3tables ListTagsForResource` on the ARN the stack publishes answers with the
/// stack's own tags.
///
/// The one property that create takes no member for is the `UnreferencedFileRemoval`
/// maintenance block, which S3 Tables writes through a put of its own
/// ([`put_unreferenced_file_removal`]) — made right after the create, so the bucket a
/// completed stack publishes runs the unreferenced file removal its declaration asked for.
pub(super) async fn create_table_bucket(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, S3TABLES_TABLE_BUCKET_PROPERTIES)?;
    let mut request = Map::new();
    request.insert(
        "name".to_string(),
        Value::String(table_bucket_name(resource)),
    );
    if let Some(configuration) = resource.properties.get("EncryptionConfiguration") {
        request.insert(
            "encryptionConfiguration".to_string(),
            mapped_members(configuration, S3TABLES_ENCRYPTION_MEMBERS),
        );
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        "CreateTableBucket",
        "PUT",
        S3TABLES_BUCKETS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = answered
        .get("arn")
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .ok_or_else(|| Refusal {
            code: "InternalFailure".to_string(),
            message: "Amazon S3 Tables answered CreateTableBucket without an arn".to_string(),
        })?;
    if let Some(declared) = resource.properties.get(S3TABLES_UNREFERENCED_FILE_REMOVAL) {
        put_unreferenced_file_removal(ctx, region, arn, declared).await?;
    }
    let mut attributes = BTreeMap::new();
    attributes.insert(
        S3TABLES_TABLE_BUCKET_ARN_ATTRIBUTE.to_string(),
        arn.to_string(),
    );
    Ok(Created::attributes(attributes))
}

/// Apply a CHANGED `AWS::S3Tables::TableBucket` declaration to the table bucket the stack
/// already has, through the one call Amazon S3 Tables publishes for a property of a bucket
/// that exists.
///
/// `PutTableBucketMaintenanceConfiguration` "creates a new maintenance configuration or
/// replaces an existing maintenance configuration for a table bucket", so a changed
/// `UnreferencedFileRemoval` — another status, another day count — is applied to the bucket
/// the stack created rather than replacing it: the resource keeps the ARN that is its physical
/// id and the bucket keeps everything it holds.
///
/// The bucket's NAME is its identity, which a change to replaces the resource before this is
/// reached. A declaration that states any OTHER property differently, or that drops the
/// maintenance block S3 Tables publishes no call to remove, is reported as a change that was
/// NOT applied rather than as a success that dropped it.
pub(super) async fn update_table_bucket(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if table_bucket_fixed_properties(&resource.properties)
        != table_bucket_fixed_properties(declared)
    {
        return Ok(None);
    }
    let held = resource.properties.get(S3TABLES_UNREFERENCED_FILE_REMOVAL);
    let now = declared.get(S3TABLES_UNREFERENCED_FILE_REMOVAL);
    if held == now {
        return Ok(Some(Created::default()));
    }
    let Some(now) = now else {
        return Ok(None);
    };
    put_unreferenced_file_removal(ctx, region, &resource.physical_resource_id, now).await?;
    Ok(Some(Created::default()))
}

/// One table bucket declaration less the property a call of its own carries: the half of it no
/// Amazon S3 Tables call can change on a bucket that already exists, so a declaration that
/// states one of these differently is reported as not applied.
fn table_bucket_fixed_properties(properties: &Value) -> Map<String, Value> {
    match properties {
        Value::Object(object) => object
            .iter()
            .filter(|(property, _)| property.as_str() != S3TABLES_UNREFERENCED_FILE_REMOVAL)
            .map(|(property, value)| (property.clone(), value.clone()))
            .collect(),
        _ => Map::new(),
    }
}

/// Write the unreferenced-file-removal maintenance configuration a declaration states onto the
/// table bucket of `table_bucket_arn`, through the put Amazon S3 Tables publishes for it.
///
/// That one call "creates a new maintenance configuration or replaces an existing maintenance
/// configuration for a table bucket", so it is this property's path at CREATE and at UPDATE
/// alike — and the configuration a later `GetTableBucketMaintenanceConfiguration` reports for
/// the task is exactly what the declaration asked for.
async fn put_unreferenced_file_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    table_bucket_arn: &str,
    declared: &Value,
) -> Result<(), Refusal> {
    let value = unreferenced_file_removal_value(declared)?;
    let uri = format!(
        "{}/{S3TABLES_MAINTENANCE_SEGMENT}/{S3TABLES_UNREFERENCED_FILE_REMOVAL_TYPE}",
        table_bucket_uri(table_bucket_arn)
    );
    call_rest_json_named(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        S3TABLES_PUT_MAINTENANCE_CONFIGURATION,
        "PUT",
        &uri,
        Some(&serde_json::json!({ S3TABLES_MAINTENANCE_VALUE_MEMBER: value })),
    )
    .await
    .map(|_answered| ())
}

/// The `TableBucketMaintenanceConfigurationValue` an `UnreferencedFileRemoval` declaration
/// states: the task's `status`, and its `settings` — the `TableBucketMaintenanceSettings`
/// union, whose one member is the task itself and carries the two day counts the declaration
/// spells with an upper-case initial.
///
/// A member the declaration leaves out is not sent, so the task runs it under the setting S3
/// Tables itself decides; a declaration that is no structure at all states no configuration
/// this property can carry and is refused rather than deployed as an empty one.
fn unreferenced_file_removal_value(declared: &Value) -> Result<Value, Refusal> {
    let Some(members) = declared.as_object() else {
        return Err(Refusal {
            code: "ValidationException".to_string(),
            message: format!(
                "the {property} property of {resource_type} states a {status}, an {unreferenced} \
                 and a {non_current}",
                property = S3TABLES_UNREFERENCED_FILE_REMOVAL,
                resource_type = S3TABLES_TABLE_BUCKET,
                status = S3TABLES_MAINTENANCE_STATUS,
                unreferenced = S3TABLES_UNREFERENCED_FILE_REMOVAL_DAYS[0].0,
                non_current = S3TABLES_UNREFERENCED_FILE_REMOVAL_DAYS[1].0,
            ),
        });
    };
    let mut settings = Map::new();
    for (property, member) in S3TABLES_UNREFERENCED_FILE_REMOVAL_DAYS {
        if let Some(days) = members.get(*property) {
            // Each day count is an INTEGER member of that settings structure, so a template
            // that states one as the text a parameter reference resolves to carries the number
            // itself; a value that is no number travels as written, and that service's own
            // refusal of it is what the stack reports.
            settings.insert(
                (*member).to_string(),
                declared_number(days).unwrap_or_else(|| days.clone()),
            );
        }
    }
    let mut value = Map::new();
    if let Some(status) = members.get(S3TABLES_MAINTENANCE_STATUS) {
        value.insert(
            S3TABLES_MAINTENANCE_STATUS_MEMBER.to_string(),
            Value::String(maintenance_status(&attribute_text(status))),
        );
    }
    if !settings.is_empty() {
        value.insert(
            S3TABLES_MAINTENANCE_SETTINGS_MEMBER.to_string(),
            serde_json::json!({
                S3TABLES_UNREFERENCED_FILE_REMOVAL_TYPE: Value::Object(settings),
            }),
        );
    }
    Ok(Value::Object(value))
}

/// A declared maintenance status as the OWNING service's model spells it: that enum's values
/// and the ones `AWS::S3Tables::TableBucket` publishes differ in their CASE alone (`Enabled`
/// against `enabled`), so a declaration naming one of them is carried as the value the model
/// names ([`S3TABLES_MAINTENANCE_STATUS_VALUES`]).
///
/// A declaration naming anything else travels as written rather than as a value invented here,
/// so what the stack reports is Amazon S3 Tables' own refusal of the status the template really
/// asked for.
fn maintenance_status(declared: &str) -> String {
    S3TABLES_MAINTENANCE_STATUS_VALUES
        .iter()
        .find(|value| value.eq_ignore_ascii_case(declared))
        .map_or_else(|| declared.to_string(), |value| (*value).to_string())
}

/// Delete the table bucket a stack created, addressed by its ARN — this resource's physical
/// id, which is what `Ref` returns for the type. A bucket S3 Tables no longer holds
/// (`NotFoundException`, judged on the CODE and never on a message that carries the
/// request's own name) is already in the state the delete asks for; one that still holds
/// namespaces is NOT deleted (`ConflictException`), and that refusal is what the stack
/// reports.
pub(super) async fn delete_table_bucket(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        "DELETE",
        &table_bucket_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == S3TABLES_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The table bucket's configuration as Amazon S3 Tables reports it, for drift detection:
/// `GetTableBucket`'s own record, read by the ARN that is this resource's physical id. One
/// it no longer holds (`NotFoundException`) was deleted.
///
/// The one property its read reports under the name this type declares is the bucket's
/// `TableBucketName` — that read answers the bucket's `name` — and a property the read does
/// not report is not evidence of anything ([`super::drift`]).
pub(super) async fn describe_table_bucket(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        "GET",
        &table_bucket_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(answer) => Ok(Some(reported_as_properties(
            &answer,
            &[("TableBucketName", "name")],
        ))),
        Err(refusal) if refusal.code == S3TABLES_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI an operation on ONE table bucket is bound to (`/buckets/{tableBucketARN}`), with
/// the ARN written as the single path segment that `@httpLabel` is: its `:` and `/` travel
/// percent-encoded ([`percent_encoded`]), or the request would address a different route.
fn table_bucket_uri(table_bucket_arn: &str) -> String {
    format!(
        "{S3TABLES_BUCKETS_URI}/{}",
        percent_encoded(table_bucket_arn)
    )
}

/// The NAME of the table bucket a resource record describes: the segment its own ARN
/// carries, which is this type's physical id and how S3 Tables keys a table bucket.
///
/// A record whose physical id is NOT such an ARN is left naming itself, so what the stack
/// reports is S3 Tables' own refusal of the name the declaration really asked for rather
/// than one invented here.
pub(super) fn table_bucket_name(resource: &StoredResource) -> String {
    arn_bucket_of(&resource.physical_resource_id, S3TABLES_BUCKET_SEGMENT)
        .unwrap_or_else(|| resource.physical_resource_id.clone())
}

// ---------------------------------------------------------------------------
// Amazon S3 Tables: AWS::S3Tables::Namespace
// ---------------------------------------------------------------------------

/// The URI `CreateNamespace` is bound to (`PUT /namespaces/{tableBucketARN}`), from that
/// model's `@http` trait — the same URI `ListNamespaces` reads the bucket's namespaces off.
const S3TABLES_NAMESPACES_URI: &str = "/namespaces";

/// The operation that create is, as Amazon S3 Tables' own model names it.
const S3TABLES_CREATE_NAMESPACE: &str = "CreateNamespace";

/// The member `CreateNamespace` takes the namespace's name as: a `NamespaceList`
/// (`@length(min: 1, max: 1)`), so one name travels as a one-element list — and the same
/// member is what `GetNamespace` answers it under.
const S3TABLES_NAMESPACE_MEMBER: &str = "namespace";

/// Create the namespace an `AWS::S3Tables::Namespace` declares, in the table bucket its
/// `TableBucketARN` names — so a client that deployed the stack and then calls
/// `s3tables ListNamespaces` on that bucket, or `CreateTable` in that namespace, reaches the
/// namespace the stack reported created rather than a `NotFoundException` for one nothing
/// made.
///
/// Both of the type's properties ARE its identity ([`S3TABLES_NAMESPACE_IDENTITY`]): the
/// bucket's ARN, which is what that service keys a table bucket by, and the namespace's own
/// name, which `CreateNamespace` takes as the one-element `NamespaceList` its model declares.
/// The type publishes no attribute of its own and its physical id is that pair
/// ([`super::template::composite_identity_properties`]), so the create reports neither back.
///
/// A declaration naming any other property is REFUSED ([`refuse_unmapped_properties`]) rather
/// than deployed with the property dropped.
pub(super) async fn create_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, S3TABLES_NAMESPACE_IDENTITY)?;
    let (table_bucket_arn, namespace) = namespace_identity(resource)?;
    let answered = call_rest_json_named(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        S3TABLES_CREATE_NAMESPACE,
        "PUT",
        &namespaces_uri(&table_bucket_arn),
        Some(&serde_json::json!({
            S3TABLES_NAMESPACE_MEMBER: [namespace.clone()],
        })),
    )
    .await?;
    // That create echoes the namespace it made. A resource is reported created only when the
    // owning service really made THAT namespace, so an answer carrying another one — or none
    // — is a failure rather than a success the stack records.
    if answered_namespace(&answered).as_deref() != Some(namespace.as_str()) {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "Amazon S3 Tables answered {S3TABLES_CREATE_NAMESPACE} without the namespace \
                 {namespace}"
            ),
        });
    }
    Ok(Created::default())
}

/// Delete the namespace a stack created, addressed by the pair Amazon S3 Tables keys one by:
/// the table bucket it lives in and its own name, which together are this resource's physical
/// id.
///
/// A namespace that service no longer holds — and a table bucket it no longer holds, which
/// took the namespace with it — answers `NotFoundException`, judged on the CODE and never on
/// a message that carries the request's own name, and is already in the state the delete asks
/// for. A namespace that still holds tables is NOT deleted (`ConflictException`), and that
/// refusal is what the stack reports.
pub(super) async fn delete_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let (table_bucket_arn, namespace) = namespace_identity(resource)?;
    match call_rest_json(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        "DELETE",
        &namespace_uri(&table_bucket_arn, &namespace),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == S3TABLES_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The namespace's configuration as Amazon S3 Tables reports it, for drift detection:
/// `GetNamespace`'s own record, read by the same pair the delete uses. One it no longer holds
/// (`NotFoundException`) was deleted.
///
/// The one property its read reports under a name this type declares is the namespace's own
/// (`namespace`, answered as the one-element list the model declares, so the name itself is
/// what is reported); the bucket is the handle the read was ADDRESSED by rather than
/// something the answer carries, and a property the read does not report is not evidence of
/// anything ([`super::drift`]).
pub(super) async fn describe_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let (table_bucket_arn, namespace) = namespace_identity(resource)?;
    match call_rest_json(
        ctx,
        S3TABLES_SIGNING_NAME,
        region,
        "GET",
        &namespace_uri(&table_bucket_arn, &namespace),
        None,
    )
    .await
    {
        Ok(answer) => {
            let mut reported = Map::new();
            if let Some(name) = answered_namespace(&answer) {
                reported.insert(
                    S3TABLES_NAMESPACE_PROPERTY.to_string(),
                    Value::String(name),
                );
            }
            Ok(Some(Value::Object(reported)))
        }
        Err(refusal) if refusal.code == S3TABLES_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The namespace an answer of that service carries: the single element of the `NamespaceList`
/// both `CreateNamespace` and `GetNamespace` answer it as, or `None` for an answer that
/// carries no one namespace.
fn answered_namespace(answered: &Value) -> Option<String> {
    match answered.get(S3TABLES_NAMESPACE_MEMBER)?.as_array()?.as_slice() {
        [only] => only.as_str().map(ToString::to_string),
        _ => None,
    }
}

/// The pair that identifies one namespace: the table bucket holding it and its own name, both
/// of them required properties of the type and both read off the resource's own declaration.
///
/// A declaration missing either names no namespace that could be created or addressed, so it
/// is refused here rather than with a request Amazon S3 Tables would reject.
fn namespace_identity(resource: &StoredResource) -> Result<(String, String), Refusal> {
    Ok((
        required_property(resource, S3TABLES_TABLE_BUCKET_ARN_ATTRIBUTE)?,
        required_property(resource, S3TABLES_NAMESPACE_PROPERTY)?,
    ))
}

/// The URI the namespaces OF one table bucket are bound to
/// (`/namespaces/{tableBucketARN}`), with the ARN written as the single path segment that
/// `@httpLabel` is ([`percent_encoded`]).
fn namespaces_uri(table_bucket_arn: &str) -> String {
    format!(
        "{S3TABLES_NAMESPACES_URI}/{}",
        percent_encoded(table_bucket_arn)
    )
}

/// The URI that addresses ONE namespace (`/namespaces/{tableBucketARN}/{namespace}`): the two
/// `@httpLabel`s of every operation on one, each percent-encoded, as a non-greedy label is
/// sent — a table bucket ARN carries both `:` and `/`, so it reaches that service as one
/// value rather than as extra URI structure.
fn namespace_uri(table_bucket_arn: &str, namespace: &str) -> String {
    format!(
        "{}/{}",
        namespaces_uri(table_bucket_arn),
        percent_encoded(namespace)
    )
}
