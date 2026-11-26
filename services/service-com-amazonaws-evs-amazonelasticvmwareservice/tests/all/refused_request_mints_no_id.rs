// SPDX-License-Identifier: CC0-1.0

//! What a REFUSED request does to the ids this service hands out, over the wire.
//!
//! The rule under test: minting an id is a change to the store, and a request this service
//! refuses leaves the store exactly as it was — so a create draws an id only once every
//! precondition of the request has passed, and a refused call consumes none. Every id this
//! service mints (`environmentId`, `connectorId`, the fallback VLAN subnet id, an Elastic
//! IP association id) is drawn from ONE counter of the store, so a refused request that
//! consumed an id would change the id the next create of ANY of those resources reports —
//! a resource that was never created cannot have taken one. `CreateEnvironment` states the
//! rule for its own minting, and reads the account's Amazon EC2 subnet room before it
//! mints anything so it can keep to it.
//!
//! The counter is read through the store's own
//! [`evs::state::ComAmazonawsEvsAmazonelasticvmwareserviceState::minted_id_count`], which
//! is what every mint draws from, so what is asserted is the store's state and not the
//! shape of the ids themselves.
//!
//! The rows differ along what the rule generalises over: WHICH refusal the request runs
//! into (a resource that is not there, a Region that holds no such resource, a resource
//! that has reached a terminal state, an identity another record already occupies, a
//! precondition read from another service's store), the operation that is refused
//! (`CreateEnvironmentConnector`, `CreateEnvironment`, `AssociateEipToVlan` — every
//! operation of this service that mints an id), the modelled error the refusal carries,
//! and the connector type and secret form the request names. The mirror of each table is
//! the served call beside it: a create that IS served consumes exactly the ids of the
//! resources it created.

use serde_json::{json, Value};
use service_com_amazonaws_evs_amazonelasticvmwareservice as evs;

use crate::environment_lifecycle::{
    call, call_in_region, create_settled, emulator, error_type, register_secret_storing, settle,
    Emulator, REGION,
};
use crate::hcx_eip_associations::{create_public_hcx, register_elastic_ip};

/// An `environmentId` of the modelled `^(env-[a-zA-Z0-9]{10})$` form that no environment
/// was ever minted for.
const ABSENT_ENVIRONMENT: &str = "env-0000000000";

/// An allocation id of the form `allocationId` takes that the account holds no Elastic IP
/// allocation for.
const ABSENT_ALLOCATION: &str = "eipalloc-0123456789abcdef0";

/// A Region other than the one the fixtures are created in: an environment exists at the
/// endpoint of the Region it was created in, so this endpoint holds none of them.
const OTHER_REGION: &str = "us-east-1";

/// A credential secret the account holds, named as a full Secrets Manager ARN — one of the
/// identifiers `environment_lifecycle::emulator` registers.
const HELD_SECRET_ARN: &str = "arn:aws:secretsmanager:us-west-2:000000000000:secret:evs/vcf-AbCd12";

/// A credential secret the account holds, named as a bare name — the other form
/// `secretIdentifier` accepts.
const HELD_SECRET_NAME: &str = "evs/vcf-appliance-credentials";

/// A bare name the account holds no secret at all for.
const UNHELD_SECRET_NAME: &str = "evs/no-such-credentials";

/// A bare name the account holds a secret at that stores none of the fields any connector
/// type requires.
const FIELDLESS_SECRET_NAME: &str = "evs/credentials-without-fields";

/// The appliance the fixture connector of a row carries.
const STORED_FQDN: &str = "vcenter.evs.example.com";

/// The appliance the refused create names: another one, so a create for a type the
/// environment already holds a connector of is a conflict rather than the same create
/// again.
const REQUEST_FQDN: &str = "vcenter-2.evs.example.com";

/// How many ids this service's store has minted so far — the counter every mint draws
/// from.
fn minted_ids(router: &Emulator) -> u64 {
    let state = evs::state::service_state(&*router.engine)
        .expect("the in-memory engine carries this service's store");
    let store = state.lock();
    store.minted_id_count()
}

/// A `CreateEnvironmentConnector` request.
fn create_connector_request(
    environment_id: &str,
    connector_type: &str,
    appliance_fqdn: &str,
    secret_identifier: &str,
) -> Value {
    json!({
        "environmentId": environment_id,
        "type": connector_type,
        "applianceFqdn": appliance_fqdn,
        "secretIdentifier": secret_identifier,
    })
}

