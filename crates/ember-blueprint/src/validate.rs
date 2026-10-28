//! Parse and validate bounded, complete snapshots before publication.
use crate::{Blueprint, Error, ErrorKind, Geometry, ValidatedBlueprint};
use std::{collections::BTreeSet, io::Read, path::Path};

/// Resource limits for a source boundary. JSON nesting retains Serde's depth bound.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum bytes read or parsed. Default 16 MiB.
    pub max_input_bytes: usize,
    /// Maximum selectable structures. Default 10,000.
    pub max_objects: usize,
    /// Maximum primitives across the complete model. Default 100,000.
    pub max_primitives: usize,
    /// Maximum line vertices and slab openings across the model. Default 500,000.
    pub max_points: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_objects: 10_000,
            max_primitives: 100_000,
            max_points: 500_000,
        }
    }
}

/// Parse a complete interchange document and validate all semantics.
pub fn parse(bytes: &[u8], limits: Limits) -> Result<ValidatedBlueprint, Error> {
    input_size(bytes, limits)?;
    let document: Blueprint = serde_json::from_slice(bytes)
        .map_err(|e| Error::new(ErrorKind::InvalidJson, "document", e.to_string()))?;
    document.validate(limits)
}

/// Read at most the input budget plus one byte. Metadata length is not trusted.
pub fn read_bounded(path: &Path, limits: Limits) -> Result<Vec<u8>, Error> {
    let budget = u64::try_from(limits.max_input_bytes).map_err(|_| {
        Error::new(
            ErrorKind::Limit,
            "source",
            "input budget is not representable",
        )
    })?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(budget.saturating_add(1))
        .read_to_end(&mut bytes)?;
    input_size(&bytes, limits)?;
    Ok(bytes)
}

/// Check byte length before the JSON parser allocates a model.
pub(crate) fn input_size(bytes: &[u8], limits: Limits) -> Result<(), Error> {
    count(bytes.len(), limits.max_input_bytes, "source bytes")
}

/// Check a count against its complete-document budget.
pub(crate) fn count(value: usize, maximum: usize, path: &str) -> Result<(), Error> {
    if value > maximum {
        return Err(Error::new(
            ErrorKind::Limit,
            path,
            format!("{value} exceeds {maximum}"),
        ));
    }
    Ok(())
}

/// Validate a text field without leaking source text into errors.
pub(crate) fn text(value: &str, path: &str, maximum: usize) -> Result<(), Error> {
    if value.is_empty() || value.chars().count() > maximum || value.chars().any(char::is_control) {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            path,
            "nonempty bounded text without control characters required",
        ));
    }
    Ok(())
}

/// Stable IDs exclude whitespace and HTML. They are identifiers, never file paths.
pub(crate) fn id(value: &str, path: &str) -> Result<(), Error> {
    text(value, path, 128)?;
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_.:/@-".contains(&b))
    {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            path,
            "stable ASCII ID required",
        ));
    }
    Ok(())
}

/// Reject nonfinite values and coordinates too large for a consumer.
pub(crate) fn scalar(value: f64, path: &str) -> Result<(), Error> {
    if !value.is_finite() || value.abs() > 1_000_000.0 {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            path,
            "finite value within one million metres required",
        ));
    }
    Ok(())
}

/// Check a finite position.
pub(crate) fn point(value: &[f64; 3], path: &str) -> Result<(), Error> {
    for component in value {
        scalar(*component, path)?;
    }
    Ok(())
}

/// Positive dimensions have a tighter bound than absolute positions.
pub(crate) fn dimension(value: f64, path: &str) -> Result<(), Error> {
    scalar(value, path)?;
    if value <= 0.0 || value > 100_000.0 {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            path,
            "positive dimension up to 100000 metres required",
        ));
    }
    Ok(())
}

