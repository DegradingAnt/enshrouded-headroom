//! Highcrown 3.0 adapter. It does not reuse the retired website massing renderer.
use crate::{
    validate, Anchor, Blueprint, Cutout, Error, ErrorKind, Fidelity, Frame, Geometry, Layer,
    Limits, Object, Primitive, Revision, Source, Stage, ValidatedBlueprint,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Convert one bounded, complete Highcrown plan snapshot.
///
/// The mapping is source (x, height, -y) to canonical (X, Y, Z), in metres.
/// The local altar support is an anchor; private save/world positions are ignored.
/// Source version 3.0 is informational for changes; the exact bytes are hashed.
/// The producer must supply a positive monotonic sequence for exported revisions.
pub fn convert_highcrown(
    bytes: &[u8],
    source_id: &str,
    sequence: u64,
    limits: Limits,
) -> Result<ValidatedBlueprint, Error> {
    validate::input_size(bytes, limits)?;
    let plan: Plan = serde_json::from_slice(bytes)
        .map_err(|e| Error::new(ErrorKind::InvalidJson, "highcrown", e.to_string()))?;
    if plan.version != "3.0" {
        return Err(Error::new(
            ErrorKind::UnsupportedVersion,
            "highcrown.version",
            "this adapter interprets version 3.0 only",
        ));
    }
    if plan.block_m != 0.5 {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            "highcrown.block_m",
            "the guide's declared small grid must be 0.5 metres",
        ));
    }
    let mut inventory = BTreeMap::new();
    for (collection, objects) in [
        ("zones", &plan.zones),
        ("walls", &plan.walls),
        ("towers", &plan.towers),
        ("buildings", &plan.buildings),
        ("features", &plan.features),
        ("measurements", &plan.content.guide.measurements),
    ] {
        for object in objects {
            validate::id(&object.id, "highcrown.object.id")?;
            if inventory
                .insert(object.id.clone(), (collection, object))
                .is_some()
            {
                return Err(Error::new(
                    ErrorKind::DuplicateId,
                    "highcrown.objects",
                    "duplicate source object ID",
                ));
            }
        }
    }
    validate::count(inventory.len(), limits.max_objects, "highcrown.objects")?;
    let mut converter = Converter {
        levels: BTreeMap::new(),
        layers: BTreeMap::new(),
        memberships: BTreeMap::new(),
        pictures: BTreeMap::new(),
        floor_thickness: plan.block_m,
        limits,
        primitive_count: 0,
        point_count: 0,
    };
    validate::count(plan.levels.len(), 10_000, "highcrown.levels")?;
    validate::count(plan.content.guide.phases.len(), 2048, "highcrown.phases")?;
    for level in &plan.levels {
        if converter
            .levels
            .insert(level.id.clone(), level.elev)
            .is_some()
        {
            return Err(Error::new(
                ErrorKind::DuplicateId,
                "highcrown.levels",
                "duplicate level ID",
            ));
        }
        converter.layers.insert(
            level.id.clone(),
            Layer {
                id: level.id.clone(),
                name: level.name.clone(),
                elevation: level.elev,
            },
        );
    }
    let mut stages = Vec::new();
    for phase in &plan.content.guide.phases {
        let mut unresolved = BTreeSet::new();
        for session in &phase.sessions {
            for step in &session.steps {
                let mut refs: BTreeSet<String> = step
                    .measure_ids
                    .iter()
                    .chain(&step.placement_ids)
                    .chain(&step.host_ids)
                    .chain(&step.circle_floor_ids)
                    .cloned()
                    .collect();
                for reference in step
                    .sections
                    .iter()
                    .chain(&step.wall_budgets)
                    .chain(&step.round_wall_budgets)
                {
                    refs.insert(reference.object_id.clone());
                }
                for picture in step
                    .block_pictures
                    .iter()
                    .chain(&step.block_picture)
                    .flat_map(Pictures::iter)
                {
                    refs.insert(picture.object_id.clone());
                    if picture.view == "floor" {
                        converter
                            .pictures
                            .entry(picture.object_id.clone())
                            .or_default()
                            .push(FloorPicture {
                                top: step.placement_floor,
                                thickness: picture.layer.unwrap_or(plan.block_m),
                            });
                    }
                }
                for reference in refs {
                    if inventory.contains_key(&reference) {
                        converter
                            .memberships
                            .entry(reference)
                            .or_default()
                            .insert(phase.id.clone());
                    } else {
                        unresolved.insert(reference);
                    }
                }
            }
        }
        stages.push(Stage {
            id: phase.id.clone(),
            name: phase.name.clone(),
            order: phase.num,
            unresolved: unresolved.into_iter().collect(),
        });
    }
    let mut objects = Vec::new();
    for (id, (collection, raw)) in &inventory {
        converter.append(raw, id, collection, None, &mut objects)?;
    }
    objects.sort_by(|a, b| a.id.cmp(&b.id));
    stages.sort_by(|a, b| a.order.cmp(&b.order).then(a.id.cmp(&b.id)));
    Blueprint {
        schema_version: "1.0".to_owned(),
        title: plan.name,
        source: Source {
            id: source_id.to_owned(), adapter: "highcrown-v1".to_owned(),
            content_version: plan.version,
            revision: Revision { sequence, content_sha256: crate::sha256(bytes) },
        },
        frame: Frame {
            units: "metres".to_owned(),
            axes: "right_handed_x_east_y_up_z_south".to_owned(),
            source_mapping: "Source x is east, source y is north, height is up; canonical (X,Y,Z)=(x,height,-y), metres. Origin is source plan (0,0,0), independent of any engine/save frame.".to_owned(),
        },
        stages,
        layers: converter.layers.into_values().collect(),
        anchors: vec![Anchor {
            id: "altar-support".to_owned(), name: "Altar supporting floor centre".to_owned(),
            point: [plan.altar.x, plan.altar.abs, -plan.altar.y],
        }],
        objects,
    }.validate(limits)
}

