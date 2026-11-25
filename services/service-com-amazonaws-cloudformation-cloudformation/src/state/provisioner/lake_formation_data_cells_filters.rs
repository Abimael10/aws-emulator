// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the LAKE FORMATION DATA CELLS FILTER: the cell-level
//! filter an `AWS::LakeFormation::DataCellsFilter` declares on a Data Catalog table. One
//! file of [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! A data cells filter is a resource OF a catalog table rather than a named resource of its
//! own: `CreateDataCellsFilter` takes the whole filter as one `TableData` structure, and
//! `GetDataCellsFilter` and `DeleteDataCellsFilter` each address one by the catalog, the
//! database, the table and the filter's own name (`smithy_query(query="operation",
//! operation="CreateDataCellsFilter,GetDataCellsFilter,DeleteDataCellsFilter")` of the
//! `lakeformation` model: `POST /CreateDataCellsFilter`, `POST /GetDataCellsFilter`,
//! `POST /DeleteDataCellsFilter`). So those four values ARE the resource's identity
//! ([`super::template::composite_identity_properties`]) and the physical id of one is them
//! joined, which is what the type's `Ref` returns.
//!
//! The type's properties are that `TableData`'s own members, named identically on both
//! sides all the way down ([`DATA_CELLS_FILTER_ADDRESS`] and
//! [`DATA_CELLS_FILTER_SELECTION`] against
//! `smithy_query(query="shape", shape="DataCellsFilter")`), so the evaluated property
//! document IS the filter the create sends — with one member of that shape left out, the
//! `VersionId` Lake Formation assigns, which the type publishes no property for.
//!
//! The type publishes no `Tags` property and no `Fn::GetAtt` attribute: a filter is not a
//! taggable resource — Lake Formation holds tags on the LF-tags its own tagging calls name —
//! so nothing is propagated to one, and there is nothing to report beside the identity the
//! stack already computed from the declaration.
//!
//! There is no UPDATE arm, and the type needs none: every property of it is published with
//! "Update requires: `Replacement`", so a changed declaration replaces the filter
//! ([`super::template::replacement_properties`]) — the parts of the identity by
//! construction, and the cells and rows it selects because the resource provider of this
//! type has no update handler at all.

use super::*;

/// `AWS::LakeFormation::DataCellsFilter`: one cell-level filter on one Data Catalog table,
/// whose `Ref` returns the four values Lake Formation addresses one by, joined.
pub(in crate::state) const LAKEFORMATION_DATA_CELLS_FILTER: &str =
    "AWS::LakeFormation::DataCellsFilter";

/// The four properties that address ONE filter, in the order the composite identity joins
/// them: the catalog the table belongs to, the database, the table, and the filter's own
/// name. Every call about one filter takes exactly these.
pub(in crate::state) const DATA_CELLS_FILTER_ADDRESS: &[&str] =
    &["TableCatalogId", "DatabaseName", "TableName", "Name"];

/// The property that states which ROWS the filter selects, which is the `RowFilter` member
/// of that shape.
const ROW_FILTER_PROPERTY: &str = "RowFilter";

/// The two alternative ways a filter names the COLUMNS it selects, each a member of that
/// shape under the same name.
const DATA_CELLS_FILTER_COLUMNS: &[&str] = &["ColumnNames", "ColumnWildcard"];

/// The three properties BEYOND that address, which state what the filter selects: the row
/// predicate and the two ways of naming columns. Each is published with "Update requires:
/// `Replacement`".
pub(in crate::state) const DATA_CELLS_FILTER_SELECTION: &[&str] = &[
    ROW_FILTER_PROPERTY,
    DATA_CELLS_FILTER_COLUMNS[0],
    DATA_CELLS_FILTER_COLUMNS[1],
];

/// The member each call takes the whole filter in, and the member a read answers it under.
const TABLE_DATA_MEMBER: &str = "TableData";
const REPORTED_FILTER_MEMBER: &str = "DataCellsFilter";

/// The three operations this module calls, as that service's own model names and binds them.
const CREATE_DATA_CELLS_FILTER: &str = "CreateDataCellsFilter";
const GET_DATA_CELLS_FILTER: &str = "GetDataCellsFilter";
const DELETE_DATA_CELLS_FILTER: &str = "DeleteDataCellsFilter";
const CREATE_DATA_CELLS_FILTER_URI: &str = "/CreateDataCellsFilter";
const GET_DATA_CELLS_FILTER_URI: &str = "/GetDataCellsFilter";
const DELETE_DATA_CELLS_FILTER_URI: &str = "/DeleteDataCellsFilter";