/// Validate a complete model, including cross references.
pub(crate) fn validate(doc: Blueprint, limits: Limits) -> Result<ValidatedBlueprint, Error> {
    if doc.schema_version != "1.0" {
        return Err(Error::new(
            ErrorKind::UnsupportedVersion,
            "schema_version",
            "expected 1.0",
        ));
    }
    text(&doc.title, "title", 256)?;
    id(&doc.source.id, "source.id")?;
    id(&doc.source.adapter, "source.adapter")?;
    text(&doc.source.content_version, "source.content_version", 128)?;
    if doc.source.revision.sequence == 0 || doc.source.revision.sequence > 9_007_199_254_740_991 {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            "source.revision.sequence",
            "positive JSON safe integer required",
        ));
    }
    let hash = &doc.source.revision.content_sha256;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            "source.revision.content_sha256",
            "64 lowercase SHA-256 digits required",
        ));
    }
    if doc.frame.units != "metres" || doc.frame.axes != "right_handed_x_east_y_up_z_south" {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            "frame",
            "explicit canonical metres and right handed X east / Y up / Z south required",
        ));
    }
    text(&doc.frame.source_mapping, "frame.source_mapping", 1024)?;
    count(doc.stages.len(), 2048, "stages")?;
    count(doc.layers.len(), 10_000, "layers")?;
    count(doc.anchors.len(), 1024, "anchors")?;
    count(doc.objects.len(), limits.max_objects, "objects")?;

    let mut stages = BTreeSet::new();
    for stage in &doc.stages {
        unique(&stage.id, &mut stages, "stage.id")?;
        text(&stage.name, "stage.name", 256)?;
        count(stage.unresolved.len(), 10_000, "stage.unresolved")?;
        for unresolved in &stage.unresolved {
            id(unresolved, "stage.unresolved")?;
        }
    }
    let mut layers = BTreeSet::new();
    for layer in &doc.layers {
        unique(&layer.id, &mut layers, "layer.id")?;
        text(&layer.name, "layer.name", 256)?;
        scalar(layer.elevation, "layer.elevation")?;
    }
    let mut anchors = BTreeSet::new();
    for anchor in &doc.anchors {
        unique(&anchor.id, &mut anchors, "anchor.id")?;
        text(&anchor.name, "anchor.name", 256)?;
        point(&anchor.point, "anchor.point")?;
    }
    let mut objects = BTreeSet::new();
    let mut primitives = BTreeSet::new();
    let mut primitive_count = 0_usize;
    let mut point_count = 0_usize;
    for object in &doc.objects {
        unique(&object.id, &mut objects, "object.id")?;
        text(&object.name, "object.name", 256)?;
        text(&object.source_ref, "object.source_ref", 512)?;
        count(object.stages.len(), 2048, "object.stages")?;
        let mut memberships = BTreeSet::new();
        for stage in &object.stages {
            unique(stage, &mut memberships, "object.stages")?;
            reference(stage, &stages, "object.stages")?;
        }
        count(object.omissions.len(), 128, "object.omissions")?;
        for omission in &object.omissions {
            text(omission, "object.omission", 2048)?;
        }
        if object.primitives.is_empty() && object.omissions.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                "object",
                "empty geometry needs an omission explanation",
            ));
        }
        primitive_count = primitive_count.saturating_add(object.primitives.len());
        count(primitive_count, limits.max_primitives, "primitives")?;
        for primitive in &object.primitives {
            unique(&primitive.id, &mut primitives, "primitive.id")?;
            text(&primitive.note, "primitive.note", 2048)?;
            if let Some(layer) = &primitive.layer {
                reference(layer, &layers, "primitive.layer")?;
            }
            point_count = point_count.saturating_add(geometry(&primitive.geometry)?);
            count(point_count, limits.max_points, "geometry points")?;
        }
    }
    Ok(ValidatedBlueprint(doc))
}

/// Insert an ID into one namespace.
fn unique<'a>(value: &'a str, ids: &mut BTreeSet<&'a str>, path: &str) -> Result<(), Error> {
    id(value, path)?;
    if !ids.insert(value) {
        return Err(Error::new(
            ErrorKind::DuplicateId,
            path,
            "duplicate stable ID",
        ));
    }
    Ok(())
}

/// Check a foreign key.
fn reference(value: &str, ids: &BTreeSet<&str>, path: &str) -> Result<(), Error> {
    if !ids.contains(value) {
        return Err(Error::new(
            ErrorKind::MissingReference,
            path,
            "referenced ID is absent",
        ));
    }
    Ok(())
}