/// Supported portions of the upstream plan. Other fields remain upstream data.
#[derive(Deserialize)]
struct Plan {
    /// Known upstream interpretation version.
    version: String,
    /// Public source title.
    name: String,
    /// Declared source metre grid.
    block_m: f64,
    /// Local reference, with no world positions.
    altar: PlanAltar,
    /// Source elevation records.
    levels: Vec<PlanLevel>,
    /// Zone reference bounds.
    #[serde(default)]
    zones: Vec<PlanObject>,
    /// Recorded wall paths.
    #[serde(default)]
    walls: Vec<PlanObject>,
    /// Recorded tower bounds.
    #[serde(default)]
    towers: Vec<PlanObject>,
    /// Recorded building envelopes.
    #[serde(default)]
    buildings: Vec<PlanObject>,
    /// Feature reference bounds.
    #[serde(default)]
    features: Vec<PlanObject>,
    /// Explicit construction and measurement data.
    content: PlanContent,
}

/// Source-local altar reference.
#[derive(Deserialize)]
struct PlanAltar {
    /// Plan east coordinate.
    x: f64,
    /// Plan north coordinate.
    y: f64,
    /// Plan support elevation.
    abs: f64,
}

/// One explicit guide level.
#[derive(Deserialize)]
struct PlanLevel {
    /// Stable source ID.
    id: String,
    /// Display name.
    name: String,
    /// Absolute plan elevation.
    elev: f64,
}

/// Content container.
#[derive(Deserialize)]
struct PlanContent {
    /// Construction guide data.
    guide: PlanGuide,
}

/// Guide geometry and phase references.
#[derive(Deserialize)]
struct PlanGuide {
    /// Additional explicitly measured structures.
    #[serde(default)]
    measurements: Vec<PlanObject>,
    /// Ordered phases with explicit object references.
    phases: Vec<PlanPhase>,
}

/// Accept only the two observed representations of this Highcrown metadata field.
fn deserialize_phase_order<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_any(PhaseOrderVisitor)
}

/// Field-local parsing keeps public stage orders and geometry strictly numeric.
struct PhaseOrderVisitor;

impl<'de> serde::de::Visitor<'de> for PhaseOrderVisitor {
    type Value = u32;

    /// Describe the accepted source representations without including source content.
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a u32 integer or canonical decimal u32 string")
    }

    /// JSON nonnegative integer tokens must fit the existing public order type.
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        u32::try_from(value).map_err(|_| E::custom("phase order exceeds u32"))
    }

    /// Bound ASCII digits before checked conversion; only zero may start with zero.
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if value.is_empty()
            || value.len() > 10
            || !value.bytes().all(|digit| digit.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(E::custom("phase order requires canonical decimal u32 text"));
        }
        value
            .parse::<u32>()
            .map_err(|_| E::custom("phase order exceeds u32"))
    }
}
/// Construction phase.
#[derive(Deserialize)]
struct PlanPhase {
    /// Stable source ID.
    id: String,
    /// Public name.
    name: String,
    /// Display order, accepting only the observed integer/decimal-string source forms.
    #[serde(deserialize_with = "deserialize_phase_order")]
    num: u32,
    /// Construction sessions.
    sessions: Vec<PlanSession>,
}

