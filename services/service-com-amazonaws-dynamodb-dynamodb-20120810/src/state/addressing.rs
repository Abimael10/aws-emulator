// SPDX-License-Identifier: CC0-1.0

//! How a `TableName`, a table ARN and a `ResourceArn` are read and held to their form.
//!
//! One file for the ADDRESSING half of this service's requests: the two spellings a member
//! that names a table accepts ("You can also provide the Amazon Resource Name (ARN) of the
//! table in this parameter" — `CreateTable$TableName`), the form a bare NAME must take, the
//! fields a table ARN must carry and the Region it must address, and the clause each of
//! those refusals states. It is about the IDENTIFIER a request carries rather than about
//! what the store holds, so it lives beside [`super`]; `super` re-exports it, so every
//! caller still names it `crate::state::…`.

use super::*;

/// The requirement phrase for a value that is spelled as an ARN and is not a valid one.
///
/// A member that takes either a name or an ARN (`TableArn`, which the data-plane
/// `TableName` members target) states the ARN FORM rather than the name's pattern when
/// the value it refused was an attempt at an ARN: the pattern a bare table name must
/// satisfy says nothing about which ARN field was wrong.
///
/// The clause states the ARN template AND the form its RESOURCE NAME must take, which
/// is the table name's own ([`MIN_TABLE_NAME_LENGTH`], [`MAX_TABLE_NAME_LENGTH`],
/// [`TABLE_NAME_PATTERN`]) — so the one clause covers every way the resource name can
/// be unacceptable, a resource name spelled as a PATH (`<table>/index/<i>`,
/// `<table>/stream/<label>`) included, and a caller reading it is told that ARN parsing
/// is what refused the value.
pub fn arn_form_requirement() -> String {
    format!(
        "Valid ARN format is \
         'arn:<awsPartition>:<vendor>:<region>:<subscriber>:resourceType/resourceName', where \
         Resource name must have length less than or equal to {MAX_TABLE_NAME_LENGTH}, \
         Resource name must have length greater than or equal to {MIN_TABLE_NAME_LENGTH}, \
         Resource name must satisfy regular expression pattern: {}",
        unanchored(TABLE_NAME_PATTERN)
    )
}

/// The form a table name must take: 3..=255 characters of
/// `[a-zA-Z0-9_.-]` (the `CreateTable$TableName` doc's own length and pattern —
/// the data-plane members target `TableArn`, which widens the modelled bound to
/// 1..=1024 so an ARN fits, while the name inside it keeps these).
pub(super) const MIN_TABLE_NAME_LENGTH: usize = 3;
pub(super) const MAX_TABLE_NAME_LENGTH: usize = 255;
pub(super) const TABLE_NAME_PATTERN: &str = "^[a-zA-Z0-9_.-]+$";

/// How AWS's refusals name the table-name member: the `tableName` path its own
/// request validator reports this constraint against, with the rejected name
/// echoed.
///
/// NOT the wire spelling `src/service.rs` restores for a violation of a bound
/// the MODEL declares, and deliberately so: the public model does not carry this
/// bound at all (the data-plane members target `TableArn`, length 1..=1024), so
/// this is a check the service itself makes, and the recorded AWS refusals for
/// it state the validator's own form — `Value 'ab' at 'tableName' failed to
/// satisfy constraint: Member must have length greater than or equal to 3` for
/// a two-character name on `GetItem`. The same clause is stated for every
/// operation that resolves a table name, and the echoed value is the name the
/// caller sent, never a figure of this emulator's.
pub const TABLE_NAME_MEMBER_PATH: &str = "tableName";

/// The resource type a table's ARN names: `arn:<partition>:dynamodb:<region>:<account>:
/// table/<name>`, the form [`table_arn`] mints and the only one the `TableName` member
/// accepts.
pub(super) const TABLE_RESOURCE_TYPE: &str = "table";