/// Check positions, dimensions and resulting extrema.
fn box_bounds(min: &[f64; 3], size: &[f64; 3]) -> Result<(), Error> {
    point(min, "geometry.min")?;
    for (position, extent) in min.iter().zip(size) {
        dimension(*extent, "geometry.size")?;
        scalar(position + extent, "geometry.maximum")?;
        if position + extent <= *position {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                "geometry.size",
                "extent is below coordinate precision",
            ));
        }
    }
    Ok(())
}

/// Validate one geometry and return its point-budget cost.
pub(crate) fn geometry(value: &Geometry) -> Result<usize, Error> {
    match value {
        Geometry::Box { min, size } => box_bounds(min, size)?,
        Geometry::Slab {
            min,
            size,
            openings,
        } => {
            box_bounds(min, size)?;
            count(openings.len(), 4096, "slab.openings")?;
            let [x, _, z] = *min;
            let [width, _, depth] = *size;
            for cutout in openings {
                let [cx, cz] = cutout.min;
                let [cw, cd] = cutout.size;
                dimension(cw, "cutout.size")?;
                dimension(cd, "cutout.size")?;
                scalar(cx, "cutout.min")?;
                scalar(cz, "cutout.min")?;
                if cx < x || cz < z || cx + cw > x + width || cz + cd > z + depth {
                    return Err(Error::new(
                        ErrorKind::InvalidValue,
                        "cutout",
                        "opening must stay within the slab footprint",
                    ));
                }
            }
            return Ok(openings.len());
        }
        Geometry::Polyline { points, closed } => {
            points_valid(points, if *closed { 3 } else { 2 })?;
            return Ok(points.len());
        }
        Geometry::WallPath {
            points,
            thickness,
            height,
        } => {
            points_valid(points, 2)?;
            dimension(*thickness, "wall.thickness")?;
            dimension(*height, "wall.height")?;
            let base = points.first().map(|[_, y, _]| *y);
            for [x, y, z] in points {
                if Some(*y) != base {
                    return Err(Error::new(
                        ErrorKind::InvalidValue,
                        "wall.points",
                        "wall centreline must have one common base Y",
                    ));
                }
                scalar(y + height, "wall.top")?;
                for edge in [
                    x - thickness / 2.0,
                    x + thickness / 2.0,
                    z - thickness / 2.0,
                    z + thickness / 2.0,
                ] {
                    scalar(edge, "wall.edges")?;
                }
            }
            return Ok(points.len());
        }
        Geometry::Cylinder {
            base,
            radius,
            height,
        } => {
            circle_bounds(base, *radius)?;
            dimension(*height, "cylinder.height")?;
            let [_, y, _] = *base;
            scalar(y + height, "cylinder.top")?;
        }
        Geometry::Circle { center, radius } => circle_bounds(center, *radius)?,
    }
    Ok(0)
}

/// Check a bounded, nondegenerate ordered line.
fn points_valid(points: &[[f64; 3]], minimum: usize) -> Result<(), Error> {
    count(points.len(), 4096, "polyline.points")?;
    if points.len() < minimum {
        return Err(Error::new(
            ErrorKind::InvalidValue,
            "polyline.points",
            "too few vertices",
        ));
    }
    for vertex in points {
        point(vertex, "polyline.point")?;
    }
    for pair in points.windows(2) {
        if let [a, b] = pair {
            if a == b {
                return Err(Error::new(
                    ErrorKind::InvalidValue,
                    "polyline.points",
                    "consecutive duplicate vertices",
                ));
            }
        }
    }
    Ok(())
}

/// Check circle extrema rather than just its centre.
fn circle_bounds(center: &[f64; 3], radius: f64) -> Result<(), Error> {
    point(center, "circle.center")?;
    dimension(radius, "circle.radius")?;
    let [x, _, z] = *center;
    for edge in [x - radius, x + radius, z - radius, z + radius] {
        scalar(edge, "circle.edges")?;
    }
    Ok(())
}
