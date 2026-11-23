// SPDX-License-Identifier: CC0-1.0

//! Asking for the service-managed default managed login style AND supplying
//! customizations in the same request, over the wire through the router exactly
//! as an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: "When you specify true for this option, you must also
//! omit values for Settings and Assets in the request"
//! (`smithy_query(query="operation",operation="CreateManagedLoginBranding")`,
//! member `UseCognitoProvidedValues`), so the default style and a customization
//! of one's own are mutually exclusive — and the refusal carries ONE fixed
//! sentence, naming the members as the service words them and interpolating
//! nothing from the request. Both operations that take the pair answer it, and a
//! refused request leaves the stored style exactly as it was.
//!
//! Each table row names the case it asserts and every assertion reports the
//! value it actually saw.

use serde_json::{json, Value};

use crate::managed_login_branding::{
    asset, call, client, error_code, error_message, ok, pool, router, CATEGORIES, COLOR_MODES,
    EXTENSIONS,
};

/// The one sentence a request that asks for the service-managed defaults and
/// customizes the style as well is refused with: fixed text in full.
const DEFAULTS_OR_SETTINGS: &str =
    "useCognitoProvidedValues or settings should be specified (but not both)";

/// One row: the customization the request supplies ALONGSIDE
/// `UseCognitoProvidedValues: true`, and whether that is a value at all —
/// an `Assets` list with no element supplies no image file, so it is not one.
struct ConflictCase {
    case: &'static str,
    customization: Value,
    conflicts: bool,
}

/// The rows. They differ along WHICH member carries the customization and how
/// much of it there is: a settings document, a document that names no option at
/// all, one image file, both members at once, and the two forms that supply
/// nothing (no member, and an empty `Assets` list, whose `AssetListType` length
/// minimum is 0 per
/// `smithy_query(query="operation",operation="CreateManagedLoginBranding")`).
fn conflict_cases() -> Vec<ConflictCase> {
    let one_asset = asset(
        CATEGORIES[0],
        COLOR_MODES[0],
        EXTENSIONS[2],
        b"a page background",
        None,
    );
    vec![
        ConflictCase {
            case: "the defaults with a settings document",
            customization: json!({ "Settings": { "components": { "primaryButton": {} } } }),
            conflicts: true,
        },
        ConflictCase {
            case: "the defaults with a settings document that names no option",
            customization: json!({ "Settings": {} }),
            conflicts: true,
        },
        ConflictCase {
            case: "the defaults with one image file",
            customization: json!({ "Assets": [one_asset.clone()] }),
            conflicts: true,
        },
        ConflictCase {
            case: "the defaults with both a settings document and an image file",
            customization: json!({
                "Settings": { "componentClasses": { "buttons": { "borderRadius": 0 } } },
                "Assets": [one_asset],
            }),
            conflicts: true,
        },
        ConflictCase {
            case: "the defaults with an image list carrying no file",
            customization: json!({ "Assets": [] }),
            conflicts: false,
        },
        ConflictCase {
            case: "the defaults alone",
            customization: json!({}),
            conflicts: false,
        },
    ]
}

/// `customization` as a request body for `operation`, with the members that name
/// the style added and the defaults asked for.
fn request(customization: &Value, named: &[(&str, &str)]) -> Value {
    let mut body = customization
        .as_object()
        .cloned()
        .expect("each case's customization is an object");
    for (member, value) in named {
        body.insert((*member).to_string(), json!(value));
    }
    body.insert("UseCognitoProvidedValues".to_string(), json!(true));
    Value::Object(body)
}

/// The id a created or read style reports.
fn branding_id(payload: &Value) -> String {
    payload["ManagedLoginBranding"]["ManagedLoginBrandingId"]
        .as_str()
        .unwrap_or_else(|| panic!("a style must report its id, got {payload}"))
        .to_string()
}

