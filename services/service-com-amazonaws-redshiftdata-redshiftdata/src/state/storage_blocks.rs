// SPDX-License-Identifier: CC0-1.0

//! How a data warehouse LAYS A TABLE OUT on disk: the slices it spreads a table
//! over, and the 1 MB blocks each column of it occupies on each of them.
//!
//! Amazon Redshift stores a table COLUMN by column, in blocks of one megabyte,
//! on every slice of the cluster: "STV_BLOCKLIST contains one row per 1 MB disk
//! block", and the *minimum table size* the Amazon Redshift Database Developer
//! Guide publishes is "the number of columns × the number of slices × the block
//! size", because a column occupies a block on each slice from the moment the
//! table exists. That geometry is what decides how many rows `STV_BLOCKLIST`
//! answers with and what size `SVV_TABLE_INFO` reports, so it is computed ONCE
//! here and both of those system tables are materialised from it
//! ([`super::system_tables`]).
//!
//! Nothing here is a fixed number standing in for storage: the slices come from
//! the shape of the warehouse the statement runs on, as the control plane that
//! owns it reports that shape ([`cluster_slices`]), and the blocks from the
//! values the engine really holds and the width the warehouse stores a value of
//! that declared type in ([`super::analyze_compression::stored_width`]).

use super::copy_load::TypeClass;

/// The size of one disk block.
///
/// Source: the Amazon Redshift *System tables and views reference*,
/// `STV_BLOCKLIST` — "the number of 1 MB disk blocks used by each slice, table,
/// or column in a database".
pub(super) const BLOCK_BYTES: u64 = 1024 * 1024;

/// How many columns a stored table carries BEYOND the ones it declares.
///
/// Source: the *System tables and views reference*, `STV_BLOCKLIST` — "every
/// table you create has three hidden columns appended to it: INSERT_XID,
/// DELETE_XID, ROW_ID (OID)". They occupy blocks of their own and are reported
/// under the column indexes past the declared ones.
pub(super) const HIDDEN_COLUMNS: usize = 3;

/// The width one value of a hidden column takes: a transaction id and a row id
/// are 64-bit values, which the *Data types* reference stores in 8 bytes.
const HIDDEN_COLUMN_WIDTH: u64 = 8;

/// The most blocks one column of one table can be reported as holding.
///
/// A column cannot really occupy more of the engine's storage than the whole
/// database may ([`super::MAX_DATABASE_PAGES`] pages of 4 KiB, which is 256 MiB
/// and so this many blocks of [`BLOCK_BYTES`]), so a declared width multiplied
/// by a row count that would exceed it is held here — what is reported stays
/// something this engine could really be storing.
const MAX_BLOCKS_PER_COLUMN: i64 = 256;

/// The slices one node of a provisioned cluster has.
///
/// Source: the Amazon Redshift Cluster Management Guide, which publishes the
/// slices per node of each node type. Every published node type has at least
/// two, which is what [`SLICES_PER_NODE_FLOOR`] stands for.
const SLICES_PER_NODE: &[(&str, i64)] = &[
    ("dc1.large", 2),
    ("dc1.8xlarge", 32),
    ("dc2.large", 2),
    ("dc2.8xlarge", 16),
    ("ds2.xlarge", 2),
    ("ds2.8xlarge", 16),
    ("ra3.large", 2),
    ("ra3.xlplus", 2),
    ("ra3.4xlarge", 4),
    ("ra3.16xlarge", 16),
];

/// The slices a node is reported as having when the node type it runs is not one
/// this service can resolve — a warehouse whose owner could not be asked, and
/// serverless compute, which publishes no node type at all.
///
/// The smallest slices-per-node figure the Cluster Management Guide publishes:
/// no node of a data warehouse has fewer, so a shape this service could not read
/// is reported at that floor rather than at a slice count it cannot establish.
const SLICES_PER_NODE_FLOOR: i64 = 2;