/// The refusal an ARN of this service that names a resource of ANOTHER KIND earns, stated
/// as the SENTENCE naming the ARN: the offending ARN, as the request spelled it.
///
/// The TYPE field of the ARN is what is wrong with the value, and a fact about an ARN's own
/// field is stated over that ARN rather than as a constraint of the member that carried it
/// — exactly as the fact about its REGION field is ([`invalid_arn_region`]). Both are read
/// out of the same ARN by the same resolution, so both are stated in the same frame, and an
/// operation that states one form states the other in it too ([`ArnRefusalForm`]).
pub(super) fn invalid_resource_type(arn: &str) -> RequestError {
    refused(format!("{INVALID_RESOURCE_TYPE_REQUIREMENT} in '{arn}'"))
}

/// The same fact as that member's own CONSTRAINT: the member path, the ARN the request sent
/// and the requirement its resource-type field broke ([`invalid_member`]).
///
/// The echo is held to the longest value the MEMBER itself admits
/// ([`MAX_ECHOED_TABLE_NAME`]), like every other clause about this member.
fn resource_type_outside_this_service(member: &str, arn: &str) -> RequestError {
    let echoed = ::service_common::validation::quoted_echo_up_to(arn, MAX_ECHOED_TABLE_NAME);
    invalid_member(member, Some(&echoed), INVALID_RESOURCE_TYPE_REQUIREMENT)
}

/// The requirement phrase an ARN naming a resource of another kind fails: fixed text,
/// stating the FIELD of the ARN that is wrong rather than any form the value should have
/// taken — the resource type is wrong whatever the rest of the ARN says.
pub(super) const INVALID_RESOURCE_TYPE_REQUIREMENT: &str = "Invalid resource type";

/// The `TableName` member accepts a bare table name or the table's ARN ("You can
/// also provide the Amazon Resource Name (ARN) of the table in this parameter" —
/// the `CreateTable$TableName` doc). Resolve either to the bare name, refusing a
/// name outside the documented 3..=255 `[a-zA-Z0-9_.-]+` form.
///
/// A name outside that form is refused the way DynamoDB's request validator words a
/// constraint violation ([`invalid_member`]), because that is what this check is: the
/// member, the value and the constraint it broke.
pub fn resolve_table_name(table_name: &str) -> Result<String, RequestError> {
    resolve_table_name_of(TABLE_NAME_MEMBER_PATH, table_name)
}

/// The same resolution for a `TableName` carried by ONE ELEMENT of a request, whose
/// refusal names the member by the path that element's member sits at (`member`)
/// rather than by the request-level `tableName`.
///
/// A request-validator clause names the FULL path of the member it refused, so a
/// caller can find which element of a batched or transactional request was rejected:
/// the list the element came in, its position in that list, the action it states and
/// the leaf member ([`TABLE_NAME_MEMBER_PATH`]). The constraint and the echoed value
/// are the same whichever member carried the name, which is why there is one
/// resolution and only the path differs.
pub fn resolve_table_name_of(member: &str, table_name: &str) -> Result<String, RequestError> {
    resolve_table_name_framed(member, table_name, ArnRefusalForm::Sentence)
}

