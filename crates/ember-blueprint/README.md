# Ember Blueprint

A reusable data library and local converter for structure guides. It is the W38
foundation for a later world hologram consumer. It installs no game hooks, UI,
controller bindings, item, file watcher or background thread.

- [Interchange and refresh contract](../../docs/blueprint-data.md)
- [Open JSON Schema 1.0](schema/blueprint-v1.schema.json)
- [Generic watchtower document](examples/watchtower.blueprint.json)
- [Independent centimetre-based guide](examples/cabin-source.json) and
  [its adapter](examples/cabin_adapter.rs) and [export](examples/cabin.blueprint.json)
- [Actual Highcrown geometry excerpt](tests/fixtures/highcrown-recorded.json) and
  [converted document](examples/highcrown-recorded.blueprint.json)

From the repository root:

~~~text
cargo run -p ember-blueprint -- schema
cargo run -p ember-blueprint -- validate path/to/guide.blueprint.json
cargo run -p ember-blueprint -- convert-highcrown path/to/plan.json guide:stable-id 1
cargo run -p ember-blueprint --example cabin_adapter -- crates/ember-blueprint/examples/cabin-source.json 1
~~~

Conversion writes a complete document to stdout after validation. It never
rewrites the source. Consumers can poll the raw Highcrown source through
Session::poll_highcrown or a versioned interchange file through
Session::poll_interchange. A caller must schedule these bounded operations on a
worker and handle the published freshness state.

Highcrown phase `num` accepts a JSON u32 integer or a canonical ASCII decimal
string: `0`, or a nonzero value without leading zeros, up to `4294967295`.
This compatibility rule applies only to that source metadata field. Exported
`Stage.order` and all public schema/geometry numbers remain numeric.

A recorded envelope is a measurement aid. It is not a block-perfect assembly.
Unsupported details have explicit omission notes. The native camera, engine
coordinate binding, rendering, PlayStation input and vanilla-style UI remain
separate work.