/// How many slices the compute of one warehouse has: the slices of each of its
/// nodes, over the nodes it runs.
///
/// A single-node cluster runs one node, so `nodes` below one counts as one.
pub(super) fn cluster_slices(node_type: Option<&str>, nodes: i32) -> i64 {
    let per_node = node_type
        .map(str::trim)
        .and_then(|node_type| {
            SLICES_PER_NODE
                .iter()
                .find(|(published, _)| published.eq_ignore_ascii_case(node_type))
                .map(|(_, slices)| *slices)
        })
        .unwrap_or(SLICES_PER_NODE_FLOOR);
    per_node.saturating_mul(i64::from(nodes).max(1))
}

/// How many slices a table is reported as spread over: the count the warehouse's
/// own shape gives, or [`SLICES_PER_NODE_FLOOR`] when no shape was reported at
/// all — no node of a data warehouse has fewer slices than that.
pub(super) fn spread_over(slices: i64) -> i64 {
    slices.max(SLICES_PER_NODE_FLOOR)
}

/// How one COLUMN of a table is stored on one slice: the values it holds there
/// and the blocks they occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ColumnStorage {
    /// How many values the column holds.
    pub values: i64,
    /// How many blocks those values occupy, which is at least one: the published
    /// minimum size of a table is one block for each of its columns on each
    /// slice, so a block is occupied from the moment the table exists.
    pub blocks: i64,
}

impl ColumnStorage {
    /// How many of the column's values the block numbered `block` holds.
    ///
    /// The values are spread over the blocks that hold them, so the count of
    /// each block adds up to the column's own: with more values than blocks, the
    /// first `values % blocks` blocks carry one more than the rest.
    pub fn values_in(&self, block: i64) -> i64 {
        let blocks = self.blocks.max(1);
        let each = self.values / blocks;
        let remainder = self.values % blocks;
        each + i64::from(block < remainder)
    }
}

/// How many blocks `values` values of a column `width` bytes wide occupy, and
/// the storage that column therefore reports.
fn column_storage(width: u64, values: i64) -> ColumnStorage {
    let bytes = width.saturating_mul(u64::try_from(values).unwrap_or(0));
    let blocks = bytes.div_ceil(BLOCK_BYTES);
    ColumnStorage {
        values,
        blocks: i64::try_from(blocks)
            .unwrap_or(MAX_BLOCKS_PER_COLUMN)
            .clamp(1, MAX_BLOCKS_PER_COLUMN),
    }
}

/// How each column of the relation `table` is stored, in the engine's own
/// column order: the columns it declares, then the ones Amazon Redshift appends
/// to every table it stores ([`HIDDEN_COLUMNS`]).
///
/// Read off the engine's own catalog — the columns the relation declares and the
/// type each one was declared with, from the query that returns them, and the
/// values it holds from the relation itself. `None` for a relation the engine
/// cannot prepare a read of, which holds no block this reader can report.
pub(super) fn columns_of(
    connection: &rusqlite::Connection,
    table: &str,
) -> Option<Vec<ColumnStorage>> {
    let quoted = table.replace('"', "\"\"");
    let probe = connection
        .prepare(&format!("SELECT * FROM \"{quoted}\" LIMIT 0"))
        .ok()?;
    let widths: Vec<u64> = probe
        .columns()
        .map(|column| stored_value_width(column.decl_type()))
        .collect();
    drop(probe);
    let values: i64 = connection
        .query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |row| {
            row.get(0)
        })
        .unwrap_or_default();
    Some(
        widths
            .into_iter()
            .chain(std::iter::repeat_n(HIDDEN_COLUMN_WIDTH, HIDDEN_COLUMNS))
            .map(|width| column_storage(width, values))
            .collect(),
    )
}

/// The width one value of a column declared with this type is stored in, which
/// is the same width the warehouse's own compression analysis charges for it
/// ([`super::analyze_compression::stored_width`]).
///
/// A column that declares no width — a character column with no length — is
/// charged nothing, so what it reports is the one block every column of a stored
/// table occupies rather than a width this service would have to invent.
fn stored_value_width(declared: Option<&str>) -> u64 {
    let declared = declared.unwrap_or_default();
    super::analyze_compression::stored_width(declared, TypeClass::of(declared))
}
