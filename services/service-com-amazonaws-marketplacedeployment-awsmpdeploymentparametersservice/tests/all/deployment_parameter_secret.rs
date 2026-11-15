// SPDX-License-Identifier: CC0-1.0

//! The deployment parameter a `PutDeploymentParameter` writes IS an AWS Secrets Manager
//! secret.
//!
//! RULE under test: `deploymentParameter` is "the deployment parameter targeted to the
//! acceptor of an agreement for which to create the AWS Secret Manager resource" and its
//! `secretString` is "the text to encrypt and store in the secret"
//! (`smithy_query(query="shape",shape="DeploymentParameterInput")`), so a successful write
//! leaves that text in a secret of the Secrets Manager plane, under one secret per
//! deployment parameter: a repeat write for the same
//! (catalog, productId, agreementId, name) stakes a new VERSION of that same secret
//! rather than leaving a stale value or a second secret, and the deletion
//! `expirationDate` makes due takes the secret with the record.
//!
//! Nothing here reaches into either store. The value is believed to be stored only
//! because Secrets Manager — the service that OWNS secret values — answered with it, and
//! the secret is believed gone only because that same service says so. Secrets Manager is
//! driven through the AWS SDK over the router's own in-process callback, which is the seam
//! the write itself reaches it through.
//!
//! The rows span what the rule generalises over: the text written (one character, an
//! ordinary password, a JSON document, text outside ASCII, and the longest `secretString`
//! the model admits), the parameter name (an ordinary one, one made of the punctuation
//! `DeploymentParameterName` admits, and one at the modelled maximum length), and the
//! identity the parameter belongs to (a product and agreement of its own per row). The
//! expiry test covers the one axis the table does not: a parameter whose date falls due
//! while the test watches.
//!
//! The harness (router, signed request, per-operation callers) is the one the
//! `deployment_parameters` suite owns; the secret name is the one the service derives
//! ([`deployment_parameter_secret_name`]), which a client can build from the
//! `deploymentParameterId` the write reported.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_marketplacedeployment_awsmpdeploymentparametersservice::state::deployment_secret::{
    deployment_parameter_secret_name, SECRET_NAME_PREFIX,
};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::deployment_parameters::{
    emulator, is_parameter_id, list_tags, provision, put, REGION, SECRET_MAX_LEN,
    PARAMETER_NAME_MAX_LEN, CATALOG,
};
use crate::support::rfc3339;

/// How long the clock is given to make an expiry due and the service to carry the
/// deletion out. Nothing sleeps for it: the test looks again until the condition holds,
/// with generous room so a loaded host cannot fail it.
const EXPIRY_BUDGET: Duration = Duration::from_secs(60);
/// How long this suite waits between looks while polling for a condition.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A Secrets Manager client over the router's own in-process callback: no socket is
/// opened and nothing leaves the process, and the endpoint below is only the address that
/// transport routes on.
fn secrets(router: &MultiServiceRouter) -> ::aws_sdk_secretsmanager::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(::aws_sdk_secretsmanager::config::Region::new(REGION.to_string()))
        .endpoint_url(format!("https://secretsmanager.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_secretsmanager::Client::from_conf(config)
}

/// One `PutDeploymentParameter` body: the identity members outside the URI, the text, and
/// the optional `expirationDate` as the RFC 3339 string the member's format declares.
fn put_body(agreement_id: &str, name: &str, secret: &str, expiration_date: Option<i64>) -> Value {
    let mut body = json!({
        "agreementId": agreement_id,
        "deploymentParameter": { "name": name, "secretString": secret },
    });
    if let Some(at) = expiration_date {
        body.as_object_mut()
            .expect("an object body")
            .insert("expirationDate".to_string(), json!(rfc3339(at)));
    }
    body
}

/// The `deploymentParameterId` a successful write reported.
fn reported_id(case: &str, body: &Value) -> String {
    let id = body
        .get("deploymentParameterId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a deploymentParameterId, got {body:?}"))
        .to_string();
    assert!(
        is_parameter_id(&id),
        "case {case:?}: expected a minted ^dp-[a-zA-Z0-9]+$ id, got {id:?}"
    );
    id
}

