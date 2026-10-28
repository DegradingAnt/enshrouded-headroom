//! Complete immutable snapshots and an explicit, bounded source polling boundary.
use crate::{
    convert_highcrown, parse, read_bounded, validate, Error, ErrorKind, Limits, Revision,
    ValidatedBlueprint,
};
use std::{path::Path, sync::Arc};

/// A selected ID remains visibly removed until the consumer explicitly selects again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdState {
    /// No selection has been made.
    None,
    /// This ID exists in the current complete model.
    Active(String),
    /// The selected ID was deleted by a valid replacement.
    Removed {
        /// The original ID, retained for UI feedback.
        id: String,
        /// Revision in which it first disappeared.
        sequence: u64,
    },
}

impl IdState {
    /// Reconcile an active selection without choosing a replacement implicitly.
    fn reconcile(&mut self, exists: impl FnOnce(&str) -> bool, sequence: u64) {
        if let Self::Active(id) = self {
            if !exists(id) {
                *self = Self::Removed {
                    id: id.clone(),
                    sequence,
                };
            }
        }
    }
}

/// Independent viewer filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionKind {
    /// A structure or object.
    Object,
    /// A construction stage.
    Stage,
    /// A floor or elevation.
    Layer,
}

/// A source failure must be visible even while the last valid geometry is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// The last complete source read validated and matched or replaced this model.
    Current,
    /// The last source attempt failed. A consumer must label or hide retained geometry.
    Rejected(Error),
}

/// Result of a complete successful source refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    /// A new revision replaced the complete model and reconciled viewer state.
    Replaced,
    /// The source is byte-identical to the accepted revision.
    Unchanged,
}

/// Consumer-owned alignment in a declared external copy of the canonical frame.
///
/// This is local math, not a claim about Enshrouded world or camera coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// Target position of the selected guide anchor in consumer-supplied metres.
    pub target_metres: [f64; 3],
    /// Right-handed rotation around canonical Y, from -360 through +360 degrees.
    pub yaw_degrees: f64,
    /// Additional vertical adjustment in metres.
    pub height_offset_metres: f64,
    /// Consumer display opacity from 0 through 1.
    pub opacity: f64,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            target_metres: [0.0; 3],
            yaw_degrees: 0.0,
            height_offset_metres: 0.0,
            opacity: 0.5,
        }
    }
}

impl Placement {
    /// Validate consumer values before accepting alignment state.
    fn validate(&self) -> Result<(), Error> {
        validate::point(&self.target_metres, "placement.target_metres")?;
        validate::scalar(self.height_offset_metres, "placement.height_offset_metres")?;
        if !self.yaw_degrees.is_finite() || self.yaw_degrees.abs() > 360.0 {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                "placement.yaw_degrees",
                "finite yaw from -360 to 360 required",
            ));
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                "placement.opacity",
                "finite opacity from zero to one required",
            ));
        }
        Ok(())
    }
}

/// One immutable view of geometry, selection, alignment and source freshness.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Geometry is shared with older snapshots without mutation.
    model: Arc<ValidatedBlueprint>,
    /// Independent object selection.
    object: IdState,
    /// Independent stage selection.
    stage: IdState,
    /// Independent floor selection.
    layer: IdState,
    /// Guide-local anchor identity.
    anchor: IdState,
    /// Consumer-owned placement values.
    placement: Placement,
    /// Explicit success or failure of the last source attempt.
    freshness: Freshness,
}

impl Snapshot {
    /// Read a complete validated model.
    pub fn model(&self) -> &ValidatedBlueprint {
        &self.model
    }

    /// Read one independent viewer filter.
    pub fn selection(&self, kind: SelectionKind) -> &IdState {
        match kind {
            SelectionKind::Object => &self.object,
            SelectionKind::Stage => &self.stage,
            SelectionKind::Layer => &self.layer,
        }
    }

    /// Read the alignment anchor state, including a deleted anchor.
    pub fn anchor(&self) -> &IdState {
        &self.anchor
    }

    /// Read retained consumer placement and opacity.
    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    /// Read whether retained geometry is current or rejected as stale.
    pub fn freshness(&self) -> &Freshness {
        &self.freshness
    }

    /// Transform a point by keeping the selected local anchor at the chosen target.
    ///
    /// A deleted anchor disables alignment. A local anchor that moves in a new
    /// revision still maps to the same consumer target.
    pub fn aligned_point(&self, point: [f64; 3]) -> Result<[f64; 3], Error> {
        validate::point(&point, "alignment.point")?;
        let IdState::Active(id) = &self.anchor else {
            return Err(Error::new(
                ErrorKind::NoAlignment,
                "alignment.anchor",
                "select an existing anchor",
            ));
        };
        let anchor = self
            .model
            .document()
            .anchors
            .iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NoAlignment,
                    "alignment.anchor",
                    "anchor is absent",
                )
            })?;
        let [x, y, z] = point;
        let [ax, ay, az] = anchor.point;
        let [tx, ty, tz] = self.placement.target_metres;
        let (sin, cos) = self.placement.yaw_degrees.to_radians().sin_cos();
        let result = [
            tx + (x - ax) * cos + (z - az) * sin,
            ty + (y - ay) + self.placement.height_offset_metres,
            tz - (x - ax) * sin + (z - az) * cos,
        ];
        validate::point(&result, "alignment.result")?;
        Ok(result)
    }
}

