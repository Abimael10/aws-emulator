// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for three properties of the PostgreSQL endpoint a DB instance publishes,
//! each driven through a real client session on that endpoint.
//!
//! The rules under test:
//!
//! * A text value a statement COMPUTES — a concatenation, an aggregate over a table's rows, a
//!   digest over either — is served whole up to what this endpoint can hand a session back,
//!   and nothing of the store the engine keeps its pages in is visible in the answer: a value
//!   past that ceiling is refused under PostgreSQL's own condition for a limit of the
//!   implementation, in a sentence that names this endpoint's limit.
//! * A bulk load that stops on a datum its column cannot read is refused with PostgreSQL's
//!   condition and sentence for that datum AND with the `CONTEXT` that says where the load
//!   stopped — the table, the line of the copy data and the column — and leaves no row
//!   behind.
//! * A role the service itself owns cannot be altered or dropped by a session whose user is
//!   not a superuser, which the master user is not: such a statement is refused with
//!   PostgreSQL's insufficient-privilege condition and the role is left as it was, while the
//!   same statements on a role of the instance's own are served.
//!
//! Each table below spans what its rule generalises over — the size of the computed value,
//! the column type and the line a load stops on, and the role and the statement — rather than
//! one recorded statement.

use crate::db_instance_engine_endpoint::{
    available_instance, master_session, must_run, PostgresSession,
};
use crate::db_instances::emulator;

/// The rows one case of [`a_computed_text_value_is_served_up_to_what_this_endpoint_hands_back`]
/// generates, and how wide each row's text is.
///
/// The widths span the ceiling the engine's backing store holds ONE value under: an aggregate
/// of a few kilobytes, one past a megabyte, and one of several megabytes. All three are
/// ordinary work on a PostgreSQL server, whose own limit on a text value is about 1 GB.
const AGGREGATE_CASES: [(&str, usize, usize); 3] = [
    ("an aggregate of a few kilobytes", 64, 100),
    ("an aggregate past one megabyte", 1_400, 900),
    ("an aggregate of several megabytes", 2_500, 1_000),
];

/// The table one case loads into, and the rows it generates.
///
/// The statement generates the rows inside the engine — one row per number of a recursive
/// series — so the test sends no megabytes of its own and the rows are the engine's.
async fn generated_rows(
    case: &str,
    session: &mut PostgresSession,
    table: &str,
    rows: usize,
    width: usize,
) -> String {
    let label = "x".repeat(width);
    must_run(
        case,
        session,
        &format!("CREATE TABLE {table} (id int PRIMARY KEY, label text NOT NULL)"),
    )
    .await;
    must_run(
        case,
        session,
        &format!(
            "INSERT INTO {table} (id, label) WITH RECURSIVE series(n) AS \
             (SELECT 1 UNION ALL SELECT n + 1 FROM series WHERE n < {rows}) \
             SELECT n, '{label}' FROM series"
        ),
    )
    .await;
    // What the aggregate below must come to: every row's id and label, in order of id,
    // separated by the aggregate's own separator.
    (1..=rows)
        .map(|id| format!("{id}{label}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The statement one case reads its aggregate with: `function` over the whole table
/// concatenated, in order of id, into ONE text value.
fn over_the_aggregate(function: &str, table: &str) -> String {
    format!(
        "SELECT {function}(string_agg(id::text || label, ',' ORDER BY id)) FROM {table}"
    )
}

#[tokio::test]
async fn a_computed_text_value_is_served_up_to_what_this_endpoint_hands_back() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-bigvalue-pg", "postgres", &[]).await;
    let mut session = master_session("a computed text value", &address, port).await;

    for (case, rows, width) in AGGREGATE_CASES {
        let table = format!("bulk_{rows}_{width}");
        let expected = generated_rows(case, &mut session, &table, rows, width).await;

        // The whole value: its length is the length of the text the rows really hold.
        let measured = session
            .query(&over_the_aggregate("length", &table))
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the aggregate must be served, got {refusal:?}")
            });
        assert_eq!(
            measured.only(),
            Some(expected.chars().count().to_string().as_str()),
            "case {case:?}: expected an aggregate of {} characters, got {measured:?}",
            expected.chars().count()
        );

        // And the digest over it is the digest of those same bytes, which is what a client
        // verifying what it loaded compares against its own.
        let digest = session
            .query(&over_the_aggregate("md5", &table))
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the digest must be served, got {refusal:?}")
            });
        let computed = ::hex::encode(<::md5::Md5 as ::md5::Digest>::digest(expected.as_bytes()));
        assert_eq!(
            digest.only(),
            Some(computed.as_str()),
            "case {case:?}: expected the client's own digest {computed:?}, got {digest:?}"
        );
    }
}

