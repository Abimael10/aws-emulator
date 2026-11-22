// SPDX-License-Identifier: CC0-1.0

//! The constraints a user pool's schema declares for an attribute, enforced on
//! every write of that attribute, driven through the router exactly as an SDK
//! drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: a schema attribute carries the constraints of its own
//! data type — `NumberAttributeConstraints` with `MinValue`/`MaxValue` for a
//! `Number` attribute and `StringAttributeConstraints` with
//! `MinLength`/`MaxLength` for a `String` one (`SchemaAttributeType` in this
//! service's model), and a `Number` attribute's values have the FORM of a
//! number — and a value submitted for that attribute has to satisfy them,
//! whichever operation submits it. A value that misses one is refused with 400
//! `InvalidParameterException`, and WHICH sentence that refusal carries is the
//! validation STAGE that judged it:
//!
//! * a REGISTRATION of a new account (`SignUp`, `AdminCreateUser`) states the
//!   schema validation's envelope, "Attributes did not conform to the schema: ",
//!   then the attribute's SCHEMA PATH (a custom attribute under `Custom.User.`,
//!   carrying the name it was declared with rather than the prefixed name the
//!   request wrote) and the validation it did not satisfy — one finding, with no
//!   trailing newline;
//! * an UPDATE of a stored user's attributes (`AdminUpdateUserAttributes`,
//!   `UpdateUserAttributes`, a `NEW_PASSWORD_REQUIRED` response) states the
//!   per-attribute report: a line per offending attribute naming it under the
//!   `user.` prefix and the bound it missed as the schema declares it, each line
//!   ending in a newline.
//!
//! A value that is not of its data type's form is reported by both stages in the
//! constraint envelope naming the pattern it failed, the registration stage
//! stating its own prefix ahead of it. A refused write stores nothing, so the
//! user keeps every value it held (and a refused create leaves no user at all).
//! Both bounds are INCLUSIVE, an undeclared bound bounds nothing, and a length is
//! counted in characters.
//!
//! The rows span the class the rule generalises over: both data types, both
//! bounds of each, bounds declared singly and in pairs, values at the bound,
//! one step past it and far past it, the forms a number may be written in
//! (signed, fractional, exponent) and values that are no number at all, a
//! non-ASCII value whose characters and bytes differ, an attribute with no
//! constraints at all, and the standard attributes the default schema bounds
//! (`birthdate`, exactly ten characters; `updated_at`, the standard schema's one
//! `Number`; `name`, the 2048-character string bound) beside custom attributes
//! declared by `CreateUserPool.Schema` and by `AddCustomAttributes` — every row
//! driven through BOTH stages, so each row states both wordings.
//!
//! Every pool, schema and user here is real: they come from `CreateUserPool`,
//! `AddCustomAttributes`, `AdminCreateUser` and `AdminUpdateUserAttributes`, and
//! every write is read back through `AdminGetUser`. The wire harness is the one
//! `log_delivery_configuration.rs` already builds for this service, reached
//! through the test crate root rather than copied.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::log_delivery_configuration::{call, error_code, error_message, ok, router};

/// The constraint a row's value misses, which each stage of the schema validation
/// states in its own words.
enum Missed {
    /// A bound the schema declares: the VALIDATION the registration stage names
    /// it by, and the sentence the update stage states the declared bound in.
    Bound {
        validation: &'static str,
        sentence: &'static str,
    },
    /// The form a `Number` attribute's values take, which both stages report in
    /// the constraint envelope.
    NumberForm,
}

/// A bound missed, as both stages name it.
fn bound(validation: &'static str, sentence: &'static str) -> Missed {
    Missed::Bound {
        validation,
        sentence,
    }
}

/// One row of the table: an attribute of the pool this suite builds, a value
/// submitted for it, and the constraint that value misses — `None` where the
/// value satisfies every constraint declared for it.
struct Row {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// The attribute the value is submitted for, under its STORED name.
    attribute: &'static str,
    /// The submitted value.
    value: String,
    /// The constraint the value misses, or `None` for a value the schema admits.
    missed: Option<Missed>,
}

impl Row {
    /// A row whose value the schema admits.
    fn admitted(case: &'static str, attribute: &'static str, value: impl Into<String>) -> Self {
        Self {
            case,
            attribute,
            value: value.into(),
            missed: None,
        }
    }

