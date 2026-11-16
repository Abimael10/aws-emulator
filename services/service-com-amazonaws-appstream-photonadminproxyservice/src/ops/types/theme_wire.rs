// SPDX-License-Identifier: CC0-1.0

//! The `Theme` every reader of a stack's custom branding answers with, and the six
//! members of it the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error,
//! so a member the service really reports but the model omits cannot be declared through
//! the derive at all. A theme reports the branding record's own name, the title again, the
//! name and the colour map of the colour theme it applies, and an address per branding
//! asset — six members the shape's modelled member list does not mention — so this module
//! emits for `Theme` the same items the derive emits: one member schema per member (index
//! = its position in the member list), the struct `SCHEMA` naming them in that order, the
//! `THEME_SCHEMA` static and the `SerializableStruct`/`DeserializableStruct` pair. It is
//! the same arrangement [`super::stack_wire`] makes for the stack's own unmodelled member.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="Theme")` lists them in); the members it does not
//! declare are appended LAST, with their shape ids in a namespace of their own
//! ([`RECORDED_NAMESPACE`]), so they can neither claim to be modelled nor collide with
//! members the model may declare later — a model refresh only has to move them into place
//! and put the derive back. The ids never leave the process: the codec writes a member's
//! NAME as the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and
//! this structure is output-only), the `@http` chain (baked on only for an operation's
//! input) and `@jsonName` (no member here declares one: each one's wire name IS its member
//! name).
//!
//! Nothing here decides a VALUE: every member is written from what the stored theme holds
//! (`crate::state::ThemeRecord::to_theme`).

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;
use ::aws_smithy_types::DateTime;
use ::std::collections::BTreeMap;

use super::ThemeFooterLink;

/// The namespace the members the on-disk model does not declare sit in: NOT this service's
/// own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.appstream.recorded";

/// A stack's custom branding, as every operation that reports a theme answers it.
///
/// The first eight members are `com.amazonaws.appstream#Theme`'s, in the model's member
/// order. The six after them are the ones AppStream reports beyond the published model:
/// the branding record's name, its title, the name and the colour map of the colour theme
/// it applies, and the two asset addresses.
#[derive(Debug, Clone, Default)]
pub struct Theme {
    pub stack_name: Option<String>,
    pub state: Option<String>,
    pub theme_title_text: Option<String>,
    pub theme_styling: Option<String>,
    pub theme_footer_links: Option<Vec<ThemeFooterLink>>,
    pub theme_organization_logo_url: Option<String>,
    pub theme_favicon_url: Option<String>,
    pub created_time: Option<DateTime>,
    /// The name of the branding record the service holds for the stack.
    pub theme_name: Option<String>,
    /// The browser tab page title, the same text as `ThemeTitleText`.
    pub title_text: Option<String>,
    /// The name of the colour theme the branding applies.
    pub styling_name: Option<String>,
    /// The colours that colour theme paints the catalog page with.
    pub styling_property: Option<BTreeMap<String, String>>,
    /// The address of the logo, the same URL as `ThemeOrganizationLogoURL`.
    pub organization_logo_url: Option<String>,
    /// The address of the favicon, the same URL as `ThemeFaviconURL`.
    pub favicon_url: Option<String>,
}

/// One member this service's model DOES declare: its shape id in the service's own
/// namespace, its shape type, its wire name and its index in the member list — the member
/// schema the derive would emit for it.
macro_rules! modelled_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.appstream#Theme$", $wire),
                "com.amazonaws.appstream",
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

/// One member the model does NOT declare, in [`RECORDED_NAMESPACE`] so it cannot be
/// mistaken for a modelled one.
macro_rules! recorded_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.appstream.recorded#Theme$", $wire),
                RECORDED_NAMESPACE,
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

impl Theme {
    pub const M_STACK_NAME: Schema = modelled_member!("StackName", ShapeType::String, 0);
    pub const M_STATE: Schema = modelled_member!("State", ShapeType::String, 1);
    pub const M_THEME_TITLE_TEXT: Schema =
        modelled_member!("ThemeTitleText", ShapeType::String, 2);
    pub const M_THEME_STYLING: Schema = modelled_member!("ThemeStyling", ShapeType::String, 3);
    pub const M_THEME_FOOTER_LINKS: Schema =
        modelled_member!("ThemeFooterLinks", ShapeType::List, 4);
    pub const M_THEME_ORGANIZATION_LOGO_URL: Schema =
        modelled_member!("ThemeOrganizationLogoURL", ShapeType::String, 5);
    pub const M_THEME_FAVICON_URL: Schema =
        modelled_member!("ThemeFaviconURL", ShapeType::String, 6);
    pub const M_CREATED_TIME: Schema = modelled_member!("CreatedTime", ShapeType::Timestamp, 7);