#[tokio::test]
async fn a_computed_value_past_what_this_endpoint_serves_is_refused_as_a_program_limit() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-hugevalue-pg", "postgres", &[]).await;
    let case = "an aggregate past this endpoint's own ceiling";
    let mut session = master_session(case, &address, port).await;
    // Past the 8 MiB one value is served under, so the aggregate cannot be answered at all.
    let table = "past_the_ceiling";
    let _ = generated_rows(case, &mut session, table, 11_000, 800).await;

    let refusal = session
        .query(&over_the_aggregate("length", table))
        .await
        .expect_err(&format!("case {case:?}: the aggregate must be refused"));
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        (
            "54000",
            "the value is larger than this emulator serves (8 MiB)"
        ),
        "case {case:?}: expected this endpoint's own limit under PostgreSQL's \
         program-limit condition, got {refusal:?}"
    );
    // The session is still usable afterwards, and nothing of the store the engine keeps its
    // pages in is named in what the client was told.
    let still_there = session
        .query(&format!("SELECT count(*) FROM {table}"))
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the session must stay usable, got {refusal:?}"));
    assert_eq!(
        still_there.only(),
        Some("11000"),
        "case {case:?}: the rows must still be there, got {still_there:?}"
    );
}

#[tokio::test]
async fn a_session_setting_is_put_back_by_the_reset_that_undoes_the_set() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-reset-pg", "postgres", &[]).await;
    let case = "a session setting";
    let mut session = master_session(case, &address, port).await;

    /// The settings the cases drive, each a dynamic parameter a session sets for itself.
    const SETTINGS: [(&str, &str); 3] = [
        ("work_mem", "64MB"),
        ("statement_timeout", "5000"),
        ("log_statement", "all"),
    ];

    // What the SERVER runs with for each of them, which is what a reset puts back.
    let mut served = Vec::new();
    for (name, _) in SETTINGS {
        let read = session
            .query(&format!("SHOW {name}"))
            .await
            .unwrap_or_else(|refusal| panic!("case {name:?}: the server's value must be readable, got {refusal:?}"));
        served.push(
            read.only()
                .unwrap_or_else(|| panic!("case {name:?}: the server must have a value for it"))
                .to_string(),
        );
    }

    // One setting at a time: the session's own value is in force until the reset takes it
    // away, and then the server's is read again.
    for (at, (name, value)) in SETTINGS.into_iter().enumerate() {
        must_run(name, &mut session, &format!("SET {name} = '{value}'")).await;
        let set = session
            .query(&format!("SHOW {name}"))
            .await
            .expect("the session's own value must be readable");
        assert_eq!(
            set.only(),
            Some(value),
            "case {name:?}: the session's own value must be in force, got {set:?}"
        );
        must_run(name, &mut session, &format!("RESET {name}")).await;
        let reset = session
            .query(&format!("SHOW {name}"))
            .await
            .expect("the server's value must be readable again");
        assert_eq!(
            reset.only(),
            Some(served[at].as_str()),
            "case {name:?}: the reset must put the server's own value {:?} back, got {reset:?}",
            served[at]
        );
    }

    // And `RESET ALL` puts every one of them back at once.
    for (name, value) in SETTINGS {
        must_run(name, &mut session, &format!("SET {name} = '{value}'")).await;
    }
    must_run(case, &mut session, "RESET ALL").await;
    for (at, (name, _)) in SETTINGS.into_iter().enumerate() {
        let reset = session
            .query(&format!("SHOW {name}"))
            .await
            .expect("the server's value must be readable again");
        assert_eq!(
            reset.only(),
            Some(served[at].as_str()),
            "case {name:?}: RESET ALL must put the server's own value {:?} back, got {reset:?}",
            served[at]
        );
    }

    // `RESET ROLE` names no parameter of the configuration and is served as the `SET` forms
    // that name none are.
    must_run(case, &mut session, "RESET ROLE").await;

    // A name that is no setting of this server is refused by `RESET` exactly as `SET` refuses
    // it: the same condition, about the same name.
    let unknown = "emu_not_a_parameter";
    let refused_set = session
        .query(&format!("SET {unknown} = '1'"))
        .await
        .expect_err("a parameter the server does not have must be refused");
    let refused_reset = session
        .query(&format!("RESET {unknown}"))
        .await
        .expect_err("a parameter the server does not have must be refused");
    assert_eq!(
        (refused_reset.code.as_str(), refused_reset.message.as_str()),
        (refused_set.code.as_str(), refused_set.message.as_str()),
        "case {unknown:?}: expected the refusal SET answers with ({refused_set:?}), got \
         {refused_reset:?}"
    );
    assert_eq!(
        refused_reset.message,
        format!("unrecognized configuration parameter \"{unknown}\""),
        "case {unknown:?}: expected PostgreSQL's own sentence, got {refused_reset:?}"
    );
}

/// The table every bulk load below streams into, with one column of each class whose text
/// form has to parse.
const COPY_TABLE: &str = "CREATE TABLE bulk \
     (id int PRIMARY KEY, label text NOT NULL, amount numeric(12,4) NOT NULL, flag boolean)";

