// SPDX-License-Identifier: CC0-1.0

//! The continuation tokens Glue's paginated listings hand out, and which ones they honour.
//!
//! RULE: a listing's `NextToken` is an OPAQUE value the service itself minted for that
//! request's scope — it carries no stored name — and it is honoured only when this store
//! minted it for THIS listing of THIS resource; any other value names no position and is
//! refused with the operation's own invalid-token error. A page holds at most `MaxResults`
//! entries and carries a `NextToken` exactly while entries remain, and a `MaxResults`
//! larger than the biggest page the service admits is refused at the request boundary.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: how many records are stored, the
//! page size asked for, the filter that narrows the selection, which scope a token was
//! minted for, and how a value that was not minted is shaped.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    decode_page_token, encode_page_token, listing_scope, service_state, PageTokenKey,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    call, call_in_region, emulator_on, error_code, error_message, ok, table_names, ACCOUNT_ID,
    REGION,
};

/// Another Region's endpoint, for the job listing: a job exists only in the Region it was
/// created in, so a token minted while listing one Region's jobs names no position in
/// another's.
const OTHER_REGION: &str = "us-west-2";

/// A role ARN `CreateJob` accepts.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/glue-pagination-role";

/// The refusal a Data Catalog getter answers a token it did not issue with.
const CATALOG_TOKEN_REFUSAL: &str = "Invalid token value";

/// The refusal the job listing answers one with — Glue words it per operation.
const JOB_TOKEN_REFUSAL: &str = "Invalid pagination token.";

/// The largest page `GetTables` serves; past it the request is refused at the boundary.
const MAX_TABLE_PAGE: i64 = 1_000;

/// The smallest page it serves: the minimum of the member's modelled range.
const MIN_TABLE_PAGE: i64 = 1;

/// The refusal a page size that names no servable page earns.
const PAGE_SIZE_REFUSAL: &str = "MaxItems has to be a positive number";

/// An emulator and the engine it was mounted on, so a suite can read the store's own
/// page-token key — the secret a listing authenticates its cursors under.
fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    (router, engine)
}

/// The secret this store's listings mint their tokens under.
fn store_key(engine: &engine::InMemoryEngine) -> PageTokenKey {
    service_state(engine)
        .expect("the Glue store is reachable")
        .lock()
        .page_token_key
}

/// The scope `GetTables` binds the tokens of one database's listing to.
fn table_scope(database: &str) -> String {
    listing_scope("GetTables", &[ACCOUNT_ID, database])
}

/// The scope `ListJobs` binds the tokens of one Region's listing to.
fn job_scope(region: &str) -> String {
    listing_scope("ListJobs", &[region])
}

/// Create `name` as a database.
async fn create_database(router: &MultiServiceRouter, name: &str) {
    ok(router, "CreateDatabase", &json!({ "DatabaseInput": { "Name": name } })).await;
}

/// Create `table` in `database`.
async fn create_table(router: &MultiServiceRouter, database: &str, table: &str) {
    ok(
        router,
        "CreateTable",
        &json!({ "DatabaseName": database, "TableInput": { "Name": table } }),
    )
    .await;
}

/// Create one job in `region`.
async fn create_job(router: &MultiServiceRouter, region: &str, name: &str) {
    let body = json!({
        "Name": name,
        "Role": ROLE_ARN,
        "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl-scripts/etl.py" },
    });
    let (status, answer) = call_in_region(router, region, "CreateJob", &body).await;
    assert_eq!(
        status, 200,
        "CreateJob {name:?} in {region} must answer 200, got {status} with {answer}"
    );
}

/// Assert that `token` discloses none of `records`: a cursor spelled as a stored name is
/// exactly what an opaque token is not.
fn assert_discloses_nothing(case: &str, token: &str, records: &[&str]) {
    for record in records {
        assert!(
            !token.contains(record),
            "case {case}: a continuation token must disclose no stored name, but {token:?} \
             carries {record:?}"
        );
    }
    assert!(
        token.len() >= 2 * service_common::page_tokens::AUTHENTICATOR_BYTES,
        "case {case}: a token carries an authenticator over its cursor, got {token:?}"
    );
}

