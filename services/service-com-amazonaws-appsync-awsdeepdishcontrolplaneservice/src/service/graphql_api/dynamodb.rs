// SPDX-License-Identifier: CC0-1.0

//! What a resolver step whose data source is of the `AMAZON_DYNAMODB` type DOES: it CALLS
//! Amazon DynamoDB with the request its request half returned, against the table the data
//! source was configured with (`DynamodbDataSourceConfig$tableName` and `awsRegion`).
//!
//! The request half of such a step returns the data source's own request object: an
//! `operation` naming the call to make and, beside it, the members that call takes — the
//! `key` of one item, the `attributeValues` of one to write, the `update` and `condition`
//! expressions, or the `query`/`filter` of a selection. The result the response half is then
//! handed is what DynamoDB answered, with every attribute value UNMARSHALLED
//! ([`plain_value`]): a single-item operation answers the item itself, and `Query` and `Scan`
//! answer `items`, `nextToken` and `scannedCount`.
//!
//! Three things make this a real call rather than a reported one:
//!
//! * the request reaches Amazon DynamoDB ITSELF — the in-process hop into this same emulator
//!   over the service's own wire form — so what a field resolves to is what that service's
//!   store holds, a write a field makes is readable by that service's own readers, and a
//!   refusal it answers (a condition that did not hold, a table that is not there) is reported
//!   to the field as that service worded it. The call is placed by the WIRE FORM rather than
//!   through an SDK client because the request object a resolver returns already carries its
//!   keys, items and expression values as DynamoDB's own attribute values: passing them
//!   through unchanged is what keeps the call the one the resolver asked for;
//! * the permissions that admit it are weighed FIRST, and the call is not made when they do
//!   not: under the caller's own if the data source
//!   uses them ("`useCallerCredentials`: Set to TRUE to use Amazon Cognito credentials with
//!   this data source"), where the principal the GraphQL request signed as has to be allowed
//!   the DynamoDB action on the table ([`super::caller::withholding`]); otherwise under the
//!   role the data source names, as every other calling data source of this endpoint is
//!   ([`super::data_source_role`]). A refusal is reported as the error class AWS words an
//!   unauthorized call with, and nothing is read or written;
//! * an operation this data source does not serve, and a request missing what its operation
//!   takes, is refused BEFORE the hop, so no half-built call reaches the table.
//!
//! A data source configured for VERSIONED items — "set to TRUE to use Conflict Detection and
//! Resolution with this data source" (`DynamodbDataSourceConfig$versioned`) — makes more of a
//! write than the one call: AppSync keeps the item's version and the time of its last change on
//! the item itself, a write whose stated version is not the stored one is a CONFLICT, and what
//! becomes of such a write is the resolution strategy the resolver configured ([`versioned`]).
//!
//! Nothing of the stored record is changed here: this module reads the data source's
//! configuration and calls another service.

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::caller::SignedCaller;
// The attribute names DynamoDB holds each type of value under, which is what an answer's own
// values are read back out of: the MARSHALLING half of that same table is what a request
// object is written with, so the names come from there rather than being stated twice.
use super::mapping_template::utilities::dynamodb::{
    BINARY, BINARY_SET, BOOLEAN, LIST, MAP, NULL, NUMBER, NUMBER_SET, STRING, STRING_SET,
};
use super::PlaneContext;

pub mod versioned;

/// The `AMAZON_DYNAMODB` data source a step resolves against, as the step's own record
/// carries it.
#[derive(Debug, Clone, Copy)]
pub struct Configured<'a> {
    /// The data source's name, which an error of the step names it by.
    pub name: &'a str,
    /// `dynamodbConfig.tableName` — "The table name", the table every call of this data source
    /// is made against.
    pub table_name: Option<&'a str>,
    /// `dynamodbConfig.awsRegion` — "The Amazon Web Services Region" the table is in, or `None`
    /// for a data source that names none, whose table is read in the API's own Region.
    pub table_region: Option<&'a str>,
    /// `dynamodbConfig.useCallerCredentials` — "Set to TRUE to use Amazon Cognito credentials
    /// with this data source", which decides WHOSE permissions admit the call.
    pub use_caller_credentials: bool,
    /// `serviceRoleArn`, the role AppSync makes the call under where the data source does not
    /// use the caller's own credentials, or `None` for a data source that names none.
    pub service_role_arn: Option<&'a str>,
    /// `dynamodbConfig.versioned` — "Set to TRUE to use Conflict Detection and Resolution with
    /// this data source", which is what makes a write of this data source a VERSIONED one
    /// ([`versioned`]): AppSync keeps the item's version and the time of its last change on
    /// the item itself, and a write whose stated version is not the stored one is a conflict.
    pub versioned: bool,
    /// `dynamodbConfig.deltaSyncConfig.baseTableTTL` — "the number of minutes that an Item is
    /// stored in the data source", which is how long the row a versioned delete leaves behind
    /// is retained, and `0` for a data source that configures no delta sync at all.
    pub base_table_ttl: i64,
}

/// Make the call `requested` asks for against `source`'s table and answer the result the
/// step's response half is handed, or what the step did not resolve with — a [`Refused`],
/// whose type is what the step's own `ctx.error.type` carries.
///
/// A BATCH request is bounded BEFORE any of the per-table work is done: the call is built
/// first ([`Call::wire_request`], which refuses a batch past the limit of the one DynamoDB
/// call it is made as — [`MAX_BATCH_WRITE_REQUESTS`], [`MAX_BATCH_GET_KEYS`]), so neither the
/// permission weighing below nor the hop itself can be multiplied by a table or entry count
/// the request chose.
pub async fn answered(
    context: &PlaneContext,
    source: &Configured<'_>,
    sync: &versioned::Syncing<'_>,
    caller: Option<&SignedCaller>,
    requested: &Json,
) -> Result<Json, Refused> {
    let Some(table) = source
        .table_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return Err(Refused::request(format!("{NO_TABLE} {}.", source.name)));
    };
    let operation = requested
        .get(OPERATION_MEMBER)
        .and_then(Json::as_str)
        .unwrap_or_default();
    let Some(call) = Call::named(operation) else {
        return Err(Refused::request(format!(
            "{ONE_OF_THE_OPERATIONS} {}, and this request names {operation:?}.",
            Call::SERVED.join(", ")
        )));
    };
    let region = source
        .table_region
        .map(str::trim)
        .filter(|region| !region.is_empty())
        .unwrap_or(&context.region);
    let body = call.wire_request(table, requested).map_err(Refused::request)?;
    // The permissions are weighed BEFORE the hop: a call they do not admit reads and writes
    // nothing, which is what lets a denied read leave the item it addressed untouched. A
    // BATCH call addresses the tables its own request names, so each of them is weighed.
    let action = call.action();
    for named in call.tables(table, requested) {
        let resource = ::engine::emulator_arn(
            ARN_SERVICE,
            region,
            &format!("{TABLE_RESOURCE_PREFIX}{named}"),
        );
        if let Some(refusal) = unauthorized(context, source, caller, &action, &resource).await {
            return Err(Refused {
                message: refusal,
                error_type: answered_error_type(ACCESS_DENIED_EXCEPTION),
                data: None,
            });
        }
    }
    // A write through a data source configured for VERSIONED items is not the plain call: the
    // item's own version decides whether it is applied at all, and what it comes to is the
    // resolution strategy the resolver configured ([`versioned`]). Every other call — a read,
    // a selection, and the batches, which that detection is not defined over — is made as it
    // stands.
    if source.versioned {
        if let Some(write) = versioned::Write::of(call) {
            return versioned::written(
                context,
                source,
                sync,
                &versioned::Asked {
                    write,
                    region,
                    table,
                    requested,
                    body: &body,
                },
            )
            .await;
        }
    }
    let answer = called(context, region, call.target(), &body).await?;
    Ok(call.read_answer(&body, &answer))
}

/// The sentence this call is refused with when the permissions in force do not admit it, or
/// `None` when they do.
///
/// WHOSE permissions those are is the data source's own configuration: the CALLER's where it
/// uses the caller's credentials, and the role it names otherwise. A data source that uses
/// caller credentials on a request that presented no AWS credentials at all has no principal
/// to weigh, which is a call it cannot make rather than one it may — an API may configure
/// `AWS_IAM` beside another provider, so a call of that other provider reaches such a data
/// source with no identity to act as.
async fn unauthorized(
    context: &PlaneContext,
    source: &Configured<'_>,
    caller: Option<&SignedCaller>,
    action: &str,
    resource: &str,
) -> Option<String> {
    if source.use_caller_credentials {
        let Some(caller) = caller else {
            return Some(NO_CALLER_CREDENTIALS.to_string());
        };
        let reason = super::caller::withholding(caller, action, resource)?;
        return Some(
            ::service_common::iam_authorization::not_authorized_by_identity_policy(
                &caller.principal(),
                action,
                resource,
                &reason,
            ),
        );
    }
    let role_arn = source.service_role_arn?;
    super::data_source_role::withheld(context, role_arn, action, resource).await
}