#[tokio::test]
async fn a_loaded_exact_decimal_keeps_the_scale_its_column_declares() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-copyscale-pg", "postgres", &[]).await;
    let case = "a loaded exact decimal";
    let mut session = master_session(case, &address, port).await;
    must_run(case, &mut session, COPY_TABLE).await;
    // A column with NO declared scale keeps whatever scale each value has, which is the other
    // half of the rule.
    must_run(
        case,
        &mut session,
        "CREATE TABLE unscaled (id int PRIMARY KEY, amount numeric)",
    )
    .await;
    session
        .copy_in(
            "COPY bulk FROM STDIN WITH (FORMAT text)",
            // The rows span what the declaration does to a datum: a value with the declared
            // digits, one with fewer, one whose last digits are zeros, and one with more —
            // which is rounded to the scale, as "the value will be rounded to the specified
            // number of fractional digits" says.
            "1\tone\t12.3400\tt\n2\ttwo\t5\tt\n3\tthree\t0.0001\tt\n4\tfour\t7.123456\tt\n",
        )
        .await
        .expect("the load must be accepted");
    session
        .copy_in(
            "COPY unscaled FROM STDIN WITH (FORMAT text)",
            "1\t12.34\n2\t5\n",
        )
        .await
        .expect("the load must be accepted");
    let read = session
        .query("SELECT amount, amount::text, id::text || amount::text FROM bulk ORDER BY id")
        .await
        .expect("the rows must be readable");
    assert_eq!(
        read.rows,
        vec![
            vec![
                Some("12.3400".to_string()),
                Some("12.3400".to_string()),
                Some("112.3400".to_string())
            ],
            vec![
                Some("5.0000".to_string()),
                Some("5.0000".to_string()),
                Some("25.0000".to_string())
            ],
            vec![
                Some("0.0001".to_string()),
                Some("0.0001".to_string()),
                Some("30.0001".to_string())
            ],
            vec![
                Some("7.1235".to_string()),
                Some("7.1235".to_string()),
                Some("47.1235".to_string())
            ],
        ],
        "a value of a column declared numeric(12,4) must keep that scale, got {read:?}"
    );
    // The column that declares no scale: each value keeps the digits it has.
    let unscaled = session
        .query("SELECT amount, amount::text FROM unscaled ORDER BY id")
        .await
        .expect("the rows must be readable");
    assert_eq!(
        unscaled.rows,
        vec![
            vec![Some("12.34".to_string()), Some("12.34".to_string())],
            vec![Some("5".to_string()), Some("5".to_string())],
        ],
        "a value of a column declared numeric with no scale must keep its own digits, got \
         {unscaled:?}"
    );

    // An AGGREGATE over such a column is exact too, and is reported as the exact-decimal type
    // it answers: the total, the least and the greatest carry the column's own scale, and the
    // average carries the digits the quotient has. The expected values are the digits of the
    // four loaded rows (12.3400, 5.0000, 0.0001, 7.1235).
    for (case, sql, expected) in [
        ("the total", "SELECT sum(amount) FROM bulk", "24.4636"),
        ("the least", "SELECT min(amount) FROM bulk", "0.0001"),
        ("the greatest", "SELECT max(amount) FROM bulk", "12.3400"),
        ("the average", "SELECT avg(amount) FROM bulk", "6.1159"),
    ] {
        let read = session
            .query(sql)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"));
        assert_eq!(
            (read.only(), read.type_oids.first().copied()),
            (Some(expected), Some(NUMERIC_TYPE_OID)),
            "case {case:?}: expected the exact decimal {expected:?} under the numeric type, \
             got {read:?}"
        );
    }
    // And an aggregate over NO rows is NULL, as both engines answer.
    let empty = session
        .query("SELECT sum(amount) FROM bulk WHERE id < 0")
        .await
        .expect("the aggregate must run");
    assert_eq!(
        empty.rows,
        vec![vec![None]],
        "an aggregate over no rows must be NULL, got {empty:?}"
    );
}

