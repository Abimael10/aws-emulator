// SPDX-License-Identifier: CC0-1.0

//! How this service REFUSES a create whose name is already taken, and what the refusal
//! reports besides its sentence.
//!
//! AWS IoT has no single already-exists sentence, exactly as it has no single not-found
//! one ([`super::not_found`]): each resource FAMILY states the collision in its own fixed
//! text, and the only variable part of it is the identifier the REQUEST carried — never a
//! value taken from one call.
//!
//! * The NAMED-RESOURCE families whose create is refused "by name" — a policy, a role
//!   alias — word it `<Family> cannot be created - name already exists (name=<name>)`
//!   ([`name_already_exists`]).
//! * The JOBS and COMMANDS planes word it `<Family> <id> already exists.`, with a
//!   terminating period ([`plane_already_exists`]).
//! * The THING family names the dimension that conflicts and the account the collision is
//!   scoped to: `Thing <name> already exists in account with different attributes`.
//! * The THING GROUP and THING TYPE families name theirs as `properties`, scoped to the
//!   `current account`, with the family noun in TWO capitalised words.
//! * The TOPIC RULE family single-quotes the name and scopes the collision to the
//!   account: `Topic Rule '<name>' already exists for account`.
//! * The CERTIFICATE PROVIDER family BRACKETS the name and scopes the collision to the
//!   account: `Certificate provider [<name>] already exists for this account`
//!   ([`existing_certificate_provider_message`]).
//! * The SOFTWARE PACKAGE family does not report an existing resource at all: it reports
//!   a duplicate REQUEST, as a conditional-check failure
//!   ([`existing_package_message`]).
//! * The AUTHORIZER family's sentence lives beside that family's other rules
//!   ([`super::existing_authorizer_message`]).
//!
//! Which MEMBERS the 409 body carries is the family's too, and is decided by the handler:
//! the families whose refusal reports the record that already exists — a policy, a role
//! alias, an authorizer — fill `resourceId`/`resourceArn`, while the registry, jobs,
//! commands, packages and rules families send the message alone. Both members are
//! optional on `ResourceAlreadyExistsException` and `resourceId` is on
//! `ConflictException` (`smithy_query(query="shape",shape="ResourceAlreadyExistsException")`),
//! so a body that fills one AWS leaves absent is a shape a strict client does not expect.
//!
//! Every operation that creates a resource of a family refuses a taken name with that
//! family's sentence, so two creates of one family cannot word one collision two ways —
//! which is why these sentences live here rather than in each handler.

/// The sentence a family that refuses a create BY NAME states a taken name with: the
/// family noun, the fixed clause, and the name the request asked for in `name=<...>`.
///
/// The noun is the family's own spelling of itself (`Policy`, `RoleAlias`), and nothing
/// of the stored record is part of the sentence.
fn name_already_exists(family: &str, name: &str) -> String {
    format!("{family} cannot be created - name already exists (name={name})")
}

/// The sentence the JOBS and COMMANDS planes state a taken id with: the family noun, the
/// id the request carried, and the fixed clause ` already exists.` — terminated by a
/// period, unlike the registry families' sentences.
fn plane_already_exists(family: &str, id: &str) -> String {
    format!("{family} {id} already exists.")
}

/// How this service refuses a `CreatePolicy` under a name the account already holds a
/// policy under.
pub fn existing_policy_message(policy_name: &str) -> String {
    name_already_exists("Policy", policy_name)
}

/// How this service refuses a `CreateRoleAlias` under a name the account already holds an
/// alias under: the policy family's form, with this family's own noun.
pub fn existing_role_alias_message(role_alias: &str) -> String {
    name_already_exists("RoleAlias", role_alias)
}

/// How this service refuses a `CreateJob` under an id a job already holds — the id a job
/// "must be unique for your account".
pub fn existing_job_message(job_id: &str) -> String {
    plane_already_exists("Job", job_id)
}

/// How this service refuses a `CreateCommand` under an id a command already holds: the
/// jobs plane's clause, which the commands plane shares.
pub fn existing_command_message(command_id: &str) -> String {
    plane_already_exists("Command", command_id)
}

/// How this service refuses a `CreateThing` whose name is registered already with
/// DIFFERENT attributes — the dimension a thing's create conflicts on, since a repeat
/// create of the same attributes succeeds.
///
/// The sentence names that dimension and the ACCOUNT the collision is scoped to, so the
/// caller is told the name is taken account-wide rather than in one region.
pub fn existing_thing_message(thing_name: &str) -> String {
    format!("Thing {thing_name} already exists in account with different attributes")
}

/// How this service refuses a `CreateThingGroup` (or a `CreateDynamicThingGroup`) whose
/// name a group with different PROPERTIES holds.
///
/// This family's noun is TWO capitalised words and its account clause is the `current`
/// account's — a wording of its own, not the thing family's.
pub fn existing_thing_group_message(thing_group_name: &str) -> String {
    format!(
        "Thing Group {thing_group_name} already exists in current account with different \
         properties"
    )
}

/// How this service refuses a `CreateThingType` whose name a type with different
/// PROPERTIES holds: the thing-group family's sentence, with the thing-type noun.
pub fn existing_thing_type_message(thing_type_name: &str) -> String {
    format!(
        "Thing Type {thing_type_name} already exists in current account with different \
         properties"
    )
}

/// How this service refuses a `CreateTopicRule` under a rule name the account already
/// holds: the name SINGLE-QUOTED, and the collision scoped to the account.
pub fn existing_topic_rule_message(rule_name: &str) -> String {
    format!("Topic Rule '{rule_name}' already exists for account")
}

/// How this service refuses a `CreateCertificateProvider` under a name the account already
/// holds a certificate provider under.
///
/// This family BRACKETS the name the request carried — the way every certificate-provider
/// refusal states a name it reports back
/// ([`super::missing_certificate_provider_message`]) — and scopes the collision to the
/// account, since a provider is an account-wide record rather than a regional one.
pub fn existing_certificate_provider_message(certificate_provider_name: &str) -> String {
    format!("Certificate provider [{certificate_provider_name}] already exists for this account")
}

/// How this service refuses a `CreatePackage` under a name the catalog already holds.
///
/// This family reports a duplicate REQUEST rather than an existing resource — the create
/// carries an `@idempotencyToken`, so a second create under a taken name is read as a
/// repeat of the request that took it, and the refusal is phrased as the conditional
/// check that failed. A duplicate VERSION of a package is stated in the same form, with
/// the version appended ([`super::duplicate_package_version_message`]).
pub fn existing_package_message(package_name: &str) -> String {
    format!("Conditional check failure: Received duplicate request for package {package_name}")
}