/// One call an `AMAZON_DYNAMODB` data source serves, as the request's own `operation` member
/// names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    GetItem,
    PutItem,
    UpdateItem,
    DeleteItem,
    Query,
    Scan,
    BatchGetItem,
    BatchPutItem,
    BatchDeleteItem,
}

impl Call {
    /// The operations this data source serves, as a refusal names them.
    const SERVED: &'static [&'static str] = &[
        "GetItem",
        "PutItem",
        "UpdateItem",
        "DeleteItem",
        "Query",
        "Scan",
        "BatchGetItem",
        "BatchPutItem",
        "BatchDeleteItem",
    ];

    /// The call `operation` names, or `None` for a value this data source does not serve.
    fn named(operation: &str) -> Option<Self> {
        match operation {
            "GetItem" => Some(Self::GetItem),
            "PutItem" => Some(Self::PutItem),
            "UpdateItem" => Some(Self::UpdateItem),
            "DeleteItem" => Some(Self::DeleteItem),
            "Query" => Some(Self::Query),
            "Scan" => Some(Self::Scan),
            "BatchGetItem" => Some(Self::BatchGetItem),
            "BatchPutItem" => Some(Self::BatchPutItem),
            "BatchDeleteItem" => Some(Self::BatchDeleteItem),
            _ => None,
        }
    }

    /// Whether this call addresses MANY items across the tables its own request names, rather
    /// than one item of the table the data source was configured with.
    fn is_batch(self) -> bool {
        matches!(
            self,
            Self::BatchGetItem | Self::BatchPutItem | Self::BatchDeleteItem
        )
    }

    /// The DynamoDB operation this call makes, which is the name its own wire target carries
    /// and the name of the IAM action it performs ("Actions defined by Amazon DynamoDB").
    ///
    /// A batch operation of this data source is one DynamoDB call over many items: the two
    /// WRITE batches are both that service's `BatchWriteItem`, which takes a put and a delete
    /// request alike, and the read batch is its `BatchGetItem`.
    fn target(self) -> &'static str {
        match self {
            Self::GetItem => "GetItem",
            Self::PutItem => "PutItem",
            Self::UpdateItem => "UpdateItem",
            Self::DeleteItem => "DeleteItem",
            Self::Query => "Query",
            Self::Scan => "Scan",
            Self::BatchGetItem => "BatchGetItem",
            Self::BatchPutItem | Self::BatchDeleteItem => "BatchWriteItem",
        }
    }

    /// The IAM action that admits this call: the DynamoDB action namespace and the operation
    /// it performs.
    fn action(self) -> String {
        format!("{ACTION_NAMESPACE}:{}", self.target())
    }

    /// The tables this call addresses: the one the data source was configured with, and — for
    /// a BATCH call, which states its own tables — every table its request names.
    ///
    /// Each one is a table the permissions in force have to admit the call on, because each is
    /// a table the call reads or writes.
    fn tables(self, configured: &str, requested: &Json) -> Vec<String> {
        if !self.is_batch() {
            return vec![configured.to_string()];
        }
        batch_tables(requested)
            .map(|tables| tables.into_iter().map(|(name, _)| name).collect())
            .unwrap_or_default()
    }

    /// The request this call is made with, as DynamoDB's own input carries it, or the sentence
    /// the request object is refused with.
    ///
    /// Every attribute value travels EXACTLY as the resolver's request object carried it — a
    /// key, an item's values and the values an expression is parameterised by are already
    /// DynamoDB's own representation — and every other member is the one DynamoDB's input
    /// names for it.
    fn wire_request(self, table: &str, requested: &Json) -> Result<Json, String> {
        let mut input = JsonMap::new();
        // A batch call addresses the tables its own request names, so it carries no single
        // table at all; every other call is made against the configured one.
        if !self.is_batch() {
            input.insert(TABLE_NAME_INPUT.to_string(), Json::String(table.to_string()));
        }
        match self {
            Self::GetItem => {
                input.insert(KEY_INPUT.to_string(), object(requested, KEY_MEMBER)?);
                carry_bool(&mut input, requested, CONSISTENT_READ_MEMBER, CONSISTENT_READ_INPUT)?;
                let projection = expression(requested, PROJECTION_MEMBER)?;
                projection.carry(&mut input, PROJECTION_EXPRESSION_INPUT);
            }
            Self::PutItem => {
                // The item PUT is the key the request addresses plus the values it carries,
                // which is the one item AppSync's `PutItem` request describes.
                input.insert(ITEM_INPUT.to_string(), written_item(requested)?);
                expression(requested, CONDITION_MEMBER)?.carry(&mut input, CONDITION_EXPRESSION_INPUT);
            }
            Self::UpdateItem => {
                input.insert(KEY_INPUT.to_string(), object(requested, KEY_MEMBER)?);
                let update = expression(requested, UPDATE_MEMBER)?;
                if update.expression.is_none() {
                    return Err(format!("{AN_UPDATE_STATES} {UPDATE_MEMBER}.{EXPRESSION_MEMBER}."));
                }
                update.carry(&mut input, UPDATE_EXPRESSION_INPUT);
                expression(requested, CONDITION_MEMBER)?.carry(&mut input, CONDITION_EXPRESSION_INPUT);
                // The result of an update is the item as it now stands, so the call asks for
                // the new values.
                input.insert(
                    RETURN_VALUES_INPUT.to_string(),
                    Json::String(ALL_NEW.to_string()),
                );
            }
            Self::DeleteItem => {
                input.insert(KEY_INPUT.to_string(), object(requested, KEY_MEMBER)?);
                expression(requested, CONDITION_MEMBER)?.carry(&mut input, CONDITION_EXPRESSION_INPUT);
                // The result of a delete is the item as it was, so the call asks for the old
                // values.
                input.insert(
                    RETURN_VALUES_INPUT.to_string(),
                    Json::String(ALL_OLD.to_string()),
                );
            }
            Self::Query => {
                let query = expression(requested, QUERY_MEMBER)?;
                if query.expression.is_none() {
                    return Err(format!("{A_QUERY_STATES} {QUERY_MEMBER}.{EXPRESSION_MEMBER}."));
                }
                query.carry(&mut input, KEY_CONDITION_EXPRESSION_INPUT);
                expression(requested, FILTER_MEMBER)?.carry(&mut input, FILTER_EXPRESSION_INPUT);
                carry_selection(&mut input, requested)?;
                carry_bool(&mut input, requested, SCAN_INDEX_FORWARD_MEMBER, SCAN_INDEX_FORWARD_INPUT)?;
            }
            Self::Scan => {
                expression(requested, FILTER_MEMBER)?.carry(&mut input, FILTER_EXPRESSION_INPUT);
                carry_selection(&mut input, requested)?;
                carry_number(&mut input, requested, SEGMENT_MEMBER, SEGMENT_INPUT)?;
                carry_number(&mut input, requested, TOTAL_SEGMENTS_MEMBER, TOTAL_SEGMENTS_INPUT)?;
            }
            // Both write batches are one `BatchWriteItem` over the write requests the tables
            // the request names carry: a put request per item written, a delete request per
            // key removed. How MANY writes one such call may carry is bounded before any of
            // them is built ([`MAX_BATCH_WRITE_REQUESTS`]).
            Self::BatchPutItem | Self::BatchDeleteItem => {
                let mut request_items = JsonMap::new();
                let mut carried: usize = 0;
                for (name, stated) in batch_tables(requested)? {
                    let Json::Array(entries) = stated else {
                        return Err(format!("{A_BATCH_TABLE_IS_A_LIST} {TABLES_MEMBER}.{name}."));
                    };
                    if entries.is_empty() {
                        return Err(format!("{A_BATCH_TABLE_IS_NOT_EMPTY} {TABLES_MEMBER}.{name}."));
                    }
                    carried = carried.saturating_add(entries.len());
                    if carried > MAX_BATCH_WRITE_REQUESTS {
                        return Err(format!(
                            "{A_WRITE_BATCH_CARRIES} {MAX_BATCH_WRITE_REQUESTS} \
                             {ITEMS_OR_KEYS_IN_ALL}"
                        ));
                    }
                    let writes: Vec<Json> = entries
                        .iter()
                        .map(|entry| match self {
                            Self::BatchDeleteItem => {
                                json!({DELETE_REQUEST_INPUT: {KEY_INPUT: entry}})
                            }
                            _ => json!({PUT_REQUEST_INPUT: {ITEM_INPUT: entry}}),
                        })
                        .collect();
                    request_items.insert(name, Json::Array(writes));
                }
                input.insert(REQUEST_ITEMS_INPUT.to_string(), Json::Object(request_items));
            }
            // The read batch is one `BatchGetItem` over the keys each table the request names
            // states, with that table's own read flag and projection. How many keys one such
            // call may read is bounded before any of them is carried
            // ([`MAX_BATCH_GET_KEYS`]).
            Self::BatchGetItem => {
                let mut request_items = JsonMap::new();
                let mut carried: usize = 0;
                for (name, stated) in batch_tables(requested)? {
                    let keys = match stated.get(KEYS_MEMBER) {
                        Some(Json::Array(keys)) if !keys.is_empty() => keys.clone(),
                        _ => {
                            return Err(format!(
                                "{A_BATCH_GET_STATES} {TABLES_MEMBER}.{name}.{KEYS_MEMBER}."
                            ))
                        }
                    };
                    carried = carried.saturating_add(keys.len());
                    if carried > MAX_BATCH_GET_KEYS {
                        return Err(format!(
                            "{A_READ_BATCH_CARRIES} {MAX_BATCH_GET_KEYS} {ITEMS_OR_KEYS_IN_ALL}"
                        ));
                    }
                    let mut per_table = JsonMap::new();
                    per_table.insert(KEYS_INPUT.to_string(), Json::Array(keys));
                    carry_bool(
                        &mut per_table,
                        &stated,
                        CONSISTENT_READ_MEMBER,
                        CONSISTENT_READ_INPUT,
                    )?;
                    expression(&stated, PROJECTION_MEMBER)?
                        .carry(&mut per_table, PROJECTION_EXPRESSION_INPUT);
                    request_items.insert(name, Json::Object(per_table));
                }
                input.insert(REQUEST_ITEMS_INPUT.to_string(), Json::Object(request_items));
            }
        }
        Ok(wire_values(&Json::Object(input)))
    }

    /// The result the step's response half is handed, read out of what DynamoDB answered.
    ///
    /// A single-item operation answers the ITEM — the one it read, the one it wrote, or the one
    /// it removed — and a selection answers the page it read: its items, the token that
    /// continues it and how many values it examined. `null` is the result of a read that found
    /// no item, which is what a field whose item is not there resolves to.
    fn read_answer(self, requested: &Json, answered: &Json) -> Json {
        match self {
            Self::GetItem => plain_member(answered, ITEM_OUTPUT),
            // What a put came to is the item it wrote, which the request itself describes: the
            // call is answered the values it REPLACED, and the item now in the table is the
            // one it carried.
            Self::PutItem => requested
                .get(ITEM_INPUT)
                .map_or(Json::Null, plain_value),
            Self::UpdateItem | Self::DeleteItem => plain_member(answered, ATTRIBUTES_OUTPUT),
            Self::Query | Self::Scan => {
                let items: Vec<Json> = answered
                    .get(ITEMS_OUTPUT)
                    .and_then(Json::as_array)
                    .map(|items| items.iter().map(plain_value).collect())
                    .unwrap_or_default();
                json!({
                    ITEMS_MEMBER: items,
                    NEXT_TOKEN_MEMBER: answered
                        .get(LAST_EVALUATED_KEY_OUTPUT)
                        .filter(|key| !key.is_null())
                        .map_or(Json::Null, |key| Json::String(encoded_token(key))),
                    SCANNED_COUNT_MEMBER: answered
                        .get(SCANNED_COUNT_OUTPUT)
                        .cloned()
                        .unwrap_or_else(|| json!(0)),
                })
            }
            // What a write batch came to is what it wrote or removed, per table, which the
            // request itself describes: `BatchWriteItem` answers only what it did NOT carry
            // out, which is reported beside it under the member that batch states it in.
            Self::BatchPutItem => batch_answer(
                requested,
                answered,
                |write| write.get(PUT_REQUEST_INPUT).and_then(|put| put.get(ITEM_INPUT)),
                UNPROCESSED_ITEMS_MEMBER,
            ),
            Self::BatchDeleteItem => batch_answer(
                requested,
                answered,
                |write| {
                    write
                        .get(DELETE_REQUEST_INPUT)
                        .and_then(|delete| delete.get(KEY_INPUT))
                },
                UNPROCESSED_KEYS_MEMBER,
            ),
            // What a read batch came to is what each table ANSWERED, beside the keys it did
            // not get to.
            Self::BatchGetItem => {
                let data = per_table(answered.get(RESPONSES_OUTPUT), |items| {
                    items
                        .as_array()
                        .map(|items| items.iter().map(plain_value).collect())
                        .unwrap_or_default()
                });
                let unprocessed = per_table(answered.get(UNPROCESSED_KEYS_OUTPUT), |stated| {
                    stated
                        .get(KEYS_INPUT)
                        .and_then(Json::as_array)
                        .map(|keys| keys.iter().map(plain_value).collect())
                        .unwrap_or_default()
                });
                json!({
                    DATA_MEMBER: data,
                    UNPROCESSED_KEYS_MEMBER: unprocessed,
                })
            }
        }
    }
}

