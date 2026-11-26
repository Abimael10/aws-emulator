// SPDX-License-Identifier: CC0-1.0

//! A route's authorization configuration: which `Authorizer` resource a route of each
//! `AuthorizationType` has to be associated with, and how the service refuses one whose
//! `AuthorizerId` does not name such an authorizer.
//!
//! A module of its own under [`crate::state`], because more than one operation writes a
//! route's authorization — `CreateRoute` and `UpdateRoute` — so every writer has to hold it
//! to one requirement, or a route reaches the store naming an authorizer that cannot
//! authorize it. [`crate::state`] re-exports every public item here, so a handler reaches
//! them the way it reaches the rest of this service's state helpers.

use super::*;

/// The `AuthorizationType` that hands a route's authorization decision to a Lambda
/// authorizer, out of the generated `AUTHORIZATION_TYPE_VALUES` catalog (`AWS_IAM | CUSTOM
/// | JWT | NONE`). From `Route$AuthorizationType`: "CUSTOM for using a Lambda authorizer".
pub const CUSTOM_AUTHORIZATION_TYPE: &str = "CUSTOM";

/// The `AuthorizationType` that hands the decision to a JWT authorizer, from the same
/// catalog: "JWT for using JSON Web Tokens" (`Route$AuthorizationType`).
pub const JWT_AUTHORIZATION_TYPE: &str = AUTHORIZER_TYPE_JWT;

/// The `AuthorizerType` an authorizer has to carry to authorize a route of this
/// `AuthorizationType`, or `None` for a type that decides without an authorizer at all.
///
/// The pairing is each value's own documentation. `Route$AuthorizationType` says "JWT for
/// using JSON Web Tokens" and `Authorizer$AuthorizerType` says "Specify JWT to use JSON Web
/// Tokens", so a JWT route is authorized by a JWT authorizer; the same member says "CUSTOM
/// for using a Lambda authorizer" against "Specify REQUEST for a Lambda function using
/// incoming request parameters", so a CUSTOM route is authorized by a REQUEST authorizer.
/// The other two values name no resource: `NONE` is "open access" and `AWS_IAM` uses "AWS
/// IAM permissions".
pub fn authorizer_type_authorizing(authorization_type: &str) -> Option<&'static str> {
    match authorization_type {
        JWT_AUTHORIZATION_TYPE => Some(AUTHORIZER_TYPE_JWT),
        CUSTOM_AUTHORIZATION_TYPE => Some(AUTHORIZER_TYPE_REQUEST),
        _ => None,
    }
}

/// The sentence this service refuses a route whose `AuthorizerId` names no authorizer of
/// its api with.
///
/// The refusal is about the ASSOCIATION, not about the id — an id a route may not name is
/// one no authorizer of that api carries — so the sentence states the requirement rather
/// than echoing the id back.
pub const UNRESOLVED_AUTHORIZER_MESSAGE: &str = "Invalid authorizer ID specified. Setting \
    the authorization type to CUSTOM or COGNITO_USER_POOLS requires a valid authorizer.";

/// The sentence this service refuses a route that is not associated with an authorizer able
/// to authorize it with, in the form for the `AuthorizationType` the request set.
///
/// The refusal is about the authorizer REFERENCE — a route of an authorizer-backed type
/// that names no authorizer at all is as unauthorizable as one naming an id no authorizer
/// of its api carries, or one naming an authorizer of the other kind — so the sentence
/// states the requirement the type imposes and echoes no id back. `JWT` is the one
/// authorization type authorized by an authorizer of its own name
/// ([`authorizer_type_authorizing`]), so its sentence names that kind; `CUSTOM` is "for
/// using a Lambda authorizer", whose requirement is the one
/// [`UNRESOLVED_AUTHORIZER_MESSAGE`] states.
pub fn invalid_authorizer_reference_message(authorization_type: &str) -> String {
    if authorization_type == JWT_AUTHORIZATION_TYPE {
        return format!(
            "Invalid authorizer ID specified. Setting the authorization type to \
             {JWT_AUTHORIZATION_TYPE} requires a valid {AUTHORIZER_TYPE_JWT} authorizer."
        );
    }
    UNRESOLVED_AUTHORIZER_MESSAGE.to_string()
}

