// SPDX-License-Identifier: CC0-1.0

//! `AWS::DynamoDB::Table`, as it lives in the Amazon DynamoDB service's own store.
//!
//! A table is a resource of the Amazon DynamoDB service: `dynamodb:CreateTable` creates it,
//! `dynamodb:DescribeTable` and `dynamodb:ListTables` report it, `dynamodb:UpdateTable` and
//! the tagging calls change it and `dynamodb:DeleteTable` removes it, and the record of it —
//! with the items it holds — lives in that service's state in the same shared engine. So,
//! exactly as for a queue ([`super::queues`]) or a parameter ([`super::parameters`]), this
//! provider holds a handle to THAT store and nothing of its own: a table this emulator holds
//! is one Cloud Control API resolves, whichever API created it, and a table Amazon DynamoDB
//! no longer holds is one Cloud Control API reports as not found.
//!
//! Tables are regional — "table names must be unique within each Region" (the `CreateTable`
//! doc), and the owning store holds one namespace per Region — so every answer here is about
//! the Region the call was made in.
//!
//! The resource model is the type's registry schema. Catalog source: the
//! `AWS::DynamoDB::Table` resource provider schema's `properties`, whose `primaryIdentifier`
//! is `/properties/TableName` and whose `readOnlyProperties` are `/properties/Arn` and
//! `/properties/StreamArn`. The properties this path APPLIES are the identity, the primary
//! key the table is defined over and the settings the owning record carries on its own:
//!
//! * `TableName`, the table's own name, which is its primary identifier and which the schema
//!   declares create-only;
//! * `KeySchema` and `AttributeDefinitions`, the primary key and the attributes it is
//!   declared over, held to the OWNING service's own rules for a declared table
//!   ([`ddb_create::validated_key_schema`]), so nothing is created here that service's own
//!   `CreateTable` would refuse;
//! * `BillingMode` and `ProvisionedThroughput`, the capacity mode and the capacity a
//!   provisioned table holds, resolved by that service's own rule for the pair
//!   ([`ddb_create::validated_declared_throughput`]) — a CREATE settles the mode, while a
//!   patch may only re-state the mode in force, because switching it is an `UpdateTable` the
//!   owning service applies with a capacity move and a span nothing here drives;
//! * `TableClass` and `DeletionProtectionEnabled`, two settings the owning record carries
//!   and every description of the table reports;
//! * `Tags`, as a list of `{ Key, Value }` pairs, written through the owning service's own
//!   tag set — which propagates as that service's tag writes do, so a read reports the set
//!   its own tag reader can see yet;
//! * the read-only `Arn` and `StreamArn`, which a read reports beside them: the table's own
//!   ARN, and the ARN of the latest stream the streams registry minted for it.
//!
//! A read also reports the `GlobalSecondaryIndexes`, `LocalSecondaryIndexes` and
//! `StreamSpecification` the table holds, because the model a read answers describes the
//! resource as the owning service holds it. Applying one of them is work THIS path does not
//! perform ([`UNAPPLIED_PROPERTIES`]) — an index has a lifecycle of its own, a stream is a
//! resource of the DynamoDB Streams service, encryption resolves a KMS key, an import reads
//! objects out of a bucket, and continuous backups, contributor insights, Time to Live, a
//! Kinesis destination, a resource policy and the on-demand and warm capacity ceilings are
//! each settled by a call of their own — so a model may CARRY such a property only with the
//! value the table already reports for it, which is what lets a client round-trip a model it
//! read back and patch something else on it.
//!
//! A table's items are untouched by everything here: this type's schema declares no item
//! data, so a resource operation of this service never writes one.

use ::std::time::SystemTime;

use ::serde_json::{Map, Value};
use ::service_com_amazonaws_dynamodb_dynamodb_20120810::ops::create_table as ddb_create;
use ::service_com_amazonaws_dynamodb_dynamodb_20120810::ops::types::{
    AttributeDefinition, KeySchemaElement, Projection, ProvisionedThroughput, Tag,
    BILLING_MODE_VALUES, KEY_TYPE_VALUES, SCALAR_ATTRIBUTE_TYPE_VALUES, TABLE_CLASS_VALUES,
};
use ::service_com_amazonaws_dynamodb_dynamodb_20120810::state as ddb;

use super::{
    require_reported, require_value, tag_entries, tag_list, DynamodbState, NativeRefusal,
    NativeResourceProvider, TAGS,
};

/// The `TypeName` this provider stands behind.
pub const TYPE_NAME: &str = "AWS::DynamoDB::Table";

/// The property that carries the type's primary identifier, which the schema's
/// `primaryIdentifier` points at (`/properties/TableName`).
const IDENTIFIER_PROPERTY: &str = "TableName";

/// The table's own ARN (`Arn`), which the owning service builds from the Region and the name
/// and which no caller supplies: one of the type's two read-only properties.
const ARN: &str = "Arn";

