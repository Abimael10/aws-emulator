// SPDX-License-Identifier: CC0-1.0

//! The credential secret an Amazon EVS connector is created against and pointed at, end
//! to end over the wire.
//!
//! The rule under test: a connector "establishes a connection to the given appliance type
//! using the credentials stored in Amazon Web Services Secrets Manager" (`Connector` doc),
//! so a `secretIdentifier` is only accepted when those credentials are really there — the
//! account holds a secret that identifier names, and that secret stores the fields the
//! connector's type requires ("SDDC_MANAGER requires an apiKey field; OPERATIONS_MANAGER
//! and VCENTER require username and password fields",
//! `smithy_query(query="doc", shape="CreateEnvironmentConnectorRequest",
//! member="secretIdentifier")`). A request that fails either is refused with the
//! operation's modelled `ValidationException` naming `secretIdentifier`, and nothing is
//! stored for it — no connector ever reaches `ACTIVE` on credentials that are not there.
//!
//! The rows differ along what the rule generalises over: the FORM the secret is named in
//! (a bare name, a full ARN, the partial-ARN form, an ARN of another Region), WHY the
//! credentials are not there (no secret at all, a secret in another Region, a secret
//! marked for deletion, a secret storing the wrong fields, a value that is not a JSON
//! object at all), the connector TYPE whose field requirement applies, and the operation
//! that writes the member (`CreateEnvironmentConnector` and
//! `UpdateEnvironmentConnector`). The secrets the account does hold are registered in its
//! own Secrets Manager store (`environment_lifecycle::register_credential_secret`), which
//! is where both operations resolve the identifier.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager as secretsmanager;

use crate::environment_lifecycle::{
    call, create_settled, emulator, error_type, poll_connector_active, register_credential_secret,
    register_secret_storing, secret_arn_of, secret_region_of, settle, Emulator,
};

/// The appliance every connector here connects to: this suite varies the SECRET
/// (`tests/all/connectors.rs` owns the FQDN class).
const APPLIANCE_FQDN: &str = "vcenter.evs.example.com";

/// A bare name the account holds no secret for.
const UNHELD_NAME: &str = "evs/no-such-credentials";

/// A full secret ARN — a name followed by the hyphen and six characters Secrets Manager
/// appends — the account holds no secret for.
const UNHELD_FULL_ARN: &str =
    "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/does-not-exist-AbCd12";

/// The partial-ARN form (no trailing hyphen and six characters) of a secret the account
/// holds none of.
const UNHELD_PARTIAL_ARN: &str =
    "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/does-not-exist";

/// A secret the account holds in ANOTHER Region: a secret is identified within one Region,
/// so the same name in the Region the request is addressed to names nothing.
const OTHER_REGION_ARN: &str =
    "arn:aws:secretsmanager:eu-central-1:000000000000:secret:evs/elsewhere-ZyXw98";

/// The same name in the Region the test client signs for, which no secret is held at.
const THIS_REGION_ARN: &str =
    "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/elsewhere-ZyXw98";

/// A bare name the account holds a secret at, which is marked for deletion: its value is
/// no longer one a retrieval returns.
const DELETED_SECRET_NAME: &str = "evs/credentials-on-the-way-out";

/// A `CreateEnvironmentConnector` request.
fn create_request(environment_id: &str, connector_type: &str, secret_identifier: &str) -> Value {
    json!({
        "environmentId": environment_id,
        "type": connector_type,
        "applianceFqdn": APPLIANCE_FQDN,
        "secretIdentifier": secret_identifier,
    })
}

/// Mark the secret `secret_identifier` names for deletion, the state `DeleteSecret` leaves
/// a secret in while its recovery window runs.
fn schedule_secret_deletion(engine: &engine::InMemoryEngine, secret_identifier: &str) {
    let state = secretsmanager::state::service_state(engine)
        .expect("the in-memory engine carries the account's Secrets Manager store");
    let now = SystemTime::now();
    let mut store = state.lock();
    let secret = store
        .get_mut(secret_region_of(secret_identifier), secret_identifier)
        .expect("the secret was registered before it was marked for deletion");
    secret.schedule_deletion(now, now, now + std::time::Duration::from_secs(60 * 60 * 24 * 30));
}

