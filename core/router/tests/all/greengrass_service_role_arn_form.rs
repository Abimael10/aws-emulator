// SPDX-License-Identifier: CC0-1.0

//! What the account's Greengrass service role may be SET to, across the two services the
//! question spans.
//!
//! RULE: `greengrassv2:AssociateServiceRoleToAccount` validates the FORM of `roleArn` and
//! nothing else — the value must be the ARN of an IAM role (vendor `iam`, resource type
//! `role`, and a non-empty role name after the type), and any value that is not is refused
//! with HTTP 400 carrying the fixed sentence "That role ARN is invalid." in the legacy
//! surface's capitalized `Message`, before anything is written, so the association the
//! account held is reported back unchanged. Nothing ABOUT the role is resolved: a role IAM
//! does not hold, and a role whose trust policy names another service or denies Greengrass
//! outright, are associated exactly as a Greengrass-trusted one is (trust-policy
//! compatibility surfaces later, at client-device verification, not on this call).
//!
//! `smithy_query(query="operation",operation="AssociateServiceRoleToAccount")` on
//! `services/service-com-amazonaws-greengrassv2-greengrassv2/smithy-model/\
//! com-amazonaws-greengrassv2-greengrassv2.json` shows `PUT /greengrass/servicerole` with
//! input member `roleArn` (String, required, jsonName `RoleArn`) carrying NO `@pattern` and
//! NO `@length`, so the ARN form is a server-side rule the model does not express, which is
//! why it is asserted here rather than left to the request boundary.
//!
//! This suite lives beside the router because every role ARN it accepts or refuses is one
//! IAM ITSELF minted: the roles, users, groups, instance profiles and policies below are
//! written through IAM's own `CreateRole`/`CreateUser`/`CreateGroup`/
//! `CreateInstanceProfile`/`CreatePolicy` on the same engine that serves Greengrass, so the
//! ARN forms under test are the forms the account's IAM actually reports and not a suite's
//! idea of them. The in-crate suite
//! `services/service-com-amazonaws-greengrassv2-greengrassv2/tests/all/\
//! service_role_association.rs` owns the single-service half (the replacement, the stamp and
//! the read-back).
//!
//! The rows differ along what the rule generalises over: for the accepted half, the trust
//! policy the role carries (which service principal it names, and whether the grant is an
//! `Allow` or a `Deny`), the PATH IAM holds the role under, the length of the role name up
//! to the modelled bound, and whether IAM holds the role at all; for the refused half, WHICH
//! IAM entity the ARN names (a user, a group, an instance profile, a managed policy) and the
//! ways a role ARN can name no role (a resource field that stops at the type, an empty name,
//! an empty name under a path, and a role name submitted without the ARN around it).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds an awsQuery form body and the reader that pulls one
// element's text out of the XML answer are the test crate's own, written once in
// `all/query_wire.rs`.
use crate::query_wire::{element, form_encode, opt_element};

/// The Region the Greengrass requests below are signed for.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The route both Greengrass API versions publish the account's service role at.
const SERVICE_ROLE_PATH: &str = "/greengrass/servicerole";

/// The status and the sentence a value that is not the ARN of an IAM role is refused with —
/// written here as the literals AWS answers rather than read from the crate's constants, so
/// a status or wording that drifts away from AWS fails this suite. Entirely fixed text: the
/// refusal is about the FORM of the value, so it quotes neither the value, the account nor
/// the member name back.
const INVALID_ROLE_ARN_STATUS: u16 = 400;
const INVALID_ROLE_ARN_MESSAGE: &str = "That role ARN is invalid.";

/// The longest role name IAM admits, from `CreateRole$RoleName`
/// (`smithy_query(query="operation",operation="CreateRole")` on the IAM model: `length 1..64`,
/// `pattern ^[\w+=,.@-]+$`).
const MAX_ROLE_NAME_LENGTH: usize = 64;

/// IAM and Greengrass V2 over ONE shared engine, exactly as the production router mounts
/// them, so the ARN Greengrass is handed is the one this IAM minted.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_greengrassv2_greengrassv2::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One `restJson1` call on the legacy Greengrass service-role route, answered with the HTTP
/// status and the parsed JSON body.
async fn service_role_call(
    router: &MultiServiceRouter,
    method: &str,
    body: Vec<u8>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://greengrass.{REGION}.amazonaws.com{SERVICE_ROLE_PATH}"
        ))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/greengrass/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or(Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, parsed)
}

/// `AssociateServiceRoleToAccount` for `role_arn`.
async fn associate(router: &MultiServiceRouter, role_arn: &str) -> (u16, Value) {
    let body = serde_json::to_vec(&json!({ "RoleArn": role_arn })).expect("a serializable body");
    service_role_call(router, "PUT", body).await
}

/// `GetServiceRoleForAccount`.
async fn get_service_role(router: &MultiServiceRouter) -> (u16, Value) {
    service_role_call(router, "GET", Vec::new()).await
}