/// The ARN of the latest stream of the table (`StreamArn`), the type's other read-only
/// property — minted by the DynamoDB Streams service for the table's stream.
const STREAM_ARN: &str = "StreamArn";

/// The table's primary key (`KeySchema`), which the schema declares required and create-only.
const KEY_SCHEMA: &str = "KeySchema";

/// The attributes the table's keys are declared over (`AttributeDefinitions`).
const ATTRIBUTE_DEFINITIONS: &str = "AttributeDefinitions";

/// The table's read/write capacity mode (`BillingMode`).
const BILLING_MODE: &str = "BillingMode";

/// The capacity a provisioned table holds (`ProvisionedThroughput`).
const PROVISIONED_THROUGHPUT: &str = "ProvisionedThroughput";

/// The storage class the table is in (`TableClass`).
const TABLE_CLASS: &str = "TableClass";

/// Whether the table is protected against deletion (`DeletionProtectionEnabled`).
const DELETION_PROTECTION_ENABLED: &str = "DeletionProtectionEnabled";

/// The global secondary indexes the table carries (`GlobalSecondaryIndexes`).
const GLOBAL_SECONDARY_INDEXES: &str = "GlobalSecondaryIndexes";

/// The local secondary indexes the table carries (`LocalSecondaryIndexes`).
const LOCAL_SECONDARY_INDEXES: &str = "LocalSecondaryIndexes";

/// The stream the table publishes its item changes to (`StreamSpecification`).
const STREAM_SPECIFICATION: &str = "StreamSpecification";

/// The name of one index (`IndexName`), inside the two index lists.
const INDEX_NAME: &str = "IndexName";

/// The attribute one key schema element or attribute definition names (`AttributeName`).
const ATTRIBUTE_NAME: &str = "AttributeName";

/// The type one attribute definition declares (`AttributeType`).
const ATTRIBUTE_TYPE: &str = "AttributeType";

/// The role one key schema element gives its attribute (`KeyType`).
const KEY_TYPE: &str = "KeyType";

/// What one index projects of the table's items (`Projection`).
const PROJECTION: &str = "Projection";

/// Which attributes that projection covers (`ProjectionType`).
const PROJECTION_TYPE: &str = "ProjectionType";

/// The attributes an `INCLUDE` projection names (`NonKeyAttributes`).
const NON_KEY_ATTRIBUTES: &str = "NonKeyAttributes";

/// The read half of a capacity pair (`ReadCapacityUnits`).
const READ_CAPACITY_UNITS: &str = "ReadCapacityUnits";

/// The write half of a capacity pair (`WriteCapacityUnits`).
const WRITE_CAPACITY_UNITS: &str = "WriteCapacityUnits";

/// What a stream captures of each change (`StreamViewType`).
const STREAM_VIEW_TYPE: &str = "StreamViewType";

/// The properties of the type's schema THIS path does not apply — see this module's header.
///
/// Named here so a model asking for one is told it is a property of the type this handler
/// does not apply, rather than one the type does not declare at all: the two are different
/// faults and a caller fixes them differently. A model may still carry one with the value
/// the table already reports for it ([`require_reported`]).
const UNAPPLIED_PROPERTIES: &[&str] = &[
    "ContributorInsightsSpecification",
    GLOBAL_SECONDARY_INDEXES,
    "ImportSourceSpecification",
    "KinesisStreamSpecification",
    LOCAL_SECONDARY_INDEXES,
    "OnDemandThroughput",
    "PointInTimeRecoverySpecification",
    "ResourcePolicy",
    "SSESpecification",
    STREAM_SPECIFICATION,
    "TimeToLiveSpecification",
    "WarmThroughput",
];

/// The Amazon DynamoDB tables of one engine, as Cloud Control API resources.
pub struct TableProvider {
    /// The owning service's store. A handle, so every answer is read from the live store.
    state: ::engine::ServiceState<DynamodbState>,
    /// The DynamoDB Streams store, where the stream a table publishes to lives, or `None`
    /// for a deployment whose engine carries none.
    ///
    /// A table's stream is a resource of THAT service, reached through the OWNING service's
    /// own seam ([`ddb::streams_state`]), so a removal here hands the stream back exactly as
    /// that service's own delete does ([`ddb::hand_back_table_stream`]).
    streams: Option<::engine::ServiceState<ddb::StreamsState>>,
}

impl TableProvider {
    /// The provider for the Amazon DynamoDB store THIS engine carries, or `None` when it
    /// carries none.
    pub fn for_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        ddb::service_state(engine).map(|state| Self {
            state,
            streams: ddb::streams_state(engine),
        })
    }
}