/// Assert `body` is the modelled `ValidationException` that names `secretIdentifier`, at
/// the status this awsJson1_0 service answers a client error with.
fn assert_refused_naming_the_secret(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body}"
    );
    let reported = error_type(body);
    assert!(
        reported.ends_with("ValidationException"),
        "case {case:?}: expected a ValidationException, got {reported:?} {body}"
    );
    assert_eq!(
        body["reason"],
        json!("fieldValidationFailed"),
        "case {case:?}: expected reason fieldValidationFailed, got {body}"
    );
    let named: Vec<String> = body["fieldList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|field| field["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        named,
        vec!["secretIdentifier".to_string()],
        "case {case:?}: the refusal must name secretIdentifier, got {body}"
    );
}

/// Every connector of one environment, as `ListEnvironmentConnectors` reports them.
async fn connectors_of(router: &Emulator, case: &str, environment_id: &str) -> Vec<Value> {
    let (status, body) = call(
        router,
        "ListEnvironmentConnectors",
        &json!({ "environmentId": environment_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: list expected 200, got {status} {body}"
    );
    body["connectors"].as_array().cloned().unwrap_or_default()
}

/// Assert the environment holds no connector at all — now, and once every state change the
/// clock owes has been applied, so a refused create cannot leave one that settles to
/// `ACTIVE` later.
async fn assert_no_connector_settles(router: &Emulator, case: &str, environment_id: &str) {
    let listed = connectors_of(router, case, environment_id).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: a refused create must store no connector, got {listed:?}"
    );
    settle(router);
    let settled = connectors_of(router, case, environment_id).await;
    assert!(
        settled.is_empty(),
        "case {case:?}: no connector may appear once the clock has settled, got {settled:?}"
    );
}

#[tokio::test]
async fn a_create_is_refused_for_a_secret_the_account_does_not_hold() {
    let router = emulator();
    // The secrets the account DOES hold for this test: one in another Region, and one on
    // its way out.
    register_credential_secret(&router.engine, OTHER_REGION_ARN);
    register_credential_secret(&router.engine, DELETED_SECRET_NAME);
    schedule_secret_deletion(&router.engine, DELETED_SECRET_NAME);

    // Each row: the case, the `secretIdentifier` the create names, and the connector type
    // it asks for — cycled over the modelled catalog, so the rule is not tested on one
    // type only.
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a bare name the account holds no secret for", UNHELD_NAME, "VCENTER"),
        (
            "a full ARN the account holds no secret for",
            UNHELD_FULL_ARN,
            "SDDC_MANAGER",
        ),
        (
            "a partial ARN the account holds no secret for",
            UNHELD_PARTIAL_ARN,
            "OPERATIONS_MANAGER",
        ),
        (
            "an ARN of this Region for a secret held in another",
            THIS_REGION_ARN,
            "VCENTER",
        ),
        (
            "a secret marked for deletion, whose value no retrieval returns",
            DELETED_SECRET_NAME,
            "SDDC_MANAGER",
        ),
    ];

    for (case, secret_identifier, connector_type) in rows {
        let environment = create_settled(&router, None).await;
        let (status, body) = call(
            &router,
            "CreateEnvironmentConnector",
            &create_request(&environment, connector_type, secret_identifier),
        )
        .await;
        assert_refused_naming_the_secret(case, status, &body);
        // The sentence states the secret the request named, so a client learns which value
        // to correct.
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(secret_identifier),
            "case {case:?}: the refusal must state the secret it refuses, got {message:?}"
        );
        assert_no_connector_settles(&router, case, &environment).await;
    }

    // The mirror of the Region rows: the secret the account really holds is reached by the
    // ARN that names where it lives, so a cross-Region ARN is served.
    let case = "the ARN of the secret in the Region that holds it";
    let environment = create_settled(&router, None).await;
    let (status, body) = call(
        &router,
        "CreateEnvironmentConnector",
        &create_request(&environment, "VCENTER", OTHER_REGION_ARN),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {body}"
    );
    assert_eq!(
        body["connector"]["secretArn"],
        json!(secret_arn_of(OTHER_REGION_ARN)),
        "case {case:?}: the connector must report the secret's own ARN, got {body}"
    );
    // The round trip: the connector was really stored.
    let listed = connectors_of(&router, case, &environment).await;
    assert_eq!(
        listed,
        vec![body["connector"].clone()],
        "case {case:?}: the list must report the created connector, got {listed:?}"
    );
}