/// A group of construction steps.
#[derive(Deserialize)]
struct PlanSession {
    /// Structured steps; prose is intentionally not interpreted as geometry.
    steps: Vec<PlanStep>,
}

/// Explicit associations only; no ID-prefix or HTML heuristics.
#[derive(Deserialize)]
struct PlanStep {
    /// Measured object references.
    #[serde(default)]
    measure_ids: Vec<String>,
    /// Placement object references.
    #[serde(default)]
    placement_ids: Vec<String>,
    /// Host structure references.
    #[serde(default)]
    host_ids: Vec<String>,
    /// Explicit round-floor references.
    #[serde(default)]
    circle_floor_ids: Vec<String>,
    /// Optional explicit floor elevation.
    placement_floor: Option<f64>,
    /// Multiple or single drawing configuration.
    block_pictures: Option<Pictures>,
    /// Legacy singular key uses the same source contract.
    block_picture: Option<Pictures>,
    /// Structured section references.
    #[serde(default)]
    sections: Vec<PlanReference>,
    /// Structured wall references.
    #[serde(default)]
    wall_budgets: Vec<PlanReference>,
    /// Structured circular wall references.
    #[serde(default)]
    round_wall_budgets: Vec<PlanReference>,
}

/// A structured source object reference.
#[derive(Deserialize)]
struct PlanReference {
    /// Source ID.
    object_id: String,
}

/// The source supports both one picture and an array.
#[derive(Deserialize)]
#[serde(untagged)]
enum Pictures {
    /// One configuration.
    One(PlanPicture),
    /// Several configurations.
    Many(Vec<PlanPicture>),
}

impl Pictures {
    /// Iterate both source spellings without cloning.
    fn iter(&self) -> std::slice::Iter<'_, PlanPicture> {
        match self {
            Self::One(one) => std::slice::from_ref(one).iter(),
            Self::Many(many) => many.iter(),
        }
    }
}

/// Only floor configurations can imply a horizontal layer.
#[derive(Deserialize)]
struct PlanPicture {
    /// Measured source object.
    object_id: String,
    /// Floor, wall or other view.
    view: String,
    /// Explicit metre thickness; absent uses the guide's one-small-floor contract.
    layer: Option<f64>,
}

/// Parsed source geometry. Missing dimensions never receive guessed values.
#[derive(Deserialize)]
struct PlanObject {
    /// Stable source ID.
    id: String,
    /// Display name.
    name: String,
    /// Elevation reference.
    level: Option<String>,
    /// Southwest plan east.
    x: Option<f64>,
    /// Southwest plan north.
    y: Option<f64>,
    /// East span.
    w: Option<f64>,
    /// North span.
    d: Option<f64>,
    /// Source footprint type.
    shape: Option<String>,
    /// Source classification.
    kind: Option<String>,
    /// Original walls mix curtain tops and relative parapet/revetment heights.
    #[serde(rename = "type")]
    wall_type: Option<String>,
    /// New measured wall records declare this role.
    expansion_role: Option<String>,
    /// Absolute floor top.
    abs_floor: Option<f64>,
    /// Absolute envelope top.
    abs_eaves: Option<f64>,
    /// Absolute part base.
    abs_z0: Option<f64>,
    /// Absolute part top.
    abs_z1: Option<f64>,
    /// Relative part base or explicit solid-feature base.
    z0: Option<f64>,
    /// Relative part top.
    z1: Option<f64>,
    /// Explicit source base.
    base: Option<f64>,
    /// Absolute tower top.
    height: Option<f64>,
    /// Absolute wall top, as displayed by the guide.
    height_total: Option<f64>,
    /// Explicit path thickness.
    thickness: Option<f64>,
    /// Explicit bottom and top deck levels.
    deck: Option<[f64; 2]>,
    /// Recorded wall centreline.
    polyline: Option<Vec<[f64; 2]>>,
    /// Explicit floor surfaces, without inferred storey counts.
    #[serde(default)]
    floor_levels: Vec<f64>,
    /// Explicit openings in floor layers.
    #[serde(default)]
    floor_openings: Vec<PlanOpening>,
    /// Unsupported cutout semantics prevent an exact slab claim.
    cutouts: Option<serde_json::Value>,
    /// Absent is allowed; a declared region union must be a valid nonempty array.
    #[serde(default, deserialize_with = "deserialize_floor_regions")]
    floor_regions: Option<Vec<PlanRegion>>,
    /// Alternate negative masks must not disappear during conversion.
    holes: Option<serde_json::Value>,
    /// Alternate reserved voids must not disappear during conversion.
    voids: Option<serde_json::Value>,
    /// Special floor patterns are never silently filled solid.
    floor_pattern: Option<String>,
    /// Only explicitly solid feature bases use z0 as absolute.
    solid: Option<bool>,
    /// Recorded local roof cross-section.
    roofgeo: Option<PlanRoof>,
    /// Explicit substructure records.
    #[serde(default)]
    parts: Vec<PlanObject>,
}

