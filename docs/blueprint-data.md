# Blueprint data and source refresh (W38)

Ember Blueprint is a data foundation for an interactive world hologram. It
converts actual guide structure data into a common, validated model. It does not
implement an in-game overlay. Camera and engine coordinate verification,
rendering, PlayStation controls and vanilla-style UI remain separate work. No
crafted item is introduced.

The [crate](../crates/ember-blueprint/README.md) has no dependency on Headroom's
settings, entry point or native hooks. The converter never modifies the original
guide, checklist, site, save or server. It does not restore the retired website 3D
view.

## Common interchange

The [JSON Schema 1.0](../crates/ember-blueprint/schema/blueprint-v1.schema.json)
defines an open JSON interchange. The [watchtower example](../crates/ember-blueprint/examples/watchtower.blueprint.json)
is an independent guide authored directly in it. Another builder can emit this
format from any tool, then run the CLI validator or use the Rust parser.

A source adapter can instead construct the public Blueprint types and call
validate. The [cabin adapter](../crates/ember-blueprint/examples/cabin_adapter.rs)
demonstrates an unrelated guide format with explicitly declared centimetres. It
converts units and axes, hashes the original source snapshot, preserves IDs and
passes through the same validator. No library changes are needed for that
adapter. Its small [source file](../crates/ember-blueprint/examples/cabin-source.json)
is included.

Every document declares:

- schema_version, currently exactly 1.0. Unknown versions and fields are rejected.
- A stable source ID and adapter interpretation ID.
- A positive monotonic source sequence and SHA-256 of the exact upstream bytes.
- Canonical metres, axes and a written source-to-canonical mapping.
- Stable object, primitive, stage, layer and local anchor IDs.
- Per-primitive fidelity and per-object omission notes.

IDs are identifiers, not filenames or fetch addresses. Consumers must never join
an ID to a filesystem path or interpret labels as executable markup. Array
positions and display order are not identities. Objects reference declared
stages; primitives reference declared layers. Stages can retain unresolved
upstream associations explicitly.

The producer owns the increasing sequence and should persist its last accepted
revision. content_version is informational: an unchanged string such as 3.0 does
not imply unchanged content. content_sha256 hashes the upstream source bytes,
including whitespace. It is not a self-hash of the exported document and is not
an authenticity signature. The direct example uses the SHA-256 of the literal
upstream bytes abc as an illustrative source revision.

## Units, axes and geometry

Canonical coordinates are right handed:

| Axis | Meaning |
| --- | --- |
| X | East |
| Y | Up |
| Z | South |

All positions and dimensions are metres. Extents are positive. Rotation and
consumer placement are separate from source geometry. No engine frame or current
save position is encoded in the generic format.

Supported primitives are axis-aligned boxes, horizontal slabs with rectangular
through-openings, ordered polylines, centreline wall envelopes, vertical
cylinders and horizontal circles. A slab's minimum Y is its underside; its
walking surface is minimum Y plus size Y. Its opening coordinates are absolute
canonical X/Z, rather than an ambiguous local offset. Multiple openings form a
union, so overlaps do not imply duplicate floor placement.

Fidelity describes one primitive:

| Label | Meaning |
| --- | --- |
| exact_layer | A specified geometric layer mask, with all recorded cutouts |
| recorded_envelope | Recorded outer dimensions before unmodelled details |
| approximate_massing | An adapter explicitly declares an approximate mass |
| reference_outline | A measurement boundary or profile without solid placement |

Exact layer does not promise a game-tested build, material total or particular
native block palette. The format trusts an adapter's fidelity declaration;
validation cannot prove correspondence to terrain or game assets. Highcrown's
adapter never uses arbitrary fallback dimensions or approximate massing.

## Highcrown 3.0 adapter

The adapter reads the actual plan's zones, walls, towers, buildings, features,
guide measurements, explicit parts, phase associations and supported floor
drawing configurations. Source references preserve collection and stable IDs.
The [fixture](../crates/ember-blueprint/tests/fixtures/highcrown-recorded.json)
copies representative real geometry while stripping unrelated prose and world
positions. Its [export](../crates/ember-blueprint/examples/highcrown-recorded.blueprint.json)
is an inspectable interchange artifact.

Highcrown's source X/Y are east/north; height is separate. The mapping is
canonical (X,Y,Z) = source (x,height,-y). The original plan origin remains
(0,0,0). The altar supporting floor centre is a guide-local anchor, independent
of the saved-world coordinates in the guide. Source world positions and
uncalibrated engine hints are not exported.

The implementation follows the measured guide contracts:

- abs_floor and declared level elevations identify floor surfaces. Deck tops
  and explicit parent-relative part offsets keep their stated meanings.
- abs_eaves, absolute part tops and tower heights define envelope tops.
- Curtain height_total and expansion wall tops are absolute plan heights.
  Legacy parapet and revetment records have mixed meanings. Their centreline
  remains a reference with an omission note; the adapter does not guess a volume.
- Roof profiles with x/y ridge axes use eaves-relative heights. The measured
  NS/EW profiles use absolute plan heights, as recorded by the guide's construction
  source. Only the recorded cross-section is exported. It is not extruded into
  a guessed complete hip, overhang, roof thickness or voxel recipe.