    /// A row whose value misses `missed`.
    fn refused(
        case: &'static str,
        attribute: &'static str,
        value: impl Into<String>,
        missed: Missed,
    ) -> Self {
        Self {
            case,
            attribute,
            value: value.into(),
            missed: Some(missed),
        }
    }

    /// A row whose value is no number at all for a `Number` attribute.
    fn not_a_number(case: &'static str, attribute: &'static str, value: impl Into<String>) -> Self {
        Self::refused(case, attribute, value, Missed::NumberForm)
    }

    /// The message a REGISTRATION refuses this row's value with.
    fn registration_refusal(&self) -> Option<String> {
        self.missed.as_ref().map(|missed| match missed {
            Missed::Bound { validation, .. } => registration_bound(self.attribute, validation),
            Missed::NumberForm => {
                format!("{SCHEMA_ENVELOPE}{}", constraint_envelope(self.attribute))
            }
        })
    }

    /// The report an UPDATE refuses this row's value with.
    fn update_refusal(&self) -> Option<String> {
        self.missed.as_ref().map(|missed| match missed {
            Missed::Bound { sentence, .. } => report_line(self.attribute, sentence),
            Missed::NumberForm => constraint_envelope(self.attribute),
        })
    }
}

/// The envelope every refusal of the REGISTRATION stage opens with.
const SCHEMA_ENVELOPE: &str = "Attributes did not conform to the schema: ";

/// The pattern the values of a `Number` attribute satisfy, which the refusal of a
/// value that does not states as the constraint it failed.
const NUMBER_PATTERN: &str = r"$|^[+-]?((\p{Nd}+(\.\p{Nd}*)?)|(\.\p{Nd}+))([eE][+-]?\p{Nd}+)?";

/// The path the pool's schema holds `attribute` under, which is how a
/// registration-stage refusal names it: a custom attribute under `Custom.User.`
/// with the name it was declared with (`SchemaAttributeType.Name`: a `Name` of
/// `MyAttribute` is created as `custom:MyAttribute`), a standard attribute under
/// its own name.
fn schema_path(attribute: &str) -> String {
    match attribute
        .strip_prefix("custom:")
        .or_else(|| attribute.strip_prefix("dev:"))
    {
        Some(declared) => format!("Custom.User.{declared}"),
        None => attribute.to_string(),
    }
}

/// The refusal a REGISTRATION answers a value outside a declared bound with: the
/// envelope, the attribute's schema path and the validation that bound is checked
/// by.
fn registration_bound(attribute: &str, validation: &str) -> String {
    format!(
        "{SCHEMA_ENVELOPE}{}: This attribute does not satisfy the {validation} validation \
         defined in the schema.",
        schema_path(attribute)
    )
}

/// The line an UPDATE's report states for `attribute`: the attribute under the
/// `user.` prefix a user's own attributes are validated under, the sentence, and
/// the newline every line of that report ends with.
fn report_line(attribute: &str, sentence: &str) -> String {
    format!("user.{attribute}: {sentence}\n")
}

/// The constraint envelope a value that is of the wrong FORM is refused with: the
/// position of the value and the pattern it failed to satisfy.
fn constraint_envelope(attribute: &str) -> String {
    format!(
        "1 validation error detected: Value at '{}' failed to satisfy constraint: Member must \
         satisfy regular expression pattern: {NUMBER_PATTERN}",
        schema_path(attribute)
    )
}

/// The validations a declared bound is checked by, as a registration-stage refusal
/// names them.
const MAX_VALUE: &str = "MAX_VALUE_VALIDATION";
const MIN_VALUE: &str = "MIN_VALUE_VALIDATION";
const MAX_LENGTH: &str = "MAX_LENGTH_VALIDATION";
const MIN_LENGTH: &str = "MIN_LENGTH_VALIDATION";

