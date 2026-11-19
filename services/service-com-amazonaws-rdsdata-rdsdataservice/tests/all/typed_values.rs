// SPDX-License-Identifier: CC0-1.0

//! What a TYPED value is sent to its column as, and what the column's catalog entry
//! says about it, over the real wire.
//!
//! Every call goes through the router, so routing, restJson1 deserialization, the
//! handler and serialization are all exercised; the client harness is the one
//! `sql_data_api` builds.

use serde_json::{json, Value};

use crate::sql_data_api::{error_code, list, number, parameter, DataApi};

/// RULE: a value of a BINARY type can be written as TEXT, in the input format
/// PostgreSQL documents for `bytea` — its hex format, a `\x` prefix and two
/// hexadecimal digits per byte — and what the column then holds is the BYTES that text
/// spells, so the value round-trips byte-identically into `blobValue` (which this
/// protocol carries base64-encoded) whether the text is a literal the statement
/// carries or a value a parameter binds.
///
/// Each row pairs a byte string, written in hexadecimal, with the base64 text that is
/// its encoding; both are written here rather than computed, so what the case asserts
/// is the encoding and not the implementation's own codec. The bytes really stored are
/// read back through the engine's own `hex` and `length`, which is independent of how
/// the response renders them. The rows differ along the byte string — arbitrary bytes,
/// bytes that are zero, a length base64 pads, and none at all — and along HOW the value
/// reached the column.
#[tokio::test]
async fn a_binary_value_written_in_its_input_format_keeps_its_bytes() {
    let cases = [
        ("three arbitrary bytes", "0102ff", "AQL/"),
        ("bytes that are zero", "000000", "AAAA"),
        ("one byte, a length base64 pads", "ff", "/w=="),
        ("no bytes at all", "", ""),
    ];
    // (how the value reaches the column, the expression, the parameters it needs)
    let forms = [
        "a literal cast in the statement",
        "a parameter cast in the statement",
        "a parameter of the binary member",
    ];

    for (index, (case, bytes_in_hex, as_base64)) in cases.into_iter().enumerate() {
        for form in forms {
            let api = DataApi::new();
            api.run(json!({"sql": "CREATE TABLE payloads (id INT, payload BYTEA)"}))
                .await;
            let hex_text = format!("\\x{bytes_in_hex}");
            let (value, parameters) = match form {
                "a literal cast in the statement" => {
                    (format!("'{hex_text}'::bytea"), json!([]))
                }
                "a parameter cast in the statement" => (
                    ":payload::bytea".to_string(),
                    json!([parameter("payload", json!({"stringValue": hex_text}))]),
                ),
                _ => (
                    ":payload".to_string(),
                    json!([parameter("payload", json!({"blobValue": as_base64}))]),
                ),
            };
            api.run(json!({
                "sql": format!("INSERT INTO payloads (id, payload) VALUES ({index}, {value})"),
                "parameters": parameters,
            }))
            .await;

            // What the engine holds, read back through the engine itself.
            let bytes = hex::decode(bytes_in_hex).expect("each case writes whole bytes in hex");
            let stored = api
                .run(json!({
                    "sql": "SELECT hex(payload) AS written, length(payload) AS bytes \
                            FROM payloads",
                }))
                .await;
            assert_eq!(
                stored["records"],
                json!([[
                    {"stringValue": hex::encode_upper(&bytes)},
                    {"longValue": bytes.len()},
                ]]),
                "case {case:?} as {form}: expected the {} byte(s) of {bytes_in_hex:?} \
                 to be stored, got {stored}",
                bytes.len()
            );

            // And what each form of the result set renders them as.
            let records = api
                .run(json!({"sql": "SELECT payload FROM payloads"}))
                .await;
            assert_eq!(
                records["records"],
                json!([[{"blobValue": as_base64}]]),
                "case {case:?} as {form}: expected blobValue {as_base64:?}, got {records}"
            );
            let formatted = api
                .run(json!({
                    "sql": "SELECT payload FROM payloads",
                    "formatRecordsAs": "JSON",
                }))
                .await;
            assert_eq!(
                formatted["formattedRecords"],
                json!(format!("[{{\"payload\":{:?}}}]", as_base64)),
                "case {case:?} as {form}: expected the JSON form {as_base64:?}, \
                 got {formatted}"
            );
        }
    }
}

