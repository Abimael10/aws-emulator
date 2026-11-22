// SPDX-License-Identifier: CC0-1.0

//! What a catalog column's DECLARED type is: whether the catalog's type text names a type
//! at all, and the name a query reports for it.
//!
//! A Data Catalog column carries its type as the text of a Hive type name
//! (`com.amazonaws.glue#Column$Type`, "the data type of this column"), and the catalog
//! stores whatever text a client wrote there. The QUERY layer reads that text as a type:
//!
//! * a column whose type text names no type cannot be read at all — the table's metadata is
//!   invalid, which is what [`names_a_type`] answers and what makes such a read a failure
//!   rather than a query over a column of some fallback type;
//! * a column whose type text DOES name a type has that type in every result the query
//!   computes from it, whatever the values in the objects happen to be — which is what
//!   [`reported_type`] answers, in the type names Athena's own SQL uses.
//!
//! The two spellings differ because the catalog records a type in HIVE's DDL names and a
//! query reports it in Athena's SQL names: Athena's data-types reference states the pairs —
//! `int` is written in DDL and `integer` in SQL, `float` in DDL is `real` in SQL, Hive's
//! unbounded `string` is a `varchar`, and its `binary` a `varbinary` — while the types whose
//! names are the same in both (`boolean`, `tinyint`, `smallint`, `bigint`, `double`,
//! `decimal`, `char`, `varchar`, `date`, `timestamp`) are reported as declared.
//!
//! This is also the ONE place those pairs are held: a view's own definition records a
//! column's type in the engine's names too, and reads them from here
//! ([`super::ddl::engine_type_name`]).
//!
//! What the CATALOG's own readers ask of the same syntax is answered in the catalog's crate
//! (`glue_state::base_hive_type`, which the type a column is held as is read through —
//! [`super::data::engine_type`]): that answers which type a declaration NAMES, while this
//! module answers the two questions a QUERY has about it — whether the text names a type at
//! all, and the name a result reports for it.
//!
//! Nothing here looks at a VALUE. The type of a column is a property of the catalog entry,
//! so a column every object leaves empty keeps the type the catalog declares for it.

/// The Hive type names a catalog column may declare that carry no parameters, each with
/// the name Athena's SQL reports for it.
///
/// Source: Athena's "Data types in Amazon Athena" reference, which lists the DDL name
/// beside the SQL name of every supported type — `int` versus `integer`, `float` versus
/// `real`, `string` and `binary` as the unbounded character and binary types — together with
/// Hive's own spellings of the types whose DDL name is written differently again (`double
/// precision`, and the two interval types, which Hive's metastore writes with underscores).
///
/// The list is the whole set those references give, not the types one query happens to read:
/// a type text outside it makes its table unreadable ([`names_a_type`]), so a type AWS reads
/// and this list omitted would be a table refused here and served there.
const PLAIN_TYPES: &[(&str, &str)] = &[
    ("boolean", "boolean"),
    ("tinyint", "tinyint"),
    ("smallint", "smallint"),
    ("int", "integer"),
    ("integer", "integer"),
    ("bigint", "bigint"),
    ("float", "real"),
    ("real", "real"),
    ("double", "double"),
    ("double precision", "double"),
    ("decimal", "decimal"),
    ("numeric", "decimal"),
    ("char", "char"),
    ("varchar", "varchar"),
    ("string", "varchar"),
    ("binary", "varbinary"),
    ("varbinary", "varbinary"),
    ("json", "json"),
    ("date", "date"),
    ("time", "time"),
    ("time with time zone", "time with time zone"),
    ("timestamp", "timestamp"),
    ("timestamp with time zone", "timestamp with time zone"),
    ("timestamp with local time zone", "timestamp with time zone"),
    ("interval year to month", "interval year to month"),
    ("interval_year_month", "interval year to month"),
    ("interval day to second", "interval day to second"),
    ("interval_day_time", "interval day to second"),
];

