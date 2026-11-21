// SPDX-License-Identifier: CC0-1.0

//! The general-label TAXONOMY this service reports a measured entity through, and where a
//! measurement is turned into the modelled [`Label`]s that report it.
//!
//! `DetectLabels` "detects instances of real-world entities within an image … objects like
//! flower, tree, and table; events like wedding, graduation, and birthday party; and concepts
//! like landscape, evening, and nature", and "for each object, scene, and concept the API
//! returns one or more labels". So every name this service reports in `Labels` is an ENTITY of
//! that taxonomy — a thing the image CARRIES — and a measurement of the frame's own pixels (how
//! bright they are, which colours cover them) is reported where the model puts it, under
//! `DetectLabelsImageProperties`, and never as an entity: a colour is a property of the
//! picture, not something in it.
//!
//! One measured entity is therefore reported as SEVERAL labels. "DetectLabels returns a
//! hierarchical taxonomy of detected labels … The response includes the all ancestors for a
//! label, where every ancestor is a unique label. In the previous example, Car, Vehicle, and
//! Transportation are returned as unique labels in the response" — so the entity is reported
//! together with every entity the taxonomy puts above it, each appearing ONCE, each carrying
//! its own `Parents` (all of ITS ancestors, nearest first) and its own `Categories`.
//! [`reported_labels`] is that expansion and the one place a reported label is built, so no
//! reporting path can leave one of the six members `Label` models out, report one entity
//! twice, or report a name the taxonomy holds no row for.
//!
//! The taxonomy itself is published rather than measured: "Categories - The label categories
//! that the detected label belongs to" and "Aliases - Possible Aliases for the label" come
//! from the category, ancestor and alias lists the Amazon Rekognition Developer Guide
//! publishes for the general-label model, linked from the filters' own documentation ("To see
//! a list of label categories, see …",
//! `smithy_query(query="shape",shape="GeneralLabelsSettings")`). This module holds a row for
//! each entity this service can MEASURE — the category that entity belongs to and the entity
//! it is a part of — and holds no alias list at all, so `Aliases` reports the empty list it
//! has nothing to put in rather than a name nothing looked up.
//!
//! What can be measured is what [`super::face_detection`]'s geometric detector finds, so the
//! rows below are the person-description chain a measured face is reported through. An entity
//! an image carries that no measurement here reads — an apparel item, an animal, a scene — is
//! reported by nothing: recognising it needs a trained classifier over the whole published
//! taxonomy, and no such classifier runs in this process (see [`super::face_detection`] on
//! what a classifier would be needed for), so an image in which nothing was measured is
//! answered with an EMPTY label list rather than with a name nothing measured.

use crate::ops::types::{Instance, Label, LabelCategory, Parent};

/// The category of the labels that describe a person and the parts of one: the row the
/// entities measured by this service's geometric face detector belong to.
pub const PERSON_DESCRIPTION_CATEGORY: &str = "Person Description";

/// The entity a measured face is reported as, and the entities the published taxonomy puts
/// above it: a face is part of a head, which is part of a person.
pub const FACE: &str = "Face";
pub const HEAD: &str = "Head";
pub const PERSON: &str = "Person";

/// One row of the taxonomy: an entity's name, the category the published taxonomy puts it in,
/// and the entity it is a part of — `None` for a root, an entity with no ancestor above it.
#[derive(Debug, Clone, Copy)]
struct Entity {
    name: &'static str,
    category: &'static str,
    parent: Option<&'static str>,
}

/// The rows of the published taxonomy this service holds: one per entity it can measure, and
/// one per ancestor those entities bring with them.
const TAXONOMY: [Entity; 3] = [
    Entity {
        name: FACE,
        category: PERSON_DESCRIPTION_CATEGORY,
        parent: Some(HEAD),
    },
    Entity {
        name: HEAD,
        category: PERSON_DESCRIPTION_CATEGORY,
        parent: Some(PERSON),
    },
    Entity {
        name: PERSON,
        category: PERSON_DESCRIPTION_CATEGORY,
        parent: None,
    },
];

