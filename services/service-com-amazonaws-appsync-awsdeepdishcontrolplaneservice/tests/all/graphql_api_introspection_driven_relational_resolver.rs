// SPDX-License-Identifier: CC0-1.0

//! The documented path a client of a relational database takes end to end: DISCOVER the
//! database's schema with `StartDataSourceIntrospection`, build a GraphQL schema and an
//! APPSYNC_JS unit resolver out of the field names that discovery reported, and then QUERY the
//! API over the address it published for its `GRAPHQL` endpoint.
//!
//! The rule asserted here, one level above the one recorded round trip (one query reading one
//! column of one row):
//!
//! > A field bound to a `RELATIONAL_DATABASE` data source whose
//! > `relationalDatabaseConfig.relationalDatabaseSourceType` is `RDS_HTTP_ENDPOINT` is
//! > resolved by EVALUATING its resolver's request and response halves and running the
//! > statements the request half returned against the database that configuration names,
//! > through that cluster's own HTTP endpoint — so the GraphQL answer carries `data` with no
//! > `errors` member and the field reports the rows that database holds, including when the
//! > statement is built from the very column names an introspection of that same database
//! > discovered.
//!
//! The cases span what the rule generalises over: which discovered model the resolver reads
//! (two tables of one database), how many columns that model has, the type of the column a row
//! is addressed by (a number and text), the form the statement is built in (the `select`
//! builder of `@aws-appsync/utils/rds`, and a tagged `sql` template), and how many rows the
//! statement matches (one, none).
//!
//! Everything a case needs belongs to the service that OWNS it: the cluster and its database
//! to Amazon RDS and the Data API, the login to Secrets Manager, the permissions to IAM. The
//! helpers that create them, and the response half every coded resolver shares, are this
//! crate's own ([`crate::graphql_api_relational_database_data_source`]), so there is one
//! reading of how a relational data source is set up rather than one per suite.

use std::time::Duration;