/// The Hive type names that take a parenthesised parameter list of DECIMAL DIGITS: the
/// fixed-point type's precision and scale, and the bounded character types' length.
///
/// Source: the same reference — `decimal(precision, scale)`, `char(length)` and
/// `varchar(length)`, and the precision the two time types take (`timestamp(p)`, `time(p)`,
/// each also with a time zone) — and each parameter is a number.
const SIZED_TYPES: &[&str] = &[
    "decimal",
    "numeric",
    "char",
    "varchar",
    "time",
    "time with time zone",
    "timestamp",
    "timestamp with time zone",
    "timestamp with local time zone",
];

/// The Hive type names that take an angle-bracketed list of ELEMENT types: the collection
/// and record types, whose elements are themselves declared types.
///
/// Source: the same reference, which gives `array<data_type>`, `map<primitive_type,
/// data_type>` and `struct<col_name:data_type[comment]>`; `uniontype` is Hive's own
/// alternation of element types and is declared the same way.
const ELEMENT_TYPES: &[&str] = &["array", "map", "struct", "uniontype"];

/// How many levels of element type one declaration may nest before this reader stops
/// reading it: a declaration deeper than that is not a type a query here resolves.
const MAX_NESTING: usize = 8;

/// Whether `declared` — the type text a catalog column carries — NAMES a type.
///
/// Every form above is one: a plain type name, a parameterised one whose parameters are
/// numbers, and a collection or record type each of whose element types names a type in its
/// own right. Anything else names none, including the empty text.
#[must_use]
pub fn names_a_type(declared: &str) -> bool {
    reads(declared, 0).is_some()
}

/// The type name Athena's SQL reports for a column the catalog declares as `declared`, or
/// `None` where this reader reports no single name for it.
///
/// `None` covers both halves of that: a type text that names no type at all, and a
/// collection or record type — whose SQL name is written out of its element types, which
/// this reader does not render.
#[must_use]
pub fn reported_type(declared: &str) -> Option<String> {
    match reads(declared, 0)? {
        Read::Plain(reported) => Some(reported.to_string()),
        // The parameters go back where a declaration writes them: after the FIRST word of
        // the type's name, so a type whose name carries words after its parameters reads
        // back as one name again (`timestamp(3) with time zone`).
        Read::Sized { reported, sizes } => Some(match reported.split_once(' ') {
            None => format!("{reported}({sizes})"),
            Some((first, rest)) => format!("{first}({sizes}) {rest}"),
        }),
        Read::Elements => None,
    }
}

/// Whether `declared` — the type text a catalog column carries — names a COLLECTION or
/// RECORD type: one whose values are themselves values of a declared type, written with the
/// angle brackets `array<…>`, `map<…,…>`, `struct<…>` and `uniontype<…>` take.
///
/// Such a column's value is a STRUCTURE rather than one scalar, so a statement reads INTO it
/// — a field, an element or an entry of it — which is what makes this question worth asking
/// (see [`super::nested_values`]).
#[must_use]
pub fn is_nested(declared: &str) -> bool {
    matches!(reads(declared, 0), Some(Read::Elements))
}

/// What reading one type text established.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Read<'t> {
    /// A type name with no parameters, carrying the name Athena's SQL reports for it.
    Plain(&'t str),
    /// A parameterised type, carrying that name and the parameters as declared.
    Sized {
        /// The name Athena's SQL reports for it.
        reported: &'t str,
        /// Its parameters, written as one comma-separated list.
        sizes: String,
    },
    /// A collection or record type, whose element types all read as types themselves.
    Elements,
}

