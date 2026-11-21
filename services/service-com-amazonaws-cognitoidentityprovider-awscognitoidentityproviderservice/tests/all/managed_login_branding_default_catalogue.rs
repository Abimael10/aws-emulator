// SPDX-License-Identifier: CC0-1.0

//! The CATALOGUE of image files Amazon Cognito provides for a managed login
//! branding style: the sequence a merged read of a style renders for every role
//! the style itself supplied no file for, driven through the router exactly as
//! an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: the provided files are a FIXED SEQUENCE the service
//! serves in its own order — the categories in the order the managed login
//! configuration lays a style out in, each category's `DARK` file immediately
//! before its `LIGHT` one, and each file carrying the file type its category is
//! published in — so a read renders that stored order rather than sorting it,
//! both readers of a style render the same sequence, and every index of `Assets`
//! addresses the same role on every read.
//!
//! Each table row names the case it asserts and every assertion reports the
//! value it actually saw.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::managed_login_branding::{asset, client, ok, pool, router, CATEGORIES, EXTENSIONS};

/// The categories the provided catalogue serves, in the order it serves them.
///
/// Every value of `AssetCategoryType`
/// (`smithy_query(query="shape",shape="AssetCategoryType")`), ordered as the
/// managed login configuration lays a style out: the page's own surfaces from
/// the page footer inwards, then the graphic of each authentication factor, then
/// the logo each surface carries, and last the favicons a browser fetches and
/// the icon an identity-provider button wears.
const CATALOGUE: [&str; 15] = [
    "PAGE_FOOTER_BACKGROUND",
    "FORM_BACKGROUND",
    "PAGE_HEADER_BACKGROUND",
    "PAGE_BACKGROUND",
    "EMAIL_GRAPHIC",
    "AUTH_APP_GRAPHIC",
    "SMS_GRAPHIC",
    "PASSWORD_GRAPHIC",
    "PASSKEY_GRAPHIC",
    "PAGE_HEADER_LOGO",
    "PAGE_FOOTER_LOGO",
    "FORM_LOGO",
    "FAVICON_ICO",
    "FAVICON_SVG",
    "IDP_BUTTON_ICON",
];

/// The display modes the catalogue serves a file for, in the order it serves
/// them: the two `ColorSchemeModeType` values an image is served for, dark mode
/// first (`DYNAMIC` is the mode that follows the browser rather than a third
/// image to serve).
const CATALOGUE_MODES: [&str; 2] = ["DARK", "LIGHT"];

/// The file type the catalogue publishes `category` in: a category whose name
/// states the format its role is fetched in — the favicon a browser asks for as
/// `favicon.ico` — is published in that format, and every other role is artwork
/// the page scales (`AssetExtensionType`,
/// `smithy_query(query="shape",shape="AssetExtensionType")`).
fn catalogue_extension(category: &str) -> &'static str {
    if category == "FAVICON_ICO" {
        "ICO"
    } else {
        "SVG"
    }
}

/// The `(Category, ColorMode, Extension)` sequence the catalogue serves.
fn catalogue_sequence() -> Vec<(String, String, String)> {
    CATALOGUE
        .iter()
        .flat_map(|category| {
            CATALOGUE_MODES.iter().map(move |mode| {
                (
                    (*category).to_string(),
                    (*mode).to_string(),
                    catalogue_extension(category).to_string(),
                )
            })
        })
        .collect()
}

