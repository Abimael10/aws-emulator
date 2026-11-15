// SPDX-License-Identifier: CC0-1.0

//! This front door's DATA leg: the calls on the ITEMS a table holds, which CloudTrail
//! records as events of the `Data` category rather than as management events.
//!
//! CloudTrail classes the calls of an account in two, and the two reach different readers.
//! The ADMINISTRATION of this service's resources — creating, reading, changing and
//! removing a table, a backup, an export, an import, a replica, a resource policy or a tag
//! set — is a management event, recorded in event history where `LookupEvents` reports it
//! ([`super::management_events`]). A call on the ITEMS a table holds acts on that table's
//! contents instead, so it is a DATA event:
//!
//! * it is NOT event history ("data events are not logged by default when you create a
//!   trail", and a lookup serves management and Insights events), so no such call appears in
//!   a lookup at all — which is what keeps a client's reads and writes of a table's items
//!   out of the account's event history however many of them it makes;
//! * it is NOT published onto the default event bus, which carries management events; and
//! * it is delivered to a trail's BUCKET only while one of that trail's event selectors
//!   admits it — which such a selector states by naming the `Data` category and the TYPE of
//!   the resource the call acted on ([`TABLE_RESOURCE_TYPE`]), and may narrow further by
//!   that table's ARN.
//!
//! Which class a call falls in is stated once, where this service's routing table is built
//! ([`super::management_events::audited_table`]): the record itself is built from the typed
//! call by the shared recorder either way ([`service_common::audited_operation`]), so a data
//! event reports the same parameters, identity and outcome a management event of this
//! service would.
//!
//! This module is the two things that are THIS service's: which of its operations are data
//! events and which table each one acts on ([`DATA_EVENT_OPERATIONS`],
//! [`data_event_resources`]), and the delivery of one such record into the account's trails
//! ([`TableDataEvents`]).

use std::sync::Arc;
use std::time::SystemTime;

use aws_smithy_runtime_api::http::Response as RtResponse;
use service_common::data_events::DataEventDelivery;
use service_common::management_events::{recorded_member_name, AuditedResource, ServedApiCall};
use service_common::trail_log_files::DeliveredLogFile;

/// The type a table is named by in a delivered record's `resources` and in an advanced
/// event selector's `resources.type` field: the resource type CloudTrail publishes for the
/// data events of this service, and the one a basic selector's `DataResource$Type` names
/// them by as well.
pub const TABLE_RESOURCE_TYPE: &str = "AWS::DynamoDB::Table";