#[tokio::test]
async fn the_storage_a_relation_occupies_is_reported_from_the_rows_it_holds() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-relsize-pg", "postgres", &[]).await;
    let case = "the storage a relation occupies";
    let mut session = master_session(case, &address, port).await;
    must_run(
        case,
        &mut session,
        "CREATE TABLE stored (id int PRIMARY KEY, body text NOT NULL)",
    )
    .await;

    /// The unit PostgreSQL reports a relation's size in: its own page size.
    const PAGE_BYTES: i64 = 8192;

    // Each of the three functions, over a relation holding NO rows: a relation with no rows
    // fills no page, which is the zero PostgreSQL reports for an empty table.
    for function in [
        "pg_relation_size",
        "pg_table_size",
        "pg_total_relation_size",
    ] {
        let read = session
            .query(&format!("SELECT {function}('stored')"))
            .await
            .unwrap_or_else(|refusal| panic!("case {function:?}: must be served, got {refusal:?}"));
        assert_eq!(
            (read.only(), read.columns.first().map(String::as_str)),
            (Some("0"), Some(function)),
            "case {function:?}: an empty relation must occupy no bytes, got {read:?}"
        );
    }

    // The rows a load puts in it are storage it occupies, and more rows are more of it: the
    // size is read from the engine's own rows, so it GROWS with them and is a whole number of
    // pages.
    let mut sizes = Vec::new();
    for rows in [1_usize, 400, 4_000] {
        must_run(
            case,
            &mut session,
            &format!(
                "INSERT INTO stored (id, body) WITH RECURSIVE series(n) AS \
                 (SELECT 1 UNION ALL SELECT n + 1 FROM series WHERE n < {rows}) \
                 SELECT n + (SELECT IFNULL(MAX(id), 0) FROM stored), '{}' FROM series",
                "b".repeat(100)
            ),
        )
        .await;
        let read = session
            .query("SELECT pg_relation_size('stored')")
            .await
            .expect("the size must be served");
        let size: i64 = read
            .only()
            .unwrap_or_default()
            .parse()
            .unwrap_or_else(|_| panic!("case {rows:?}: the size must be a number, got {read:?}"));
        assert!(
            size > 0 && size % PAGE_BYTES == 0,
            "case {rows:?}: expected a whole number of {PAGE_BYTES}-byte pages above zero, \
             got {size}"
        );
        sizes.push(size);
    }
    assert!(
        sizes.windows(2).all(|pair| pair[1] >= pair[0])
            && sizes.last() > sizes.first(),
        "the storage must grow with the rows the relation holds, got {sizes:?}"
    );

    // A name the database holds no relation of has no storage at all, which is the NULL
    // PostgreSQL answers for one it does not have.
    let absent = session
        .query("SELECT pg_relation_size('emu_absent')")
        .await
        .expect("the size of an absent relation must be served");
    assert_eq!(
        absent.rows,
        vec![vec![None]],
        "a relation the database does not have must report no size, got {absent:?}"
    );
}

#[tokio::test]
async fn a_copy_to_stdout_streams_back_the_bytes_a_load_streamed_in() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-copyout-pg", "postgres", &[]).await;
    let case = "a copy to stdout";
    let mut session = master_session(case, &address, port).await;
    must_run(case, &mut session, COPY_TABLE).await;
    // The rows span what the format has to carry back: ordinary values, a NULL, and a value
    // holding the separator, a newline and a backslash of its own.
    let streamed = "1\tplain\t1.5000\tt\n\
                    2\tno flag\t2.0000\t\\N\n\
                    3\ttab\\there\t3.2500\tt\n\
                    4\tline\\nbreak\t4.0000\tf\n\
                    5\tback\\\\slash\t5.0000\tt\n";
    session
        .copy_in("COPY bulk FROM STDIN WITH (FORMAT text)", streamed)
        .await
        .expect("the load must be accepted");

    // The whole table, in the order the rows are read back: what the client streamed in is
    // what the server streams out, byte for byte.
    let out = session
        .copy_out("COPY bulk TO STDOUT")
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the copy must be served, got {refusal:?}"));
    assert_eq!(
        out, streamed,
        "the rows streamed out must be the rows the load streamed in, got {out:?}"
    );

    // The column list and the QUERY form each stream what they name.
    for (named, statement, expected) in [
        (
            "a column list",
            "COPY bulk (id, amount) TO STDOUT",
            "1\t1.5000\n2\t2.0000\n3\t3.2500\n4\t4.0000\n5\t5.0000\n",
        ),
        (
            "a query",
            "COPY (SELECT id, label FROM bulk WHERE id < 3 ORDER BY id) TO STDOUT",
            "1\tplain\n2\tno flag\n",
        ),
    ] {
        let out = session
            .copy_out(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {named:?}: must be served, got {refusal:?}"));
        assert_eq!(
            out, expected,
            "case {named:?}: expected {expected:?}, got {out:?}"
        );
    }

    // The BINARY format: the stream opens with the signature and the two header words that
    // format carries, each row is its field count and each field its length and the bytes of
    // its own type's binary form, and the stream ends with a field count of -1.
    let binary = session
        .copy_out_bytes("COPY bulk (id, amount, flag) TO STDOUT WITH (FORMAT binary)")
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the binary copy must be served, got {refusal:?}"));
    let binary = binary.as_slice();
    assert!(
        binary.starts_with(b"PGCOPY\n\xff\r\n\0"),
        "the binary stream must open with PostgreSQL's own signature, got {:?}",
        &binary[..binary.len().min(16)]
    );
    assert_eq!(
        binary.get(11..19),
        Some([0u8, 0, 0, 0, 0, 0, 0, 0].as_slice()),
        "the flags and the header extension of a stream with neither must be zero, got {:?}",
        binary.get(11..19)
    );
    assert_eq!(
        binary.len().checked_sub(2).and_then(|at| binary.get(at..)),
        Some([0xffu8, 0xff].as_slice()),
        "the binary stream must end with a field count of -1"
    );
    // The first row's three fields, in the forms their types carry: `id` is a 4-byte integer,
    // `amount` the digits of an exact decimal (one base-10000 group of 1, a weight of 0, a
    // positive sign and the four fractional digits its column declares), and `flag` one byte.
    assert_eq!(
        binary.get(19..21),
        Some([0u8, 3].as_slice()),
        "the row must carry its three fields"
    );
    assert_eq!(
        binary.get(21..29),
        Some([0u8, 0, 0, 4, 0, 0, 0, 1].as_slice()),
        "the integer field must be its own four big-endian bytes, got {:?}",
        binary.get(21..29)
    );
    assert_eq!(
        binary.get(29..45),
        Some([0u8, 0, 0, 12, 0, 2, 0, 0, 0, 0, 0, 4, 0, 1, 19, 136].as_slice()),
        "the exact decimal must carry its digits, weight, sign and declared scale, got {:?}",
        binary.get(29..45)
    );
    assert_eq!(
        binary.get(45..50),
        Some([0u8, 0, 0, 1, 1].as_slice()),
        "the boolean field must be its own single byte, got {:?}",
        binary.get(45..50)
    );

    // The same stream LOADS: a binary copy in reads each value under its column's own type,
    // so a table loaded from what a binary copy out streamed holds the same rows.
    must_run(
        case,
        &mut session,
        "CREATE TABLE copied (id int PRIMARY KEY, amount numeric(12,4) NOT NULL, flag boolean)",
    )
    .await;
    session
        .copy_in_bytes(
            "COPY copied (id, amount, flag) FROM STDIN WITH (FORMAT binary)",
            binary,
        )
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the binary load must be accepted, got {refusal:?}"));
    let loaded = session
        .query("SELECT id, amount, flag FROM copied ORDER BY id")
        .await
        .expect("the loaded rows must be readable");
    let read_back = session
        .query("SELECT id, amount, flag FROM bulk ORDER BY id")
        .await
        .expect("the source rows must be readable");
    assert_eq!(
        loaded.rows, read_back.rows,
        "the rows a binary copy loaded must be the rows it streamed, got {loaded:?}"
    );

    // A copy out of a relation the database does not have is refused as the undefined
    // relation it is, and the session stays usable.
    let refusal = session
        .copy_out("COPY emu_absent TO STDOUT")
        .await
        .expect_err("a relation the database does not have must be refused");
    assert_eq!(
        refusal.message, "relation \"emu_absent\" does not exist",
        "expected PostgreSQL's own sentence for a relation that is not there, got {refusal:?}"
    );
    let after = session
        .query("SELECT count(*) FROM bulk")
        .await
        .expect("the session must stay usable");
    assert_eq!(
        after.only(),
        Some("5"),
        "the table must still be readable, got {after:?}"
    );
}