impl NativeResourceProvider for TableProvider {
    fn type_name(&self) -> &'static str {
        TYPE_NAME
    }

    fn identifier_property(&self) -> &'static str {
        IDENTIFIER_PROPERTY
    }

    /// The name the table a create describes will answer to.
    ///
    /// The type's schema declares `TableName` create-only but not required, so a model may
    /// name no table: "if you don't specify a name, CloudFormation generates a unique
    /// physical ID", which is the name this picks for it ([`super::generated_name`]) — a
    /// table name carries no suffix any kind of table requires, so the generated one is the
    /// bare minted token. A model that NAMES the table names its own identifier, and a
    /// model whose `TableName` could not be an identifier at all names none, which is a
    /// fault in the desired state rather than a resource to provision.
    fn assigned_identifier(&self, region: &str, properties: &Value) -> Option<String> {
        let _ = region;
        match properties.get(IDENTIFIER_PROPERTY) {
            Some(named) => crate::state::identifier_text(named),
            None => Some(super::generated_name(false, "")),
        }
    }

    fn read(&self, region: &str, identifier: &str) -> Option<Value> {
        let name = table_name(identifier)?;
        let now = SystemTime::now();
        let mut store = self.state.lock();
        // What the owning service's clock already owes its store: a table whose deletion has
        // fallen due is gone, and a tag write whose propagation span has elapsed is visible.
        // Materialising that is what keeps this read and that service's own reads from
        // disagreeing about what exists.
        store.apply_due_transitions(now);
        store.table(region, &name).ok().map(model)
    }

    fn list(&self, region: &str, after: Option<&str>, limit: usize) -> Vec<(String, Value)> {
        let now = SystemTime::now();
        let mut store = self.state.lock();
        store.apply_due_transitions(now);
        // The owning service's own reader of one Region's tables answers in ascending name
        // order (`ListTables`), and a table's name IS this type's primary identifier, so
        // that order is the order a listing pages over: the window is that order resumed
        // after `after`, and the per-Region table quota is what bounds the names behind it.
        let window: Vec<String> = store
            .table_names_in(region)
            .into_iter()
            .filter(|name| after.is_none_or(|after| name.as_str() > after))
            .take(limit)
            .collect();
        window
            .into_iter()
            .filter_map(|name| {
                let rendered = model(store.table(region, &name).ok()?);
                Some((name, rendered))
            })
            .collect()
    }

    fn create(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let name = named(identifier)?;
        let arn = ddb::table_arn(region, &name);
        let desired = DesiredModel::read(
            properties,
            &Identity {
                name: &name,
                arn: &arn,
                reported: None,
            },
            Intent::Create,
        )?;
        // The primary key, the attributes it is declared over and the capacity the table is
        // created with are the OWNING service's to accept: its own rules for a declared
        // table are what refuse a key schema nothing defines, an attribute no schema uses,
        // and a capacity pair that does not hold with the mode asked for.
        let declared = ddb_create::DeclaredTable {
            key_schema: &desired.key_schema,
            attribute_definitions: &desired.attribute_definitions,
            // An index is declared by a property this path does not apply, so a table
            // created here carries none.
            global_secondary_indexes: &[],
            local_secondary_indexes: &[],
            vector_indexes: &[],
        };
        declared.clamped().map_err(refusal)?;
        let key_schema = ddb_create::validated_key_schema(&declared).map_err(refusal)?;
        let (billing_mode, provisioned_throughput) = ddb_create::validated_declared_throughput(
            desired.billing_mode.as_deref(),
            None,
            desired.provisioned_throughput.as_ref(),
        )
        .map_err(refusal)?;
        ddb::check_capacity_bounds(provisioned_throughput.as_ref()).map_err(refusal)?;
        let tags = desired.tag_set()?;

        let mut store = self.state.lock();
        store.apply_due_transitions(at);
        // The table is created in the owning service's store under its own rules: a name
        // that Region already holds is that service's own already-exists verdict, and its
        // per-Region quotas bound what this create may add.
        if store.contains_table(region, &name) {
            return Err(NativeRefusal::AlreadyExists);
        }
        if store.table_count_in(region) >= ddb::MAX_TABLES {
            return Err(NativeRefusal::AtCapacity(format!(
                "Subscriber limit exceeded: this account already holds the maximum of {} \
                 tables in {region}",
                ddb::MAX_TABLES
            )));
        }
        if store.region_bucket_exhausted(region) {
            return Err(NativeRefusal::AtCapacity(format!(
                "Subscriber limit exceeded: this emulator holds tables for at most {} regions, \
                 and {region} is not one of them",
                ddb::MAX_REGIONS
            )));
        }
        // Minted by the OWNING service from the decisions this create carries
        // ([`ddb::mint_table`]), so the record this store holds — its id, its ARN, the
        // budgets its first request is admitted against, the counters its description
        // reports — is the one that service's own `CreateTable` leaves behind.
        let table = ddb::mint_table(ddb::MintedTable {
            region: region.to_string(),
            name: name.clone(),
            key_schema,
            attribute_definitions: desired.attribute_definitions.clone(),
            warm_throughput: ddb::WarmThroughputSettings::tracking(
                provisioned_throughput.as_ref(),
                at,
            ),
            billing_mode,
            provisioned_throughput,
            // An index, a stream, encryption, a policy and an on-demand ceiling are each
            // asked for by a property this path does not apply, so a table minted here
            // carries none of them.
            global_secondary_indexes: Vec::new(),
            local_secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
            table_class: desired.table_class.clone(),
            deletion_protection_enabled: desired.deletion_protection_enabled,
            tags,
            creation_date_time: at,
            // A Cloud Control API resource operation reports `SUCCESS` only once the type's
            // handler has stabilised the resource — a handler that cannot is what
            // `NotStabilizedException` reports — and the request's own span is what the
            // caller waited through, so the table this effect provisions is already
            // `ACTIVE` and writable rather than starting a transition of its own.
            active_at: at,
            resource_policy: ddb::ResourcePolicySlot::default(),
            stream: None,
            encryption: None,
            on_demand_throughput: ddb::OnDemandCeilings::default(),
        });
        store.insert_table(table);
        let created = store
            .table(region, &name)
            // Unreachable: the insert above recorded it, under this same guard.
            .map_err(|_| NativeRefusal::NotFound)?;
        Ok(model(created))
    }

    fn update(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let name = table_name(identifier).ok_or(NativeRefusal::NotFound)?;
        let mut store = self.state.lock();
        store.apply_due_transitions(at);
        let held = store
            .table(region, &name)
            .map_err(|_| NativeRefusal::NotFound)?;
        let reported = model(held);
        let (arn, billing_mode) = (held.arn.clone(), held.billing_mode.clone());
        let held_keys: Vec<String> = held.tags.stored().keys().cloned().collect();
        let desired = DesiredModel::read(
            properties,
            &Identity {
                name: &name,
                arn: &arn,
                reported: Some(&reported),
            },
            Intent::Update,
        )?;
        // The capacity pair is held to the mode the table IS in: a model that states a
        // capacity for an on-demand table, or none for a provisioned one, asks the owning
        // service for something no table can be.
        let (_, provisioned_throughput) = ddb_create::validated_declared_throughput(
            Some(&billing_mode),
            None,
            desired.provisioned_throughput.as_ref(),
        )
        .map_err(refusal)?;
        ddb::check_capacity_bounds(provisioned_throughput.as_ref()).map_err(refusal)?;
        // The model is the WHOLE desired state of the resource, so the set it carries is the
        // one the table ends up with: it is held to the tag ceiling as a create's is, and
        // there is no earlier set for it to be merged into.
        let tags = desired.tag_set()?;
        // Each capacity figure the model states is moved through the owning service's own
        // capacity move, so the table's throughput history and its decrease count are exactly
        // what that service's `UpdateTable` leaves behind.
        if let Some(throughput) = &provisioned_throughput {
            for (reads, units) in [
                (true, throughput.read_capacity_units),
                (false, throughput.write_capacity_units),
            ] {
                store.scale_provisioned_capacity(region, &name, None, reads, units, at);
            }
        }
        let table = store
            .table_mut(region, &name)
            .map_err(|_| NativeRefusal::NotFound)?;
        // A class the model MOVES is one of that table's changes, so it is held to the owning
        // service's own table-class change quota: past it that service refuses the change, and
        // this path reports the refusal as the capacity verdict it is rather than spending a
        // change the DynamoDB control plane would not. Asked before anything is written.
        let moves_class = table.table_class != desired.table_class;
        if moves_class && table.table_class_changes.exhausted(at) {
            return Err(NativeRefusal::AtCapacity(
                ddb::table_class_change_limit_message(),
            ));
        }
        // The move that capacity change starts is SETTLED here, for the same reason a table
        // this path creates is already `ACTIVE`: a Cloud Control API request reports `SUCCESS`
        // only once the type's handler has stabilised the resource, and the request's own span
        // is what the caller waited through — so a caller that has just been told the patch
        // succeeded does not find the table still `UPDATING`.
        table.updated_at = None;
        // "The date and time at which the table class was last updated"
        // (`TableClassSummary$LastUpdateDateTime`): recorded only where the class moves.
        if moves_class {
            table.table_class = desired.table_class.clone();
            table.table_class_changes.record(at);
        }
        table.deletion_protection_enabled = desired.deletion_protection_enabled;
        // A resource model is the WHOLE desired state, so the tag set it carries is the one
        // in force afterwards: a key it no longer names is removed and the rest are written,
        // both through the owning record's own tag set, which propagates as that service's
        // own tag writes do.
        let dropped: Vec<&str> = held_keys
            .iter()
            .filter(|key| !tags.contains_key(key.as_str()))
            .map(String::as_str)
            .collect();
        if !dropped.is_empty() {
            table.tags.remove(dropped, at);
        }
        if !tags.is_empty() {
            table.tags.merge(tags, at);
        }
        Ok(model(table))
    }

    fn delete(&self, region: &str, identifier: &str, at: SystemTime) -> Result<(), NativeRefusal> {
        let name = table_name(identifier).ok_or(NativeRefusal::NotFound)?;
        let mut store = self.state.lock();
        store.apply_due_transitions(at);
        let table = store
            .table(region, &name)
            .map_err(|_| NativeRefusal::NotFound)?;
        // A table the owning service will not delete at all is that service's own verdict
        // about the resource as it is.
        if table.deletion_protection_enabled {
            return Err(NativeRefusal::Rejected(
                ddb::DELETION_PROTECTED_MESSAGE.to_string(),
            ));
        }
        let stream = table.stream.clone().filter(|stream| stream.enabled);
        let table = store
            .table_mut(region, &name)
            .map_err(|_| NativeRefusal::NotFound)?;
        // Removed through the owning store's own removal: the table's deletion is marked due
        // and materialised at once, because a Cloud Control API delete reports completion
        // only once the resource is gone rather than while it is still `DELETING`.
        table.removed_at = Some(at);
        store.apply_due_transitions(at);
        drop(store);
        // The stream the deleted table published to goes back to the streams service through
        // the OWNING service's own seam — after this store's guard is released, so no caller
        // here holds two owning stores at once.
        if let Some(stream) = stream {
            ddb::hand_back_table_stream(self.streams.as_ref(), &name, &stream.arn, at);
        }
        Ok(())
    }
}