/// The pool this suite writes to: the custom attributes its create declares,
/// those `AddCustomAttributes` appends afterwards, and the minted pool id.
///
/// Two declaration paths rather than one, because the rule is about what the
/// SCHEMA declares and not about which call put it there.
async fn pool_with_declared_constraints(router: &MultiServiceRouter) -> String {
    let created = ok(
        router,
        "CreateUserPool",
        &json!({
            "PoolName": "declared-constraints",
            "Schema": [
                {
                    "Name": "rank",
                    "AttributeDataType": "Number",
                    "Mutable": true,
                    "NumberAttributeConstraints": { "MinValue": "1", "MaxValue": "10" },
                },
                {
                    "Name": "tenant",
                    "AttributeDataType": "String",
                    "Mutable": true,
                    "StringAttributeConstraints": { "MinLength": "2", "MaxLength": "8" },
                },
            ],
        }),
    )
    .await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("CreateUserPool mints a pool id")
        .to_string();

    ok(
        router,
        "AddCustomAttributes",
        &json!({
            "UserPoolId": user_pool_id,
            "CustomAttributes": [
                {
                    "Name": "level",
                    "AttributeDataType": "Number",
                    "Mutable": true,
                    "NumberAttributeConstraints": { "MaxValue": "10" },
                },
                {
                    "Name": "floor",
                    "AttributeDataType": "Number",
                    "Mutable": true,
                    "NumberAttributeConstraints": { "MinValue": "5" },
                },
                {
                    "Name": "label",
                    "AttributeDataType": "String",
                    "Mutable": true,
                    "StringAttributeConstraints": { "MaxLength": "4" },
                },
                { "Name": "free", "AttributeDataType": "String", "Mutable": true },
            ],
        }),
    )
    .await;
    user_pool_id
}

/// Every row of the table, built once so both write paths below drive the same
/// cases.
fn rows() -> Vec<Row> {
    vec![
        // A Number bounded on both sides: the bounds themselves are admitted,
        // one step past either is not, and so is a value far past it.
        Row::admitted("a number at the declared maximum", "custom:rank", "10"),
        Row::admitted("a number at the declared minimum", "custom:rank", "1"),
        Row::admitted("a number inside the declared range", "custom:rank", "7"),
        Row::refused(
            "a number one past the declared maximum",
            "custom:rank",
            "11",
            bound(MAX_VALUE, "Number must be less than 10"),
        ),
        Row::refused(
            "a number far past the declared maximum",
            "custom:rank",
            "99",
            bound(MAX_VALUE, "Number must be less than 10"),
        ),
        Row::refused(
            "a number one under the declared minimum",
            "custom:rank",
            "0",
            bound(MIN_VALUE, "Number must be greater than 1"),
        ),
        Row::refused(
            "a negative number under the declared minimum",
            "custom:rank",
            "-5",
            bound(MIN_VALUE, "Number must be greater than 1"),
        ),
        // The FORMS a number may be written in, which the pattern admits: an
        // explicit sign, a fraction and an exponent.
        Row::admitted("a signed number inside the declared range", "custom:rank", "+7"),
        Row::admitted("a fractional number inside the declared range", "custom:rank", "7.5"),
        Row::admitted("an exponent number inside the declared range", "custom:rank", "1e0"),
        // And the values that are no number at all, which no bound can be
        // compared against.
        Row::not_a_number("letters where a number belongs", "custom:rank", "abc"),
        Row::not_a_number("a number followed by a space", "custom:rank", "7 "),
        Row::not_a_number("a number written with a thousands separator", "custom:rank", "1,5"),
        Row::not_a_number("a number with two decimal points", "custom:rank", "7.5.1"),
        // A Number bounded above only: nothing bounds it from below.
        Row::refused(
            "a number past an attribute's only declared maximum",
            "custom:level",
            "11",
            bound(MAX_VALUE, "Number must be less than 10"),
        ),
        Row::admitted(
            "a number far below an attribute that declares no minimum",
            "custom:level",
            "-1000",
        ),
        // A Number bounded below only.
        Row::refused(
            "a number under an attribute's only declared minimum",
            "custom:floor",
            "4",
            bound(MIN_VALUE, "Number must be greater than 5"),
        ),
        Row::refused(
            "a fraction under an attribute's only declared minimum",
            "custom:floor",
            ".5",
            bound(MIN_VALUE, "Number must be greater than 5"),
        ),
        Row::admitted(
            "an exponent number above the declared minimum",
            "custom:floor",
            "1e1",
        ),
        Row::admitted(
            "a number far above an attribute that declares no maximum",
            "custom:floor",
            "2000000",
        ),
        // A String bounded on both sides, counted in characters.
        Row::admitted("a string at the declared maximum length", "custom:tenant", "exactly8"),
        Row::admitted("a string at the declared minimum length", "custom:tenant", "ab"),
        Row::refused(
            "a string one character past the declared maximum",
            "custom:tenant",
            "exactly89",
            bound(MAX_LENGTH, "String must be no longer than 8 characters"),
        ),
        Row::refused(
            "a string far past the declared maximum",
            "custom:tenant",
            "waytoolongvalue",
            bound(MAX_LENGTH, "String must be no longer than 8 characters"),
        ),
        Row::refused(
            "a string shorter than the declared minimum",
            "custom:tenant",
            "x",
            bound(MIN_LENGTH, "String must be no shorter than 2 characters"),
        ),
        // A String bounded above only, with a non-ASCII row: the bound counts
        // characters, so a value of five two-byte characters is five long.
        Row::admitted("a string at an attribute's only declared maximum", "custom:label", "abcd"),
        Row::refused(
            "a string past an attribute's only declared maximum",
            "custom:label",
            "abcde",
            bound(MAX_LENGTH, "String must be no longer than 4 characters"),
        ),
        Row::refused(
            "a non-ASCII string past the declared maximum",
            "custom:label",
            "ééééé",
            bound(MAX_LENGTH, "String must be no longer than 4 characters"),
        ),
        Row::admitted(
            "a non-ASCII string whose characters are within the declared maximum",
            "custom:label",
            "éééé",
        ),
        // An attribute that declares no bound at all bounds nothing: the only
        // limit left is the modelled one on an attribute value
        // (`AttributeValueType`, length 0..2048).
        Row::admitted(
            "the longest modelled value of an attribute with no declared constraints",
            "custom:free",
            "v".repeat(2_048),
        ),
        // The standard attributes the default schema bounds, which no recorded
        // case cited: a fixed-length date, the schema's one Number, and the
        // 2048-character string bound.
        Row::admitted("a date of the declared length", "birthdate", "1990-01-01"),
        Row::refused(
            "a date one character short",
            "birthdate",
            "1990-01-0",
            bound(MIN_LENGTH, "String must be no shorter than 10 characters"),
        ),
        Row::refused(
            "a date one character long",
            "birthdate",
            "1990-01-011",
            bound(MAX_LENGTH, "String must be no longer than 10 characters"),
        ),
        Row::admitted("a standard number at its declared minimum", "updated_at", "0"),
        Row::refused(
            "a standard number under its declared minimum",
            "updated_at",
            "-1",
            bound(MIN_VALUE, "Number must be greater than 0"),
        ),
        Row::not_a_number(
            "a standard number that is no number at all",
            "updated_at",
            "yesterday",
        ),
        Row::admitted("a standard string at its declared maximum", "name", "n".repeat(2_048)),
        // A value longer than that bound cannot be submitted at all: the
        // modelled `AttributeValueType` caps an attribute value at the same
        // 2048 characters, so the request is refused before the schema is
        // consulted — which is why no row sends one.
    ]
}

