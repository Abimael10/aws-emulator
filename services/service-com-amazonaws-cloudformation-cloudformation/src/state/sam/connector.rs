// SPDX-License-Identifier: CC0-1.0

//! The CONNECTOR a SAM document declares (`AWS::Serverless::Connector`): the permissions its
//! profile publishes for the source/destination pair it connects, and what the transform
//! refuses when the declaration asks for anything else.
//!
//! A connector states "Source … Destination … Permissions: The permissions needed between the
//! source and destination resources", and the permissions available are not a free vocabulary:
//! the macro carries a PROFILE per source/destination resource-type pair, and that profile is
//! what decides which `Permissions` values the connector can be expanded for ("Supported source
//! and destination resource types" in the AWS SAM Developer Guide lists the pairs and the
//! permissions each pair supports). A connector asking for a permission its pair does not
//! publish is a declaration the macro can expand NOTHING for — the policies a connector becomes
//! are read out of that profile — so it is a fault of the document rather than something to
//! expand as written, and [`super::validation`] reports it with the other faults the macro
//! found, counted and named by the declaration it is about.
//!
//! What the diagnostic says is fixed apart from three things the submitted document and the
//! profile table decide: the two resource types the connector runs between, and the values the
//! profile publishes for them ([`PERMISSION_FAULT`]).
//!
//! ## Which types a connector's ends have
//!
//! An end is either a resource of the same document (`Id`, the ordinary form) or a resource
//! outside it (`Arn` plus the `Type` that names what the ARN is). A resource of the document is
//! the type the document declares it as — and for a declaration the macro EXPANDS, the type it
//! expands into ([`GENERATED_TYPES`]), because the connector is between the resources
//! CloudFormation ends up with: a connector from an `AWS::Serverless::Function` is a connector
//! from the `AWS::Lambda::Function` that declaration becomes.
//!
//! ## What a connector BECOMES
//!
//! A connector is no resource CloudFormation creates: it IS the grant between the two resources
//! it names, and the profile of the pair decides which form that grant takes. Where the source
//! is a resource of the document that executes as an IAM role — a function, a state machine —
//! the grant is a customer managed policy attached to that role, which is the
//! `AWS::IAM::ManagedPolicy` the macro adds under `<connector logical id>Policy`
//! ([`expanded`]): one statement per permission the connector asks for, carrying the actions
//! that permission's access category publishes for the destination's TYPE
//! ([`TARGETS`]), over the destination's own ARN and the sub-resources those actions reach. The
//! connector declaration itself is then gone from the processed template, as every type the
//! macro expands is.
//!
//! A pair whose grant is a policy on the DESTINATION instead (a queue policy, a topic policy, a
//! permission on an invoked function) is not expanded here, and neither is a connector whose
//! source executes as a role this document does not hold: such a declaration is left exactly as
//! written rather than turned into a resource nothing computed — the stance [`super`] takes for
//! every expansion it does not model — while its `Permissions` are judged all the same.
//!
//! ## What is left unjudged
//!
//! An end this module cannot place (one naming a logical id the document does not declare, or
//! stating its type through an intrinsic function CloudFormation resolves as it deploys), a pair
//! no profile here publishes, and a `Permissions` that is not a list of literal strings: each is
//! left to expand as declared rather than refused in words nothing published. Only the rule
//! above is judged here — the permissions a published pair supports — which is the one the macro
//! cannot expand past.
//!
//! ## Bounds
//!
//! One pass over the connector declarations of an already bounded document
//! ([`super::super::template::parse`] refuses an over-long body before anything here sees it),
//! and each one is judged against a fixed table, so nothing here grows with anything but the
//! document's own resource count. The grant one declaration becomes is bounded the same way:
//! one statement per destination it lists and permission it asks for, both of them parts of
//! that same parsed document, and no walk here recurses into a value at all.

use super::*;

/// The `Permissions` values AWS SAM publishes, as a connector's profiles name them.
const READ: &str = "Read";
const WRITE: &str = "Write";

/// The permission sets a profile publishes: a pair a connector can both read from and write
/// to, and one it can only write to.
const READ_AND_WRITE: &[&str] = &[READ, WRITE];
const WRITE_ONLY: &[&str] = &[WRITE];

/// The property a connector states the permissions it needs under.
const PERMISSIONS: &str = "Permissions";

/// One connector profile: the resource types it runs between, and the `Permissions` values it
/// publishes for that pair.
struct Profile {
    /// The type of the resource the connector's `Source` is.
    source: &'static str,
    /// The type of the resource the connector's `Destination` is.
    destination: &'static str,
    /// The permissions the pair supports, in the order the published table lists them — which
    /// is the order the diagnostic lists them in too.
    permissions: &'static [&'static str],
}