/// Read one type text, `depth` levels inside the declaration it is an element of.
fn reads(declared: &str, depth: usize) -> Option<Read<'static>> {
    if depth > MAX_NESTING {
        return None;
    }
    // The catalog holds whatever text a client wrote, so the spacing inside a type name of
    // several words is read as one separator however it was written.
    let text = declared
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    if let Some((head, rest)) = text.split_once('<') {
        let head = head.trim();
        let elements = rest.strip_suffix('>')?;
        if !ELEMENT_TYPES.contains(&head) || !reads_elements(elements, depth) {
            return None;
        }
        return Some(Read::Elements);
    }
    if let Some((head, rest)) = text.split_once('(') {
        // A parameterised name may carry words AFTER its parameters (`timestamp(3) with time
        // zone`), so the name it is looked up under is the head and that tail together.
        let (parameters, tail) = rest.split_once(')')?;
        let named = match tail.trim() {
            "" => head.trim().to_string(),
            tail => format!("{} {tail}", head.trim()),
        };
        let sizes = parameters.trim().to_string();
        let reported = reported_name(&named)?;
        if !SIZED_TYPES.contains(&named.as_str()) || !names_sizes(&sizes) {
            return None;
        }
        return Some(Read::Sized { reported, sizes });
    }
    Some(Read::Plain(reported_name(&text)?))
}

/// The name Athena's SQL reports for the plain Hive type name `head`.
fn reported_name(head: &str) -> Option<&'static str> {
    PLAIN_TYPES
        .iter()
        .find(|(hive, _)| *hive == head)
        .map(|(_, reported)| *reported)
}

/// Whether every element of an angle-bracketed list reads as a type.
///
/// The list is split at its TOP-LEVEL commas, so a nested collection's own elements stay
/// with it, and a record's entry carries its field name in front of the type — which is
/// taken off before the type is read.
fn reads_elements(elements: &str, depth: usize) -> bool {
    let elements = elements.trim();
    if elements.is_empty() {
        return false;
    }
    top_level_terms(elements).is_some_and(|terms| {
        terms
            .iter()
            .all(|term| reads(element_type(term), depth + 1).is_some())
    })
}

/// The type one element of a collection or record declares: the text after its field name
/// where it carries one, and the whole term otherwise.
///
/// A record's entry is `name:type`, and the separator cannot appear in a type name, so the
/// first one closes the field name. A nested collection's own entries are inside its
/// brackets and are never looked at here.
fn element_type(term: &str) -> &str {
    match term.split_once(':') {
        Some((name, data_type)) if !name.contains('<') && !name.contains('(') => data_type,
        _ => term,
    }
}

