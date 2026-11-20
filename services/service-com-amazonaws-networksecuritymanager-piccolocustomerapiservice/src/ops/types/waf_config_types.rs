// SPDX-License-Identifier: CC0-1.0

//! `WafConfig` — the AWS WAF settings a policy's `policyConfiguration` carries.
//!
//! HAND-AUTHORED schema and codec, and the one shape of this service declared that way.
//! The model marks both of `WafConfig`'s members `@required`, but the service does NOT
//! refuse an omitted one at the protocol boundary: it answers its own
//! `ValidationException` naming the member, its dotted path and the values its target enum
//! admits (see [`crate::state::waf_config_violation`]). That refusal is a MODELLED error
//! shape with `reason` and `fieldList` members, so it can only be returned by the handler
//! — which means the request has to reach the handler with the member absent. The derive
//! cannot express that: a `@required` member must be a non-`Option` field, and an absent
//! one fails the codec before any handler runs.
//!
//! What follows is exactly what `#[smithy_macros::smithy(shape = ..)]` emits for a
//! two-member structure — one `M_<MEMBER>` member schema per member, in the model's member
//! order, each carrying its target enum's admissible value set so a value outside it is
//! still refused by the validating walk; a `SCHEMA` of type `Structure` naming them in that
//! order; the `<SHAPE>_SCHEMA` static; and the `SerializableStruct` /
//! `DeserializableStruct` pair — with both members read as optional.
//!
//! Member set, member order and targets come from
//! `smithy_query(query="shape", shape="WafConfig", model_path="services/\
//! service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/smithy-model/\
//! com-amazonaws-networksecuritymanager-piccolocustomerapiservice.json")`.
#![allow(unused_imports)]

use super::*;

/// `WafConfig` — "AWS WAF-specific policy configuration settings."
///
/// Both members are `Option` because the service, not the boundary, refuses an omitted
/// one; see the module docs.
#[derive(Debug, Clone, Default)]
pub struct WafConfig {
    /// `ExistingCustomerWebACLResolution` — "Determines how AWS Network Security Manager
    /// handles remediation when a resource already has a customer-created web ACL.
    /// Required for AWS WAF policies."
    pub existing_customer_web_acl_resolution: Option<String>,
    /// `WAFConflictResolutionOptions` — "The conflict-resolution strategy for AWS WAF
    /// policies. Required for AWS WAF policies."
    pub conflict_resolution: Option<String>,
}

/// The constraint trait maps the member schemas below reference, in their own module for
/// the reason the derive puts them in one: a `static` cannot be an associated item.
#[doc(hidden)]
#[allow(non_snake_case)]
mod waf_config_traits {
    use super::*;

    /// `existingCustomerWebACLResolution` targets an enum, so the walk refuses any value
    /// outside its admissible set — exactly as it does for a derived enum member.
    pub(super) static M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION_TRAITS:
        ::std::sync::LazyLock<::aws_smithy_schema::TraitMap> =
        ::std::sync::LazyLock::new(|| {
            let mut m = ::aws_smithy_schema::TraitMap::new();
            m.insert(::std::boxed::Box::new(
                ::aws_smithy_schema_server::constraints::EnumTrait::new(
                    EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION_VALUES,
                ),
            ));
            m
        });

    /// See [`M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION_TRAITS`].
    pub(super) static M_CONFLICT_RESOLUTION_TRAITS:
        ::std::sync::LazyLock<::aws_smithy_schema::TraitMap> =
        ::std::sync::LazyLock::new(|| {
            let mut m = ::aws_smithy_schema::TraitMap::new();
            m.insert(::std::boxed::Box::new(
                ::aws_smithy_schema_server::constraints::EnumTrait::new(
                    W_A_F_CONFLICT_RESOLUTION_OPTIONS_VALUES,
                ),
            ));
            m
        });
}

#[allow(non_upper_case_globals)]
impl WafConfig {
    const M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.networksecuritymanager#WafConfig$existingCustomerWebACLResolution",
                "com.amazonaws.networksecuritymanager",
                "existingCustomerWebACLResolution",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "existingCustomerWebACLResolution",
            0,
        )
        .with_traits(&waf_config_traits::M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION_TRAITS);

    const M_CONFLICT_RESOLUTION: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.networksecuritymanager#WafConfig$conflictResolution",
                "com.amazonaws.networksecuritymanager",
                "conflictResolution",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "conflictResolution",
            1,
        )
        .with_traits(&waf_config_traits::M_CONFLICT_RESOLUTION_TRAITS);

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.networksecuritymanager#WafConfig",
            "com.amazonaws.networksecuritymanager",
            "WafConfig",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION,
            &Self::M_CONFLICT_RESOLUTION,
        ],
    );
}

pub static WAF_CONFIG_SCHEMA: &::aws_smithy_schema::Schema = &WafConfig::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for WafConfig {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        if let Some(value) = self.existing_customer_web_acl_resolution.as_ref() {
            s.write_string(
                &Self::M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION,
                value.as_str(),
            )?;
        }
        if let Some(value) = self.conflict_resolution.as_ref() {
            s.write_string(&Self::M_CONFLICT_RESOLUTION, value.as_str())?;
        }
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for WafConfig {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut existing_customer_web_acl_resolution: Option<String> = None;
        let mut conflict_resolution: Option<String> = None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => {
                    existing_customer_web_acl_resolution = Some(
                        d.read_string(&Self::M_EXISTING_CUSTOMER_WEB_A_C_L_RESOLUTION)?,
                    );
                }
                Some(1) => {
                    conflict_resolution = Some(d.read_string(&Self::M_CONFLICT_RESOLUTION)?);
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            existing_customer_web_acl_resolution,
            conflict_resolution,
        })
    }
}