/// RULE: a `typeHint` names the type its `stringValue` is SENT AS, so what reaches the
/// column is a value of that type and not the request's spelling of one — and a round
/// trip therefore answers that type's own form of the value: a JSON document's members
/// come back in name order, whatever order the request wrote them in, and a time of day
/// comes back with the thousandths of a second its documented `HH:MM:SS[.FFF]` carries,
/// while the hints whose documented format admits exactly one spelling of each value
/// come back as they were written.
///
/// The rows span the six hints the model declares (`TypeHint`: DATE, DECIMAL, JSON,
/// TIME, TIMESTAMP, UUID), each into a column of that type, and differ inside the two
/// that normalise along what the request's spelling differs by: the order of an
/// object's members, the nesting it is at, the whitespace around them, and the number
/// of fractional digits a time is written with.
#[tokio::test]
async fn a_hinted_parameter_reaches_its_column_as_that_types_own_value() {
    // (case, the hint, the column's declared type, the string sent, the string back)
    let cases = [
        (
            "a JSON object whose members are not in the document's order",
            "JSON",
            "JSONB",
            r#"{"b":1,"a":[2,3]}"#,
            r#"{"a":[2,3],"b":1}"#,
        ),
        (
            "a JSON object nesting another, written with whitespace",
            "JSON",
            "JSONB",
            r#"{ "z": 1, "aa": {"d":1,"c":2} }"#,
            r#"{"aa":{"c":2,"d":1},"z":1}"#,
        ),
        (
            "a JSON array, whose order is the document's own",
            "JSON",
            "JSONB",
            "[3,1,2]",
            "[3,1,2]",
        ),
        (
            "a time of day written with one fractional digit",
            "TIME",
            "TIME",
            "18:30:00.5",
            "18:30:00.500",
        ),
        (
            "a time of day written with more digits than the format carries",
            "TIME",
            "TIME",
            "18:30:00.123456",
            "18:30:00.123",
        ),
        (
            "a time of day written with the three digits the format carries",
            "TIME",
            "TIME",
            "23:59:59.999",
            "23:59:59.999",
        ),
        (
            "a time of day with no fraction at all",
            "TIME",
            "TIME",
            "00:00:00",
            "00:00:00",
        ),
        (
            "a date",
            "DATE",
            "DATE",
            "2026-10-22",
            "2026-10-22",
        ),
        (
            "a timestamp carrying microseconds",
            "TIMESTAMP",
            "TIMESTAMP",
            "2026-10-22 18:30:00.123456",
            "2026-10-22 18:30:00.123456",
        ),
        (
            "a timestamp carrying none",
            "TIMESTAMP",
            "TIMESTAMP",
            "2026-10-22 18:30:00",
            "2026-10-22 18:30:00",
        ),
        (
            "a UUID",
            "UUID",
            "UUID",
            "123e4567-e89b-12d3-a456-426614174000",
            "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "an exact number at the column's own scale",
            "DECIMAL",
            "NUMERIC(12,4)",
            "123.4500",
            "123.4500",
        ),
    ];

    for (index, (case, hint, declared, sent, back)) in cases.into_iter().enumerate() {
        let api = DataApi::new();
        let table = format!("typed_{index}");
        api.run(json!({"sql": format!("CREATE TABLE {table} (value {declared})")}))
            .await;
        api.run(json!({
            "sql": format!("INSERT INTO {table} (value) VALUES (:value)"),
            "parameters": [
                json!({"name": "value", "value": {"stringValue": sent}, "typeHint": hint}),
            ],
        }))
        .await;

        let round_trip = api
            .run(json!({"sql": format!("SELECT value FROM {table}")}))
            .await;
        assert_eq!(
            round_trip["records"],
            json!([[{"stringValue": back}]]),
            "case {case:?} under typeHint {hint}: expected {back:?} back, got {round_trip}"
        );
        assert_eq!(
            number(&round_trip, "numberOfRecordsUpdated"),
            0,
            "case {case:?}: a SELECT changes no rows, got {round_trip}"
        );
    }
}