/// `numeric`'s own type code, which is what a client decodes an exact decimal under: the
/// object identifier PostgreSQL publishes for the type in its own catalog.
const NUMERIC_TYPE_OID: i32 = 1700;

/// One well-formed row of that table's copy data.
fn copy_row(id: usize) -> String {
    format!("{id}\tlabel-{id}\t{id}.5000\tt\n")
}

#[tokio::test]
async fn a_bulk_load_that_stops_on_a_datum_says_where_it_stopped() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-copyctx-pg", "postgres", &[]).await;
    let mut session = master_session("a bulk load", &address, port).await;
    must_run("a bulk load", &mut session, COPY_TABLE).await;

    // A well-formed load of many rows is served whole: the rows the client streamed are the
    // rows the table then holds.
    let loaded: String = (1..=2_000).map(copy_row).collect();
    session
        .copy_in("COPY bulk FROM STDIN WITH (FORMAT text)", &loaded)
        .await
        .expect("a well-formed bulk load must be accepted");
    let count = session
        .query("SELECT count(*) FROM bulk")
        .await
        .expect("the loaded rows must be readable");
    assert_eq!(
        count.only(),
        Some("2000"),
        "the load must hold every row the client streamed, got {count:?}"
    );

    // (case, the copy data, the line it stops on, the column, the datum, the type that
    // cannot read it). The rows span WHERE in the data the load stops — the first line, a
    // line in the middle, the last line — and WHICH column's type refuses the datum. Each
    // load carries ids of its own, so the datum is the only thing the table could refuse.
    let in_the_middle: String = (1..=120)
        .map(|at| match at {
            64 => "10064\tlabel-10064\tnot-a-number\tt\n".to_string(),
            other => copy_row(10_000 + other),
        })
        .collect();
    let at_the_end: String = (1..=50)
        .map(|at| match at {
            50 => "20050\tlabel-20050\t5.0000\tperhaps\n".to_string(),
            other => copy_row(20_000 + other),
        })
        .collect();
    for (case, data, line, column, datum, named) in [
        (
            "a malformed integer on the first line",
            "nine\tlabel\t1.0000\tt\n".to_string(),
            1,
            "id",
            "nine",
            "integer",
        ),
        (
            "a malformed numeric in the middle of the load",
            in_the_middle,
            64,
            "amount",
            "not-a-number",
            "numeric",
        ),
        (
            "a malformed boolean on the last line",
            at_the_end,
            50,
            "flag",
            "perhaps",
            "boolean",
        ),
    ] {
        let before = session
            .query("SELECT count(*) FROM bulk")
            .await
            .expect("the table must be readable");
        let refusal = session
            .copy_in("COPY bulk FROM STDIN WITH (FORMAT text)", &data)
            .await
            .expect_err(&format!("case {case:?}: the malformed datum must be refused"));
        assert_eq!(
            (
                refusal.code.as_str(),
                refusal.message.as_str(),
                session.last_refusal_context.as_deref(),
            ),
            (
                "22P02",
                format!("invalid input syntax for type {named}: \"{datum}\"").as_str(),
                Some(format!("COPY bulk, line {line}, column {column}: \"{datum}\"").as_str()),
            ),
            "case {case:?}: expected PostgreSQL's input-syntax refusal and the place the \
             load stopped, got {refusal:?} at {:?}",
            session.last_refusal_context
        );
        // "COPY stops operation at the first error": the rows before the bad one are rolled
        // back with it, so the table holds exactly what it held before the load.
        let after = session
            .query("SELECT count(*) FROM bulk")
            .await
            .expect("the table must be readable");
        assert_eq!(
            after.only(),
            before.only(),
            "case {case:?}: the refused load must leave no row behind, got {after:?}"
        );
    }

    // A record whose SHAPE does not fit the columns is the load's other condition, and says
    // the line it stopped on in the same field.
    for (case, data, line, message) in [
        (
            "a row with a column too many",
            "1\tlabel\t1.0000\tt\textra\n",
            1,
            "extra data after last expected column",
        ),
        (
            "a row with a column too few",
            "1\tlabel\n2\tlabel\t2.0000\tt\n",
            1,
            "missing data for column \"amount\"",
        ),
    ] {
        let refusal = session
            .copy_in("COPY bulk FROM STDIN WITH (FORMAT text)", data)
            .await
            .expect_err(&format!("case {case:?}: the malformed row must be refused"));
        assert_eq!(
            (
                refusal.code.as_str(),
                refusal.message.as_str(),
                session.last_refusal_context.as_deref(),
            ),
            (
                "22P04",
                message,
                Some(format!("COPY bulk, line {line}").as_str()),
            ),
            "case {case:?}: expected PostgreSQL's copy-format refusal and the line it \
             stopped on, got {refusal:?} at {:?}",
            session.last_refusal_context
        );
    }
}