/// One row of the page-walk table below: a store, how it is paged, and what the walk must
/// report. A named structure rather than a tuple, because a row carries five fields.
struct PageWalk {
    /// What the row is about, named in every assertion.
    case: &'static str,
    /// The tables the database holds, created in this order.
    stored: &'static [&'static str],
    /// The `MaxResults` the walk asks for, or none at all.
    page_size: Option<i64>,
    /// The `Expression` that narrows the selection, or none at all.
    expression: Option<&'static str>,
    /// The names the walk must report, in order.
    expected: &'static [&'static str],
}

/// `GetTables` pages the tables of one database: `MaxResults` bounds the tables ONE
/// response examines, in the catalog's own name order, and the page holds those of them the
/// request selects — so a filtered listing serves short pages, an empty one included, and
/// goes on being continued while stored tables are left. Every token it hands out is an
/// opaque cursor this store minted for this database's listing, never a table name.
///
/// The rows differ along what the rule generalises over: how many tables are stored, the
/// page size asked for (one, a size that divides the store, one that does not, one past it,
/// none at all), and whether an `Expression` narrows the selection — including a filter that
/// selects nothing at all.
#[tokio::test]
async fn get_tables_pages_its_selection_behind_opaque_tokens() {
    let rows: &[PageWalk] = &[
        PageWalk {
            case: "one table, one per page",
            stored: &["cloudfront_access_logs"],
            page_size: Some(1),
            expression: None,
            expected: &["cloudfront_access_logs"],
        },
        PageWalk {
            case: "a page size that divides the store",
            stored: &[
                "elb_logs",
                "cloudfront_access_logs",
                "vpc_flow_logs",
                "waf_logs",
            ],
            page_size: Some(2),
            expression: None,
            expected: &[
                "cloudfront_access_logs",
                "elb_logs",
                "vpc_flow_logs",
                "waf_logs",
            ],
        },
        PageWalk {
            case: "a page size that does not divide the store",
            stored: &["elb_logs", "cloudfront_access_logs", "vpc_flow_logs"],
            page_size: Some(2),
            expression: None,
            expected: &["cloudfront_access_logs", "elb_logs", "vpc_flow_logs"],
        },
        PageWalk {
            case: "a page larger than the store",
            stored: &["elb_logs", "cloudfront_access_logs"],
            page_size: Some(50),
            expression: None,
            expected: &["cloudfront_access_logs", "elb_logs"],
        },
        PageWalk {
            case: "no page size at all",
            stored: &["elb_logs", "cloudfront_access_logs"],
            page_size: None,
            expression: None,
            expected: &["cloudfront_access_logs", "elb_logs"],
        },
        PageWalk {
            case: "a filtered selection, one table examined per page",
            stored: &[
                "elb_logs",
                "cloudfront_access_logs",
                "sales_daily",
                "vpc_flow_logs",
            ],
            page_size: Some(1),
            expression: Some("*_logs"),
            expected: &["cloudfront_access_logs", "elb_logs", "vpc_flow_logs"],
        },
        PageWalk {
            case: "a filtered selection, two tables examined per page",
            stored: &[
                "elb_logs",
                "cloudfront_access_logs",
                "sales_daily",
                "vpc_flow_logs",
            ],
            page_size: Some(2),
            expression: Some("*_logs"),
            expected: &["cloudfront_access_logs", "elb_logs", "vpc_flow_logs"],
        },
        PageWalk {
            case: "a filter that selects nothing",
            stored: &["elb_logs", "cloudfront_access_logs"],
            page_size: Some(1),
            expression: Some("sales*"),
            expected: &[],
        },
    ];

    for PageWalk {
        case,
        stored,
        page_size,
        expression,
        expected,
    } in rows
    {
        let (router, engine) = emulator_with_engine();
        let key = store_key(&engine);
        create_database(&router, "logs_db").await;
        for table in *stored {
            create_table(&router, "logs_db", table).await;
        }

        // The order the catalog keeps the tables in, which is the order a page examines.
        let mut order: Vec<&str> = stored.to_vec();
        order.sort_unstable();
        // The tables one response examines: the size asked for, else the page a request
        // that names none is served (the largest the model declares for the member).
        let window = usize::try_from(page_size.unwrap_or(100)).expect("a positive page size");

        let mut collected: Vec<String> = Vec::new();
        let mut examined = 0_usize;
        let mut pages = 0_usize;
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({ "DatabaseName": "logs_db" });
            let request = body.as_object_mut().expect("a structure");
            if let Some(size) = page_size {
                request.insert("MaxResults".to_string(), json!(size));
            }
            if let Some(expression) = expression {
                request.insert("Expression".to_string(), json!(expression));
            }
            if let Some(cursor) = &token {
                request.insert("NextToken".to_string(), json!(cursor));
            }

            let page = ok(&router, "GetTables", &body).await;
            let names = table_names(&page);
            pages += 1;

            // The page is exactly what the request selects among the tables this response
            // examined — at most as many as it asked for, and no table it did not reach.
            let examined_to = examined.saturating_add(window).min(order.len());
            let expected_page: Vec<String> = order[examined..examined_to]
                .iter()
                .filter(|name| expected.contains(name))
                .map(|name| (*name).to_string())
                .collect();
            assert_eq!(
                names, expected_page,
                "case {case}: expected the selection among the tables this page examined, \
                 got {page}"
            );
            collected.extend(names.iter().cloned());

            token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                token.is_some(),
                examined_to < order.len(),
                "case {case}: a NextToken is returned exactly while stored tables are left, \
                 got {page}"
            );
            if let Some(cursor) = &token {
                // The token is opaque, and it is this store's: it decodes only under the
                // key this store minted it with and the scope of THIS database's listing,
                // to the last table the page examined.
                assert_discloses_nothing(case, cursor, stored);
                assert_eq!(
                    decode_page_token(&key, &table_scope("logs_db"), cursor).as_deref(),
                    Some(order[examined_to - 1]),
                    "case {case}: the token must carry the position the page examined up to"
                );
            }
            examined = examined_to;
            if token.is_none() {
                break;
            }
            assert!(
                pages <= order.len() + 1,
                "case {case}: paging must terminate, but it produced {pages} pages"
            );
        }

        assert_eq!(
            collected, *expected,
            "case {case}: the pages must concatenate to the whole selection, in order"
        );
        let expected_pages = if order.is_empty() {
            1
        } else {
            order.len().div_ceil(window)
        };
        assert_eq!(
            pages, expected_pages,
            "case {case}: expected {expected_pages} pages over {} stored tables examined \
             {window} at a time, got {pages}",
            order.len()
        );
    }
}