/// RULE: a value a statement writes out as TEXT is only a value of the type it is
/// written as if that type reads it: a `stringValue` with no `typeHint` is TEXT, so a
/// column of another type does not take it, and text cast to a NUMBER type must be a
/// number of it. Either way the database refuses the statement with its own
/// diagnostic — naming the column and the two types, or the type and the text — rather
/// than storing or computing a value the caller did not write. The `typeHint` and a
/// CAST the statement writes are the two ways of saying which type the text is.
///
/// The rows span the type families a text value can be refused for (a document, a
/// UUID, a date, a time, a timestamp, an exact number, an integer, a boolean, binary)
/// and the two ways the same value IS accepted.
#[tokio::test]
async fn text_is_refused_for_a_column_and_a_cast_that_do_not_read_it() {
    let api = DataApi::new();
    // (case, the column's declared type, the type the database names it by, the text)
    let columns = [
        ("a document", "JSONB", "jsonb", r#"{"a":1}"#),
        ("a UUID", "UUID", "uuid", "123e4567-e89b-12d3-a456-426614174000"),
        ("a date", "DATE", "date", "2026-10-22"),
        (
            "a time of day",
            "TIME",
            "time without time zone",
            "18:30:00.500",
        ),
        (
            "a timestamp",
            "TIMESTAMP",
            "timestamp without time zone",
            "2026-10-22 18:30:00",
        ),
        (
            "a zone-aware timestamp",
            "TIMESTAMPTZ",
            "timestamp with time zone",
            "2026-10-22 18:30:00",
        ),
        ("an exact number", "NUMERIC(12,4)", "numeric", "123.4500"),
        ("an integer", "INT", "integer", "42"),
        ("a boolean", "BOOLEAN", "boolean", "true"),
        ("binary data", "BYTEA", "bytea", r"\x0102ff"),
    ];

    for (index, (case, declared, named, text)) in columns.into_iter().enumerate() {
        let table = format!("refused_{index}");
        api.run(json!({"sql": format!("CREATE TABLE {table} (value {declared})")}))
            .await;
        let statement = format!("INSERT INTO {table} (value) VALUES (:value)");        // The placeholder's own position in the statement, which is where the database
        // reports an error about the value it carries: the characters before it, and
        // then the character it starts at.
        let position = statement.find(":value").expect("the statement binds it") + 1;
        let (status, refused) = api
            .execute(json!({
                "sql": statement,
                "parameters": [parameter("value", json!({"stringValue": text}))],
            }))
            .await;
        assert_eq!(
            (status, error_code(&refused), refused["message"].clone()),
            (
                400,
                "DatabaseErrorException",
                json!(format!(
                    "ERROR: column \"value\" is of type {named} but expression is of type \
                     text; Hint: You will need to rewrite or cast the expression.; \
                     Position: {position}; SQLState: 42804"
                ))
            ),
            "case {case:?}: expected the database's datatype-mismatch diagnostic, \
             got {status} {refused}"
        );
        assert!(
            list(&api.run(json!({"sql": format!("SELECT value FROM {table}")})).await, "records")
                .is_empty(),
            "case {case:?}: the refused statement stored nothing"
        );
    }

    // And the two ways the same text IS a value of the column's type: a cast the
    // statement writes, and the `typeHint` that names the type the string is sent as.
    api.run(json!({"sql": "CREATE TABLE accepted (value NUMERIC(12,4))"}))
        .await;
    api.run(json!({
        "sql": "INSERT INTO accepted (value) VALUES (CAST(:value AS NUMERIC(12,4)))",
        "parameters": [parameter("value", json!({"stringValue": "123.4500"}))],
    }))
    .await;
    api.run(json!({
        "sql": "INSERT INTO accepted (value) VALUES (:value)",
        "parameters": [
            json!({"name": "value", "value": {"stringValue": "123.4500"}, "typeHint": "DECIMAL"}),
        ],
    }))
    .await;
    let stored = api
        .run(json!({"sql": "SELECT value FROM accepted"}))
        .await;
    assert_eq!(
        stored["records"],
        json!([[{"stringValue": "123.4500"}], [{"stringValue": "123.4500"}]]),
        "case \"a cast and a typeHint\": both ways store the value, got {stored}"
    );

    // Text cast to a NUMBER type has to be a number of it, whatever the statement
    // writes it in and wherever the text comes from: the rows differ along the type,
    // along why the text is no value of it, and along whether the statement carries the
    // text itself or binds it as a parameter.
    let casts = [
        ("a word cast to a float", "'abc'::float8", "double precision", "abc"),
        ("empty text cast to a float", "''::float8", "double precision", ""),
        ("a word cast to an exact number", "'abc'::numeric", "numeric", "abc"),
        ("a fraction cast to an integer", "'1.5'::int4", "integer", "1.5"),
        ("a word cast to a wide integer", "'x'::int8", "bigint", "x"),
        (
            "a number with a trailing word cast to an integer",
            "CAST('42 rows' AS SMALLINT)",
            "smallint",
            "42 rows",
        ),
    ];
    for (case, projection, named, text) in casts {
        let (status, refused) = api
            .execute(json!({"sql": format!("SELECT {projection} AS computed")}))
            .await;
        assert_eq!(
            (status, error_code(&refused), refused["message"].clone()),
            (
                400,
                "DatabaseErrorException",
                json!(format!(
                    "ERROR: invalid input syntax for type {named}: \"{text}\"; \
                     SQLState: 22P02"
                ))
            ),
            "case {case:?}: expected the database's invalid-input diagnostic, \
             got {status} {refused}"
        );
    }
    // The same text, bound as a parameter instead of written into the statement.
    for (case, cast_to, named, text) in [
        ("a word cast to a float", "float8", "double precision", "abc"),
        ("a fraction cast to an integer", "int4", "integer", "1.5"),
        ("a word cast to an exact number", "numeric", "numeric", "abc"),
    ] {
        let (status, refused) = api
            .execute(json!({
                "sql": format!("SELECT CAST(:value AS {cast_to}) AS computed"),
                "parameters": [parameter("value", json!({"stringValue": text}))],
            }))
            .await;
        assert_eq!(
            (status, error_code(&refused), refused["message"].clone()),
            (
                400,
                "DatabaseErrorException",
                json!(format!(
                    "ERROR: invalid input syntax for type {named}: \"{text}\"; \
                     SQLState: 22P02"
                ))
            ),
            "case {case:?} as a parameter: expected the database's invalid-input \
             diagnostic, got {status} {refused}"
        );
    }

    // A number the type DOES read is computed, whichever spelling it is written in.
    for (case, projection, expected) in [
        ("a whole number", "'42'::int4", json!({"longValue": 42})),
        ("a signed number", "'-7'::int8", json!({"longValue": -7})),
        ("a fraction", "'1.5'::float8", json!({"doubleValue": 1.5})),
        ("an exponent", "'1e3'::float8", json!({"doubleValue": 1000.0})),
        ("padded with spaces", "' 42 '::int4", json!({"longValue": 42})),
    ] {
        let computed = api
            .run(json!({"sql": format!("SELECT {projection} AS computed")}))
            .await;
        assert_eq!(
            computed["records"],
            json!([[expected]]),
            "case {case:?}: got {computed}"
        );
    }
}

/// RULE: `columnMetadata` describes a COMPUTED column by the type the statement's own
/// text gives it and not by the column it derives from: an arithmetic over an exact
/// column is an exact number of no declared precision, one over an inexact column is
/// inexact, and either way the column is stored in no table — so `tableName` is empty
/// and the precision is the type's own.
///
/// The rows differ along the operand types the rule generalises over and along the
/// operator, and the table is EMPTY, so what answers is the type the statement names
/// rather than the storage class a value happened to arrive in.
#[tokio::test]
async fn a_computed_column_is_described_by_the_type_the_statement_gives_it() {
    let api = DataApi::new();
    api.run(json!({
        "sql": "CREATE TABLE amounts (exact NUMERIC(12,2), inexact DOUBLE PRECISION, \
                whole INT)",
    }))
    .await;
    // (case, the projection, typeName, precision, scale)
    let cases = [
        ("an exact column times a number", "exact * 2", "numeric", 0, 0),
        ("a number plus an exact column", "1 + exact", "numeric", 0, 0),
        ("an exact column over another", "exact / exact", "numeric", 0, 0),
        (
            "an inexact column times an exact one",
            "inexact * exact",
            "float8",
            17,
            17,
        ),
        ("an inexact column minus a number", "inexact - 1", "float8", 17, 17),
    ];

    for (case, projection, type_name, precision, scale) in cases {
        let described = api
            .run(json!({
                "sql": format!("SELECT {projection} AS computed FROM amounts"),
                "includeResultMetadata": true,
            }))
            .await;
        let entry = list(&described, "columnMetadata")
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            (
                entry["typeName"].clone(),
                entry["tableName"].clone(),
                json!(number(&entry, "precision")),
                json!(number(&entry, "scale")),
                entry["nullable"].clone(),
            ),
            (
                json!(type_name),
                json!(""),
                json!(precision),
                json!(scale),
                json!(1),
            ),
            "case {case:?}: expected the type the statement gives it, got {entry}"
        );
    }
}