/// `Ok(())`, or why this route's authorization configuration cannot hold together — the
/// message for the operation's `BadRequestException`.
///
/// One rule: a route whose `AuthorizationType` hands the decision to an `Authorizer`
/// resource has to be associated with an authorizer OF ITS OWN API whose `AuthorizerType`
/// authorizes that type ([`authorizer_type_authorizing`]), because `Route$AuthorizerId` is
/// what names it and nothing else on the route does. So a route of such a type naming no
/// authorizer, one naming an id the api holds no authorizer under, and one naming an
/// authorizer of the other kind are all refused, in the sentence that type states
/// ([`invalid_authorizer_reference_message`]). A route of a type that decides without an
/// authorizer may still name one — the member is not cleared by switching type — but the id
/// has to resolve to an authorizer of the api all the same, since a route may reference
/// nothing its api does not hold.
///
/// `authorizers` is the api's own authorizer map, so an id belonging to an authorizer of
/// ANOTHER api resolves to nothing here, exactly as it does on AWS.
///
/// Shared by every writer of a route, so the requirement cannot differ between the create
/// and a later update; an update validates the MERGED record, so a route that already names
/// an authorizer may switch type without resending the id.
pub fn validate_route_authorization(
    authorizers: &BTreeMap<String, Authorizer>,
    authorization_type: &str,
    authorizer_id: Option<&str>,
) -> Result<(), String> {
    let required = authorizer_type_authorizing(authorization_type);
    // An `AuthorizerId` carried EMPTY names no authorizer, the same reading
    // `crate::state::Api::route_keys_using_authorizer` gives it.
    let Some(authorizer_id) = authorizer_id.filter(|id| !id.is_empty()) else {
        return match required {
            Some(_) => Err(invalid_authorizer_reference_message(authorization_type)),
            None => Ok(()),
        };
    };
    let authorizes = authorizers.get(authorizer_id).is_some_and(|authorizer| {
        required.is_none_or(|required| authorizer.authorizer_type == required)
    });
    if !authorizes {
        return Err(invalid_authorizer_reference_message(authorization_type));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        authorizer_type_authorizing, invalid_authorizer_reference_message,
        validate_route_authorization, CUSTOM_AUTHORIZATION_TYPE, JWT_AUTHORIZATION_TYPE,
        UNRESOLVED_AUTHORIZER_MESSAGE,
    };
    use crate::state::{Authorizer, AUTHORIZER_TYPE_JWT, AUTHORIZER_TYPE_REQUEST};

    /// An api's authorizer map holding one authorizer of each modelled type.
    fn authorizers() -> BTreeMap<String, Authorizer> {
        ["jwt-one", "request-one"]
            .into_iter()
            .map(|authorizer_id| {
                let authorizer_type = if authorizer_id == "jwt-one" {
                    AUTHORIZER_TYPE_JWT
                } else {
                    AUTHORIZER_TYPE_REQUEST
                };
                (
                    authorizer_id.to_string(),
                    Authorizer {
                        authorizer_id: authorizer_id.to_string(),
                        authorizer_type: authorizer_type.to_string(),
                        ..Authorizer::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn each_authorization_type_is_paired_with_the_authorizer_type_that_authorizes_it() {
        // The rows are the whole modelled `AUTHORIZATION_TYPE_VALUES` catalog.
        let rows = [
            ("AWS_IAM", None),
            (CUSTOM_AUTHORIZATION_TYPE, Some(AUTHORIZER_TYPE_REQUEST)),
            (JWT_AUTHORIZATION_TYPE, Some(AUTHORIZER_TYPE_JWT)),
            ("NONE", None),
        ];
        for (authorization_type, expected) in rows {
            let paired = authorizer_type_authorizing(authorization_type);
            assert_eq!(
                paired, expected,
                "case {authorization_type:?}: expected {expected:?}, got {paired:?}"
            );
        }
    }

    #[test]
    fn a_route_is_authorized_only_by_an_authorizer_of_the_type_its_authorization_type_names() {
        let authorizers = authorizers();
        // (the authorization type, the id the route names, the outcome)
        let accepted = [
            (JWT_AUTHORIZATION_TYPE, Some("jwt-one")),
            (CUSTOM_AUTHORIZATION_TYPE, Some("request-one")),
            ("NONE", None),
            ("AWS_IAM", None),
            // A type that decides without an authorizer may still carry one the api holds,
            // of either kind, and an empty id names none at all.
            ("NONE", Some("jwt-one")),
            ("AWS_IAM", Some("request-one")),
            ("NONE", Some("")),
        ];
        for (authorization_type, authorizer_id) in accepted {
            let weighed =
                validate_route_authorization(&authorizers, authorization_type, authorizer_id);
            assert_eq!(
                weighed,
                Ok(()),
                "case ({authorization_type:?}, {authorizer_id:?}): expected the \
                 configuration accepted, got {weighed:?}"
            );
        }

        // The ways the association can fail: no id at all, an empty id, an id the api holds
        // no authorizer under, and an authorizer of the OTHER kind.
        let refused = [
            (JWT_AUTHORIZATION_TYPE, None),
            (JWT_AUTHORIZATION_TYPE, Some("")),
            (JWT_AUTHORIZATION_TYPE, Some("nosuchauth")),
            (JWT_AUTHORIZATION_TYPE, Some("request-one")),
            (CUSTOM_AUTHORIZATION_TYPE, None),
            (CUSTOM_AUTHORIZATION_TYPE, Some("")),
            (CUSTOM_AUTHORIZATION_TYPE, Some("nosuchauth")),
            (CUSTOM_AUTHORIZATION_TYPE, Some("jwt-one")),
            // A type that needs none still cannot reference an authorizer the api does not
            // hold.
            ("NONE", Some("nosuchauth")),
            ("AWS_IAM", Some("nosuchauth")),
        ];
        for (authorization_type, authorizer_id) in refused {
            let weighed =
                validate_route_authorization(&authorizers, authorization_type, authorizer_id);
            assert_eq!(
                weighed,
                Err(invalid_authorizer_reference_message(authorization_type)),
                "case ({authorization_type:?}, {authorizer_id:?}): expected the \
                 authorization type's own refusal, got {weighed:?}"
            );
        }
    }

    #[test]
    fn the_refusal_states_the_requirement_of_the_authorization_type_it_refuses() {
        let jwt = invalid_authorizer_reference_message(JWT_AUTHORIZATION_TYPE);
        assert_eq!(
            jwt,
            "Invalid authorizer ID specified. Setting the authorization type to JWT \
             requires a valid JWT authorizer.",
            "expected the JWT requirement, got {jwt:?}"
        );
        for authorization_type in [CUSTOM_AUTHORIZATION_TYPE, "NONE", "AWS_IAM"] {
            let message = invalid_authorizer_reference_message(authorization_type);
            assert_eq!(
                message, UNRESOLVED_AUTHORIZER_MESSAGE,
                "case {authorization_type:?}: expected the association requirement, got \
                 {message:?}"
            );
        }
    }
}