/// The result of one WRITE batch: what it carried per table, read back out of the call it
/// made with `written`, beside the writes DynamoDB did not carry out.
///
/// `written` is what one entry of a table's write list states — the item a put carries or the
/// key a delete addresses — so one reading serves both batches, and the unprocessed writes the
/// answer reports are read the same way.
fn batch_answer(
    requested: &Json,
    answered: &Json,
    written: impl Fn(&Json) -> Option<&Json> + Copy,
    unprocessed_member: &str,
) -> Json {
    let carried = |writes: &Json| -> Vec<Json> {
        writes
            .as_array()
            .map(|writes| writes.iter().filter_map(written).map(plain_value).collect())
            .unwrap_or_default()
    };
    let mut result = JsonMap::new();
    result.insert(
        DATA_MEMBER.to_string(),
        per_table(requested.get(REQUEST_ITEMS_INPUT), carried),
    );
    result.insert(
        unprocessed_member.to_string(),
        per_table(answered.get(UNPROCESSED_ITEMS_OUTPUT), carried),
    );
    Json::Object(result)
}

/// One per-table map of a batch's result: every table the map `stated` names, with `read`
/// applied to that table's own entry.
///
/// An absent map is an empty one — a batch that left nothing unprocessed states no table —
/// so what a resolver reads is a map it can walk either way.
fn per_table(stated: Option<&Json>, read: impl Fn(&Json) -> Vec<Json>) -> Json {
    let tables = stated
        .and_then(Json::as_object)
        .map(|tables| {
            tables
                .iter()
                .map(|(name, entry)| (name.clone(), Json::Array(read(entry))))
                .collect::<JsonMap<String, Json>>()
        })
        .unwrap_or_default();
    Json::Object(tables)
}

/// The tables a BATCH request names and what it states for each, in the order the request
/// writes them, or the sentence the request is refused with.
///
/// "`tables`: the tables to … in" is where every batch operation of this data source states
/// what it addresses, so a request that names none addresses no table and is refused before
/// any call is built.
fn batch_tables(requested: &Json) -> Result<Vec<(String, Json)>, String> {
    match requested.get(TABLES_MEMBER) {
        Some(Json::Object(tables)) if !tables.is_empty() => Ok(tables
            .iter()
            .map(|(name, stated)| (name.clone(), stated.clone()))
            .collect()),
        Some(Json::Object(_)) | None | Some(Json::Null) => {
            Err(format!("{A_BATCH_STATES} {TABLES_MEMBER}."))
        }
        Some(_) => Err(format!("{A_MEMBER_IS_A_MAP} {TABLES_MEMBER}.")),
    }
}

/// The item a `PutItem` request writes: the key it addresses and the values it carries, which
/// is one map.
fn written_item(requested: &Json) -> Result<Json, String> {
    let mut item = JsonMap::new();
    for member in [KEY_MEMBER, ATTRIBUTE_VALUES_MEMBER] {
        match requested.get(member) {
            None | Some(Json::Null) => {}
            Some(Json::Object(members)) => {
                item.extend(members.iter().map(|(name, value)| (name.clone(), value.clone())));
            }
            Some(_) => return Err(format!("{A_MEMBER_IS_A_MAP} {member}.")),
        }
    }
    if item.is_empty() {
        return Err(format!("{A_PUT_STATES} {ATTRIBUTE_VALUES_MEMBER}."));
    }
    Ok(Json::Object(item))
}

/// Every attribute value of a built call as DynamoDB's own WIRE form carries it.
///
/// The one difference between the representation a resolver's request object writes and the
/// one the service's input takes is the NUMBER: a request object carries it as the JSON number
/// it is (`$util.dynamodb.toDynamoDB(3)` is `{"N": 3}`), and the wire form carries every
/// number as text, so a number attribute and a number of a number set are written out as the
/// text of that number wherever one appears in the call. Nothing else of the call is touched.
fn wire_values(value: &Json) -> Json {
    match value {
        Json::Object(members) => Json::Object(
            members
                .iter()
                .map(|(name, held)| {
                    let written = match (name.as_str(), held) {
                        (NUMBER, Json::Number(count)) => Json::String(count.to_string()),
                        (NUMBER_SET, Json::Array(elements)) => Json::Array(
                            elements
                                .iter()
                                .map(|element| match element {
                                    Json::Number(count) => Json::String(count.to_string()),
                                    other => other.clone(),
                                })
                                .collect(),
                        ),
                        (_, held) => wire_values(held),
                    };
                    (name.clone(), written)
                })
                .collect(),
        ),
        Json::Array(elements) => Json::Array(elements.iter().map(wire_values).collect()),
        other => other.clone(),
    }
}