/// The profiles this module carries, as the published table of supported connections ("AWS SAM
/// connector reference" / "Supported source and destination resource types" in the AWS SAM
/// Developer Guide) lists them: one row per source and destination resource-type pair, carrying
/// the `Permissions` values that pair supports.
///
/// A pair outside this table is a pair this module judges nothing about (see the module docs),
/// never one it refuses.
const PROFILES: &[Profile] = &[
    // A function reaching the resources it is granted on.
    Profile {
        source: LAMBDA_FUNCTION,
        destination: DYNAMODB_TABLE,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: LAMBDA_FUNCTION,
        destination: S3_BUCKET,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: LAMBDA_FUNCTION,
        destination: SQS_QUEUE,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: LAMBDA_FUNCTION,
        destination: STATE_MACHINE_TYPE,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: LAMBDA_FUNCTION,
        destination: SNS_TOPIC,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: LAMBDA_FUNCTION,
        destination: EVENT_BUS,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: LAMBDA_FUNCTION,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    // A state machine reaching the resources its states act on.
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: DYNAMODB_TABLE,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: S3_BUCKET,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: STATE_MACHINE_TYPE,
        permissions: READ_AND_WRITE,
    },
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: SQS_QUEUE,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: SNS_TOPIC,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: EVENT_BUS,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: STATE_MACHINE_TYPE,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    // An event rule, an api and a notification source invoking what they target: a source that
    // only ever hands work to its destination.
    Profile {
        source: EVENTS_RULE,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: EVENTS_RULE,
        destination: SQS_QUEUE,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: EVENTS_RULE,
        destination: SNS_TOPIC,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: EVENTS_RULE,
        destination: STATE_MACHINE_TYPE,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: EVENTS_RULE,
        destination: EVENT_BUS,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: REST_API,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: HTTP_API_TYPE,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: SNS_TOPIC,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: SNS_TOPIC,
        destination: SQS_QUEUE,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: SQS_QUEUE,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: S3_BUCKET,
        destination: LAMBDA_FUNCTION,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: S3_BUCKET,
        destination: SQS_QUEUE,
        permissions: WRITE_ONLY,
    },
    Profile {
        source: S3_BUCKET,
        destination: SNS_TOPIC,
        permissions: WRITE_ONLY,
    },
];

/// The CloudFormation resource types the profiles above are written over.
const LAMBDA_FUNCTION: &str = "AWS::Lambda::Function";
const DYNAMODB_TABLE: &str = "AWS::DynamoDB::Table";
const S3_BUCKET: &str = "AWS::S3::Bucket";
const SQS_QUEUE: &str = "AWS::SQS::Queue";
const SNS_TOPIC: &str = "AWS::SNS::Topic";
const EVENT_BUS: &str = "AWS::Events::EventBus";
const EVENTS_RULE: &str = "AWS::Events::Rule";
const REST_API: &str = "AWS::ApiGateway::RestApi";
const HTTP_API_TYPE: &str = "AWS::ApiGatewayV2::Api";
const STATE_MACHINE_TYPE: &str = "AWS::StepFunctions::StateMachine";

/// What each type the macro EXPANDS becomes, which is the type a connector naming such a
/// declaration runs between ("Generated AWS CloudFormation resources" in the AWS SAM Developer
/// Guide, and [`super::expanded_resources`], which is what produces them here).
const GENERATED_TYPES: &[(&str, &str)] = &[
    (FUNCTION, LAMBDA_FUNCTION),
    (API, REST_API),
    (HTTP_API, HTTP_API_TYPE),
    (SIMPLE_TABLE, DYNAMODB_TABLE),
    (LAYER_VERSION, "AWS::Lambda::LayerVersion"),
    (STATE_MACHINE, STATE_MACHINE_TYPE),
];

/// `Some(detail)` for a connector whose `Permissions` the profile of its source/destination
/// pair does not publish, in the validator's own words — the whole sentence is fixed apart from
/// the two resource types and the list of values the profile publishes.
///
/// The FIRST destination at fault is the one reported, since a declaration is invalid for one
/// thing ([`super::validation`] reports at most one fault per declaration).
pub(super) fn fault(declaration: &Value, resources: &Map<String, Value>) -> Option<String> {
    let properties = object(declaration.get("Properties"));
    let source = endpoint_type(properties.get("Source"), resources)?;
    let requested = requested_permissions(properties.get(PERMISSIONS))?;
    destinations(properties.get("Destination"))
        .iter()
        .filter_map(|destination| endpoint_type(Some(destination), resources))
        .find_map(|destination| unsupported(&source, &destination, &requested))
}