/// A positive floor region with the shape accepted by the source mask contract.
#[derive(Deserialize)]
struct PlanRegion {
    /// Absolute plan east coordinate.
    x: f64,
    /// Absolute plan north coordinate.
    y: f64,
    /// Positive east extent.
    w: f64,
    /// Positive north extent.
    d: f64,
    /// Missing/null is rectangular, as in the guide; specialized shapes are invalid.
    shape: Option<String>,
}

/// Preserve declared-field presence: an explicit null must not deserialize as absence.
fn deserialize_floor_regions<'de, D>(deserializer: D) -> Result<Option<Vec<PlanRegion>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let regions = Vec::<PlanRegion>::deserialize(deserializer)?;
    let on_grid = |value: f64| {
        value.is_finite()
            && value.abs() <= 1_000_000.0
            && (value * 2.0 - (value * 2.0).round()).abs() < 1e-7
    };
    if regions.is_empty()
        || regions.len() > 64
        || regions.iter().any(|region| {
            ![region.x, region.y, region.w, region.d]
                .into_iter()
                .all(on_grid)
                || region.w <= 0.0
                || region.d <= 0.0
                || !matches!(
                    region.shape.as_deref(),
                    None | Some("rect" | "rectangle" | "square")
                )
        })
    {
        return Err(serde::de::Error::custom("declared floor_regions must contain 1 to 64 positive, finite, grid-aligned rectangular regions"));
    }
    Ok(Some(regions))
}

/// One source opening. Shape and clipping determine whether a rectangular cutout is faithful.
#[derive(Deserialize)]
struct PlanOpening {
    /// Absolute plan east.
    x: f64,
    /// Absolute plan north.
    y: f64,
    /// Width.
    w: f64,
    /// Depth.
    d: f64,
    /// Absent or rect/rectangle is rectangular; other shapes need a mask adapter.
    shape: Option<String>,
    /// Clipping against the floor boundary needs a mask adapter.
    clip: Option<bool>,
    /// Applicable floor surfaces; absent applies to all recorded floors.
    at_floors: Option<Vec<f64>>,
}

/// A recorded source roof profile, not a full roof mesh.
#[derive(Deserialize)]
struct PlanRoof {
    /// Source ridge direction.
    axis: String,
    /// Profile vertices; optional further fields describe source rendering.
    prof: Vec<Vec<serde_json::Value>>,
}

/// An explicit requested floor picture.
struct FloorPicture {
    /// Step-local absolute surface when present.
    top: Option<f64>,
    /// Declared or guide-contract thickness.
    thickness: f64,
}

/// Parent values used only where source part offsets are explicitly relative.
struct Parent {
    /// Stable parent ID.
    id: String,
    /// Explicit parent base, if known.
    base: Option<f64>,
    /// Inherited source level.
    layer: Option<String>,
    /// Explicit parent phase memberships.
    stages: Vec<String>,
}

/// Conversion scratch lives entirely outside any runtime hook.
struct Converter {
    /// Source level lookup.
    levels: BTreeMap<String, f64>,
    /// Output layer inventory.
    layers: BTreeMap<String, Layer>,
    /// Explicit source phase memberships.
    memberships: BTreeMap<String, BTreeSet<String>>,
    /// Explicit floor drawing requests.
    pictures: BTreeMap<String, Vec<FloorPicture>>,
    /// The guide specifies thin floors as one small grid cell.
    floor_thickness: f64,
    /// Complete output resource budgets.
    limits: Limits,
    /// Primitive budget consumed so far.
    primitive_count: usize,
    /// Vertex/opening budget consumed so far.
    point_count: usize,
}