/// A string member of a JSON object, or the empty string when it carries none.
fn text(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A trust policy with one statement of `effect` naming `principals`.
fn trust(effect: &str, principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Create a role through IAM's OWN `CreateRole` under `path`, and answer the ARN IAM reports
/// for it.
async fn role_arn(
    router: &MultiServiceRouter,
    name: &str,
    path: &str,
    document: &str,
) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("Path", path),
            ("AssumeRolePolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole({name:?}, path {path:?}): expected the role to be persisted, got \
         {status} {created}"
    );
    element(&created, "Arn")
}

/// One role the association must ACCEPT, and how IAM holds it.
struct Accepted {
    case: &'static str,
    name: String,
    path: &'static str,
    /// The trust policy IAM holds the role under, or `None` when IAM holds NO such role, so
    /// the request carries a well-formed role ARN that resolves to nothing.
    document: Option<String>,
}

/// The accepted rows: the trust policy is varied because the rule says it is NOT consulted,
/// the path and the name length because they vary the ARN's resource field, and the last row
/// because the role's EXISTENCE is not consulted either.
fn accepted_rows() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "a role whose trust policy names Greengrass",
            name: "GreengrassV2ServiceRole".to_string(),
            path: "/",
            document: Some(trust("Allow", &["greengrass.amazonaws.com"])),
        },
        Accepted {
            case: "a role whose trust policy names another service only",
            name: "Ec2TrustedRole".to_string(),
            path: "/",
            document: Some(trust("Allow", &["ec2.amazonaws.com"])),
        },
        Accepted {
            case: "a role whose trust policy denies Greengrass outright",
            name: "GreengrassDeniedRole".to_string(),
            path: "/",
            document: Some(trust("Deny", &["greengrass.amazonaws.com"])),
        },
        Accepted {
            case: "a role under the console's service-role path",
            name: "Greengrass_ServiceRole".to_string(),
            path: "/service-role/",
            document: Some(trust("Allow", &["greengrass.amazonaws.com"])),
        },
        Accepted {
            case: "a role under the service-linked role path",
            name: "AWSServiceRoleForGreengrass".to_string(),
            path: "/aws-service-role/greengrass.amazonaws.com/",
            document: Some(trust("Allow", &["greengrass.amazonaws.com"])),
        },
        Accepted {
            // The modelled bound of `CreateRole$RoleName`, so the longest resource field a
            // role ARN of this account can carry at the root path.
            case: "a role whose name is as long as IAM admits",
            name: "R".repeat(MAX_ROLE_NAME_LENGTH),
            path: "/",
            document: Some(trust("Allow", &["greengrass.amazonaws.com"])),
        },
        Accepted {
            case: "a well-formed role ARN IAM holds no role under",
            name: "GreengrassRoleThatWasNeverCreated".to_string(),
            path: "/",
            document: None,
        },
    ]
}

#[tokio::test]
async fn the_service_role_association_admits_every_well_formed_role_arn_whatever_iam_says() {
    let router = emulator();

    for row in accepted_rows() {
        let arn = match &row.document {
            Some(document) => role_arn(&router, &row.name, row.path, document).await,
            // Nothing is created under the name, so the value is the ARN a request carries
            // for a role this account's IAM does not hold. Asserted below through IAM's own
            // read, so the row tests an absent role rather than a typo.
            None => format!("arn:aws:iam::{ACCOUNT}:role/{}", row.name),
        };

        if row.document.is_none() {
            let (status, read) = iam(&router, "GetRole", &[("RoleName", &row.name)]).await;
            assert_eq!(
                (status, opt_element(&read, "Code")),
                (404, Some("NoSuchEntity".to_string())),
                "case {:?}: IAM must hold no role under {:?} for this row to mean anything, \
                 got {status} {read}",
                row.case,
                row.name
            );
        }

        let (status, written) = associate(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {:?}: a well-formed role ARN must be associated, got {status} {written}",
            row.case
        );
        let associated_at = text(&written, "AssociatedAt");
        assert!(
            !associated_at.is_empty(),
            "case {:?}: the association must be stamped with the instant of the call, got \
             {written}",
            row.case
        );

        // Backed by state, and by the ARN IAM minted rather than one Greengrass rebuilt:
        // the read reports exactly the value that was associated, and the same instant.
        let (status, got) = get_service_role(&router).await;
        assert_eq!(
            (status, text(&got, "RoleArn"), text(&got, "AssociatedAt")),
            (200, arn.clone(), associated_at),
            "case {:?}: expected the association just written reported back, got {status} {got}",
            row.case
        );
    }
}

