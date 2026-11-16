// SPDX-License-Identifier: CC0-1.0

//! What a stack's custom branding REPORTS beside the members its request supplied: the
//! name the service gives the branding record, the palette the chosen colour theme expands
//! to, and the address each branding asset is fetched from.
//!
//! A theme is created from a title, a colour theme and two S3 locations, and reported with
//! more than that: the branding record's own name, the human-readable name of the colour
//! theme and the colours it paints the catalog page with, and a URL per asset. None of
//! those is a value a caller supplies, so each is DERIVED here — in one place, so the
//! create, the update and the read of one theme never disagree about them.

use ::std::collections::BTreeMap;

use crate::ops::types::{S3Location, THEME_STYLING_VALUES};

/// The fixed prefix the name of a branding record starts with, its `<stack name>` slot
/// filled by the stack the branding belongs to (`theme_<stack name>`).
///
/// A theme has no name of its own in the API — every theme operation addresses it by
/// `StackName` alone — so the name the service reports for the record is derived from the
/// stack it brands.
const THEME_NAME_PREFIX: &str = "theme_";

/// The name of the branding record held for the stack `stack_name`.
pub fn theme_name(stack_name: &str) -> String {
    format!("{THEME_NAME_PREFIX}{stack_name}")
}

/// How long the URL a theme reports for one of its branding assets stays valid.
///
/// The same span this service gives the other S3 object address it signs
/// ([`super::ICON_URL_VALIDITY`]): both are time-limited addresses of an object in a
/// caller's own bucket, published by a response rather than by the model, so neither one
/// has a span of its own to honour.
pub const THEME_ASSET_URL_VALIDITY: ::std::time::Duration = super::ICON_URL_VALIDITY;

/// The time-limited URL one of a theme's branding assets is fetched from, signed so that
/// it leads back to this emulator's own S3 front door.
///
/// The asset is named by an `S3Location` (a bucket and an optional key —
/// `smithy_query(query="shape", shape="S3Location")`), and the theme reports the address
/// of exactly that object: nothing is copied or fetched, so the object keeps its own
/// lifecycle and a later read reports an address of the same object. A location carrying
/// no key addresses its bucket, which is the whole path there is to sign for it — the one
/// thing that differs from the other S3 address this service signs
/// ([`super::signed_s3_object_url`], which both go through).
pub fn theme_asset_url(
    signer: &::smithy_server_runtime::UrlSigner,
    location: &S3Location,
) -> Option<String> {
    let bucket = &location.s3_bucket;
    let path = match location.s3_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => format!("/{bucket}/{key}"),
        None => format!("/{bucket}"),
    };
    super::signed_s3_object_url(signer, &path, THEME_ASSET_URL_VALIDITY)
}

/// The keys the colour map a theme reports is keyed by: the parts of the catalog page the
/// chosen colour theme paints.
pub const THEME_STYLING_BACKGROUND_COLOR: &str = "backgroundColor";
pub const THEME_STYLING_BODY_TEXT_COLOR: &str = "bodyTextColor";
pub const THEME_STYLING_BUTTON_COLOR: &str = "buttonColor";
pub const THEME_STYLING_LINK_COLOR: &str = "linkColor";
pub const THEME_STYLING_SPINNER_COLOR: &str = "spinnerColor";

/// The catalog page's background colour, the same under every colour theme.
const BACKGROUND_COLOR: &str = "#FFFFFF";

/// The colour the catalog page's body text is drawn in, the same under every colour
/// theme.
const BODY_TEXT_COLOR: &str = "#666666";

/// The colour the catalog page's buttons are drawn in, the same under every colour theme.
const BUTTON_COLOR: &str = "#8AC53E";

/// The colour the catalog page's loading spinner is drawn in, the same under every colour
/// theme.
const SPINNER_COLOR: &str = "#666666";

/// One `ThemeStyling` value's palette: the words the colour theme is named by, and the
/// accent colour it applies.
///
/// `ThemeStyling` is "the color that is used for the website links, text, buttons, and
/// catalog page background" (`smithy_query(query="shape", shape="Theme")`, member
/// `ThemeStyling`), and a theme reports that one choice in two further forms: the name a
/// console displays for it, and the colours the page is painted with. The accent is what
/// the choice decides — the colour the name states and the one the page's links are drawn
/// in; every other colour in the map is the same whichever theme is chosen.
struct ThemeStylingPalette {
    /// The `ThemeStyling` enum value this palette belongs to, one of
    /// [`THEME_STYLING_VALUES`].
    styling: &'static str,
    /// The human-readable colour name, the fixed part of the name a theme reports.
    label: &'static str,
    /// The accent colour, as the hex a theme reports it by.
    accent: &'static str,
}

/// The palette of every value `ThemeStyling` models — LIGHT_BLUE, BLUE, PINK and RED
/// ([`THEME_STYLING_VALUES`] in `ops/types/shared_types.rs`, from
/// `smithy_query(query="shape", shape="ThemeStyling")`).
///
/// A recorded AppStream response establishes the `BLUE` row — the name `Blue (#0070BA)`
/// and the five colours above with that accent — and each of the other three rows is the
/// same five colours carrying the accent its own name states. Those three accents are
/// INFERRED from the rule the recorded row states, not read from a response of their own,
/// so a recording that covers one of them is what should correct it.
const THEME_STYLING_PALETTES: &[ThemeStylingPalette] = &[
    ThemeStylingPalette {
        styling: "LIGHT_BLUE",
        label: "Light Blue",
        accent: "#007DBC",
    },
    ThemeStylingPalette {
        styling: "BLUE",
        label: "Blue",
        accent: "#0070BA",
    },
    ThemeStylingPalette {
        styling: "PINK",
        label: "Pink",
        accent: "#CC2264",
    },
    ThemeStylingPalette {
        styling: "RED",
        label: "Red",
        accent: "#D13212",
    },
];

/// The palette of the colour theme `styling` names.
///
/// `None` for a value outside the modelled set, which names no colour theme this service
/// paints a page with. Every MODELLED value has a row above, so a model that grows one has
/// to grow the table with it rather than quietly reporting no palette for it.
fn palette(styling: &str) -> Option<&'static ThemeStylingPalette> {
    let found = THEME_STYLING_PALETTES
        .iter()
        .find(|palette| palette.styling == styling);
    debug_assert!(
        found.is_some() || !THEME_STYLING_VALUES.contains(&styling),
        "no palette carries the modelled theme styling {styling}"
    );
    found
}

/// The name a theme reports for the colour theme `styling` names: the colour's own name
/// followed by the accent hex it applies, as `Blue (#0070BA)`.
pub fn theme_styling_name(styling: &str) -> Option<String> {
    palette(styling).map(|palette| format!("{} ({})", palette.label, palette.accent))
}

/// The colours the colour theme `styling` names paints the catalog page with, keyed by the
/// part of the page each one paints.
pub fn theme_styling_property(styling: &str) -> Option<BTreeMap<String, String>> {
    let palette = palette(styling)?;
    Some(
        [
            (THEME_STYLING_BACKGROUND_COLOR, BACKGROUND_COLOR),
            (THEME_STYLING_BODY_TEXT_COLOR, BODY_TEXT_COLOR),
            (THEME_STYLING_BUTTON_COLOR, BUTTON_COLOR),
            (THEME_STYLING_LINK_COLOR, palette.accent),
            (THEME_STYLING_SPINNER_COLOR, SPINNER_COLOR),
        ]
        .into_iter()
        .map(|(part, colour)| (part.to_string(), colour.to_string()))
        .collect(),
    )
}
