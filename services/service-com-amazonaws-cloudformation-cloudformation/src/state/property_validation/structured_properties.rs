// SPDX-License-Identifier: CC0-1.0

//! The STRUCTURE a property's value has to have: a property whose type is a LIST or an OBJECT,
//! carrying a value that is neither.
//!
//! A resource type's schema states a structural type for every property that is not a scalar —
//! a list of items, or an object with members — and a declaration carrying a SCALAR under such
//! a property is one no handler of that type could ever be given: the stage that runs before
//! provisioning reports it, naming the property, the structure its schema states and the
//! structure the template wrote, and the operation deploys nothing.
//!
//! This is the other half of the first question [`super::typed_properties`] answers: that
//! module holds the properties whose schema states a SCALAR type and judges the spelling of a
//! scalar, this one holds the properties whose schema states a STRUCTURE and judges whether
//! the value is one at all. They are separate catalogues because they are separate questions
//! with separate sentences — a scalar property's refusal quotes the type the property is held
//! to, a structural one quotes both the structure expected and the structure found
//! ([`type_mismatch_reason`]) — which is the same split the sibling questions of this stage
//! are already in.
//!
//! ## What this stage deliberately does not judge
//!
//! The judgement is made of the DEPLOYED property document, as every judgement of that stage
//! is, and over the same kind of catalogue: one row per (resource type, property) whose
//! structural type this emulator holds, so a property no row covers is left to the service
//! that owns the resource rather than guessed at.
//!
//! Only a value whose structure is DECIDABLE here is judged:
//!
//! * an OBJECT carried under a list-valued property is passed over, because an expression this
//!   service left unevaluated is written as an object (`{"Fn::GetAtt": […]}`) and the value it
//!   stands for is a list as often as not — judging it would refuse a declaration that
//!   deploys. A LIST under an object-valued property is judged: no expression is written as
//!   one;
//! * a text carrying a DYNAMIC REFERENCE is not the value the owning service will be given at
//!   all, since this stage runs before a deployment resolves those references
//!   ([`crate::state::dynamic_references`]), so nothing about the value one stands for can be
//!   decided here;
//! * a property the deployed document does not carry, and one whose value is absent, have no
//!   structure to judge.

use ::serde_json::Value;

use super::{declared_properties::property_path, PropertyFailure, StoredResource};

/// The STRUCTURE a property's schema states for its value, over the structures this stage
/// judges.
///
/// The token each renders in a refusal ([`Structure::expected`]) is the JSON type name the
/// template validator puts in that sentence, which is the name of the JSON structure itself
/// rather than the prose the property reference spells it with — so a property documented
/// "Type: Array of String" is reported as `JSONArray` and one documented with a structure or a
/// map as `JSONObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structure {
    /// A list of items ("Type: Array of …" in the property reference).
    Array,
    /// An object with members: a property type's own structure, or a map ("Type: Object").
    Object,
}

impl Structure {
    /// The structure's token, as the validator's sentence renders it.
    pub fn expected(self) -> &'static str {
        match self {
            Self::Array => "JSONArray",
            Self::Object => "JSONObject",
        }
    }

    /// The structure `declared` IS, as that same sentence names it — `None` for a value whose
    /// structure this stage does not decide against this one (the module docs say which, and
    /// why each is left alone).
    pub fn found(self, declared: &Value) -> Option<&'static str> {
        match declared {
            Value::Array(_) => (self != Self::Array).then_some("JSONArray"),
            // An object is the stated structure where one is stated, and where a LIST is
            // stated it is passed over: an expression this service left unevaluated is written
            // as an object, and what it stands for is a list as often as not.
            Value::Object(_) => None,
            // A whole number and a fraction are different JSON types, and the sentence names
            // the one the template wrote.
            Value::Number(number) => Some(if number.is_i64() || number.is_u64() {
                "Integer"
            } else {
                "Double"
            }),
            Value::Bool(_) => Some("Boolean"),
            Value::String(text) => {
                (!crate::state::dynamic_references::carries_a_reference(text)).then_some("String")
            }
            // A property whose value is absent has no structure to judge.
            Value::Null => None,
        }
    }
}

/// One property whose structural type its resource type's schema states.
#[derive(Debug, Clone, Copy)]
pub struct StructuredProperty {
    /// The resource type the property belongs to, as a template declares it.
    pub resource_type: &'static str,
    /// The property's name, as a template spells it.
    pub property: &'static str,
    /// The structure its schema states for the property.
    pub structure: Structure,
}

