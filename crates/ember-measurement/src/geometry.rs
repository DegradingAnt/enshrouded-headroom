//! Canonical geometry and explicitly distinct span/cell semantics.
use crate::{context::validate_frame, Error};
use ember_blueprint::{Frame, Geometry};

/// A finite position in the W38 canonical metre frame. No engine conversion is implied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point([f64; 3]);

impl Point {
    /// Accept canonical metres within +/-1,000,000 on each axis.
    pub fn new(coordinates: [f64; 3]) -> Result<Self, Error> {
        if coordinates
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 1_000_000.0)
        {
            return Err(Error::InvalidPoint);
        }
        Ok(Self(coordinates.map(|value| {
            if value == 0.0 {
                0.0
            } else {
                value
            }
        })))
    }

    /// Read X east, Y up and Z south in metres.
    pub fn coordinates(self) -> [f64; 3] {
        self.0
    }
}

/// Face-to-face lengths on one axis, without an inclusive endpoint adjustment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisSpan {
    /// Nonnegative face-to-face metres.
    pub metres: f64,
    /// Length divided by 0.5 m; fractional spans remain fractional.
    pub small_spans: f64,
    /// X/Z use 4 m; Y uses 2 m. These are spans, not item counts.
    pub large_spans: f64,
}

/// A diagonal has no orientation-independent large-building-piece unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diagonal {
    /// Euclidean length in metres.
    pub metres: f64,
    /// Euclidean length in 0.5 m spans.
    pub small_spans: f64,
}

/// One immutable calculation and its world-aligned reference outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    /// First explicitly supplied point.
    a: Point,
    /// Second explicitly supplied point.
    b: Point,
    /// Signed B minus A in canonical axes.
    signed: [f64; 3],
    /// X/Y/Z nonnegative spans.
    axes: [AxisSpan; 3],
    /// Three-dimensional diagonal.
    diagonal: Diagonal,
    /// At most twelve unique nonzero reference lines.
    outline: Vec<Geometry>,
}

impl Measurement {
    /// Read the first point.
    pub fn a(&self) -> Point {
        self.a
    }
    /// Read the second point.
    pub fn b(&self) -> Point {
        self.b
    }
    /// Read signed X/Y/Z direction and displacement.
    pub fn signed_delta(&self) -> [f64; 3] {
        self.signed
    }
    /// Read X/Y/Z face spans in axis-specific units.
    pub fn axes(&self) -> [AxisSpan; 3] {
        self.axes
    }
    /// Read Euclidean metres and small spans.
    pub fn diagonal(&self) -> Diagonal {
        self.diagonal
    }
    /// Read W38 line geometry. It does not imply a current native render binding.
    pub fn outline(&self) -> &[Geometry] {
        &self.outline
    }
}

/// Calculate bounded canonical geometry without reading any runtime state.
pub fn measure(frame: &Frame, a: Point, b: Point) -> Result<Measurement, Error> {
    validate_frame(frame)?;
    let [ax, ay, az] = a.coordinates();
    let [bx, by, bz] = b.coordinates();
    // Finite +/-1e6 inputs bound every subtraction by 2e6.
    let signed = [bx - ax, by - ay, bz - az];
    let [dx, dy, dz] = signed.map(f64::abs);
    let span = |metres: f64, large: f64| AxisSpan {
        metres,
        small_spans: metres / 0.5,
        large_spans: metres / large,
    };
    let distance = dx.hypot(dy).hypot(dz);
    let [x0, y0, z0] = [ax.min(bx), ay.min(by), az.min(bz)];
    let [x1, y1, z1] = [ax.max(bx), ay.max(by), az.max(bz)];
    let mut outline = Vec::with_capacity(12);
    for y in [y0, y1] {
        for z in [z0, z1] {
            line(&mut outline, [x0, y, z], [x1, y, z]);
        }
    }
    for x in [x0, x1] {
        for z in [z0, z1] {
            line(&mut outline, [x, y0, z], [x, y1, z]);
        }
    }
    for x in [x0, x1] {
        for y in [y0, y1] {
            line(&mut outline, [x, y, z0], [x, y, z1]);
        }
    }
    Ok(Measurement {
        a,
        b,
        signed,
        axes: [span(dx, 4.0), span(dy, 2.0), span(dz, 4.0)],
        diagonal: Diagonal {
            metres: distance,
            small_spans: distance / 0.5,
        },
        outline,
    })
}

/// Skip degenerate/coincident edges instead of inventing a positive box thickness.
fn line(outline: &mut Vec<Geometry>, a: [f64; 3], b: [f64; 3]) {
    if a == b {
        return;
    }
    let geometry = Geometry::Polyline {
        points: vec![a, b],
        closed: false,
    };
    if !outline.contains(&geometry) {
        outline.push(geometry);
    }
}

/// Explicit integer cell addresses, not face coordinates or snapped grid lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellAddress {
    /// X cell index.
    pub x: i64,
    /// Y cell index.
    pub y: i64,
    /// Z cell index.
    pub z: i64,
}

/// Inclusive counts per axis. No inventory or volume is inferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellCounts {
    /// Inclusive X cells.
    pub x: u64,
    /// Inclusive Y cells.
    pub y: u64,
    /// Inclusive Z cells.
    pub z: u64,
}

/// Count explicitly identified cells. Equal addresses count as one cell.
pub fn inclusive_cells(a: CellAddress, b: CellAddress) -> Result<CellCounts, Error> {
    let count = |a: i64, b: i64| a.abs_diff(b).checked_add(1).ok_or(Error::Overflow);
    Ok(CellCounts {
        x: count(a.x, b.x)?,
        y: count(a.y, b.y)?,
        z: count(a.z, b.z)?,
    })
}