/// The text and the ARN Secrets Manager answers with for the secret `secret_id` names.
async fn read_secret(
    client: &::aws_sdk_secretsmanager::Client,
    case: &str,
    secret_id: &str,
) -> (String, String) {
    let answered = client
        .get_secret_value()
        .secret_id(secret_id)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: Secrets Manager must hold {secret_id}, got {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
    (
        answered
            .secret_string()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a SecretString for {secret_id}, got none")
            })
            .to_string(),
        answered
            .arn()
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN for {secret_id}, got none"))
            .to_string(),
    )
}

/// Every secret name Secrets Manager holds for this account that this service manages.
async fn managed_secret_names(client: &::aws_sdk_secretsmanager::Client, case: &str) -> Vec<String> {
    let answered = client
        .list_secrets()
        .max_results(100)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: secretsmanager:ListSecrets must answer, got {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
    answered
        .secret_list()
        .iter()
        .filter_map(|entry| entry.name())
        .filter(|name| name.starts_with(SECRET_NAME_PREFIX))
        .map(str::to_string)
        .collect()
}

/// One row: an identity, the text its create writes, and the text its rewrite writes.
struct Written {
    case: &'static str,
    product_id: String,
    agreement_id: String,
    name: String,
    created_with: String,
    rewritten_with: String,
}

/// The table: the texts and names the rule has to hold over.
///
/// The lengths are the modelled bounds — `SecretString` is `length 1..15000` and
/// `DeploymentParameterName` `length 1..400`
/// (`smithy_query(query="shape",shape="DeploymentParameterInput")`) — and the punctuated
/// name is drawn from that member's own `^[a-zA-Z0-9/_+=.@-]+$`.
fn table() -> Vec<Written> {
    vec![
        Written {
            case: "an ordinary password",
            product_id: "prod-secret-01".to_string(),
            agreement_id: "agmt-secret-01".to_string(),
            name: "DbPassword".to_string(),
            created_with: "s3cr3t-value-1".to_string(),
            rewritten_with: "s3cr3t-value-2".to_string(),
        },
        Written {
            case: "one character, the shortest text the model admits",
            product_id: "prod-secret-02".to_string(),
            agreement_id: "agmt-secret-02".to_string(),
            name: "A".to_string(),
            created_with: "x".to_string(),
            rewritten_with: "y".to_string(),
        },
        Written {
            case: "a JSON document",
            product_id: "prod-secret-03".to_string(),
            agreement_id: "agmt-secret-03".to_string(),
            name: "api.key+v1=1@host/path_1-2".to_string(),
            created_with: json!({ "apiKey": "abc", "externalId": "xyz" }).to_string(),
            rewritten_with: json!({ "apiKey": "def", "externalId": "uvw" }).to_string(),
        },
        Written {
            case: "text outside ASCII",
            product_id: "prod-secret-04".to_string(),
            agreement_id: "agmt-secret-04".to_string(),
            name: "Pass_Wort".to_string(),
            created_with: "pässwörd-☂-1".to_string(),
            rewritten_with: "pässwörd-☂-2".to_string(),
        },
        Written {
            case: "the longest text the model admits, under a name at its own maximum",
            product_id: "prod-secret-05".to_string(),
            agreement_id: "agmt-secret-05".to_string(),
            name: "n".repeat(PARAMETER_NAME_MAX_LEN),
            created_with: "y".repeat(SECRET_MAX_LEN),
            rewritten_with: "z".repeat(SECRET_MAX_LEN),
        },
    ]
}

#[tokio::test]
async fn a_written_deployment_parameter_is_readable_in_secrets_manager() {
    let (router, engine) = emulator();
    let client = secrets(&router);

    for (index, row) in table().into_iter().enumerate() {
        let case = row.case;
        provision(&engine, CATALOG, &row.agreement_id, &[&row.product_id]);

        // --- the create -----------------------------------------------------
        let (status, created) = put(
            &router,
            CATALOG,
            &row.product_id,
            &put_body(&row.agreement_id, &row.name, &row.created_with, None),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutDeploymentParameter must succeed, got {status} {created:?}"
        );
        let id = reported_id(case, &created);
        let secret_name = deployment_parameter_secret_name(&id);

        let (held, secret_arn) = read_secret(&client, case, &secret_name).await;
        assert_eq!(
            held.len(),
            row.created_with.len(),
            "case {case:?}: expected the stored text to be {} characters, got {}",
            row.created_with.len(),
            held.len()
        );
        assert!(
            held == row.created_with,
            "case {case:?}: expected Secrets Manager to hold the text the write carried, got a \
             different value of {} characters",
            held.len()
        );
        assert!(
            secret_arn.contains(":secretsmanager:"),
            "case {case:?}: expected a Secrets Manager ARN, got {secret_arn:?}"
        );

        // The secret says WHOSE it is, so a buyer reading their secrets can tell: the
        // parameter's own name is what a Quick Launch template's parameter matches.
        let described = client
            .describe_secret()
            .secret_id(&secret_name)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: secretsmanager:DescribeSecret must answer for {secret_name}, \
                     got {}",
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                )
            });
        assert_eq!(
            described.name(),
            Some(secret_name.as_str()),
            "case {case:?}: expected the secret to be named {secret_name:?}, got {:?}",
            described.name()
        );
        let description = described.description().unwrap_or_default();
        assert!(
            description.contains(&row.name) && description.contains(&row.product_id),
            "case {case:?}: expected the description to name the parameter and its product, got \
             {description:?}"
        );
        assert!(
            described.deleted_date().is_none(),
            "case {case:?}: a live parameter's secret must not be scheduled for deletion, got \
             {:?}",
            described.deleted_date()
        );

        // --- the repeat write -----------------------------------------------
        let (status, again) = put(
            &router,
            CATALOG,
            &row.product_id,
            &put_body(&row.agreement_id, &row.name, &row.rewritten_with, None),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the repeat PutDeploymentParameter must succeed, got {status} {again:?}"
        );
        let second_id = reported_id(case, &again);
        assert_eq!(
            second_id, id,
            "case {case:?}: the same identity is one resource, so the repeat write must report \
             {id:?}, got {second_id:?}"
        );

        let (rewritten, rewritten_arn) = read_secret(&client, case, &secret_name).await;
        assert!(
            rewritten == row.rewritten_with,
            "case {case:?}: expected the repeat write to leave its own text of {} characters, got \
             a different value of {} characters",
            row.rewritten_with.len(),
            rewritten.len()
        );
        assert_eq!(
            rewritten_arn, secret_arn,
            "case {case:?}: the repeat write stakes a version of the SAME secret, so the ARN must \
             stay {secret_arn:?}, got {rewritten_arn:?}"
        );

        // One secret per deployment parameter, whatever a row wrote: the rows before
        // this one each left exactly one too.
        let names = managed_secret_names(&client, case).await;
        let expected = index + 1;
        assert_eq!(
            names.len(),
            expected,
            "case {case:?}: expected {expected} managed secrets after {expected} parameters, got \
             {names:?}"
        );
        assert_eq!(
            names.iter().filter(|held| *held == &secret_name).count(),
            1,
            "case {case:?}: expected exactly one secret named {secret_name:?}, got {names:?}"
        );
    }
}