/// What one row of the field-requirement table names the secret by, so each row has a
/// secret of its own and no row sees another's.
fn row_secret_name(index: usize) -> String {
    format!("evs/credentials-row-{index}")
}

#[tokio::test]
async fn a_create_is_refused_unless_the_secret_stores_the_fields_its_type_requires() {
    let router = emulator();

    // Each row: the case, the connector type, the value the account's secret stores, and
    // the fields the refusal must name — empty for a row that is SERVED. The catalog of
    // required fields is the `secretIdentifier` doc's: SDDC_MANAGER an apiKey field,
    // OPERATIONS_MANAGER and VCENTER a username and a password field.
    let rows: Vec<(&str, &str, Value, &[&str])> = vec![
        (
            "SDDC_MANAGER against a secret storing apiKey",
            "SDDC_MANAGER",
            json!({ "apiKey": "fixture" }),
            &[],
        ),
        (
            "SDDC_MANAGER against a secret storing a username and a password",
            "SDDC_MANAGER",
            json!({ "username": "fixture", "password": "fixture" }),
            &["apiKey"],
        ),
        (
            "VCENTER against a secret storing a username and a password",
            "VCENTER",
            json!({ "username": "fixture", "password": "fixture" }),
            &[],
        ),
        (
            "VCENTER against a secret storing only a username",
            "VCENTER",
            json!({ "username": "fixture" }),
            &["password"],
        ),
        (
            "VCENTER against a secret storing only a password",
            "VCENTER",
            json!({ "password": "fixture" }),
            &["username"],
        ),
        (
            "OPERATIONS_MANAGER against a secret storing neither field",
            "OPERATIONS_MANAGER",
            json!({}),
            &["username", "password"],
        ),
        (
            "OPERATIONS_MANAGER against a secret storing an apiKey as well",
            "OPERATIONS_MANAGER",
            json!({ "username": "fixture", "password": "fixture", "apiKey": "fixture" }),
            &[],
        ),
        (
            "VCENTER against a secret whose value is not a key/value document",
            "VCENTER",
            json!("a bare string"),
            &["username", "password"],
        ),
    ];

    for (index, (case, connector_type, stored, missing)) in rows.iter().enumerate() {
        let case = *case;
        let secret_name = row_secret_name(index);
        register_secret_storing(&router.engine, &secret_name, stored);
        let environment = create_settled(&router, None).await;
        let (status, body) = call(
            &router,
            "CreateEnvironmentConnector",
            &create_request(&environment, connector_type, &secret_name),
        )
        .await;

        if missing.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {body}"
            );
            assert_eq!(
                body["connector"]["secretArn"],
                json!(secret_arn_of(&secret_name)),
                "case {case:?}: the connector must report the secret's ARN, got {body}"
            );
            // The round trip: a served create really stored the connector.
            let listed = connectors_of(&router, case, &environment).await;
            assert_eq!(
                listed,
                vec![body["connector"].clone()],
                "case {case:?}: the list must report the created connector, got {listed:?}"
            );
            continue;
        }

        assert_refused_naming_the_secret(case, status, &body);
        let message = body["message"].as_str().unwrap_or_default();
        for field in *missing {
            assert!(
                message.contains(field),
                "case {case:?}: the refusal must name the missing {field} field, got \
                 {message:?}"
            );
        }
        assert!(
            message.contains(connector_type),
            "case {case:?}: the refusal must name the type that requires the fields, got \
             {message:?}"
        );
        assert_no_connector_settles(&router, case, &environment).await;
    }
}