/// The properties this stage knows a stated STRUCTURE of.
///
/// Source for each row: the `Type` the resource type's entry in the "AWS resource and property
/// types reference" of the CloudFormation User Guide publishes for that property — "Array of
/// …" for a list and a property type or map for an object — which is the `type` the type's
/// registry schema states, and which, for a property that IS a member of the owning API, is
/// the form that API declares for the member the property is passed to (named beside each row
/// below), so a value of another structure is one that service could not be given.
///
/// A property documented `Type: Json` is deliberately NO row: what such a property carries is
/// a document whose shape its own service decides, and the text of one is a spelling of it.
pub const STRUCTURED_PROPERTIES: &[StructuredProperty] = &[
    // `AWS::ApiGatewayV2::Authorizer`'s `IdentitySource` is `CreateAuthorizer$IdentitySource`,
    // "the identity source for which authorization is requested", which that API declares a
    // list (`IdentitySourceList`); its `JwtConfiguration` is that call's `JwtConfiguration`,
    // the `JWTConfiguration` structure naming the issuer and audience of a JWT authorizer.
    StructuredProperty {
        resource_type: crate::state::template::APIGATEWAYV2_AUTHORIZER,
        property: "IdentitySource",
        structure: Structure::Array,
    },
    StructuredProperty {
        resource_type: crate::state::template::APIGATEWAYV2_AUTHORIZER,
        property: "JwtConfiguration",
        structure: Structure::Object,
    },
    // `AWS::ApiGatewayV2::Api`'s `CorsConfiguration` is `CreateApi$CorsConfiguration`, the
    // `Cors` structure holding a cross-origin configuration.
    StructuredProperty {
        resource_type: crate::state::template::APIGATEWAYV2_API,
        property: "CorsConfiguration",
        structure: Structure::Object,
    },
    // `AWS::ApiGatewayV2::Route`'s `AuthorizationScopes` is
    // `CreateRoute$AuthorizationScopes`, "the authorization scopes supported by this route",
    // declared a list; its `RequestParameters` is that call's `RequestParameters`, a map of
    // parameter constraints keyed by parameter.
    StructuredProperty {
        resource_type: crate::state::template::APIGATEWAYV2_ROUTE,
        property: "AuthorizationScopes",
        structure: Structure::Array,
    },
    StructuredProperty {
        resource_type: crate::state::template::APIGATEWAYV2_ROUTE,
        property: "RequestParameters",
        structure: Structure::Object,
    },
    // `AWS::Cognito::UserPool`'s `Schema` is `CreateUserPool$Schema`, "an array of schema
    // attributes for the new user pool"; its `Policies` is that call's `Policies`, the
    // `UserPoolPolicyType` structure holding the pool's password policy.
    StructuredProperty {
        resource_type: crate::state::template::COGNITO_USER_POOL,
        property: "Schema",
        structure: Structure::Array,
    },
    StructuredProperty {
        resource_type: crate::state::template::COGNITO_USER_POOL,
        property: "Policies",
        structure: Structure::Object,
    },
    // `AWS::Lambda::Function`'s `Code` is `CreateFunction$Code`, "the code for the function",
    // the `FunctionCode` structure; its `Environment` is that call's `Environment`, "the
    // function's environment variables"; its `Layers` is that call's `Layers`, "a list of
    // function layers to add to the function's execution environment", declared a list.
    StructuredProperty {
        resource_type: "AWS::Lambda::Function",
        property: "Code",
        structure: Structure::Object,
    },
    StructuredProperty {
        resource_type: "AWS::Lambda::Function",
        property: "Environment",
        structure: Structure::Object,
    },
    StructuredProperty {
        resource_type: "AWS::Lambda::Function",
        property: "Layers",
        structure: Structure::Array,
    },
    // `AWS::DynamoDB::Table`'s `AttributeDefinitions` and `KeySchema` are
    // `CreateTable$AttributeDefinitions` ("an array of attributes that describe the key schema
    // for the table") and `CreateTable$KeySchema` ("specifies the attributes that make up the
    // primary key for a table"), both declared lists; its `ProvisionedThroughput` is that
    // call's `ProvisionedThroughput`, the structure holding the table's read and write
    // capacity.
    StructuredProperty {
        resource_type: "AWS::DynamoDB::Table",
        property: "AttributeDefinitions",
        structure: Structure::Array,
    },
    StructuredProperty {
        resource_type: "AWS::DynamoDB::Table",
        property: "KeySchema",
        structure: Structure::Array,
    },
    StructuredProperty {
        resource_type: "AWS::DynamoDB::Table",
        property: "ProvisionedThroughput",
        structure: Structure::Object,
    },
    // `AWS::SQS::QueuePolicy`'s `Queues` and `AWS::SNS::TopicPolicy`'s `Topics` are the
    // resources the policy is installed on — "the URLs of the queues to which you want to add
    // the policy", "the Amazon Resource Names (ARN) of the topics to which you want to add the
    // policy" — each published as an array of strings.
    StructuredProperty {
        resource_type: "AWS::SQS::QueuePolicy",
        property: "Queues",
        structure: Structure::Array,
    },
    StructuredProperty {
        resource_type: "AWS::SNS::TopicPolicy",
        property: "Topics",
        structure: Structure::Array,
    },
    // `AWS::EC2::SecurityGroup`'s `SecurityGroupIngress` is the group's inbound rules, "the
    // inbound rules associated with the security group", published as an array of the `Ingress`
    // property type.
    StructuredProperty {
        resource_type: "AWS::EC2::SecurityGroup",
        property: "SecurityGroupIngress",
        structure: Structure::Array,
    },
];