/// The attributes `AdminGetUser` reports for `username`, as a sorted list of
/// `(name, value)` pairs.
async fn stored_attributes(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
    username: &str,
) -> Vec<(String, String)> {
    let read = ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    let mut held: Vec<(String, String)> = read["UserAttributes"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: AdminGetUser reports attributes: {read}"))
        .iter()
        .map(|attribute| {
            (
                attribute["Name"].as_str().unwrap_or_default().to_string(),
                attribute["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    held.sort();
    held
}

/// The password a user of this suite signs in with, and the temporary one an
/// administrator creates it with — both satisfying the default pool policy.
const PASSWORD: &str = "Str0ng!Passw0rd";
const TEMPORARY_PASSWORD: &str = "Temp0rary!Pass";

/// The value every path below submits for `custom:rank` to be refused, and the
/// line the refusal reports for it: the attribute declares `MaxValue` 10.
const OUT_OF_BOUNDS_RANK: &str = "99";

/// The value every path below submits for `custom:rank` to be accepted.
const IN_BOUNDS_RANK: &str = "7";

/// An app client of `user_pool_id` that signs users in through the admin
/// password flow, and its minted `ClientId`. The client declares no
/// `WriteAttributes`, so it narrows nothing: what refuses a value is the pool's
/// schema alone.
async fn client_of(router: &MultiServiceRouter, user_pool_id: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "declared-constraints-app",
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("CreateUserPoolClient mints a client id")
        .to_string()
}

#[tokio::test]
async fn a_self_service_sign_up_is_judged_against_the_declared_constraints() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;
    let client_id = client_of(&router, &user_pool_id).await;

    // The refused registration first: a value past the declared maximum earns
    // the registration stage's own refusal, and leaves no user for the pool to
    // hold.
    let case = "a sign-up carrying a number past the declared maximum";
    let (status, payload) = call(
        &router,
        "SignUp",
        &json!({
            "ClientId": client_id,
            "Username": "signing-up-refused",
            "Password": PASSWORD,
            "UserAttributes": [{ "Name": "custom:rank", "Value": OUT_OF_BOUNDS_RANK }],
        }),
    )
    .await;
    let expected = registration_bound("custom:rank", MAX_VALUE);
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", expected.clone()),
        "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got {status} \
         with {payload}"
    );
    let (status, payload) = call(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": "signing-up-refused" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "UserNotFoundException"),
        "case {case:?}: a refused sign-up must register no user, got {status} with {payload}"
    );

    // The control: the same registration with a value inside the bounds is
    // served, so the refusal is about the value rather than about the path.
    let case = "a sign-up carrying a number inside the declared range";
    let (status, payload) = call(
        &router,
        "SignUp",
        &json!({
            "ClientId": client_id,
            "Username": "signing-up-served",
            "Password": PASSWORD,
            "UserAttributes": [{ "Name": "custom:rank", "Value": IN_BOUNDS_RANK }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a value inside the declared bounds registers the user, got {status} \
         with {payload}"
    );
    let held = stored_attributes(&router, case, &user_pool_id, "signing-up-served").await;
    assert!(
        held.iter()
            .any(|(name, value)| name == "custom:rank" && value == IN_BOUNDS_RANK),
        "case {case:?}: expected custom:rank to read back as the submitted value, got {held:?}"
    );
}

#[tokio::test]
async fn a_self_service_update_is_judged_against_the_declared_constraints() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;
    let client_id = client_of(&router, &user_pool_id).await;
    let username = "self-updating-user";
    ok(
        &router,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    ok(
        &router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let signed_in = ok(
        &router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": PASSWORD },
        }),
    )
    .await;
    let access_token = signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .expect("the sign-in hands out an access token")
        .to_string();

    // The user's own write is held to the same schema as an administrator's.
    let case = "a self-service update carrying a string past the declared maximum";
    let (status, payload) = call(
        &router,
        "UpdateUserAttributes",
        &json!({
            "AccessToken": access_token,
            "UserAttributes": [{ "Name": "custom:tenant", "Value": "waytoolongvalue" }],
        }),
    )
    .await;
    let expected = report_line("custom:tenant", "String must be no longer than 8 characters");
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", expected.clone()),
        "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got {status} \
         with {payload}"
    );
    let held = stored_attributes(&router, case, &user_pool_id, username).await;
    assert!(
        !held.iter().any(|(name, _)| name == "custom:tenant"),
        "case {case:?}: a refused update must write nothing, got {held:?}"
    );

    let case = "a self-service update carrying a string of the declared length";
    let (status, payload) = call(
        &router,
        "UpdateUserAttributes",
        &json!({
            "AccessToken": access_token,
            "UserAttributes": [{ "Name": "custom:tenant", "Value": "exactly8" }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a value inside the declared bounds is stored, got {status} with {payload}"
    );
    let held = stored_attributes(&router, case, &user_pool_id, username).await;
    assert!(
        held.iter()
            .any(|(name, value)| name == "custom:tenant" && value == "exactly8"),
        "case {case:?}: expected custom:tenant to read back as the submitted value, got {held:?}"
    );
}

/// Create `username` with a temporary password, start its sign-in and return the
/// `Session` of the `NEW_PASSWORD_REQUIRED` challenge it is handed: an
/// administrator-created user owes a password change, and that challenge's
/// response is the one that may carry attribute values.
async fn new_password_challenge_session(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
) -> String {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "TemporaryPassword": TEMPORARY_PASSWORD,
        }),
    )
    .await;
    let started = ok(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": TEMPORARY_PASSWORD },
        }),
    )
    .await;
    assert_eq!(
        started["ChallengeName"], "NEW_PASSWORD_REQUIRED",
        "case {username:?}: an administrator-created user owes a password change, got {started}"
    );
    started["Session"]
        .as_str()
        .expect("the challenge carries a session")
        .to_string()
}

#[tokio::test]
async fn a_new_password_challenge_response_is_judged_against_the_declared_constraints() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;
    let client_id = client_of(&router, &user_pool_id).await;

    let case = "a challenge response carrying a number past the declared maximum";
    let refused_user = "challenged-refused";
    let session =
        new_password_challenge_session(&router, &user_pool_id, &client_id, refused_user).await;
    let (status, payload) = call(
        &router,
        "AdminRespondToAuthChallenge",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "ChallengeName": "NEW_PASSWORD_REQUIRED",
            "Session": session,
            "ChallengeResponses": {
                "USERNAME": refused_user,
                "NEW_PASSWORD": PASSWORD,
                "userAttributes.custom:rank": OUT_OF_BOUNDS_RANK,
            },
        }),
    )
    .await;
    // The response writes the attributes of a user the pool already holds, so it
    // is judged at the UPDATE stage and carries that stage's report line.
    let expected = report_line("custom:rank", "Number must be less than 10");
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", expected.clone()),
        "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got {status} \
         with {payload}"
    );
    let held = stored_attributes(&router, case, &user_pool_id, refused_user).await;
    assert!(
        !held.iter().any(|(name, _)| name == "custom:rank"),
        "case {case:?}: a refused challenge response must write nothing, got {held:?}"
    );

    let case = "a challenge response carrying a number inside the declared range";
    let served_user = "challenged-served";
    let session =
        new_password_challenge_session(&router, &user_pool_id, &client_id, served_user).await;
    let (status, payload) = call(
        &router,
        "AdminRespondToAuthChallenge",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "ChallengeName": "NEW_PASSWORD_REQUIRED",
            "Session": session,
            "ChallengeResponses": {
                "USERNAME": served_user,
                "NEW_PASSWORD": PASSWORD,
                "userAttributes.custom:rank": IN_BOUNDS_RANK,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a value inside the declared bounds completes the sign-in, got {status} \
         with {payload}"
    );
    let held = stored_attributes(&router, case, &user_pool_id, served_user).await;
    assert!(
        held.iter()
            .any(|(name, value)| name == "custom:rank" && value == IN_BOUNDS_RANK),
        "case {case:?}: expected custom:rank to read back as the submitted value, got {held:?}"
    );
}