/// `Some(detail)` where the pair has a profile and that profile publishes none of what the
/// connector asked for; `None` for a pair with no profile here, and for one whose profile
/// supports every permission requested.
///
/// A connector asking for NOTHING is in the same position as one asking for a value the pair
/// does not publish: no profile publishes an empty permission set, so there is nothing to
/// expand for it either.
fn unsupported(source: &str, destination: &str, requested: &[String]) -> Option<String> {
    let profile = PROFILES
        .iter()
        .find(|profile| profile.source == source && profile.destination == destination)?;
    let supported = |permission: &String| {
        profile
            .permissions
            .iter()
            .any(|published| published == permission)
    };
    if !requested.is_empty() && requested.iter().all(supported) {
        return None;
    }
    Some(format!(
        "Unsupported '{PERMISSIONS}' provided for connector from {source} to {destination}; \
         valid values are: {}.",
        profile.permissions.join(", ")
    ))
}

/// The permissions a connector asks for, or `None` where they are not a list of literal values
/// — one the template computes is resolved as CloudFormation deploys, which is not something
/// this validator can read.
fn requested_permissions(stated: Option<&Value>) -> Option<Vec<String>> {
    let listed = stated?.as_array()?;
    listed
        .iter()
        .map(|permission| permission.as_str().map(ToString::to_string))
        .collect()
}

/// The destinations a connector states: "Destination" is one resource reference or a list of
/// them, and a connector states permissions that hold for every one of them.
fn destinations(stated: Option<&Value>) -> Vec<Value> {
    match stated {
        Some(Value::Array(listed)) => listed.clone(),
        Some(Value::Object(members)) => vec![Value::Object(members.clone())],
        _ => Vec::new(),
    }
}

/// The resource type one end of a connector has: the `Type` the reference states for a resource
/// outside the document, and otherwise the type of the declaration its `Id` names — as the macro
/// EXPANDS that declaration, since the connector is between the resources CloudFormation ends up
/// with.
///
/// `None` for an end this module cannot place, which is an end it judges nothing about.
fn endpoint_type(stated: Option<&Value>, resources: &Map<String, Value>) -> Option<String> {
    let endpoint = stated?;
    if let Some(declared) = endpoint.get("Type").and_then(Value::as_str) {
        return Some(generated_type(declared).to_string());
    }
    let id = endpoint.get("Id").and_then(Value::as_str)?;
    let declared = resource_type(resources.get(id)?)?;
    Some(generated_type(declared).to_string())
}

/// The type a declaration of `declared` ends up as: the type the macro expands it into, or
/// `declared` itself for a type it leaves alone (every type CloudFormation owns).
fn generated_type(declared: &str) -> &str {
    GENERATED_TYPES
        .iter()
        .find(|(input, _)| *input == declared)
        .map_or(declared, |(_, generated)| *generated)
}

// ---------------------------------------------------------------------------
// What a connector BECOMES: the managed policy its grant is
// ---------------------------------------------------------------------------

/// One DESTINATION type a connector can grant on: where its ARN comes from, the sub-resources
/// a grant on it reaches, and the actions each permission's access category publishes.
struct Target {
    /// The resource type, as the profiles above name it.
    resource_type: &'static str,
    /// The attribute whose value is the resource's ARN, as that type's "Return values" names it
    /// ([`super::super::template::return_values`] is where this emulator answers them).
    arn_attribute: &'static str,
    /// What a grant reaches BESIDE the resource's own ARN, as the suffix the ARN of each carries
    /// — the indexes of a table, the objects in a bucket. Empty for a resource whose actions are
    /// all taken on the resource itself.
    scopes: &'static [&'static str],
    /// The actions reading this resource takes, or empty for a type with no `Read` category.
    read: &'static [&'static str],
    /// The actions writing to this resource takes, or empty for a type with no `Write` category.
    write: &'static [&'static str],
}

impl Target {
    /// The actions one permission's access category publishes for this type, or `None` where it
    /// publishes none.
    fn actions(&self, permission: &str) -> Option<&'static [&'static str]> {
        let published = match permission {
            READ => self.read,
            WRITE => self.write,
            _ => return None,
        };
        (!published.is_empty()).then_some(published)
    }
}