/// The name of the table `identifier` names, or `None` for a value that names no table of
/// this type at all.
///
/// The type's schema declares its primary identifier to be the table's own name and no
/// `additionalIdentifiers`, so the name is the ONE reference a resource of this type answers
/// to: the table's ARN — which its read-only `Arn` property reports — names no resource here.
/// The name itself is held to the form the OWNING service accepts
/// ([`ddb::resolve_table_name`]), so a reference no table of that service could carry names
/// nothing rather than being looked up.
fn table_name(identifier: &str) -> Option<String> {
    ddb::resolve_table_name(identifier)
        .ok()
        .filter(|name| name == identifier)
}

/// The same, for a create, whose identifier has to name a table: the owning service's own
/// diagnostic for a name it would not accept, and this type's own for a reference that is
/// not a name at all.
fn named(identifier: &str) -> Result<String, NativeRefusal> {
    let name = ddb::resolve_table_name(identifier).map_err(refusal)?;
    (name == identifier).then_some(name).ok_or_else(|| {
        NativeRefusal::Invalid(format!(
            "{IDENTIFIER_PROPERTY} of {TYPE_NAME} is the table's own name"
        ))
    })
}

/// The owning service's refusal of a declared table, as this path reports it: that service's
/// own diagnostic, which is what a client of this API reads, under the handler code for an
/// error the downstream service raised.
fn refusal(error: ddb::RequestError) -> NativeRefusal {
    NativeRefusal::Rejected(error.message().to_string())
}

