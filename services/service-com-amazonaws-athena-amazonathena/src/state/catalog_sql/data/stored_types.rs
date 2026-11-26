// SPDX-License-Identifier: CC0-1.0

//! Whether a catalog column may be read from the leaf a SELF-DESCRIBING object stores for
//! it.
//!
//! An object of a columnar format carries its own schema, so a read of it has two
//! statements about every column: the type the FILE stores the values as, and the type the
//! catalog entry DECLARES for the column. A client that evolves a table's schema makes the
//! two differ, and what may be read then is not a free reinterpretation of the bytes: a
//! declaration of the same type family as the stored leaf is read (a wider integer type over
//! a narrower stored one included), a declaration of another family is not readable from
//! those bytes at all, and a stored value outside the range of the declared type is not a
//! value of it — so neither is reinterpreted or truncated, and the read fails in Hive's own
//! class for data that is not what the table declares.
//!
//! What the two formats' own vocabularies share is the FAMILY: integers, floating-point
//! numbers, booleans and character/byte data. A declaration this module places in none of
//! them — a calendar value, an exact-numeric value, a collection or a record — and a leaf of
//! none of them are left to the reader, which holds every such value as the text the format
//! decoded it to.

use ::service_com_amazonaws_glue_awsglue::state as glue_state;

use super::parquet_objects::{LeafType, LeafValue};
use super::{DeclaredColumn, Failure};

/// The TYPE FAMILY a stored leaf or a declared column belongs to: what the two vocabularies
/// have in common, and so what decides whether one can be read as the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// An integral number, of the width the member carries.
    Integer(IntegerWidth),
    /// A floating-point number.
    Floating,
    /// A boolean.
    Boolean,
    /// Character or byte data: text, a bounded character type, and raw bytes — one family,
    /// because a byte array is what a columnar format stores every one of them as.
    Characters,
}

/// How wide an integral type is, as the values it can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntegerWidth {
    /// The largest value of the type, which is what a stored value has to fit in.
    max: i64,
    /// The smallest value of the type.
    min: i64,
    /// How the type's upper bound is NAMED, for the failure a value past it reports.
    bound: &'static str,
}

/// The integral types Hive declares, with the range of each and the name its bound carries.
///
/// Source: the Data Catalog's own integral column types — `tinyint` (8-bit), `smallint`
/// (16-bit), `int`/`integer` (32-bit) and `bigint` (64-bit) — each over the range of a signed
/// number of that width.
const TINYINT: IntegerWidth = IntegerWidth {
    max: i8::MAX as i64,
    min: i8::MIN as i64,
    bound: "MAX_BYTE",
};
/// See [`TINYINT`].
const SMALLINT: IntegerWidth = IntegerWidth {
    max: i16::MAX as i64,
    min: i16::MIN as i64,
    bound: "MAX_SHORT",
};
/// See [`TINYINT`].
const INT: IntegerWidth = IntegerWidth {
    max: i32::MAX as i64,
    min: i32::MIN as i64,
    bound: "MAX_INT",
};
/// See [`TINYINT`].
const BIGINT: IntegerWidth = IntegerWidth {
    max: i64::MAX,
    min: i64::MIN,
    bound: "MAX_LONG",
};

/// Why `column` cannot be read from the leaf `held` of the object at `split`, or `None` when
/// it can.
///
/// Two refusals, in this order: a declaration of another type family cannot be read from
/// those bytes at all, and a value the declared type has no room for is not a value of it.
/// Everything else is readable — including a declaration WIDER than the stored leaf, which is
/// the schema change a client makes to keep older files readable.
///
/// `stored_bytes` is how many bytes that object holds, which is the byte range of it the read
/// was given — what the reader names beside the split it could not get through.
pub fn refusal(
    split: &str,
    stored_bytes: usize,
    column: &DeclaredColumn,
    held: &LeafValue,
) -> Option<Failure> {
    let stored = family_of_leaf(held.stored)?;
    let declared = family_of_declaration(&column.data_type)?;
    if !same_family(stored, declared) {
        return Some(incompatible(split, column, held.stored));
    }
    let Family::Integer(declared) = declared else {
        return None;
    };
    // The stored leaf is an integer, so its value is one — and a value the declared type has
    // no room for is reported with the bound it passed, not narrowed to fit. A value of the
    // stored width that this platform cannot read as a number at all is left to the reader.
    let value: i64 = held.cell.text().trim().parse().ok()?;
    if value > declared.max || value < declared.min {
        return Some(out_of_range(split, stored_bytes, value, declared.bound));
    }
    None
}