/// That resolution with the FRAME an ARN-field refusal is stated in made explicit, so the
/// operation that resolves the name chooses it ([`ArnRefusalForm`]).
fn resolve_table_name_framed(
    member: &str,
    table_name: &str,
    form: ArnRefusalForm,
) -> Result<String, RequestError> {
    // A value spelled as an ARN is refused in one of THREE ways, and which one depends
    // on what is wrong with it:
    //
    //   * a string that is NOT a well-formed table ARN — a required field missing or
    //     empty, or a resource name outside the form a table name takes — is refused by
    //     the ARN FORM, because that is what is wrong with it: the value was parsed as
    //     an ARN and the ARN is what it failed as;
    //   * an ARN OF THIS SERVICE whose resource type is not a table's names a resource
    //     of another kind, and the refusal says so over the ARN that was sent, in the
    //     frame this operation states an ARN field's refusals in ([`ArnRefusalForm`]);
    //   * any other well-formed ARN that does not name a table of this service is no way
    //     of naming a table, so it is refused as the table NAME it would have to be,
    //     by the pattern a table name must satisfy.
    let malformed_arn = || invalid_member(member, Some(table_name), &arn_form_requirement());
    let not_a_table_arn = || {
        invalid_member(
            member,
            Some(table_name),
            &pattern_requirement(TABLE_NAME_PATTERN),
        )
    };
    let name = match table_name.strip_prefix(ARN_PREFIX) {
        Some(rest) => {
            let fields: Vec<&str> = rest.splitn(5, ':').collect();
            let [partition, service, region, account, resource] = fields.as_slice() else {
                return Err(malformed_arn());
            };
            if partition.is_empty()
                || service.is_empty()
                || region.is_empty()
                || account.is_empty()
                || resource.is_empty()
            {
                return Err(malformed_arn());
            }
            if *service != "dynamodb" {
                return Err(not_a_table_arn());
            }
            // The resource TYPE this ARN names. An ARN of this service that names a
            // resource of another kind (a backup, or a mis-cased `Table`) is refused by
            // THAT, because the type is what is wrong with it.
            let (resource_type, path) = match resource.split_once('/') {
                Some((resource_type, path)) => (resource_type, path),
                None => (*resource, ""),
            };
            if resource_type != TABLE_RESOURCE_TYPE {
                return Err(form.resource_type_refusal(member, table_name));
            }
            // The resource NAME of a table's ARN is the table name, so it is held to
            // the table name's own form — and a value spelled as an ARN that breaks it
            // is refused by the ARN FORM clause, which states that form, over the whole
            // ARN that was sent: what is unacceptable is the ARN's resource name, not
            // the characters of a bare name the caller never sent. A resource name that
            // is a PATH (`<table>/index/<i>`, `<table>/stream/<label>`) addresses
            // something deeper than the table and breaks it on the separator, as does
            // an ARN with no resource name at all.
            if table_name_violation(path).is_some() {
                return Err(malformed_arn());
            }
            path
        }
        None => table_name,
    };
    // A BARE name is refused by the clauses for the bounds it broke, naming the member
    // and echoing the name itself: there is no ARN to blame. EVERY rule the name breaks is
    // stated, each as its own clause of the one envelope — one submitted value can break
    // two of them at once (a name both too short and outside the pattern), and a refusal
    // that stated one would send the caller back for the other.
    match validation_message(table_name_form_clauses(member, name)) {
        Some(refusal) => Err(RequestError::Validation(refusal)),
        None => Ok(name.to_string()),
    }
}

/// Every clause a bare table NAME that breaks its form earns: one per rule it broke, in the
/// order [`table_name_violations`] states them, each naming `member` and echoing the name.
///
/// Public because the refusal the validating WALK raises on such a member is restated
/// through it (`crate::service`): the member admits an ARN as well as a name, so the bound
/// the model declares on it is wider than the form a NAME takes, and the clauses a caller
/// reads are this form's either way.
///
/// The echo is held to the longest value the MEMBER itself admits
/// ([`MAX_ECHOED_TABLE_NAME`]), so a value the member could hold is quoted in full while a
/// value no request could ever be served with cannot size the response by the request.
pub fn table_name_form_clauses(member: &str, name: &str) -> Vec<String> {
    let echoed = ::service_common::validation::quoted_echo_up_to(name, MAX_ECHOED_TABLE_NAME);
    table_name_violations(name)
        .into_iter()
        .map(|requirement| constraint_clause(member, Some(&echoed), &requirement))
        .collect()
}

/// The longest table name one clause quotes: the maximum the member that carries it
/// declares (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#TableArn")` gives
/// `@length` 1..1024, because the member admits a table's ARN as well as its name). A
/// longer value is outside that bound in every request, so no clause has to quote one
/// whole.
pub(super) const MAX_ECHOED_TABLE_NAME: usize = 1024;