#[tokio::test]
async fn an_update_is_judged_against_the_declared_constraints_and_writes_nothing_when_refused() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;
    let username = "constrained-user";
    ok(
        &router,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;

    for row in rows() {
        let case = row.case;
        let held = stored_attributes(&router, case, &user_pool_id, username).await;
        let (status, payload) = call(
            &router,
            "AdminUpdateUserAttributes",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "UserAttributes": [{ "Name": row.attribute, "Value": row.value }],
            }),
        )
        .await;

        match row.update_refusal() {
            Some(expected) => {
                assert_eq!(
                    (
                        status,
                        error_code(&payload),
                        error_message(&payload).to_string()
                    ),
                    (400, "InvalidParameterException", expected.clone()),
                    "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got \
                     {status} with {payload}"
                );
                let after = stored_attributes(&router, case, &user_pool_id, username).await;
                assert_eq!(
                    after, held,
                    "case {case:?}: a refused update must leave every stored value intact, got \
                     {after:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a value inside the declared bounds is stored, got {status} \
                     with {payload}"
                );
                let after = stored_attributes(&router, case, &user_pool_id, username).await;
                assert!(
                    after
                        .iter()
                        .any(|(name, value)| name == row.attribute && *value == row.value),
                    "case {case:?}: expected {} to read back as the submitted value, got {after:?}",
                    row.attribute
                );
            }
        }
    }
}