/// The members every selection of a page carries, where the request states them: which index
/// it reads, how many values it reads, whether the read is consistent, which attributes it
/// projects, what it selects and where the previous page left off.
fn carry_selection(input: &mut JsonMap<String, Json>, requested: &Json) -> Result<(), String> {
    if let Some(index) = requested.get(INDEX_MEMBER).and_then(Json::as_str) {
        input.insert(INDEX_NAME_INPUT.to_string(), Json::String(index.to_string()));
    }
    carry_number(input, requested, LIMIT_MEMBER, LIMIT_INPUT)?;
    carry_bool(input, requested, CONSISTENT_READ_MEMBER, CONSISTENT_READ_INPUT)?;
    if let Some(select) = requested.get(SELECT_MEMBER).and_then(Json::as_str) {
        input.insert(SELECT_INPUT.to_string(), Json::String(select.to_string()));
    }
    expression(requested, PROJECTION_MEMBER)?.carry(input, PROJECTION_EXPRESSION_INPUT);
    match requested.get(NEXT_TOKEN_MEMBER) {
        None | Some(Json::Null) => {}
        Some(Json::String(token)) => {
            let key = decoded_token(token)
                .ok_or_else(|| format!("{NOT_A_NEXT_TOKEN} {NEXT_TOKEN_MEMBER}."))?;
            input.insert(EXCLUSIVE_START_KEY_INPUT.to_string(), key);
        }
        Some(_) => return Err(format!("{NOT_A_NEXT_TOKEN} {NEXT_TOKEN_MEMBER}.")),
    }
    Ok(())
}

/// One `{expression, expressionNames, expressionValues}` object of a request, read as the
/// three members DynamoDB's own input carries them in.
#[derive(Debug, Clone, Default)]
struct Expression {
    expression: Option<String>,
    names: JsonMap<String, Json>,
    values: JsonMap<String, Json>,
}

impl Expression {
    /// Carry this expression into `input` under `member` — the name DynamoDB's input gives
    /// this expression — with its names and values MERGED into the request's own maps, because
    /// one call carries one map of each however many of its expressions contribute to them.
    fn carry(self, input: &mut JsonMap<String, Json>, member: &str) {
        let Some(expression) = self.expression else {
            return;
        };
        input.insert(member.to_string(), Json::String(expression));
        merged(input, EXPRESSION_NAMES_INPUT, self.names);
        merged(input, EXPRESSION_VALUES_INPUT, self.values);
    }
}

/// Add `contributed` to the map `member` of `input`, which one more expression of the same
/// request contributes to.
fn merged(input: &mut JsonMap<String, Json>, member: &str, contributed: JsonMap<String, Json>) {
    if contributed.is_empty() {
        return;
    }
    let mut held = match input.remove(member) {
        Some(Json::Object(held)) => held,
        _ => JsonMap::new(),
    };
    held.extend(contributed);
    input.insert(member.to_string(), Json::Object(held));
}

/// The expression object `member` of `requested` carries, or the sentence it is refused with.
/// A member the request does not state contributes nothing.
fn expression(requested: &Json, member: &str) -> Result<Expression, String> {
    let held = match requested.get(member) {
        None | Some(Json::Null) => return Ok(Expression::default()),
        Some(Json::Object(members)) => members,
        Some(_) => return Err(format!("{A_MEMBER_IS_A_MAP} {member}.")),
    };
    let map = |name: &str| -> Result<JsonMap<String, Json>, String> {
        match held.get(name) {
            None | Some(Json::Null) => Ok(JsonMap::new()),
            Some(Json::Object(members)) => Ok(members.clone()),
            Some(_) => Err(format!("{A_MEMBER_IS_A_MAP} {member}.{name}.")),
        }
    };
    let expression = match held.get(EXPRESSION_MEMBER) {
        None | Some(Json::Null) => None,
        Some(Json::String(text)) => Some(text.clone()),
        Some(_) => return Err(format!("{AN_EXPRESSION_IS_TEXT} {member}.{EXPRESSION_MEMBER}.")),
    };
    Ok(Expression {
        expression,
        names: map(EXPRESSION_NAMES_MEMBER)?,
        values: map(EXPRESSION_VALUES_MEMBER)?,
    })
}

/// The object `member` of `requested` carries, which an operation addressing one item needs.
fn object(requested: &Json, member: &str) -> Result<Json, String> {
    match requested.get(member) {
        Some(Json::Object(members)) if !members.is_empty() => Ok(Json::Object(members.clone())),
        Some(Json::Object(_)) | None | Some(Json::Null) => {
            Err(format!("{AN_OPERATION_STATES} {member}."))
        }
        Some(_) => Err(format!("{A_MEMBER_IS_A_MAP} {member}.")),
    }
}

/// Carry a boolean member of the request into `input` where it states one.
///
/// A member of another shape is no boolean, and the call is refused by that member's name
/// rather than made without what the request asked for.
fn carry_bool(
    input: &mut JsonMap<String, Json>,
    requested: &Json,
    member: &str,
    name: &str,
) -> Result<(), String> {
    match requested.get(member) {
        None | Some(Json::Null) => Ok(()),
        Some(Json::Bool(decided)) => {
            input.insert(name.to_string(), Json::Bool(*decided));
            Ok(())
        }
        Some(_) => Err(format!("{A_FLAG_IS_A_BOOLEAN} {member}.")),
    }
}

/// Carry a numeric member of the request into `input` where it states one, under the same
/// reading: a member of another shape is no count, so the call is refused by its name.
fn carry_number(
    input: &mut JsonMap<String, Json>,
    requested: &Json,
    member: &str,
    name: &str,
) -> Result<(), String> {
    match requested.get(member) {
        None | Some(Json::Null) => Ok(()),
        Some(held) => {
            let count = held
                .as_i64()
                .ok_or_else(|| format!("{A_COUNT_IS_A_NUMBER} {member}."))?;
            input.insert(name.to_string(), json!(count));
            Ok(())
        }
    }
}

/// The member `name` of what DynamoDB answered, unmarshalled — `null` where the answer
/// carries none, which is how DynamoDB reports that there was no such item.
fn plain_member(answered: &Json, name: &str) -> Json {
    answered
        .get(name)
        .filter(|held| !held.is_null())
        .map_or(Json::Null, plain_value)
}

/// One DynamoDB attribute value as the PLAIN JSON value it holds, which is what a resolver's
/// response half reads.
///
/// The inverse of the marshalling a request object is written with
/// (`$util.dynamodb.toDynamoDB` and its typed siblings): a one-member object naming an
/// attribute type carries that type's content, a list and a map are unmarshalled through
/// this, and a value that is no attribute value at all stands for itself.
#[must_use]
pub fn plain_value(value: &Json) -> Json {
    let Some(members) = value.as_object() else {
        return value.clone();
    };
    let mut named = members.iter();
    let (Some((kind, content)), None) = (named.next(), named.next()) else {
        // Not one attribute value but a MAP of them, which is what an item is: every member
        // carries its own attribute value.
        return Json::Object(
            members
                .iter()
                .map(|(name, held)| (name.clone(), plain_value(held)))
                .collect(),
        );
    };
    match (kind.as_str(), content) {
        (STRING | BINARY, Json::String(text)) => Json::String(text.clone()),
        (NUMBER, held) => number(held),
        (BOOLEAN, Json::Bool(decided)) => Json::Bool(*decided),
        (NULL, _) => Json::Null,
        (LIST, Json::Array(elements)) => {
            Json::Array(elements.iter().map(plain_value).collect())
        }
        (MAP, Json::Object(nested)) => Json::Object(
            nested
                .iter()
                .map(|(name, held)| (name.clone(), plain_value(held)))
                .collect(),
        ),
        (NUMBER_SET, Json::Array(elements)) => {
            Json::Array(elements.iter().map(number).collect())
        }
        (STRING_SET | BINARY_SET, Json::Array(elements)) => Json::Array(elements.clone()),
        // A one-member object that is not an attribute value is a map holding one member.
        _ => Json::Object(
            [(kind.clone(), plain_value(content))]
                .into_iter()
                .collect(),
        ),
    }
}

/// A number attribute's content as the JSON number it holds: DynamoDB carries every number as
/// TEXT, so the text is read back as the number it stands for and a value that is already a
/// number stands for itself.
fn number(content: &Json) -> Json {
    match content {
        Json::String(text) => ::serde_json::from_str::<Json>(text.trim())
            .ok()
            .filter(Json::is_number)
            .unwrap_or_else(|| Json::String(text.clone())),
        other => other.clone(),
    }
}

/// The token a selection reports for the page AFTER the one it read: the key DynamoDB answered
/// as the last one it evaluated, held as base64url text so a client carries it back opaquely.
fn encoded_token(key: &Json) -> String {
    use ::base64::Engine as _;
    ::base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.to_string())
}

/// The key one such token stands for, or `None` for text that is not a token this endpoint
/// issued.
fn decoded_token(token: &str) -> Option<Json> {
    use ::base64::Engine as _;
    let decoded = ::base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.trim())
        .ok()?;
    let parsed: Json = ::serde_json::from_slice(&decoded).ok()?;
    parsed.is_object().then_some(parsed)
}

