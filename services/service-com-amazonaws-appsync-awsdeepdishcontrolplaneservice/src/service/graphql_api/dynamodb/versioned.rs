// SPDX-License-Identifier: CC0-1.0

//! What a WRITE through a VERSIONED `AMAZON_DYNAMODB` data source does: the two system
//! attributes AppSync owns on every item such a data source writes, the version comparison
//! that decides whether a write is applied, and the resolution strategy a write whose version
//! does not match is handed to.
//!
//! `dynamodbConfig.versioned` is "Set to TRUE to use Conflict Detection and Resolution with
//! this data source" (`DynamodbDataSourceConfig$versioned`), and that detection is made
//! possible by two attributes AppSync maintains on the item itself:
//!
//! * [`VERSION_ATTRIBUTE`] — the item's version: [`FIRST_VERSION`] on the write that creates
//!   it, and one more on every write accepted after that;
//! * [`LAST_CHANGED_AT_ATTRIBUTE`] — when that write happened, in epoch milliseconds.
//!
//! Both are written into the item the TABLE holds, not only into what the field answers, so a
//! client that reads the row back through any reader sees the version it has to carry into its
//! next write. A DELETE of such a data source leaves the row behind as a TOMBSTONE instead of
//! removing it — marked [`DELETED_ATTRIBUTE`], with its version advanced and
//! [`TTL_ATTRIBUTE`] saying when it may be reaped ("the number of minutes that an Item is
//! stored in the data source", `DeltaSyncConfig$baseTableTTL`) — because a client that syncs
//! changes by version has to be able to observe the deletion.
//!
//! The write the resolver asked for carries the version the caller believes is current (the
//! request object's own [`VERSION_ATTRIBUTE`]). Where the resolver's `syncConfig` asks for
//! version-based detection ("`VERSION`: Detect conflicts based on object versions",
//! `SyncConfig$conflictDetection`), that value has to equal the stored one for the write to be
//! applied; where it does not, the write is a CONFLICT and what becomes of it is the
//! `conflictHandler` the resolver names (`SyncConfig$conflictHandler`):
//!
//! * `OPTIMISTIC_CONCURRENCY` — "Resolve conflicts by rejecting mutations when versions don't
//!   match the latest version at the server", and so does a resolver naming `NONE` or naming no
//!   handler at all: nothing is written, and the field reports
//!   [`CONFLICT_UNHANDLED_ERROR_TYPE`] carrying the row as the server holds it, which is what a
//!   caller needs to build the write it retries with;
//! * `AUTOMERGE` — "Resolve conflicts with the Automerge conflict resolution strategy": the
//!   write is merged into the stored row without asking anyone ([`automerged`]) and the merged
//!   row is written as the item's next version;
//! * `LAMBDA` — "Resolve conflicts with an Lambda function supplied in the
//!   `LambdaConflictHandlerConfig`": that function is INVOKED with the row as it stands and the
//!   row the write would make, and its answer decides — a resolved item is written as the next
//!   version, and anything else leaves the row as it was and reports the conflict.
//!
//! That function is invoked under no role of the data source: `serviceRoleArn` is the role "the
//! system assumes … when accessing the data source" (`DataSource$serviceRoleArn`) and a
//! conflict handler is not that data source — the only thing the model carries about it is "the
//! Amazon Resource Name (ARN) for the Lambda function to use as the Conflict Handler"
//! (`LambdaConflictHandlerConfig$lambdaConflictHandlerArn`).

use super::super::lambda;
use super::super::mapping_template::utilities::dynamodb::attribute_value;
use super::*;

/// The attribute carrying the item's version, which is also the member a request object states
/// the version it expects in.
pub const VERSION_ATTRIBUTE: &str = "_version";

/// The attribute carrying when the item was last written, in epoch milliseconds.
pub const LAST_CHANGED_AT_ATTRIBUTE: &str = "_lastChangedAt";

/// The attribute marking the row a versioned delete left behind, and the one saying when that
/// row may be reaped.
const DELETED_ATTRIBUTE: &str = "_deleted";
const TTL_ATTRIBUTE: &str = "_ttl";

/// The version the write that CREATES an item stamps, and how much each accepted write after it
/// advances the version by.
const FIRST_VERSION: i64 = 1;
const VERSION_STEP: i64 = 1;

/// The `conflictDetection` value that compares versions (`ConflictDetectionType`, whose other
/// value detects nothing).
const VERSION_DETECTION: &str = "VERSION";

/// The `conflictHandler` values that RESOLVE a conflict rather than reject it
/// (`ConflictHandlerType`); every other value, `NONE` and `OPTIMISTIC_CONCURRENCY` among them,
/// rejects the write.
const AUTOMERGE_HANDLER: &str = "AUTOMERGE";
const LAMBDA_HANDLER: &str = "LAMBDA";

/// The error class a mutation whose conflict was NOT resolved is reported under, and the
/// sentence AppSync states for it. Both are fixed text: nothing of the request or of the stored
/// row is written into them, because the row itself travels beside the error.
const CONFLICT_UNHANDLED_ERROR_TYPE: &str = "ConflictUnhandled";
const CONFLICT_REJECTED_MESSAGE: &str = "Conflict resolver rejects mutation.";