/// Create the data cells filter an `AWS::LakeFormation::DataCellsFilter` declares, so a
/// client that deployed the stack and then calls `lakeformation ListDataCellsFilter` on the
/// parent table reads the filter back rather than nothing.
///
/// The filter is created in the service that OWNS it, so everything the declaration states
/// is resolved by Lake Formation exactly as it is for a client's own call: a filter whose
/// cells are named both ways at once, one that states no row predicate, and a name that
/// table already carries a filter under are all that service's refusals, which fail the
/// resource and roll the stack back rather than publishing a filter nothing holds.
///
/// The identity is the one the naming rules already computed from the declaration, so
/// nothing here replaces it: the create answers no identifier of its own
/// (`CreateDataCellsFilterResponse` models no member).
pub(super) async fn create_lake_formation_data_cells_filter(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let request = Value::Object(Map::from_iter([(
        TABLE_DATA_MEMBER.to_string(),
        Value::Object(declared_filter(&resource.properties)),
    )]));
    call_lake_formation(
        ctx,
        region,
        CREATE_DATA_CELLS_FILTER,
        CREATE_DATA_CELLS_FILTER_URI,
        &request,
    )
    .await?;
    Ok(Created::default())
}

/// Delete the filter a stack created, addressed by the four values Lake Formation keys one
/// by — which its declaration carries and which are this resource's physical id.
///
/// A filter that service no longer holds (`EntityNotFoundException`, judged on the CODE and
/// never on a message that carries the request's own names) is already in the state the
/// delete asks for — as is one whose table or database is already gone, which answers the
/// same code.
pub(super) async fn delete_lake_formation_data_cells_filter(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let request = Value::Object(filter_address(&resource.properties));
    match call_lake_formation(
        ctx,
        region,
        DELETE_DATA_CELLS_FILTER,
        DELETE_DATA_CELLS_FILTER_URI,
        &request,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == LAKEFORMATION_ENTITY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The filter as Lake Formation reports it, for drift detection: its own read's answer, with
/// the four values that address the filter and the two ways of naming columns reported under
/// the properties the type declares them as. A filter that service no longer holds
/// (`EntityNotFoundException`) was deleted.
///
/// The `RowFilter` is NOT reported, because a property the actual document does not carry is
/// not evidence of anything ([`super::drift`]) while a MISREAD one is evidence of something
/// that did not happen: Lake Formation reports a filter that selects every row carrying both
/// forms of that predicate — the all-rows wildcard and the `TRUE` expression that says the
/// same thing — so a declaration stating one of them would read as drifted on a filter
/// nobody touched. That is the position an `AWS::Glue::TableOptimizer`'s configuration is in
/// ([`super::glue_table_optimizers`]).
pub(super) async fn describe_lake_formation_data_cells_filter(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let request = Value::Object(filter_address(&resource.properties));
    let answer = match call_lake_formation(
        ctx,
        region,
        GET_DATA_CELLS_FILTER,
        GET_DATA_CELLS_FILTER_URI,
        &request,
    )
    .await
    {
        Ok(answer) => answer,
        Err(refusal) if refusal.code == LAKEFORMATION_ENTITY_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(reported) = answer.get(REPORTED_FILTER_MEMBER) else {
        return Ok(None);
    };
    let mut actual = Map::new();
    for property in DATA_CELLS_FILTER_ADDRESS
        .iter()
        .chain(DATA_CELLS_FILTER_COLUMNS.iter())
    {
        if let Some(value) = reported.get(*property) {
            actual.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(actual)))
}

/// The whole filter a declaration describes, as the `DataCellsFilter` the create takes it
/// as: every property of the type the declaration states, under the member of that shape it
/// is.
///
/// A property the declaration leaves out is left out of the request, so the omission is
/// judged by Lake Formation's own validation of the filter — a missing row predicate is that
/// service's refusal, not one invented here — and nothing of the shape the declaration does
/// not state is filled in.
fn declared_filter(declared: &Value) -> Map<String, Value> {
    let mut filter = Map::new();
    for property in DATA_CELLS_FILTER_ADDRESS
        .iter()
        .chain(DATA_CELLS_FILTER_SELECTION.iter())
    {
        if let Some(value) = declared.get(*property) {
            filter.insert((*property).to_string(), value.clone());
        }
    }
    filter
}

/// The four members that address ONE filter, read off a declaration: the catalog, the
/// database, the table and the filter's name, each sent only where the declaration states it
/// — an omission being that service's own validation of the member rather than something
/// decided here.
fn filter_address(declared: &Value) -> Map<String, Value> {
    let mut request = Map::new();
    for property in DATA_CELLS_FILTER_ADDRESS {
        if let Some(text) = declared_text(declared, property) {
            request.insert((*property).to_string(), Value::String(text));
        }
    }
    request
}