#[tokio::test]
async fn a_create_is_judged_against_the_same_declared_constraints() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;

    for (at, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let username = format!("created-{at}");
        let (status, payload) = call(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "UserAttributes": [{ "Name": row.attribute, "Value": row.value }],
            }),
        )
        .await;

        match row.registration_refusal() {
            Some(expected) => {
                assert_eq!(
                    (
                        status,
                        error_code(&payload),
                        error_message(&payload).to_string()
                    ),
                    (400, "InvalidParameterException", expected.clone()),
                    "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got \
                     {status} with {payload}"
                );
                // A refused create leaves no user behind, so the read that
                // would find one is itself refused.
                let (status, payload) = call(
                    &router,
                    "AdminGetUser",
                    &json!({ "UserPoolId": user_pool_id, "Username": username }),
                )
                .await;
                assert_eq!(
                    (status, error_code(&payload)),
                    (400, "UserNotFoundException"),
                    "case {case:?}: a refused create must store no user, got {status} with \
                     {payload}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a value inside the declared bounds is stored, got {status} \
                     with {payload}"
                );
                let held = stored_attributes(&router, case, &user_pool_id, &username).await;
                assert!(
                    held.iter()
                        .any(|(name, value)| name == row.attribute && *value == row.value),
                    "case {case:?}: expected {} to read back as the submitted value, got {held:?}",
                    row.attribute
                );
            }
        }
    }
}