use serde_json::{json, Map as JsonMap, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_api_data_plane::appsync;
use crate::graphql_api_mapping_template_resolvers::{
    install_schema, try_post_answer, GRAPHQL_PATH, REGION,
};
use crate::graphql_api_relational_database_data_source::{
    allowing, authority_of, emulator, execute, given_cluster, given_role, responds_with_rows,
    Cluster, DATABASE, IMPORTS, RUNTIME, SEEDED, TABLE,
};

/// The path both introspection operations are served on (`StartDataSourceIntrospection`
/// `POST /v1/datasources/introspections`, `GetDataSourceIntrospection` the same path with the
/// minted id appended).
const INTROSPECTIONS_PATH: &str = "/v1/datasources/introspections";

/// The name of the one data source every resolver of this suite resolves against.
const DATA_SOURCE: &str = "discoveredDb";

/// The second table of this suite's database, beside the one the shared cluster helper
/// creates: another column count, another key type and a nullable column, so the cases read
/// more than one discovered model.
const OTHER_TABLE: &str = "gadgets";
const OTHER_CREATE: &str = "CREATE TABLE gadgets (sku VARCHAR(16) NOT NULL PRIMARY KEY, \
                            label VARCHAR(64) NOT NULL, weight INT)";
const OTHER_SEEDED: &[(&str, &str, i64)] = &[("g-1", "gadget one", 5), ("g-2", "gadget two", 11)];

/// The columns each table of this suite's database declares, in the order it declares them:
/// what an introspection of that database must discover, and what every resolver below is
/// built from. Source: the `CREATE TABLE` statements this suite runs through the Data API —
/// the shared helper's for [`TABLE`], [`OTHER_CREATE`] for [`OTHER_TABLE`].
const DECLARED: &[(&str, &[&str])] = &[
    (TABLE, &["id", "name"]),
    (OTHER_TABLE, &["sku", "label", "weight"]),
];

/// How long a case waits for the clock to perform an introspection, far past the service's own
/// span so a loaded machine still reaches it, and how long it leaves between two looks. The
/// wait is on the CONDITION — the status the record reports — so nothing here depends on how
/// fast the host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const POLL_SPACING: Duration = Duration::from_millis(50);

/// RULE: a resolver built from the columns a data source introspection discovered is EVALUATED
/// for its field, its statements are run against the database that data source names, and the
/// field reports the rows that database holds — with no `errors` member in the answer.
#[tokio::test]
async fn a_resolver_built_from_a_discovered_schema_reads_that_databases_rows() {
    let router = emulator();
    let cluster = given_cluster(&router, "discovered-widgets").await;
    execute(&router, &cluster, OTHER_CREATE).await;
    for (sku, label, weight) in OTHER_SEEDED {
        execute(
            &router,
            &cluster,
            &format!(
                "INSERT INTO {OTHER_TABLE} (sku, label, weight) \
                 VALUES ('{sku}', '{label}', {weight})"
            ),
        )
        .await;
    }
    let role = given_role(
        &router,
        "appsync-discovered-rds",
        Some(&allowing(&cluster.arn)),
    )
    .await;

    // The names every resolver below is built from are DISCOVERED, through the two operations
    // a client discovers them with, and they are the columns the database itself declares.
    let models = introspected(&router, &cluster).await;
    for (table, columns) in DECLARED {
        let model = models
            .iter()
            .find(|model| model.name == *table)
            .unwrap_or_else(|| {
                panic!("case {table:?}: expected a discovered model of this table, got {models:?}")
            });
        assert_eq!(
            model.names(),
            *columns,
            "case {table:?}: expected the columns the table declares, got {:?}",
            model.fields
        );
    }

    let api = create_api(&router, &cluster, &role, &models).await;

    /// One case: the query document a client posts and what the field must resolve to.
    struct Case {
        case: String,
        document: String,
        field: String,
        expected: Value,
    }
    let mut cases: Vec<Case> = Vec::new();
    for model in &models {
        let (key, _) = addressed(&model.name);
        let key_scalar = model.scalar(key).to_string();
        let at = model.at(key);
        let field = field_name(&model.name);
        let selection = model.names().join(" ");
        for values in rows_of(&model.name) {
            cases.push(Case {
                case: format!(
                    "the row of {} whose {key} is {:?}, read through the {} form",
                    model.name,
                    values[at],
                    match addressed(&model.name).1 {
                        true => "builder",
                        false => "tagged template",
                    }
                ),
                document: format!(
                    "query {{ {field}({key}: {}) {{ {selection} }} }}",
                    literal(&key_scalar, &values[at])
                ),
                field: field.clone(),
                expected: expected_row(model, &values),
            });
        }
        cases.push(Case {
            case: format!("a {key} no row of {} carries", model.name),
            document: format!(
                "query {{ {field}({key}: {}) {{ {selection} }} }}",
                literal(&key_scalar, &absent_key(&model.name))
            ),
            field: field.clone(),
            expected: Value::Null,
        });
    }

    for case in &cases {
        let answered = post(&case.case, &api, &json!({"query": case.document})).await;
        assert!(
            answered.get("errors").is_none(),
            "case {:?}: a resolver the data plane evaluates reports no error, got {answered}",
            case.case
        );
        assert_eq!(
            answered["data"][&case.field], case.expected,
            "case {:?}: expected {}, got {answered}",
            case.case, case.expected
        );
    }

    // Reading a field changes nothing a later caller sees: the same query answers the same
    // document.
    let first = &cases[0];
    let again = post(&first.case, &api, &json!({"query": first.document})).await;
    assert_eq!(
        again["data"][&first.field], first.expected,
        "case {:?}: expected a second read to answer the same row, got {again}",
        first.case
    );
}

/// One discovered model: the table's own name and the fields the introspection reported for
/// it, each as its name and the AppSync scalar its column's type maps to.
#[derive(Debug, Clone)]
struct Discovered {
    name: String,
    fields: Vec<(String, String)>,
}

impl Discovered {
    /// The names of every field discovered, in the order they were reported.
    fn names(&self) -> Vec<&str> {
        self.fields.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// The scalar discovery reported for the field `field`.
    fn scalar(&self, field: &str) -> &str {
        let (_, scalar) = self
            .fields
            .iter()
            .find(|(name, _)| name == field)
            .unwrap_or_else(|| panic!("the column {field:?} of {} was discovered", self.name));
        scalar
    }

    /// Where the field `field` sits among the discovered fields, which is where its value sits
    /// in a row of this table.
    fn at(&self, field: &str) -> usize {
        self.fields
            .iter()
            .position(|(name, _)| name == field)
            .unwrap_or_else(|| panic!("the column {field:?} of {} was discovered", self.name))
    }
}

/// Introspect `cluster`'s own database the way a client does — start one introspection and read
/// the record back until the clock has performed it — and answer the models it discovered.
async fn introspected(router: &MultiServiceRouter, cluster: &Cluster) -> Vec<Discovered> {
    let (status, started) = appsync(
        router,
        "POST",
        INTROSPECTIONS_PATH,
        Some(json!({
            "rdsDataApiConfig": {
                "resourceArn": cluster.arn,
                "secretArn": cluster.secret_arn,
                "databaseName": DATABASE,
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartDataSourceIntrospection must answer 200, got {started}"
    );
    let introspection_id = started["introspectionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a started introspection carries its id, got {started}"))
        .to_string();
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = appsync(
            router,
            "GET",
            &format!("{INTROSPECTIONS_PATH}/{introspection_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "GetDataSourceIntrospection must answer 200, got {read}"
        );
        match read["introspectionStatus"].as_str() {
            Some(reported) if reported == state::INTROSPECTION_SUCCESS => {
                return discovered(&read)
            }
            Some(reported) if reported == state::INTROSPECTION_PROCESSING => {}
            _ => panic!("the introspection must read the schema of the database, got {read}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the introspection must be performed, got {read}"
        );
        tokio::time::sleep(POLL_SPACING).await;
    }
}

/// The models one settled introspection reported.
fn discovered(read: &Value) -> Vec<Discovered> {
    read["introspectionResult"]["models"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|model| Discovered {
            name: model["name"].as_str().unwrap_or_default().to_string(),
            fields: model["fields"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|field| {
                    (
                        field["name"].as_str().unwrap_or_default().to_string(),
                        scalar_of(&field["type"]).to_string(),
                    )
                })
                .collect(),
        })
        .collect()
}

/// The scalar a discovered field's type names, through the non-null wrapper a column that
/// admits no null is reported in.
fn scalar_of(field_type: &Value) -> &str {
    match field_type["kind"].as_str() {
        Some("NonNull") => scalar_of(&field_type["type"]),
        _ => field_type["name"].as_str().unwrap_or_default(),
    }
}

/// The column a case addresses a row of `table` by, and whether that table's resolver builds
/// its statement with the `select` builder (`true`) or with a tagged `sql` template (`false`) —
/// the two forms the `@aws-appsync/utils/rds` module offers.
fn addressed(table: &str) -> (&'static str, bool) {
    if table == TABLE {
        ("id", true)
    } else {
        ("sku", false)
    }
}

/// The rows this suite wrote into `table` through the Data API, as the text of every column in
/// the order the table declares them.
fn rows_of(table: &str) -> Vec<Vec<String>> {
    if table == TABLE {
        SEEDED
            .iter()
            .map(|(id, name)| vec![id.to_string(), (*name).to_string()])
            .collect()
    } else {
        OTHER_SEEDED
            .iter()
            .map(|(sku, label, weight)| {
                vec![
                    (*sku).to_string(),
                    (*label).to_string(),
                    weight.to_string(),
                ]
            })
            .collect()
    }
}

/// A value of the key column that no row of `table` carries.
fn absent_key(table: &str) -> String {
    if table == TABLE {
        "4242".to_string()
    } else {
        "g-absent".to_string()
    }
}

/// One row as the field must report it: every field the introspection discovered, carrying the
/// value the database holds for that column, under the scalar discovery mapped its type to.
fn expected_row(model: &Discovered, values: &[String]) -> Value {
    let mut row = JsonMap::new();
    for ((name, scalar), value) in model.fields.iter().zip(values) {
        row.insert(name.clone(), scalar_value(scalar, value));
    }
    Value::Object(row)
}

/// The JSON a column's value is reported as under the AppSync scalar discovery mapped its type
/// to: a numeric scalar carries the number, a boolean the truth of it, every other scalar the
/// text. Source: the scalars a relational introspection maps a column's type to
/// (`smithy_query(query="shape",shape="DataSourceIntrospectionModelFieldType")`).
fn scalar_value(scalar: &str, value: &str) -> Value {
    match scalar {
        "Int" => json!(value
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("{value:?} is the value of an Int column"))),
        "Float" => json!(value
            .parse::<f64>()
            .unwrap_or_else(|_| panic!("{value:?} is the value of a Float column"))),
        "Boolean" => json!(value == "true" || value == "1"),
        _ => json!(value),
    }
}

/// `value` written as a GraphQL argument literal of the scalar `scalar`.
fn literal(scalar: &str, value: &str) -> String {
    match scalar {
        "Int" | "Float" => value.to_string(),
        _ => format!("\"{value}\""),
    }
}

/// The GraphQL type the fields of the model `model` are declared as: its own name, as a client
/// building a schema out of a discovered model writes it.
fn type_name(model: &str) -> String {
    let mut characters = model.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// The query field that reads one row of the model `model`.
fn field_name(model: &str) -> String {
    format!("get{}", type_name(model))
}

/// The schema this suite installs: one type per discovered model, carrying exactly the fields
/// discovery reported under the scalars it reported them as, and one query field per model that
/// reads one row of it by its key column.
fn schema_of(models: &[Discovered]) -> String {
    let mut queries = String::new();
    let mut types = String::new();
    for model in models {
        let (key, _) = addressed(&model.name);
        queries.push_str(&format!(
            "  {}({key}: {}!): {}\n",
            field_name(&model.name),
            model.scalar(key),
            type_name(&model.name),
        ));
        let fields: String = model
            .fields
            .iter()
            .map(|(name, scalar)| format!("  {name}: {scalar}\n"))
            .collect();
        types.push_str(&format!(
            "\ntype {} {{\n{fields}}}\n",
            type_name(&model.name)
        ));
    }
    format!("schema {{ query: Query }}\n\ntype Query {{\n{queries}}}\n{types}")
}

/// The APPSYNC_JS code the resolver of `model`'s field is defined by: a request half that
/// builds its statement out of the DISCOVERED column names, and the response half every coded
/// resolver of the relational suite shares.
fn resolver_code(model: &Discovered) -> String {
    let (key, builder) = addressed(&model.name);
    let names = model.names();
    let request = if builder {
        format!(
            "export function request(ctx) {{\n\
             return createMySQLStatement(select({{\n\
             table: '{}',\n\
             columns: [{}],\n\
             where: {{{key}: {{eq: ctx.args.{key}}}}},\n\
             limit: 1,\n\
             }}));\n\
             }}\n",
            model.name,
            names
                .iter()
                .map(|name| format!("'{name}'"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    } else {
        format!(
            "export function request(ctx) {{\n\
             return createMySQLStatement(\n\
             sql`select {} from {} where {key} = ${{ctx.args.{key}}}`\n\
             );\n\
             }}\n",
            names.join(", "),
            model.name,
        )
    };
    format!("{IMPORTS}{request}{}", responds_with_rows(0, true))
}

/// One created GraphQL API of this suite: the address its `GRAPHQL` endpoint is served on and a
/// live API key to present to it.
struct Api {
    graphql: String,
    api_key: String,
}

/// Create one GraphQL API with an API key, the schema the discovered models define, a
/// `RELATIONAL_DATABASE` data source on `cluster` under `role_arn`, and one coded resolver per
/// discovered model.
async fn create_api(
    router: &MultiServiceRouter,
    cluster: &Cluster,
    role_arn: &str,
    models: &[Discovered],
) -> Api {
    let (status, created) = appsync(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": "rds-discovered",
            "authenticationType": "API_KEY",
        })),
    )
    .await;
    assert_eq!(status, 200, "CreateGraphqlApi must answer 200, got {created}");
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    // The address a client dials is the one the API PUBLISHES for its GraphQL endpoint, which
    // is what the documented path posts to.
    let published = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_else(|| panic!("a created API publishes its GRAPHQL endpoint, got {created}"))
        .to_string();
    let (status, key) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    assert_eq!(status, 200, "CreateApiKey must answer 200, got {key}");
    install_schema(router, &api_id, &schema_of(models)).await;
    let (status, data_source) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": DATA_SOURCE,
            "type": "RELATIONAL_DATABASE",
            "serviceRoleArn": role_arn,
            "relationalDatabaseConfig": {
                "relationalDatabaseSourceType": "RDS_HTTP_ENDPOINT",
                "rdsHttpEndpointConfig": {
                    "awsRegion": REGION,
                    "dbClusterIdentifier": cluster.arn,
                    "databaseName": DATABASE,
                    "awsSecretStoreArn": cluster.secret_arn,
                },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateDataSource must answer 200, got {data_source}"
    );
    for model in models {
        let field = field_name(&model.name);
        let (status, created) = appsync(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": DATA_SOURCE,
                "code": resolver_code(model),
                "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateResolver for {field} must answer 200, got {created}"
        );
        let (status, read) = appsync(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/types/Query/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "GetResolver for {field} must answer 200, got {read}"
        );
        assert_eq!(
            read["resolver"]["dataSourceName"].as_str(),
            Some(DATA_SOURCE),
            "the stored resolver names the data source it was created with, got {read}"
        );
    }
    Api {
        graphql: authority_of(&published),
        api_key: key["apiKey"]["id"].as_str().unwrap_or_default().to_string(),
    }
}

/// POST one GraphQL request to the address the API published, as a client does, and answer the
/// document it was served.
async fn post(case: &str, api: &Api, document: &Value) -> Value {
    let (status, _, answered) = try_post_answer(
        case,
        &api.graphql,
        GRAPHQL_PATH,
        "POST",
        document,
        &[("x-api-key", api.api_key.as_str())],
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "case {case:?}: the published GraphQL endpoint {:?} must answer this request",
            api.graphql
        )
    });
    assert_eq!(
        status, 200,
        "case {case:?}: a GraphQL request is answered 200, got {status} and {answered}"
    );
    answered
}