/// The `(Category, ColorMode, Extension)` of each file a rendered style reports,
/// in the order the answer reports them.
fn rendered_sequence(case: &str, rendered: &Value) -> Vec<(String, String, String)> {
    rendered["Assets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a style reports Assets as a list, got {rendered}"))
        .iter()
        .map(|file| {
            (
                file["Category"].as_str().unwrap_or_default().to_string(),
                file["ColorMode"].as_str().unwrap_or_default().to_string(),
                file["Extension"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// A merged read of the style `branding_id` of `user_pool_id`.
async fn merged_by_id(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    branding_id: &str,
) -> Value {
    ok(
        router,
        "DescribeManagedLoginBranding",
        &json!({
            "UserPoolId": user_pool_id,
            "ManagedLoginBrandingId": branding_id,
            "ReturnMergedResources": true,
        }),
    )
    .await["ManagedLoginBranding"]
        .clone()
}

/// A merged read of the style assigned to the app client `client_id`.
async fn merged_by_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
) -> Value {
    ok(
        router,
        "DescribeManagedLoginBrandingByClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "ReturnMergedResources": true,
        }),
    )
    .await["ManagedLoginBranding"]
        .clone()
}

/// One row of the table: the files the style supplies of its own, and the
/// catalogue entries those files fill.
struct CatalogueCase {
    case: &'static str,
    /// The files the style is created with.
    own: Vec<Value>,
    /// The `(Category, ColorMode)` slots those files fill.
    filled: Vec<(&'static str, &'static str)>,
}

/// The table. The rows differ along what the rule generalises over: how many
/// files the style supplies of its own and which catalogue slots they fill —
/// none, one at the head of the catalogue, one at its tail, several, and a file
/// for the browser-adaptive display mode the catalogue provides none for.
fn catalogue_cases() -> Vec<CatalogueCase> {
    vec![
        CatalogueCase {
            case: "the Cognito-provided default style",
            own: Vec::new(),
            filled: Vec::new(),
        },
        CatalogueCase {
            case: "a style that supplies the dark file of the catalogue's first category",
            own: vec![asset(CATALOGUE[0], "DARK", EXTENSIONS[2], b"footer", None)],
            filled: vec![(CATALOGUE[0], "DARK")],
        },
        CatalogueCase {
            case: "a style that supplies the light file of the catalogue's last category",
            own: vec![asset(CATALOGUE[14], "LIGHT", EXTENSIONS[3], b"<svg/>", None)],
            filled: vec![(CATALOGUE[14], "LIGHT")],
        },
        CatalogueCase {
            // A category the issue's own report does not cite, so the catalogue
            // is held to its rule away from the head of the sequence too.
            case: "a style that supplies both files of a category in the middle",
            own: vec![
                asset("PASSKEY_GRAPHIC", "DARK", EXTENSIONS[4], b"dark", None),
                asset("PASSKEY_GRAPHIC", "LIGHT", EXTENSIONS[1], b"light", None),
            ],
            filled: vec![("PASSKEY_GRAPHIC", "DARK"), ("PASSKEY_GRAPHIC", "LIGHT")],
        },
        CatalogueCase {
            case: "a style whose only file is for the browser-adaptive display mode",
            own: vec![asset("FORM_LOGO", "DYNAMIC", EXTENSIONS[2], b"any", None)],
            filled: Vec::new(),
        },
    ]
}

#[tokio::test]
async fn the_provided_catalogue_is_served_in_its_own_order() {
    let router = router();
    let catalogue = catalogue_sequence();

    // The catalogue covers every category the model declares, once per display
    // mode, so the sequence and the modelled value set name the same
    // categories.
    for category in CATEGORIES {
        assert!(
            CATALOGUE.contains(&category),
            "the catalogue must provide the modelled category {category}, got {CATALOGUE:?}"
        );
    }
    assert_eq!(
        CATALOGUE.len(),
        CATEGORIES.len(),
        "the catalogue must provide exactly the modelled categories, got {CATALOGUE:?}"
    );

    for (index, row) in catalogue_cases().into_iter().enumerate() {
        let case = row.case;
        let user_pool_id = pool(&router, &format!("catalogue-{index}-pool")).await;
        let client_id = client(&router, &user_pool_id, &format!("catalogue-{index}")).await;
        let mut create = json!({ "UserPoolId": user_pool_id, "ClientId": client_id });
        if row.own.is_empty() {
            create["UseCognitoProvidedValues"] = json!(true);
        } else {
            create["Assets"] = Value::Array(row.own.clone());
        }
        let created = ok(&router, "CreateManagedLoginBranding", &create).await;
        let branding_id = created["ManagedLoginBranding"]["ManagedLoginBrandingId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a minted branding id, got {created}"))
            .to_string();

        // The style's own files come first and keep the order they were
        // submitted in; the provided catalogue follows, in its own order, with
        // the slots the style already filled left out of it.
        let mut expected: Vec<(String, String, String)> = row
            .own
            .iter()
            .map(|file| {
                (
                    file["Category"].as_str().unwrap_or_default().to_string(),
                    file["ColorMode"].as_str().unwrap_or_default().to_string(),
                    file["Extension"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        expected.extend(catalogue.iter().filter(|(category, mode, _)| {
            !row.filled
                .iter()
                .any(|(filled, filled_mode)| filled == category && filled_mode == mode)
        }).cloned());

        let merged = merged_by_id(&router, &user_pool_id, &branding_id).await;
        let served = rendered_sequence(case, &merged);
        assert_eq!(
            served, expected,
            "case {case:?}: the merged view must serve the catalogue in its own order, got \
             {served:?}"
        );

        // The reader by app client resolves the same style through the client it
        // is assigned to, so it renders the same sequence.
        let by_client = merged_by_client(&router, &user_pool_id, &client_id).await;
        assert_eq!(
            rendered_sequence(case, &by_client),
            served,
            "case {case:?}: the reader by app client must serve the same sequence, got \
             {by_client}"
        );

        // A read never reorders what it renders: a second read of the unchanged
        // style answers the same sequence, so an index addresses one role.
        let again = merged_by_id(&router, &user_pool_id, &branding_id).await;
        assert_eq!(
            rendered_sequence(case, &again),
            served,
            "case {case:?}: a second read must serve the same sequence, got {again}"
        );
    }
}

#[tokio::test]
async fn every_catalogue_entry_names_its_role_its_mode_and_its_file_type() {
    let router = router();
    let user_pool_id = pool(&router, "catalogue-entries-pool").await;
    let client_id = client(&router, &user_pool_id, "catalogue-entries").await;
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
    let branding_id = created["ManagedLoginBranding"]["ManagedLoginBrandingId"]
        .as_str()
        .expect("a minted branding id")
        .to_string();
    let merged = merged_by_id(&router, &user_pool_id, &branding_id).await;
    let served = rendered_sequence("the provided catalogue", &merged);

    // Each category's dark file comes immediately before its light one, and
    // each file is published in the file type its category states.
    for (position, category) in CATALOGUE.iter().enumerate() {
        let dark = served.get(position * 2).cloned().unwrap_or_default();
        let light = served.get(position * 2 + 1).cloned().unwrap_or_default();
        let extension = catalogue_extension(category).to_string();
        assert_eq!(
            (dark, light),
            (
                ((*category).to_string(), "DARK".to_string(), extension.clone()),
                ((*category).to_string(), "LIGHT".to_string(), extension)
            ),
            "category {category}: its dark file comes immediately before its light one, got \
             {served:?}"
        );
    }

    // The catalogue is the service's own artwork, so no entry claims a file a
    // caller uploaded, and the list stays inside `AssetListType`'s modelled
    // maximum of 40 files.
    let files = merged["Assets"].as_array().expect("Assets is a list");
    assert!(
        files.len() <= 40,
        "the catalogue must stay inside the modelled maximum, got {} files",
        files.len()
    );
    for file in files {
        assert!(
            file.get("Bytes").is_none() && file.get("ResourceId").is_none(),
            "a provided file carries no uploaded file, got {file}"
        );
    }
}