/// The resource model one table renders as — the schema's properties the owning record
/// carries, and only those it HAS: a provisioned table reports its capacity, a table with no
/// stream reports neither stream property, and an untagged one reports no tag set, the same
/// way the owning service's own readers report them.
fn model(table: &ddb::Table) -> Value {
    let mut model = Map::new();
    model.insert(
        IDENTIFIER_PROPERTY.to_string(),
        Value::from(table.name.clone()),
    );
    model.insert(ARN.to_string(), Value::from(table.arn.clone()));
    model.insert(KEY_SCHEMA.to_string(), key_schema(&table.key_schema));
    model.insert(
        ATTRIBUTE_DEFINITIONS.to_string(),
        Value::Array(
            table
                .attribute_definitions
                .iter()
                .map(attribute_definition)
                .collect(),
        ),
    );
    model.insert(
        BILLING_MODE.to_string(),
        Value::from(table.billing_mode.clone()),
    );
    if let Some(throughput) = &table.provisioned_throughput {
        model.insert(
            PROVISIONED_THROUGHPUT.to_string(),
            provisioned_throughput(throughput),
        );
    }
    if let Some(class) = &table.table_class {
        model.insert(TABLE_CLASS.to_string(), Value::from(class.clone()));
    }
    model.insert(
        DELETION_PROTECTION_ENABLED.to_string(),
        Value::from(table.deletion_protection_enabled),
    );
    if !table.global_secondary_indexes.is_empty() {
        model.insert(
            GLOBAL_SECONDARY_INDEXES.to_string(),
            Value::Array(
                table
                    .global_secondary_indexes
                    .iter()
                    .map(global_secondary_index)
                    .collect(),
            ),
        );
    }
    if !table.local_secondary_indexes.is_empty() {
        model.insert(
            LOCAL_SECONDARY_INDEXES.to_string(),
            Value::Array(
                table
                    .local_secondary_indexes
                    .iter()
                    .map(local_secondary_index)
                    .collect(),
            ),
        );
    }
    if let Some(stream) = &table.stream {
        // "The ARN that uniquely identifies the latest stream for this table"
        // (`TableDescription$LatestStreamArn`), which the table keeps whether or not the
        // stream is still on; the specification describes a stream that EXISTS, so it is
        // reported only while that stream is on.
        model.insert(STREAM_ARN.to_string(), Value::from(stream.arn.clone()));
        if stream.specification().is_some() {
            model.insert(
                STREAM_SPECIFICATION.to_string(),
                Value::Object(Map::from_iter([(
                    STREAM_VIEW_TYPE.to_string(),
                    Value::from(stream.view_type.clone()),
                )])),
            );
        }
    }
    // The set an eventually consistent tag read reports, which is what the owning service's
    // own tag reader answers for the table.
    let tags = table.tags.visible();
    if !tags.is_empty() {
        model.insert(TAGS.to_string(), tag_list(tags.iter()));
    }
    Value::Object(model)
}