#[tokio::test]
async fn a_malformed_binary_copy_stream_is_refused_and_loads_nothing() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-badbinary-pg", "postgres", &[]).await;
    let case = "a malformed binary copy";
    let mut session = master_session(case, &address, port).await;
    must_run(
        case,
        &mut session,
        "CREATE TABLE binloaded (id int PRIMARY KEY, label text NOT NULL)",
    )
    .await;

    /// The signature and the two header words a well-formed stream opens with.
    fn header() -> Vec<u8> {
        let mut out = b"PGCOPY\n\xff\r\n\0".to_vec();
        out.extend_from_slice(&0i32.to_be_bytes());
        out.extend_from_slice(&0i32.to_be_bytes());
        out
    }
    /// One field of `bytes`, with the 32-bit length the format prefixes it with.
    fn field(bytes: &[u8]) -> Vec<u8> {
        let mut out = i32::try_from(bytes.len()).unwrap_or_default().to_be_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    // A truncated stream: the header says it carries an extension longer than the stream.
    let mut overlong_extension = b"PGCOPY\n\xff\r\n\0".to_vec();
    overlong_extension.extend_from_slice(&0i32.to_be_bytes());
    overlong_extension.extend_from_slice(&0x7fff_ffffi32.to_be_bytes());
    // A field whose own length runs past the end of the stream.
    let mut field_past_the_end = header();
    field_past_the_end.extend_from_slice(&1i16.to_be_bytes());
    field_past_the_end.extend_from_slice(&64i32.to_be_bytes());
    field_past_the_end.extend_from_slice(b"short");
    // A row the stream ends after, with no trailer at all.
    let mut no_trailer = header();
    no_trailer.extend_from_slice(&2i16.to_be_bytes());
    no_trailer.extend(field(&1i32.to_be_bytes()));
    no_trailer.extend(field(b"one"));
    // A row carrying more fields than the table has columns, and one carrying fewer.
    let mut too_many = header();
    too_many.extend_from_slice(&3i16.to_be_bytes());
    too_many.extend(field(&1i32.to_be_bytes()));
    too_many.extend(field(b"one"));
    too_many.extend(field(b"extra"));
    too_many.extend_from_slice(&(-1i16).to_be_bytes());
    let mut too_few = header();
    too_few.extend_from_slice(&1i16.to_be_bytes());
    too_few.extend(field(&1i32.to_be_bytes()));
    too_few.extend_from_slice(&(-1i16).to_be_bytes());

    // (case, the stream, the SQLSTATE, the message). The rows span the ways a stream a client
    // controls can be malformed: not that format at all, a header that ends early, a header
    // that claims more than it carries, a field length past the end, no trailer, and a row
    // whose field count does not fit the table.
    for (named, stream, code, message) in [
        (
            "bytes that are no copy stream at all",
            b"not-a-pgcopy-stream-at-all".to_vec(),
            "22P04",
            "COPY file signature not recognized",
        ),
        (
            "a header that ends before its extension length",
            {
                let mut out = b"PGCOPY\n\xff\r\n\0".to_vec();
                out.extend_from_slice(&0i32.to_be_bytes());
                out
            },
            "22P04",
            "COPY file header is truncated",
        ),
        (
            "a header extension longer than the stream",
            overlong_extension,
            "22P04",
            "COPY file header is truncated",
        ),
        (
            "a field whose length runs past the end",
            field_past_the_end,
            "22P04",
            "COPY data is truncated",
        ),
        (
            "a stream that ends with no trailer",
            no_trailer,
            "22P04",
            "COPY data is truncated",
        ),
        (
            "a row with a field too many",
            too_many,
            "22P04",
            "extra data after last expected column",
        ),
        (
            "a row with a field too few",
            too_few,
            "22P04",
            "missing data for column \"label\"",
        ),
    ] {
        let refusal = session
            .copy_in_bytes(
                "COPY binloaded (id, label) FROM STDIN WITH (FORMAT binary)",
                &stream,
            )
            .await
            .expect_err(&format!("case {named:?}: the stream must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            (code, message),
            "case {named:?}: expected PostgreSQL's copy-format refusal, got {refusal:?}"
        );
    }

    // And a stream LARGER than this endpoint serves is refused for its size, before any of
    // its rows are read: the data is streamed in messages of the protocol's own size, so what
    // is bounded is the load and not one message.
    let mut oversized = header();
    oversized.resize(9 * 1024 * 1024, 0);
    let refusal = session
        .copy_in_bytes(
            "COPY binloaded (id, label) FROM STDIN WITH (FORMAT binary)",
            &oversized,
        )
        .await
        .expect_err("a load larger than this endpoint serves must be refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        (
            "54000",
            format!(
                "the copy data is larger than this endpoint serves ({} bytes)",
                8 * 1024 * 1024
            )
            .as_str()
        ),
        "expected this endpoint's own copy limit, got {refusal:?}"
    );

    // Not one of them left a row behind, and the session is still usable.
    let held = session
        .query("SELECT count(*) FROM binloaded")
        .await
        .expect("the table must be readable");
    assert_eq!(
        held.only(),
        Some("0"),
        "a refused load must leave the table empty, got {held:?}"
    );
}