/// Make the call `target` names against Amazon DynamoDB in `region`, in process, and answer
/// the document it answered with.
///
/// The hop is the emulator's own in-process transport, signed for the called service by the
/// tree's one in-process signer with the credential this endpoint holds — which is never
/// reported or logged — so the call is served by the DynamoDB this emulator mounts and no
/// connection is opened. A refusal that service ANSWERED is read off the answer and reported
/// as that service's own error class; an answer from no service at all (an emulator with no
/// DynamoDB mounted) is reported as a call that could not be made.
async fn called(
    context: &PlaneContext,
    region: &str,
    target: &str,
    body: &Json,
) -> Result<Json, Refused> {
    let host = format!("{ARN_SERVICE}.{region}.amazonaws.com");
    let mut request = ::http::Request::builder()
        .method(::http::Method::POST)
        .uri(format!("https://{host}/"))
        .header(::http::header::HOST, host.clone())
        .header(::http::header::CONTENT_TYPE, WIRE_MEDIA_TYPE)
        .header(WIRE_TARGET_FIELD, format!("{WIRE_TARGET_PREFIX}{target}"))
        .body(body.to_string().into_bytes())
        .map_err(|error| Refused::unmade(&format!("{NOT_CALLED} {error}")))?;
    ::service_common::in_process_sigv4::sign_callback_request(
        &mut request,
        ARN_SERVICE,
        region,
        &context.callback,
    )
    .await
    .map_err(|error| Refused::unmade(&format!("{NOT_SIGNED} {error}")))?;
    let (parts, payload) = request.into_parts();
    let request = ::aws_smithy_runtime_api::http::Request::try_from(::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(payload)),
    ))
    .map_err(|error| Refused::unmade(&format!("{NOT_CALLED} {error}")))?;
    let answer = ::aws_smithy_runtime_api::client::http::HttpConnector::call(
        &context.callback.http_connector(),
        request,
    )
    .await
    .map_err(|error| {
        Refused::unmade(&format!(
            "{NOT_CALLED} {}",
            ::aws_smithy_types::error::display::DisplayErrorContext(&error)
        ))
    })?;
    let status = answer.status().as_u16();
    let payload = answer.body().bytes().unwrap_or_default().to_vec();
    if ::service_common::in_process_refusal::is_transport_refusal(&payload) {
        return Err(Refused::unmade(NO_SUCH_SERVICE));
    }
    let document: Json = ::serde_json::from_slice(&payload).unwrap_or(Json::Null);
    if status == SERVED_STATUS {
        return Ok(document);
    }
    // What the OWNING service answered about the call, which is what a client of this field
    // has to act on: the error class and the sentence DynamoDB stated.
    Err(Refused {
        message: answered_message(&document),
        error_type: answered_error_type(&answered_code(&document)),
        data: None,
    })
}

/// The error class the called service answered under, read off the type its refusal names
/// (`com.amazonaws.dynamodb#ConditionalCheckFailedException`), whose last segment is the class
/// itself.
fn answered_code(document: &Json) -> String {
    document
        .get(ERROR_TYPE_MEMBER)
        .and_then(Json::as_str)
        .map(|named| {
            named
                .rsplit(['#', ':'])
                .next()
                .unwrap_or(named)
                .to_string()
        })
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| UNKNOWN_ERROR_CODE.to_string())
}

/// The sentence it answered with, under either spelling an AWS JSON refusal carries it in.
fn answered_message(document: &Json) -> String {
    [ERROR_MESSAGE_MEMBER, ERROR_MESSAGE_MEMBER_CAPITALISED]
        .iter()
        .filter_map(|member| document.get(*member).and_then(Json::as_str))
        .next()
        .unwrap_or(NOT_ANSWERED)
        .to_string()
}

/// Why one call did not produce a result: the sentence it did not resolve with, and the type
/// the step's own `ctx.error.type` carries.
#[derive(Debug, Clone)]
pub struct Refused {
    /// The sentence the step did not resolve with.
    pub message: String,
    /// The class of the refusal, as an error of the field reports it as its `errorType`.
    pub error_type: String,
    /// What the refusal carries BESIDE its sentence, where the refusal is one AppSync reports a
    /// value with: the row as the server holds it, which is what the error of an unresolved
    /// conflict carries so the caller can build the write it retries with
    /// ([`versioned::written`]). `None` for every refusal that reports no value of its own.
    pub data: Option<Json>,
}

impl Refused {
    /// A request this data source cannot act on: the step's own definition produced it, so it
    /// is an error of that CODE rather than anything the owning service answered.
    fn request(message: String) -> Self {
        Self {
            message,
            error_type: REQUEST_ERROR_TYPE.to_string(),
            data: None,
        }
    }

    /// A call that was never MADE: there is no answer from the owning service to report, so
    /// the step is reported as one whose call could not be placed, under the class a client
    /// raises for a call it could not complete.
    fn unmade(reason: &str) -> Self {
        Self {
            message: reason.to_string(),
            error_type: answered_error_type(BASE_EXCEPTION),
            data: None,
        }
    }
}

/// The `errorType` of what the OWNING service answered about the call: AppSync reports a
/// downstream refusal as the service it came from and the class of the exception its client
/// raised.
///
/// Which class that is follows the called service's own MODEL: an error DynamoDB declares for
/// one of these operations is raised as that error's own class, and anything else it answers
/// — an authorization refusal, a request it could not read, a fault of its frontend — is
/// raised as the service's BASE exception, because a client has no modelled class to raise for
/// it ([`MODELLED_ERRORS`]).
fn answered_error_type(code: &str) -> String {
    let class = if MODELLED_ERRORS.contains(&code) {
        code
    } else {
        BASE_EXCEPTION
    };
    format!("{ANSWERED_ERROR_TYPE_PREFIX}{class}")
}

/// The errors Amazon DynamoDB DECLARES for the operations an `AMAZON_DYNAMODB` data source
/// serves, each of which a client raises under its own name.
///
/// Source: the `errors` of `GetItem`, `PutItem`, `UpdateItem`, `DeleteItem`, `Query` and
/// `Scan` in that service's model
/// (`smithy_query(query="operation",operation="GetItem,PutItem,UpdateItem,DeleteItem,Query,Scan",
/// model_path="services/service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/com-amazonaws-dynamodb-dynamodb-20120810.json")`).
const MODELLED_ERRORS: &[&str] = &[
    "ConditionalCheckFailedException",
    "InternalServerError",
    "InvalidEndpointException",
    "ItemCollectionSizeLimitExceededException",
    "ProvisionedThroughputExceededException",
    "ReplicatedWriteConflictException",
    "RequestLimitExceeded",
    "ResourceNotFoundException",
    "ThrottlingException",
    "TransactionConflictException",
];

/// The class a client raises for an answer the called service's model declares no error for:
/// that service's own base exception.
const BASE_EXCEPTION: &str = "DynamoDbException";

/// The ARN namespace, the endpoint prefix and the signing name of the called service, which
/// are one value for Amazon DynamoDB, and the resource type a table's ARN carries.
const ARN_SERVICE: &str = "dynamodb";
const TABLE_RESOURCE_PREFIX: &str = "table/";

/// The namespace Amazon DynamoDB publishes its IAM actions under.
const ACTION_NAMESPACE: &str = "dynamodb";

/// The wire form the called service is reached by: its media type and the field naming the
/// operation, both the tree's own spellings of the `awsJson` protocol
/// (`service_common::response_media_type`, `service_common::rpc_target`), and the prefix that
/// field's value carries, which is the service shape's own name (`DynamoDB_20120810`,
/// `smithy_query(query="service_info")` of that service's model).
const WIRE_MEDIA_TYPE: &str = ::service_common::response_media_type::AWS_JSON_1_0;
const WIRE_TARGET_FIELD: &str = ::service_common::rpc_target::TARGET_HEADER;
const WIRE_TARGET_PREFIX: &str = "DynamoDB_20120810.";

/// The status that service answers a call it SERVED with; every other status carries a
/// refusal.
const SERVED_STATUS: u16 = 200;

/// The members of the request object this data source is handed.
const OPERATION_MEMBER: &str = "operation";
const KEY_MEMBER: &str = "key";
const ATTRIBUTE_VALUES_MEMBER: &str = "attributeValues";
const UPDATE_MEMBER: &str = "update";
const CONDITION_MEMBER: &str = "condition";
const QUERY_MEMBER: &str = "query";
const FILTER_MEMBER: &str = "filter";
const PROJECTION_MEMBER: &str = "projection";
const EXPRESSION_MEMBER: &str = "expression";
const EXPRESSION_NAMES_MEMBER: &str = "expressionNames";
const EXPRESSION_VALUES_MEMBER: &str = "expressionValues";
const INDEX_MEMBER: &str = "index";
const LIMIT_MEMBER: &str = "limit";
const CONSISTENT_READ_MEMBER: &str = "consistentRead";
const SCAN_INDEX_FORWARD_MEMBER: &str = "scanIndexForward";
const SELECT_MEMBER: &str = "select";
const SEGMENT_MEMBER: &str = "segment";
const TOTAL_SEGMENTS_MEMBER: &str = "totalSegments";
const NEXT_TOKEN_MEMBER: &str = "nextToken";
/// The members a BATCH request and its result carry: the tables it addresses, the keys a read
/// batch states per table, and the two members the result reports under.
const TABLES_MEMBER: &str = "tables";
const KEYS_MEMBER: &str = "keys";
const DATA_MEMBER: &str = "data";
const UNPROCESSED_ITEMS_MEMBER: &str = "unprocessedItems";
const UNPROCESSED_KEYS_MEMBER: &str = "unprocessedKeys";