/// The form a table NAME must take ([`MIN_TABLE_NAME_LENGTH`]..=[`MAX_TABLE_NAME_LENGTH`]
/// characters of [`TABLE_NAME_PATTERN`]), as the requirement phrase for EVERY rule the
/// value broke, and empty for a name that takes it.
///
/// The one place the three rules are stated, so a name the request sent as itself and a
/// name carried inside an ARN's resource field are held to exactly the same form — only
/// the clause that reports a violation differs ([`arn_form_requirement`] states all
/// three at once, which is how AWS words an ARN-shaped value's refusal).
///
/// The rules are stated in the order a name is read: its LENGTH, then the characters it is
/// spelt with, which is the order the clauses of one refusal follow. A name can break
/// several at once — a value is held to every rule rather than refused by the first — and
/// the EMPTY name breaks the pattern as well as the minimum, because the expression admits
/// one or more characters and so is satisfied by no empty value.
pub(super) fn table_name_violations(name: &str) -> Vec<String> {
    let length = name.chars().count();
    let mut broken = Vec::new();
    if length < MIN_TABLE_NAME_LENGTH {
        broken.push(min_length_requirement(MIN_TABLE_NAME_LENGTH));
    }
    if length > MAX_TABLE_NAME_LENGTH {
        broken.push(max_length_requirement(MAX_TABLE_NAME_LENGTH));
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
    {
        broken.push(pattern_requirement(TABLE_NAME_PATTERN));
    }
    broken
}

/// Whether a value takes the table-name form at all, for the callers that only ask that.
pub(super) fn table_name_violation(name: &str) -> Option<String> {
    table_name_violations(name).into_iter().next()
}

/// The requirement phrase an ARN addressing a REGION other than the one the request arrived
/// at fails: fixed text, stating the FIELD of the ARN that is wrong rather than any form the
/// value should have taken — exactly as the phrase for an ARN naming a resource of another
/// kind does ([`INVALID_RESOURCE_TYPE_REQUIREMENT`]).
pub(super) const INVALID_ARN_REGION_REQUIREMENT: &str = "Invalid AWS region";

/// The clause AWS states for an ARN that addresses a REGION other than the one the
/// request arrived at, as the SENTENCE alone: the offending ARN, as the request spelled it.
///
/// An endpoint serves one region, so an ARN naming another names nothing this endpoint
/// can reach. This form of the refusal names the VALUE rather than the member it came in,
/// because the fact is about the ARN's own region field.
///
/// The same fact is observed to be stated the OTHER way as well — as that member's own
/// constraint clause ([`region_outside_this_endpoint`]) — and which form a refusal takes
/// follows the operation being served, exactly as the form of a clause about a declared
/// bound does (`crate::service`'s "Which form a constraint clause takes"). Nothing in the
/// model distinguishes the two, so the operation that states the clause asks for it.
pub(super) fn invalid_arn_region(table_name: &str) -> RequestError {
    refused(format!(
        "{INVALID_ARN_REGION_REQUIREMENT} in '{table_name}'"
    ))
}

/// The same fact as that member's own CONSTRAINT: the member path, the ARN the request sent
/// and the requirement its region field broke — the form a refusal of this service's
/// clauses about `tableName` normally takes ([`invalid_member`]).
///
/// The echo is held to the longest value the MEMBER itself admits
/// ([`MAX_ECHOED_TABLE_NAME`]), like every other clause about this member.
fn region_outside_this_endpoint(member: &str, table_name: &str) -> RequestError {
    let echoed = ::service_common::validation::quoted_echo_up_to(table_name, MAX_ECHOED_TABLE_NAME);
    invalid_member(member, Some(&echoed), INVALID_ARN_REGION_REQUIREMENT)
}

/// Which FORM an operation states a refusal about a FIELD of the ARN a table-naming member
/// carried in, read at the one place the resolution raises one ([`resolve_region_addressed`],
/// [`resolve_table_name_framed`]) so the forms cannot drift apart.
///
/// The frame belongs to the OPERATION rather than to the field: an operation that states one
/// ARN field's refusal as the sentence states every other field's that way too, so the
/// region and the resource type are reported in one frame per operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArnRefusalForm {
    /// The sentence alone, naming the ARN ([`invalid_arn_region`],
    /// [`invalid_resource_type`]).
    Sentence,
    /// That member's own constraint clause ([`region_outside_this_endpoint`],
    /// [`resource_type_outside_this_service`]).
    MemberClause,
}