/// One key schema as the schema's `KeySchema` carries it: the elements in the order the key
/// declares them.
fn key_schema(elements: &[KeySchemaElement]) -> Value {
    Value::Array(
        elements
            .iter()
            .map(|element| {
                Value::Object(Map::from_iter([
                    (
                        ATTRIBUTE_NAME.to_string(),
                        Value::from(element.attribute_name.clone()),
                    ),
                    (KEY_TYPE.to_string(), Value::from(element.key_type.clone())),
                ]))
            })
            .collect(),
    )
}

/// One attribute definition as the schema's `AttributeDefinitions` carries it.
fn attribute_definition(definition: &AttributeDefinition) -> Value {
    Value::Object(Map::from_iter([
        (
            ATTRIBUTE_NAME.to_string(),
            Value::from(definition.attribute_name.clone()),
        ),
        (
            ATTRIBUTE_TYPE.to_string(),
            Value::from(definition.attribute_type.clone()),
        ),
    ]))
}

/// One capacity pair as the schema's `ProvisionedThroughput` carries it.
fn provisioned_throughput(throughput: &ProvisionedThroughput) -> Value {
    Value::Object(Map::from_iter([
        (
            READ_CAPACITY_UNITS.to_string(),
            Value::from(throughput.read_capacity_units),
        ),
        (
            WRITE_CAPACITY_UNITS.to_string(),
            Value::from(throughput.write_capacity_units),
        ),
    ]))
}

/// One projection as the schema's `Projection` carries it: only the members the index HOLDS,
/// so an index projecting the whole item reports no attribute list.
fn projection(projection: &Projection) -> Value {
    let mut carried = Map::new();
    if let Some(kind) = &projection.projection_type {
        carried.insert(PROJECTION_TYPE.to_string(), Value::from(kind.clone()));
    }
    if let Some(attributes) = &projection.non_key_attributes {
        carried.insert(
            NON_KEY_ATTRIBUTES.to_string(),
            Value::Array(attributes.iter().map(|name| Value::from(name.clone())).collect()),
        );
    }
    Value::Object(carried)
}

/// One global secondary index as the schema's `GlobalSecondaryIndexes` carries it.
fn global_secondary_index(index: &ddb::GlobalSecondaryIndex) -> Value {
    let mut carried = Map::from_iter([
        (INDEX_NAME.to_string(), Value::from(index.name.clone())),
        (KEY_SCHEMA.to_string(), key_schema(&index.key_schema)),
        (PROJECTION.to_string(), projection(&index.projection)),
    ]);
    if let Some(throughput) = &index.provisioned_throughput {
        carried.insert(
            PROVISIONED_THROUGHPUT.to_string(),
            provisioned_throughput(throughput),
        );
    }
    Value::Object(carried)
}

/// One local secondary index as the schema's `LocalSecondaryIndexes` carries it. A local
/// index carries no capacity of its own, so there is none to report.
fn local_secondary_index(index: &ddb::LocalSecondaryIndex) -> Value {
    Value::Object(Map::from_iter([
        (INDEX_NAME.to_string(), Value::from(index.name.clone())),
        (KEY_SCHEMA.to_string(), key_schema(&index.key_schema)),
        (PROJECTION.to_string(), projection(&index.projection)),
    ]))
}

/// What the resource a model describes already answers to: the values its identity and its
/// read-only properties may be stated back with, and the model the resource reports now.
struct Identity<'a> {
    /// The table's own name.
    name: &'a str,
    /// Its ARN.
    arn: &'a str,
    /// The model a read of the resource reports, or `None` for a resource that does not
    /// exist yet — which is what makes every property this path does not apply refusable on
    /// a create.
    reported: Option<&'a Value>,
}

/// Which resource operation is reading the model.
///
/// A create SETTLES the properties the type's schema declares create-only — the primary key
/// and the capacity mode the table is defined with — while a patch may only re-state them:
/// "one or more properties included in this resource operation are defined as create-only,
/// and therefore can't be updated" (`NotUpdatableException`).
#[derive(PartialEq, Eq)]
enum Intent {
    Create,
    Update,
}