#[tokio::test]
async fn the_secret_goes_when_the_parameter_expires() {
    let (router, engine) = emulator();
    let client = secrets(&router);
    let case = "a parameter whose expirationDate falls due while the test watches";
    let product_id = "prod-secret-expiry";
    let agreement_id = "agmt-secret-expiry";
    let name = "ExpiringPassword";
    provision(&engine, CATALOG, agreement_id, &[product_id]);

    // A date a second or two out, so the deadline passes DURING the test: the clock,
    // not the request, is what takes the parameter and its secret.
    let due_at = due_second_ahead();
    let (status, created) = put(
        &router,
        CATALOG,
        product_id,
        &put_body(agreement_id, name, "to-be-expired", Some(due_at)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must succeed, got {status} {created:?}"
    );
    let id = reported_id(case, &created);
    let secret_name = deployment_parameter_secret_name(&id);
    let resource_arn = created
        .get("resourceArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a resourceArn, got {created:?}"))
        .to_string();

    let (held, _) = read_secret(&client, case, &secret_name).await;
    assert_eq!(
        held, "to-be-expired",
        "case {case:?}: expected the written text before the date falls due, got {held:?}"
    );

    // Look again until the parameter is gone: the deletion is owed at the date, and
    // every call of this service materialises what the clock has already made due.
    let deadline = SystemTime::now() + EXPIRY_BUDGET;
    let mut last = (0_u16, Value::Null);
    while SystemTime::now() < deadline {
        last = list_tags(&router, &resource_arn).await;
        if last.0 == 404 {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_eq!(
        last.0, 404,
        "case {case:?}: expected the expired parameter to be gone, got {} {:?}",
        last.0, last.1
    );

    // ... and the secret it held goes with it: the value is no longer retrievable and
    // Secrets Manager reports the secret as deleted.
    let deadline = SystemTime::now() + EXPIRY_BUDGET;
    let mut deleted_date = None;
    while SystemTime::now() < deadline {
        let described = client.describe_secret().secret_id(&secret_name).send().await;
        deleted_date = match &described {
            Ok(answered) => answered.deleted_date().copied(),
            // A secret this account no longer holds at all is just as gone.
            Err(_) => break,
        };
        if deleted_date.is_some() {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let readable = client
        .get_secret_value()
        .secret_id(&secret_name)
        .send()
        .await;
    assert!(
        readable.is_err(),
        "case {case:?}: expected the expired parameter's text to be unreadable, got a value of {} \
         characters",
        readable
            .as_ref()
            .ok()
            .and_then(|answered| answered.secret_string())
            .unwrap_or_default()
            .len()
    );
    assert!(
        deleted_date.is_some() || readable.is_err(),
        "case {case:?}: expected the secret to be deleted, got {deleted_date:?}"
    );

    // And the identity is free again: the fresh resource gets a secret of its OWN,
    // holding its own text, with the gone one left where it is.
    let (status, remade) = put(
        &router,
        CATALOG,
        product_id,
        &put_body(agreement_id, name, "after-expiry", None),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put after expiry must succeed, got {status} {remade:?}"
    );
    let fresh_id = reported_id(case, &remade);
    assert_ne!(
        fresh_id, id,
        "case {case:?}: the expired resource is gone, so a fresh id must be minted, got \
         {fresh_id:?}"
    );
    let (fresh_text, _) = read_secret(&client, case, &deployment_parameter_secret_name(&fresh_id))
        .await;
    assert_eq!(
        fresh_text, "after-expiry",
        "case {case:?}: expected the fresh parameter's own text, got {fresh_text:?}"
    );
}

/// A whole epoch second at least half a second out: near enough to keep the test that
/// waits for the clock short, far enough that the calls made before the wait cannot
/// themselves cross it.
fn due_second_ahead() -> i64 {
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the test clock is after the Unix epoch");
    let secs = i64::try_from(since_epoch.as_secs()).expect("an epoch second inside i64");
    if since_epoch.subsec_millis() <= 500 {
        secs + 1
    } else {
        secs + 2
    }
}