/// How a request names the table (or tables) a data call acts on.
///
/// Three forms, because the data plane addresses a table in three ways — and the MODELLED
/// member names are the ones stated here, with the record's own spelling of them derived
/// through [`recorded_member_name`] rather than transcribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressedTables {
    /// One member carrying the table's name, or its ARN: `TableName` on the single-item
    /// reads and writes, on `Query`, `Scan` and on the vector search.
    Member(&'static str),
    /// A MAP whose KEYS are table names, one entry per table: `RequestItems` on the batch
    /// operations.
    MapKeys(&'static str),
    /// A LIST whose elements each carry one action, and each action names its own table:
    /// `TransactItems` -> `Get`/`Put`/`Update`/`Delete`/`ConditionCheck` -> `TableName`.
    ElementsOf(&'static str, &'static str),
    /// The table is named INSIDE a PartiQL statement the request carries as text, which no
    /// member of the request holds on its own. Such a record carries the statement (the
    /// record's `statement` / `statements` / `transactStatements`) and names no resource:
    /// the ARN a trail's selector matches on is not derivable from a member here, and a
    /// record naming a table nothing resolved would be reported to a lookup as fact — so
    /// the delivery is withheld rather than invented, the same fail-closed direction the
    /// shared selector match takes for a field it cannot read.
    InStatement,
}

/// The operations of this service CloudTrail records as DATA events, each with the way its
/// request names the tables the call acts on.
///
/// The set is the documented one: the data-plane operations of DynamoDB are the item reads
/// and writes (`GetItem`, `PutItem`, `UpdateItem`, `DeleteItem`), the multi-item forms of
/// them (`BatchGetItem`, `BatchWriteItem`, `TransactGetItems`, `TransactWriteItems`), the
/// two queries over a table's items (`Query`, `Scan`) and the PartiQL surfaces that run
/// statements against them (`ExecuteStatement`, `BatchExecuteStatement`,
/// `ExecuteTransaction`). The vector search is here for the same reason `Query` is: it
/// "performs a vector similarity search on a vector index associated with an Amazon DynamoDB
/// table, and returns" the matching items (`smithy_query(query="digest")`), which is a read
/// of that table's contents and not administration of the account's resources.
///
/// Public, so this crate's own tests read the catalog the routing table is built from rather
/// than a second copy of it.
pub const DATA_EVENT_OPERATIONS: &[(&str, AddressedTables)] = &[
    (
        "BatchExecuteStatement",
        AddressedTables::InStatement,
    ),
    ("BatchGetItem", AddressedTables::MapKeys("RequestItems")),
    ("BatchWriteItem", AddressedTables::MapKeys("RequestItems")),
    ("DeleteItem", AddressedTables::Member("TableName")),
    ("ExecuteStatement", AddressedTables::InStatement),
    ("ExecuteTransaction", AddressedTables::InStatement),
    ("GetItem", AddressedTables::Member("TableName")),
    ("PutItem", AddressedTables::Member("TableName")),
    ("Query", AddressedTables::Member("TableName")),
    ("Scan", AddressedTables::Member("TableName")),
    ("SearchVectors", AddressedTables::Member("TableName")),
    (
        "TransactGetItems",
        AddressedTables::ElementsOf("TransactItems", "TableName"),
    ),
    (
        "TransactWriteItems",
        AddressedTables::ElementsOf("TransactItems", "TableName"),
    ),
    ("UpdateItem", AddressedTables::Member("TableName")),
];

/// Whether CloudTrail records the calls of `operation` as DATA events.
#[must_use]
pub fn is_data_event(operation: &str) -> bool {
    addressing(operation).is_some()
}

/// How a request of `operation` names its tables, or `None` for an operation whose calls
/// are management events.
fn addressing(operation: &str) -> Option<AddressedTables> {
    DATA_EVENT_OPERATIONS
        .iter()
        .find(|(named, _)| *named == operation)
        .map(|(_, addressing)| *addressing)
}

/// The resources a call of `operation` acted on, read off the parameters its record carries
/// — each table the request addressed, by its ARN in `region`.
///
/// A record names the resource a data event acted on because that is what a trail's selector
/// SELECTS the event by: `resources.type` names the class of data events a trail logs, and
/// `resources.ARN` narrows it to one table, so a record naming no resource is admitted by no
/// such selector. A MANAGEMENT call of this service names none (see
/// [`super::management_events`]), so this answers a resource only for the operations above.
///
/// The value a request carries is a table NAME or that table's ARN ("You can also provide
/// the Amazon Resource Name (ARN) of the table in this parameter" — the `TableName` docs),
/// so it is resolved through the one reader this service resolves it with everywhere else
/// (`crate::state::resolve_table_name_in_member`) and the ARN is built from the name that
/// reader answers. A value that is not a table of this endpoint's own Region names no
/// resource rather than one built from a value this endpoint cannot reach, and the same goes
/// for an absent, non-text or empty value.
#[must_use]
pub fn data_event_resources(
    operation: &str,
    request_parameters: &serde_json::Value,
    region: &str,
) -> Vec<AuditedResource> {
    let Some(addressing) = addressing(operation) else {
        return Vec::new();
    };
    let mut resources: Vec<AuditedResource> = Vec::new();
    for addressed in addressed_tables(addressing, request_parameters) {
        let Ok(name) =
            crate::state::resolve_table_name_in_member(TABLE_NAME_MEMBER, &addressed, region)
        else {
            continue;
        };
        let resource =
            AuditedResource::new(TABLE_RESOURCE_TYPE, crate::state::table_arn(region, &name));
        // One resource per table, however many of a batch's or a transaction's actions
        // addressed it: `resources` names the resources the call acted on, not its actions.
        if !resources.contains(&resource) {
            resources.push(resource);
        }
    }
    resources
}

/// The member path a refusal of a table-naming value is stated under. It reaches no caller
/// from here — this walk DROPS a value the reader refuses — and is the request-level path so
/// a trace of one reads as the handlers' own refusals do.
const TABLE_NAME_MEMBER: &str = crate::state::TABLE_NAME_MEMBER_PATH;

/// Every table-naming value a record's `request_parameters` carries, as the request wrote
/// it: a name or an ARN, unresolved.
fn addressed_tables(
    addressing: AddressedTables,
    request_parameters: &serde_json::Value,
) -> Vec<String> {
    match addressing {
        AddressedTables::Member(member) => request_parameters
            .get(recorded_member_name(member))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| vec![value.to_string()])
            .unwrap_or_default(),
        AddressedTables::MapKeys(member) => request_parameters
            .get(recorded_member_name(member))
            .and_then(serde_json::Value::as_object)
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default(),
        AddressedTables::ElementsOf(member, leaf) => request_parameters
            .get(recorded_member_name(member))
            .and_then(serde_json::Value::as_array)
            .map(|elements| {
                // One element carries ONE action (`Get`, `Put`, `Update`, `Delete`,
                // `ConditionCheck`), and the action is what names the table — so the walk
                // reads the leaf off each action the element holds rather than naming the
                // actions, which keeps it right for an action this model gains later.
                elements
                    .iter()
                    .filter_map(serde_json::Value::as_object)
                    .flat_map(|element| element.values())
                    .filter_map(|action| action.get(recorded_member_name(leaf)))
                    .filter_map(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        AddressedTables::InStatement => Vec::new(),
    }
}

/// What this service contributes to the delivery of one of its data events: the log files
/// the account's trails deliver it as.
///
/// Holds the shared engine — the trail projection the delivery is decided by lives in the
/// log that engine carries, the one CloudTrail recomputes whenever a trail of the account is
/// created, reconfigured, started or stopped — so it is generic over the engine type exactly
/// as the service is.
pub struct TableDataEvents<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> TableDataEvents<Eng> {
    /// Build the deliverer from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> DataEventDelivery for TableDataEvents<Eng> {
    fn delivered(
        &self,
        call: ServedApiCall,
        served_at: SystemTime,
        answered_as: &str,
    ) -> Vec<DeliveredLogFile> {
        let Some(log) = super::management_events::management_event_log(&*self.engine) else {
            // Not silent: an undelivered data event leaves no trace in the account's trail
            // by definition, so it leaves one here instead.
            ::tracing::warn!(
                operation = call.event_name,
                "the engine carries no management-event log: the data-plane call was served \
                 with no trail able to deliver it"
            );
            return Vec::new();
        };
        // Nothing is RECORDED: a data event is not event history, so the log is only asked
        // which of the account's trails deliver this record. The guard is BOUND rather than
        // used as a temporary: a local is released before the handle it borrows, which a
        // temporary at the end of this block would not be.
        let delivering = log.lock();
        delivering.data_event_log_files(call, served_at, answered_as)
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        // The same reason the management recorder takes this per service: this service
        // spells its request ids its own way ([`super::request_id`]), and a delivered record
        // must name the request the CALLER was answered under.
        super::request_id::named_request(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Region every row below addresses a table in.
    const REGION: &str = "us-east-1";

    /// The ARN this endpoint builds for the table `name`.
    fn table(name: &str) -> String {
        crate::state::table_arn(REGION, name)
    }

    /// The resource a record names for the table `name`.
    fn resource(name: &str) -> AuditedResource {
        AuditedResource::new(TABLE_RESOURCE_TYPE, table(name))
    }

    /// The resources as the `<type> <arn>` pairs they name, sorted — the SET of them, which
    /// is what the rule is about.
    fn named(resources: &[AuditedResource]) -> Vec<String> {
        let mut named: Vec<String> = resources
            .iter()
            .map(|resource| format!("{} {}", resource.resource_type, resource.resource_name))
            .collect();
        named.sort();
        named
    }

    /// RULE: a data event's record names each table the request addressed, by that table's
    /// ARN and under the data-event resource type; a management call of this service names
    /// no resource.
    ///
    /// The rows differ along the FORM the request names its tables in (a member, a map's
    /// keys, a list of actions, a statement's text), along whether the value is a name or an
    /// ARN, along how many tables one call addresses, and along the class of the call.
    #[test]
    fn a_data_events_record_names_the_tables_the_request_addressed() {
        let rows: Vec<(&str, &str, serde_json::Value, Vec<AuditedResource>)> = vec![
            (
                "a single-item read naming the table by name",
                "GetItem",
                serde_json::json!({"tableName": "orders", "key": {"pk": {"S": "1"}}}),
                vec![resource("orders")],
            ),
            (
                "a single-item write naming the table by ARN",
                "PutItem",
                serde_json::json!({"tableName": table("orders")}),
                vec![resource("orders")],
            ),
            (
                "a query over an index of a table",
                "Query",
                serde_json::json!({"tableName": "orders", "indexName": "by-date"}),
                vec![resource("orders")],
            ),
            (
                "a vector search",
                "SearchVectors",
                serde_json::json!({"tableName": "orders", "indexName": "vectors"}),
                vec![resource("orders")],
            ),
            (
                "a batch read over two tables",
                "BatchGetItem",
                serde_json::json!({"requestItems": {"orders": {}, "customers": {}}}),
                vec![resource("customers"), resource("orders")],
            ),
            (
                "a batch write over one table",
                "BatchWriteItem",
                serde_json::json!({"requestItems": {"orders": []}}),
                vec![resource("orders")],
            ),
            (
                "a transaction over two tables, one of them twice",
                "TransactWriteItems",
                serde_json::json!({"transactItems": [
                    {"put": {"tableName": "orders"}},
                    {"delete": {"tableName": "customers"}},
                    {"conditionCheck": {"tableName": "orders"}},
                ]}),
                vec![resource("orders"), resource("customers")],
            ),
            (
                "a transactional read",
                "TransactGetItems",
                serde_json::json!({"transactItems": [{"get": {"tableName": "orders"}}]}),
                vec![resource("orders")],
            ),
            (
                "a PartiQL statement, whose table no member of the request names",
                "ExecuteStatement",
                serde_json::json!({"statement": "SELECT * FROM \"orders\""}),
                Vec::new(),
            ),
            (
                "a control-plane read of the same table",
                "DescribeTable",
                serde_json::json!({"tableName": "orders"}),
                Vec::new(),
            ),
            (
                "a control-plane tag write addressing the table by ARN",
                "TagResource",
                serde_json::json!({"resourceArn": table("orders")}),
                Vec::new(),
            ),
            (
                "a data call carrying no table at all",
                "GetItem",
                serde_json::json!({"key": {"pk": {"S": "1"}}}),
                Vec::new(),
            ),
            (
                "a data call whose member carries no text",
                "GetItem",
                serde_json::json!({"tableName": ""}),
                Vec::new(),
            ),
            (
                "a data call whose member carries a value of another type",
                "GetItem",
                serde_json::json!({"tableName": 7}),
                Vec::new(),
            ),
            (
                "a data call naming a table of ANOTHER Region",
                "GetItem",
                serde_json::json!({"tableName": crate::state::table_arn("eu-west-1", "orders")}),
                Vec::new(),
            ),
            (
                "a data call whose value is no way of naming a table",
                "GetItem",
                serde_json::json!({"tableName": "a name with spaces"}),
                Vec::new(),
            ),
        ];
        for (case, operation, request_parameters, expected) in rows {
            let actual = data_event_resources(operation, &request_parameters, REGION);
            // Compared by the SET of resources: which tables a record names is the rule,
            // and the order a map's keys are walked in is not part of it.
            assert_eq!(
                named(&actual),
                named(&expected),
                "case {case:?}: expected the record to name {expected:?}, got {actual:?}"
            );
            assert_eq!(
                actual.len(),
                expected.len(),
                "case {case:?}: expected one resource per table addressed, got {actual:?}"
            );
        }
    }

    /// RULE: the operations recorded as data events are exactly the catalog, and every other
    /// operation of this service is a management event.
    #[test]
    fn only_the_cataloged_operations_are_data_events() {
        for (operation, _) in DATA_EVENT_OPERATIONS {
            assert!(
                is_data_event(operation),
                "case {operation:?}: a cataloged operation is a data event"
            );
        }
        // The control plane, one row per kind of resource it administers.
        for operation in [
            "CreateTable",
            "DescribeTable",
            "ListTables",
            "TagResource",
            "UntagResource",
            "ListTagsOfResource",
            "CreateBackup",
            "ExportTableToPointInTime",
            "PutResourcePolicy",
            "UpdateTimeToLive",
        ] {
            assert!(
                !is_data_event(operation),
                "case {operation:?}: a control-plane call is no data event"
            );
        }
    }
}
