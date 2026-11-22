// SPDX-License-Identifier: CC0-1.0

//! Which EXPRESSION a `@pattern` clause of the cloud connector slice quotes, and that
//! widening a clause's text never widens or narrows what the boundary accepts.
//!
//! The rule under test: a `@pattern` clause states the expression the service PUBLISHES for
//! the member's shape, which belongs to the SHAPE and not to one operation or one request —
//! the model's text verbatim, whole-value anchors included, for the two ARNs a cloud connector
//! is created from, and the model's text without the pair a model writes around a published
//! expression for the shapes published that way. The CHECK is the model's own expression
//! either way — including the one the front door's engine does not compile as the model
//! spelled it — so the rows below also drive the values each expression admits through the
//! boundary and on to the handler.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router — and the tables
//! span the class the rule generalises over: three member shapes, three operations, two
//! constraint classes, several spellings of a value that is not the ARN its member models,
//! and partitions and punctuation on either side of each expression's own alternation.

use serde_json::{json, Value};

use crate::constraint_set_refusal_wording::{clause, detected, pattern};
use crate::parameter_store::{call, error_code, error_message, router, ACCOUNT};

/// `com.amazonaws.ssm#ConfigConnectorArn`'s expression as the model declares it
/// (`smithy_query(query="shape",shape="ConfigConnectorArn")`), which is what a clause about a
/// `configConnectorArn` quotes: this service publishes it as declared.
const CONFIG_CONNECTOR_ARN: &str = r"^arn:aws(-cn|-us-gov)?:config:([^:]+):\d{12}:connector/.+$";

/// `com.amazonaws.ssm#ConfigConnectorArn`'s published ceiling
/// (`smithy_query(query="shape",shape="ConfigConnectorArn")`: `length 1..512`).
const CONFIG_CONNECTOR_ARN_MAX: usize = 512;

/// The expression a clause about a `cloudConnectorId` quotes: `CloudConnectorId`'s text
/// (`smithy_query(query="shape",shape="CloudConnectorId")` is
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) without the whole-value
/// pair the model wrote around it, which is how this service publishes that shape.
const CLOUD_CONNECTOR_ID: &str = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";

/// A 36-character id that is not hexadecimal: it satisfies the exact length
/// `CloudConnectorId` declares and breaks only its expression, so the clause it draws is
/// about the expression alone.
const UNHEXED_ID: &str = "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz";

/// An Azure tenant id (`AzureConfiguration$TenantId`, a hex UUID).
const TENANT_ID: &str = "11111111-2222-3333-4444-555555555555";

/// `AzureConfiguration$ApplicationId`, the same shape.
const APPLICATION_ID: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

/// `com.amazonaws.ssm#CloudConnectorIamRoleArn`'s expression as the model declares it
/// (`smithy_query(query="shape",shape="CloudConnectorIamRoleArn")`), which is what a clause
/// about a `roleArn` quotes: this service publishes it as declared too.
const ROLE_ARN: &str = r"^arn:aws[a-z0-9-]*:iam::\d{12}:role\/[\w-\/.@+=,]{1,1017}$";

/// A `CreateCloudConnector` body carrying `config_connector_arn`; every other member is
/// well-formed, so a refusal of one of these bodies is about that member alone.
fn create_body(config_connector_arn: &str) -> Value {
    create_body_with(
        &format!("arn:aws:iam::{ACCOUNT}:role/SsmCloudConnectorRole"),
        config_connector_arn,
    )
}

/// A `CreateCloudConnector` body carrying both ARNs a create is resolved from.
fn create_body_with(role_arn: &str, config_connector_arn: &str) -> Value {
    json!({
        "DisplayName": "Azure production tenant",
        "RoleArn": role_arn,
        "ConfigConnectorArn": config_connector_arn,
        "Configuration": {
            "AzureConfiguration": {
                "TenantId": TENANT_ID,
                "TenantDisplayName": "Contoso",
                "ApplicationId": APPLICATION_ID,
                "ApplicationDisplayName": "Systems Manager connector",
            },
        },
    })
}

/// Refuse `body` under `operation` and return the message, asserting the status and the error
/// code a broken constraint carries.
async fn refusal(case: &str, operation: &str, body: &Value) -> String {
    let router = router();
    let (status, response) = call(&router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a broken constraint is refused with 400, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: the refusal is a ValidationException, got {response}"
    );
    error_message(&response)
}