- Explicit floor_levels and floor drawing requests produce supported
  rectangular layers. The drawing's thickness applies; otherwise the guide's
  documented one-small thin-floor rule uses its declared 0.5 m grid. Applicable
  at_floors openings stay empty. Building floor envelopes without complete
  opening masks retain envelope fidelity.
- Source labels may be empty for diagram elements. Those objects keep their
  existing stable ID as the display label. No geometry is invented for a label.

The adapter does not infer geometry from prose, HTML, object-name prefixes,
storey counts or the retired website renderer. Stage membership uses explicit
measure_ids, placement_ids, host_ids and drawing/section references. It is a
coarse source association, not a complete per-block build sequence. Objects with
no explicit association remain unassigned.

Detailed room/floor-plan systems, rounded voxel masks, special footprints,
stairs, openings in wall envelopes, furnishings and individual block placements
are not yet adapted. Bounds and profiles retain reference fidelity, and each
object reports the omitted scope. A complete source import is not a
block-perfect export.

## Complete model updates

Session publishes one immutable Arc snapshot containing geometry, filters,
anchor state, placement and freshness. Parse and validation finish before that
pointer is replaced. An older Arc remains a coherent older snapshot; consumers
should obtain the current Arc on their own update cadence. The session has one
serialized writer. It does not synchronize a renderer's private stale cache.

A worker can call poll_highcrown on its configured raw plan file. The function:

1. Reads one bounded byte snapshot and hashes those bytes.
2. Returns Unchanged for the exact accepted bytes.
3. Converts changed bytes, even when plan.version is still 3.0.
4. Validates the entire candidate and assigns the next local sequence.
5. Reconciles state and publishes the complete snapshot in one replacement.

This is a one-shot polling boundary, not an installed real-time watcher. Native
integration must schedule it, define its cadence and persist the last snapshot
when continuity across restarts is needed. No work belongs in a render/input
hook. A local file reverting to earlier content is a new observed edit; raw
Highcrown files do not carry their own monotonic event sequence.

For other sources, poll_interchange reads producer-versioned files. apply also
accepts a prepared ValidatedBlueprint with an expected base revision. It rejects
older sequences, changed content reusing one sequence, a higher sequence reusing
the same source hash, mismatched source/adapter/frame and a raced expected base.
Changed frame or adapter interpretation requires an explicit new session and
alignment decision.

On malformed JSON, partial writes, invalid geometry, read failure or a rejected
revision, geometry and selection stay unchanged and freshness becomes Rejected
with an inspectable error. The consumer must visibly label or hide that retained
geometry. It must not present the retained revision as current. A successful
complete identical source read can clear a transient failure.

Object, stage and floor selections survive through stable IDs. If an ID is
deleted, it becomes Removed with the ID and deletion sequence; the library does
not silently choose a different target. Removed selections do not auto-reactivate
if an ID reappears. An explicit consumer selection is required.

Alignment retains a stable guide-local anchor ID and consumer-supplied target,
yaw, height adjustment and opacity. A changed local anchor position still maps
to the same chosen target. Deleting the anchor produces Removed and disables
the transform. Placement math uses a declared external copy of the canonical
frame; no game-world binding or camera calibration is implied.

## Validation and dependencies

The default boundary is 16 MiB, 10,000 objects, 100,000 primitives and 500,000
line vertices/openings. Stages, layers, anchors, text and individual geometry
arrays have additional fixed bounds. Coordinates must be finite within one
million metres, and dimensions are positive up to 100,000 metres. Resulting
extrema, slab cutout containment, duplicate IDs and missing references are
checked. JSON retains Serde's recursion bound. The adapter enforces output
budgets while constructing primitives, including explicit parts.

The JSON Schema handles structural checks. Rust validation additionally checks
cross references, uniqueness by ID, complete-document budgets, resulting
extrema, cutout containment and revision ordering. Passing a schema checker alone
does not produce a ValidatedBlueprint.

Production dependencies are pinned:

| Dependency | Reason |
| --- | --- |
| serde =1.0.229 | Strict typed JSON interchange and raw adapter records |
| serde_json =1.0.154 | JSON parsing and serialization |
| sha2 =0.10.9 | Hash the exact in-memory source snapshot on any supported platform |

The existing emberkit has Windows native infrastructure and a Windows file
hasher. This data library needs neither hooks nor file re-reads for hashing.
Keeping hashing in memory avoids a different-file-snapshot race and keeps the
library independent of a native loader.

Run focused checks from the repository root:

~~~text
cargo fmt -p ember-blueprint --check
cargo clippy -p ember-blueprint --all-targets --all-features -- -D warnings
cargo test -p ember-blueprint --all-targets
cargo test -p ember-blueprint --doc
cargo doc -p ember-blueprint --no-deps
uv run --no-project --with jsonschema==4.25.1 crates/ember-blueprint/tests/verify_schema.py
~~~

The last command uses a development-only independent Draft 2020-12 validator;
it is not a production or Cargo dependency. Additional converted artifacts can
be passed as script arguments.

Tests cover import/round-trip, schema/version/units, nonfinite values, bounds,
resource limits, stable IDs, authentic floor holes, wall and roof conventions,
independent guide conversion, atomic replacement, invalid/partial/stale/mismatched
updates, deleted selections/anchors, unchanged version strings and real bounded
file polling. These prove the data/converter contract only. Native rendering,
input and in-game behavior still require their own integration and validation.