impl Converter {
    /// Validate each small primitive before it consumes the complete output budget.
    fn add(&mut self, out: &mut Object, primitive: Primitive) -> Result<(), Error> {
        let points = validate::geometry(&primitive.geometry).map_err(|error| {
            Error::new(
                error.kind,
                format!("highcrown.objects.{}/{}", out.id, error.path),
                error.message,
            )
        })?;
        let primitives = self.primitive_count.saturating_add(1);
        let points = self.point_count.saturating_add(points);
        validate::count(
            primitives,
            self.limits.max_primitives,
            "highcrown.primitives",
        )?;
        validate::count(points, self.limits.max_points, "highcrown.geometry points")?;
        self.primitive_count = primitives;
        self.point_count = points;
        out.primitives.push(primitive);
        Ok(())
    }

    /// Add one source object and any explicitly recorded parts.
    fn append(
        &mut self,
        raw: &PlanObject,
        id: &str,
        collection: &str,
        parent: Option<&Parent>,
        output: &mut Vec<Object>,
    ) -> Result<(), Error> {
        validate::count(
            output.len().saturating_add(1),
            self.limits.max_objects,
            "highcrown.objects including parts",
        )?;
        validate::count(raw.floor_levels.len(), 10_000, "highcrown.floor_levels")?;
        validate::count(raw.floor_openings.len(), 4096, "highcrown.floor_openings")?;
        let base = self.base(raw, parent)?;
        let layer = raw
            .level
            .clone()
            .or_else(|| parent.and_then(|p| p.layer.clone()));
        let stages: Vec<String> = self
            .memberships
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_else(|| parent.map(|p| p.stages.clone()).unwrap_or_default());
        let source_ref = match parent {
            Some(p) => format!("{collection}/{}/parts/{}", p.id, raw.id),
            None => format!("{collection}/{}", raw.id),
        };
        let mut object = Object {
            id: id.to_owned(),
            name: if raw.name.is_empty() {
                id.to_owned()
            } else {
                raw.name.clone()
            },
            source_ref,
            stages: stages.clone(),
            primitives: Vec::new(),
            omissions: Vec::new(),
        };
        let bounds = self.bounds(raw)?;
        if let Some(base) = base {
            if let Some(points) = &raw.polyline {
                self.wall(raw, &mut object, points, base, layer.clone())?;
            } else if let Some(bounds) = bounds {
                self.footprint(raw, &mut object, bounds, base, layer.clone())?;
                self.envelope(raw, &mut object, bounds, base, layer.clone(), parent)?;
                self.floors(raw, &mut object, bounds, base)?;
                self.roof(raw, &mut object, bounds, layer.clone())?;
            } else {
                object.omissions.push(
                    "No complete recorded footprint or wall path; geometry omitted.".to_owned(),
                );
            }
        } else {
            object.omissions.push(
                "No explicit floor, deck, part base or declared source level; geometry omitted."
                    .to_owned(),
            );
        }
        object.omissions.push("This adapter exports recorded envelopes, reference outlines and supported rectangular layers. Openings in walls, interiors, roof assembly, props, raster circles and other source drawing systems remain unmodelled.".to_owned());
        output.push(object);
        let parent = Parent {
            id: id.to_owned(),
            base,
            layer,
            stages,
        };
        for part in &raw.parts {
            self.append(
                part,
                &format!("{id}/{}", part.id),
                collection,
                Some(&parent),
                output,
            )?;
        }
        Ok(())
    }

    /// Follow the guide's explicit height precedence, with declared part-relative offsets.
    fn base(&self, raw: &PlanObject, parent: Option<&Parent>) -> Result<Option<f64>, Error> {
        let value = if let Some(value) = raw.abs_z0 {
            Some(value)
        } else if let Some(value) = raw.abs_floor {
            Some(value)
        } else if let Some([bottom, top]) = raw.deck {
            if top < bottom {
                return Err(Error::new(
                    ErrorKind::InvalidValue,
                    "highcrown.deck",
                    "deck top is below its bottom",
                ));
            }
            Some(top)
        } else if let Some(value) = raw.base {
            Some(value)
        } else if let Some(parent) = parent {
            parent.base.zip(raw.z0).map(|(base, offset)| base + offset)
        } else if raw.solid == Some(true) {
            raw.z0
        } else {
            raw.level
                .as_ref()
                .and_then(|id| self.levels.get(id))
                .copied()
        };
        if let Some(value) = value {
            validate::scalar(value, "highcrown.base")?;
        }
        Ok(value)
    }