/// The roles Amazon RDS owns on every PostgreSQL instance: the superuser account it keeps for
/// itself and the predefined roles it grants from.///
/// Source: the Amazon RDS User Guide's `rds_superuser` / PostgreSQL roles material, which is
/// the same list this server seeds its role catalog with.
const RDS_OWNED_ROLES: [&str; 5] = [
    "rdsadmin",
    "rds_superuser",
    "rds_replication",
    "rds_iam",
    "rds_password",
];

#[tokio::test]
async fn a_role_the_service_owns_cannot_be_altered_or_dropped_by_the_master_user() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-ownedrole-pg", "postgres", &[]).await;
    let mut session = master_session("an RDS-owned role", &address, port).await;

    // Every RDS-owned role, under both statements that would re-shape or remove it — and the
    // conditional form of the drop, which does not lift the gate either.
    for role in RDS_OWNED_ROLES {
        for (statement, acted) in [
            (format!("ALTER ROLE {role} NOLOGIN"), "alter"),
            (format!("ALTER ROLE {role} LOGIN"), "alter"),
            (format!("DROP ROLE {role}"), "drop"),
            (format!("DROP ROLE IF EXISTS {role}"), "drop"),
        ] {
            let refusal = session
                .query(&statement)
                .await
                .expect_err(&format!("case {statement:?}: the statement must be refused"));
            assert_eq!(
                (refusal.code.as_str(), refusal.message.as_str()),
                ("42501", format!("cannot {acted} \"{role}\"").as_str()),
                "case {statement:?}: expected PostgreSQL's privilege refusal naming the \
                 role, got {refusal:?}"
            );
        }
    }

    // And the roles are exactly as they were: the superuser account can still log in, every
    // owned role is still there, and the master user still holds the group it is a member of.
    let unchanged = session
        .query(
            "SELECT rolname, rolcanlogin FROM pg_roles WHERE rolname IN \
             ('rdsadmin', 'rds_superuser', 'rds_replication', 'rds_iam', 'rds_password') \
             ORDER BY rolname",
        )
        .await
        .expect("the role catalog must be readable");
    assert_eq!(
        unchanged.rows,
        vec![
            vec![Some("rds_iam".to_string()), Some("f".to_string())],
            vec![Some("rds_password".to_string()), Some("f".to_string())],
            vec![Some("rds_replication".to_string()), Some("f".to_string())],
            vec![Some("rds_superuser".to_string()), Some("f".to_string())],
            vec![Some("rdsadmin".to_string()), Some("t".to_string())],
        ],
        "the refused statements must leave every owned role exactly as it was, got \
         {unchanged:?}"
    );
    let member = session
        .query("SELECT pg_has_role(current_user, 'rds_superuser', 'MEMBER')")
        .await
        .expect("the membership must be readable");
    assert_eq!(
        member.only(),
        Some("t"),
        "the master user must still be a member of the group RDS puts it in, got {member:?}"
    );

    // The positive controls: a role of the INSTANCE's own goes through every one of those
    // statements, and a session setting is the session's to set.
    for statement in [
        "CREATE ROLE emu_app LOGIN",
        "ALTER ROLE emu_app NOLOGIN",
        "GRANT rds_iam TO emu_app",
        "DROP ROLE emu_app",
        "SET work_mem = '64MB'",
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {statement:?}: must be served, got {refusal:?}"));
    }
}

