//! Current-frame text and control availability for a measurement interface.
use crate::{AimStatus, InputContext, Session, Snap, View, ViewStatus};

/// Display units only. Selecting a unit never enables snapping.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnitDisplay {
    /// Canonical metres on every axis and the diagonal.
    #[default]
    Metres,
    /// Fractional 0.5 m spans, without inclusive cell counting.
    SmallGrid,
    /// Fractional 4 m X/Z and 2 m Y spans; diagonal stays in metres.
    LargeGrid,
}
impl UnitDisplay {
    /// Describe the unit and its axis meaning for the interface.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Metres => "Metres",
            Self::SmallGrid => "0.5 m spans",
            Self::LargeGrid => "X/Z: 4 m spans; Y: 2 m spans; diagonal: metres",
        }
    }
}

/// Suggested controls for one checked frame, never a native input grant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PanelControls {
    /// Current world and input permit an explicit Begin.
    pub begin: bool,
    /// A fresh candidate may be marked through Session::act.
    pub mark: bool,
    /// Current accepted pins or candidate may be cleared.
    pub clear: bool,
    /// The active measurement mode may be exited.
    pub exit: bool,
}

/// Owned interface text with no raw world context or point coordinates.
///
/// Rebuild through Session::panel for each interface frame. Discard it on any
/// later focus, menu or world change. Every intent must still use Session::act
/// with current time and input assertions; these flags are not permission leases.
#[derive(Debug, PartialEq)]
pub struct Panel {
    /// Availability from the checked session view.
    status: ViewStatus,
    /// Current candidate status, separate from accepted pins.
    aim_status: AimStatus,
    /// Requested display unit, independent of snap policy.
    units: UnitDisplay,
    /// At most four dimension lines; unavailable views have none.
    lines: Vec<String>,
    /// Suggested controls, not acquired native ownership.
    controls: PanelControls,
    /// A short state message suitable for the interface.
    message: &'static str,
    /// The next point action, including restarting a completed pair.
    mark_label: &'static str,
}
impl Panel {
    /// Read current measurement availability.
    pub fn status(&self) -> ViewStatus {
        self.status
    }
    /// Read current candidate availability.
    pub fn aim_status(&self) -> AimStatus {
        self.aim_status
    }
    /// Read the selected display unit.
    pub fn units(&self) -> UnitDisplay {
        self.units
    }
    /// Read the explicit unit and axis explanation.
    pub fn unit_hint(&self) -> &'static str {
        self.units.hint()
    }
    /// Read checked dimension labels. This is never an inventory count.
    pub fn dimension_lines(&self) -> &[String] {
        &self.lines
    }
    /// Read suggested controls for this frame only.
    pub fn controls(&self) -> PanelControls {
        self.controls
    }
    /// Read whether this is a preview, accepted pair or unavailable state.
    pub fn status_text(&self) -> &'static str {
        self.message
    }
    /// Read the action for the next accepted candidate.
    pub fn mark_label(&self) -> &'static str {
        self.mark_label
    }
    /// Prepare labels from a newly checked view, never a retained native sample.
    fn from_view(view: View, units: UnitDisplay) -> Self {
        let status = view.status();
        let aim_status = view.aim_status();
        let available = !matches!(status, ViewStatus::Unavailable(_));
        let mark = available && matches!(aim_status, AimStatus::Hit | AimStatus::PlayerPosition);
        let controls = PanelControls {
            // Inactive is reached only after the core checked current world/input.
            begin: status == ViewStatus::Unavailable(crate::Error::Inactive),
            mark,
            clear: available && (view.first().is_some() || mark),
            exit: available,
        };
        let message = match status {
            ViewStatus::Unavailable(crate::Error::Inactive) => "Ready to measure.",
            ViewStatus::Unavailable(crate::Error::FocusLost) => "Focus the game to measure.",
            ViewStatus::Unavailable(crate::Error::NativeMenuOpen | crate::Error::MenuUnknown) => {
                "Close other menus to measure."
            }
            ViewStatus::Unavailable(crate::Error::InputNotOwned) => {
                "Measurement controls unavailable."
            }
            ViewStatus::Unavailable(_) => "Measurement unavailable.",
            ViewStatus::AwaitingFirst => "Mark the first point.",
            ViewStatus::FirstPinned => "A marked. Select the second point.",
            ViewStatus::Preview => "Preview (B is not marked).",
            ViewStatus::Pinned => "A and B marked.",
        };
        let mark_label = match status {
            ViewStatus::FirstPinned | ViewStatus::Preview => "Mark B",
            ViewStatus::Pinned => "New A",
            _ => "Mark A",
        };
        let lines = dimension_lines(&view, units);
        Self {
            status,
            aim_status,
            units,
            lines,
            controls,
            message,
            mark_label,
        }
    }
}
impl Session {
    /// Recheck world, time, capture and candidate state before preparing UI text.
    ///
    /// This supplies no native sample or input ownership. A future adapter must
    /// obtain its current assertions from verified sources and must recheck them
    /// through Session::act before consuming a button or controller intent.
    pub fn panel(
        &mut self,
        now_ms: u64,
        input: InputContext,
        snap: Snap<'_>,
        units: UnitDisplay,
    ) -> Panel {
        Panel::from_view(self.view(now_ms, input, snap), units)
    }
}
/// Format bounded, nonnegative lengths without turning a tiny distance into zero.
fn amount(value: f64) -> String {
    if value == 0.0 {
        "0.000".to_owned()
    } else if value < 0.001 {
        "<0.001".to_owned()
    } else {
        format!("{value:.3}")
    }
}

/// Render only a checked measurement; unavailable views carry no measurement.
fn dimension_lines(view: &View, units: UnitDisplay) -> Vec<String> {
    let Some(measurement) = view.measurement() else {
        return Vec::new();
    };
    let mut lines = Vec::with_capacity(4);
    for (axis, span) in ["X", "Y", "Z"].into_iter().zip(measurement.axes()) {
        let (value, suffix) = match units {
            UnitDisplay::Metres => (span.metres, "m"),
            UnitDisplay::SmallGrid => (span.small_spans, "spans"),
            UnitDisplay::LargeGrid => (span.large_spans, "spans"),
        };
        lines.push(format!("{axis}: {} {suffix}", amount(value)));
    }
    let diagonal = measurement.diagonal();
    let (value, suffix) = match units {
        UnitDisplay::SmallGrid => (diagonal.small_spans, "spans"),
        // A diagonal has no orientation-independent large-piece span unit.
        UnitDisplay::Metres | UnitDisplay::LargeGrid => (diagonal.metres, "m"),
    };
    lines.push(format!("Diagonal: {} {suffix}", amount(value)));
    lines
}