/// The destinations a grant is written over, with the actions each access category publishes:
/// the calls that permission is made of on that resource's own API, which is what the generated
/// policy has to allow for the connector to be the grant it stands for.
const TARGETS: &[Target] = &[
    Target {
        resource_type: DYNAMODB_TABLE,
        arn_attribute: "Arn",
        // A table's items are read and written through its own ARN and through the ARN of any
        // index of it, which is a resource of its own ("table/<name>/index/<index>").
        scopes: &["/index/*"],
        read: &[
            "dynamodb:GetItem",
            "dynamodb:Query",
            "dynamodb:Scan",
            "dynamodb:BatchGetItem",
            "dynamodb:ConditionCheckItem",
            "dynamodb:PartiQLSelect",
        ],
        write: &[
            "dynamodb:PutItem",
            "dynamodb:UpdateItem",
            "dynamodb:DeleteItem",
            "dynamodb:BatchWriteItem",
            "dynamodb:PartiQLInsert",
            "dynamodb:PartiQLUpdate",
            "dynamodb:PartiQLDelete",
        ],
    },
    Target {
        resource_type: S3_BUCKET,
        arn_attribute: "Arn",
        // The bucket's ARN is what a listing is taken on; an object's ARN is the bucket's
        // followed by its key.
        scopes: &["/*"],
        read: &[
            "s3:GetObject",
            "s3:GetObjectAcl",
            "s3:GetObjectVersion",
            "s3:ListBucket",
            "s3:ListBucketVersions",
            "s3:ListBucketMultipartUploads",
            "s3:ListMultipartUploadParts",
        ],
        write: &[
            "s3:PutObject",
            "s3:DeleteObject",
            "s3:AbortMultipartUpload",
            "s3:RestoreObject",
        ],
    },
    Target {
        resource_type: SQS_QUEUE,
        arn_attribute: "Arn",
        scopes: &[],
        read: &[
            "sqs:ReceiveMessage",
            "sqs:DeleteMessage",
            "sqs:ChangeMessageVisibility",
            "sqs:GetQueueAttributes",
            "sqs:GetQueueUrl",
        ],
        write: &[
            "sqs:SendMessage",
            "sqs:GetQueueAttributes",
            "sqs:GetQueueUrl",
        ],
    },
    Target {
        resource_type: SNS_TOPIC,
        arn_attribute: "TopicArn",
        scopes: &[],
        read: &[],
        write: &["sns:Publish"],
    },
    Target {
        resource_type: EVENT_BUS,
        arn_attribute: "Arn",
        scopes: &[],
        read: &[],
        write: &["events:PutEvents"],
    },
    Target {
        resource_type: LAMBDA_FUNCTION,
        arn_attribute: "Arn",
        scopes: &[],
        read: &[],
        write: &["lambda:InvokeFunction", "lambda:InvokeAsync"],
    },
    Target {
        resource_type: STATE_MACHINE_TYPE,
        arn_attribute: "Arn",
        scopes: &[],
        read: &[
            "states:DescribeStateMachine",
            "states:DescribeExecution",
            "states:ListExecutions",
        ],
        write: &[
            "states:StartExecution",
            "states:StartSyncExecution",
            "states:StopExecution",
        ],
    },
];

/// The type the grant a connector stands for is, where the source executes as a role of the
/// same document: a customer managed policy attached to that role.
const IAM_MANAGED_POLICY: &str = "AWS::IAM::ManagedPolicy";

/// What the generated policy's logical id is, after the connector's own: "AWS SAM generates …
/// `<connector-logical-id>Policy`".
const POLICY_SUFFIX: &str = "Policy";

/// The IAM policy language version every policy document the transform writes states, which is
/// the one IAM publishes for a document carrying variables.
const POLICY_LANGUAGE_VERSION: &str = "2012-10-17";

/// The `Roles` property a managed policy lists the roles it is attached to under.
const ROLES: &str = "Roles";

/// What the macro adds for ONE connector: the logical id of the grant it stands for and the
/// resource that grant is, or `None` for a connector this module expands nothing for (see the
/// module docs), which is left in the processed template exactly as declared.
pub(super) fn expanded(
    logical_id: &str,
    declaration: &Value,
    resources: &Map<String, Value>,
) -> Option<(String, Value)> {
    let properties = object(declaration.get("Properties"));
    let source = properties.get("Source")?;
    let source_type = endpoint_type(Some(source), resources)?;
    let roles = attached_role(source, resources)?;
    let requested = requested_permissions(properties.get(PERMISSIONS))?;
    let mut statements: Vec<Value> = Vec::new();
    for destination in destinations(properties.get("Destination")) {
        let destination_type = endpoint_type(Some(&destination), resources)?;
        let profile = PROFILES.iter().find(|profile| {
            profile.source == source_type && profile.destination == destination_type
        })?;
        let target = TARGETS
            .iter()
            .find(|target| target.resource_type == destination_type)?;
        let arn = endpoint_arn(&destination, resources, target)?;
        // In the order the profile publishes its permissions, so the same connector always
        // writes the same document whichever order the declaration listed them in.
        for permission in profile
            .permissions
            .iter()
            .filter(|published| requested.iter().any(|asked| asked == *published))
        {
            statements.push(json!({
                "Effect": "Allow",
                "Action": target.actions(permission)?,
                "Resource": granted_over(&arn, target.scopes),
            }));
        }
    }
    if statements.is_empty() {
        return None;
    }
    let mut policy = Map::new();
    policy.insert(
        "PolicyDocument".to_string(),
        json!({
            "Version": POLICY_LANGUAGE_VERSION,
            "Statement": statements,
        }),
    );
    policy.insert(ROLES.to_string(), Value::Array(roles));
    Some((
        format!("{logical_id}{POLICY_SUFFIX}"),
        resource(IAM_MANAGED_POLICY, policy),
    ))
}