#[tokio::test]
async fn a_create_that_asks_for_the_defaults_and_customizes_them_is_refused_with_one_sentence() {
    let router = router();
    let user_pool_id = pool(&router, "defaults-conflict-create-pool").await;

    for case in conflict_cases() {
        let label = case.case;
        // One app client per row, so the answer is about the parameters the row
        // carries and not about a style the client already has.
        let client_id = client(&router, &user_pool_id, label).await;
        let body = request(
            &case.customization,
            &[("UserPoolId", user_pool_id.as_str()), ("ClientId", &client_id)],
        );
        let (status, payload) = call(&router, "CreateManagedLoginBranding", &body).await;

        if !case.conflicts {
            assert_eq!(
                status, 200,
                "case {label:?}: a request that supplies no customization is served, got {status} \
                 with {payload}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&payload)),
            (400, "InvalidParameterException"),
            "case {label:?}: expected a 400 InvalidParameterException, got {status} with {payload}"
        );
        let message = error_message(&payload);
        assert_eq!(
            message, DEFAULTS_OR_SETTINGS,
            "case {label:?}: expected {DEFAULTS_OR_SETTINGS:?}, got {message:?}"
        );
        // The sentence above is fixed text; these are the request's own values
        // it must therefore not carry.
        for (what, value) in [
            ("the pool", user_pool_id.as_str()),
            ("the app client", client_id.as_str()),
        ] {
            assert!(
                !message.contains(value),
                "case {label:?}: the refusal must not name {what} ({value:?}), got {message:?}"
            );
        }

        // The refused create stored nothing, so the defaults-only request for
        // the same app client is still its first style.
        let created = ok(
            &router,
            "CreateManagedLoginBranding",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "UseCognitoProvidedValues": true,
            }),
        )
        .await;
        assert_eq!(
            created["ManagedLoginBranding"]["UseCognitoProvidedValues"],
            json!(true),
            "case {label:?}: a refused create stores no style, so this one is the client's first, \
             got {created}"
        );
    }
}

#[tokio::test]
async fn an_update_that_reverts_to_the_defaults_and_customizes_them_is_refused_with_one_sentence() {
    let router = router();
    let user_pool_id = pool(&router, "defaults-conflict-update-pool").await;

    for case in conflict_cases() {
        let label = case.case;
        // The style this row updates starts life CUSTOMIZED, so a refused
        // revert is observable: the stored style would otherwise already be the
        // service's defaults.
        let client_id = client(&router, &user_pool_id, label).await;
        let created = ok(
            &router,
            "CreateManagedLoginBranding",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "Settings": { "components": { "pageHeader": {} } },
            }),
        )
        .await;
        let style_id = branding_id(&created);

        let body = request(
            &case.customization,
            &[
                ("UserPoolId", user_pool_id.as_str()),
                ("ManagedLoginBrandingId", &style_id),
            ],
        );
        let (status, payload) = call(&router, "UpdateManagedLoginBranding", &body).await;

        if !case.conflicts {
            assert_eq!(
                status, 200,
                "case {label:?}: a revert that supplies no customization is served, got {status} \
                 with {payload}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&payload)),
            (400, "InvalidParameterException"),
            "case {label:?}: expected a 400 InvalidParameterException, got {status} with {payload}"
        );
        let message = error_message(&payload);
        assert_eq!(
            message, DEFAULTS_OR_SETTINGS,
            "case {label:?}: expected {DEFAULTS_OR_SETTINGS:?}, got {message:?}"
        );

        // The refused update changed nothing: the style is still the customized
        // one the create stored.
        let read = ok(
            &router,
            "DescribeManagedLoginBranding",
            &json!({ "UserPoolId": user_pool_id, "ManagedLoginBrandingId": style_id }),
        )
        .await;
        assert_eq!(
            read["ManagedLoginBranding"]["UseCognitoProvidedValues"],
            json!(false),
            "case {label:?}: a refused revert leaves the style customized, got {read}"
        );
        assert_eq!(
            read["ManagedLoginBranding"]["Settings"],
            json!({ "components": { "pageHeader": {} } }),
            "case {label:?}: a refused revert leaves the stored settings as they were, got {read}"
        );
    }
}