impl ArnRefusalForm {
    /// The REGION refusal this form states for `table_name`, reported under `member`.
    fn region_refusal(self, member: &str, table_name: &str) -> RequestError {
        match self {
            Self::Sentence => invalid_arn_region(table_name),
            Self::MemberClause => region_outside_this_endpoint(member, table_name),
        }
    }

    /// The RESOURCE-TYPE refusal this form states for `table_name`, reported under `member`.
    fn resource_type_refusal(self, member: &str, table_name: &str) -> RequestError {
        match self {
            Self::Sentence => invalid_resource_type(table_name),
            Self::MemberClause => resource_type_outside_this_service(member, table_name),
        }
    }
}

/// A `TableName` resolved against the REGION the request's endpoint names: the same form
/// check [`resolve_table_name`] makes, plus the rule that an ARN spelling of the name
/// must address this endpoint's own region.
///
/// "You can also provide the Amazon Resource Name (ARN) of the table in this parameter"
/// (the `TableName` docs), and an ARN carries a region — while a DynamoDB endpoint
/// serves exactly one, which is the scope a table name is unique in. So an ARN naming
/// another region addresses a table this endpoint has no way of reaching, and the
/// request is refused rather than served from the same-named table of the region it
/// reached. A BARE name carries no region and is the endpoint's own, so it is unaffected.
///
/// The one statement of that rule; the public entry points below differ only in the member
/// the refusal is reported under and the FORM it is stated in.
fn resolve_region_addressed(
    member: &str,
    table_name: &str,
    region: &str,
    form: ArnRefusalForm,
) -> Result<String, RequestError> {
    let name = resolve_table_name_framed(member, table_name, form)?;
    if table_arn_region(table_name).is_some_and(|addressed| addressed != region) {
        return Err(form.region_refusal(member, table_name));
    }
    Ok(name)
}

/// That resolution for the request-level `TableName`, stating an ARN-FIELD refusal as the
/// sentence alone — the form every operation but the ones below is observed to state.
pub fn resolve_table_name_in(table_name: &str, region: &str) -> Result<String, RequestError> {
    resolve_table_name_in_member(TABLE_NAME_MEMBER_PATH, table_name, region)
}

/// The same resolution whose ARN-FIELD refusals are stated as the table-name member's own
/// CONSTRAINT CLAUSE rather than as the sentence alone — the form the single-item READ is
/// observed to state them in, over the `tableName` path with the ARN echoed.
///
/// Called by the operations observed to state that form; every other operation keeps the
/// field sentence. Which form an operation states is recorded at its one call site rather
/// than in a list here, because the operation is what makes the choice — and it is the same
/// form for every field of the ARN ([`ArnRefusalForm`]).
pub fn resolve_table_name_in_clause(table_name: &str, region: &str) -> Result<String, RequestError> {
    resolve_region_addressed(
        TABLE_NAME_MEMBER_PATH,
        table_name,
        region,
        ArnRefusalForm::MemberClause,
    )
}

/// The same resolution for a `TableName` carried by ONE ELEMENT of a request, naming
/// the member by its full path ([`resolve_table_name_of`]).
pub fn resolve_table_name_in_member(
    member: &str,
    table_name: &str,
    region: &str,
) -> Result<String, RequestError> {
    resolve_region_addressed(member, table_name, region, ArnRefusalForm::Sentence)
}

/// The REGION field of a value spelled as an ARN, or `None` when the value is not
/// spelled as one — a bare table name addresses no region of its own.
pub(super) fn table_arn_region(table_name: &str) -> Option<&str> {
    table_name
        .strip_prefix(ARN_PREFIX)?
        .split(':')
        .nth(2)
        .filter(|region| !region.is_empty())
}