/// The members of the event a LAMBDA conflict handler is invoked with: the row the write would
/// make, the row as the server holds it, the arguments of the field being resolved, which
/// resolver of which table the write is of, and who is making it.
const NEW_ITEM_MEMBER: &str = "newItem";
const EXISTING_ITEM_MEMBER: &str = "existingItem";
const ARGUMENTS_MEMBER: &str = "arguments";
const RESOLVER_MEMBER: &str = "resolver";
const IDENTITY_MEMBER: &str = "identity";
const TABLE_NAME_MEMBER: &str = "tableName";
const AWS_REGION_MEMBER: &str = "awsRegion";
const PARENT_TYPE_MEMBER: &str = "parentType";
const FIELD_MEMBER: &str = "field";

/// The members of the field's own `info` that say WHICH resolver the write is of.
const FIELD_NAME_INFO: &str = "fieldName";
const PARENT_TYPE_NAME_INFO: &str = "parentTypeName";

/// The members of the decision such a handler answers with: the action it decided and, for the
/// one action that supplies a row, that row.
const ACTION_MEMBER: &str = "action";
const ITEM_MEMBER: &str = "item";

/// The one action that RESOLVES the conflict with a row of the handler's own, and the one that
/// REMOVES the item; every other answer leaves the stored row alone.
const RESOLVE_ACTION: &str = "RESOLVE";
const REMOVE_ACTION: &str = "REMOVE";

/// The prefix the placeholders of this module's own update expressions carry, so no name or
/// value a request object states can collide with them.
const PLACEHOLDER_PREFIX: &str = "appsyncSystem";

/// How many seconds a minute of a retention window is, and how many milliseconds a second is:
/// the window is stated in minutes and the reaping marker is an epoch-SECOND stamp, while the
/// write's own stamp is in milliseconds.
const SECONDS_PER_MINUTE: i64 = 60;
const MILLIS_PER_SECOND: i64 = 1_000;

/// How deep into a row a merge composes: "attribute values can be nested up to 32 levels deep"
/// (Amazon DynamoDB's item limits), so a value nested deeper than an item may be is one no merge
/// composes — the value at the server stands — and this is what bounds the walk.
const MAX_ATTRIBUTE_DEPTH: usize = 32;

/// The sentences a versioned write is refused with, each naming what is missing rather than
/// anything one request carried.
const NO_CONFLICT_HANDLER: &str =
    "This resolver resolves conflicts with a LAMBDA conflict handler and names no function to \
     invoke in syncConfig.lambdaConflictHandlerConfig.lambdaConflictHandlerArn, so the write \
     was not made.";
const NOT_A_CONFLICT_HANDLER: &str =
    "This resolver's syncConfig.lambdaConflictHandlerConfig.lambdaConflictHandlerArn names no \
     Lambda function, so the write was not made.";

/// The `errorType` a conflict handler that could not be invoked at all is reported under: the
/// one spelling this endpoint reports a failed invocation in.
const HANDLER_NOT_INVOKED_ERROR_TYPE: &str = super::super::execution::LAMBDA_INVOCATION_ERROR_TYPE;

/// One write of a versioned data source: the three calls that change ONE item, which are the
/// calls AppSync's conflict detection is defined over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    Put,
    Update,
    Delete,
}

impl Write {
    /// The versioned write `call` is, or `None` for a call that changes no single item — a read,
    /// a selection, or a batch, none of which that detection covers.
    pub(super) fn of(call: Call) -> Option<Self> {
        match call {
            Call::PutItem => Some(Self::Put),
            Call::UpdateItem => Some(Self::Update),
            Call::DeleteItem => Some(Self::Delete),
            _ => None,
        }
    }
}

/// The write as the call built for it states it: which of the three it is, the table it is made
/// against and the Region that table is in, the request object the resolver's request half
/// returned, and the DynamoDB input built from it.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    pub write: Write,
    pub region: &'a str,
    pub table: &'a str,
    pub requested: &'a Json,
    pub body: &'a Json,
}

/// What the RESOLVER making this write asks of a conflict — `SyncConfig` as the stored resolver
/// (or the pipeline function) carries it — and the two parts of the field's own context a
/// LAMBDA conflict handler's event carries.
#[derive(Debug, Clone, Copy)]
pub struct Syncing<'a> {
    /// `syncConfig.conflictDetection`: whether versions are compared at all.
    pub detection: Option<&'a str>,
    /// `syncConfig.conflictHandler`: what becomes of a write whose version does not match.
    pub handler: Option<&'a str>,
    /// `syncConfig.lambdaConflictHandlerConfig.lambdaConflictHandlerArn`.
    pub handler_arn: Option<&'a str>,
    /// The arguments of the field being resolved.
    pub arguments: &'a Json,
    /// The field's own `info`, which says which field of which type is being resolved.
    pub info: &'a Json,
    /// Who the request presented itself as.
    pub identity: &'a Json,
}

impl Syncing<'_> {
    /// Whether this resolver asks for the version comparison: only the value naming it does, so
    /// a resolver carrying no sync configuration compares nothing and its writes are applied as
    /// they stand.
    ///
    /// What is compared is the version the REQUEST OBJECT states against the stored one, so a
    /// request that states none makes no claim about the version it read: there is nothing to
    /// detect a conflict against, and such a write is applied and advances the version like any
    /// other accepted one.
    fn detects_versions(&self) -> bool {
        self.detection == Some(VERSION_DETECTION)
    }
}

/// What resolving one conflict came to.
enum Decided {
    /// Nothing is written and the mutation is reported as a conflict that was not resolved.
    Reject,
    /// This row — in the attribute-value form DynamoDB holds one — is written as the item's next
    /// version.
    Write {
        row: JsonMap<String, Json>,
        /// Whether that row is the one a REMOVAL leaves, which carries the deletion marker and
        /// the reaping marker beside the version.
        tombstone: bool,
    },
}

