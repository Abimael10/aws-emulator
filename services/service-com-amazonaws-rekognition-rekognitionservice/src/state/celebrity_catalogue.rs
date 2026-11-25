// SPDX-License-Identifier: CC0-1.0

//! The CELEBRITIES this service holds, and the identification `RecognizeCelebrities`
//! answers from them.
//!
//! `RecognizeCelebrities` "lists the recognized celebrities in the CelebrityFaces array and
//! any unrecognized faces in the UnrecognizedFaces array", so which side of that partition a
//! measured face falls on is an IDENTITY: AWS answers it against its own catalogue of
//! celebrities, and the Id it reports for a recognized face is what the caller "use[s] as a
//! unique identifier for the celebrity … to identify the celebrity in a call to the
//! GetCelebrityInfo operation" (the operation's own documentation). An emulator that holds no
//! catalogue can report no celebrity and hand out no such Id, so this module is the catalogue:
//! a SEEDED set of celebrities this service holds, each with the identity the API reports for
//! one — an `Id` (a `RekognitionUniqueId`, `pattern ^[0-9A-Za-z]*$`), a `Name`, its `Urls` and
//! its `KnownGender` — beside the appearance of that entry's own REFERENCE FACE.
//!
//! The identity is the catalogue's, declared here: these are this emulator's own people, not
//! AWS's, and nothing in this process infers who a photographed face belongs to. What IS
//! measured is the MATCH. A reference face is carried in exactly the form the detector
//! measures a submitted face in ([`FaceAppearance`], the normalised grid of a face's own
//! light and dark), so the one comparison this process can make about two faces — the
//! template correlation [`FaceAppearance::similarity_to`] computes, the same measurement
//! `CompareFaces` reports a `Similarity` from — is what resolves a submitted face against the
//! catalogue:
//!
//! * a measured face is the entry whose reference face it RESEMBLES MOST of those the answer
//!   has not already named, and the resemblance measured for it is the `MatchConfidence`
//!   reported with it — "the confidence, in percentage, that Amazon Rekognition has that the
//!   recognized face is the celebrity" (`smithy_query(query="shape",shape="Celebrity")`), so a
//!   caller reads off the answer how near the match it was given was;
//! * one entry is one PERSON, so an entry is matched at most once per answer and the face
//!   that resembles it most takes it: the same person is not reported twice in one image.
//!   Two faces of one image that resemble one entry alike are therefore reported as two
//!   different people rather than one of them being dropped from the answer, because the
//!   partition reports every face it measured and the catalogue is what names them;
//! * a face that resembles NO entry of the catalogue AT ALL ([`MINIMUM_RESEMBLANCE`]) — one
//!   whose light and dark rises and falls with no reference face's, which includes a flat
//!   patch that resembles nothing, itself included — is not identified, and neither is a face
//!   the catalogue has run out of people for, since one entry is one person. Either way it is
//!   reported on the unrecognized side of the partition, which is where the operation itself
//!   puts a face it does not recognise.
//!
//! The catalogue is READ, never written: it is seeded state, the same for every caller, so two
//! identical calls answer identically and a recognition persists nothing. Its entries are what
//! `GetCelebrityInfo` resolves an `Id` through, which is what closes the documented
//! `RecognizeCelebrities` → `Celebrity.Id` → `GetCelebrityInfo` chain.

use super::face_detection::{FaceAppearance, APPEARANCE_GRID};
use crate::ops::types::{Celebrity, ComparedFace, KnownGender};

/// How many cells one reference face carries: the same grid a submitted face's appearance is
/// measured on, so the two correlate cell for cell.
const REFERENCE_CELLS: usize = APPEARANCE_GRID * APPEARANCE_GRID;