    pub const M_THEME_NAME: Schema = recorded_member!("themeName", ShapeType::String, 8);
    pub const M_TITLE_TEXT: Schema = recorded_member!("titleText", ShapeType::String, 9);
    pub const M_STYLING_NAME: Schema = recorded_member!("stylingName", ShapeType::String, 10);
    pub const M_STYLING_PROPERTY: Schema =
        recorded_member!("stylingProperty", ShapeType::Map, 11);
    pub const M_ORGANIZATION_LOGO_URL: Schema =
        recorded_member!("organizationLogoURL", ShapeType::String, 12);
    pub const M_FAVICON_URL: Schema = recorded_member!("faviconURL", ShapeType::String, 13);

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.appstream#Theme",
            "com.amazonaws.appstream",
            "Theme",
        ),
        ShapeType::Structure,
        &[
            &Self::M_STACK_NAME,
            &Self::M_STATE,
            &Self::M_THEME_TITLE_TEXT,
            &Self::M_THEME_STYLING,
            &Self::M_THEME_FOOTER_LINKS,
            &Self::M_THEME_ORGANIZATION_LOGO_URL,
            &Self::M_THEME_FAVICON_URL,
            &Self::M_CREATED_TIME,
            &Self::M_THEME_NAME,
            &Self::M_TITLE_TEXT,
            &Self::M_STYLING_NAME,
            &Self::M_STYLING_PROPERTY,
            &Self::M_ORGANIZATION_LOGO_URL,
            &Self::M_FAVICON_URL,
        ],
    );
}

pub static THEME_SCHEMA: &Schema = &Theme::SCHEMA;

impl SerializableStruct for Theme {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(value) = self.stack_name.as_ref() {
            s.write_string(&Self::M_STACK_NAME, value.as_str())?;
        }
        if let Some(value) = self.state.as_ref() {
            s.write_string(&Self::M_STATE, value.as_str())?;
        }
        if let Some(value) = self.theme_title_text.as_ref() {
            s.write_string(&Self::M_THEME_TITLE_TEXT, value.as_str())?;
        }
        if let Some(value) = self.theme_styling.as_ref() {
            s.write_string(&Self::M_THEME_STYLING, value.as_str())?;
        }
        if let Some(value) = self.theme_footer_links.as_ref() {
            s.write_list(&Self::M_THEME_FOOTER_LINKS, &|element| {
                for link in value.iter() {
                    element.write_struct(&ThemeFooterLink::SCHEMA, link)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.theme_organization_logo_url.as_ref() {
            s.write_string(&Self::M_THEME_ORGANIZATION_LOGO_URL, value.as_str())?;
        }
        if let Some(value) = self.theme_favicon_url.as_ref() {
            s.write_string(&Self::M_THEME_FAVICON_URL, value.as_str())?;
        }
        if let Some(value) = self.created_time.as_ref() {
            s.write_timestamp(&Self::M_CREATED_TIME, value)?;
        }
        if let Some(value) = self.theme_name.as_ref() {
            s.write_string(&Self::M_THEME_NAME, value.as_str())?;
        }
        if let Some(value) = self.title_text.as_ref() {
            s.write_string(&Self::M_TITLE_TEXT, value.as_str())?;
        }
        if let Some(value) = self.styling_name.as_ref() {
            s.write_string(&Self::M_STYLING_NAME, value.as_str())?;
        }
        if let Some(value) = self.styling_property.as_ref() {
            s.write_map(&Self::M_STYLING_PROPERTY, &|entries| {
                for (part, colour) in value.iter() {
                    entries.write_string(&Self::M_STYLING_PROPERTY, part.as_str())?;
                    entries.write_string(&Self::M_STYLING_PROPERTY, colour.as_str())?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.organization_logo_url.as_ref() {
            s.write_string(&Self::M_ORGANIZATION_LOGO_URL, value.as_str())?;
        }
        if let Some(value) = self.favicon_url.as_ref() {
            s.write_string(&Self::M_FAVICON_URL, value.as_str())?;
        }
        Ok(())
    }
}

impl DeserializableStruct for Theme {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut theme = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => theme.stack_name = Some(d.read_string(&Self::M_STACK_NAME)?),
                Some(1) => theme.state = Some(d.read_string(&Self::M_STATE)?),
                Some(2) => {
                    theme.theme_title_text = Some(d.read_string(&Self::M_THEME_TITLE_TEXT)?)
                }
                Some(3) => theme.theme_styling = Some(d.read_string(&Self::M_THEME_STYLING)?),
                Some(4) => {
                    let mut links: Vec<ThemeFooterLink> = Vec::new();
                    d.read_list(&Self::M_THEME_FOOTER_LINKS, &mut |element| {
                        links.push(ThemeFooterLink::deserialize(element)?);
                        Ok(())
                    })?;
                    theme.theme_footer_links = Some(links);
                }
                Some(5) => {
                    theme.theme_organization_logo_url =
                        Some(d.read_string(&Self::M_THEME_ORGANIZATION_LOGO_URL)?)
                }
                Some(6) => {
                    theme.theme_favicon_url = Some(d.read_string(&Self::M_THEME_FAVICON_URL)?)
                }
                Some(7) => theme.created_time = Some(d.read_timestamp(&Self::M_CREATED_TIME)?),
                Some(8) => theme.theme_name = Some(d.read_string(&Self::M_THEME_NAME)?),
                Some(9) => theme.title_text = Some(d.read_string(&Self::M_TITLE_TEXT)?),
                Some(10) => theme.styling_name = Some(d.read_string(&Self::M_STYLING_NAME)?),
                Some(11) => {
                    let mut colours: BTreeMap<String, String> = BTreeMap::new();
                    d.read_map(&Self::M_STYLING_PROPERTY, &mut |part, entry| {
                        let colour = entry.read_string(&Self::M_STYLING_PROPERTY)?;
                        colours.insert(part, colour);
                        Ok(())
                    })?;
                    theme.styling_property = Some(colours);
                }
                Some(12) => {
                    theme.organization_logo_url =
                        Some(d.read_string(&Self::M_ORGANIZATION_LOGO_URL)?)
                }
                Some(13) => theme.favicon_url = Some(d.read_string(&Self::M_FAVICON_URL)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(theme)
    }
}