#[tokio::test]
async fn an_update_is_refused_for_a_secret_the_connector_cannot_connect_with() {
    let router = emulator();
    // The secret the connector is created against, and one storing only the field the
    // OTHER connector type requires.
    let held = "evs/vcenter-credentials";
    let api_key_only = "evs/api-key-only";
    register_credential_secret(&router.engine, held);
    register_secret_storing(
        &router.engine,
        api_key_only,
        &json!({ "apiKey": "fixture" }),
    );

    let environment = create_settled(&router, None).await;
    let (status, body) = call(
        &router,
        "CreateEnvironmentConnector",
        &create_request(&environment, "VCENTER", held),
    )
    .await;
    assert_eq!(status, 200, "the create expected 200, got {status} {body}");
    let connector_id = body["connector"]["connectorId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    settle(&router);
    let before = connectors_of(&router, "the created connector", &environment).await;

    // Each row: the case, and the `secretIdentifier` the update names. Every one of them
    // is a secret the connector cannot connect with, so the stored record must be left
    // exactly as it was.
    let rows: Vec<(&str, &str)> = vec![
        ("a bare name the account holds no secret for", UNHELD_NAME),
        (
            "a full ARN the account holds no secret for",
            UNHELD_FULL_ARN,
        ),
        (
            "a secret that does not store the fields this connector's type requires",
            api_key_only,
        ),
    ];

    for (case, secret_identifier) in rows {
        let (status, body) = call(
            &router,
            "UpdateEnvironmentConnector",
            &json!({
                "environmentId": environment,
                "connectorId": connector_id,
                "secretIdentifier": secret_identifier,
            }),
        )
        .await;
        assert_refused_naming_the_secret(case, status, &body);
        let after = connectors_of(&router, case, &environment).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must leave the connector as it was, got \
             {after:?}"
        );
    }

    // And the update IS served for a secret the connector can connect with: the member
    // takes the new value and the connector reconnects.
    let case = "another secret storing the fields the type requires";
    let other = "evs/vcenter-credentials-rotated";
    register_credential_secret(&router.engine, other);
    let (status, body) = call(
        &router,
        "UpdateEnvironmentConnector",
        &json!({
            "environmentId": environment,
            "connectorId": connector_id,
            "secretIdentifier": other,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: update expected 200, got {status} {body}"
    );
    assert_eq!(
        body["connector"]["secretArn"],
        json!(secret_arn_of(other)),
        "case {case:?}: the connector must report the new secret's ARN, got {body}"
    );
    assert_eq!(
        body["connector"]["state"],
        json!("UPDATING"),
        "case {case:?}: a reconfigured connector is UPDATING, got {body}"
    );
    let listed = connectors_of(&router, case, &environment).await;
    assert_eq!(
        listed,
        vec![body["connector"].clone()],
        "case {case:?}: the list must report the updated connector, got {listed:?}"
    );
}

#[tokio::test]
async fn a_secret_of_the_account_is_resolved_wherever_the_identifier_names_it() {
    // Every form one secret is named by resolves to the secret the account holds: what the
    // rule is about is the secret, not the spelling.
    let router = emulator();
    // One secret registered by a bare name, which carries the ARN of that name, and one
    // registered by the full ARN Secrets Manager mints — a name followed by a hyphen and
    // six characters.
    let name = "evs/one-secret";
    let full_arn = "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/minted-WvUt76";
    let partial_arn = "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/minted";
    register_credential_secret(&router.engine, name);
    register_credential_secret(&router.engine, full_arn);

    // Each row: the case, and the form the create names a held secret in.
    let rows: Vec<(&str, String)> = vec![
        ("the bare name of a secret held under it", name.to_string()),
        (
            "the ARN that secret carries",
            secret_arn_of(name),
        ),
        ("the full ARN of a secret held under it", full_arn.to_string()),
        (
            "the partial-ARN form of that secret",
            partial_arn.to_string(),
        ),
    ];

    for (case, secret_identifier) in rows {
        let environment = create_settled(&router, None).await;
        let (status, body) = call(
            &router,
            "CreateEnvironmentConnector",
            &create_request(&environment, "VCENTER", &secret_identifier),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body}"
        );
        assert_eq!(
            body["connector"]["secretArn"],
            json!(secret_arn_of(&secret_identifier)),
            "case {case:?}: the connector must report the secret's ARN, got {body}"
        );
    }
}

/// Remove the secret `secret_identifier` names from the account's store altogether — the
/// state a secret deleted without recovery leaves behind, where not even a record of it is
/// left.
fn forget_secret(engine: &engine::InMemoryEngine, secret_identifier: &str) {
    let state = secretsmanager::state::service_state(engine)
        .expect("the in-memory engine carries the account's Secrets Manager store");
    let mut store = state.lock();
    let key = store
        .resolve(secret_region_of(secret_identifier), secret_identifier)
        .expect("the secret was registered before it was removed");
    store
        .remove(&key)
        .expect("the key resolved above names a secret the store holds");
}

/// What became of the credential secret in Secrets Manager AFTER the connector was
/// created against it and already carries it.
///
/// Each variant is a reason the precondition on a NEW secret would not hold any more: the
/// account no longer holds usable credentials under that identifier, or — [`Afterwards::
/// Held`] — it still does, which is the plain repeat.
#[derive(Clone, Copy, Debug)]
enum Afterwards {
    /// Still held, still storing the fields the connector's type requires.
    Held,
    /// Marked for deletion, the state `DeleteSecret` leaves a secret in: its value is no
    /// longer one a retrieval returns.
    MarkedForDeletion,
    /// Gone from the account's Secrets Manager store.
    Removed,
    /// Still held, but its value rotated to one that no longer states the fields an
    /// `OPERATIONS_MANAGER` or `VCENTER` connector requires.
    RotatedToOtherFields,
}

/// Put the secret `secret_identifier` names into the state `afterwards` describes.
fn become_afterwards(
    engine: &engine::InMemoryEngine,
    secret_identifier: &str,
    afterwards: Afterwards,
) {
    match afterwards {
        Afterwards::Held => {}
        Afterwards::MarkedForDeletion => schedule_secret_deletion(engine, secret_identifier),
        Afterwards::Removed => forget_secret(engine, secret_identifier),
        Afterwards::RotatedToOtherFields => {
            register_secret_storing(engine, secret_identifier, &json!({ "apiKey": "fixture" }));
        }
    }
}

#[tokio::test]
async fn an_update_naming_the_secret_the_connector_already_carries_reconfigures_nothing() {
    let router = emulator();

    // The precondition is on the secret a call really points a connector AT: a request
    // naming the secret the stored connector already carries re-establishes no connection,
    // so it needs no credentials for one — exactly as the sibling create's idempotent
    // repeat needs none — and the stored record is reported untouched.
    //
    // Each row: the case, the connector type, the identifier the connector is created
    // against, the form the repeat names that SAME secret in, and what became of the
    // secret in between. The rows differ along what the rule generalises over: the
    // connector TYPE whose field requirement would apply, the FORM the secret is named in
    // (a bare name, the ARN that name resolves to, the full ARN Secrets Manager minted, an
    // ARN of another Region), and WHY the credentials would now fail the precondition a
    // new secret has to pass (nothing at all, marked for deletion, gone from the store,
    // rotated to other fields).
    let rows: Vec<(&str, &str, String, String, Afterwards)> = vec![
        (
            "the bare name of a secret the account still holds",
            "VCENTER",
            "evs/carried-held".to_string(),
            "evs/carried-held".to_string(),
            Afterwards::Held,
        ),
        (
            "the bare name of a secret since marked for deletion",
            "SDDC_MANAGER",
            "evs/carried-on-the-way-out".to_string(),
            "evs/carried-on-the-way-out".to_string(),
            Afterwards::MarkedForDeletion,
        ),
        (
            "the ARN of a secret since removed from the account",
            "OPERATIONS_MANAGER",
            "evs/carried-removed".to_string(),
            secret_arn_of("evs/carried-removed"),
            Afterwards::Removed,
        ),
        (
            "the full ARN of a secret since rotated to other fields",
            "VCENTER",
            "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/carried-minted-AbCd12"
                .to_string(),
            "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/carried-minted-AbCd12"
                .to_string(),
            Afterwards::RotatedToOtherFields,
        ),
        (
            "an ARN of another Region, whose secret is since marked for deletion",
            "OPERATIONS_MANAGER",
            "arn:aws:secretsmanager:eu-central-1:000000000000:secret:evs/carried-elsewhere-ZyXw98"
                .to_string(),
            "arn:aws:secretsmanager:eu-central-1:000000000000:secret:evs/carried-elsewhere-ZyXw98"
                .to_string(),
            Afterwards::MarkedForDeletion,
        ),
    ];

    for (case, connector_type, created_identifier, repeat_identifier, afterwards) in rows {
        register_credential_secret(&router.engine, &created_identifier);
        let environment = create_settled(&router, None).await;
        let (status, body) = call(
            &router,
            "CreateEnvironmentConnector",
            &create_request(&environment, connector_type, &created_identifier),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create expected 200, got {status} {body}"
        );
        let connector_id = body["connector"]["connectorId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // The connector carries those credentials once it has connected with them: the
        // test waits for the connector to BE ACTIVE, not for a span of wall clock.
        settle(&router);
        let before = poll_connector_active(&router, case, &environment, &connector_id).await;

        // What became of the secret the stored connector points at.
        become_afterwards(&router.engine, &created_identifier, afterwards);

        // The sibling create, repeated: the stored record, with no credentials asked for.
        let (status, body) = call(
            &router,
            "CreateEnvironmentConnector",
            &create_request(&environment, connector_type, &repeat_identifier),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the repeated create expected 200, got {status} {body}"
        );
        assert_eq!(
            body["connector"], before,
            "case {case:?}: the repeated create must report the stored connector, got {body}"
        );

        // And the update naming that same secret, which reconfigures nothing either: the
        // record keeps its state and its modifiedAt, so the answer is the stored record.
        let (status, body) = call(
            &router,
            "UpdateEnvironmentConnector",
            &json!({
                "environmentId": environment,
                "connectorId": connector_id,
                "secretIdentifier": repeat_identifier,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the no-change update expected 200, got {status} {body}"
        );
        assert_eq!(
            body["connector"], before,
            "case {case:?}: an update naming the secret the connector already carries must \
             change nothing, got {body}"
        );
        let listed = connectors_of(&router, case, &environment).await;
        assert_eq!(
            listed,
            vec![before.clone()],
            "case {case:?}: the stored record must be left exactly as it was, got {listed:?}"
        );

        // The precondition still holds where the call really points the connector at
        // ANOTHER secret: one the account holds none of is refused and writes nothing.
        let (status, body) = call(
            &router,
            "UpdateEnvironmentConnector",
            &json!({
                "environmentId": environment,
                "connectorId": connector_id,
                "secretIdentifier": UNHELD_NAME,
            }),
        )
        .await;
        assert_refused_naming_the_secret(case, status, &body);
        let listed = connectors_of(&router, case, &environment).await;
        assert_eq!(
            listed,
            vec![before.clone()],
            "case {case:?}: a refused update must leave the connector as it was, got {listed:?}"
        );
    }
}