#[tokio::test]
async fn two_offending_attributes_of_one_request_are_reported_as_each_stage_reports_them() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;
    let username = "multi-offender";
    ok(
        &router,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;

    // Two attributes of different data types, each missing a different bound, in
    // one request. At the UPDATE stage the report carries a line for each, in the
    // order the request submitted them, so a client fixes both from one answer.
    let case = "an update with a number past its maximum beside a string past its maximum";
    let (status, payload) = call(
        &router,
        "AdminUpdateUserAttributes",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "custom:rank", "Value": "99" },
                { "Name": "custom:tenant", "Value": "waytoolongvalue" },
            ],
        }),
    )
    .await;
    let expected = format!(
        "{}{}",
        report_line("custom:rank", "Number must be less than 10"),
        report_line("custom:tenant", "String must be no longer than 8 characters")
    );
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", expected.clone()),
        "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got {status} \
         with {payload}"
    );

    // The same two attributes in the other order: the lines follow the request.
    let case = "the same two attributes, the string submitted first";
    let (status, payload) = call(
        &router,
        "AdminUpdateUserAttributes",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "custom:tenant", "Value": "waytoolongvalue" },
                { "Name": "custom:rank", "Value": "99" },
            ],
        }),
    )
    .await;
    let expected = format!(
        "{}{}",
        report_line("custom:tenant", "String must be no longer than 8 characters"),
        report_line("custom:rank", "Number must be less than 10")
    );
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", expected.clone()),
        "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got {status} \
         with {payload}"
    );

    // Neither value was written: the whole request was refused.
    let held = stored_attributes(&router, case, &user_pool_id, username).await;
    assert!(
        !held
            .iter()
            .any(|(name, _)| name == "custom:rank" || name == "custom:tenant"),
        "case {case:?}: a refused update must write neither value, got {held:?}"
    );

    // The REGISTRATION stage's message names one attribute and one validation, so
    // the same two attributes in one create earn the finding of the first of them,
    // and no user is left behind.
    let case = "a create with the same two offending attributes";
    let created = "multi-offender-create";
    let (status, payload) = call(
        &router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": created,
            "UserAttributes": [
                { "Name": "custom:rank", "Value": "99" },
                { "Name": "custom:tenant", "Value": "waytoolongvalue" },
            ],
        }),
    )
    .await;
    let expected = registration_bound("custom:rank", MAX_VALUE);
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", expected.clone()),
        "case {case:?}: expected a 400 InvalidParameterException {expected:?}, got {status} \
         with {payload}"
    );
    let (status, payload) = call(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": created }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "UserNotFoundException"),
        "case {case:?}: a refused create must store no user, got {status} with {payload}"
    );
}

#[tokio::test]
async fn a_blank_value_deletes_the_attribute_whatever_its_declared_minimum() {
    let router = router();
    let user_pool_id = pool_with_declared_constraints(&router).await;
    let username = "deleting-user";
    ok(
        &router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "custom:tenant", "Value": "tenant01" },
                { "Name": "custom:rank", "Value": "5" },
            ],
        }),
    )
    .await;

    // "To delete an attribute from your user, submit the attribute in your API
    // request with a blank value" (this service's model): a blank value stores
    // nothing, so a declared minimum does not refuse it. The rows cover both
    // data types that carry a minimum.
    for attribute in ["custom:tenant", "custom:rank"] {
        let case = format!("a blank value for {attribute}");
        let (status, payload) = call(
            &router,
            "AdminUpdateUserAttributes",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "UserAttributes": [{ "Name": attribute, "Value": "" }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a blank value deletes the attribute, got {status} with {payload}"
        );
        let held = stored_attributes(&router, &case, &user_pool_id, username).await;
        assert!(
            !held.iter().any(|(name, _)| name == attribute),
            "case {case:?}: the deleted attribute must be absent, got {held:?}"
        );
    }
}