/// The members of the result a selection answers with.
const ITEMS_MEMBER: &str = "items";
const SCANNED_COUNT_MEMBER: &str = "scannedCount";

/// The members of the DynamoDB input each call is made with, as that service's model names
/// them.
const TABLE_NAME_INPUT: &str = "TableName";
const KEY_INPUT: &str = "Key";
const ITEM_INPUT: &str = "Item";
const INDEX_NAME_INPUT: &str = "IndexName";
const LIMIT_INPUT: &str = "Limit";
const SELECT_INPUT: &str = "Select";
const SEGMENT_INPUT: &str = "Segment";
const TOTAL_SEGMENTS_INPUT: &str = "TotalSegments";
const CONSISTENT_READ_INPUT: &str = "ConsistentRead";
const SCAN_INDEX_FORWARD_INPUT: &str = "ScanIndexForward";
const RETURN_VALUES_INPUT: &str = "ReturnValues";
const EXCLUSIVE_START_KEY_INPUT: &str = "ExclusiveStartKey";
const UPDATE_EXPRESSION_INPUT: &str = "UpdateExpression";
const CONDITION_EXPRESSION_INPUT: &str = "ConditionExpression";
const KEY_CONDITION_EXPRESSION_INPUT: &str = "KeyConditionExpression";
const FILTER_EXPRESSION_INPUT: &str = "FilterExpression";
const PROJECTION_EXPRESSION_INPUT: &str = "ProjectionExpression";
const EXPRESSION_NAMES_INPUT: &str = "ExpressionAttributeNames";
const EXPRESSION_VALUES_INPUT: &str = "ExpressionAttributeValues";
/// The members a batch call's own input carries: the per-table map, the write requests it
/// holds and the keys a read batch states.
const REQUEST_ITEMS_INPUT: &str = "RequestItems";
const PUT_REQUEST_INPUT: &str = "PutRequest";
const DELETE_REQUEST_INPUT: &str = "DeleteRequest";
const KEYS_INPUT: &str = "Keys";

/// The values `ReturnValues` takes for the two calls whose result is an item they did not
/// read: the item as it now stands after an update, and as it stood before a delete.
const ALL_NEW: &str = "ALL_NEW";
const ALL_OLD: &str = "ALL_OLD";

/// The members of the output each call answers with.
const ITEM_OUTPUT: &str = "Item";
const ATTRIBUTES_OUTPUT: &str = "Attributes";
const ITEMS_OUTPUT: &str = "Items";
const SCANNED_COUNT_OUTPUT: &str = "ScannedCount";
const LAST_EVALUATED_KEY_OUTPUT: &str = "LastEvaluatedKey";
/// The members a batch call's own answer carries: what each table answered, and what it did
/// not get to.
const RESPONSES_OUTPUT: &str = "Responses";
const UNPROCESSED_ITEMS_OUTPUT: &str = "UnprocessedItems";
const UNPROCESSED_KEYS_OUTPUT: &str = "UnprocessedKeys";

/// The members an AWS JSON refusal names its class and states its sentence in.
const ERROR_TYPE_MEMBER: &str = "__type";
const ERROR_MESSAGE_MEMBER: &str = "message";
const ERROR_MESSAGE_MEMBER_CAPITALISED: &str = "Message";

/// The error class a call no identity-based policy allows is refused with, which every AWS
/// service words an authorization refusal as; DynamoDB's own model declares no error for it,
/// so a client raises it as that service's base exception ([`answered_error_type`]).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The `errorType` a step's failure is reported under, the two forms
/// [`super::event_bridge`]'s own errors carry: a request the data source cannot act on is a
/// failure of the resolver's own definition, so it is reported as one of that code
/// ([`super::data_source_role::REQUEST_FAULT_ERROR_TYPE`], the one spelling every calling data
/// source of this endpoint states it in); a refusal the OWNING service answered is reported as
/// that service and the class of the exception its client raised, the `<service>:<class>` form
/// a downstream refusal is read by.
const REQUEST_ERROR_TYPE: &str = super::data_source_role::REQUEST_FAULT_ERROR_TYPE;
const ANSWERED_ERROR_TYPE_PREFIX: &str = "DynamoDB:";

/// The sentences a step that did not resolve states.
///
/// AppSync publishes no wording for a request its own data source cannot act on, so these are
/// this emulator's own sentences, written as [`super::event_bridge`]'s are: each names the rule
/// the request broke and ends in the member of the request object a client has to correct.
/// Nothing of one request's own values is written into them.
const NO_TABLE: &str = "No table is configured for the AMAZON_DYNAMODB data source";
const ONE_OF_THE_OPERATIONS: &str = "An AMAZON_DYNAMODB data source serves the operations";
const AN_OPERATION_STATES: &str = "This AMAZON_DYNAMODB operation states the item it addresses in";
const A_PUT_STATES: &str = "A PutItem request states the values of the item it writes in";
const AN_UPDATE_STATES: &str = "An UpdateItem request states the update it makes in";
const A_QUERY_STATES: &str = "A Query request states the key condition it selects by in";
const A_BATCH_STATES: &str = "A batch request states the tables it addresses in";
const A_BATCH_TABLE_IS_A_LIST: &str =
    "A write batch states the items or keys of each table it addresses as a list in";
const A_BATCH_TABLE_IS_NOT_EMPTY: &str =
    "A batch addresses at least one item of every table it names, and none is stated in";
const A_BATCH_GET_STATES: &str = "A BatchGetItem request states the keys it reads in";
const A_WRITE_BATCH_CARRIES: &str = "A write batch carries at most";
const A_READ_BATCH_CARRIES: &str = "A BatchGetItem request reads at most";
const ITEMS_OR_KEYS_IN_ALL: &str = "items across the tables it addresses.";

/// How many writes one write batch may carry, across every table it names.
///
/// The limit of the ONE call it is made as: "a single call to `BatchWriteItem` can transmit up
/// to 16MB of data over the network, consisting of up to 25 item put or delete operations"
/// (Amazon DynamoDB's `BatchWriteItem`). It is also what bounds the work ONE GraphQL request
/// can ask of this data source: the permissions of every table named are weighed before the
/// call is made, so a request past this limit is refused before any of that is done.
const MAX_BATCH_WRITE_REQUESTS: usize = 25;

/// How many keys one read batch may read, across every table it names: "a single operation can
/// retrieve up to 16 MB of data, which can contain as many as 100 items"
/// (Amazon DynamoDB's `BatchGetItem`). Bounds the same work for the same reason.
const MAX_BATCH_GET_KEYS: usize = 100;
const A_MEMBER_IS_A_MAP: &str =
    "This member of an AMAZON_DYNAMODB request is a JSON object, which is not what is carried \
     at";
const A_FLAG_IS_A_BOOLEAN: &str =
    "This member of an AMAZON_DYNAMODB request is true or false, which is not what is carried \
     at";
const A_COUNT_IS_A_NUMBER: &str =
    "This member of an AMAZON_DYNAMODB request is a whole number, which is not what is carried \
     at";
const NO_CALLER_CREDENTIALS: &str =
    "This AMAZON_DYNAMODB data source reaches its table with the credentials of the caller, and \
     this request presented none, so no call was made.";
const AN_EXPRESSION_IS_TEXT: &str =
    "An expression of an AMAZON_DYNAMODB request is text, which is not what is carried at";
const NOT_A_NEXT_TOKEN: &str =
    "The pagination token of an AMAZON_DYNAMODB request is one this endpoint issued, which is \
     not what is carried at";
const NOT_SIGNED: &str = "This field's DynamoDB call could not be signed, so none was made:";
const NOT_CALLED: &str = "This field's DynamoDB call could not be made:";
const NO_SUCH_SERVICE: &str =
    "This field's DynamoDB call reached no Amazon DynamoDB in this emulator, so the table was \
     not read.";