/// The comma-separated terms of `elements` at the TOP level of its bracketing, or `None`
/// when its brackets do not balance.
fn top_level_terms(elements: &str) -> Option<Vec<&str>> {
    let mut terms: Vec<&str> = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (position, character) in elements.char_indices() {
        match character {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                terms.push(&elements[start..position]);
                start = position + character.len_utf8();
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    terms.push(&elements[start..]);
    Some(terms)
}

/// Whether a parameterised type's parameters are the numbers such a type takes: one or two
/// of them, each a run of decimal digits.
fn names_sizes(sizes: &str) -> bool {
    let terms: Vec<&str> = sizes.split(',').map(str::trim).collect();
    (1..=2).contains(&terms.len())
        && terms
            .iter()
            .all(|term| !term.is_empty() && term.chars().all(|digit| digit.is_ascii_digit()))
}

/// The Hive DDL name a catalog column is DECLARED with for a column whose type a query
/// reports as `reported`.
///
/// The inverse of [`reported_type`] over the same pairs: a statement that DEFINES a table
/// from the rows a query produced knows each column by the name Athena's SQL reports for its
/// type, and the catalog records a column's type in HIVE's DDL names — so the few names that
/// differ are mapped back here, and every type whose two names are the same is recorded as
/// reported. A parameterised type keeps its parameters.
///
/// A column the query reports NO type for — a projection of nothing but NULLs, whose type
/// nothing fixes — is recorded as the unbounded character type, which is the type such a
/// column is read back as.
#[must_use]
pub fn declared_type(reported: &str) -> String {
    let (named, parameters) = match reported.trim().split_once('(') {
        Some((named, rest)) => (named.trim(), rest.split_once(')').map(|(sizes, _)| sizes)),
        None => (reported.trim(), None),
    };
    let declared = match parameters {
        // A PARAMETERISED type is declared under its own name: every name mapped below is
        // a plain one, and the bounded character type is declared as itself with the
        // length it carries rather than as the unbounded type's name.
        Some(_) => named,
        // A column the query reports no type for AT ALL is recorded as the unbounded
        // character type, exactly as one it reports the unknown type for is.
        None if named.is_empty() => UNDECLARED_TYPE,
        None => DECLARED_NAMES
            .iter()
            .find(|(sql, _)| sql.eq_ignore_ascii_case(named))
            .map_or(named, |(_, hive)| *hive),
    };
    match parameters {
        Some(sizes) => format!("{declared}({sizes})"),
        None => declared.to_string(),
    }
}

/// The type names a query reports that the catalog DECLARES under another name, each with
/// that name; see [`declared_type`].
///
/// Source: the same reference [`PLAIN_TYPES`] is read from, taken the other way round — the
/// SQL name beside the DDL one. Only the names that DIFFER are listed: a type whose two
/// names are the same needs no entry, and recording one for it would be a second place the
/// pair is spelled.
const DECLARED_NAMES: &[(&str, &str)] = &[
    ("integer", "int"),
    ("real", "float"),
    ("varbinary", "binary"),
    // The unbounded character type is `string` in DDL; the bounded one keeps its own name
    // and its length, which the parameters carried beside it supply.
    ("varchar", "string"),
    // A column of no type at all is recorded as the unbounded character type.
    ("unknown", UNDECLARED_TYPE),
];

/// The Hive type a column whose type a query reports as nothing at all is DECLARED with: the
/// unbounded character type, which is the type such a column is read back as.
const UNDECLARED_TYPE: &str = "string";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a column's type text names a type exactly where it is one of the declared
    /// forms, and the name a query reports for it is that type's own SQL name.
    ///
    /// The rows span what the rule generalises over: the DDL names whose SQL name differs
    /// from them, the ones where it does not, the parameterised forms, the type names of
    /// several words, the collection and record forms, the case and spacing the catalog may
    /// hold the text in, and text that names no type at all. The entries the reported issue
    /// never cited — `tinyint`, `float`, `char(1)`, `array<...>`, the interval and zoned time
    /// types — are rows of their own.
    #[test]
    fn a_declared_type_is_read_as_the_type_its_text_names() {
        // (the type text the catalog holds, whether it names a type, the name a query
        // reports for it)
        let cases: Vec<(&str, bool, Option<&str>)> = vec![
            ("boolean", true, Some("boolean")),
            ("tinyint", true, Some("tinyint")),
            ("smallint", true, Some("smallint")),
            ("int", true, Some("integer")),
            ("integer", true, Some("integer")),
            ("bigint", true, Some("bigint")),
            ("float", true, Some("real")),
            ("double", true, Some("double")),
            ("double precision", true, Some("double")),
            ("string", true, Some("varchar")),
            ("binary", true, Some("varbinary")),
            ("json", true, Some("json")),
            ("date", true, Some("date")),
            ("time", true, Some("time")),
            ("timestamp", true, Some("timestamp")),
            // The types whose name is several words, including the two the metastore writes
            // with underscores.
            (
                "timestamp with time zone",
                true,
                Some("timestamp with time zone"),
            ),
            (
                "timestamp with local time zone",
                true,
                Some("timestamp with time zone"),
            ),
            ("time with time zone", true, Some("time with time zone")),
            (
                "interval year to month",
                true,
                Some("interval year to month"),
            ),
            (
                "interval_year_month",
                true,
                Some("interval year to month"),
            ),
            (
                "interval day to second",
                true,
                Some("interval day to second"),
            ),
            (
                "interval_day_time",
                true,
                Some("interval day to second"),
            ),
            // The catalog holds whatever case and spacing a client wrote.
            ("BigInt", true, Some("bigint")),
            ("  int  ", true, Some("integer")),
            ("DOUBLE  PRECISION", true, Some("double")),
            // The parameterised forms keep their parameters, which sit after the first word
            // of the name.
            ("decimal(10,2)", true, Some("decimal(10,2)")),
            ("decimal(38)", true, Some("decimal(38)")),
            ("varchar(20)", true, Some("varchar(20)")),
            ("char(1)", true, Some("char(1)")),
            ("decimal", true, Some("decimal")),
            ("timestamp(3)", true, Some("timestamp(3)")),
            (
                "timestamp(6) with time zone",
                true,
                Some("timestamp(6) with time zone"),
            ),
            // A collection or record type is a type, and no single name is reported for it.
            ("array<string>", true, None),
            ("map<string,int>", true, None),
            ("struct<id:int,label:string>", true, None),
            ("array<struct<id:int>>", true, None),
            ("uniontype<int,string>", true, None),
            ("array<timestamp with time zone>", true, None),
            // And text that names no type at all.
            ("notatype", false, None),
            ("", false, None),
            ("   ", false, None),
            ("intt", false, None),
            ("decimal(ten,2)", false, None),
            ("decimal(1,2,3)", false, None),
            ("varchar(20", false, None),
            ("bigint(10)", false, None),
            ("timestamp(3) with no time zone", false, None),
            ("array<notatype>", false, None),
            ("array<>", false, None),
            ("notacollection<int>", false, None),
            ("struct<id:notatype>", false, None),
            ("map<string,array<notatype>>", false, None),
        ];

        for (declared, names, reported) in &cases {
            let read = names_a_type(declared);
            assert_eq!(
                read, *names,
                "case {declared:?}: expected names_a_type={names}, got {read}"
            );
            let actual = reported_type(declared);
            assert_eq!(
                actual.as_deref(),
                *reported,
                "case {declared:?}: expected the reported type {reported:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the Hive name a column is DECLARED with, for a type a query reports, is that
    /// type's DDL name — and every declaration this answers names a type, so a table defined
    /// from the rows a query produced can be read back.
    ///
    /// The rows span what the rule generalises over: the names that differ between the two
    /// spellings, the ones that do not, the parameterised forms (whose name is their own),
    /// the case the reported name may carry, and a column no type at all is reported for.
    #[test]
    fn a_reported_type_is_declared_under_its_own_hive_name() {
        // (the type a query reports, the Hive name the catalog declares it under)
        let cases: Vec<(&str, &str)> = vec![
            ("integer", "int"),
            ("bigint", "bigint"),
            ("double", "double"),
            ("real", "float"),
            ("boolean", "boolean"),
            ("varchar", "string"),
            ("varbinary", "binary"),
            ("tinyint", "tinyint"),
            ("date", "date"),
            ("timestamp", "timestamp"),
            ("decimal(10,2)", "decimal(10,2)"),
            ("varchar(20)", "varchar(20)"),
            ("timestamp(3)", "timestamp(3)"),
            ("INTEGER", "int"),
            // A column the query reports no type for at all.
            ("unknown", "string"),
            ("", "string"),
            ("   ", "string"),
        ];
        for (reported, expected) in &cases {
            let actual = declared_type(reported);
            assert_eq!(
                actual, *expected,
                "case {reported:?}: expected the declared type {expected:?}, got {actual:?}"
            );
            assert!(
                names_a_type(&actual),
                "case {reported:?}: the declared type {actual:?} must name a type"
            );
        }
    }

    /// RULE: a declaration nested deeper than this reader reads names no type, so a
    /// pathologically nested text is reported rather than walked without bound.
    #[test]
    fn a_declaration_nested_past_the_bound_names_no_type() {
        let nested = |levels: usize| {
            let mut text = String::from("int");
            for _ in 0..levels {
                text = format!("array<{text}>");
            }
            text
        };
        // (case, the declaration, whether it names a type)
        let cases: Vec<(&str, String, bool)> = vec![
            ("one level", nested(1), true),
            ("every level this reader reads", nested(MAX_NESTING), true),
            ("the first level past the bound", nested(MAX_NESTING + 1), false),
            ("well past the bound", nested(MAX_NESTING * 2), false),
        ];
        for (case, declared, expected) in &cases {
            let actual = names_a_type(declared);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected names_a_type={expected} for {declared:?}, got {actual}"
            );
        }
    }
}
