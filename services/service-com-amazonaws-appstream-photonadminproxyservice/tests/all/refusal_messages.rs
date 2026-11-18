// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MESSAGE every AppStream control-plane refusal carries.
//!
//! Each case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is asserted is
//! the body a client actually parses.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * a refusal for a resource the account does not hold carries AWS's sentence for that
//!   error class — the resource kind, the name the REQUEST asked for and the account
//!   that was searched — whichever operation reports it;
//! * a refusal for a name already taken carries AWS's duplicate-name sentence, with the
//!   same two variable parts;
//! * a refusal because a fleet/stack association still exists states what blocks the
//!   delete and how to clear it, and names NONE of the resources on the other side of
//!   the association, however many there are;
//! * a refusal of an `UpdateStack` that both sets a stack attribute's configuration and
//!   names that attribute in `AttributesToDelete` states AWS's sentence for that
//!   attribute, which names the configuration in prose;
//! * a clause refusing a LIST member for its modelled `@length` QUOTES the list the
//!   request carried — `[]` for an empty one, and one printed element per element
//!   otherwise — rather than a count of its elements.

use serde_json::{json, Value};

use crate::stack_fleet_lifecycle::{error_code, Emulator};

/// The account every refusal names: the single account this emulator presents, so the
/// sentence reports the account that was searched rather than a value from one run.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The resources these tests leave stored, so a refusal row can make exactly one side of
/// a two-sided request the missing one.
const STORED_FLEET: &str = "stored-fleet";
const STORED_STACK: &str = "stored-stack";

/// AWS's `ResourceNotFoundException` sentence for a named resource the account does not
/// hold.
///
/// Shared with the other suites in this target (`tests/all/stack_registry.rs`), so every
/// suite states the sentence once.
pub(crate) fn not_found_sentence(kind: &str, name: &str) -> String {
    format!("The {kind} {name} for account {ACCOUNT} does not exist.")
}

/// AWS's `ResourceAlreadyExistsException` sentence for a name already taken. The two
/// spaces after the account id are AWS's own.
///
/// Shared with the other suites in this target (`tests/all/stack_theme.rs`), so every
/// suite states the sentence once.
pub(crate) fn already_exists_sentence(kind: &str, name: &str) -> String {
    format!(
        "The {kind} {name} for account {ACCOUNT}  already exists. \
         Use a unique name within an AWS region in your AWS account."
    )
}