/// A serialized writer for immutable snapshots. The consumer schedules polling.
///
/// No file watcher, background thread or game hook is installed by this type.
/// A renderer can retain an Arc to an earlier complete snapshot while the writer
/// prepares the next one. Publication replaces a single Arc under exclusive access.
pub struct Session {
    /// The only published pointer.
    snapshot: Arc<Snapshot>,
}

impl Session {
    /// Start with one complete validated revision and no active selections.
    pub fn new(model: ValidatedBlueprint) -> Self {
        Self {
            snapshot: Arc::new(Snapshot {
                model: Arc::new(model),
                object: IdState::None,
                stage: IdState::None,
                layer: IdState::None,
                anchor: IdState::None,
                placement: Placement::default(),
                freshness: Freshness::Current,
            }),
        }
    }

    /// Obtain one immutable publication, including its freshness and viewer state.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshot)
    }

    /// Select a stable object, stage or floor ID.
    pub fn select(&mut self, kind: SelectionKind, id: &str) -> Result<(), Error> {
        let doc = self.snapshot.model.document();
        let found = match kind {
            SelectionKind::Object => doc.objects.iter().any(|o| o.id == id),
            SelectionKind::Stage => doc.stages.iter().any(|s| s.id == id),
            SelectionKind::Layer => doc.layers.iter().any(|l| l.id == id),
        };
        if !found {
            return Err(Error::new(
                ErrorKind::InvalidSelection,
                "selection",
                "ID is absent",
            ));
        }
        let mut next = (*self.snapshot).clone();
        let selected = IdState::Active(id.to_owned());
        match kind {
            SelectionKind::Object => next.object = selected,
            SelectionKind::Stage => next.stage = selected,
            SelectionKind::Layer => next.layer = selected,
        }
        self.snapshot = Arc::new(next);
        Ok(())
    }

    /// Clear one viewer filter explicitly.
    pub fn clear_selection(&mut self, kind: SelectionKind) {
        let mut next = (*self.snapshot).clone();
        match kind {
            SelectionKind::Object => next.object = IdState::None,
            SelectionKind::Stage => next.stage = IdState::None,
            SelectionKind::Layer => next.layer = IdState::None,
        }
        self.snapshot = Arc::new(next);
    }

    /// Align a guide-local anchor with consumer-supplied placement values.
    pub fn align(&mut self, anchor_id: &str, placement: Placement) -> Result<(), Error> {
        placement.validate()?;
        if !self
            .snapshot
            .model
            .document()
            .anchors
            .iter()
            .any(|a| a.id == anchor_id)
        {
            return Err(Error::new(
                ErrorKind::InvalidSelection,
                "alignment.anchor",
                "anchor ID is absent",
            ));
        }
        let mut next = (*self.snapshot).clone();
        next.anchor = IdState::Active(anchor_id.to_owned());
        next.placement = placement;
        self.snapshot = Arc::new(next);
        Ok(())
    }

    /// Apply a fully validated revision against the expected visible base.
    ///
    /// Source identity, adapter and frame must match. Reusing a sequence for
    /// different content or reusing a source hash at a higher sequence is rejected.
    pub fn apply(
        &mut self,
        expected: &Revision,
        candidate: ValidatedBlueprint,
    ) -> Result<RefreshOutcome, Error> {
        match self.check_update(expected, &candidate) {
            Ok(RefreshOutcome::Unchanged) => {
                self.mark_current();
                Ok(RefreshOutcome::Unchanged)
            }
            Ok(RefreshOutcome::Replaced) => {
                let mut next = (*self.snapshot).clone();
                let doc = candidate.document();
                let sequence = candidate.revision().sequence;
                next.object
                    .reconcile(|id| doc.objects.iter().any(|o| o.id == id), sequence);
                next.stage
                    .reconcile(|id| doc.stages.iter().any(|s| s.id == id), sequence);
                next.layer
                    .reconcile(|id| doc.layers.iter().any(|l| l.id == id), sequence);
                next.anchor
                    .reconcile(|id| doc.anchors.iter().any(|a| a.id == id), sequence);
                next.model = Arc::new(candidate);
                next.freshness = Freshness::Current;
                self.snapshot = Arc::new(next);
                Ok(RefreshOutcome::Replaced)
            }
            Err(error) => self.reject(error),
        }
    }

    /// Parse and validate a complete interchange update before replacing anything.
    pub fn refresh_json(
        &mut self,
        expected: &Revision,
        bytes: &[u8],
        limits: Limits,
    ) -> Result<RefreshOutcome, Error> {
        let candidate = match parse(bytes, limits) {
            Ok(candidate) => candidate,
            Err(error) => return self.reject(error),
        };
        self.apply(expected, candidate)
    }

    /// Poll one interchange file once. The file's producer owns its monotonic sequence.
    ///
    /// Read and parse failures retain geometry with Rejected freshness. Call this
    /// from a consumer worker at a chosen cadence, outside any render or input hook.
    pub fn poll_interchange(
        &mut self,
        path: &Path,
        limits: Limits,
    ) -> Result<RefreshOutcome, Error> {
        let expected = self.snapshot.model.revision().clone();
        let bytes = match read_bounded(path, limits) {
            Ok(bytes) => bytes,
            Err(error) => return self.reject(error),
        };
        self.refresh_json(&expected, &bytes, limits)
    }

    /// Poll the configured Highcrown source once and hash the complete raw snapshot.
    ///
    /// Changed raw bytes, even with the same plan version, receive the next local
    /// sequence. The consumer must persist its last snapshot across restarts if
    /// sequence continuity matters. This does not watch or mutate the source file.
    pub fn poll_highcrown(&mut self, path: &Path, limits: Limits) -> Result<RefreshOutcome, Error> {
        if self.snapshot.model.document().source.adapter != "highcrown-v1" {
            return self.reject(Error::new(
                ErrorKind::SourceMismatch,
                "source.adapter",
                "Highcrown polling needs a highcrown-v1 model",
            ));
        }
        let expected = self.snapshot.model.revision().clone();
        let bytes = match read_bounded(path, limits) {
            Ok(bytes) => bytes,
            Err(error) => return self.reject(error),
        };
        if crate::sha256(&bytes) == expected.content_sha256 {
            self.mark_current();
            return Ok(RefreshOutcome::Unchanged);
        }
        let sequence = match expected.sequence.checked_add(1) {
            Some(sequence) => sequence,
            None => {
                return self.reject(Error::new(
                    ErrorKind::Limit,
                    "source.sequence",
                    "revision sequence exhausted",
                ))
            }
        };
        let source_id = &self.snapshot.model.document().source.id;
        let candidate = match convert_highcrown(&bytes, source_id, sequence, limits) {
            Ok(candidate) => candidate,
            Err(error) => return self.reject(error),
        };
        self.apply(&expected, candidate)
    }

    /// Verify revision ordering and source compatibility without mutating a snapshot.
    fn check_update(
        &self,
        expected: &Revision,
        candidate: &ValidatedBlueprint,
    ) -> Result<RefreshOutcome, Error> {
        let current = &self.snapshot.model;
        if expected != current.revision() {
            return Err(Error::new(
                ErrorKind::RevisionConflict,
                "expected_revision",
                "visible revision changed while the source was prepared",
            ));
        }
        let a = &current.document().source;
        let b = &candidate.document().source;
        if a.id != b.id
            || a.adapter != b.adapter
            || current.document().frame != candidate.document().frame
        {
            return Err(Error::new(
                ErrorKind::SourceMismatch,
                "source",
                "source ID, adapter and frame must match",
            ));
        }
        if b.revision.sequence < a.revision.sequence {
            return Err(Error::new(
                ErrorKind::StaleRevision,
                "source.revision.sequence",
                "older than the visible revision",
            ));
        }
        if b.revision.sequence == a.revision.sequence {
            if candidate == current.as_ref() {
                return Ok(RefreshOutcome::Unchanged);
            }
            return Err(Error::new(
                ErrorKind::RevisionConflict,
                "source.revision",
                "one sequence cannot name two models",
            ));
        }
        if a.revision.content_sha256 == b.revision.content_sha256 {
            return Err(Error::new(
                ErrorKind::RevisionConflict,
                "source.revision.content_sha256",
                "unchanged source bytes cannot name a new model revision",
            ));
        }
        Ok(RefreshOutcome::Replaced)
    }

    /// Publish a source failure with the retained model and selection.
    fn reject<T>(&mut self, error: Error) -> Result<T, Error> {
        let mut next = (*self.snapshot).clone();
        next.freshness = Freshness::Rejected(error.clone());
        self.snapshot = Arc::new(next);
        Err(error)
    }

    /// A successful identical source read can clear a previous transient failure.
    fn mark_current(&mut self) {
        let mut next = (*self.snapshot).clone();
        next.freshness = Freshness::Current;
        self.snapshot = Arc::new(next);
    }
}