/// RULE: a member whose values the model declares as an ENUM carries one of those
/// values: a request naming any other is refused against the model, in the framework's
/// own sentence, before the statement runs — it is not read as the member's default,
/// which would answer a caller that asked for one form of the result set with another.
///
/// The rows are the three enumerated members of this operation's result options
/// (`RecordsFormatType` NONE|JSON, `LongReturnType` STRING|LONG, `DecimalReturnType`
/// STRING|DOUBLE_OR_LONG — `smithy_query(query="shape", shape="ResultSetOptions")`),
/// each with a value outside its set — one at the top level and two inside the nested
/// structure — and then each of the values inside it, which is served.
#[tokio::test]
async fn a_member_the_model_enumerates_carries_one_of_its_values() {
    let api = DataApi::new();
    // (case, the value sent, the member that carries it, the request, the enum's own
    //  values in the order the model declares them)
    let refused: [(&str, &str, &str, Value, &[&str]); 3] = [
        (
            "a records format the enum does not declare",
            "CSV",
            "formatRecordsAs",
            json!({"sql": "SELECT 1 AS x", "formatRecordsAs": "CSV"}),
            &["NONE", "JSON"],
        ),
        (
            "a long return type the enum does not declare",
            "BIGINT",
            "resultSetOptions.longReturnType",
            json!({"sql": "SELECT 1 AS x", "resultSetOptions": {"longReturnType": "BIGINT"}}),
            &["STRING", "LONG"],
        ),
        (
            "a decimal return type the enum does not declare",
            "DOUBLE",
            "resultSetOptions.decimalReturnType",
            json!({"sql": "SELECT 1 AS x", "resultSetOptions": {"decimalReturnType": "DOUBLE"}}),
            &["STRING", "DOUBLE_OR_LONG"],
        ),
    ];

    for (case, sent, member, request, declared) in refused {
        let (status, body) = api.execute(request).await;
        let listed = declared.join(", ");
        assert_eq!(
            (status, error_code(&body), body["message"].clone()),
            (
                400,
                "ValidationException",
                json!(format!(
                    "1 validation error detected: Value '{sent}' at '{member}' failed to \
                     satisfy constraint: Member must satisfy enum value set: [{listed}]"
                ))
            ),
            "case {case:?}: got {status} {body}"
        );
    }

    // And every value the three enums DO declare is served.
    for (case, members) in [
        ("the default records format", json!({"formatRecordsAs": "NONE"})),
        ("the JSON records format", json!({"formatRecordsAs": "JSON"})),
        (
            "a long returned as a long",
            json!({"resultSetOptions": {"longReturnType": "LONG"}}),
        ),
        (
            "a long returned as a string",
            json!({"resultSetOptions": {"longReturnType": "STRING"}}),
        ),
        (
            "an exact number returned as a string",
            json!({"resultSetOptions": {"decimalReturnType": "STRING"}}),
        ),
        (
            "an exact number returned as a number",
            json!({"resultSetOptions": {"decimalReturnType": "DOUBLE_OR_LONG"}}),
        ),
    ] {
        let mut request = members;
        request["sql"] = json!("SELECT 1 AS x");
        let (status, body) = api.execute(request).await;
        assert_eq!(status, 200, "case {case:?}: got {status} {body}");
    }
}