/// The `Roles` the generated policy is attached to: the one role the connector's SOURCE
/// executes as, named the way a managed policy names a role — "the name (friendly name, not
/// ARN) of the role to attach the policy to".
///
/// A declaration the macro generates a role FOR is attached through that role's logical id, and
/// one that states a role of its own through the resource or the ARN it states it as: `Ref` on
/// an `AWS::IAM::Role` is that role's name, and the name inside a role ARN is its last segment.
/// `None` for a source whose role is not a role of this document, which is a connector nothing
/// here can attach a policy to.
fn attached_role(source: &Value, resources: &Map<String, Value>) -> Option<Vec<Value>> {
    let id = source.get("Id").and_then(Value::as_str)?;
    let declaration = resources.get(id)?;
    let properties = object(declaration.get("Properties"));
    // A function states its role under `Role`, a state machine the type the macro generates
    // under `Role` and the CloudFormation one under `RoleArn`.
    let stated = properties.get("Role").or_else(|| properties.get("RoleArn"));
    match stated {
        Some(role) => role_name(role).map(|name| vec![name]),
        // "If this property isn't specified, AWS SAM creates an IAM role for you", under the
        // declaration's own logical id followed by `Role` ([`super::execution_role`]).
        None if matches!(resource_type(declaration)?, FUNCTION | STATE_MACHINE) => {
            Some(vec![json!({ REF: format!("{id}Role") })])
        }
        None => None,
    }
}

/// The NAME of the role a declaration states as `role`: the role resource an `Fn::GetAtt` reads
/// the ARN of (whose `Ref` is that role's name), or the last segment of a literal role ARN.
/// `None` for anything else, which names no role this module can attach to.
fn role_name(role: &Value) -> Option<Value> {
    if let Some(reference) = role.get("Fn::GetAtt") {
        let named = match reference {
            Value::Array(parts) => parts.first()?.as_str()?.to_string(),
            Value::String(path) => path.split('.').next()?.to_string(),
            _ => return None,
        };
        return Some(json!({ REF: named }));
    }
    let (_, name) = role.as_str()?.rsplit_once('/')?;
    (!name.is_empty()).then(|| json!(name))
}

/// The ARN of one end: the ARN the reference states for a resource outside the document, and
/// otherwise the ARN attribute of the declaration its `Id` names.
fn endpoint_arn(
    endpoint: &Value,
    resources: &Map<String, Value>,
    target: &Target,
) -> Option<Value> {
    if let Some(arn) = endpoint.get("Arn") {
        return Some(arn.clone());
    }
    let id = endpoint.get("Id").and_then(Value::as_str)?;
    resources.get(id)?;
    Some(json!({ "Fn::GetAtt": [id, target.arn_attribute] }))
}