/// The `Message` of a failure body (the modelled member is `Message`; the synthetic
/// channel spells it `message`).
///
/// Shared with the other suites in this target (`tests/all/user_pool.rs`), so every
/// message assertion reads the body the same way.
pub(crate) fn error_message(payload: &Value) -> String {
    payload
        .get("Message")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Send a request that must be refused with `status` and `code`, and return its message.
pub(crate) async fn refusal(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    body: Value,
    status: u16,
    code: &str,
) -> String {
    let (actual_status, payload) = emulator.call(operation, body).await;
    assert_eq!(
        actual_status, status,
        "case {case:?}: {operation} must answer HTTP {status}, got {actual_status} {payload}"
    );
    assert_eq!(
        error_code(&payload),
        code,
        "case {case:?}: {operation} must answer {code}, got {payload}"
    );
    error_message(&payload)
}

/// Names spanning the class: the rows differ along length, character classes and case.
/// The last is the longest name a create admits — the `Name` member's pattern is
/// `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`
/// (`smithy_query(query="operation", operation="CreateStack")`), so 101 characters.
fn names() -> Vec<String> {
    vec![
        "absent".to_string(),
        "Mixed.Case_name-9".to_string(),
        "0123456789".to_string(),
        format!("z{}", "9".repeat(100)),
    ]
}

/// One not-found row: the operation that reports it, the resource kind its refusal
/// names, and how the request asks for `name`.
struct NotFoundRow {
    operation: &'static str,
    kind: &'static str,
    request: fn(&str) -> Value,
}

/// Every implemented operation that refuses a name the account does not hold, for both
/// resource kinds: a delete, a reader, and both sides of the two-sided association
/// operations.
fn not_found_rows() -> Vec<NotFoundRow> {
    vec![
        NotFoundRow {
            operation: "DeleteFleet",
            kind: "fleet",
            request: |name| json!({ "Name": name }),
        },
        NotFoundRow {
            operation: "DescribeFleets",
            kind: "fleet",
            request: |name| json!({ "Names": [name] }),
        },
        NotFoundRow {
            operation: "AssociateFleet",
            kind: "fleet",
            request: |name| json!({ "FleetName": name, "StackName": STORED_STACK }),
        },
        NotFoundRow {
            operation: "DisassociateFleet",
            kind: "fleet",
            request: |name| json!({ "FleetName": name, "StackName": STORED_STACK }),
        },
        NotFoundRow {
            operation: "DeleteStack",
            kind: "stack",
            request: |name| json!({ "Name": name }),
        },
        NotFoundRow {
            operation: "DescribeStacks",
            kind: "stack",
            request: |name| json!({ "Names": [name] }),
        },
        NotFoundRow {
            operation: "UpdateStack",
            kind: "stack",
            request: |name| json!({ "Name": name, "Description": "an update nobody applies" }),
        },
        NotFoundRow {
            operation: "AssociateFleet",
            kind: "stack",
            request: |name| json!({ "FleetName": STORED_FLEET, "StackName": name }),
        },
        NotFoundRow {
            operation: "DisassociateFleet",
            kind: "stack",
            request: |name| json!({ "FleetName": STORED_FLEET, "StackName": name }),
        },
    ]
}

/// Store one fleet and one stack, so a two-sided request can have exactly one missing
/// side.
async fn emulator_with_stored_pair() -> Emulator {
    let emulator = Emulator::new();
    emulator
        .ok("setup", "CreateStack", json!({ "Name": STORED_STACK }))
        .await;
    emulator
        .ok(
            "setup",
            "CreateFleet",
            json!({ "Name": STORED_FLEET, "InstanceType": "stream.standard.small" }),
        )
        .await;
    emulator
}

#[tokio::test]
async fn a_missing_resource_is_refused_with_its_kind_name_and_account() {
    let emulator = emulator_with_stored_pair().await;

    for row in not_found_rows() {
        for name in names() {
            let case = format!("{} {} {name:?}", row.operation, row.kind);
            let message = refusal(
                &emulator,
                &case,
                row.operation,
                (row.request)(&name),
                400,
                "ResourceNotFoundException",
            )
            .await;

            let expected = not_found_sentence(row.kind, &name);
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_name_already_taken_is_refused_with_awss_duplicate_name_sentence() {
    // Rows: the two resources a name can collide on, each created with the members that
    // create requires.
    struct CreateRow {
        operation: &'static str,
        kind: &'static str,
        request: fn(&str) -> Value,
    }

    let creates = vec![
        CreateRow {
            operation: "CreateStack",
            kind: "stack",
            request: |name| json!({ "Name": name }),
        },
        CreateRow {
            operation: "CreateFleet",
            kind: "fleet",
            request: |name| json!({ "Name": name, "InstanceType": "stream.standard.small" }),
        },
    ];

    for row in creates {
        for name in names() {
            let emulator = Emulator::new();
            let case = format!("{} {name:?}", row.operation);
            emulator
                .ok(&case, row.operation, (row.request)(&name))
                .await;

            let message = refusal(
                &emulator,
                &case,
                row.operation,
                (row.request)(&name),
                400,
                "ResourceAlreadyExistsException",
            )
            .await;

            let expected = already_exists_sentence(row.kind, &name);
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn deleting_an_associated_fleet_is_refused_without_naming_the_stacks() {
    // Rows: how many stacks the fleet is associated with. The sentence is the same for
    // one as for many, and names none of them.
    for stack_count in 1..=3_usize {
        for fleet_name in names() {
            let case = format!("{fleet_name:?} associated with {stack_count} stack(s)");
            let emulator = Emulator::new();
            emulator
                .ok(
                    &case,
                    "CreateFleet",
                    json!({ "Name": fleet_name, "InstanceType": "stream.standard.small" }),
                )
                .await;

            let stacks: Vec<String> = (0..stack_count)
                .map(|index| format!("partner-stack-{index}"))
                .collect();
            for stack in &stacks {
                emulator
                    .ok(&case, "CreateStack", json!({ "Name": stack }))
                    .await;
                emulator
                    .ok(
                        &case,
                        "AssociateFleet",
                        json!({ "FleetName": fleet_name, "StackName": stack }),
                    )
                    .await;
            }

            let message = refusal(
                &emulator,
                &case,
                "DeleteFleet",
                json!({ "Name": fleet_name }),
                400,
                "ResourceInUseException",
            )
            .await;

            let expected = format!(
                "The fleet {fleet_name} is associated with a stack. To delete the fleet, \
                 you must dissociate it from the stack."
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
            for stack in &stacks {
                assert!(
                    !message.contains(stack.as_str()),
                    "case {case:?}: the refusal must not name the associated stack \
                     {stack:?}, got {message:?}"
                );
            }

            // Clearing every edge makes the fleet deletable, so the refusal reported a
            // live association rather than a permanent state.
            for stack in &stacks {
                emulator
                    .ok(
                        &case,
                        "DisassociateFleet",
                        json!({ "FleetName": fleet_name, "StackName": stack }),
                    )
                    .await;
            }
            emulator
                .ok(&case, "DeleteFleet", json!({ "Name": fleet_name }))
                .await;
        }
    }
}

#[tokio::test]
async fn deleting_a_stack_with_fleets_is_refused_without_naming_the_fleets() {
    // Rows: how many fleets the stack has associated. The sentence is the same for one
    // as for many, and names none of them.
    for fleet_count in 1..=3_usize {
        for stack_name in names() {
            let case = format!("{stack_name:?} with {fleet_count} associated fleet(s)");
            let emulator = Emulator::new();
            emulator
                .ok(&case, "CreateStack", json!({ "Name": stack_name }))
                .await;

            let fleets: Vec<String> = (0..fleet_count)
                .map(|index| format!("partner-fleet-{index}"))
                .collect();
            for fleet in &fleets {
                emulator
                    .ok(
                        &case,
                        "CreateFleet",
                        json!({ "Name": fleet, "InstanceType": "stream.standard.small" }),
                    )
                    .await;
                emulator
                    .ok(
                        &case,
                        "AssociateFleet",
                        json!({ "FleetName": fleet, "StackName": stack_name }),
                    )
                    .await;
            }

            let message = refusal(
                &emulator,
                &case,
                "DeleteStack",
                json!({ "Name": stack_name }),
                400,
                "ResourceInUseException",
            )
            .await;

            let expected = format!(
                "Cannot delete stack {stack_name}. This stack has fleets associated with \
                 it. Only stacks with no fleets associated with it can be deleted."
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
            for fleet in &fleets {
                assert!(
                    !message.contains(fleet.as_str()),
                    "case {case:?}: the refusal must not name the associated fleet \
                     {fleet:?}, got {message:?}"
                );
            }

            for fleet in &fleets {
                emulator
                    .ok(
                        &case,
                        "DisassociateFleet",
                        json!({ "FleetName": fleet, "StackName": stack_name }),
                    )
                    .await;
            }
            emulator
                .ok(&case, "DeleteStack", json!({ "Name": stack_name }))
                .await;
        }
    }
}

/// One `UpdateStack` that asks for two outcomes for one attribute: the attribute it
/// deletes, the member that sets the same configuration, the value that member carries,
/// and the configuration AWS's sentence names in prose.
struct SetAndDeleted {
    attribute: &'static str,
    member: &'static str,
    value: fn() -> Value,
    prose: &'static str,
}

/// Attributes spanning the class: the one whose configuration AWS names other than after
/// the attribute (the interface VPC endpoints a stack is reached through), a scalar, a
/// list of structures, a list of strings, a structure, and one of the per-connector-type
/// attributes — so the rows differ along the kind of configuration the attribute clears
/// and along how many words name it.
fn set_and_deleted_rows() -> Vec<SetAndDeleted> {
    vec![
        SetAndDeleted {
            attribute: "ACCESS_ENDPOINTS",
            member: "AccessEndpoints",
            value: || json!([{ "EndpointType": "STREAMING", "VpceId": "vpce-0a1b2c3d4e5f60718" }]),
            prose: "vpc endpoints",
        },
        SetAndDeleted {
            attribute: "REDIRECT_URL",
            member: "RedirectURL",
            value: || json!("https://example.invalid/after"),
            prose: "redirect url",
        },
        SetAndDeleted {
            attribute: "USER_SETTINGS",
            member: "UserSettings",
            value: || json!([{ "Action": "FILE_DOWNLOAD", "Permission": "DISABLED" }]),
            prose: "user settings",
        },
        SetAndDeleted {
            attribute: "EMBED_HOST_DOMAINS",
            member: "EmbedHostDomains",
            value: || json!(["embed.example.invalid"]),
            prose: "embed host domains",
        },
        SetAndDeleted {
            attribute: "STREAMING_EXPERIENCE_SETTINGS",
            member: "StreamingExperienceSettings",
            value: || json!({ "PreferredProtocol": "UDP" }),
            prose: "streaming experience settings",
        },
        SetAndDeleted {
            attribute: "STORAGE_CONNECTOR_HOMEFOLDERS",
            member: "StorageConnectors",
            value: || json!([{ "ConnectorType": "HOMEFOLDERS" }]),
            prose: "storage connector homefolders",
        },
    ]
}

#[tokio::test]
async fn setting_and_deleting_one_stack_attribute_states_aws_sentence_for_it() {
    let emulator = Emulator::new();
    let stack = "conflicting";
    let created = emulator
        .ok("setup", "CreateStack", json!({ "Name": stack }))
        .await;

    for row in set_and_deleted_rows() {
        let case = format!("{} set and {} deleted at once", row.member, row.attribute);
        let mut request = json!({ "Name": stack, "AttributesToDelete": [row.attribute] });
        request[row.member] = (row.value)();
        let message = refusal(
            &emulator,
            &case,
            "UpdateStack",
            request,
            400,
            "InvalidParameterCombinationException",
        )
        .await;

        // The apostrophe is the right single quotation mark AWS's own sentence carries.
        let expected = format!("You can’t update and delete {} at the same time.", row.prose);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        // A refused request changes nothing, so the stack still reports what it was
        // created with.
        let described = emulator
            .ok(&case, "DescribeStacks", json!({ "Names": [stack] }))
            .await;
        assert_eq!(
            described["Stacks"][0], created["Stack"],
            "case {case:?}: the refused update must leave the stack alone, got {described}"
        );
    }

    // The control: the delete on its own is accepted, so the refusal was about the
    // combination and not about the attribute.
    emulator
        .ok(
            "the delete on its own",
            "UpdateStack",
            json!({ "Name": stack, "AttributesToDelete": ["ACCESS_ENDPOINTS"] }),
        )
        .await;
}

/// One `AccessEndpoints` entry; the model admits at most four of them
/// (`smithy_query(query="shape", shape="AccessEndpointList")`).
fn access_endpoint(index: usize) -> Value {
    json!({ "EndpointType": "STREAMING", "VpceId": format!("vpce-0a1b2c3d4e5f6071{index}") })
}

/// `message` with each printed element of the list it quotes replaced by `<entry>`, after
/// checking that each really is a printed element of `shape`: the class the validation
/// layer prints, then the identity of the object it held that element in.
///
/// The identity is a handle on an object of ONE run — AWS's own is the JVM's — so it is
/// masked rather than asserted on; the per-element rendering and the element COUNT are what
/// the clause states.
fn with_entries_masked(case: &str, message: &str, shape: &str) -> String {
    let (head, rest) = message
        .split_once("Value '[")
        .unwrap_or_else(|| panic!("case {case:?}: the clause must quote a list, got {message:?}"));
    let (value, tail) = rest
        .split_once("]' at ")
        .unwrap_or_else(|| panic!("case {case:?}: the clause must quote a list, got {message:?}"));
    let class = format!("com.amazonaws.photon.{shape}@");
    let printed: Vec<&str> = if value.is_empty() {
        Vec::new()
    } else {
        value.split(", ").collect()
    };
    let masked: Vec<&str> = printed
        .iter()
        .map(|entry| {
            let identity = entry.strip_prefix(class.as_str()).unwrap_or_else(|| {
                panic!("case {case:?}: expected a printed {shape}, got {entry:?}")
            });
            assert!(
                !identity.is_empty() && identity.chars().all(|digit| digit.is_ascii_hexdigit()),
                "case {case:?}: expected an object identity, got {entry:?}"
            );
            "<entry>"
        })
        .collect();
    format!("{head}Value '[{}]' at {tail}", masked.join(", "))
}

/// One list a write must refuse for its modelled `@length`: the write path it arrives on,
/// the member it arrives under, the path a clause names that member at, the shape its
/// elements are, the elements themselves and the requirement the list broke.
struct OverLength {
    case: &'static str,
    operation: &'static str,
    member: &'static str,
    path: &'static str,
    shape: &'static str,
    entries: Vec<Value>,
    requirement: &'static str,
}

/// Lists spanning the class: the bound that was broken (the maximum of four endpoints, the
/// minimum of one user setting), how many elements the request carried past it, and the
/// write path the list arrived on.
fn over_length_rows() -> Vec<OverLength> {
    let over_the_maximum = "Member must have length less than or equal to 4";
    vec![
        OverLength {
            case: "a create one entry over the endpoint maximum",
            operation: "CreateStack",
            member: "AccessEndpoints",
            path: "accessEndpoints",
            shape: "AccessEndpoint",
            entries: (0..5).map(access_endpoint).collect(),
            requirement: over_the_maximum,
        },
        OverLength {
            case: "a create far over the endpoint maximum",
            operation: "CreateStack",
            member: "AccessEndpoints",
            path: "accessEndpoints",
            shape: "AccessEndpoint",
            entries: (0..7).map(access_endpoint).collect(),
            requirement: over_the_maximum,
        },
        OverLength {
            case: "an update one entry over the endpoint maximum",
            operation: "UpdateStack",
            member: "AccessEndpoints",
            path: "accessEndpoints",
            shape: "AccessEndpoint",
            entries: (0..5).map(access_endpoint).collect(),
            requirement: over_the_maximum,
        },
        OverLength {
            case: "a create whose user settings carry no entry",
            operation: "CreateStack",
            member: "UserSettings",
            path: "userSettings",
            shape: "UserSetting",
            entries: Vec::new(),
            requirement: "Member must have length greater than or equal to 1",
        },
    ]
}

#[tokio::test]
async fn a_list_outside_its_modelled_length_is_refused_quoting_the_list() {
    let emulator = Emulator::new();
    emulator
        .ok("setup", "CreateStack", json!({ "Name": "stored" }))
        .await;

    for row in over_length_rows() {
        let count = row.entries.len();
        let mut request = json!({ "Name": "stored" });
        request[row.member] = Value::Array(row.entries);
        let message = refusal(
            &emulator,
            row.case,
            row.operation,
            request,
            400,
            "ValidationException",
        )
        .await;

        let masked = with_entries_masked(row.case, &message, row.shape);
        let quoted = vec!["<entry>"; count].join(", ");
        let expected = format!(
            "1 validation error detected: Value '[{quoted}]' at '{}' failed to satisfy \
             constraint: {}",
            row.path, row.requirement
        );
        assert_eq!(
            masked, expected,
            "case {:?}: expected message {expected:?}, got {masked:?} (raw {message:?})",
            row.case
        );
    }

    // The controls: a list INSIDE its bounds is accepted, so the clauses above reported
    // the bound rather than the member. One endpoint, because the model declares a single
    // `AccessEndpointType` and a list may name each type once
    // (`src/state/access_endpoints.rs`), and one user setting, which is the minimum the
    // empty row broke.
    emulator
        .ok(
            "a create inside both bounds",
            "CreateStack",
            json!({
                "Name": "inside-the-bounds",
                "AccessEndpoints": [access_endpoint(0)],
                "UserSettings": [{ "Action": "FILE_DOWNLOAD", "Permission": "DISABLED" }],
            }),
        )
        .await;
}

/// Every `StackAttribute` value a refusal of this service ADVERTISES, in the order it lists
/// them: the order the validation layer in front of this service walks its own table of the
/// values, which is neither the model's declaration order nor sorted.
///
/// Three of them — `AGENT_ACCESS_CONFIG_COMPUTER_USE_INTERFACE_ID`, `WEB_AUTHN_REDIRECTION`
/// and `AGENT_ACCESS_CONFIG_KMS_KEY_ARN` — are attributes the service has that the shipped
/// model does not declare (`smithy_query(query="shape", shape="StackAttribute")` lists the
/// other fifteen). Advertising them is all this states: which values the member ADMITS is
/// the enum the member targets, so a value the shipped model does not declare is still
/// refused.
const STACK_ATTRIBUTE_SET: &str = "AGENT_ACCESS_CONFIG_COMPUTER_USE_INTERFACE_ID, THEME_NAME, \
     AGENT_ACCESS_CONFIG, ACCESS_ENDPOINTS, CONTENT_REDIRECTION, WEB_AUTHN_REDIRECTION, \
     STORAGE_CONNECTORS, REDIRECT_URL, STORAGE_CONNECTOR_AUTHENTICATION, \
     STORAGE_CONNECTOR_HOMEFOLDERS, AGENT_ACCESS_CONFIG_KMS_KEY_ARN, FEEDBACK_URL, \
     USER_SETTINGS, STORAGE_CONNECTOR_GOOGLE_DRIVE, STREAMING_EXPERIENCE_SETTINGS, \
     STORAGE_CONNECTOR_ONE_DRIVE, IAM_ROLE_ARN, EMBED_HOST_DOMAINS";

/// `EmbedHostDomain`'s expression, as the model writes it
/// (`smithy_query(query="shape", shape="EmbedHostDomain")`).
const DOMAIN_PATTERN: &str =
    r"^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9][a-z0-9-]{0,61}[a-z0-9]$";

/// `TagKey`'s expression, as the model writes it
/// (`smithy_query(query="shape", shape="TagKey")`).
const TAG_KEY_PATTERN: &str = r"^(^(?!aws:).[\p{L}\p{Z}\p{N}_.:/=+\-@]*)$";

/// A resource ARN of this emulator's account, well inside the `Arn` member's own expression,
/// so a refusal of a `TagKeys` row reports that list and nothing else.
fn resource_arn() -> String {
    format!("arn:aws:appstream:us-east-1:{ACCOUNT}:stack/stored")
}

/// The requirement a clause states at a collection member for a constraint its ELEMENTS
/// broke: the element shape's whole declared set, nested.
fn element_set(requirements: &[String]) -> String {
    format!("Member must satisfy constraint: [{}]", requirements.join(", "))
}

/// The nested set a clause about one of a list's enum elements states.
fn enum_element_set() -> String {
    element_set(&[format!(
        "Member must satisfy enum value set: [{STACK_ATTRIBUTE_SET}]"
    )])
}

/// The nested set a clause about one of `EmbedHostDomains`' elements states: the element
/// shape's `@length` bounds and its expression, whichever of them the value broke.
fn domain_element_set() -> String {
    element_set(&[
        "Member must have length less than or equal to 128".to_string(),
        "Member must have length greater than or equal to 0".to_string(),
        format!("Member must satisfy regular expression pattern: {DOMAIN_PATTERN}"),
    ])
}

/// The nested set a clause about one of `TagKeys`' elements states.
fn tag_key_element_set() -> String {
    element_set(&[
        "Member must have length less than or equal to 128".to_string(),
        "Member must have length greater than or equal to 1".to_string(),
        format!("Member must satisfy regular expression pattern: {TAG_KEY_PATTERN}"),
    ])
}

/// One request a list ELEMENT's constraint refuses: the operation it arrives on, the request
/// itself, the member path the clause blames, the list as the clause quotes it, and the
/// nested requirement the clause states.
struct ElementRefusal {
    case: &'static str,
    operation: &'static str,
    request: Value,
    path: &'static str,
    quoted: String,
    element_set: String,
}

/// Requests spanning the class: which CONSTRAINT the element broke (an enumerated value set,
/// an expression, a length maximum), which MEMBER and which operation carried the list, WHICH
/// element of the list broke it (the only one, a later one, several at once) and how many
/// elements the list carried beside it.
fn element_refusals() -> Vec<ElementRefusal> {
    let long_domain = format!("{}.example", "d".repeat(128));
    vec![
        ElementRefusal {
            case: "the only attribute is not a stack attribute",
            operation: "UpdateStack",
            request: json!({ "Name": "stored", "AttributesToDelete": ["access_endpoints"] }),
            path: "attributesToDelete",
            quoted: "[access_endpoints]".to_string(),
            element_set: enum_element_set(),
        },
        ElementRefusal {
            case: "the second of two attributes is not a stack attribute",
            operation: "UpdateStack",
            request: json!({
                "Name": "stored",
                "AttributesToDelete": ["ACCESS_ENDPOINTS", "redirect_url"],
            }),
            path: "attributesToDelete",
            quoted: "[ACCESS_ENDPOINTS, redirect_url]".to_string(),
            element_set: enum_element_set(),
        },
        ElementRefusal {
            case: "two attributes are not stack attributes",
            operation: "UpdateStack",
            request: json!({
                "Name": "stored",
                "AttributesToDelete": ["user_settings", "THEME_NAME", "feedback_url"],
            }),
            path: "attributesToDelete",
            quoted: "[user_settings, THEME_NAME, feedback_url]".to_string(),
            element_set: enum_element_set(),
        },
        ElementRefusal {
            case: "a create whose embed host domain is not a domain",
            operation: "CreateStack",
            request: json!({ "Name": "embedding", "EmbedHostDomains": ["test,com"] }),
            path: "embedHostDomains",
            quoted: "[test,com]".to_string(),
            element_set: domain_element_set(),
        },
        ElementRefusal {
            case: "an update whose second embed host domain is not a domain",
            operation: "UpdateStack",
            request: json!({
                "Name": "stored",
                "EmbedHostDomains": ["embed.example", ".com"],
            }),
            path: "embedHostDomains",
            quoted: "[embed.example, .com]".to_string(),
            element_set: domain_element_set(),
        },
        ElementRefusal {
            case: "an embed host domain past the element length maximum",
            operation: "UpdateStack",
            request: json!({ "Name": "stored", "EmbedHostDomains": [long_domain.clone()] }),
            path: "embedHostDomains",
            quoted: format!("[{long_domain}]"),
            element_set: domain_element_set(),
        },
        ElementRefusal {
            case: "an untag whose tag key is longer than the element maximum",
            operation: "UntagResource",
            request: json!({
                "ResourceArn": resource_arn(),
                "TagKeys": ["k".repeat(129)],
            }),
            path: "tagKeys",
            quoted: format!("[{}]", "k".repeat(129)),
            element_set: tag_key_element_set(),
        },
        ElementRefusal {
            case: "an untag whose second tag key is empty",
            operation: "UntagResource",
            request: json!({ "ResourceArn": resource_arn(), "TagKeys": ["Environment", ""] }),
            path: "tagKeys",
            quoted: "[Environment, ]".to_string(),
            element_set: tag_key_element_set(),
        },
    ]
}

#[tokio::test]
async fn a_constraint_a_list_element_breaks_is_refused_at_the_list() {
    let emulator = Emulator::new();
    emulator
        .ok("setup", "CreateStack", json!({ "Name": "stored" }))
        .await;

    for row in element_refusals() {
        let message = refusal(
            &emulator,
            row.case,
            row.operation,
            row.request,
            400,
            "ValidationException",
        )
        .await;

        // ONE validation error, whatever the number of offending elements: the clause names
        // the member, the list it held and the whole set each element has to satisfy.
        let expected = format!(
            "1 validation error detected: Value '{}' at '{}' failed to satisfy constraint: {}",
            row.quoted, row.path, row.element_set
        );
        assert_eq!(
            message, expected,
            "case {:?}: expected message {expected:?}, got {message:?}",
            row.case
        );
    }

    // The controls: the same members inside the element constraints are accepted, so the
    // clauses above reported the element shape rather than the member — and the stack keeps
    // the attributes a refused update named.
    emulator
        .ok(
            "every attribute the model declares",
            "UpdateStack",
            json!({
                "Name": "stored",
                "AttributesToDelete": ["ACCESS_ENDPOINTS", "REDIRECT_URL", "THEME_NAME"],
            }),
        )
        .await;
    emulator
        .ok(
            "a domain inside the element expression",
            "UpdateStack",
            json!({ "Name": "stored", "EmbedHostDomains": ["embed.example.com"] }),
        )
        .await;
}