/// The system attributes ONE write stamps onto the row it leaves: the version it makes, when it
/// happened, whether the row is the tombstone a delete left, and how long such a row is
/// retained.
#[derive(Debug, Clone, Copy)]
struct Stamp {
    version: i64,
    changed_at: i64,
    deleted: bool,
    /// The retention window in minutes, or `None` where the data source states none — a
    /// tombstone then carries no reaping marker at all.
    retained: Option<i64>,
}

/// Make the versioned write `asked` describes and answer the result the step's response half is
/// handed.
///
/// `asked.body` is the call as [`Call::wire_request`] built it, so the update expression, the
/// condition and every attribute value are exactly the ones the resolver's request object
/// stated; what this adds is the system attributes and the version comparison.
pub async fn written(
    context: &PlaneContext,
    source: &Configured<'_>,
    sync: &Syncing<'_>,
    asked: &Asked<'_>,
) -> Result<Json, Refused> {
    let key = wire_values(&object(asked.requested, KEY_MEMBER).map_err(Refused::request)?);
    let held = stored(context, asked, &key).await?;
    // A delete of a row that is not there removes nothing and leaves no tombstone: there is no
    // version to advance and nothing for a client to sync, so the call is made as it stands.
    let Some(held) = held else {
        if asked.write == Write::Delete {
            let answered = called(
                context,
                asked.region,
                Call::DeleteItem.target(),
                asked.body,
            )
            .await?;
            return Ok(plain_member(&answered, ATTRIBUTES_OUTPUT));
        }
        return applied(context, source, asked, &key, None).await;
    };
    let stated = stated_version(asked.requested);
    let mismatched = sync.detects_versions()
        && stated.is_some_and(|stated| stated != version_held(&held).unwrap_or_default());
    if !mismatched {
        return applied(context, source, asked, &key, Some(&held)).await;
    }
    let handler = sync.handler.unwrap_or_default();
    if handler != AUTOMERGE_HANDLER && handler != LAMBDA_HANDLER {
        return Err(rejected(&held));
    }
    // Both strategies decide between the row as it stands and the row this write WOULD make, so
    // that second row is composed first ([`intended`]).
    let intended = intended(context, asked, &held).await?;
    let decided = match handler {
        AUTOMERGE_HANDLER => automerge(asked.write, &held, &intended),
        _ => handled(context, sync, asked, &held, &intended).await?,
    };
    match decided {
        Decided::Reject => Err(rejected(&held)),
        Decided::Write { row, tombstone } => {
            let stamp = Stamp {
                version: version_held(&held)
                    .unwrap_or_default()
                    .saturating_add(VERSION_STEP),
                changed_at: now_millis(),
                deleted: tombstone,
                retained: retention(source),
            };
            // The row a STRATEGY composed is written under no condition of the request's own:
            // the condition the caller stated is the one its own write was to be refused on
            // (`attribute_not_exists` of a row it believed absent, say), and the conflict it is
            // being resolved from is proof that write is not the one being made.
            put(context, asked, row, &key, &stamp, None).await
        }
    }
}

/// The item `key` addresses as the table holds it NOW, or `None` for a key holding none.
///
/// Read CONSISTENTLY: the version this write is compared against has to be the one the last
/// accepted write left, not one an eventually consistent read may still be serving.
async fn stored(
    context: &PlaneContext,
    asked: &Asked<'_>,
    key: &Json,
) -> Result<Option<Json>, Refused> {
    let input = json!({
        TABLE_NAME_INPUT: asked.table,
        KEY_INPUT: key.clone(),
        CONSISTENT_READ_INPUT: true,
    });
    let answered = called(context, asked.region, Call::GetItem.target(), &input).await?;
    Ok(answered
        .get(ITEM_OUTPUT)
        .filter(|item| item.is_object())
        .cloned())
}

/// Carry out the write and answer the item as it now stands, with the system attributes
/// advanced: the next version after the one `held` carries, and this write's own time.
async fn applied(
    context: &PlaneContext,
    source: &Configured<'_>,
    asked: &Asked<'_>,
    key: &Json,
    held: Option<&Json>,
) -> Result<Json, Refused> {
    let version = held
        .and_then(version_held)
        .map_or(FIRST_VERSION, |held| held.saturating_add(VERSION_STEP));
    let stamp = Stamp {
        version,
        changed_at: now_millis(),
        deleted: asked.write == Write::Delete,
        retained: retention(source),
    };
    match asked.write {
        // The item the request describes IS the row written, so the system attributes are
        // stamped onto it and the one call writes the row the field then answers.
        Write::Put => {
            let item = asked
                .body
                .get(ITEM_INPUT)
                .and_then(Json::as_object)
                .cloned()
                .unwrap_or_default();
            put(context, asked, item, key, &stamp, Some(asked.body)).await
        }
        // The update is the resolver's own — its expression, its condition and its values — so
        // DynamoDB applies it, and the system attributes are stamped once it has: a write that
        // service refused advances no version, because the stamp is never reached.
        Write::Update => {
            called(
                context,
                asked.region,
                Call::UpdateItem.target(),
                asked.body,
            )
            .await?;
            let stamped = system_update(asked.table, key, &stamp, None);
            let answered =
                called(context, asked.region, Call::UpdateItem.target(), &stamped).await?;
            Ok(plain_member(&answered, ATTRIBUTES_OUTPUT))
        }
        // A versioned delete leaves the row behind as a tombstone, so what it makes is an
        // update: the deletion marker, the advanced version and the reaping marker, under the
        // condition the request stated for the delete it asked for.
        Write::Delete => {
            let tombstone = system_update(asked.table, key, &stamp, Some(asked.body));
            let answered =
                called(context, asked.region, Call::UpdateItem.target(), &tombstone).await?;
            Ok(plain_member(&answered, ATTRIBUTES_OUTPUT))
        }
    }
}