/// Every ARN one statement is granted over: the resource's own, and one per sub-resource scope
/// the actions reach — the resource's ARN followed by that scope, composed as the template
/// composes a value out of another ([`super::super::template`] resolves it as it deploys) when
/// the ARN is itself computed.
fn granted_over(arn: &Value, scopes: &[&str]) -> Value {
    let mut granted = vec![arn.clone()];
    for scope in scopes {
        granted.push(match arn.as_str() {
            Some(literal) => json!(format!("{literal}{scope}")),
            None => json!({ "Fn::Join": ["", [arn.clone(), *scope]] }),
        });
    }
    Value::Array(granted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document whose connector declares a source and a destination, the permissions it asks
    /// for, and the resources it runs between.
    fn document(source: Value, destination: Value, permissions: Value, resources: Value) -> Value {
        let mut declared = object(Some(&resources));
        declared.insert(
            "Connector".to_string(),
            json!({
                "Type": CONNECTOR,
                "Properties": {
                    "Source": source,
                    "Destination": destination,
                    "Permissions": permissions,
                },
            }),
        );
        Value::Object(declared)
    }

    /// The fault a document's one connector carries.
    fn connector_fault(resources: &Value) -> Option<String> {
        let resources = object(Some(resources));
        fault(resources.get("Connector").unwrap(), &resources)
    }

    /// RULE: a connector is invalid for asking a permission its source/destination profile does
    /// not publish, reported with both resource types and the values that pair does publish.
    ///
    /// The rows differ along what the rule generalises over: WHICH pair the connector runs
    /// between (a pair that reads and writes, and one that only writes), HOW the ends state
    /// their types (a declaration of the document, a declaration the macro expands into another
    /// type, and an ARN stating its own type), and WHAT is asked for (a value no profile
    /// publishes, a value another pair publishes, a list with one supported value and one that
    /// is not, and no permission at all).
    #[test]
    fn a_connector_asking_an_unpublished_permission_is_invalid() {
        // (case, the document's resources, the detail it is invalid for)
        let rows: Vec<(&str, Value, String)> = vec![
            (
                "a value no profile publishes, between declared resources",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Table"}),
                    json!(["Delete"]),
                    json!({
                        "Fn": {"Type": LAMBDA_FUNCTION},
                        "Table": {"Type": DYNAMODB_TABLE},
                    }),
                ),
                unsupported_detail(LAMBDA_FUNCTION, DYNAMODB_TABLE, READ_AND_WRITE),
            ),
            (
                "a value no profile publishes, between declarations the macro expands",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Table"}),
                    json!(["Delete"]),
                    json!({
                        "Fn": {"Type": FUNCTION, "Properties": {}},
                        "Table": {"Type": SIMPLE_TABLE, "Properties": {}},
                    }),
                ),
                unsupported_detail(LAMBDA_FUNCTION, DYNAMODB_TABLE, READ_AND_WRITE),
            ),
            (
                "reading a destination its pair only publishes writing",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Topic"}),
                    json!([READ]),
                    json!({
                        "Fn": {"Type": FUNCTION, "Properties": {}},
                        "Topic": {"Type": SNS_TOPIC},
                    }),
                ),
                unsupported_detail(LAMBDA_FUNCTION, SNS_TOPIC, WRITE_ONLY),
            ),
            (
                "one supported value beside one the pair does not publish",
                document(
                    json!({"Id": "Rule"}),
                    json!({"Id": "Queue"}),
                    json!([WRITE, READ]),
                    json!({
                        "Rule": {"Type": EVENTS_RULE},
                        "Queue": {"Type": SQS_QUEUE},
                    }),
                ),
                unsupported_detail(EVENTS_RULE, SQS_QUEUE, WRITE_ONLY),
            ),
            (
                "no permission at all",
                document(
                    json!({"Id": "Machine"}),
                    json!({"Id": "Bucket"}),
                    json!([]),
                    json!({
                        "Machine": {"Type": STATE_MACHINE, "Properties": {}},
                        "Bucket": {"Type": S3_BUCKET},
                    }),
                ),
                unsupported_detail(STATE_MACHINE_TYPE, S3_BUCKET, READ_AND_WRITE),
            ),
            (
                "a destination outside the document, stating its own type",
                document(
                    json!({"Id": "Fn"}),
                    json!({
                        "Type": DYNAMODB_TABLE,
                        "Arn": "arn:aws:dynamodb:us-east-1:111122223333:table/items",
                        "ResourceId": "items",
                    }),
                    json!(["Delete"]),
                    json!({"Fn": {"Type": LAMBDA_FUNCTION}}),
                ),
                unsupported_detail(LAMBDA_FUNCTION, DYNAMODB_TABLE, READ_AND_WRITE),
            ),
            (
                "the faulty one of two destinations",
                document(
                    json!({"Id": "Fn"}),
                    json!([{"Id": "Table"}, {"Id": "Topic"}]),
                    json!([READ, WRITE]),
                    json!({
                        "Fn": {"Type": LAMBDA_FUNCTION},
                        "Table": {"Type": DYNAMODB_TABLE},
                        "Topic": {"Type": SNS_TOPIC},
                    }),
                ),
                unsupported_detail(LAMBDA_FUNCTION, SNS_TOPIC, WRITE_ONLY),
            ),
        ];

        for (case, resources, expected) in rows {
            let answered = connector_fault(&resources);
            assert_eq!(
                answered.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: a connector whose pair publishes everything it asks for, and one this module
    /// places nothing about, is left to expand as declared.
    ///
    /// The rows differ along what the rule generalises over: a permission set the pair publishes
    /// whole (one value and both of them), a pair no profile here carries, an end naming a
    /// declaration the document does not make, and a `Permissions` the template computes.
    #[test]
    fn a_connector_the_profile_publishes_carries_no_fault() {
        // (case, the document's resources)
        let rows: Vec<(&str, Value)> = vec![
            (
                "both values of a pair that publishes both",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Table"}),
                    json!([READ, WRITE]),
                    json!({
                        "Fn": {"Type": FUNCTION, "Properties": {}},
                        "Table": {"Type": SIMPLE_TABLE, "Properties": {}},
                    }),
                ),
            ),
            (
                "the one value a write-only pair publishes",
                document(
                    json!({"Id": "Rule"}),
                    json!({"Id": "Machine"}),
                    json!([WRITE]),
                    json!({
                        "Rule": {"Type": EVENTS_RULE},
                        "Machine": {"Type": STATE_MACHINE, "Properties": {}},
                    }),
                ),
            ),
            (
                "a pair no profile here publishes",
                document(
                    json!({"Id": "Table"}),
                    json!({"Id": "Bucket"}),
                    json!(["Delete"]),
                    json!({
                        "Table": {"Type": DYNAMODB_TABLE},
                        "Bucket": {"Type": S3_BUCKET},
                    }),
                ),
            ),
            (
                "an end naming a declaration the document does not make",
                document(
                    json!({"Id": "Missing"}),
                    json!({"Id": "Table"}),
                    json!(["Delete"]),
                    json!({"Table": {"Type": DYNAMODB_TABLE}}),
                ),
            ),
            (
                "permissions the template computes",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Table"}),
                    json!({"Ref": "APermissionsParameter"}),
                    json!({
                        "Fn": {"Type": LAMBDA_FUNCTION},
                        "Table": {"Type": DYNAMODB_TABLE},
                    }),
                ),
            ),
        ];

        for (case, resources) in rows {
            let answered = connector_fault(&resources);
            assert_eq!(
                answered, None,
                "case {case:?}: expected no fault, got {answered:?}"
            );
        }
    }

    /// The detail one unsupported pair reads as, which is the wrapper the rows above are
    /// written against.
    fn unsupported_detail(source: &str, destination: &str, published: &[&str]) -> String {
        format!(
            "Unsupported 'Permissions' provided for connector from {source} to {destination}; \
             valid values are: {}.",
            published.join(", ")
        )
    }

    /// Every permission a profile publishes has an access category for its destination type, so
    /// a connector the validator ACCEPTS is one the expansion can write a statement for.
    #[test]
    fn every_published_permission_has_actions_for_its_destination() {
        for profile in PROFILES {
            let target = TARGETS
                .iter()
                .find(|target| target.resource_type == profile.destination);
            for permission in profile.permissions {
                let actions = target.and_then(|target| target.actions(permission));
                assert!(
                    actions.is_some_and(|actions| !actions.is_empty()),
                    "case {:?} -> {:?}: {permission:?} is published with no actions, got \
                     {actions:?}",
                    profile.source,
                    profile.destination
                );
            }
        }
    }

    /// One expansion row: the document, the role the generated policy must be attached to, and
    /// the actions and ARNs of each statement it must carry, in order.
    struct Grant {
        case: &'static str,
        resources: Value,
        role: Value,
        actions: Vec<Value>,
        granted: Vec<Value>,
    }

    impl Grant {
        fn new(
            case: &'static str,
            resources: Value,
            role: Value,
            actions: Vec<Value>,
            granted: Vec<Value>,
        ) -> Self {
            Self {
                case,
                resources,
                role,
                actions,
                granted,
            }
        }
    }

    /// RULE: a connector whose source executes as a role of the document becomes the managed
    /// policy attached to that role, carrying one statement per permission it asks for over the
    /// destination's ARN and the sub-resources those actions reach.
    ///
    /// The rows differ along what the rule generalises over: WHICH permissions the connector
    /// asks for (one, and both), where the role comes from (one the macro generates, and one the
    /// declaration states as a role of the document), and which destination it is granted on (a
    /// table, whose indexes the grant reaches too, and a topic, whose ARN it does not).
    #[test]
    fn a_connector_becomes_the_policy_attached_to_its_sources_role() {
        let rows: Vec<Grant> = vec![
            Grant::new(
                "reading a table through a role the macro generates",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Items"}),
                    json!([READ]),
                    json!({
                        "Fn": {"Type": FUNCTION, "Properties": {}},
                        "Items": {"Type": SIMPLE_TABLE, "Properties": {}},
                    }),
                ),
                json!({"Ref": "FnRole"}),
                vec![json!(table_target().read)],
                vec![json!([
                    {"Fn::GetAtt": ["Items", "Arn"]},
                    {"Fn::Join": ["", [{"Fn::GetAtt": ["Items", "Arn"]}, "/index/*"]]},
                ])],
            ),
            Grant::new(
                "reading and writing one table: one statement per permission",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Items"}),
                    json!([WRITE, READ]),
                    json!({
                        "Fn": {"Type": FUNCTION, "Properties": {}},
                        "Items": {"Type": DYNAMODB_TABLE},
                    }),
                ),
                json!({"Ref": "FnRole"}),
                vec![json!(table_target().read), json!(table_target().write)],
                vec![
                    json!([
                        {"Fn::GetAtt": ["Items", "Arn"]},
                        {"Fn::Join": ["", [{"Fn::GetAtt": ["Items", "Arn"]}, "/index/*"]]},
                    ]),
                    json!([
                        {"Fn::GetAtt": ["Items", "Arn"]},
                        {"Fn::Join": ["", [{"Fn::GetAtt": ["Items", "Arn"]}, "/index/*"]]},
                    ]),
                ],
            ),
            Grant::new(
                "publishing to a topic through a role the document declares",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Topic"}),
                    json!([WRITE]),
                    json!({
                        "Fn": {"Type": LAMBDA_FUNCTION, "Properties": {
                            "Role": {"Fn::GetAtt": ["Executor", "Arn"]},
                        }},
                        "Executor": {"Type": "AWS::IAM::Role", "Properties": {}},
                        "Topic": {"Type": SNS_TOPIC},
                    }),
                ),
                json!({"Ref": "Executor"}),
                vec![json!(["sns:Publish"])],
                vec![json!([{"Fn::GetAtt": ["Topic", "TopicArn"]}])],
            ),
        ];

        for Grant {
            case,
            resources,
            role,
            actions,
            granted,
        } in rows
        {
            let resources = object(Some(&resources));
            let expanded = expanded(
                "Connector",
                resources.get("Connector").expect("the connector declared"),
                &resources,
            );
            let Some((logical_id, policy)) = expanded else {
                panic!("case {case:?}: expected the connector to become a policy, got nothing");
            };
            assert_eq!(
                logical_id, "ConnectorPolicy",
                "case {case:?}: expected the policy to carry the connector's own logical id, \
                 got {logical_id:?}"
            );
            assert_eq!(
                policy.get("Type").and_then(Value::as_str),
                Some(IAM_MANAGED_POLICY),
                "case {case:?}: expected a managed policy, got {policy}"
            );
            assert_eq!(
                policy.pointer("/Properties/Roles"),
                Some(&json!([role])),
                "case {case:?}: expected the policy attached to {role}, got {policy}"
            );
            let statements = policy
                .pointer("/Properties/PolicyDocument/Statement")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                statements.len(),
                actions.len(),
                "case {case:?}: expected {} statements, got {policy}",
                actions.len()
            );
            for (index, statement) in statements.iter().enumerate() {
                assert_eq!(
                    statement.get("Effect").and_then(Value::as_str),
                    Some("Allow"),
                    "case {case:?} statement {index}: expected an allow, got {statement}"
                );
                assert_eq!(
                    statement.get("Action"),
                    Some(&actions[index]),
                    "case {case:?} statement {index}: expected {}, got {statement}",
                    actions[index]
                );
                assert_eq!(
                    statement.get("Resource"),
                    Some(&granted[index]),
                    "case {case:?} statement {index}: expected {}, got {statement}",
                    granted[index]
                );
            }
        }
    }

    /// A connector this module writes no grant for keeps its declaration: a source that executes
    /// as no role of the document, and a pair whose grant is a policy on the destination.
    #[test]
    fn a_connector_with_no_role_to_attach_to_expands_to_nothing() {
        // (case, the document's resources)
        let rows: Vec<(&str, Value)> = vec![
            (
                "a source whose role is no role of this document",
                document(
                    json!({"Id": "Fn"}),
                    json!({"Id": "Items"}),
                    json!([READ]),
                    json!({
                        "Fn": {"Type": FUNCTION, "Properties": {
                            "Role": {"Ref": "ARoleParameter"},
                        }},
                        "Items": {"Type": DYNAMODB_TABLE},
                    }),
                ),
            ),
            (
                "a pair whose grant is a policy on the destination",
                document(
                    json!({"Id": "Topic"}),
                    json!({"Id": "Queue"}),
                    json!([WRITE]),
                    json!({
                        "Topic": {"Type": SNS_TOPIC},
                        "Queue": {"Type": SQS_QUEUE},
                    }),
                ),
            ),
            (
                "a source outside the document",
                document(
                    json!({
                        "Type": LAMBDA_FUNCTION,
                        "Arn": "arn:aws:lambda:us-east-1:111122223333:function:reader",
                    }),
                    json!({"Id": "Items"}),
                    json!([READ]),
                    json!({"Items": {"Type": DYNAMODB_TABLE}}),
                ),
            ),
        ];

        for (case, resources) in rows {
            let resources = object(Some(&resources));
            let answered = expanded(
                "Connector",
                resources.get("Connector").expect("the connector declared"),
                &resources,
            );
            assert!(
                answered.is_none(),
                "case {case:?}: expected no grant to be written, got {answered:?}"
            );
        }
    }

    /// The DynamoDB table target, whose action lists the expansion rows are written against.
    fn table_target() -> &'static Target {
        TARGETS
            .iter()
            .find(|target| target.resource_type == DYNAMODB_TABLE)
            .expect("a table target")
    }
}