    /// Read complete measured bounds; absent fields do not imply zero or a default span.
    fn bounds(&self, raw: &PlanObject) -> Result<Option<[f64; 4]>, Error> {
        let (Some(x), Some(y), Some(w), Some(d)) = (raw.x, raw.y, raw.w, raw.d) else {
            return Ok(None);
        };
        validate::scalar(x, "highcrown.x")?;
        validate::scalar(y, "highcrown.y")?;
        validate::dimension(w, "highcrown.w")?;
        validate::dimension(d, "highcrown.d")?;
        Ok(Some([x, y, w, d]))
    }

    /// Preserve a footprint as a measurement reference without any solid fill.
    fn footprint(
        &mut self,
        raw: &PlanObject,
        out: &mut Object,
        bounds: [f64; 4],
        base: f64,
        layer: Option<String>,
    ) -> Result<(), Error> {
        let [x, y, w, d] = bounds;
        let geometry = if raw.shape.as_deref() == Some("round") {
            if w != d {
                return Err(Error::new(
                    ErrorKind::InvalidValue,
                    "highcrown.round",
                    "recorded circular bounds are not square",
                ));
            }
            Geometry::Circle {
                center: [x + w / 2.0, base, -(y + d / 2.0)],
                radius: w / 2.0,
            }
        } else {
            Geometry::Polyline {
                points: vec![
                    [x, base, -y],
                    [x + w, base, -y],
                    [x + w, base, -(y + d)],
                    [x, base, -(y + d)],
                ],
                closed: true,
            }
        };
        self.add(out, Primitive {
            id: format!("{}/footprint", out.id), layer, fidelity: Fidelity::ReferenceOutline,
            note: "Recorded footprint bounds at the declared floor. Special shapes remain bounds only; this outline does not prescribe a solid floor.".to_owned(),
            geometry,
        })?;
        Ok(())
    }

