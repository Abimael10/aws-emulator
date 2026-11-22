// SPDX-License-Identifier: CC0-1.0

//! What a data source introspection DIAGNOSES, and how a call that names no data source
//! at all is refused.
//!
//! Two rules, each stated over a class of input rather than over the one request that
//! exposed the defect:
//!
//! 1. an introspection resolves the cluster its configuration names with the control
//!    plane that OWNS it before it reads any schema, so a `resourceArn` that control
//!    plane holds no cluster for settles the job `FAILED` with the service's fixed
//!    cluster-not-found sentence — whatever makes the ARN name nothing (an identifier
//!    nothing created, one that extends or differs by a character from a live one, another
//!    Region, no identifier at all) — while a cluster that IS there keeps the diagnosis of
//!    whatever the Data API refused the read with;
//! 2. a `StartDataSourceIntrospection` carrying no data source configuration is a 400
//!    `BadRequestException` with the fixed sentence for the general absence of a
//!    configuration, in every form such a request takes.
//!
//! Every cluster a case needs is created through its OWNER's own operation
//! (`rds:CreateDBCluster`, RDS managing the master user password), so what the
//! introspection reports about a cluster rests on a cluster this emulator really holds —
//! and the positive control in the first case proves the resolution admits one.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::{state, EmulatedService};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, and the Region every resource lives in.
pub(crate) const REGION: &str = "us-east-1";

/// A second Region, so a case can address a cluster where this emulator holds none.
const OTHER_REGION: &str = "eu-west-1";

/// The master user every cluster here is created with.
const MASTER_USER: &str = "pgadmin";

/// The database every cluster here is created with (`DBName`), which is the one an
/// introspection names.
pub(crate) const DATABASE: &str = "appdb";

/// The access key id every call presents: a key neither STS nor IAM holds a record for,
/// which is the position AWS account root credentials are in.
const ACCOUNT_KEY_ID: &str = "AKIAEMULATOR";

/// The path both introspection operations are served on.
const INTROSPECTIONS_PATH: &str = "/v1/datasources/introspections";

/// The non-terminal `DataSourceIntrospectionStatus`, as the service names it.
const PROCESSING: &str = state::INTROSPECTION_PROCESSING;

/// How long a case waits for the status the clock owes, at the outside. Generous on
/// purpose: the case waits for the STATUS to settle rather than for a span of time, so a
/// loaded host only polls more often, never fails.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case leaves between two looks at a record that has not settled yet.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One answer from the emulator: the parts a client branches on.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    /// The `x-amzn-errortype` header restJson1 resolves the error shape from.
    error_type: String,
    pub(crate) body: Value,
}

/// One emulator: AppSync with the owners of what an introspection reads mounted beside it
/// — the RDS control plane that holds the cluster, the Data API that serves its databases,
/// and the Secrets Manager (with the KMS its value is sealed under) the login comes from —
/// over ONE shared engine, exactly as the assembled router mounts them.
pub(crate) struct Cloud {
    router: MultiServiceRouter,
}