/// Create one connector and return the `connectorId` the create minted.
async fn create_connector(
    router: &Emulator,
    case: &str,
    environment_id: &str,
    connector_type: &str,
) -> String {
    let (status, body) = call(
        router,
        "CreateEnvironmentConnector",
        &create_connector_request(
            environment_id,
            connector_type,
            STORED_FQDN,
            HELD_SECRET_ARN,
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture create expected 200, got {status} {body}"
    );
    body["connector"]["connectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no connectorId in {body}"))
        .to_string()
}

/// The short name of the modelled error an error body reports.
fn reported_error(body: &Value) -> String {
    error_type(body)
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Assert one refusal: the HTTP 400 every modelled CLIENT error of this awsJson1_0 service
/// is reported with (`tests/all/error_wire_status.rs` owns that rule), carrying the
/// modelled error expected.
fn assert_refused(case: &str, expected_error: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} ({}) {body}",
        error_type(body)
    );
    assert_eq!(
        reported_error(body),
        expected_error,
        "case {case:?}: expected a {expected_error}, got {} {body}",
        error_type(body)
    );
}

/// What makes the connector create of a row refusable.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// An `environmentId` nothing was ever minted for.
    UnknownEnvironment,
    /// An environment of another Region, addressed from this one.
    EnvironmentOfAnotherRegion,
    /// An environment whose deletion has come due.
    DeletedEnvironment,
    /// A connector type a live connector of the environment already holds.
    TypeHeldByALiveConnector,
    /// A connector type whose connector is still being torn down.
    TypeHeldWhileTornDown,
    /// A credential secret the account does not hold.
    SecretTheAccountDoesNotHold,
    /// A credential secret that stores none of the fields the type requires.
    SecretWithoutTheRequiredFields,
}