/// The expression a `@pattern` clause of this slice quotes, over two member shapes, three
/// operations and four spellings of a value that is not the ARN its member models.
#[tokio::test]
async fn a_pattern_clause_quotes_the_expression_the_service_publishes() {
    // (case, operation, body, the clause the refusal states)
    let cases: Vec<(&str, &str, Value, String)> = vec![
        (
            "a Config connector ARN that is not an ARN at all",
            "CreateCloudConnector",
            create_body("not-a-valid-config-connector-arn"),
            clause(
                "not-a-valid-config-connector-arn",
                "configConnectorArn",
                &pattern(CONFIG_CONNECTOR_ARN),
            ),
        ),
        (
            "an ARN of another service in place of the Config connector's",
            "CreateCloudConnector",
            create_body(&format!("arn:aws:ssm:us-west-2:{ACCOUNT}:connector/production")),
            clause(
                &format!("arn:aws:ssm:us-west-2:{ACCOUNT}:connector/production"),
                "configConnectorArn",
                &pattern(CONFIG_CONNECTOR_ARN),
            ),
        ),
        (
            "the right service with no connector resource at all",
            "CreateCloudConnector",
            create_body(&format!("arn:aws:config:us-west-2:{ACCOUNT}:rule/production")),
            clause(
                &format!("arn:aws:config:us-west-2:{ACCOUNT}:rule/production"),
                "configConnectorArn",
                &pattern(CONFIG_CONNECTOR_ARN),
            ),
        ),
        (
            "an account that is not twelve digits",
            "CreateCloudConnector",
            create_body("arn:aws:config:us-west-2:12345:connector/production"),
            clause(
                "arn:aws:config:us-west-2:12345:connector/production",
                "configConnectorArn",
                &pattern(CONFIG_CONNECTOR_ARN),
            ),
        ),
        (
            // The OTHER rendering, on a shape of the same slice: the pair the model wrote
            // around a published expression is dropped.
            "a connector id of the modelled length that is not hexadecimal",
            "GetCloudConnector",
            json!({ "CloudConnectorId": UNHEXED_ID }),
            clause(
                UNHEXED_ID,
                "cloudConnectorId",
                &pattern(CLOUD_CONNECTOR_ID),
            ),
        ),
        (
            // The same shape under another operation: the rendering belongs to the shape.
            "the same connector id on the validate",
            "ValidateCloudConnector",
            json!({ "CloudConnectorId": UNHEXED_ID }),
            clause(
                UNHEXED_ID,
                "cloudConnectorId",
                &pattern(CLOUD_CONNECTOR_ID),
            ),
        ),
        (
            // The IAM role ARN: an ARN of another service is not the role the connector acts
            // under, which is a refusal of the INPUT and not an answer about any role.
            "an ARN of another service in place of the IAM role's",
            "CreateCloudConnector",
            create_body_with(
                "arn:aws:s3:::not-an-iam-role",
                &format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/production"),
            ),
            clause("arn:aws:s3:::not-an-iam-role", "roleArn", &pattern(ROLE_ARN)),
        ),
        (
            "an IAM ARN naming a user rather than a role",
            "CreateCloudConnector",
            create_body_with(
                &format!("arn:aws:iam::{ACCOUNT}:user/SsmCloudConnectorRole"),
                &format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/production"),
            ),
            clause(
                &format!("arn:aws:iam::{ACCOUNT}:user/SsmCloudConnectorRole"),
                "roleArn",
                &pattern(ROLE_ARN),
            ),
        ),
        (
            "a role ARN whose account is not twelve digits",
            "CreateCloudConnector",
            create_body_with(
                "arn:aws:iam::12345:role/SsmCloudConnectorRole",
                &format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/production"),
            ),
            clause(
                "arn:aws:iam::12345:role/SsmCloudConnectorRole",
                "roleArn",
                &pattern(ROLE_ARN),
            ),
        ),
    ];

    for (case, operation, body, clause) in cases {
        let stated = refusal(case, operation, &body).await;
        let expected = detected(&[clause]);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// The ANCHORS are part of what a Config connector ARN's clause states, and nothing else
/// about that member's clauses is: a value past the published ceiling draws the length clause
/// and no expression at all.
#[tokio::test]
async fn a_config_connector_arns_clauses_are_the_constraints_its_value_broke() {
    // A well-formed ARN whose resource name runs one character past the ceiling: it
    // satisfies the expression and breaks only the length.
    let prefix = format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/");
    let overlong = format!(
        "{prefix}{}",
        "c".repeat(CONFIG_CONNECTOR_ARN_MAX + 1 - prefix.chars().count())
    );
    assert_eq!(
        overlong.chars().count(),
        CONFIG_CONNECTOR_ARN_MAX + 1,
        "the overlong case is one character past the ceiling, got {}",
        overlong.chars().count()
    );

    let stated = refusal(
        "a Config connector ARN one character past the ceiling",
        "CreateCloudConnector",
        &create_body(&overlong),
    )
    .await;
    // The clause quotes the value under the ceiling every clause of this service quotes a
    // value back under, so the case is the requirement it states and the one clause it is.
    let requirement =
        format!("Member must have length less than or equal to {CONFIG_CONNECTOR_ARN_MAX}");
    assert!(
        stated.starts_with("1 validation error detected: Value 'arn:aws:config:")
            && stated.ends_with(&format!(
                "at 'configConnectorArn' failed to satisfy constraint: {requirement}"
            )),
        "case a Config connector ARN past the ceiling: expected one clause stating \
         {requirement:?}, got {stated:?}"
    );
    assert!(
        !stated.contains("regular expression pattern"),
        "case a Config connector ARN past the ceiling: the value satisfies the expression, so \
         no clause states it, got {stated:?}"
    );

    let anchored = refusal(
        "a Config connector ARN outside the expression",
        "CreateCloudConnector",
        &create_body("not-a-valid-config-connector-arn"),
    )
    .await;
    assert!(
        anchored.ends_with(CONFIG_CONNECTOR_ARN),
        "case the anchors of a published expression: expected the clause to end with \
         {CONFIG_CONNECTOR_ARN:?}, got {anchored:?}"
    );
}

/// The other direction: every value the expression ADMITS reaches the handler and is served,
/// so quoting the anchored expression never narrowed what the boundary accepts. The rows walk
/// the alternation the expression itself carries — the commercial partition and the two
/// others — and the resource name at its shortest.
#[tokio::test]
async fn a_config_connector_arn_the_expression_admits_is_served() {
    let router = router();

    // (case, the Config connector ARN the create carries)
    let cases: Vec<(&str, String)> = vec![
        (
            "the commercial partition",
            format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/production"),
            ),
        (
            "the China partition, the first branch of the expression's alternation",
            format!("arn:aws-cn:config:cn-north-1:{ACCOUNT}:connector/production"),
        ),
        (
            "the GovCloud partition, its second branch",
            format!("arn:aws-us-gov:config:us-gov-west-1:{ACCOUNT}:connector/production"),
        ),
        (
            "a resource name of exactly one character, which `.+` admits",
            format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/c"),
        ),
    ];

    for (case, config_connector_arn) in cases {
        let body = create_body(&config_connector_arn);
        let (status, response) = call(&router, "CreateCloudConnector", &body).await;
        assert_ne!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: a value the expression admits is not refused at the boundary, got \
             {status} {}",
            error_message(&response)
        );
        assert_eq!(
            status, 200,
            "case {case:?}: the create is served, got {status} {response}"
        );
        let id = response["CloudConnectorId"].as_str().unwrap_or_default();
        assert_eq!(
            id.len(),
            36,
            "case {case:?}: a create reports the 36-character id it minted, got {response}"
        );

        // The create is backed by state: the read reports the ARN the create carried.
        let (read_status, read) = call(
            &router,
            "GetCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: GetCloudConnector {id} must succeed, got {read_status} {read}"
        );
        assert_eq!(
            read["ConfigConnectorArn"].as_str(),
            Some(config_connector_arn.as_str()),
            "case {case:?}: the read reports the ARN the create carried, got {read}"
        );
    }
}

/// The same direction for the IAM role ARN, the expression this engine evaluates in an
/// equivalent spelling: every value it admits reaches the handler and is served, so enforcing
/// the requirement the model states never refuses a role ARN AWS accepts. The rows walk the
/// partitions the expression admits, the path a role name may sit under, every punctuation
/// character of its class, and the run at its shortest.
#[tokio::test]
async fn a_role_arn_the_expression_admits_is_served() {
    let router = router();
    let config_connector_arn = format!("arn:aws:config:us-west-2:{ACCOUNT}:connector/production");

    // (case, the IAM role ARN the create carries)
    let cases: Vec<(&str, String)> = vec![
        (
            "a role at the account's root path",
            format!("arn:aws:iam::{ACCOUNT}:role/SsmCloudConnectorRole"),
        ),
        (
            "a role under a path, which the class's slash admits",
            format!("arn:aws:iam::{ACCOUNT}:role/service-role/SsmCloudConnectorRole"),
        ),
        (
            // `[\w-\/.@+=,]`, the class the model writes for a role name.
            "every punctuation character the class admits",
            format!("arn:aws:iam::{ACCOUNT}:role/a_b.c@d+e=f,g-h"),
        ),
        (
            "a role name of exactly one character, the class's own floor",
            format!("arn:aws:iam::{ACCOUNT}:role/r"),
        ),
        (
            "the GovCloud partition, which the partition run admits",
            format!("arn:aws-us-gov:iam::{ACCOUNT}:role/SsmCloudConnectorRole"),
        ),
    ];

    for (case, role_arn) in cases {
        let body = create_body_with(&role_arn, &config_connector_arn);
        let (status, response) = call(&router, "CreateCloudConnector", &body).await;
        assert_ne!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: a role ARN the expression admits is not refused at the boundary, \
             got {status} {}",
            error_message(&response)
        );
        assert_eq!(
            status, 200,
            "case {case:?}: the create is served, got {status} {response}"
        );
        let id = response["CloudConnectorId"].as_str().unwrap_or_default();

        // The create is backed by state: the read reports the role the create carried.
        let (read_status, read) = call(
            &router,
            "GetCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: GetCloudConnector {id} must succeed, got {read_status} {read}"
        );
        assert_eq!(
            read["RoleArn"].as_str(),
            Some(role_arn.as_str()),
            "case {case:?}: the read reports the role the create carried, got {read}"
        );
    }
}
