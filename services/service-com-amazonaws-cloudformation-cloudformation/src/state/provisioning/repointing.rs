// SPDX-License-Identifier: CC0-1.0

//! Re-pointing a deployment's stand-ins at the identifiers the owning services MINTED.
//!
//! A declaration is resolved before any of its resources exist, so a reference to a resource
//! whose identity its owning service assigns resolves to the STAND-IN the naming rules
//! computed ([`crate::state::template`]). As [`super::reconcile`] walks the deployment, each
//! create answers the identifier that service really assigned, and this module is what brings
//! the rest of the deployment into step with it: the properties of a resource not created yet
//! ([`repoint_references`]) and that resource's own identity where its name is composed out of
//! such an identifier ([`repoint_identity`]).
//!
//! It lives in its own file beside the walk that drives it so neither grows past the length a
//! change to this repository may touch; [`Minted`] is the pair the walk collects, [`pairs_for`]
//! decides WHICH of those pairs one declaration may be re-pointed with, and [`retokenised`] is
//! the one place a stand-in may be matched inside a longer value.

use super::*;

/// How deep [`repoint_references`] walks a property document.
///
/// The document's nesting is the REQUESTER's, and a recursive walk bounded by its input
/// exhausts the native stack — which aborts the process rather than failing one stack. Past
/// this depth the values are left exactly as they are: a reference to another resource is a
/// property of it, never a hundred levels inside one.
pub(super) const MAX_REPOINTED_NESTING: usize = 64;

/// The minted pairs that may re-point the declaration of `logical_id`.
///
/// The rule: a value another declaration STATES is that declaration's own, so adopting an
/// identifier an owning service minted may only change the values a REFERENCE to that
/// resource put there. Which pairs those are follows from what the stand-in IS:
///
/// * a stand-in CloudFormation itself COMPUTED ([`Minted::composed`]) carries a digest drawn
///   from the stack id and the logical id, so no declaration states one by accident: such a
///   pair applies wherever its value turns up, which is how a name composed out of another
///   resource's identifier is brought into step with it;
/// * a stand-in that is the NAME the declaration asked for is a value ANY declaration may
///   state — the stack name a bucket is named after, which the user pool of the same stack was
///   also created under — so it applies only in the declarations that REFERENCE that resource.
///   `dependencies` is what records that: a `Ref`, an `Fn::GetAtt` or an `Fn::Sub` variable
///   naming a resource is exactly what the relation is built from
///   ([`crate::state::template::direct_dependencies`]), so a resource absent from it reads
///   nothing of the minted resource and every value of its own stands as written.
///
/// Without the second rule such a bucket is created under the identifier Amazon Cognito
/// minted for the pool, and the policy of that bucket is written for a bucket that does not
/// exist.
pub(super) fn pairs_for(
    logical_id: &str,
    dependencies: &Dependencies,
    minted: &[Minted],
) -> Vec<Minted> {
    if minted.is_empty() {
        return Vec::new();
    }
    let referenced = dependencies.get(logical_id);
    minted
        .iter()
        .filter(|minted| {
            minted.composed
                || referenced.is_some_and(|referenced| referenced.contains(&minted.of))
        })
        .cloned()
        .collect()
}

/// Re-point every reference inside one property document from the stand-in identifier a
/// resource was given before its create to the identifier its owning service MINTED.
///
/// `minted` is the set this document may be re-pointed with ([`pairs_for`]), not every pair
/// the walk has collected: a declaration that reads nothing of a resource states its own
/// values, whatever another resource's stand-in happens to be.
///
/// The rule: a reference to a resource resolves to the identifier that resource really has,
/// and for a type whose identity its owning service assigns
/// ([`crate::state::template::identity_is_service_assigned`]) that identifier only exists once
/// the create has answered. A declaration is resolved before any of them have, so a property
/// naming such a resource (`ResolverRuleId: !Ref Rule`) holds the stand-in the naming rules
/// computed; `minted` pairs each stand-in with what its service answered, and a property
/// whose value IS one of those stand-ins becomes the minted identifier.
///
/// The same holds for an ATTRIBUTE the owning service reports differently from the computed
/// one (`KmsMasterKeyId: !GetAtt [Key, Arn]`, where the ARN carries the key id KMS minted):
/// [`super::reconcile`] pairs those too, so a property reaches its own service as the value the
/// resource it names really publishes.
///
/// Matched WHOLE value — or, where the stand-in is an identifier CloudFormation computed
/// rather than a name the declaration asked for ([`Minted::composed`]), as one whole TOKEN
/// of a longer value ([`retokenised`]), which is how a template composes an identifier into
/// one of its own strings (`integrations/<IntegrationId>`, or the ARN an `Fn::Sub` builds
/// around a `Ref`). Never as any shorter run of characters: a stand-in is often the NAME the
/// declaration asked for, so a longer value that merely contains one (an association named
/// `<rule name>-assoc`) is a value of its own and is left exactly as written.
///
/// Called by [`super::reconcile`] for the identifiers minted in the walk it is driving. A
/// resource is never re-pointed with its OWN pair — it cannot reference itself, and for a type
/// whose stand-in is the name it declares that pair would rewrite that very property. The
/// identifiers a stack ALREADY has are not substituted at all:
/// [`crate::state::StoredStack::deploy_template`] resolves the declarations against them
/// instead.
pub(super) fn repoint_references(properties: &mut Value, minted: &[Minted]) {
    repoint(properties, minted, 0);
}