/// One entity measured in a frame: the taxonomy row it was measured as, the confidence
/// measured for it, and the instances of it located in the frame — "bounding boxes are
/// described for all instances of detected common object labels", so an instance is reported
/// for the entity whose extent was measured and for nothing else.
#[derive(Debug, Clone)]
pub struct Measured {
    /// The name of the entity's own taxonomy row.
    pub name: &'static str,
    /// How confident the measurement behind the entity is, on the 0..100 scale a `Percent`
    /// carries.
    pub confidence: f32,
    /// Where each instance of the entity was located, empty where no extent was measured.
    pub instances: Vec<Instance>,
}

/// Every label a set of measured entities is reported as: each entity, followed by each
/// ancestor the taxonomy puts above it, nearest ancestor first, with every name appearing
/// exactly ONCE however many of the measured entities brought it.
///
/// A measured entity keeps the instances located for it; an ancestor carries none, because
/// where the larger entity extends is not what the measurement below it read. Two measurements
/// of one entity report it once, at the strongest confidence measured for it and with every
/// instance located for it — "every ancestor is a unique label", and so is every entity.
///
/// A name no row above holds is reported by nothing: this service reports what it measured and
/// holds a row for, and reports nothing for what it does not.
pub fn reported_labels(measured: Vec<Measured>) -> Vec<Label> {
    let mut reported: Vec<Label> = Vec::new();
    for entity in measured {
        let Some(row) = row_of(entity.name) else {
            ::tracing::error!(
                entity = entity.name,
                "the label taxonomy holds no row for a measured entity"
            );
            continue;
        };
        report(&mut reported, row, entity.confidence, entity.instances);
        for ancestor in ancestors_of(row) {
            report(&mut reported, ancestor, entity.confidence, Vec::new());
        }
    }
    reported
}

/// Report one entity ONCE: a name already reported keeps its place in the answer and takes the
/// strongest confidence measured for it and every instance located for it, rather than being
/// reported a second time.
fn report(reported: &mut Vec<Label>, row: Entity, confidence: f32, instances: Vec<Instance>) {
    if let Some(already) = reported
        .iter_mut()
        .find(|label| label.name.as_deref() == Some(row.name))
    {
        if confidence > already.confidence.unwrap_or_default() {
            already.confidence = Some(confidence);
        }
        already
            .instances
            .get_or_insert_with(Vec::new)
            .extend(instances);
        return;
    }
    reported.push(reported_label(row, confidence, instances));
}

/// One measured entity, as the model shapes a `Label`: its name, the confidence measured for
/// it, the category the taxonomy puts it in, every ancestor above it and an `Instance` per
/// instance of it located in the frame.
///
/// Every list member is PRESENT: `Instances` and `Parents` carry the empty list where nothing
/// was located and where the entity is a root of the taxonomy, and `Aliases` carries the empty
/// list because this emulator holds no published alias list to report from (see this module's
/// docs) — an empty list is what it has to report, and reporting it is what the model asks for.
fn reported_label(row: Entity, confidence: f32, instances: Vec<Instance>) -> Label {
    Label {
        name: Some(row.name.to_string()),
        confidence: Some(confidence),
        instances: Some(instances),
        parents: Some(
            ancestors_of(row)
                .map(|ancestor| Parent {
                    name: Some(ancestor.name.to_string()),
                })
                .collect(),
        ),
        aliases: Some(Vec::new()),
        categories: Some(vec![LabelCategory {
            name: Some(row.category.to_string()),
        }]),
    }
}

/// The row the taxonomy holds for a name, or none where it holds no row for it.
fn row_of(name: &str) -> Option<Entity> {
    TAXONOMY.into_iter().find(|row| row.name == name)
}

/// Every ancestor the taxonomy puts above an entity, nearest first: its parent, its parent's
/// parent and so on up to a root. Bounded by the number of rows, so a row whose parent chain
/// does not reach a root reports each ancestor at most once rather than walking forever.
fn ancestors_of(row: Entity) -> impl Iterator<Item = Entity> {
    let mut next = row.parent;
    (0..TAXONOMY.len()).map_while(move |_| {
        let ancestor = row_of(next?)?;
        next = ancestor.parent;
        Some(ancestor)
    })
}