/// The resemblance a measured face must EXCEED to BE a catalogue entry's celebrity, on the
/// 0..100 scale [`FaceAppearance::similarity_to`] measures a resemblance on.
///
/// It is ZERO, exclusive: what makes a face one entry of the catalogue rather than another is
/// being NEAREST to that entry, and what makes it a celebrity at all is resembling that entry
/// at all — its own light and dark rising and falling WITH that reference face's, which is a
/// positive correlation. Nothing above that is a floor this emulator can set honestly: a
/// PHOTOGRAPHED face is measured on whatever its own box holds — hair, shadow, a collar, the
/// ground behind the head — so its grid correlates with any reference face at a level a floor
/// set above chance excludes, and a catalogue behind such a floor can name no face in a
/// photograph at all, which is reported as an image of nobody recognisable rather than as the
/// recognition it is. The strength of each match is reported instead, as the
/// `MatchConfidence` the entry is carried with, where a caller can see it; a face that
/// correlates with no entry positively, a flat patch included, is still named by none of them.
pub const MINIMUM_RESEMBLANCE: f32 = 0.0;

/// The `KnownGenderType` values a catalogue entry's `KnownGender` may carry: "the known
/// gender identity can be Male, Female, Nonbinary, or Unlisted"
/// (`smithy_query(query="shape",shape="KnownGenderType")`).
const KNOWN_GENDER_MALE: &str = "Male";
const KNOWN_GENDER_FEMALE: &str = "Female";
const KNOWN_GENDER_NONBINARY: &str = "Nonbinary";
const KNOWN_GENDER_UNLISTED: &str = "Unlisted";

/// Where one reference face's features sit inside its own box, and the tones that face
/// carries: how a catalogue entry ships the portrait its reference appearance is taken from,
/// since the portrait's pixels themselves are not shipped.
///
/// The rows and columns are cells of the [`APPEARANCE_GRID`]-square grid over the face's box,
/// and the tones are on the 0..255 scale of a decoded grey pixel. Only the RELATIVE tones
/// matter: a reference face is centred on its own mean and scaled to unit length exactly as a
/// measured face is, so these describe a portrait's layout and contrast and not its exposure.
#[derive(Debug, Clone, Copy)]
struct ReferencePortrait {
    /// The first and the last column of the grid the head itself spans; the columns outside
    /// it are the ground behind it.
    head: (usize, usize),
    /// How many rows at the top of the box are the hair above the forehead — none for a face
    /// whose box starts below its hairline.
    hair_rows: usize,
    /// The row the eyes sit on, and the two columns they occupy.
    eyes: (usize, usize, usize),
    /// The row the mouth sits on, and the first and last column it spans.
    mouth: (usize, usize, usize),
    /// The tone of the face's own skin.
    skin: f32,
    /// The tone of the dark features set into that skin — the eyes and the mouth.
    feature: f32,
    /// The tone of the hair above the forehead.
    hair: f32,
    /// The tone of the ground behind the head.
    ground: f32,
}

impl ReferencePortrait {
    /// This portrait as the grid of cell tones a face's appearance is measured on.
    fn cells(&self) -> Vec<f32> {
        let mut cells = Vec::with_capacity(REFERENCE_CELLS);
        for row in 0..APPEARANCE_GRID {
            for column in 0..APPEARANCE_GRID {
                cells.push(self.tone(row, column));
            }
        }
        cells
    }

    /// The tone of one cell of this portrait.
    fn tone(&self, row: usize, column: usize) -> f32 {
        let (first_column, last_column) = self.head;
        if column < first_column || column > last_column {
            return self.ground;
        }
        if row < self.hair_rows {
            return self.hair;
        }
        let (eye_row, left_eye, right_eye) = self.eyes;
        if row == eye_row && (column == left_eye || column == right_eye) {
            return self.feature;
        }
        let (mouth_row, first_mouth, last_mouth) = self.mouth;
        if row == mouth_row && column >= first_mouth && column <= last_mouth {
            return self.feature;
        }
        self.skin
    }
}

/// One celebrity as this service ships it: the identity the API reports for that person, and
/// the portrait its reference face is measured off.
struct SeededCelebrity {
    id: &'static str,
    name: &'static str,
    urls: &'static [&'static str],
    known_gender: &'static str,
    portrait: ReferencePortrait,
}