const NOT_ANSWERED: &str = "This field's DynamoDB call did not succeed.";
const UNKNOWN_ERROR_CODE: &str = "Unknown";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every attribute value of an answer is unmarshalled to the plain value it holds,
    /// a list and a map through the same reading, and a number — which DynamoDB carries as
    /// text — as the number it stands for. The rows span the attribute types DynamoDB holds a
    /// value under.
    #[test]
    fn an_answers_attribute_values_are_read_as_the_values_they_hold() {
        let rows: &[(&str, Json, Json)] = &[
            ("a string", json!({"S": "alpha"}), json!("alpha")),
            ("a whole number", json!({"N": "7"}), json!(7)),
            ("a fractional number", json!({"N": "1.5"}), json!(1.5)),
            ("a boolean", json!({"BOOL": false}), json!(false)),
            ("a null", json!({"NULL": true}), Json::Null),
            ("binary", json!({"B": "YWI="}), json!("YWI=")),
            (
                "a list of mixed values",
                json!({"L": [{"S": "a"}, {"N": "2"}, {"NULL": true}]}),
                json!(["a", 2, Json::Null]),
            ),
            (
                "a nested map",
                json!({"M": {"inner": {"M": {"deep": {"S": "value"}}}}}),
                json!({"inner": {"deep": "value"}}),
            ),
            (
                "a string set",
                json!({"SS": ["a", "b"]}),
                json!(["a", "b"]),
            ),
            ("a number set", json!({"NS": ["1", "2"]}), json!([1, 2])),
            (
                "an item, which is a map of attribute values",
                json!({"id": {"S": "A-1"}, "count": {"N": "3"}}),
                json!({"id": "A-1", "count": 3}),
            ),
        ];
        for (case, held, expected) in rows {
            let read = plain_value(held);
            assert_eq!(
                &read, expected,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }

    /// RULE: the request object's `operation` says which call is made, and every attribute
    /// value it carries travels into that call's own input unchanged. The rows span the
    /// operations this data source serves.
    #[test]
    fn each_operation_builds_the_call_dynamodb_takes() {
        let key = json!({"id": {"S": "A-1"}});
        let rows: &[(&str, Json, Json)] = &[
            (
                "a GetItem of one key",
                json!({"operation": "GetItem", "key": key.clone(), "consistentRead": true}),
                json!({"TableName": "items", "Key": key.clone(), "ConsistentRead": true}),
            ),
            (
                "a PutItem of a key and its values",
                json!({
                    "operation": "PutItem",
                    "key": key.clone(),
                    "attributeValues": {"title": {"S": "first"}, "count": {"N": 3}},
                }),
                json!({
                    "TableName": "items",
                    // Every number travels as the TEXT of that number, which is the form the
                    // called service's own input takes.
                    "Item": {"id": {"S": "A-1"}, "title": {"S": "first"}, "count": {"N": "3"}},
                }),
            ),
            (
                "an UpdateItem with a condition",
                json!({
                    "operation": "UpdateItem",
                    "key": key.clone(),
                    "update": {
                        "expression": "SET #t = :t",
                        "expressionNames": {"#t": "title"},
                        "expressionValues": {":t": {"S": "next"}},
                    },
                    "condition": {
                        "expression": "attribute_exists(id)",
                    },
                }),
                json!({
                    "TableName": "items",
                    "Key": key.clone(),
                    "UpdateExpression": "SET #t = :t",
                    "ExpressionAttributeNames": {"#t": "title"},
                    "ExpressionAttributeValues": {":t": {"S": "next"}},
                    "ConditionExpression": "attribute_exists(id)",
                    "ReturnValues": "ALL_NEW",
                }),
            ),
            (
                "a DeleteItem of one key",
                json!({"operation": "DeleteItem", "key": key.clone()}),
                json!({"TableName": "items", "Key": key.clone(), "ReturnValues": "ALL_OLD"}),
            ),
            (
                "a Query whose filter contributes its own values",
                json!({
                    "operation": "Query",
                    "index": "by-owner",
                    "limit": 2,
                    "scanIndexForward": false,
                    "query": {
                        "expression": "#o = :o",
                        "expressionNames": {"#o": "owner"},
                        "expressionValues": {":o": {"S": "ada"}},
                    },
                    "filter": {
                        "expression": "#t = :t",
                        "expressionNames": {"#t": "title"},
                        "expressionValues": {":t": {"S": "first"}},
                    },
                }),
                json!({
                    "TableName": "items",
                    "IndexName": "by-owner",
                    "Limit": 2,
                    "ScanIndexForward": false,
                    "KeyConditionExpression": "#o = :o",
                    "FilterExpression": "#t = :t",
                    "ExpressionAttributeNames": {"#o": "owner", "#t": "title"},
                    "ExpressionAttributeValues": {":o": {"S": "ada"}, ":t": {"S": "first"}},
                }),
            ),
            (
                "a Scan of one segment",
                json!({
                    "operation": "Scan",
                    "segment": 0,
                    "totalSegments": 2,
                    "select": "ALL_ATTRIBUTES",
                }),
                json!({
                    "TableName": "items",
                    "Segment": 0,
                    "TotalSegments": 2,
                    "Select": "ALL_ATTRIBUTES",
                }),
            ),
        ];
        for (case, requested, expected) in rows {
            let operation = requested
                .get(OPERATION_MEMBER)
                .and_then(Json::as_str)
                .expect("the case names an operation");
            let call = Call::named(operation)
                .unwrap_or_else(|| panic!("case {case:?}: {operation} must be served"));
            let built = call
                .wire_request("items", requested)
                .unwrap_or_else(|refusal| panic!("case {case:?}: expected a call, got {refusal}"));
            assert_eq!(
                &built, expected,
                "case {case:?}: expected {expected}, got {built}"
            );
        }
    }

    /// RULE: a request this data source cannot act on is refused BEFORE any call, naming the
    /// member of the request object to correct. The rows span the members each operation needs.
    #[test]
    fn a_request_missing_what_its_operation_takes_is_refused() {
        let rows: &[(&str, Json, &str)] = &[
            (
                "a GetItem with no key",
                json!({"operation": "GetItem"}),
                "key",
            ),
            (
                "a PutItem with nothing to write",
                json!({"operation": "PutItem"}),
                "attributeValues",
            ),
            (
                "an UpdateItem with no update expression",
                json!({"operation": "UpdateItem", "key": {"id": {"S": "A-1"}}}),
                "update.expression",
            ),
            (
                "a Query with no key condition",
                json!({"operation": "Query"}),
                "query.expression",
            ),
            (
                "a Scan whose filter is not an object",
                json!({"operation": "Scan", "filter": "attribute_exists(id)"}),
                "filter",
            ),
            (
                "a Scan whose limit is no whole number",
                json!({"operation": "Scan", "limit": "ten"}),
                "limit",
            ),
            (
                "a GetItem whose consistentRead is no flag",
                json!({"operation": "GetItem", "key": {"id": {"S": "A-1"}}, "consistentRead": 1}),
                "consistentRead",
            ),
            (
                "a Scan carrying a token this endpoint did not issue",
                json!({"operation": "Scan", "nextToken": "not-a-token-of-this-endpoint"}),
                "nextToken",
            ),
        ];
        for (case, requested, member) in rows {
            let operation = requested
                .get(OPERATION_MEMBER)
                .and_then(Json::as_str)
                .expect("the case names an operation");
            let call = Call::named(operation)
                .unwrap_or_else(|| panic!("case {case:?}: {operation} must be served"));
            let refused = call.wire_request("items", requested);
            let refusal = refused
                .as_ref()
                .err()
                .unwrap_or_else(|| panic!("case {case:?}: expected a refusal, got {refused:?}"));
            assert!(
                refusal.ends_with(&format!("{member}.")),
                "case {case:?}: expected a refusal naming {member}, got {refusal:?}"
            );
        }
        // And an operation no DynamoDB data source serves is refused too, naming the ones it
        // does.
        assert!(
            Call::named("TransactWriteItems").is_none(),
            "case an operation this data source does not serve: expected no call"
        );
    }

    /// RULE: a BATCH operation addresses the tables its own request names — the items each
    /// writes, the keys each removes or reads — and what it came to is reported per table.
    #[test]
    fn a_batch_operation_addresses_the_tables_its_request_names() {
        let item = |id: &str| json!({"id": {"S": id}, "value": {"N": 7}});
        // The rows differ along the batch (write of items, delete of keys, read of keys), how
        // many tables it names and how many entries each carries.
        let put = Call::named("BatchPutItem").expect("BatchPutItem must be served");
        let built = put
            .wire_request(
                "items",
                &json!({"tables": {"items": [item("A-1"), item("A-2")]}}),
            )
            .unwrap_or_else(|refusal| panic!("the put batch must build: {refusal}"));
        assert_eq!(
            built,
            json!({"RequestItems": {"items": [
                {"PutRequest": {"Item": {"id": {"S": "A-1"}, "value": {"N": "7"}}}},
                {"PutRequest": {"Item": {"id": {"S": "A-2"}, "value": {"N": "7"}}}},
            ]}}),
            "a put batch must carry one put request per item, got {built}"
        );
        // What it came to is the items it carried, per table, beside what was not processed.
        let read = put.read_answer(&built, &json!({}));
        assert_eq!(
            read,
            json!({
                "data": {"items": [
                    {"id": "A-1", "value": 7},
                    {"id": "A-2", "value": 7},
                ]},
                "unprocessedItems": {},
            }),
            "a put batch must report the items it wrote per table, got {read}"
        );

        let delete = Call::named("BatchDeleteItem").expect("BatchDeleteItem must be served");
        let built = delete
            .wire_request("items", &json!({"tables": {"items": [{"id": {"S": "A-1"}}]}}))
            .unwrap_or_else(|refusal| panic!("the delete batch must build: {refusal}"));
        assert_eq!(
            built,
            json!({"RequestItems": {"items": [
                {"DeleteRequest": {"Key": {"id": {"S": "A-1"}}}},
            ]}}),
            "a delete batch must carry one delete request per key, got {built}"
        );

        let get = Call::named("BatchGetItem").expect("BatchGetItem must be served");
        let built = get
            .wire_request(
                "items",
                &json!({"tables": {
                    "items": {"keys": [{"id": {"S": "A-1"}}], "consistentRead": true},
                    "others": {"keys": [{"id": {"S": "B-1"}}]},
                }}),
            )
            .unwrap_or_else(|refusal| panic!("the get batch must build: {refusal}"));
        assert_eq!(
            built,
            json!({"RequestItems": {
                "items": {"Keys": [{"id": {"S": "A-1"}}], "ConsistentRead": true},
                "others": {"Keys": [{"id": {"S": "B-1"}}]},
            }}),
            "a get batch must carry the keys of every table it names, got {built}"
        );
        let read = get.read_answer(
            &built,
            &json!({
                "Responses": {"items": [{"id": {"S": "A-1"}}], "others": []},
                "UnprocessedKeys": {"others": {"Keys": [{"id": {"S": "B-1"}}]}},
            }),
        );
        assert_eq!(
            read,
            json!({
                "data": {"items": [{"id": "A-1"}], "others": []},
                "unprocessedKeys": {"others": [{"id": "B-1"}]},
            }),
            "a get batch must report what each table answered, got {read}"
        );

        // Every batch names the tables it addresses, and those are the tables the call's
        // permissions are weighed on; a request naming none is refused.
        assert_eq!(
            put.tables(
                "configured",
                &json!({"tables": {"items": [], "others": []}})
            ),
            vec!["items".to_string(), "others".to_string()],
            "a batch is weighed on the tables its request names"
        );
        assert_eq!(
            Call::GetItem.tables("configured", &json!({})),
            vec!["configured".to_string()],
            "a single-item call is weighed on the table the data source was configured with"
        );
        let refused = put.wire_request("items", &json!({"operation": "BatchPutItem"}));
        assert!(
            refused.is_err_and(|refusal| refusal.ends_with("tables.")),
            "a batch naming no table must be refused by that member"
        );
    }

    /// RULE: a batch is bounded by the limit of the ONE DynamoDB call it is made as, counted
    /// ACROSS the tables it names, and it addresses at least one item of every table it names
    /// — so neither the permissions weighed per table nor the call itself can be multiplied by
    /// a count the request chose.
    #[test]
    fn a_batch_past_the_limit_of_its_own_call_is_refused_before_any_of_it_is_weighed() {
        let items = |count: usize| -> Json {
            Json::Array(
                (0..count)
                    .map(|at| json!({"id": {"S": format!("A-{at}")}}))
                    .collect(),
            )
        };
        let write = |tables: Json| {
            Call::named("BatchPutItem")
                .expect("BatchPutItem must be served")
                .wire_request("items", &json!({"tables": tables}))
        };
        let read = |tables: Json| {
            Call::named("BatchGetItem")
                .expect("BatchGetItem must be served")
                .wire_request("items", &json!({"tables": tables}))
        };
        // The rows sit at each published bound, one past it, and one past it ACROSS two
        // tables, beside the table that addresses nothing.
        assert!(
            write(json!({"items": items(MAX_BATCH_WRITE_REQUESTS)})).is_ok(),
            "a write batch at the limit must build"
        );
        assert!(
            write(json!({"items": items(MAX_BATCH_WRITE_REQUESTS + 1)})).is_err(),
            "a write batch one past the limit must be refused"
        );
        assert!(
            write(json!({
                "items": items(MAX_BATCH_WRITE_REQUESTS),
                "others": items(1),
            }))
            .is_err(),
            "a write batch past the limit across two tables must be refused"
        );
        assert!(
            write(json!({"items": []})).is_err(),
            "a write batch naming a table it addresses nothing of must be refused"
        );
        let keys = |count: usize| json!({"keys": items(count)});
        assert!(
            read(json!({"items": keys(MAX_BATCH_GET_KEYS)})).is_ok(),
            "a read batch at the limit must build"
        );
        assert!(
            read(json!({"items": keys(MAX_BATCH_GET_KEYS + 1)})).is_err(),
            "a read batch one past the limit must be refused"
        );
        assert!(
            read(json!({"items": keys(MAX_BATCH_GET_KEYS), "others": keys(1)})).is_err(),
            "a read batch past the limit across two tables must be refused"
        );
        assert!(
            read(json!({"items": keys(0)})).is_err(),
            "a read batch naming a table it reads nothing of must be refused"
        );
    }

    /// RULE: the token a selection reports stands for the key the page ended on, and a token
    /// handed back continues from that very key — so the two are one round trip. The rows
    /// differ along the key's own attributes.
    #[test]
    fn a_pagination_token_round_trips_the_key_the_page_ended_on() {
        let rows: &[(&str, Json)] = &[
            ("a single string key", json!({"id": {"S": "A-1"}})),
            (
                "a composite key",
                json!({"owner": {"S": "ada"}, "seq": {"N": "12"}}),
            ),
        ];
        for (case, key) in rows {
            let token = encoded_token(key);
            let decoded = decoded_token(&token);
            assert_eq!(
                decoded.as_ref(),
                Some(key),
                "case {case:?}: expected the key back, got {decoded:?}"
            );
            assert!(
                !token.contains('{'),
                "case {case:?}: expected an opaque token, got {token:?}"
            );
        }
    }

    /// RULE: the result the response half is handed is what the call answered — the item for a
    /// single-item operation, `null` where there was none, and the page for a selection.
    #[test]
    fn the_result_is_what_the_call_answered() {
        let read = Call::GetItem.read_answer(&Json::Null, &json!({"Item": {"id": {"S": "A-1"}}}));
        assert_eq!(
            read,
            json!({"id": "A-1"}),
            "case an item that is there: expected the item, got {read}"
        );
        let missing = Call::GetItem.read_answer(&Json::Null, &json!({}));
        assert_eq!(
            missing,
            Json::Null,
            "case an item that is not there: expected null, got {missing}"
        );
        let page = Call::Query.read_answer(
            &Json::Null,
            &json!({
                "Items": [{"id": {"S": "A-1"}}],
                "ScannedCount": 3,
                "LastEvaluatedKey": {"id": {"S": "A-1"}},
            }),
        );
        assert_eq!(
            page["items"],
            json!([{"id": "A-1"}]),
            "case a page of one item: expected the item, got {page}"
        );
        assert_eq!(
            page["scannedCount"],
            json!(3),
            "case a page of one item: expected the count it examined, got {page}"
        );
        assert_eq!(
            decoded_token(page["nextToken"].as_str().unwrap_or_default()),
            Some(json!({"id": {"S": "A-1"}})),
            "case a page with more to read: expected a token for the key it ended on, got {page}"
        );
        let written = Call::PutItem.read_answer(
            &json!({"TableName": "items", "Item": {"id": {"S": "A-1"}, "n": {"N": "2"}}}),
            &json!({}),
        );
        assert_eq!(
            written,
            json!({"id": "A-1", "n": 2}),
            "case a put: expected the item it wrote, got {written}"
        );
    }

    /// RULE: a refusal the called service answered is reported as that service and its own
    /// error class, whatever spelling the answer carries its sentence under.
    #[test]
    fn a_refusal_the_service_answered_is_reported_as_its_own_class() {
        let rows: &[(&str, Json, &str, &str)] = &[
            (
                "a condition that did not hold, which the model declares",
                json!({
                    "__type": "com.amazonaws.dynamodb#ConditionalCheckFailedException",
                    "message": "The conditional request failed",
                }),
                "DynamoDB:ConditionalCheckFailedException",
                "The conditional request failed",
            ),
            (
                "a table that is not there, under the capitalised spelling",
                json!({
                    "__type": "ResourceNotFoundException",
                    "Message": "Requested resource not found",
                }),
                "DynamoDB:ResourceNotFoundException",
                "Requested resource not found",
            ),
            (
                "an authorization refusal, which the model declares no error for",
                json!({
                    "__type": "com.amazon.coral.service#AccessDeniedException",
                    "message": "User: a is not authorized to perform: dynamodb:GetItem",
                }),
                "DynamoDB:DynamoDbException",
                "User: a is not authorized to perform: dynamodb:GetItem",
            ),
            (
                "a request it could not read, which the model declares no error for either",
                json!({
                    "__type": "SerializationException",
                    "message": "NUMBER_VALUE cannot be converted to String",
                }),
                "DynamoDB:DynamoDbException",
                "NUMBER_VALUE cannot be converted to String",
            ),
            (
                "an answer naming no class at all",
                json!({}),
                "DynamoDB:DynamoDbException",
                NOT_ANSWERED,
            ),
        ];
        for (case, document, error_type, message) in rows {
            let read = (
                answered_error_type(&answered_code(document)),
                answered_message(document),
            );
            assert_eq!(
                (read.0.as_str(), read.1.as_str()),
                (*error_type, *message),
                "case {case:?}: expected {error_type} and {message:?}, got {read:?}"
            );
        }
    }
}
