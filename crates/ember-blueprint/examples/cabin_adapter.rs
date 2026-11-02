//! Example adapter for an unrelated builder's explicitly centimetre-based data.
use ember_blueprint::{
    read_bounded, sha256, Anchor, Blueprint, Fidelity, Frame, Geometry, Layer, Limits, Object,
    Primitive, Revision, Source, ValidatedBlueprint,
};
use serde::Deserialize;
use std::{io::Write, path::Path};

/// A separate guide format; it does not need to adopt Highcrown's source structure.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CabinGuide {
    /// Persistent guide identity.
    id: String,
    /// Informational version.
    version: String,
    /// Explicit source unit.
    unit: String,
    /// Recorded building bounds.
    structures: Vec<Cabin>,
}

/// Explicit source dimensions in centimetres.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cabin {
    /// Persistent source ID.
    id: String,
    /// Display title.
    name: String,
    /// Southwest east coordinate.
    east: f64,
    /// Southwest north coordinate.
    north: f64,
    /// East extent.
    width: f64,
    /// North extent.
    depth: f64,
    /// Absolute base height.
    floor: f64,
    /// Absolute body top.
    wall_top: f64,
}

/// Convert declared centimetres and north/up axes into canonical metres and south/up axes.
fn convert(bytes: &[u8], sequence: u64) -> Result<ValidatedBlueprint, Box<dyn std::error::Error>> {
    if bytes.len() > Limits::default().max_input_bytes {
        return Err("source byte limit exceeded".into());
    }
    let source: CabinGuide = serde_json::from_slice(bytes)?;
    if source.unit != "centimetres" {
        return Err("this adapter requires declared centimetres".into());
    }
    if source.structures.len() > Limits::default().max_objects {
        return Err("source object limit exceeded".into());
    }
    let objects = source.structures.into_iter().map(|cabin| Object {
        primitives: vec![Primitive {
            id: format!("{}/envelope", cabin.id),
            layer: Some("ground".to_owned()),
            fidelity: Fidelity::RecordedEnvelope,
            note: "Source outer bounds converted from centimetres. Openings and block placement are unspecified.".to_owned(),
            geometry: Geometry::Box {
                min: [cabin.east / 100.0, cabin.floor / 100.0, -(cabin.north + cabin.depth) / 100.0],
                size: [cabin.width / 100.0, (cabin.wall_top - cabin.floor) / 100.0, cabin.depth / 100.0],
            },
        }],
        source_ref: format!("structures/{}", cabin.id),
        id: cabin.id, name: cabin.name, stages: Vec::new(),
        omissions: vec!["Doors, windows, roof and individual blocks are not recorded.".to_owned()],
    }).collect();
    Ok(Blueprint {
        schema_version: "1.0".to_owned(), title: "Independent cabin adapter".to_owned(),
        source: Source {
            id: source.id, adapter: "example-cabin-v1".to_owned(), content_version: source.version,
            revision: Revision { sequence, content_sha256: sha256(bytes) },
        },
        frame: Frame {
            units: "metres".to_owned(), axes: "right_handed_x_east_y_up_z_south".to_owned(),
            source_mapping: "Source is east/north/height in declared centimetres; canonical (X,Y,Z)=(east,height,-north)/100. Local origin (0,0,0), without engine/world mapping.".to_owned(),
        },
        stages: Vec::new(),
        layers: vec![Layer { id: "ground".to_owned(), name: "Reference ground plane".to_owned(), elevation: 0.0 }],
        anchors: vec![Anchor { id: "origin".to_owned(), name: "Guide-local origin".to_owned(), point: [0.0; 3] }],
        objects,
    }.validate(Limits::default())?)
}

/// Write one explicitly requested conversion to stdout, without source mutations.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    let [_, input, sequence] = args.as_slice() else {
        return Err("usage: cabin_adapter SOURCE_JSON SEQUENCE".into());
    };
    let sequence = sequence.to_str().ok_or("sequence must be UTF-8")?.parse()?;
    let bytes = read_bounded(Path::new(input), Limits::default())?;
    let model = convert(&bytes, sequence)?;
    std::io::stdout().lock().write_all(&model.to_json()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_guide_converts_explicit_centimetres_without_changing_the_library() {
        let model = convert(include_bytes!("cabin-source.json"), 7).unwrap();
        assert_eq!(
            model.document().objects[0].primitives[0].geometry,
            Geometry::Box {
                min: [1.0, 0.0, -6.0],
                size: [6.0, 3.0, 4.0]
            }
        );
        assert_eq!(model.revision().sequence, 7);
    }

    #[test]
    fn an_adapter_cannot_silently_assume_units_or_negative_dimensions() {
        let source = std::str::from_utf8(include_bytes!("cabin-source.json")).unwrap();
        assert!(convert(source.replace("centimetres", "feet").as_bytes(), 1).is_err());
        assert!(convert(
            source
                .replace("\"width\": 600", "\"width\": -600")
                .as_bytes(),
            1
        )
        .is_err());
    }
}