/// Delete an environment and let the deletion the clock owes it come due, so the record
/// has reached `DELETED` when this returns.
async fn delete_and_settle(router: &Emulator, case: &str, environment_id: &str) {
    let (status, body) = call(
        router,
        "DeleteEnvironment",
        &json!({ "environmentId": environment_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the environment delete expected 200, got {status} {body}"
    );
    settle(router);
}

#[tokio::test]
async fn a_refused_connector_create_consumes_no_id() {
    let router = emulator();
    // The account holds this secret, and it stores none of the fields a connector type
    // requires: the row that names it is refused on what the secret STORES rather than on
    // whether it is there.
    register_secret_storing(&router.engine, FIELDLESS_SECRET_NAME, &json!({}));

    // Each row: the case, what makes the create refusable, the connector type it asks
    // for, the `secretIdentifier` it names, and the modelled error the refusal carries.
    let rows: Vec<(&str, Refusal, &str, &str, &str)> = vec![
        (
            "an environmentId no environment was ever minted for",
            Refusal::UnknownEnvironment,
            "VCENTER",
            HELD_SECRET_ARN,
            "ResourceNotFoundException",
        ),
        (
            "an environment of another Region",
            Refusal::EnvironmentOfAnotherRegion,
            "SDDC_MANAGER",
            HELD_SECRET_NAME,
            "ResourceNotFoundException",
        ),
        (
            "an environment that has reached DELETED",
            Refusal::DeletedEnvironment,
            "OPERATIONS_MANAGER",
            HELD_SECRET_ARN,
            "ResourceNotFoundException",
        ),
        (
            "a type a live connector of the environment already holds",
            Refusal::TypeHeldByALiveConnector,
            "VCENTER",
            HELD_SECRET_ARN,
            "ValidationException",
        ),
        (
            "a type whose connector is still being torn down",
            Refusal::TypeHeldWhileTornDown,
            "SDDC_MANAGER",
            HELD_SECRET_NAME,
            "ValidationException",
        ),
        (
            "a credential secret the account does not hold",
            Refusal::SecretTheAccountDoesNotHold,
            "OPERATIONS_MANAGER",
            UNHELD_SECRET_NAME,
            "ValidationException",
        ),
        (
            "a credential secret storing none of the fields the type requires",
            Refusal::SecretWithoutTheRequiredFields,
            "VCENTER",
            FIELDLESS_SECRET_NAME,
            "ValidationException",
        ),
    ];
    // Every type the model declares is asked for by some row, so the table and the
    // modelled catalog (`smithy_query(query="shape", shape="ConnectorType")`, generated
    // into this crate) cannot drift apart.
    for connector_type in evs::ops::types::CONNECTOR_TYPE_VALUES {
        assert!(
            rows.iter().any(|(_, _, row_type, ..)| row_type == connector_type),
            "the table must cover the modelled connector type {connector_type:?}"
        );
    }

    for (case, refusal, connector_type, secret_identifier, expected_error) in rows {
        // The fixture the row's create is made against, and whatever makes that create
        // refusable — all of it before the count is taken, so what the count measures is
        // the refused call alone.
        let mut region = REGION;
        let environment = match refusal {
            Refusal::UnknownEnvironment => ABSENT_ENVIRONMENT.to_string(),
            Refusal::EnvironmentOfAnotherRegion => {
                region = OTHER_REGION;
                create_settled(&router, None).await
            }
            Refusal::DeletedEnvironment => {
                let environment = create_settled(&router, None).await;
                delete_and_settle(&router, case, &environment).await;
                environment
            }
            Refusal::TypeHeldByALiveConnector => {
                let environment = create_settled(&router, None).await;
                create_connector(&router, case, &environment, connector_type).await;
                settle(&router);
                environment
            }
            Refusal::TypeHeldWhileTornDown => {
                let environment = create_settled(&router, None).await;
                let connector_id =
                    create_connector(&router, case, &environment, connector_type).await;
                settle(&router);
                let (status, body) = call(
                    &router,
                    "DeleteEnvironmentConnector",
                    &json!({ "environmentId": environment, "connectorId": connector_id }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the connector delete expected 200, got {status} {body}"
                );
                environment
            }
            Refusal::SecretTheAccountDoesNotHold
            | Refusal::SecretWithoutTheRequiredFields => create_settled(&router, None).await,
        };

        let before = minted_ids(&router);
        let (status, body) = call_in_region(
            &router,
            region,
            "CreateEnvironmentConnector",
            &create_connector_request(
                &environment,
                connector_type,
                REQUEST_FQDN,
                secret_identifier,
            ),
        )
        .await;
        assert_refused(case, expected_error, status, &body);
        assert!(
            body["connector"].is_null(),
            "case {case:?}: a refused create reports no connector, got {body}"
        );

        // THE POINT: the refused create took nothing from the counter every id this
        // service mints is drawn from, so the next id the store mints is the one it would
        // have minted had the refused call never arrived.
        let after = minted_ids(&router);
        assert_eq!(
            after, before,
            "case {case:?}: a refused create must consume no id, but the store had minted \
             {before} before it and reports {after} after it"
        );
    }

    // The mirror of every row above: a create that IS served consumes exactly the one id
    // of the connector it created, and that connector is really stored.
    let case = "a served create";
    let environment = create_settled(&router, None).await;
    let before = minted_ids(&router);
    let (status, body) = call(
        &router,
        "CreateEnvironmentConnector",
        &create_connector_request(&environment, "VCENTER", REQUEST_FQDN, HELD_SECRET_ARN),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let after = minted_ids(&router);
    assert_eq!(
        after,
        before + 1,
        "case {case:?}: a served create mints the one connector id it reports, but the store \
         had minted {before} before it and reports {after} after it"
    );
    let (status, listed) = call(
        &router,
        "ListEnvironmentConnectors",
        &json!({ "environmentId": environment }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the list expected 200, got {status} {listed}"
    );
    assert_eq!(
        listed["connectors"],
        json!([body["connector"].clone()]),
        "case {case:?}: the list must report the created connector, got {listed}"
    );
}

/// Which operation of this service a row of the cross-operation table has refused.
#[derive(Debug, Clone, Copy)]
enum Refused {
    /// `CreateEnvironment`, with a request whose `vpcId` names a VPC the account's own
    /// Amazon EC2 store holds none of.
    CreateEnvironmentUnknownVpc,
    /// `CreateEnvironment`, with a request that does not accept the terms.
    CreateEnvironmentTermsNotAccepted,
    /// `CreateEnvironment`, with a name an environment of this Region already carries.
    CreateEnvironmentNameTaken,
    /// `AssociateEipToVlan`, against an environment nothing was ever minted for.
    AssociateEipUnknownEnvironment,
    /// `AssociateEipToVlan`, with an allocation the account does not hold.
    AssociateEipUnknownAllocation,
    /// `AssociateEipToVlan`, against a VLAN the association cannot be made on.
    AssociateEipUnacceptedVlan,
}

#[tokio::test]
async fn a_refused_request_consumes_no_id_whichever_operation_mints_one() {
    let router = emulator();
    // The fixtures the rows below are refused against: an environment whose HCX VLAN is
    // public (the one an Elastic IP is associated with), a named environment whose name a
    // second create cannot take, and the one allocation the account holds.
    let public_hcx = create_public_hcx(&router, "id-seed-public-hcx").await;
    let taken_name = "id-seed-named-environment";
    create_settled(&router, Some(taken_name)).await;
    register_elastic_ip(&router.engine, "eipalloc-00000000000000001", Some("198.51.100.7"));

    // Each row: the case, the operation refused and the modelled error it answers with.
    // Every operation of this service that mints an id is a row, because the rule is
    // about the store's counter rather than about one handler.
    let rows: Vec<(&str, Refused, &str)> = vec![
        (
            "a create naming a VPC the account does not hold",
            Refused::CreateEnvironmentUnknownVpc,
            "ValidationException",
        ),
        (
            "a create that does not accept the terms",
            Refused::CreateEnvironmentTermsNotAccepted,
            "ValidationException",
        ),
        (
            "a create naming an environment name already taken in this Region",
            Refused::CreateEnvironmentNameTaken,
            "ValidationException",
        ),
        (
            "an association against an environment nothing was minted for",
            Refused::AssociateEipUnknownEnvironment,
            "ResourceNotFoundException",
        ),
        (
            "an association naming an allocation the account does not hold",
            Refused::AssociateEipUnknownAllocation,
            "ValidationException",
        ),
        (
            "an association against a VLAN no Elastic IP can be associated with",
            Refused::AssociateEipUnacceptedVlan,
            "ValidationException",
        ),
    ];

    for (case, refused, expected_error) in rows {
        let (operation, request) = match refused {
            Refused::CreateEnvironmentUnknownVpc => {
                let mut request = router.create_request(None);
                request["vpcId"] = json!("vpc-0000000000000beef");
                ("CreateEnvironment", request)
            }
            Refused::CreateEnvironmentTermsNotAccepted => {
                let mut request = router.create_request(None);
                request["termsAccepted"] = json!(false);
                ("CreateEnvironment", request)
            }
            Refused::CreateEnvironmentNameTaken => {
                ("CreateEnvironment", router.create_request(Some(taken_name)))
            }
            Refused::AssociateEipUnknownEnvironment => (
                "AssociateEipToVlan",
                json!({
                    "environmentId": ABSENT_ENVIRONMENT,
                    "vlanName": "hcx",
                    "allocationId": "eipalloc-00000000000000001",
                }),
            ),
            Refused::AssociateEipUnknownAllocation => (
                "AssociateEipToVlan",
                json!({
                    "environmentId": public_hcx,
                    "vlanName": "hcx",
                    "allocationId": ABSENT_ALLOCATION,
                }),
            ),
            Refused::AssociateEipUnacceptedVlan => (
                "AssociateEipToVlan",
                json!({
                    "environmentId": public_hcx,
                    "vlanName": "vMotion",
                    "allocationId": "eipalloc-00000000000000001",
                }),
            ),
        };

        let before = minted_ids(&router);
        let (status, body) = call(&router, operation, &request).await;
        assert_refused(case, expected_error, status, &body);
        let after = minted_ids(&router);
        assert_eq!(
            after, before,
            "case {case:?}: a refused {operation} must consume no id, but the store had minted \
             {before} before it and reports {after} after it"
        );
    }

    // The mirror: a create that IS served mints the environment id it reports, so the
    // counter the rows above left alone is the one a served create really draws from.
    let case = "a served environment create";
    let before = minted_ids(&router);
    let (status, body) = call(&router, "CreateEnvironment", &router.create_request(None)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let after = minted_ids(&router);
    assert!(
        after > before,
        "case {case:?}: a served create mints the environment id it reports, but the store had \
         minted {before} before it and reports {after} after it"
    );
}