/// RULE: `columnMetadata` describes a column by the catalog type it has, and the type
/// code it reports is the `java.sql.Types` constant for that type — the code for a type
/// `java.sql.Types` does not name (PostgreSQL's zone-aware timestamp among them) being
/// the one for any other type — with the scale the type's own resolution gives it.
///
/// The rows are the date and time types of the catalog, which differ along exactly
/// that: whether `java.sql.Types` names the type, and what its resolution is.
#[tokio::test]
async fn the_catalog_entry_of_a_date_or_time_column_is_its_own() {
    let api = DataApi::new();
    // (case, the declaration, typeName, the java.sql.Types code, precision, scale)
    let cases = [
        ("a zone-aware timestamp", "TIMESTAMPTZ", "timestamptz", 1111, 35, 6),
        ("a plain timestamp", "TIMESTAMP", "timestamp", 93, 29, 6),
        ("a date, which carries no time at all", "DATE", "date", 91, 13, 0),
        ("a time of day", "TIME", "time", 92, 15, 6),
        ("a zone-aware time of day", "TIMETZ", "timetz", 1111, 21, 6),
    ];

    for (index, (case, declared, type_name, code, precision, scale)) in
        cases.into_iter().enumerate()
    {
        let table = format!("stamps_{index}");
        api.run(json!({"sql": format!("CREATE TABLE {table} (tstz {declared})")}))
            .await;
        let described = api
            .run(json!({
                "sql": format!("SELECT tstz FROM {table}"),
                "includeResultMetadata": true,
            }))
            .await;
        let entry = list(&described, "columnMetadata")
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            entry,
            json!({
                "name": "tstz",
                "label": "tstz",
                "type": code,
                "typeName": type_name,
                "nullable": 1,
                "precision": precision,
                "scale": scale,
                "isSigned": false,
                "isCaseSensitive": false,
                "isAutoIncrement": false,
                "isCurrency": false,
                "arrayBaseColumnType": 0,
                "schemaName": "",
                "tableName": table,
            }),
            "case {case:?}: got {entry}"
        );
    }
}

