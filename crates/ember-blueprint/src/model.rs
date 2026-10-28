//! Version 1 data types. Public builders require validation before consumption.
use crate::{Error, Limits};
use serde::{Deserialize, Serialize};

/// One complete guide snapshot. All geometry uses the declared canonical frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blueprint {
    /// Exactly 1.0 for this contract.
    pub schema_version: String,
    /// Display title.
    pub title: String,
    /// Stable source identity, adapter identity and content revision.
    pub source: Source,
    /// Explicit units and coordinate mapping.
    pub frame: Frame,
    /// Stable construction stages.
    pub stages: Vec<Stage>,
    /// Stable floor or elevation filters.
    pub layers: Vec<Layer>,
    /// Local alignment points. No engine or save position is implied.
    pub anchors: Vec<Anchor>,
    /// Selectable structures, including unsupported structures with omission notes.
    pub objects: Vec<Object>,
}

impl Blueprint {
    /// Check every value and reference before a consumer can display the model.
    pub fn validate(self, limits: Limits) -> Result<ValidatedBlueprint, Error> {
        crate::validate::validate(self, limits)
    }
}

/// A fully validated model. Its contents cannot be mutated without revalidation.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedBlueprint(pub(crate) Blueprint);

impl ValidatedBlueprint {
    /// Read the complete document.
    pub fn document(&self) -> &Blueprint {
        &self.0
    }

    /// Read the source revision.
    pub fn revision(&self) -> &Revision {
        &self.0.source.revision
    }

    /// Serialize only a validated document.
    pub fn to_json(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec_pretty(&self.0)
            .map_err(|e| Error::new(crate::ErrorKind::InvalidJson, "document", e.to_string()))
    }
}

/// Identity of an upstream guide and the adapter which interpreted it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Guide identity stable across all its content changes.
    pub id: String,
    /// Interpretation contract, such as highcrown-v1 or direct-v1.
    pub adapter: String,
    /// Informational upstream version. It is not a change detector.
    pub content_version: String,
    /// Ordered source revision with the exact source snapshot hash.
    pub revision: Revision,
}

/// An ordered revision. The producer owns and persists the increasing sequence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    /// A positive integer, at most the JSON safe-integer bound.
    pub sequence: u64,
    /// Lowercase SHA-256 of the exact upstream bytes, not a hash of this field.
    pub content_sha256: String,
}

/// The canonical frame. Adapter notes state how source axes became these axes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    /// Exactly metres. Converters must normalize other units explicitly.
    pub units: String,
    /// Exactly right_handed_x_east_y_up_z_south.
    pub axes: String,
    /// Human-readable mapping and origin. No implicit engine coordinates.
    pub source_mapping: String,
}

/// A construction phase. Object membership is explicit structured source association.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    /// Stable phase ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Display order; it is not the stable identity.
    pub order: u32,
    /// Explicit source references outside this adapter's supported object inventory.
    #[serde(default)]
    pub unresolved: Vec<String>,
}

/// One elevation or floor filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Stable layer ID, independent of array order.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Canonical Y elevation in metres.
    pub elevation: f64,
}

/// A guide-local alignment point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    /// Stable alignment identity.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Canonical coordinates in metres.
    pub point: [f64; 3],
}

/// A selectable guide structure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Object {
    /// Stable source-derived identity, never an array index.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Source collection and stable element identity.
    pub source_ref: String,
    /// Associated stage IDs. An empty list means unassigned.
    pub stages: Vec<String>,
    /// Geometry supported by this conversion.
    pub primitives: Vec<Primitive>,
    /// Unsupported or incomplete details. Empty geometry requires an explanation.
    pub omissions: Vec<String>,
}

/// Geometry with a stable identity, floor filter and explicit fidelity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Primitive {
    /// Stable part ID, unique throughout the document.
    pub id: String,
    /// Optional layer ID.
    pub layer: Option<String>,
    /// What the geometry actually proves.
    pub fidelity: Fidelity,
    /// Interpretation and limits of the primitive.
    pub note: String,
    /// Geometry in canonical metres.
    pub geometry: Geometry,
}

/// Fidelity applies to this primitive, not the whole source guide.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// A specified layer mask, including all recorded cutouts.
    ExactLayer,
    /// Recorded outer bounds, before unmodelled details or openings.
    RecordedEnvelope,
    /// A deliberately approximate massing shape, labelled by its adapter.
    ApproximateMassing,
    /// A measurement reference or profile, not a solid placement instruction.
    ReferenceOutline,
}

/// A rectangular opening. Its coordinates are canonical X and Z.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cutout {
    /// Southwest in canonical numeric coordinates: minimum X and minimum Z.
    pub min: [f64; 2],
    /// Positive X width and Z depth in metres.
    pub size: [f64; 2],
}

/// Small, engine-independent primitives. No tessellation or block palette is implied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Geometry {
    /// An axis-aligned solid envelope.
    Box {
        /// Minimum X, Y, Z.
        min: [f64; 3],
        /// Positive extents along X, Y, Z.
        size: [f64; 3],
    },
    /// A horizontal layer. Openings pass through its entire Y thickness.
    Slab {
        /// Minimum X, Y, Z; the walking surface is min Y plus size Y.
        min: [f64; 3],
        /// Positive extents along X, Y, Z.
        size: [f64; 3],
        /// Empty rectangles to preserve. Overlapping openings form a union.
        openings: Vec<Cutout>,
    },
    /// A recorded line or profile with no invented thickness.
    Polyline {
        /// Ordered canonical vertices.
        points: Vec<[f64; 3]>,
        /// Whether the last vertex connects to the first.
        closed: bool,
    },
    /// A centreline wall envelope. Endpoints and joins are not an assembly recipe.
    WallPath {
        /// Ordered centreline vertices at one common base Y.
        points: Vec<[f64; 3]>,
        /// Positive full width perpendicular to each segment.
        thickness: f64,
        /// Positive height above the common base Y.
        height: f64,
    },
    /// A round envelope with a vertical axis.
    Cylinder {
        /// Centre of the bottom circle.
        base: [f64; 3],
        /// Positive radius.
        radius: f64,
        /// Positive Y height.
        height: f64,
    },
    /// A horizontal measurement circle, with no raster or thickness.
    Circle {
        /// Circle centre.
        center: [f64; 3],
        /// Positive radius.
        radius: f64,
    },
}