/// Re-point the IDENTITY of the resource a deployment is about to create: the physical id the
/// naming rules computed for it, and the attributes they read off that id.
///
/// The rule: a resource is created in its owning service under the name its declaration
/// resolves to once the resources it depends on carry the identifiers THEIR services minted.
/// A declaration that composes its name out of such an identifier
/// (`LogGroupName: !Join ['', ['/aws/<service>/<kind>/', !GetAtt Api.ApiId]]`) was resolved
/// before any of them had answered, so the name — and the id the naming rules took from it,
/// and the ARN they built on it — carry the stand-in. [`repoint_references`] has just brought
/// the declaration itself into step with what those services really assigned; this brings the
/// identity along, so the create addresses the resource under the name the declaration now
/// states, `Ref` resolves to that name, and a reader that builds the same name from the same
/// dependency reaches the resource rather than a name nothing holds.
///
/// A name no minted identifier is part of is left exactly as it is, which is every resource
/// whose declaration names it outright.
pub(super) fn repoint_identity(resource: &mut StoredResource, minted: &[Minted]) {
    if minted.is_empty() {
        return;
    }
    for value in resource.attributes.values_mut() {
        if let Some(repointed) = repointed_text(value, minted) {
            *value = repointed;
        }
    }
    if let Some(repointed) = repointed_text(&resource.physical_resource_id, minted) {
        // Through the one step that takes an identifier over, so the `Id`/`<Type>Id`
        // attributes and the id a `Ref` resolves to cannot drift apart.
        crate::state::template::adopt_identity(resource, repointed);
    }
}

/// One TEXT value re-pointed the way a property document's values are
/// ([`repoint_references`]), or `None` for a value no pair changes.
pub(super) fn repointed_text(value: &str, minted: &[Minted]) -> Option<String> {
    let mut carried = Value::String(value.to_string());
    repoint(&mut carried, minted, 0);
    match carried {
        Value::String(repointed) if repointed != value => Some(repointed),
        _ => None,
    }
}

/// Whether a resource's own declaration STATES `value` — as a property's value, or as one
/// inside a list or an object the properties carry.
///
/// What it decides is whether a value the owning service reported differently from the
/// computed one may be matched inside a longer value ([`Minted::composed`]): a value the
/// template itself wrote is a value of its own wherever it turns up, while one CloudFormation
/// derived is in another declaration's longer string only because a reference to this resource
/// put it there.
///
/// Walked to the same depth a re-pointing walks ([`MAX_REPOINTED_NESTING`]), for the same
/// reason: the nesting is the requester's.
pub(super) fn states_value(properties: &Value, value: &str, depth: usize) -> bool {
    if depth >= MAX_REPOINTED_NESTING {
        return false;
    }
    match properties {
        Value::Array(items) => items
            .iter()
            .any(|item| states_value(item, value, depth + 1)),
        Value::Object(members) => members
            .values()
            .any(|member| states_value(member, value, depth + 1)),
        // A scalar is read as the one text an API member takes it as, so a number a template
        // wrote as a number is the value a service reports as text.
        scalar => crate::state::scalar_text(scalar).is_some_and(|text| text == value),
    }
}