/// A `GetTables` `NextToken` this store did not mint for THIS database's listing names no
/// position: it is refused with the catalog's own `InvalidInputException` and its own
/// sentence, rather than restarting or skipping the enumeration.
///
/// The rows differ along how the value was produced: the name of a table (the cursor a
/// plaintext listing would hand out), a value of no shape at all, a minted token with bytes
/// dropped, the SHAPE of a minted token with an authenticator a caller chose, a token this
/// store minted for another database, one it minted for another catalog, one another store
/// minted, and one minted by another listing of the same store.
#[tokio::test]
async fn get_tables_refuses_a_next_token_it_did_not_mint() {
    let (router, engine) = emulator_with_engine();
    let key = store_key(&engine);
    create_database(&router, "logs_db").await;
    for table in ["cloudfront_access_logs", "elb_logs", "vpc_flow_logs"] {
        create_table(&router, "logs_db", table).await;
    }
    create_database(&router, "other_db").await;
    for table in ["andes_3_0_data", "andes_3_0_index"] {
        create_table(&router, "other_db", table).await;
    }

    // The token the OTHER database's listing hands out, taken from that listing itself.
    let other_page = ok(
        &router,
        "GetTables",
        &json!({ "DatabaseName": "other_db", "MaxResults": 1 }),
    )
    .await;
    let other_database_token = other_page["NextToken"]
        .as_str()
        .expect("a listing with tables left over hands out a token")
        .to_string();

    let minted = encode_page_token(&key, &table_scope("logs_db"), "cloudfront_access_logs");
    let rows: Vec<(&str, String)> = vec![
        ("the name of the last table on the page", "cloudfront_access_logs".to_string()),
        ("a name no table carries", "zzz_nothing".to_string()),
        ("empty", String::new()),
        ("not a token at all", "not-a-token".to_string()),
        (
            "a minted token with its last byte dropped",
            minted[..minted.len() - 2].to_string(),
        ),
        (
            "the shape of a minted token, with a chosen authenticator",
            ::hex::encode(
                [
                    vec![0_u8; service_common::page_tokens::AUTHENTICATOR_BYTES].as_slice(),
                    b"cloudfront_access_logs".as_slice(),
                ]
                .concat(),
            ),
        ),
        ("a token minted for another database", other_database_token),
        (
            "a token minted for another catalog",
            encode_page_token(
                &key,
                &listing_scope("GetTables", &["210987654321", "logs_db"]),
                "cloudfront_access_logs",
            ),
        ),
        (
            "a token another store minted",
            encode_page_token(
                &PageTokenKey::new(),
                &table_scope("logs_db"),
                "cloudfront_access_logs",
            ),
        ),
        (
            "a token minted by the job listing",
            encode_page_token(&key, &job_scope(REGION), "cloudfront_access_logs"),
        ),
    ];

    for (case, token) in rows {
        let (status, body) = call(
            &router,
            "GetTables",
            &json!({ "DatabaseName": "logs_db", "MaxResults": 1, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a token this listing did not mint must be refused, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected the catalog's InvalidInputException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            CATALOG_TOKEN_REFUSAL,
            "case {case}: expected the catalog's own sentence, got {body}"
        );
    }

    // Positive control: the token this listing DID mint resumes it, strictly after the name
    // the previous page ended at.
    let first = ok(
        &router,
        "GetTables",
        &json!({ "DatabaseName": "logs_db", "MaxResults": 1 }),
    )
    .await;
    let resumed = ok(
        &router,
        "GetTables",
        &json!({
            "DatabaseName": "logs_db",
            "MaxResults": 1,
            "NextToken": first["NextToken"].as_str().expect("a token"),
        }),
    )
    .await;
    assert_eq!(
        (table_names(&first), table_names(&resumed)),
        (
            vec!["cloudfront_access_logs".to_string()],
            vec!["elb_logs".to_string()]
        ),
        "case the listing's own token: expected the walk to resume after the first name, got \
         {first} then {resumed}"
    );
}

/// A `MaxResults` above the largest page `GetTables` admits is refused at the request
/// boundary — the cross-service `ValidationException`, naming the member, the value sent and
/// the bound — while the bound itself is served; a `MaxResults` that names no servable page
/// at all is refused with the catalog's own `InvalidInputException` and its own fixed
/// sentence.
///
/// The rows differ along the page size asked for: the upper bound, one past it, a far larger
/// value and the largest value the member's type can carry — and, on the other side, the
/// value one below the modelled minimum, a negative size and the smallest value the type can
/// carry.
#[tokio::test]
async fn get_tables_refuses_a_page_size_above_the_largest_page_it_admits() {
    let (router, _engine) = emulator_with_engine();
    create_database(&router, "logs_db").await;
    for table in ["cloudfront_access_logs", "elb_logs"] {
        create_table(&router, "logs_db", table).await;
    }

    // The bound itself is a page the service serves.
    let served = ok(
        &router,
        "GetTables",
        &json!({ "DatabaseName": "logs_db", "MaxResults": MAX_TABLE_PAGE }),
    )
    .await;
    assert_eq!(
        table_names(&served),
        vec![
            "cloudfront_access_logs".to_string(),
            "elb_logs".to_string()
        ],
        "case MaxResults {MAX_TABLE_PAGE}: the largest admitted page is served, got {served}"
    );

    for refused in [
        MAX_TABLE_PAGE + 1,
        MAX_TABLE_PAGE + 2,
        5_000,
        i64::from(i32::MAX),
    ] {
        let (status, body) = call(
            &router,
            "GetTables",
            &json!({ "DatabaseName": "logs_db", "MaxResults": refused }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case MaxResults {refused}: a page size above the bound must be refused, got \
             {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("ValidationException"),
            "case MaxResults {refused}: expected ValidationException, got {}",
            error_code(&body)
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(
                format!(
                    "1 validation error detected: Value '{refused}' at 'maxResults' failed to \
                     satisfy constraint: Member must have value less than or equal to \
                     {MAX_TABLE_PAGE}"
                )
                .as_str()
            ),
            "case MaxResults {refused}: expected the boundary's own sentence, got {body}"
        );
    }

    // A size that names no servable page at all is the catalog's own input error, in the
    // one fixed sentence the service words for it — no value and no bound interpolated.
    for refused in [MIN_TABLE_PAGE - 1, -1, i64::from(i32::MIN)] {
        let (status, body) = call(
            &router,
            "GetTables",
            &json!({ "DatabaseName": "logs_db", "MaxResults": refused }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case MaxResults {refused}: a page no listing can serve must be refused, got \
             {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case MaxResults {refused}: expected the catalog's InvalidInputException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            PAGE_SIZE_REFUSAL,
            "case MaxResults {refused}: expected the catalog's own fixed sentence, got {body}"
        );
    }
}

/// `ListJobs` pages the names it reports behind opaque tokens of its own, and refuses any
/// value this store did not mint for THIS Region's job listing — with the sentence Glue
/// words for that operation, which is not the catalog's.
///
/// The rows differ along how the refused value was produced, and the walk itself differs
/// along the page size: the job listing's own token resumes it, a job name does not.
#[tokio::test]
async fn list_jobs_pages_behind_its_own_tokens_and_refuses_the_rest() {
    let (router, engine) = emulator_with_engine();
    let key = store_key(&engine);
    let stored = ["etl_daily", "etl_hourly", "etl_monthly", "etl_weekly"];
    // Created out of order, so the listing's own order is what the walk asserts.
    for name in stored.iter().rev() {
        create_job(&router, REGION, name).await;
    }
    // Another Region holds its own jobs, so its listing hands out its own tokens.
    create_job(&router, OTHER_REGION, "etl_daily").await;
    create_job(&router, OTHER_REGION, "etl_hourly").await;

    for page_size in [1_i64, 2, 3] {
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({ "MaxResults": page_size });
            if let Some(cursor) = &token {
                body["NextToken"] = json!(cursor);
            }
            let page = ok(&router, "ListJobs", &body).await;
            let names: Vec<String> = page["JobNames"]
                .as_array()
                .expect("JobNames is a list")
                .iter()
                .map(|name| name.as_str().expect("a job name").to_string())
                .collect();
            assert!(
                i64::try_from(names.len()).unwrap_or(i64::MAX) <= page_size,
                "case MaxResults {page_size}: a page holds at most that many names, got {names:?}"
            );
            collected.extend(names.iter().cloned());

            token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                token.is_some(),
                collected.len() < stored.len(),
                "case MaxResults {page_size}: a NextToken is returned exactly while jobs \
                 remain, got {page}"
            );
            let Some(cursor) = &token else {
                break;
            };
            assert_discloses_nothing(&format!("MaxResults {page_size}"), cursor, &stored);
            assert_eq!(
                decode_page_token(&key, &job_scope(REGION), cursor).as_deref(),
                names.last().map(String::as_str),
                "case MaxResults {page_size}: the token must carry the position the page ended \
                 at"
            );
        }
        assert_eq!(
            collected,
            stored.map(str::to_string).to_vec(),
            "case MaxResults {page_size}: the pages must concatenate to every stored job"
        );
    }

    // The token the OTHER Region's job listing hands out, taken from that listing itself.
    let (status, other_region_page) =
        call_in_region(&router, OTHER_REGION, "ListJobs", &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "ListJobs in {OTHER_REGION} must answer 200, got {status} with {other_region_page}"
    );
    let other_region_token = other_region_page["NextToken"]
        .as_str()
        .expect("a listing with jobs left over hands out a token")
        .to_string();

    let minted = encode_page_token(&key, &job_scope(REGION), "etl_daily");
    let rows: Vec<(&str, String)> = vec![
        ("the name of the last job on the page", "etl_daily".to_string()),
        ("empty", String::new()),
        ("not a token at all", "not-a-token".to_string()),
        (
            "a minted token with its last byte dropped",
            minted[..minted.len() - 2].to_string(),
        ),
        (
            "the shape of a minted token, with a chosen authenticator",
            ::hex::encode(
                [
                    vec![0_u8; service_common::page_tokens::AUTHENTICATOR_BYTES].as_slice(),
                    b"etl_daily".as_slice(),
                ]
                .concat(),
            ),
        ),
        ("a token minted for another Region", other_region_token),
        (
            "a token another store minted",
            encode_page_token(&PageTokenKey::new(), &job_scope(REGION), "etl_daily"),
        ),
        (
            "a token minted by the table listing",
            encode_page_token(&key, &table_scope("logs_db"), "etl_daily"),
        ),
    ];

    for (case, token) in rows {
        let (status, body) = call(
            &router,
            "ListJobs",
            &json!({ "MaxResults": 1, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a token this listing did not mint must be refused, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected the job listing's InvalidInputException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            JOB_TOKEN_REFUSAL,
            "case {case}: expected the sentence this operation words, got {body}"
        );
    }
}