/// How long the row a versioned delete leaves behind is retained, in minutes, or `None` where
/// the data source states no window.
///
/// "The number of minutes that an Item is stored in the data source"
/// (`DeltaSyncConfig$baseTableTTL`) of the data source being written.
fn retention(source: &Configured<'_>) -> Option<i64> {
    Some(source.base_table_ttl).filter(|minutes| *minutes > 0)
}

/// Write `item` as the item `key` addresses, with the system attributes `stamp` states, and
/// answer the row as it now stands.
///
/// The key the write ADDRESSED is written onto the row: the row belongs to that key whatever
/// the item handed here carries, which is what keeps a row a conflict handler resolved from
/// landing anywhere else. A row the stamp marks deleted is the tombstone a removal leaves, so it
/// carries the deletion marker and the reaping marker beside the version.
///
/// `carried` is the call whose CONDITION travels with this write ([`carry_condition`]).
async fn put(
    context: &PlaneContext,
    asked: &Asked<'_>,
    item: JsonMap<String, Json>,
    key: &Json,
    stamp: &Stamp,
    carried: Option<&Json>,
) -> Result<Json, Refused> {
    let mut written = item;
    if let Some(addressed) = key.as_object() {
        written.extend(
            addressed
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
    }
    for (attribute, value) in stamped(stamp) {
        written.insert(attribute.to_string(), value);
    }
    let item = Json::Object(written);
    let mut input = JsonMap::new();
    input.insert(
        TABLE_NAME_INPUT.to_string(),
        Json::String(asked.table.to_string()),
    );
    input.insert(ITEM_INPUT.to_string(), item.clone());
    carry_condition(&mut input, carried);
    called(
        context,
        asked.region,
        Call::PutItem.target(),
        &Json::Object(input),
    )
    .await?;
    Ok(plain_value(&item))
}

/// The system attributes `stamp` writes onto the row it leaves, in the order an expression
/// assigning them names them: the version this write makes and when it happened, and — for the
/// tombstone a removal leaves — the deletion marker and the reaping marker beside them.
///
/// The one reading of what a write stamps, so the row a put writes and the row an update's stamp
/// assigns carry the same attributes.
fn stamped(stamp: &Stamp) -> Vec<(&'static str, Json)> {
    let mut written: Vec<(&'static str, Json)> = vec![
        (VERSION_ATTRIBUTE, whole_attribute(stamp.version)),
        (
            LAST_CHANGED_AT_ATTRIBUTE,
            whole_attribute(stamp.changed_at),
        ),
    ];
    if stamp.deleted {
        written.push((DELETED_ATTRIBUTE, json!({BOOLEAN: true})));
        if let Some(minutes) = stamp.retained {
            written.push((
                TTL_ATTRIBUTE,
                whole_attribute(reaped_at(stamp.changed_at, minutes)),
            ));
        }
    }
    written
}

/// Carry the CONDITION the request object stated onto `input`, and the placeholders that
/// condition names beside it, where the call built for the write states one.
///
/// A write the caller ASKED for is refused on exactly the grounds that caller stated, whether the
/// versioned write it becomes is a put or the update a tombstone is written by. Only a stated
/// condition brings names and values with it, and only then are they carried: DynamoDB refuses a
/// call stating a placeholder no expression of it uses. The placeholders this module's own
/// expressions use carry a prefix of their own ([`PLACEHOLDER_PREFIX`]), so nothing a condition
/// names can collide with them.
fn carry_condition(input: &mut JsonMap<String, Json>, carried: Option<&Json>) {
    let Some(condition) = carried.and_then(|call| call.get(CONDITION_EXPRESSION_INPUT)) else {
        return;
    };
    input.insert(CONDITION_EXPRESSION_INPUT.to_string(), condition.clone());
    for member in [EXPRESSION_NAMES_INPUT, EXPRESSION_VALUES_INPUT] {
        let Some(Json::Object(stated)) = carried.and_then(|call| call.get(member)) else {
            continue;
        };
        let mut held = match input.get(member) {
            Some(Json::Object(held)) => held.clone(),
            _ => JsonMap::new(),
        };
        held.extend(
            stated
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        input.insert(member.to_string(), Json::Object(held));
    }
}

/// The row this write WOULD make, in the attribute-value form DynamoDB holds one: what a
/// conflict handler is shown, and what AUTOMERGE merges.
///
/// A put describes its row outright. An UPDATE describes it as an expression in DynamoDB's own
/// update language, so that service — the one that defines the language — is what applies it:
/// the update is made, the row it produced is read back, and the row as it stood is written
/// again, so a conflict that is then rejected leaves exactly the row it started from. That
/// restoring write is GUARDED by the system attributes the row carried ([`restoring_put`]), so a
/// write accepted in between is not discarded silently but refused. A delete would make the
/// tombstone, which is the row as it stands marked deleted.
async fn intended(
    context: &PlaneContext,
    asked: &Asked<'_>,
    held: &Json,
) -> Result<Json, Refused> {
    match asked.write {
        Write::Put => Ok(asked.body.get(ITEM_INPUT).cloned().unwrap_or(Json::Null)),
        Write::Update => {
            let answered = called(
                context,
                asked.region,
                Call::UpdateItem.target(),
                asked.body,
            )
            .await?;
            let restored = restoring_put(asked.table, held);
            called(context, asked.region, Call::PutItem.target(), &restored).await?;
            Ok(answered
                .get(ATTRIBUTES_OUTPUT)
                .cloned()
                .unwrap_or(Json::Null))
        }
        Write::Delete => {
            let mut tombstone = held.as_object().cloned().unwrap_or_default();
            tombstone.insert(DELETED_ATTRIBUTE.to_string(), json!({BOOLEAN: true}));
            Ok(Json::Object(tombstone))
        }
    }
}

/// The `PutItem` call that writes `held` back as the row stood, guarded so a write accepted in
/// between is not discarded.
///
/// The guard is the system attributes that row carried: every accepted write advances both, so a
/// restore whose condition no longer holds is one another write landed before — and DynamoDB
/// REFUSES it, which the step reports as that service worded it rather than overwriting what was
/// accepted. A row carrying neither attribute has nothing to guard with and is written back as it
/// stood.
fn restoring_put(table: &str, held: &Json) -> Json {
    let mut input = JsonMap::new();
    input.insert(TABLE_NAME_INPUT.to_string(), Json::String(table.to_string()));
    input.insert(ITEM_INPUT.to_string(), held.clone());
    let mut names = JsonMap::new();
    let mut values = JsonMap::new();
    let mut tests: Vec<String> = Vec::new();
    for (at, attribute) in [VERSION_ATTRIBUTE, LAST_CHANGED_AT_ATTRIBUTE]
        .iter()
        .enumerate()
    {
        let Some(carried) = held.get(*attribute) else {
            continue;
        };
        let name = format!("#{PLACEHOLDER_PREFIX}{at}");
        let placeholder = format!(":{PLACEHOLDER_PREFIX}{at}");
        names.insert(name.clone(), Json::String((*attribute).to_string()));
        values.insert(placeholder.clone(), carried.clone());
        tests.push(format!("{name} = {placeholder}"));
    }
    if !tests.is_empty() {
        input.insert(
            CONDITION_EXPRESSION_INPUT.to_string(),
            Json::String(tests.join(" AND ")),
        );
        input.insert(EXPRESSION_NAMES_INPUT.to_string(), Json::Object(names));
        input.insert(EXPRESSION_VALUES_INPUT.to_string(), Json::Object(values));
    }
    Json::Object(input)
}

/// What AUTOMERGE makes of this conflict.
///
/// The strategy merges the two rows rather than asking anyone, so it never rejects a write it has
/// a row for: the merged row is written as the item's next version. The row a conflicting DELETE
/// would make is the stored row MARKED deleted ([`intended`]), so merging it in adds that marker
/// and the removal goes through as the next version, which is the same reading as for every other
/// write — what the server holds for an attribute both rows carry stands, and what the write adds
/// is added.
fn automerge(write: Write, held: &Json, intended: &Json) -> Decided {
    let held = held.as_object().cloned().unwrap_or_default();
    let Some(incoming) = intended.as_object() else {
        return Decided::Reject;
    };
    Decided::Write {
        row: automerged(&held, incoming, 0),
        tombstone: write == Write::Delete,
    }
}

/// The row AUTOMERGE writes: the stored row, with everything the incoming row adds.
///
/// An attribute the stored row does not carry is ADDED. An attribute both carry is merged by
/// what it holds: a list keeps the stored elements and gains the incoming ones it does not
/// already carry, a set is the union of the two, and a map is merged member by member through
/// this same reading. Anything else — two scalars, or two values of different attribute types —
/// is a conflict the merge cannot compose, and the value at the SERVER stands.
///
/// `depth` is how deep into the row this merge already is, and it goes no deeper than an item may
/// be nested ([`MAX_ATTRIBUTE_DEPTH`]), which is what bounds this walk.
fn automerged(
    held: &JsonMap<String, Json>,
    incoming: &JsonMap<String, Json>,
    depth: usize,
) -> JsonMap<String, Json> {
    let mut merged = held.clone();
    if depth >= MAX_ATTRIBUTE_DEPTH {
        return merged;
    }
    for (name, value) in incoming {
        // The system attributes are AppSync's own: the write that follows stamps them, so what
        // either row carries for them is not merged.
        if name == VERSION_ATTRIBUTE || name == LAST_CHANGED_AT_ATTRIBUTE {
            continue;
        }
        match merged.get(name) {
            None => {
                merged.insert(name.clone(), value.clone());
            }
            Some(stored) => {
                if let Some(composed) = merged_attribute(stored, value, depth) {
                    merged.insert(name.clone(), composed);
                }
            }
        }
    }
    merged
}

/// How the two values of ONE attribute merge, or `None` where the stored value stands.
fn merged_attribute(stored: &Json, incoming: &Json, depth: usize) -> Option<Json> {
    let (kind, held) = attribute(stored)?;
    let (incoming_kind, offered) = attribute(incoming)?;
    if kind != incoming_kind {
        return None;
    }
    match (kind, held, offered) {
        (LIST | STRING_SET | NUMBER_SET | BINARY_SET, Json::Array(held), Json::Array(offered)) => {
            let mut composed = JsonMap::new();
            composed.insert(kind.to_string(), Json::Array(appended(held, offered)));
            Some(Json::Object(composed))
        }
        (MAP, Json::Object(held), Json::Object(offered)) => {
            let mut composed = JsonMap::new();
            composed.insert(
                kind.to_string(),
                Json::Object(automerged(held, offered, depth.saturating_add(1))),
            );
            Some(Json::Object(composed))
        }
        _ => None,
    }
}

/// The stored elements, and the offered ones they do not already carry — so merging one write
/// twice composes the same list.
fn appended(held: &[Json], offered: &[Json]) -> Vec<Json> {
    let mut composed = held.to_vec();
    for element in offered {
        if !composed.contains(element) {
            composed.push(element.clone());
        }
    }
    composed
}

/// The attribute type `value` is of and the content it holds, or `None` for a value that is no
/// single attribute value.
fn attribute(value: &Json) -> Option<(&str, &Json)> {
    let members = value.as_object()?;
    let mut named = members.iter();
    let (kind, content) = named.next()?;
    if named.next().is_some() {
        return None;
    }
    Some((kind.as_str(), content))
}

/// What the resolver's LAMBDA conflict handler made of this conflict.
///
/// The function is invoked with the row as the server holds it and the row the write would make,
/// both as the PLAIN values a handler reads, beside the field's arguments, who is making the
/// write, and which resolver of which table it is of. Its answer decides: the action that
/// supplies a row resolves the conflict with that row, the action that REMOVES the item leaves
/// the tombstone a versioned removal leaves, and every other answer — the rejection, and an
/// answer carrying no row to write — leaves the stored row alone.
///
/// A handler that could not be invoked, or that RAISED, is no decision at all: nothing is
/// written and the step reports what went wrong, so a broken handler never silently applies or
/// drops a write.
async fn handled(
    context: &PlaneContext,
    sync: &Syncing<'_>,
    asked: &Asked<'_>,
    held: &Json,
    intended: &Json,
) -> Result<Decided, Refused> {
    let Some(configured) = sync.handler_arn.filter(|arn| !arn.trim().is_empty()) else {
        return Err(Refused::request(NO_CONFLICT_HANDLER.to_string()));
    };
    let Some(target) = lambda::function_target(configured) else {
        return Err(Refused::request(NOT_A_CONFLICT_HANDLER.to_string()));
    };
    let event = json!({
        NEW_ITEM_MEMBER: plain_value(intended),
        EXISTING_ITEM_MEMBER: plain_value(held),
        ARGUMENTS_MEMBER: sync.arguments.clone(),
        RESOLVER_MEMBER: {
            TABLE_NAME_MEMBER: asked.table,
            AWS_REGION_MEMBER: asked.region,
            PARENT_TYPE_MEMBER: sync.info.get(PARENT_TYPE_NAME_INFO).cloned(),
            FIELD_MEMBER: sync.info.get(FIELD_NAME_INFO).cloned(),
        },
        IDENTITY_MEMBER: sync.identity.clone(),
    });
    let ran = match lambda::invoke(&context.callback, &target, &event).await {
        Ok(ran) => ran,
        Err(unreached) => {
            return Err(Refused {
                message: unreached.reason(),
                error_type: HANDLER_NOT_INVOKED_ERROR_TYPE.to_string(),
                data: None,
            })
        }
    };
    if ran.failed() {
        let (class, message) = ran.raised();
        return Err(Refused {
            message,
            error_type: class,
            data: None,
        });
    }
    let decision = ran.value();
    let action = decision
        .get(ACTION_MEMBER)
        .and_then(Json::as_str)
        .unwrap_or_default();
    match action {
        // The row the handler composed is a PLAIN value, as the rows it was shown were, so it is
        // held as the attribute values DynamoDB holds a row as. An answer that resolves the
        // conflict with no row at all supplies nothing to write, so the stored row stands.
        RESOLVE_ACTION => Ok(match marshalled(decision.get(ITEM_MEMBER)) {
            Some(row) => Decided::Write {
                row,
                tombstone: false,
            },
            None => Decided::Reject,
        }),
        // The item is REMOVED, which on a versioned data source is the tombstone: the row as it
        // stands, marked deleted, as its next version.
        REMOVE_ACTION => Ok(Decided::Write {
            row: held.as_object().cloned().unwrap_or_default(),
            tombstone: true,
        }),
        _ => Ok(Decided::Reject),
    }
}

/// The plain row `value` as the attribute values DynamoDB holds a row as, or `None` where it is
/// no row at all.
///
/// Marshalled through the one marshaller this service has (`$util.dynamodb.toDynamoDB`'s own),
/// then written in the wire form a call carries ([`wire_values`]), so a row a handler composed
/// is held exactly as a row a request object composed is.
fn marshalled(value: Option<&Json>) -> Option<JsonMap<String, Json>> {
    let members = value?.as_object()?;
    Some(
        members
            .iter()
            .map(|(name, held)| (name.clone(), wire_values(&attribute_value(held))))
            .collect(),
    )
}

/// The `UpdateItem` call that writes the system attributes `stamp` states onto the item `key`
/// addresses.
///
/// `carried` is the call whose CONDITION travels with this write ([`carry_condition`]): the
/// delete a request asked for under a condition is refused on exactly the grounds it stated.
fn system_update(table: &str, key: &Json, stamp: &Stamp, carried: Option<&Json>) -> Json {
    let mut names = JsonMap::new();
    let mut values = JsonMap::new();
    let mut assignments: Vec<String> = Vec::new();
    for (at, (attribute, value)) in stamped(stamp).iter().enumerate() {
        let name = format!("#{PLACEHOLDER_PREFIX}{at}");
        let placeholder = format!(":{PLACEHOLDER_PREFIX}{at}");
        names.insert(name.clone(), Json::String((*attribute).to_string()));
        values.insert(placeholder.clone(), value.clone());
        assignments.push(format!("{name} = {placeholder}"));
    }
    let mut input = JsonMap::new();
    input.insert(TABLE_NAME_INPUT.to_string(), Json::String(table.to_string()));
    input.insert(KEY_INPUT.to_string(), key.clone());
    input.insert(
        UPDATE_EXPRESSION_INPUT.to_string(),
        Json::String(format!("SET {}", assignments.join(", "))),
    );
    input.insert(EXPRESSION_NAMES_INPUT.to_string(), Json::Object(names));
    input.insert(EXPRESSION_VALUES_INPUT.to_string(), Json::Object(values));
    carry_condition(&mut input, carried);
    input.insert(
        RETURN_VALUES_INPUT.to_string(),
        Json::String(ALL_NEW.to_string()),
    );
    Json::Object(input)
}

/// The refusal a conflict that was not resolved is reported as: the class AppSync names for it,
/// its own sentence, and the row as the SERVER holds it.
fn rejected(held: &Json) -> Refused {
    Refused {
        message: CONFLICT_REJECTED_MESSAGE.to_string(),
        error_type: CONFLICT_UNHANDLED_ERROR_TYPE.to_string(),
        data: Some(plain_value(held)),
    }
}

/// The version the stored row carries, or `None` for a row carrying none — a row written before
/// the data source kept versions at all, whose next write starts the count.
fn version_held(item: &Json) -> Option<i64> {
    whole(item.get(VERSION_ATTRIBUTE)?.get(NUMBER)?)
}

/// The version the request object states it expects, under either representation a definition
/// writes it in: the plain number, and the number attribute a marshalling helper produced.
fn stated_version(requested: &Json) -> Option<i64> {
    let stated = requested.get(VERSION_ATTRIBUTE)?;
    whole(stated).or_else(|| whole(stated.get(NUMBER)?))
}

/// One whole number read out of a value that carries one, as a number or as the text DynamoDB
/// carries every number in.
fn whole(value: &Json) -> Option<i64> {
    match value {
        Json::Number(count) => count.as_i64(),
        Json::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// `count` as the number attribute DynamoDB holds it as, in the wire form every number travels
/// in.
fn whole_attribute(count: i64) -> Json {
    json!({NUMBER: count.to_string()})
}

/// When the tombstone written at `changed_at` may be reaped: DynamoDB's own expiry stamp is in
/// epoch SECONDS, and the window is stated in minutes.
fn reaped_at(changed_at: i64, minutes: i64) -> i64 {
    changed_at
        .saturating_div(MILLIS_PER_SECOND)
        .saturating_add(minutes.saturating_mul(SECONDS_PER_MINUTE))
}

/// This write's own time, in epoch milliseconds.
fn now_millis() -> i64 {
    ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attribute-value form of one row, which is what the table holds.
    fn row(members: &[(&str, Json)]) -> JsonMap<String, Json> {
        members
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn a_versioned_write_is_the_three_calls_that_change_one_item() {
        // The rows span every call this data source serves: the three that change one item, and
        // the reads, the selections and the batches that do not.
        let cases: &[(&str, Call, Option<Write>)] = &[
            ("a put", Call::PutItem, Some(Write::Put)),
            ("an update", Call::UpdateItem, Some(Write::Update)),
            ("a delete", Call::DeleteItem, Some(Write::Delete)),
            ("a read", Call::GetItem, None),
            ("a query", Call::Query, None),
            ("a scan", Call::Scan, None),
            ("a read batch", Call::BatchGetItem, None),
            ("a write batch", Call::BatchPutItem, None),
            ("a delete batch", Call::BatchDeleteItem, None),
        ];
        for (case, call, expected) in cases {
            let actual = Write::of(*call);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn the_version_a_request_states_is_read_under_either_representation() {
        // The rows differ along HOW a definition wrote the version it expects: the plain number
        // a template inlines, the number attribute a marshalling helper produced, the text
        // DynamoDB carries a number as, and the requests that state no version at all.
        let cases: &[(&str, Json, Option<i64>)] = &[
            ("a plain number", json!({"_version": 3}), Some(3)),
            ("a marshalled number", json!({"_version": {"N": 7}}), Some(7)),
            (
                "a marshalled number in wire form",
                json!({"_version": {"N": "11"}}),
                Some(11),
            ),
            ("no version at all", json!({"operation": "PutItem"}), None),
            (
                "a version that is no number",
                json!({"_version": "soon"}),
                None,
            ),
        ];
        for (case, requested, expected) in cases {
            let actual = stated_version(requested);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn automerge_keeps_the_server_value_and_composes_what_it_can() {
        // One row per kind of attribute the merge composes, plus the two it cannot: the stored
        // value stands for a scalar conflict and for a change of attribute type.
        let held = row(&[
            ("id", json!({"S": "one"})),
            ("title", json!({"S": "held"})),
            ("tags", json!({"SS": ["a", "b"]})),
            ("notes", json!({"L": [{"S": "first"}]})),
            ("counts", json!({"NS": ["1"]})),
            ("meta", json!({"M": {"by": {"S": "ada"}}})),
            ("shape", json!({"S": "text"})),
            ("_version", json!({"N": "4"})),
        ]);
        let incoming = row(&[
            ("id", json!({"S": "one"})),
            ("title", json!({"S": "offered"})),
            ("tags", json!({"SS": ["b", "c"]})),
            ("notes", json!({"L": [{"S": "second"}]})),
            ("counts", json!({"NS": ["2"]})),
            ("meta", json!({"M": {"at": {"S": "noon"}}})),
            ("shape", json!({"N": "4"})),
            ("added", json!({"S": "new"})),
            ("_version", json!({"N": "1"})),
        ]);
        let merged = Json::Object(automerged(&held, &incoming, 0));
        let expected = json!({
            "id": {"S": "one"},
            // A scalar conflict keeps the server's value, and so does a change of type.
            "title": {"S": "held"},
            "shape": {"S": "text"},
            // A set is the union, a list gains what it does not carry, a map merges member by
            // member, and an attribute the server does not hold is added.
            "tags": {"SS": ["a", "b", "c"]},
            "notes": {"L": [{"S": "first"}, {"S": "second"}]},
            "counts": {"NS": ["1", "2"]},
            "meta": {"M": {"by": {"S": "ada"}, "at": {"S": "noon"}}},
            "added": {"S": "new"},
            // The version is the write's to stamp, so the merge leaves the one it was given.
            "_version": {"N": "4"},
        });
        assert_eq!(
            merged, expected,
            "expected {expected}, got {merged} from merging {incoming:?} into {held:?}"
        );
    }

    #[test]
    fn the_system_update_stamps_the_version_and_carries_a_stated_condition() {
        let key = json!({"id": {"S": "one"}});
        let stamp = Stamp {
            version: 4,
            changed_at: 1_700_000_000_000,
            deleted: false,
            retained: None,
        };
        let stamped = system_update("items", &key, &stamp, None);
        assert_eq!(
            stamped[UPDATE_EXPRESSION_INPUT].as_str(),
            Some("SET #appsyncSystem0 = :appsyncSystem0, #appsyncSystem1 = :appsyncSystem1"),
            "a stamp assigns both system attributes, got {stamped}"
        );
        assert_eq!(
            (
                stamped[EXPRESSION_NAMES_INPUT]["#appsyncSystem0"].as_str(),
                stamped[EXPRESSION_VALUES_INPUT][":appsyncSystem0"]["N"].as_str(),
            ),
            (Some(VERSION_ATTRIBUTE), Some("4")),
            "a stamp writes the version it was given, got {stamped}"
        );
        assert!(
            stamped.get(CONDITION_EXPRESSION_INPUT).is_none(),
            "a stamp with no condition carried states none, got {stamped}"
        );

        // A tombstone marks the row deleted and says when it may be reaped; a condition the
        // request stated travels with it, and so do the placeholders that condition names.
        let tombstone = Stamp {
            version: 5,
            changed_at: 60_000,
            deleted: true,
            retained: Some(2),
        };
        let carried = json!({
            CONDITION_EXPRESSION_INPUT: "attribute_exists(#owner)",
            EXPRESSION_NAMES_INPUT: {"#owner": "owner"},
        });
        let written = system_update("items", &key, &tombstone, Some(&carried));
        let names = &written[EXPRESSION_NAMES_INPUT];
        assert_eq!(
            (
                names["#appsyncSystem2"].as_str(),
                names["#appsyncSystem3"].as_str(),
                names["#owner"].as_str(),
                written[CONDITION_EXPRESSION_INPUT].as_str(),
            ),
            (
                Some(DELETED_ATTRIBUTE),
                Some(TTL_ATTRIBUTE),
                Some("owner"),
                Some("attribute_exists(#owner)"),
            ),
            "a tombstone marks the row and keeps the stated condition, got {written}"
        );
        assert_eq!(
            written[EXPRESSION_VALUES_INPUT][":appsyncSystem3"]["N"].as_str(),
            // 60_000 ms is second 60, and two minutes of retention is 120 seconds past it.
            Some("180"),
            "a tombstone is reaped a retention window after the write, got {written}"
        );
    }

    #[test]
    fn the_restoring_write_is_guarded_by_the_system_attributes_the_row_carried() {
        // The rows differ along which system attributes the row carried, because those are what
        // the guard can be written over: both, one, and neither.
        let cases: &[(&str, Json, Option<&str>)] = &[
            (
                "a row carrying both",
                json!({
                    "id": {"S": "one"},
                    "_version": {"N": "3"},
                    "_lastChangedAt": {"N": "1700000000000"},
                }),
                Some("#appsyncSystem0 = :appsyncSystem0 AND #appsyncSystem1 = :appsyncSystem1"),
            ),
            (
                "a row carrying the version alone",
                json!({"id": {"S": "one"}, "_version": {"N": "3"}}),
                Some("#appsyncSystem0 = :appsyncSystem0"),
            ),
            (
                "a row carrying neither, which has nothing to guard with",
                json!({"id": {"S": "one"}}),
                None,
            ),
        ];
        for (case, held, expected) in cases {
            let restored = restoring_put("items", held);
            assert_eq!(
                (
                    restored[ITEM_INPUT].clone(),
                    restored
                        .get(CONDITION_EXPRESSION_INPUT)
                        .and_then(Json::as_str),
                ),
                (held.clone(), *expected),
                "case {case:?}: expected the row written back under {expected:?}, got {restored}"
            );
        }
    }
}