/// Whether a column DECLARED in the family `declared` can be read from a leaf stored in the
/// family `stored`.
///
/// The same family, whatever the width: a wider integral declaration over a narrower stored
/// integer is the schema change that keeps older files readable, and the width is judged on
/// the VALUE rather than on the declaration ([`refusal`]).
fn same_family(stored: Family, declared: Family) -> bool {
    matches!(
        (stored, declared),
        (Family::Integer(..), Family::Integer(..))
            | (Family::Floating, Family::Floating)
            | (Family::Boolean, Family::Boolean)
            | (Family::Characters, Family::Characters)
    )
}

/// The family a stored leaf belongs to, or `None` for a leaf whose family these two
/// vocabularies do not share.
fn family_of_leaf(stored: LeafType) -> Option<Family> {
    match stored {
        LeafType::Boolean => Some(Family::Boolean),
        LeafType::Int32 => Some(Family::Integer(INT)),
        LeafType::Int64 => Some(Family::Integer(BIGINT)),
        LeafType::Float | LeafType::Double => Some(Family::Floating),
        LeafType::ByteArray => Some(Family::Characters),
        LeafType::Other => None,
    }
}

/// The family the Hive type `declared` belongs to, or `None` for a declaration outside the
/// families the two vocabularies share.
///
/// The BASE name decides it, so a bounded character type and a parameterised one are read as
/// the family they belong to whatever their parameters say.
fn family_of_declaration(declared: &str) -> Option<Family> {
    match glue_state::base_hive_type(declared).as_str() {
        "tinyint" | "byte" => Some(Family::Integer(TINYINT)),
        "smallint" | "short" => Some(Family::Integer(SMALLINT)),
        "int" | "integer" => Some(Family::Integer(INT)),
        "bigint" | "long" => Some(Family::Integer(BIGINT)),
        "float" | "double" | "real" => Some(Family::Floating),
        "boolean" => Some(Family::Boolean),
        "string" | "varchar" | "char" | "binary" => Some(Family::Characters),
        _ => None,
    }
}

/// How Hive classifies DATA that is not what the table declares: the bytes the object holds
/// under a column are not readable as that column's declared type, which is what the file's
/// own schema says about it — known before a value of it is read.
const HIVE_BAD_DATA: &str = "HIVE_BAD_DATA";

/// The failure a column declared in another type family than the leaf stored for it reports.
///
/// Hive's class for data that is not what the table declares, and then the column, the type
/// the FILE stores it as, where that file is, and the type the TABLE declares. The two
/// schemas contradict each other whatever any record holds, so this is reported on the FILE
/// rather than as a split the reader got part way through — which is what tells its wording
/// from [`out_of_range`]'s.
///
/// Source: Athena's own troubleshooting reference for this class, which states the sentence
/// for a Parquet column whose stored type the table's declaration contradicts. The declared
/// type is stated as the name Athena's SQL gives it
/// (`super::super::hive_types::reported_type`) — the name the engine reading the column knows
/// it by, and the one the harness's own expectation for this failure carries. Every
/// declaration that reaches here HAS such a name: a collection or record type, the one kind
/// that has none, belongs to no family this reading shares with the file's own vocabulary and
/// is never refused here ([`family_of_declaration`]), so one message never mixes the two
/// vocabularies.
fn incompatible(split: &str, column: &DeclaredColumn, stored: LeafType) -> Failure {
    let declared = super::super::hive_types::reported_type(&column.data_type)
        .unwrap_or_else(|| column.data_type.clone());
    Failure::user(format!(
        "{HIVE_BAD_DATA}: Field {}'s type {} in parquet file {split} is incompatible with \
         type {declared} defined in table schema",
        column.name,
        stored.name(),
    ))
}

/// The failure a stored value the declared type has no room for reports.
///
/// A VALUE stops this read rather than the file's own schema: the column is declared in the
/// family the file stores it in, and what the declared type cannot hold is the value one
/// record carries — so what the reader gives up on is the split it was reading, which it names
/// with the byte range of the object it was given ([`super::cannot_read_split`]), and then the
/// value and the bound of the declared type it passed.
fn out_of_range(split: &str, stored_bytes: usize, value: i64, bound: &str) -> Failure {
    super::cannot_read_split(split, stored_bytes, &format!("Value {value} exceeds {bound}"))
}