#[tokio::test]
async fn a_function_that_reaches_the_host_resolves_and_is_refused_as_a_privilege() {
    let router = emulator();
    let (address, port) = available_instance(&router, "emu-hostfn-pg", "postgres", &[]).await;
    let mut session = master_session("a server-file function", &address, port).await;

    // Every function whose EXECUTE privilege belongs to one of the predefined roles the RDS
    // master user is not a member of: the function RESOLVES — it is in the catalog of every
    // PostgreSQL server — and the caller is refused the privilege for it, which is a
    // different condition from a statement the server cannot parse.
    for (case, statement, function) in [
        (
            "reading a server file",
            "SELECT pg_read_file('/etc/hostname')",
            "pg_read_file",
        ),
        (
            "reading one as bytes",
            "SELECT pg_read_binary_file('/etc/hostname')",
            "pg_read_binary_file",
        ),
        ("listing a directory", "SELECT pg_ls_dir('/tmp')", "pg_ls_dir"),
        (
            "stat-ing a file",
            "SELECT pg_stat_file('/etc/hostname')",
            "pg_stat_file",
        ),
        (
            "importing a large object from one",
            "SELECT lo_import('/etc/hostname')",
            "lo_import",
        ),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            (
                "42501",
                format!("permission denied for function {function}").as_str()
            ),
            "case {case:?}: expected PostgreSQL's privilege refusal, got {refusal:?}"
        );
    }

    // Which is what the role catalog says too: the master user is a member of the group RDS
    // puts it in and of none of the roles that hold those privileges.
    for (case, role, expected) in [
        ("the group RDS puts the master user in", "rds_superuser", "t"),
        ("reading files on the server", "pg_read_server_files", "f"),
        ("writing them", "pg_write_server_files", "f"),
        ("running a program on it", "pg_execute_server_program", "f"),
    ] {
        let held = session
            .query(&format!(
                "SELECT pg_has_role(current_user, '{role}', 'MEMBER')"
            ))
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the membership must be readable, got {refusal:?}")
            });
        assert_eq!(
            held.only(),
            Some(expected),
            "case {case:?}: expected the membership in {role:?} to be {expected:?}, got \
             {held:?}"
        );
    }

    // And the session is still usable: a refused privilege ends the statement, not the
    // connection.
    let who = session
        .query("SELECT current_user, current_database()")
        .await
        .expect("the session must stay usable");
    assert_eq!(
        who.rows.len(),
        1,
        "the session must still answer its own identity, got {who:?}"
    );
}

#[tokio::test]
async fn the_engine_reports_the_port_the_instance_was_configured_with() {
    let router = emulator();
    // (case, the `Port` the create asks for, the port the engine must then report). The rows
    // span the member's own published range: "Valid Values: 1150-65535. Default: ... RDS for
    // PostgreSQL - 5432" (`CreateDBInstanceMessage$Port`).
    for (case, requested, expected) in [
        ("no port at all, so the engine default decides", None, "5432"),
        ("the lowest port the member admits", Some(1_150), "1150"),
        ("a port of the caller's own", Some(5_439), "5439"),
        ("the highest port the member admits", Some(65_535), "65535"),
    ] {
        let identifier = format!("emu-port-{expected}-pg");
        let extra: Vec<(&str, String)> = requested
            .map(|port: u16| vec![("Port", port.to_string())])
            .unwrap_or_default();
        let (address, port) =
            available_instance(&router, &identifier, "postgres", &extra).await;
        let mut session = master_session(case, &address, port).await;
        for statement in [
            "SELECT current_setting('port')",
            "SELECT setting FROM pg_settings WHERE name = 'port'",
        ] {
            let read = session
                .query(statement)
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case:?}: {statement:?} must be served, got {refusal:?}")
                });
            assert_eq!(
                read.only(),
                Some(expected),
                "case {case:?}: expected the engine to report the configured port \
                 {expected:?}, got {read:?}"
            );
        }
    }
}