impl Cloud {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Verifying signatures is the emulator's concern, not this service's; these tests
        // sign nothing, so the check is off and the credential scope is read for routing
        // and for the access key id alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_rdsdata_rdsdataservice::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// An Amazon RDS client over this emulator's own in-process callback.
    fn rds(&self) -> aws_sdk_rds::Client {
        let callback = smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone());
        let config = aws_sdk_rds::Config::builder()
            .behavior_version(aws_sdk_rds::config::BehaviorVersion::latest())
            .region(aws_sdk_rds::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://rds.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(aws_sdk_rds::config::retry::RetryConfig::disabled())
            .timeout_config(aws_sdk_rds::config::timeout::TimeoutConfig::disabled())
            .identity_cache(aws_sdk_rds::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                aws_sdk_rds::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        aws_sdk_rds::Client::from_conf(config)
    }

    /// Create a cluster the way a Data API caller does — its HTTP endpoint on or off as
    /// `http_endpoint` says, RDS managing the master user password — and answer its ARN
    /// and the ARN of the secret RDS minted for it.
    pub(crate) async fn create_cluster(
        &self,
        identifier: &str,
        http_endpoint: bool,
    ) -> (String, String) {
        let created = self
            .rds()
            .create_db_cluster()
            .db_cluster_identifier(identifier)
            .engine("aurora-postgresql")
            .master_username(MASTER_USER)
            .manage_master_user_password(true)
            .database_name(DATABASE)
            .enable_http_endpoint(http_endpoint)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {identifier}: CreateDBCluster must be served: {}",
                    aws_smithy_types::error::display::DisplayErrorContext(&error)
                )
            });
        let cluster = created
            .db_cluster()
            .unwrap_or_else(|| panic!("case {identifier}: a created cluster is reported"));
        (
            cluster
                .db_cluster_arn()
                .unwrap_or_else(|| panic!("case {identifier}: a created cluster carries its ARN"))
                .to_string(),
            cluster
                .master_user_secret()
                .and_then(aws_sdk_rds::types::MasterUserSecret::secret_arn)
                .unwrap_or_else(|| {
                    panic!("case {identifier}: a managed-password cluster reports its secret")
                })
                .to_string(),
        )
    }

    /// Run one statement against `cluster` through the Data API, which must serve it.
    pub(crate) async fn execute(&self, cluster: &str, secret: &str, sql: &str) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://rds-data.{REGION}.amazonaws.com/Execute"))
            .header("host", format!("rds-data.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("rds-data"))
            .body(bytes::Bytes::from(
                json!({
                    "resourceArn": cluster,
                    "secretArn": secret,
                    "database": DATABASE,
                    "sql": sql,
                })
                .to_string(),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        assert_eq!(status, 200, "case {sql}: expected 200, got {status} {body}");
    }

    /// Send one AppSync request and return what the emulator answered.
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(value.to_string())
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
            .header("host", format!("appsync.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("appsync"))
            .body(payload)
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// `StartDataSourceIntrospection` with the body `body` carries.
    async fn start(&self, body: Value) -> Answer {
        self.call("POST", INTROSPECTIONS_PATH, Some(body)).await
    }

    /// Start an introspection of the cluster `resource_arn` names, which must be accepted,
    /// and read the record back once the clock has settled it.
    pub(crate) async fn settled_introspection(
        &self,
        case: &str,
        resource_arn: &str,
        secret_arn: &str,
        database: &str,
    ) -> Answer {
        let started = self
            .start(json!({
                "rdsDataApiConfig": {
                    "resourceArn": resource_arn,
                    "secretArn": secret_arn,
                    "databaseName": database,
                },
            }))
            .await;
        assert_eq!(
            started.status, 200,
            "case {case}: an introspection is accepted and reports asynchronously, got {} {}",
            started.status, started.body
        );
        let introspection_id = started.body["introspectionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a started introspection carries its id"))
            .to_string();
        // Waits for the CONDITION — a status that is no longer the non-terminal one — by
        // looking again until it holds or the deadline passes, so nothing here depends on
        // how fast the host runs.
        let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let answer = self
                .call(
                    "GET",
                    &format!("{INTROSPECTIONS_PATH}/{introspection_id}"),
                    None,
                )
                .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected GetDataSourceIntrospection 200, got {} {}",
                answer.status, answer.body
            );
            if answer.body["introspectionStatus"].as_str() != Some(PROCESSING) {
                return answer;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "case {case}: the introspection never left {PROCESSING}: {}",
                answer.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// One request's SigV4 authorization header, for the service `signing_name` claims.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=unverified"
    )
}

/// A well-formed RDS DB cluster ARN, as `RdsDataApiConfig$resourceArn`'s modelled pattern
/// (`^arn:[a-z-]*:rds:[a-z0-9-]*:\d{12}:cluster:[0-9A-Za-z_/-]*$`) admits one.
pub(crate) fn cluster_arn(region: &str, account: &str, identifier: &str) -> String {
    format!("arn:aws:rds:{region}:{account}:cluster:{identifier}")
}

/// RULE: an introspection resolves the cluster its configuration names with the control
/// plane that owns it, so a `resourceArn` that control plane holds no cluster for settles
/// the job `FAILED` with the fixed cluster-not-found sentence and reports no result —
/// while a cluster the control plane DOES hold is introspected.
#[tokio::test]
async fn a_configuration_that_names_no_cluster_reports_the_cluster_as_not_found() {
    let cloud = Cloud::new();
    let (live, secret) = cloud.create_cluster("introspection-diagnosis", true).await;
    cloud
        .execute(&live, &secret, "CREATE TABLE todos (id serial PRIMARY KEY)")
        .await;
    let account = engine::emulator_account_id();

    // (case, the resourceArn) — one row per way a well-formed ARN names no cluster: an
    // identifier nothing created, one that merely extends a live cluster's identifier,
    // one in a Region holding no cluster of that name, one differing from a live
    // identifier by a single character, and one naming no identifier at all (the member's
    // pattern admits an empty resource id).
    let rows: Vec<(&str, String)> = vec![
        (
            "an identifier nothing created",
            cluster_arn(REGION, account, "never-created-cluster"),
        ),
        (
            "an identifier extending a live one",
            format!("{live}-replica"),
        ),
        (
            "a live identifier in another Region",
            cluster_arn(OTHER_REGION, account, "introspection-diagnosis"),
        ),
        (
            "an identifier differing from a live one in one character",
            cluster_arn(REGION, account, "introspection-diagnosis1"),
        ),
        ("no identifier at all", cluster_arn(REGION, account, "")),
    ];
    for (case, resource_arn) in rows {
        let settled = cloud
            .settled_introspection(case, &resource_arn, &secret, DATABASE)
            .await;
        assert_eq!(
            settled.body["introspectionStatus"].as_str(),
            Some(state::INTROSPECTION_FAILED),
            "case {case}: expected a failed introspection, got {}",
            settled.body
        );
        assert_eq!(
            settled.body["introspectionStatusDetail"].as_str(),
            Some(state::CLUSTER_NOT_FOUND_DETAIL),
            "case {case}: expected {:?}, got {}",
            state::CLUSTER_NOT_FOUND_DETAIL,
            settled.body
        );
        assert_eq!(
            settled.body["introspectionResult"],
            Value::Null,
            "case {case}: a failed introspection read no schema, got {}",
            settled.body
        );
    }

    // The positive control: the cluster the control plane DOES hold is introspected, so
    // the resolution above admits a cluster rather than refusing every ARN.
    let case = "the cluster that was created";
    let settled = cloud
        .settled_introspection(case, &live, &secret, DATABASE)
        .await;
    assert_eq!(
        settled.body["introspectionStatus"].as_str(),
        Some(state::INTROSPECTION_SUCCESS),
        "case {case}: expected the schema to be read, got {}",
        settled.body
    );
    assert_eq!(
        settled.body["introspectionResult"]["models"][0]["name"].as_str(),
        Some("todos"),
        "case {case}: expected the table the cluster holds, got {}",
        settled.body
    );
}

/// RULE: a cluster the control plane DOES hold is diagnosed by what the read of it was
/// refused with — never as a cluster that was not found.
#[tokio::test]
async fn a_cluster_that_exists_keeps_the_diagnosis_of_the_refused_read() {
    let cloud = Cloud::new();
    let (served, secret) = cloud.create_cluster("introspection-served", true).await;
    let (unserved, unserved_secret) = cloud
        .create_cluster("introspection-endpoint-off", false)
        .await;

    // (case, the resourceArn, its secret, the database, what the reason must name)
    let rows: Vec<(&str, &str, &str, &str, &str)> = vec![
        (
            "a cluster whose Data API endpoint is off",
            &unserved,
            &unserved_secret,
            DATABASE,
            unserved.as_str(),
        ),
        (
            "a database the cluster does not have",
            &served,
            &secret,
            "nosuchdb",
            "nosuchdb",
        ),
    ];
    for (case, resource_arn, secret_arn, database, named) in rows {
        let settled = cloud
            .settled_introspection(case, resource_arn, secret_arn, database)
            .await;
        assert_eq!(
            settled.body["introspectionStatus"].as_str(),
            Some(state::INTROSPECTION_FAILED),
            "case {case}: expected a failed introspection, got {}",
            settled.body
        );
        let detail = settled.body["introspectionStatusDetail"]
            .as_str()
            .unwrap_or_default();
        assert_ne!(
            detail,
            state::CLUSTER_NOT_FOUND_DETAIL,
            "case {case}: a cluster the control plane holds is not reported as absent, got {detail:?}"
        );
        assert!(
            detail.contains(named),
            "case {case}: expected the reason to name {named:?}, got {detail:?}"
        );
    }
}

/// RULE: a `StartDataSourceIntrospection` carrying no data source configuration is a 400
/// `BadRequestException` whose message is the fixed sentence for the general absence of a
/// configuration.
#[tokio::test]
async fn a_call_with_no_data_source_config_is_refused_with_the_fixed_sentence() {
    let cloud = Cloud::new();

    // (case, the body) — one row per form such a request takes: no member at all, a
    // request carrying another data source's configuration member instead, and one
    // carrying a member this operation's input models nothing for.
    let rows: Vec<(&str, Value)> = vec![
        ("no member at all", json!({})),
        (
            "some other configuration member",
            json!({"relationalDatabaseConfig": {"relationalDatabaseSourceType": "RDS_HTTP_ENDPOINT"}}),
        ),
        (
            "a member this operation models nothing for",
            json!({"introspectionId": "an-id-of-another-call"}),
        ),
    ];
    for (case, body) in rows {
        let refused = cloud.start(body).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected 400, got {} {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "BadRequestException",
            "case {case}: expected BadRequestException, got {} {}",
            refused.error_type, refused.body
        );
        assert_eq!(
            refused.body["message"].as_str(),
            Some(state::NO_DATA_SOURCE_CONFIG_MESSAGE),
            "case {case}: expected {:?}, got {}",
            state::NO_DATA_SOURCE_CONFIG_MESSAGE,
            refused.body
        );
    }
}