/// The type the six `typeHint` values the model declares (`TypeHint`: DATE, DECIMAL, JSON,
/// TIME, TIMESTAMP, UUID) send a string as, each the object type the member's own
/// documentation names, as the database words it in a diagnostic.
const HINTED_TYPES: &[(&str, &str)] = &[
    ("DATE", "date"),
    ("DECIMAL", "numeric"),
    ("JSON", "jsonb"),
    ("TIME", "time without time zone"),
    ("TIMESTAMP", "timestamp without time zone"),
    ("UUID", "uuid"),
];

/// One row of the table below: the case, the column's declaration, the value the parameter
/// carries, the `typeHint` it carries, the type that value ARRIVES as, and whether the
/// column takes it.
type AssignmentCase = (
    &'static str,
    &'static str,
    Value,
    Option<&'static str>,
    &'static str,
    bool,
);

/// RULE: a parameter reaches the database as a value of ONE type — the one its `Field`
/// member names, or, for a `stringValue` carrying a `typeHint`, the one that hint names —
/// and the column it is assigned to takes it only where the engine has an implicit or
/// assignment cast from that type: a number reaches any number type, a character value any
/// character type, a document either spelling of one, a date a date or a timestamp, and
/// nothing else is assignable. Otherwise the database refuses the statement with its
/// datatype-mismatch diagnostic — naming the column, its declared type, the type the
/// expression ARRIVED as and where the value is written — and the row does not land.
///
/// The rows differ along what decides the type the value is sent as (each `Field` member,
/// each hint, a hint on a member that is not a string, where the member decides), along the
/// family the column belongs to, and along whether the two are assignable.
#[tokio::test]
async fn an_assigned_parameter_must_be_a_value_its_column_takes() {
    let api = DataApi::new();

    // (case, the column's declaration, the value the parameter carries, the hint it
    //  carries, the type the value ARRIVES as, whether the column takes it)
    let cases: Vec<AssignmentCase> = vec![
        // A hinted string is sent as the hint's type, so a column of another family
        // refuses it.
        (
            "a JSON-hinted document into an integer column",
            "INTEGER",
            json!({"stringValue": r#"{"a":1}"#}),
            Some("JSON"),
            "jsonb",
            false,
        ),
        (
            "a DECIMAL-hinted number into a document column",
            "JSONB",
            json!({"stringValue": "12.5"}),
            Some("DECIMAL"),
            "numeric",
            false,
        ),
        (
            "a DATE-hinted date into a time-of-day column",
            "TIME",
            json!({"stringValue": "2026-10-22"}),
            Some("DATE"),
            "date",
            false,
        ),
        (
            "a TIMESTAMP-hinted instant into a binary column",
            "BYTEA",
            json!({"stringValue": "2026-10-22 18:30:00"}),
            Some("TIMESTAMP"),
            "timestamp without time zone",
            false,
        ),
        // A hint on a member that is not a `stringValue` names nothing: the member already
        // names the type, so THAT is the type the column judges.
        (
            "a UUID hint carried on a longValue, into a UUID column",
            "UUID",
            json!({"longValue": 7}),
            Some("UUID"),
            "bigint",
            false,
        ),
        // And a member on its own, with no hint anywhere.
        (
            "a booleanValue into an integer column",
            "INTEGER",
            json!({"booleanValue": true}),
            None,
            "boolean",
            false,
        ),
        (
            "a longValue into a character column",
            "VARCHAR(16)",
            json!({"longValue": 7}),
            None,
            "bigint",
            false,
        ),
        (
            "a doubleValue into a UUID column",
            "UUID",
            json!({"doubleValue": 1.5}),
            None,
            "double precision",
            false,
        ),
        (
            "a blobValue into an integer column",
            "INTEGER",
            json!({"blobValue": "AQL/"}),
            None,
            "bytea",
            false,
        ),
        (
            "a stringValue with no hint into a boolean column",
            "BOOLEAN",
            json!({"stringValue": "true"}),
            None,
            "text",
            false,
        ),
        // The same values where the column DOES take them: one row per family, including
        // the two casts that cross a type (a date into a timestamp, an exact number into an
        // integer) and the document type's other spelling.
        (
            "a longValue into an integer column",
            "INTEGER",
            json!({"longValue": 7}),
            None,
            "bigint",
            true,
        ),
        (
            "a longValue into an exact-number column",
            "NUMERIC(12,4)",
            json!({"longValue": 7}),
            None,
            "bigint",
            true,
        ),
        (
            "a doubleValue into a floating column",
            "DOUBLE PRECISION",
            json!({"doubleValue": 1.5}),
            None,
            "double precision",
            true,
        ),
        (
            "a booleanValue into a boolean column",
            "BOOLEAN",
            json!({"booleanValue": true}),
            None,
            "boolean",
            true,
        ),
        (
            "a blobValue into a binary column",
            "BYTEA",
            json!({"blobValue": "AQL/"}),
            None,
            "bytea",
            true,
        ),
        (
            "a stringValue with no hint into a character column",
            "VARCHAR(16)",
            json!({"stringValue": "plain"}),
            None,
            "text",
            true,
        ),
        (
            "a JSON-hinted document into the other document type",
            "JSON",
            json!({"stringValue": r#"{"a":1}"#}),
            Some("JSON"),
            "jsonb",
            true,
        ),
        (
            "a DATE-hinted date into a timestamp column",
            "TIMESTAMP",
            json!({"stringValue": "2026-10-22"}),
            Some("DATE"),
            "date",
            true,
        ),
        (
            "a DECIMAL-hinted number into an integer column",
            "INTEGER",
            json!({"stringValue": "7"}),
            Some("DECIMAL"),
            "numeric",
            true,
        ),
        (
            "a UUID-hinted uuid into a UUID column",
            "UUID",
            json!({"stringValue": "123e4567-e89b-12d3-a456-426614174000"}),
            Some("UUID"),
            "uuid",
            true,
        ),
        (
            "a TIME-hinted time of day into a time-of-day column",
            "TIME",
            json!({"stringValue": "18:30:00.500"}),
            Some("TIME"),
            "time without time zone",
            true,
        ),
        // A NULL is a value of every type, so no column refuses it...
        (
            "a NULL into an integer column",
            "INTEGER",
            json!({"isNull": true}),
            None,
            "unknown",
            true,
        ),
        // ...and a type the catalog does not name types nothing, so it refuses nothing.
        (
            "a longValue into a column of a type the catalog does not name",
            "CITEXT",
            json!({"longValue": 7}),
            None,
            "bigint",
            true,
        ),
    ];

    for (index, (case, declared, value, hint, arrives_as, takes_it)) in
        cases.into_iter().enumerate()
    {
        let table = format!("assigned_{index}");
        api.run(json!({
            "sql": format!("CREATE TABLE {table} (id INT PRIMARY KEY, value {declared})"),
        }))
        .await;
        let statement = format!("INSERT INTO {table} (id, value) VALUES (1, :value)");
        // Where the value is written, which is the `Position` the database reports an error
        // about it at: the characters before the placeholder, and then the one it starts at.
        let position = statement.find(":value").expect("the statement binds it") + 1;
        let mut parameter = parameter("value", value);
        if let Some(hint) = hint {
            parameter["typeHint"] = json!(hint);
        }
        let (status, body) = api
            .execute(json!({"sql": statement, "parameters": [parameter]}))
            .await;

        if takes_it {
            assert_eq!(
                (status, number(&body, "numberOfRecordsUpdated")),
                (200, 1),
                "case {case:?}: the column takes a value of {arrives_as}, got {status} {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body), body["message"].clone()),
                (
                    400,
                    "DatabaseErrorException",
                    json!(format!(
                        "ERROR: column \"value\" is of type {} but expression is of type \
                         {arrives_as}; Hint: You will need to rewrite or cast the \
                         expression.; Position: {position}; SQLState: 42804",
                        worded(declared)
                    ))
                ),
                "case {case:?}: expected the database's datatype-mismatch diagnostic, got \
                 {status} {body}"
            );
        }

        // What the database holds either way: the row for a value the column took, and
        // nothing at all for one it refused.
        let stored = api
            .run(json!({"sql": format!("SELECT id FROM {table}")}))
            .await;
        assert_eq!(
            list(&stored, "records").len(),
            usize::from(takes_it),
            "case {case:?}: expected the row to {}, got {stored}",
            if takes_it { "land" } else { "be absent" }
        );
    }

    // The same rule serves `BatchExecuteStatement`: every set of it is judged the same way,
    // and one set the column refuses refuses the whole batch.
    api.run(json!({
        "sql": "CREATE TABLE batched (id INT PRIMARY KEY, value INTEGER)",
    }))
    .await;
    let insert = "INSERT INTO batched (id, value) VALUES (:id, :value)";
    let (status, body) = api
        .batch(json!({
            "sql": insert,
            "parameterSets": [
                [parameter("id", json!({"longValue": 1})), parameter("value", json!({"longValue": 7}))],
                [
                    parameter("id", json!({"longValue": 2})),
                    json!({
                        "name": "value",
                        "value": {"stringValue": r#"{"a":1}"#},
                        "typeHint": "JSON",
                    }),
                ],
            ],
        }))
        .await;
    assert_eq!(
        (status, error_code(&body)),
        (400, "DatabaseErrorException"),
        "case \"a batch one set of which the column refuses\": got {status} {body}"
    );
    let stored = api
        .run(json!({"sql": "SELECT id FROM batched"}))
        .await;
    assert!(
        list(&stored, "records").is_empty(),
        "case \"a batch one set of which the column refuses\": nothing of it may land, got \
         {stored}"
    );
    // And the batch whose every set the column takes lands every row.
    let (status, body) = api
        .batch(json!({
            "sql": insert,
            "parameterSets": [
                [parameter("id", json!({"longValue": 1})), parameter("value", json!({"longValue": 7}))],
                [
                    parameter("id", json!({"longValue": 2})),
                    json!({"name": "value", "value": {"stringValue": "8"}, "typeHint": "DECIMAL"}),
                ],
            ],
        }))
        .await;
    assert_eq!(
        status, 200,
        "case \"a batch every set of which the column takes\": got {status} {body}"
    );
    let stored = api
        .run(json!({"sql": "SELECT id FROM batched ORDER BY id"}))
        .await;
    assert_eq!(
        stored["records"],
        json!([[{"longValue": 1}], [{"longValue": 2}]]),
        "case \"a batch every set of which the column takes\": expected both rows, got {stored}"
    );

    // Every hint the model declares names the type its own documentation says, which is
    // what the diagnostic above reports the expression as: one refusal per hint, into a
    // column of a family none of them belongs to.
    for (hint, named) in HINTED_TYPES {
        let table = format!("hint_{}", hint.to_ascii_lowercase());
        api.run(json!({
            "sql": format!("CREATE TABLE {table} (value BYTEA)"),
        }))
        .await;
        let statement = format!("INSERT INTO {table} (value) VALUES (:value)");
        let position = statement.find(":value").expect("the statement binds it") + 1;
        // A string every one of the six reads as a value of its own type is not one string,
        // so each hint is given the form its documentation names; what is asserted is the
        // TYPE the diagnostic reports, not the value.
        let sent = match *hint {
            "DATE" => "2026-10-22",
            "DECIMAL" => "12.5",
            "JSON" => r#"{"a":1}"#,
            "TIME" => "18:30:00",
            "TIMESTAMP" => "2026-10-22 18:30:00",
            _ => "123e4567-e89b-12d3-a456-426614174000",
        };
        let (status, body) = api
            .execute(json!({
                "sql": statement,
                "parameters": [
                    json!({"name": "value", "value": {"stringValue": sent}, "typeHint": hint}),
                ],
            }))
            .await;
        assert_eq!(
            (status, error_code(&body), body["message"].clone()),
            (
                400,
                "DatabaseErrorException",
                json!(format!(
                    "ERROR: column \"value\" is of type bytea but expression is of type \
                     {named}; Hint: You will need to rewrite or cast the expression.; \
                     Position: {position}; SQLState: 42804"
                ))
            ),
            "case {hint} sent into a binary column: expected the expression type {named}, \
             got {status} {body}"
        );
    }
}

/// The name the DATABASE words a declared type by in a diagnostic, which is the SQL name
/// and not the catalog's own — as the tables above declare them.
fn worded(declared: &str) -> &'static str {
    match declared {
        "INTEGER" => "integer",
        "VARCHAR(16)" => "character varying",
        "BOOLEAN" => "boolean",
        "UUID" => "uuid",
        "JSONB" => "jsonb",
        "JSON" => "json",
        "TIME" => "time without time zone",
        "TIMESTAMP" => "timestamp without time zone",
        "BYTEA" => "bytea",
        "NUMERIC(12,4)" => "numeric",
        "DOUBLE PRECISION" => "double precision",
        other => panic!("the test declares no column of type {other}"),
    }
}