    /// Outer body dimensions are recorded bounds, not block-perfect walls.
    fn envelope(
        &mut self,
        raw: &PlanObject,
        out: &mut Object,
        bounds: [f64; 4],
        base: f64,
        layer: Option<String>,
        parent: Option<&Parent>,
    ) -> Result<(), Error> {
        let top = raw
            .abs_eaves
            .or(raw.abs_z1)
            .or(raw.height)
            .or_else(|| parent.and_then(|p| p.base.zip(raw.z1).map(|(b, offset)| b + offset)));
        let Some(top) = top else {
            out.omissions
                .push("No declared envelope top; no body height was invented.".to_owned());
            return Ok(());
        };
        validate::scalar(top, "highcrown.envelope_top")?;
        if top < base {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                "highcrown.envelope_top",
                "top is below the explicit base",
            ));
        }
        if top == base {
            out.omissions.push(
                "The recorded top equals the base; only a reference footprint is available."
                    .to_owned(),
            );
            return Ok(());
        }
        let [x, y, w, d] = bounds;
        let geometry = match raw.shape.as_deref() {
            Some("round") => Geometry::Cylinder {
                base: [x + w / 2.0, base, -(y + d / 2.0)],
                radius: w / 2.0,
                height: top - base,
            },
            None | Some("rectangle" | "square" | "rect") => Geometry::Box {
                min: [x, base, -(y + d)],
                size: [w, top - base, d],
            },
            _ => {
                out.omissions.push("A specialized source footprint has no supported solid primitive; only its recorded bounds are shown.".to_owned());
                return Ok(());
            }
        };
        self.add(out, Primitive {
            id: format!("{}/envelope", out.id), layer, fidelity: Fidelity::RecordedEnvelope,
            note: "Recorded outer bounds before doors, windows, circulation or other openings. This is an envelope, not a solid building or block assembly.".to_owned(),
            geometry,
        })?;
        Ok(())
    }

    /// Preserve recorded centrelines and absolute wall tops.
    fn wall(
        &mut self,
        raw: &PlanObject,
        out: &mut Object,
        points: &[[f64; 2]],
        base: f64,
        layer: Option<String>,
    ) -> Result<(), Error> {
        validate::count(points.len(), 4096, "highcrown.wall.points")?;
        let points: Vec<[f64; 3]> = points.iter().map(|[x, y]| [*x, base, -*y]).collect();
        let absolute_top = raw.wall_type.as_deref() == Some("curtain")
            || raw.expansion_role.as_deref() == Some("wall");
        let geometry = if let (true, Some(thickness), Some(top)) =
            (absolute_top, raw.thickness, raw.height_total)
        {
            validate::scalar(top, "highcrown.wall_top")?;
            validate::dimension(top - base, "highcrown.wall_height")?;
            Geometry::WallPath {
                points,
                thickness,
                height: top - base,
            }
        } else {
            out.omissions.push("Wall thickness/absolute top is absent, or legacy parapet/revetment height semantics are mixed. Only the centreline is exported; no wall volume was invented.".to_owned());
            Geometry::Polyline {
                points,
                closed: false,
            }
        };
        let fidelity = if matches!(geometry, Geometry::WallPath { .. }) {
            Fidelity::RecordedEnvelope
        } else {
            Fidelity::ReferenceOutline
        };
        self.add(out, Primitive {
            id: format!("{}/wall-path", out.id), layer, fidelity,
            note: "Source polyline is the wall centreline. height_total is an absolute plan top, as in the guide. Segment joins and openings remain an envelope.".to_owned(),
            geometry,
        })?;
        Ok(())
    }

    /// Emit only specified surfaces and the guide's declared one-small floor thickness.
    fn floors(
        &mut self,
        raw: &PlanObject,
        out: &mut Object,
        bounds: [f64; 4],
        base: f64,
    ) -> Result<(), Error> {
        let mut requests: BTreeMap<String, (f64, f64, bool)> = BTreeMap::new();
        for top in &raw.floor_levels {
            validate::scalar(*top, "highcrown.floor_levels")?;
            requests.insert(top.to_string(), (*top, self.floor_thickness, false));
        }
        for picture in self.pictures.get(&out.id).into_iter().flatten() {
            let top = picture.top.unwrap_or(base);
            validate::scalar(top, "highcrown.floor_surface")?;
            let key = top.to_string();
            if let Some((_, thickness, _)) = requests.get(&key) {
                if *thickness != picture.thickness {
                    return Err(Error::new(
                        ErrorKind::InvalidValue,
                        "highcrown.floor",
                        "conflicting thicknesses for one source floor",
                    ));
                }
            }
            requests.insert(key, (top, picture.thickness, true));
        }
        if requests.is_empty() {
            return Ok(());
        }
        if let Some(regions) = &raw.floor_regions {
            let [x, y, w, d] = bounds;
            if regions.iter().any(|region| {
                region.x < x
                    || region.y < y
                    || region.x + region.w > x + w
                    || region.y + region.d > y + d
            }) {
                return Err(Error::new(
                    ErrorKind::InvalidValue,
                    "highcrown.floor_regions",
                    "declared regions must stay within the source floor footprint",
                ));
            }
        }
        let unsupported_masks: Vec<&str> = [
            ("floor_regions", raw.floor_regions.is_some()),
            ("holes", raw.holes.is_some()),
            ("voids", raw.voids.is_some()),
        ]
        .into_iter()
        .filter_map(|(field, present)| present.then_some(field))
        .collect();
        if !unsupported_masks.is_empty() {
            out.omissions.push(format!("Floor layer omitted: source {} requires a positive/negative mask adapter; its geometry is not replaced by a filled rectangle.", unsupported_masks.join(", ")));
            return Ok(());
        }
        if matches!(raw.kind.as_deref(), Some("hollow" | "water" | "reference")) {
            out.omissions.push("Floor layer omitted: the source kind is hollow, water or reference; its bounds are not a solid floor mask.".to_owned());
            return Ok(());
        }
        if !matches!(
            raw.shape.as_deref(),
            None | Some("rectangle" | "square" | "rect")
        ) || raw.floor_pattern.is_some()
            || raw.cutouts.is_some()
        {
            out.omissions.push("Nonrectangular or patterned floors require their source mask adapter; no solid slab replaces a circle, gap pattern or unsupported cutout.".to_owned());
            return Ok(());
        }
        let [x, y, w, d] = bounds;
        for (key, (top, thickness, explicit)) in requests {
            let applicable_openings: Vec<&PlanOpening> = raw
                .floor_openings
                .iter()
                .filter(|opening| {
                    opening
                        .at_floors
                        .as_ref()
                        .is_none_or(|floors| floors.contains(&top))
                })
                .collect();
            if applicable_openings.iter().any(|opening| {
                !matches!(opening.shape.as_deref(), None | Some("rect" | "rectangle"))
                    || opening.clip == Some(true)
            }) {
                out.omissions.push(format!("Floor at {top} m omitted: a nonrectangular opening shape or requested clipping needs the source mask adapter. It is not replaced by rectangular cutouts; only reference bounds remain."));
                continue;
            }
            let layer_id = format!("floor/{}/{key}", out.id);
            if self.layers.contains_key(&layer_id) {
                return Err(Error::new(
                    ErrorKind::DuplicateId,
                    "highcrown.layers",
                    "generated floor ID collides with a declared source layer",
                ));
            }
            validate::count(
                self.layers.len().saturating_add(1),
                10_000,
                "highcrown.layers",
            )?;
            self.layers.insert(
                layer_id.clone(),
                Layer {
                    id: layer_id.clone(),
                    name: format!("{} floor at {top} m", raw.name),
                    elevation: top,
                },
            );
            let openings = applicable_openings
                .into_iter()
                .map(|opening| Cutout {
                    min: [opening.x, -(opening.y + opening.d)],
                    size: [opening.w, opening.d],
                })
                .collect();
            let exact = raw.kind.as_deref() == Some("floor")
                || explicit && !out.source_ref.starts_with("buildings/");
            self.add(out, Primitive {
                id: format!("{}/floor/{key}", out.id), layer: Some(layer_id),
                fidelity: if exact { Fidelity::ExactLayer } else { Fidelity::RecordedEnvelope },
                note: "Recorded rectangular floor surface and applicable openings. Slab extends downward by the drawing's layer thickness, or the guide's declared one-small (0.5 m) thin-floor rule. Other drawing systems and unpositioned openings are outside this mask.".to_owned(),
                geometry: Geometry::Slab { min: [x, top - thickness, -(y + d)], size: [w, thickness, d], openings },
            })?;
        }
        Ok(())
    }

    /// Preserve the local roof cross-section without guessing hips, overhangs or depth.
    fn roof(
        &mut self,
        raw: &PlanObject,
        out: &mut Object,
        bounds: [f64; 4],
        layer: Option<String>,
    ) -> Result<(), Error> {
        let Some(roof) = &raw.roofgeo else {
            return Ok(());
        };
        validate::count(roof.prof.len(), 4096, "highcrown.roofgeo.prof")?;
        let Some(eaves) = raw.abs_eaves.or(raw.abs_z1) else {
            out.omissions.push("Roof profile has no explicit absolute eaves/base; it was not positioned by guesswork.".to_owned());
            return Ok(());
        };
        let [x, y, _, _] = bounds;
        let mut points = Vec::new();
        for vertex in &roof.prof {
            let run = vertex.first().and_then(serde_json::Value::as_f64);
            let rise = vertex.get(1).and_then(serde_json::Value::as_f64);
            let (Some(run), Some(rise)) = (run, rise) else {
                return Err(Error::new(
                    ErrorKind::InvalidValue,
                    "highcrown.roofgeo.prof",
                    "numeric run and rise required",
                ));
            };
            let point = match roof.axis.as_str() {
                "x" => [x, eaves + rise, -(y + run)],
                "y" => [x + run, eaves + rise, -y],
                "NS" => [x + run, rise, -y],
                "EW" => [x, rise, -(y + run)],
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidValue,
                        "highcrown.roofgeo.axis",
                        "explicit x/y local or NS/EW measured source ridge axis required",
                    ))
                }
            };
            points.push(point);
        }
        self.add(out, Primitive {
            id: format!("{}/roof-profile", out.id), layer, fidelity: Fidelity::ReferenceOutline,
            note: "Recorded roofgeo.prof cross-section at the southwest edge. x/y axes use eaves-relative heights; the measured NS/EW convention records absolute plan heights. It is not extruded into a guessed hip, complete roof, overhang, thickness or voxel recipe.".to_owned(),
            geometry: Geometry::Polyline { points, closed: false },
        })?;
        Ok(())
    }
}