/// `Some(failure)` for a resource carrying a value whose structure its type's schema does not
/// state for the property it sits under ([`STRUCTURED_PROPERTIES`]) — the first such property
/// in the order the catalogue states them; `None` for a type with no row, for a property the
/// declaration does not carry, for a value whose structure this stage does not decide (the
/// module docs say which), and for every value of the stated structure.
pub(super) fn value_of_another_structure(resource: &StoredResource) -> Option<PropertyFailure> {
    STRUCTURED_PROPERTIES
        .iter()
        .filter(|row| row.resource_type == resource.resource_type)
        .find_map(|row| {
            let declared = resource.properties.get(row.property)?;
            let found = row.structure.found(declared)?;
            Some(PropertyFailure {
                logical_id: resource.logical_id.clone(),
                resource_type: resource.resource_type.clone(),
                reason: type_mismatch_reason(row.property, row.structure, found),
                path: property_path(&resource.logical_id, row.property),
            })
        })
}

/// How CloudFormation reports a value whose structure its property's schema does not state:
/// fixed text around the PROPERTY it is about, the structure that property is held to and the
/// structure the template wrote, each named as the validator names a JSON type
/// ([`Structure::expected`], [`Structure::found`]).
///
/// The value itself is deliberately NOT quoted back, as it is not for the two type failures
/// beside it ([`super::typed_properties::type_failure_reason`]): it may be a value a template
/// author marked `NoEcho`, and what the author has to act on is the structure the property
/// states.
pub fn type_mismatch_reason(property: &str, expected: Structure, found: &str) -> String {
    format!(
        "Property [{property}] expected type: {}, found: {found}",
        expected.expected()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::serde_json::json;
    use ::std::collections::BTreeMap;

    /// One declared resource of `resource_type` carrying `properties`.
    fn declared(logical_id: &str, resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: logical_id.to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: format!("{logical_id}-physical"),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// The type whose list-valued property the report leading here was filed about.
    const AUTHORIZER: &str = crate::state::template::APIGATEWAYV2_AUTHORIZER;

    /// One row of the table below: the case, the resource type declared, the property document
    /// it carries, and the `(property, expected, found)` the stage must report — `None` where
    /// every judged value has the structure its property states.
    type StructureRow<'a> = (&'a str, &'a str, Value, Option<(&'a str, Structure, &'a str)>);

    /// RULE: a value whose structure its property's schema does not state fails the validation,
    /// naming that property, the structure stated and the structure written at the property's
    /// own path, while a value of the stated structure does not.
    ///
    /// The rows span what the rule generalises over: which structure is stated (list, object),
    /// which resource type and property states it (six owning services, properties the report
    /// that led here did not cite among them), and the form the value takes (text, a whole
    /// number, a fraction, a flag, a list where an object is stated, an expression this service
    /// left unevaluated, a dynamic reference nothing has resolved yet, a value that has not
    /// landed, and a property the document does not carry at all).
    #[test]
    fn a_value_of_another_structure_fails_the_validation() {
        let rows: &[StructureRow] = &[
            (
                "an authorizer whose identity source is one plain string",
                AUTHORIZER,
                json!({"Name": "a", "AuthorizerType": "JWT", "IdentitySource": "$request.header.Authorization"}),
                Some(("IdentitySource", Structure::Array, "String")),
            ),
            (
                "an authorizer whose identity source is the list it states",
                AUTHORIZER,
                json!({"Name": "a", "IdentitySource": ["$request.header.Authorization"]}),
                None,
            ),
            (
                "an authorizer whose JWT configuration is a string",
                AUTHORIZER,
                json!({"Name": "a", "JwtConfiguration": "issuer"}),
                Some(("JwtConfiguration", Structure::Object, "String")),
            ),
            (
                "an authorizer whose JWT configuration is a LIST of configurations",
                AUTHORIZER,
                json!({"Name": "a", "JwtConfiguration": [{"Issuer": "https://i"}]}),
                Some(("JwtConfiguration", Structure::Object, "JSONArray")),
            ),
            (
                "an authorizer whose identity source is an empty list",
                AUTHORIZER,
                json!({"Name": "a", "IdentitySource": []}),
                None,
            ),
            (
                "an authorizer carrying none of the properties this stage judges",
                AUTHORIZER,
                json!({"Name": "a", "AuthorizerType": "JWT"}),
                None,
            ),
            (
                "an authorizer whose identity source is an expression left unevaluated",
                AUTHORIZER,
                json!({"Name": "a", "IdentitySource": {"Fn::Split": [",", "a,b"]}}),
                None,
            ),
            (
                "an authorizer whose identity source is a dynamic reference nothing resolved",
                AUTHORIZER,
                json!({"Name": "a", "IdentitySource": "{{resolve:ssm:/api/identity-source}}"}),
                None,
            ),
            (
                "an authorizer whose identity source has not landed yet",
                AUTHORIZER,
                json!({"Name": "a", "IdentitySource": Value::Null}),
                None,
            ),
            (
                "an api whose CORS configuration is a flag",
                crate::state::template::APIGATEWAYV2_API,
                json!({"Name": "api", "ProtocolType": "HTTP", "CorsConfiguration": true}),
                Some(("CorsConfiguration", Structure::Object, "Boolean")),
            ),
            (
                "a route whose authorization scopes are one plain string",
                crate::state::template::APIGATEWAYV2_ROUTE,
                json!({"ApiId": "abc", "RouteKey": "GET /", "AuthorizationScopes": "openid"}),
                Some(("AuthorizationScopes", Structure::Array, "String")),
            ),
            (
                "a user pool whose schema is a whole number",
                crate::state::template::COGNITO_USER_POOL,
                json!({"UserPoolName": "pool", "Schema": 3}),
                Some(("Schema", Structure::Array, "Integer")),
            ),
            (
                "a user pool whose password policy is a fraction",
                crate::state::template::COGNITO_USER_POOL,
                json!({"UserPoolName": "pool", "Policies": 1.5}),
                Some(("Policies", Structure::Object, "Double")),
            ),
            (
                "a function whose layers are one plain string",
                "AWS::Lambda::Function",
                json!({"Role": "arn:aws:iam::000000000000:role/r",
                       "Layers": "arn:aws:lambda:us-east-1:000000000000:layer:l:1"}),
                Some(("Layers", Structure::Array, "String")),
            ),
            (
                "a function whose code is a string",
                "AWS::Lambda::Function",
                json!({"Role": "arn:aws:iam::000000000000:role/r", "Code": "s3://b/k"}),
                Some(("Code", Structure::Object, "String")),
            ),
            (
                "a table whose key schema is a flag",
                "AWS::DynamoDB::Table",
                json!({"KeySchema": false}),
                Some(("KeySchema", Structure::Array, "Boolean")),
            ),
            (
                "a table whose provisioned throughput is a whole number",
                "AWS::DynamoDB::Table",
                json!({"ProvisionedThroughput": 5}),
                Some(("ProvisionedThroughput", Structure::Object, "Integer")),
            ),
            (
                "a queue policy whose queues are one plain url",
                "AWS::SQS::QueuePolicy",
                json!({"PolicyDocument": {"Version": "2012-10-17"},
                       "Queues": "https://sqs.us-east-1.amazonaws.com/000000000000/q"}),
                Some(("Queues", Structure::Array, "String")),
            ),
            (
                "a topic policy whose topics are the list they state",
                "AWS::SNS::TopicPolicy",
                json!({"PolicyDocument": {"Version": "2012-10-17"},
                       "Topics": ["arn:aws:sns:us-east-1:000000000000:t"]}),
                None,
            ),
            (
                "a security group whose inbound rules are one plain string",
                "AWS::EC2::SecurityGroup",
                json!({"GroupDescription": "g", "SecurityGroupIngress": "0.0.0.0/0"}),
                Some(("SecurityGroupIngress", Structure::Array, "String")),
            ),
            (
                "a type this stage knows no stated structure of",
                "AWS::SNS::Topic",
                json!({"Tags": "not-a-list"}),
                None,
            ),
        ];
        for (case, resource_type, properties, reported) in rows {
            let resources = vec![declared("R", resource_type, properties.clone())];
            let failed = super::super::failure(&resources);
            let expected = reported.map(|(property, expected, found)| {
                (
                    type_mismatch_reason(property, expected, found),
                    format!("/Resources/R/Properties/{property}"),
                )
            });
            assert_eq!(
                failed
                    .as_ref()
                    .map(|failure| (failure.reason.clone(), failure.path.clone())),
                expected,
                "case {case:?}: expected the failure {expected:?}, got {failed:?}"
            );
        }
    }
}