/// The IAM record a refused row reads its value from: the action that mints it, the
/// parameters that call takes, and the element of the XML answer the value is read out of.
struct Minted {
    action: &'static str,
    params: Vec<(&'static str, String)>,
    member: &'static str,
}

/// One value the association must REFUSE: how it is produced from the account's IAM records.
struct Refused {
    case: &'static str,
    /// The IAM record the value is read from — `None` for a row derived from the held role's
    /// own ARN rather than from a record of its own.
    minted: Option<Minted>,
    /// How the submitted value is built from what was minted (or, for a derived row, from the
    /// ARN of the role the account holds).
    value: fn(&str) -> String,
}

/// The refused rows. The first four name a DIFFERENT IAM entity, each read from the record
/// IAM minted for it, so the type in the ARN's resource field is the one IAM writes; the rest
/// are the ways a value can carry the `role` type and still name no role.
fn refused_rows() -> Vec<Refused> {
    vec![
        Refused {
            case: "an IAM user's own ARN",
            minted: Some(Minted {
                action: "CreateUser",
                params: vec![("UserName", "greengrass-operator".to_string())],
                member: "Arn",
            }),
            value: str::to_string,
        },
        Refused {
            case: "an IAM group's own ARN",
            minted: Some(Minted {
                action: "CreateGroup",
                params: vec![("GroupName", "GreengrassOperators".to_string())],
                member: "Arn",
            }),
            value: str::to_string,
        },
        Refused {
            case: "an EC2 instance profile's own ARN",
            minted: Some(Minted {
                action: "CreateInstanceProfile",
                params: vec![(
                    "InstanceProfileName",
                    "GreengrassCoreProfile".to_string(),
                )],
                member: "Arn",
            }),
            value: str::to_string,
        },
        Refused {
            case: "a managed policy's own ARN",
            minted: Some(Minted {
                action: "CreatePolicy",
                params: vec![
                    (
                        "PolicyName",
                        "AWSGreengrassResourceAccessRolePolicy".to_string(),
                    ),
                    (
                        "PolicyDocument",
                        json!({
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "greengrass:*",
                                "Resource": "*"
                            }]
                        })
                        .to_string(),
                    ),
                ],
                member: "Arn",
            }),
            value: str::to_string,
        },
        Refused {
            // The resource field stops at the type, so the ARN names the KIND of resource
            // and no resource of it.
            case: "the held role's ARN with the role name stripped off",
            minted: None,
            value: |held| {
                held.rsplit_once('/')
                    .map(|(prefix, _name)| prefix.to_string())
                    .unwrap_or_else(|| held.to_string())
            },
        },
        Refused {
            case: "the held role's ARN with an empty role name",
            minted: None,
            value: |held| {
                held.rsplit_once('/')
                    .map(|(prefix, _name)| format!("{prefix}/"))
                    .unwrap_or_else(|| held.to_string())
            },
        },
        Refused {
            case: "a path with no role name after it",
            minted: None,
            value: |held| {
                held.rsplit_once('/')
                    .map(|(prefix, _name)| format!("{prefix}/service-role/"))
                    .unwrap_or_else(|| held.to_string())
            },
        },
        Refused {
            case: "the held role's NAME, submitted without the ARN around it",
            minted: None,
            value: |held| {
                held.rsplit_once('/')
                    .map(|(_prefix, name)| name.to_string())
                    .unwrap_or_else(|| held.to_string())
            },
        },
    ]
}

#[tokio::test]
async fn the_service_role_association_refuses_a_value_that_names_no_iam_role() {
    let router = emulator();

    // The association the account holds before each refusal, written through the same route:
    // a refusal must leave THIS value in place, so the read after it reports the role and the
    // instant below rather than the refused value or nothing at all.
    let held = role_arn(
        &router,
        "GreengrassHeldServiceRole",
        "/",
        &trust("Allow", &["greengrass.amazonaws.com"]),
    )
    .await;
    let (status, written) = associate(&router, &held).await;
    assert_eq!(
        status, 200,
        "setup: the held association must be written, got {status} {written}"
    );
    let held_at = text(&written, "AssociatedAt");

    for row in refused_rows() {
        let submitted = match &row.minted {
            Some(minted) => {
                let borrowed: Vec<(&str, &str)> = minted
                    .params
                    .iter()
                    .map(|(name, value)| (*name, value.as_str()))
                    .collect();
                let (status, created) = iam(&router, minted.action, &borrowed).await;
                assert_eq!(
                    status,
                    200,
                    "setup {}: expected the IAM record to be persisted, got {status} {created}",
                    minted.action
                );
                (row.value)(&element(&created, minted.member))
            }
            None => (row.value)(&held),
        };

        let (status, refused) = associate(&router, &submitted).await;
        assert_eq!(
            (status, text(&refused, "Message")),
            (
                INVALID_ROLE_ARN_STATUS,
                INVALID_ROLE_ARN_MESSAGE.to_string()
            ),
            "case {:?} ({submitted:?}): expected the invalid-role-ARN refusal, got {status} \
             {refused}",
            row.case
        );

        let (status, got) = get_service_role(&router).await;
        assert_eq!(
            (status, text(&got, "RoleArn"), text(&got, "AssociatedAt")),
            (200, held.clone(), held_at.clone()),
            "case {:?} ({submitted:?}): a refusal must write nothing, so the held \
             association stands unchanged, got {status} {got}",
            row.case
        );
    }
}