/// What a resource model asks a table to be: the properties this path APPLIES, read off the
/// model's own properties.
struct DesiredModel {
    /// The table's primary key.
    key_schema: Vec<KeySchemaElement>,
    /// The attributes its keys are declared over.
    attribute_definitions: Vec<AttributeDefinition>,
    /// The capacity mode the model names, or `None` where it names none.
    billing_mode: Option<String>,
    /// The capacity a provisioned table is to hold.
    provisioned_throughput: Option<ProvisionedThroughput>,
    /// The storage class the model names, or `None` for the owning service's default.
    table_class: Option<String>,
    /// Whether the table is to be protected against deletion. "The default value is false"
    /// (`CreateTable$DeletionProtectionEnabled`), so a model that names none asks for a
    /// table that can be deleted.
    deletion_protection_enabled: bool,
    /// The tag set the table is to carry, as the owning service's own tag writes take one.
    tags: Vec<Tag>,
}

impl DesiredModel {
    /// Read `properties` as the model of the table `identity` names — or state why the
    /// owning service would not take it.
    ///
    /// The identity and the read-only properties may be CARRIED (a read reports them, so a
    /// model a client round-trips carries them) but only with the value the table already
    /// answers to: a value accepted here and never applied would be one no later read
    /// reports.
    fn read(
        properties: &Value,
        identity: &Identity<'_>,
        intent: Intent,
    ) -> Result<Self, NativeRefusal> {
        let Some(model) = properties.as_object() else {
            return Err(NativeRefusal::Invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self {
            key_schema: Vec::new(),
            attribute_definitions: Vec::new(),
            billing_mode: None,
            provisioned_throughput: None,
            table_class: None,
            deletion_protection_enabled: false,
            tags: Vec::new(),
        };
        let reported = |property: &str| {
            identity
                .reported
                .and_then(|reported| reported.get(property))
        };
        for (property, value) in model {
            match property.as_str() {
                IDENTIFIER_PROPERTY => require_value(property, value, identity.name)?,
                ARN => require_value(property, value, identity.arn)?,
                STREAM_ARN => {
                    require_reported(TYPE_NAME, property, value, reported(property))?;
                }
                // A create SETTLES the primary key, which the schema declares create-only,
                // and the capacity mode the table starts life in. A patch may re-state
                // either: the key cannot move while the resource stays the same resource,
                // and switching the capacity mode is an `UpdateTable` of the owning
                // service's own, with a capacity move and a span nothing here drives — so a
                // model that asks for the OTHER mode is refused rather than accepted and
                // dropped, the same as every property this path does not apply.
                KEY_SCHEMA if intent == Intent::Create => {
                    desired.key_schema = key_schema_of(value)?;
                }
                ATTRIBUTE_DEFINITIONS if intent == Intent::Create => {
                    desired.attribute_definitions = attribute_definitions_of(value)?;
                }
                BILLING_MODE if intent == Intent::Create => {
                    desired.billing_mode = Some(billing_mode_of(value)?);
                }
                KEY_SCHEMA | ATTRIBUTE_DEFINITIONS | BILLING_MODE => {
                    require_reported(TYPE_NAME, property, value, reported(property))?;
                }
                PROVISIONED_THROUGHPUT => {
                    desired.provisioned_throughput = Some(provisioned_throughput_of(value)?);
                }
                TABLE_CLASS => desired.table_class = Some(table_class_of(value)?),
                DELETION_PROTECTION_ENABLED => {
                    desired.deletion_protection_enabled = flag(property, value)?;
                }
                TAGS => desired.tags = tags_of(value)?,
                other if UNAPPLIED_PROPERTIES.contains(&other) => {
                    require_reported(TYPE_NAME, property, value, reported(property))?;
                }
                other => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        Ok(desired)
    }

    /// The tag set this model asks for, held to the OWNING service's own rules for a tag
    /// list: at most the tags a table may carry, no repeated key, and no key in the
    /// namespace that service keeps for itself.
    fn tag_set(&self) -> Result<ddb::TagSet, NativeRefusal> {
        if self.tags.len() > ddb::MAX_TAGS_PER_TABLE {
            return Err(NativeRefusal::Rejected(
                "Number of Tags exceed the current limit for the provided TableName".to_string(),
            ));
        }
        ddb::tag_set_from_request(&self.tags).map_err(refusal)
    }
}

/// The key schema the `KeySchema` `value` carries: the schema declares it as a list of
/// `{ AttributeName, KeyType }` elements, in the order the key declares them.
fn key_schema_of(value: &Value) -> Result<Vec<KeySchemaElement>, NativeRefusal> {
    let Some(elements) = value.as_array() else {
        return Err(NativeRefusal::Invalid(format!(
            "{KEY_SCHEMA} must be a list of {{ {ATTRIBUTE_NAME}, {KEY_TYPE} }} elements"
        )));
    };
    let mut key_schema = Vec::with_capacity(elements.len());
    for element in elements {
        let attribute_name = element.get(ATTRIBUTE_NAME).and_then(Value::as_str);
        let key_type = element.get(KEY_TYPE).and_then(Value::as_str);
        let (Some(attribute_name), Some(key_type)) = (attribute_name, key_type) else {
            return Err(NativeRefusal::Invalid(format!(
                "every {KEY_SCHEMA} element must carry {ATTRIBUTE_NAME} and {KEY_TYPE}"
            )));
        };
        // The role an element gives its attribute is one of the values the type's registry
        // schema publishes for it, so a value outside that set is a fault in the model
        // against that schema rather than anything the owning service saw.
        if !KEY_TYPE_VALUES.contains(&key_type) {
            return Err(NativeRefusal::Invalid(format!(
                "{KEY_TYPE} must be one of {KEY_TYPE_VALUES:?}"
            )));
        }
        key_schema.push(KeySchemaElement {
            attribute_name: attribute_name.to_string(),
            key_type: key_type.to_string(),
        });
    }
    Ok(key_schema)
}

/// The attribute definitions the `AttributeDefinitions` `value` carries: the schema declares
/// it as a list of `{ AttributeName, AttributeType }` entries.
fn attribute_definitions_of(value: &Value) -> Result<Vec<AttributeDefinition>, NativeRefusal> {
    let Some(entries) = value.as_array() else {
        return Err(NativeRefusal::Invalid(format!(
            "{ATTRIBUTE_DEFINITIONS} must be a list of \
             {{ {ATTRIBUTE_NAME}, {ATTRIBUTE_TYPE} }} entries"
        )));
    };
    let mut definitions = Vec::with_capacity(entries.len());
    for entry in entries {
        let attribute_name = entry.get(ATTRIBUTE_NAME).and_then(Value::as_str);
        let attribute_type = entry.get(ATTRIBUTE_TYPE).and_then(Value::as_str);
        let (Some(attribute_name), Some(attribute_type)) = (attribute_name, attribute_type) else {
            return Err(NativeRefusal::Invalid(format!(
                "every {ATTRIBUTE_DEFINITIONS} entry must carry {ATTRIBUTE_NAME} and \
                 {ATTRIBUTE_TYPE}"
            )));
        };
        // A key attribute's type is one of the scalar types the type's registry schema
        // publishes for that member, so the same holds for it.
        if !SCALAR_ATTRIBUTE_TYPE_VALUES.contains(&attribute_type) {
            return Err(NativeRefusal::Invalid(format!(
                "{ATTRIBUTE_TYPE} must be one of {SCALAR_ATTRIBUTE_TYPE_VALUES:?}"
            )));
        }
        definitions.push(AttributeDefinition {
            attribute_name: attribute_name.to_string(),
            attribute_type: attribute_type.to_string(),
        });
    }
    Ok(definitions)
}

/// The capacity mode the `BillingMode` `value` names, which is one of the values the type's
/// registry schema publishes for that property.
fn billing_mode_of(value: &Value) -> Result<String, NativeRefusal> {
    match value.as_str() {
        Some(mode) if BILLING_MODE_VALUES.contains(&mode) => Ok(mode.to_string()),
        _ => Err(NativeRefusal::Invalid(format!(
            "{BILLING_MODE} must be one of {BILLING_MODE_VALUES:?}"
        ))),
    }
}

/// The storage class the `TableClass` `value` names, which is one of the values that schema
/// publishes for that property.
fn table_class_of(value: &Value) -> Result<String, NativeRefusal> {
    match value.as_str() {
        Some(class) if TABLE_CLASS_VALUES.contains(&class) => Ok(class.to_string()),
        _ => Err(NativeRefusal::Invalid(format!(
            "{TABLE_CLASS} must be one of {TABLE_CLASS_VALUES:?}"
        ))),
    }
}

/// The capacity pair the `ProvisionedThroughput` `value` carries, in the owning service's own
/// shape: the schema declares both members required.
fn provisioned_throughput_of(value: &Value) -> Result<ProvisionedThroughput, NativeRefusal> {
    let read = value.get(READ_CAPACITY_UNITS).and_then(Value::as_i64);
    let write = value.get(WRITE_CAPACITY_UNITS).and_then(Value::as_i64);
    let (Some(read_capacity_units), Some(write_capacity_units)) = (read, write) else {
        return Err(NativeRefusal::Invalid(format!(
            "{PROVISIONED_THROUGHPUT} must carry {READ_CAPACITY_UNITS} and \
             {WRITE_CAPACITY_UNITS} as numbers"
        )));
    };
    Ok(ProvisionedThroughput {
        read_capacity_units,
        write_capacity_units,
    })
}

/// The flag `value` carries, or why it is not the boolean `property` is declared as.
fn flag(property: &str, value: &Value) -> Result<bool, NativeRefusal> {
    value
        .as_bool()
        .ok_or_else(|| NativeRefusal::Invalid(format!("{property} must be a boolean")))
}

/// The tag set `value` carries, in the owning service's own tag shape.
fn tags_of(value: &Value) -> Result<Vec<Tag>, NativeRefusal> {
    Ok(tag_entries(value)?
        .into_iter()
        .map(|(key, value)| Tag { key, value })
        .collect())
}