/// The celebrities this service holds.
///
/// They are this emulator's own — invented people with identifiers of its own minting, since
/// no catalogue of the world's celebrities is shipped with it — and each `Id` is eight
/// characters of the `^[0-9A-Za-z]*$` an `Id` is constrained to, distinct across the table so
/// one `Id` names one entry. The genders span the four `KnownGenderType` values, and each
/// entry carries one URL "pointing to additional information about the celebrity" — a page
/// for that entry under the host reserved for documentation use (RFC 2606), since these
/// people are the emulator's own and no live knowledge base holds one: a caller cannot
/// mistake the link for a real reference, and nothing in this process fetches it.
///
/// Their REFERENCE FACES span what a photographed face varies in at this grid's resolution —
/// where the eyes and the mouth sit inside the box, how much of the box is hair, how wide the
/// head is in frame, and whether the face is light against a dark ground or dark against a
/// light one — so that two different submitted faces resolve to two different entries rather
/// than to one that stands in for every face.
const SEEDED: [SeededCelebrity; 12] = [
    SeededCelebrity {
        id: "Kp7Vn2Qd",
        name: "Adaeze Okonkwo",
        urls: &["www.example.com/celebrity/Kp7Vn2Qd"],
        known_gender: KNOWN_GENDER_FEMALE,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 0,
            eyes: (3, 2, 5),
            mouth: (6, 3, 4),
            skin: 196.0,
            feature: 54.0,
            hair: 48.0,
            ground: 92.0,
        },
    },
    SeededCelebrity {
        id: "Rm4Xb9Tz",
        name: "Tobias Hjelm",
        urls: &["www.example.com/celebrity/Rm4Xb9Tz"],
        known_gender: KNOWN_GENDER_MALE,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 1,
            eyes: (3, 2, 5),
            mouth: (6, 2, 5),
            skin: 172.0,
            feature: 40.0,
            hair: 36.0,
            ground: 210.0,
        },
    },
    SeededCelebrity {
        id: "Wd6Lq1Hs",
        name: "Marisol Vega",
        urls: &["www.example.com/celebrity/Wd6Lq1Hs"],
        known_gender: KNOWN_GENDER_FEMALE,
        portrait: ReferencePortrait {
            head: (1, 6),
            hair_rows: 1,
            eyes: (4, 2, 5),
            mouth: (6, 3, 4),
            skin: 148.0,
            feature: 32.0,
            hair: 60.0,
            ground: 74.0,
        },
    },
    SeededCelebrity {
        id: "Jt8Cp3Yr",
        name: "Nikolai Brandt",
        urls: &["www.example.com/celebrity/Jt8Cp3Yr"],
        known_gender: KNOWN_GENDER_MALE,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 2,
            eyes: (4, 1, 6),
            mouth: (6, 3, 4),
            skin: 204.0,
            feature: 62.0,
            hair: 44.0,
            ground: 188.0,
        },
    },
    SeededCelebrity {
        id: "Zb2Nf7Gk",
        name: "Imani Reyes",
        urls: &["www.example.com/celebrity/Zb2Nf7Gk"],
        known_gender: KNOWN_GENDER_NONBINARY,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 0,
            eyes: (2, 2, 5),
            mouth: (5, 3, 4),
            skin: 120.0,
            feature: 28.0,
            hair: 30.0,
            ground: 196.0,
        },
    },
    SeededCelebrity {
        id: "Hv5Qs8Md",
        name: "Beatrix Laurent",
        urls: &["www.example.com/celebrity/Hv5Qs8Md"],
        known_gender: KNOWN_GENDER_FEMALE,
        portrait: ReferencePortrait {
            head: (1, 6),
            hair_rows: 0,
            eyes: (3, 2, 5),
            mouth: (5, 2, 5),
            skin: 182.0,
            feature: 46.0,
            hair: 52.0,
            ground: 150.0,
        },
    },
    SeededCelebrity {
        id: "Px9Tk4Wb",
        name: "Soren Adeyemi",
        urls: &["www.example.com/celebrity/Px9Tk4Wb"],
        known_gender: KNOWN_GENDER_MALE,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 1,
            eyes: (4, 2, 6),
            mouth: (6, 2, 4),
            skin: 96.0,
            feature: 22.0,
            hair: 26.0,
            ground: 64.0,
        },
    },
    SeededCelebrity {
        id: "Cq3Rd6Nl",
        name: "Yuki Takahara",
        urls: &["www.example.com/celebrity/Cq3Rd6Nl"],
        known_gender: KNOWN_GENDER_UNLISTED,
        portrait: ReferencePortrait {
            head: (2, 5),
            hair_rows: 1,
            eyes: (3, 3, 4),
            mouth: (6, 3, 4),
            skin: 210.0,
            feature: 70.0,
            hair: 58.0,
            ground: 118.0,
        },
    },
    SeededCelebrity {
        id: "Fs7Wm2Bv",
        name: "Elodie Marchand",
        urls: &["www.example.com/celebrity/Fs7Wm2Bv"],
        known_gender: KNOWN_GENDER_FEMALE,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 2,
            eyes: (3, 1, 6),
            mouth: (6, 2, 5),
            skin: 164.0,
            feature: 36.0,
            hair: 88.0,
            ground: 56.0,
        },
    },
    SeededCelebrity {
        id: "Gn1Hb8Xq",
        name: "Dmitri Novak",
        urls: &["www.example.com/celebrity/Gn1Hb8Xq"],
        known_gender: KNOWN_GENDER_MALE,
        portrait: ReferencePortrait {
            head: (0, 6),
            hair_rows: 0,
            eyes: (4, 2, 5),
            mouth: (7, 3, 4),
            skin: 188.0,
            feature: 50.0,
            hair: 50.0,
            ground: 230.0,
        },
    },
    SeededCelebrity {
        id: "Vk4Jz9Pt",
        name: "Amara Sandoval",
        urls: &["www.example.com/celebrity/Vk4Jz9Pt"],
        known_gender: KNOWN_GENDER_NONBINARY,
        portrait: ReferencePortrait {
            head: (1, 7),
            hair_rows: 1,
            eyes: (2, 3, 6),
            mouth: (5, 4, 5),
            skin: 136.0,
            feature: 34.0,
            hair: 42.0,
            ground: 102.0,
        },
    },
    SeededCelebrity {
        id: "Ly6Dk3Rw",
        name: "Oskar Lindqvist",
        urls: &["www.example.com/celebrity/Ly6Dk3Rw"],
        known_gender: KNOWN_GENDER_UNLISTED,
        portrait: ReferencePortrait {
            head: (0, 7),
            hair_rows: 0,
            eyes: (3, 2, 5),
            mouth: (5, 3, 5),
            skin: 158.0,
            feature: 44.0,
            hair: 44.0,
            ground: 170.0,
        },
    },
];