/// One identifier an owning service MINTED in a deployment walk, against the stand-in the
/// naming rules had computed for the same resource ([`repoint_references`]).
#[derive(Debug, Clone)]
pub(super) struct Minted {
    /// The LOGICAL ID of the resource whose identifier this is, which is what says whose
    /// declarations may be re-pointed with it ([`pairs_for`]).
    pub(super) of: String,
    /// What a reference to the resource was resolved to before its create.
    pub(super) stand_in: String,
    /// What that reference has to carry now.
    pub(super) assigned: String,
    /// Whether the stand-in may also be matched INSIDE a longer value, as one whole token
    /// of it ([`retokenised`]).
    ///
    /// True only where the stand-in is an identifier CloudFormation COMPUTED rather than
    /// the name the declaration asked for: such a value is not one a template composed out
    /// of its own strings, so a longer value carrying it carries it because a reference to
    /// this resource put it there. That is how an API Gateway v2 route names the
    /// integration it is wired to — `Target: !Join ['/', ['integrations', !Ref
    /// Integration]]`, which is the `integrations/<IntegrationId>` form that service
    /// accepts — and how a template builds the ARN of a resource it declares, with the
    /// reference inside a longer string (`!Sub 'arn:aws:execute-api:${AWS::Region}:\
    /// ${AWS::AccountId}:${Api}/*/*'`). Without this, such a route would be created
    /// pointing at an integration id nothing minted, and such a grant made for an api id
    /// nothing minted.
    ///
    /// The same question for an ATTRIBUTE's pair is whether the resource's own declaration
    /// STATES the computed value ([`states_value`]): one it does not state is a value
    /// CloudFormation derived, so another declaration carries it inside a longer value only
    /// because an `Fn::GetAtt` of this resource put it there — the api id a log-group name is
    /// built around.
    pub(super) composed: bool,
}

/// [`repoint_references`] over one value, `depth` levels into the document.
pub(super) fn repoint(properties: &mut Value, minted: &[Minted], depth: usize) {
    if minted.is_empty() || depth >= MAX_REPOINTED_NESTING {
        return;
    }
    match properties {
        Value::String(text) => {
            if let Some(minted) = minted
                .iter()
                .find(|minted| minted.stand_in.as_str() == text.as_str())
            {
                *text = minted.assigned.clone();
                return;
            }
            for minted in minted.iter().filter(|minted| minted.composed) {
                if let Some(repointed) = retokenised(text, &minted.stand_in, &minted.assigned) {
                    *text = repointed;
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                repoint(item, minted, depth + 1);
            }
        }
        Value::Object(members) => {
            for value in members.values_mut() {
                repoint(value, minted, depth + 1);
            }
        }
        _ => {}
    }
}

/// Whether `character` can be part of one IDENTIFIER: an ASCII letter or digit, a hyphen or
/// an underscore, which is what the identifiers AWS services assign are made of.
///
/// It is the boundary [`retokenised`] matches a stand-in at: a stand-in whose neighbour in a
/// longer value is one of these is part of a longer token of that value, so it is not a
/// reference to the resource and is left exactly as written.
pub(super) fn is_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

/// `text` with every WHOLE-TOKEN occurrence of `stand_in` replaced by `assigned`, or `None`
/// for a text that carries none.
///
/// This is how a reference COMPOSED INTO a longer string reaches the identifier its resource
/// really has: a template builds the ARN of a resource it declares out of its own text and
/// that reference (`!Sub 'arn:aws:execute-api:${AWS::Region}:${AWS::AccountId}:${Api}/*/*'`),
/// so the identifier sits inside a longer value delimited by whatever the template wrote
/// around it — a colon, a slash, a quote, a space — rather than being the whole value or one
/// whole slash-separated segment of it.
///
/// A token is bounded by any character an identifier cannot carry
/// ([`is_identifier_character`]), so a stand-in that merely begins or ends a longer
/// identifier is left alone. Only a stand-in CloudFormation itself computed is matched this
/// way ([`Minted::composed`]): a value a declaration asked for is a value of its own
/// wherever it appears.
pub(super) fn retokenised(text: &str, stand_in: &str, assigned: &str) -> Option<String> {
    if stand_in.is_empty() {
        return None;
    }
    let mut repointed = String::new();
    let mut cursor = 0;
    let mut replaced = false;
    while let Some(offset) = text[cursor..].find(stand_in) {
        let at = cursor + offset;
        let end = at + stand_in.len();
        let bounded = text[..at]
            .chars()
            .next_back()
            .is_none_or(|character| !is_identifier_character(character))
            && text[end..]
                .chars()
                .next()
                .is_none_or(|character| !is_identifier_character(character));
        repointed.push_str(&text[cursor..at]);
        if bounded {
            repointed.push_str(assigned);
            replaced = true;
        } else {
            repointed.push_str(stand_in);
        }
        cursor = end;
    }
    if !replaced {
        return None;
    }
    repointed.push_str(&text[cursor..]);
    Some(repointed)
}