#[cfg(test)]
mod tests {
    use super::super::parquet_objects::Cell;
    use super::*;

    /// One declared column.
    fn column(name: &str, data_type: &str) -> DeclaredColumn {
        DeclaredColumn {
            name: name.to_string(),
            data_type: data_type.to_string(),
            comment: None,
        }
    }

    /// One stored leaf.
    fn leaf(text: &str, stored: LeafType) -> LeafValue {
        LeafValue {
            cell: Cell::Text(text.to_string()),
            stored,
        }
    }

    /// A declaration of the stored leaf's own family is read, a wider integral declaration
    /// over a narrower stored integer is read, a declaration of another family is not, and a
    /// stored value the declared type has no room for is not.
    ///
    /// The rows differ along the stored type, the declared type, the direction of the width
    /// change and the value stored.
    #[test]
    fn a_column_is_read_from_a_leaf_of_its_own_family_that_holds_its_value() {
        // (case, the stored leaf, the declared type, the refusal's own text)
        let cases: Vec<(&str, LeafValue, &str, Option<&str>)> = vec![
            ("the declared type the file stores", leaf("7", LeafType::Int32), "int", None),
            (
                "a wider integral declaration over a narrower stored integer",
                leaf("7", LeafType::Int32),
                "bigint",
                None,
            ),
            (
                "a narrower declaration the stored value still fits",
                leaf("7", LeafType::Int64),
                "smallint",
                None,
            ),
            (
                "text stored as a byte array, declared as a bounded character type",
                leaf("alpha", LeafType::ByteArray),
                "varchar(10)",
                None,
            ),
            (
                "a floating-point leaf declared as the wider floating-point type",
                leaf("1.5", LeafType::Float),
                "double",
                None,
            ),
            (
                "a leaf whose family the two vocabularies do not share",
                leaf("2024-01-02", LeafType::Other),
                "date",
                None,
            ),
            (
                "an integral leaf declared as character data",
                leaf("7", LeafType::Int32),
                "string",
                Some(
                    "HIVE_BAD_DATA: Field id's type INT32 in parquet file s3://b/p/f is \
                     incompatible with type varchar defined in table schema",
                ),
            ),
            (
                "character data declared as an integral type",
                leaf("alpha", LeafType::ByteArray),
                "int",
                Some(
                    "HIVE_BAD_DATA: Field id's type BINARY in parquet file s3://b/p/f is \
                     incompatible with type integer defined in table schema",
                ),
            ),
            (
                "a floating-point leaf declared as an integral type",
                leaf("1.5", LeafType::Double),
                "bigint",
                Some(
                    "HIVE_BAD_DATA: Field id's type DOUBLE in parquet file s3://b/p/f is \
                     incompatible with type bigint defined in table schema",
                ),
            ),
            (
                "a collection declaration, which belongs to none of the shared families",
                leaf("7", LeafType::Int32),
                "array<string>",
                None,
            ),
            (
                "a stored value past the declared type's bound",
                leaf("100000", LeafType::Int32),
                "smallint",
                Some(
                    "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split s3://b/p/f (offset=0, \
                     length=512): Value 100000 exceeds MAX_SHORT",
                ),
            ),
            (
                "a stored value past the declared 32-bit bound",
                leaf("2147483648", LeafType::Int64),
                "int",
                Some(
                    "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split s3://b/p/f (offset=0, \
                     length=512): Value 2147483648 exceeds MAX_INT",
                ),
            ),
            (
                "a stored value below the declared type's bound",
                leaf("-200", LeafType::Int32),
                "tinyint",
                Some(
                    "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split s3://b/p/f (offset=0, \
                     length=512): Value -200 exceeds MAX_BYTE",
                ),
            ),
        ];

        // How many bytes the object every row reads from holds, which its failures name as
        // the byte range the read was given.
        let stored_bytes = 512;
        for (case, held, declared, expected) in cases {
            let refused = refusal("s3://b/p/f", stored_bytes, &column("id", declared), &held);
            let actual = refused.as_ref().map(|failure| failure.message.as_str());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