/// One celebrity of the catalogue: the identity this service reports for that person, and the
/// appearance of the reference face a submitted face is resolved against.
#[derive(Debug, Clone)]
pub struct CatalogueEntry {
    id: String,
    name: String,
    urls: Vec<String>,
    known_gender: String,
    reference: FaceAppearance,
}

impl CatalogueEntry {
    /// This entry's `Id`: "a unique identifier for the celebrity", and the value
    /// `GetCelebrityInfo` resolves this entry through.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// This entry's `Name`: "the name of the celebrity".
    pub fn name(&self) -> &str {
        &self.name
    }

    /// This entry's `Urls`: "an array of URLs pointing to additional information about the
    /// celebrity".
    pub fn urls(&self) -> Vec<String> {
        self.urls.clone()
    }

    /// This entry's `KnownGender`, as the modelled shape carries it.
    pub fn known_gender(&self) -> KnownGender {
        KnownGender {
            r#type: Some(self.known_gender.clone()),
        }
    }
}

/// What the catalogue answered for one measured face: the entry it was identified as, and the
/// resemblance measured between that face and the entry's reference face.
#[derive(Debug, Clone, Copy)]
pub struct Recognised<'entry> {
    entry: &'entry CatalogueEntry,
    match_confidence: f32,
}

impl<'entry> Recognised<'entry> {
    /// The catalogue entry this face was identified as.
    pub fn entry(&self) -> &'entry CatalogueEntry {
        self.entry
    }

    /// The resemblance measured between the face and that entry's reference face, which is
    /// what `Celebrity.MatchConfidence` reports.
    pub fn match_confidence(&self) -> f32 {
        self.match_confidence
    }

    /// This recognition as the modelled `Celebrity` the answer carries, around the
    /// `ComparedFace` the detector measured for the face itself: "the Celebrity object
    /// contains the celebrity name, ID, URL links to additional information, match confidence,
    /// and a ComparedFace object that you can use to locate the celebrity's face on the
    /// image".
    pub fn celebrity(&self, face: ComparedFace) -> Celebrity {
        Celebrity {
            urls: Some(self.entry.urls()),
            name: Some(self.entry.name().to_string()),
            id: Some(self.entry.id().to_string()),
            face: Some(face),
            match_confidence: Some(self.match_confidence),
            known_gender: Some(self.entry.known_gender()),
        }
    }
}

/// The celebrities this service holds: seeded from [`SEEDED`] and read by every call, never
/// written by one.
#[derive(Debug)]
pub struct CelebrityCatalogue {
    entries: Vec<CatalogueEntry>,
}

impl Default for CelebrityCatalogue {
    fn default() -> Self {
        Self {
            entries: SEEDED
                .iter()
                .map(|seeded| CatalogueEntry {
                    id: seeded.id.to_string(),
                    name: seeded.name.to_string(),
                    urls: seeded.urls.iter().map(|url| url.to_string()).collect(),
                    known_gender: seeded.known_gender.to_string(),
                    reference: FaceAppearance::of_cell_tones(seeded.portrait.cells()),
                })
                .collect(),
        }
    }
}

impl CelebrityCatalogue {
    /// The entry this catalogue holds under `id`, or `None` for an identifier it does not
    /// hold: what `GetCelebrityInfo` resolves its request's `Id` through.
    pub fn by_id(&self, id: &str) -> Option<&CatalogueEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Every entry of this catalogue, in the order it is held.
    pub fn entries(&self) -> &[CatalogueEntry] {
        &self.entries
    }

    /// Which celebrity, if any, each of `faces` IS: for every measured appearance in turn, the
    /// entry whose reference face it resembles most of those not already taken by an earlier
    /// face of the same answer, and `None` for a face that resembles no entry of the catalogue
    /// at all ([`MINIMUM_RESEMBLANCE`]).
    ///
    /// The faces are offered in the order the answer reports them, which is largest first, so
    /// a contested entry goes to the larger face of the image.
    pub fn identify(&self, faces: &[FaceAppearance]) -> Vec<Option<Recognised<'_>>> {
        let mut taken = vec![false; self.entries.len()];
        let mut identified = Vec::with_capacity(faces.len());
        for face in faces {
            let mut best: Option<(usize, f32)> = None;
            for (index, entry) in self.entries.iter().enumerate() {
                if taken[index] {
                    continue;
                }
                let resemblance = face.similarity_to(&entry.reference);
                // A face resembles an entry when its own light and dark rise and fall WITH
                // that reference face's at all; of the entries it resembles, it is the one it
                // resembles most.
                if resemblance > MINIMUM_RESEMBLANCE
                    && best.is_none_or(|(_, most)| resemblance > most)
                {
                    best = Some((index, resemblance));
                }
            }
            identified.push(best.map(|(index, match_confidence)| {
                taken[index] = true;
                Recognised {
                    entry: &self.entries[index],
                    match_confidence,
                }
            }));
        }
        identified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry is held under its own identifier, and each is a `RekognitionUniqueId`:
    /// `pattern ^[0-9A-Za-z]*$`, and non-empty, since it has to name an entry.
    #[test]
    fn every_entry_is_held_under_its_own_well_formed_identifier() {
        let catalogue = CelebrityCatalogue::default();
        for entry in catalogue.entries() {
            assert!(
                !entry.id().is_empty()
                    && entry
                        .id()
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()),
                "case {:?}: an Id is a non-empty RekognitionUniqueId",
                entry.id()
            );
            let held = catalogue.by_id(entry.id()).map(CatalogueEntry::name);
            assert_eq!(
                held,
                Some(entry.name()),
                "case {:?}: the catalogue resolves its own identifier, got {held:?}",
                entry.id()
            );
            let same_count = catalogue
                .entries()
                .iter()
                .filter(|other| other.id() == entry.id())
                .count();
            assert_eq!(
                same_count, 1,
                "case {:?}: one identifier names one entry, got {same_count} entries",
                entry.id()
            );
        }
    }

    /// A face that IS an entry's reference face resolves to that entry, at the top of the
    /// resemblance scale: the measurement the identification is made by is the one
    /// `FaceAppearance` computes, so a reference face resolves to itself.
    #[test]
    fn a_reference_face_resolves_to_its_own_entry() {
        let catalogue = CelebrityCatalogue::default();
        for seeded in &SEEDED {
            let face = FaceAppearance::of_cell_tones(seeded.portrait.cells());
            let identified = catalogue.identify(&[face]);
            let recognised = identified
                .first()
                .copied()
                .flatten()
                .unwrap_or_else(|| panic!("case {:?}: its own reference face resolves", seeded.id));
            assert_eq!(
                recognised.entry().id(),
                seeded.id,
                "case {:?}: a reference face is its own entry, got {:?}",
                seeded.id,
                recognised.entry().id()
            );
            assert!(
                (recognised.match_confidence() - 100.0).abs() < 0.01,
                "case {:?}: a face identical to the reference face is matched at the top of \
                 the scale, got {}",
                seeded.id,
                recognised.match_confidence()
            );
        }
    }

    /// One entry is one person: two faces of one answer never resolve to the same entry. And a
    /// face the catalogue holds no entry resembling is not identified: a flat patch, which
    /// "resembles nothing, including itself", correlates with nothing at all, so it falls below
    /// [`MINIMUM_RESEMBLANCE`] for every entry and is reported on the unrecognized side; and a
    /// resemblance being a SIGNED correlation, a face with a reference face's layout inverted
    /// is not that reference face's person.
    #[test]
    fn an_entry_is_matched_once_and_a_face_resembling_no_entry_is_not_identified() {
        let catalogue = CelebrityCatalogue::default();
        let first = FaceAppearance::of_cell_tones(SEEDED[0].portrait.cells());
        let flat = FaceAppearance::of_cell_tones(vec![128.0; REFERENCE_CELLS]);
        let identified = catalogue.identify(&[first.clone(), first, flat]);
        let (left, right) = (identified[0], identified[1]);
        assert!(
            left.is_some() && right.is_some(),
            "case {:?}: both faces resolve, got {identified:?}",
            "the same face twice"
        );
        assert_ne!(
            left.map(|found| found.entry().id().to_string()),
            right.map(|found| found.entry().id().to_string()),
            "case {:?}: one entry is reported once per answer, got {identified:?}",
            "the same face twice"
        );
        assert!(
            identified[2].is_none(),
            "case {:?}: a patch that resembles nothing is not identified, got {:?}",
            "a flat patch",
            identified[2]
        );

        // One negative per seeded portrait: the same layout with its tones turned over. A
        // resemblance is a SIGNED correlation, so an inverted face resembles the face it was
        // inverted from not at all — whatever else the catalogue holds, it is not that person.
        for seeded in &SEEDED {
            let negative: Vec<f32> = seeded
                .portrait
                .cells()
                .into_iter()
                .map(|tone| 255.0 - tone)
                .collect();
            let identified = catalogue.identify(&[FaceAppearance::of_cell_tones(negative)]);
            let resolved = identified[0].map(|found| found.entry().id().to_string());
            assert_ne!(
                resolved.as_deref(),
                Some(seeded.id),
                "case {:?}: the negative of a reference face is not that face, got {resolved:?}",
                seeded.id
            );
        }
    }

    /// Every match the catalogue reports is a resemblance it MEASURED — a `Percent`, above
    /// [`MINIMUM_RESEMBLANCE`] and no more than the top of the scale — and, offered on its
    /// own, a face is reported as the NEAREST entry of the catalogue: no entry resembles it
    /// more than the one it was named as. A FAINT resemblance is a match like any other: what
    /// names a face is being nearest, and how near it was is reported rather than being the
    /// difference between a celebrity and an anonymous face.
    #[test]
    fn a_reported_match_is_the_nearest_entry_and_carries_the_resemblance_measured_for_it() {
        let catalogue = CelebrityCatalogue::default();
        // Each row is a grid of its own: the reference faces themselves, two faces the
        // catalogue was not built from — a slope of light across the box, and a grid whose
        // every other cell is dark — and each reference face mixed with that slope in a
        // growing proportion, which is a face that resembles its entry ever more faintly.
        let slope: Vec<f32> = (0..REFERENCE_CELLS).map(|cell| cell as f32).collect();
        let mut faces: Vec<(String, FaceAppearance)> = SEEDED
            .iter()
            .map(|seeded| {
                (
                    seeded.id.to_string(),
                    FaceAppearance::of_cell_tones(seeded.portrait.cells()),
                )
            })
            .collect();
        faces.push((
            "a slope of light across the box".to_string(),
            FaceAppearance::of_cell_tones(slope.clone()),
        ));
        faces.push((
            "every other cell dark".to_string(),
            FaceAppearance::of_cell_tones(
                (0..REFERENCE_CELLS)
                    .map(|cell| if cell.is_multiple_of(2) { 40.0 } else { 200.0 })
                    .collect(),
            ),
        ));
        for seeded in &SEEDED {
            for weight in [2.0_f32, 8.0, 32.0] {
                let mixed: Vec<f32> = seeded
                    .portrait
                    .cells()
                    .into_iter()
                    .zip(slope.iter())
                    .map(|(tone, slope)| tone + weight * slope)
                    .collect();
                faces.push((
                    format!("{} under a slope of {weight}", seeded.id),
                    FaceAppearance::of_cell_tones(mixed),
                ));
            }
        }
        // Patterns no face carries: stripes and chequers of several periods, a bright frame,
        // one dark cell. A grid of this kind rises and falls with a reference face hardly at
        // all, which is what a FAINT resemblance is.
        let span = APPEARANCE_GRID;
        let patterns: Vec<(&str, Vec<f32>)> = vec![
            (
                "columns in stripes",
                (0..REFERENCE_CELLS)
                    .map(|cell| {
                        if (cell % span).is_multiple_of(2) {
                            40.0
                        } else {
                            210.0
                        }
                    })
                    .collect(),
            ),
            (
                "rows in stripes",
                (0..REFERENCE_CELLS)
                    .map(|cell| {
                        if (cell / span).is_multiple_of(2) {
                            40.0
                        } else {
                            210.0
                        }
                    })
                    .collect(),
            ),
            (
                "chequers two cells wide",
                (0..REFERENCE_CELLS)
                    .map(|cell| {
                        if ((cell % span) / 2 + (cell / span) / 2).is_multiple_of(2) {
                            60.0
                        } else {
                            190.0
                        }
                    })
                    .collect(),
            ),
            (
                "a bright frame around the box",
                (0..REFERENCE_CELLS)
                    .map(|cell| {
                        let (row, column) = (cell / span, cell % span);
                        let edge = row == 0 || column == 0 || row == span - 1
                            || column == span - 1;
                        if edge {
                            220.0
                        } else {
                            80.0
                        }
                    })
                    .collect(),
            ),
            (
                "one dark cell in the middle",
                (0..REFERENCE_CELLS)
                    .map(|cell| if cell == REFERENCE_CELLS / 2 + 4 { 20.0 } else { 180.0 })
                    .collect(),
            ),
            (
                "a sweep across the diagonal",
                (0..REFERENCE_CELLS)
                    .map(|cell| ((cell % span) * 7 + (cell / span) * 3) as f32 % 97.0)
                    .collect(),
            ),
        ];
        for (case, cells) in patterns {
            faces.push((case.to_string(), FaceAppearance::of_cell_tones(cells)));
        }
        // The level two unrelated grids of this many cells correlate at: one over the square
        // root of their cells. A resemblance below it is FAINT — nearer than nothing, and no
        // nearer than chance — and the rows below include such a face.
        // A match a long way below the top of the scale — the kind of reading a PHOTOGRAPHED
        // face gives, where the box the appearance is measured over carries hair, shadow and
        // whatever stands behind the head beside the face itself. The table has to include
        // one, or it says nothing about a faint match being a match.
        const FAINT: f32 = 20.0;
        let mut faint_matches = 0_usize;
        let mut measured: Vec<String> = Vec::new();
        for (case, face) in &faces {
            let nearest = catalogue
                .entries()
                .iter()
                .map(|entry| face.similarity_to(&entry.reference))
                .fold(0.0_f32, f32::max);
            measured.push(format!("{case}:{nearest:.1}"));
            let identified = catalogue.identify(std::slice::from_ref(face));
            let confidence = identified[0].map(|found| found.match_confidence());
            if nearest > MINIMUM_RESEMBLANCE {
                assert!(
                    confidence
                        .is_some_and(|measured| (measured - nearest).abs() < 0.01
                            && measured > MINIMUM_RESEMBLANCE
                            && measured <= 100.0),
                    "case {case:?}: the nearest entry is at {nearest}, so that is the match \
                     reported, got {confidence:?}"
                );
                if nearest < FAINT {
                    faint_matches += 1;
                }
            } else {
                assert!(
                    confidence.is_none(),
                    "case {case:?}: a face no entry resembles is identified as none of them, \
                     got {confidence:?}"
                );
            }
        }
        assert!(
            faint_matches > 0,
            "case {:?}: the table must include a face named on a resemblance below {FAINT}, \
             got {faint_matches} of {measured:?}",
            "a faint resemblance"
        );
    }

    /// One entry is one PERSON, so a catalogue of a given size names at most that many faces
    /// of one answer: an answer carrying more faces than the catalogue holds entries reports
    /// the rest on the unrecognized side rather than naming one person twice.
    #[test]
    fn an_answer_with_more_faces_than_the_catalogue_holds_leaves_the_rest_unidentified() {
        let catalogue = CelebrityCatalogue::default();
        let held = catalogue.entries().len();
        let crowd: Vec<FaceAppearance> = (0..held + 2)
            .map(|index| {
                FaceAppearance::of_cell_tones(SEEDED[index % SEEDED.len()].portrait.cells())
            })
            .collect();
        let identified = catalogue.identify(&crowd);
        let named: Vec<String> = identified
            .iter()
            .filter_map(|found| found.map(|found| found.entry().id().to_string()))
            .collect();
        assert!(
            named.len() <= held,
            "case {:?}: at most the {held} entries held can be named, got {named:?}",
            "more faces than entries"
        );
        let mut distinct = named.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            named.len(),
            "case {:?}: one entry is one person, got {named:?}",
            "more faces than entries"
        );
        let unidentified = identified.iter().filter(|found| found.is_none()).count();
        assert!(
            unidentified >= crowd.len() - held,
            "case {:?}: the {} face(s) past the catalogue's {held} entries are unidentified, \
             got {unidentified}",
            "more faces than entries",
            crowd.len() - held
        );
    }
}
