# Climbing balance and animation

The author wants reliable climbing that feels like part of Enshrouded. Hanging must cost stamina. Existing food, skills, equipment, movement speed and fall risk must still matter. This is a design and research record, not a claim that the unfinished native bindings work.

## Lessons from other games

| Primary source | Useful lesson | Choice for ember-climb |
|---|---|---|
| [Nintendo's Breath of the Wild adventure guide, tip 6](https://www.nintendo.com/jp/zelda/botw/guide/index.html) | Intermediate footholds, stamina food and a different route can make a climb possible. | Preserve Enshrouded's stamina progression. Rest on safe footing through its normal systems. This source does not establish a stationary hanging cost. |
| [Jusant's creative directors on the climbing system](https://dont-nod.com/en/scaling-new-heights-learn-more-about-jusants-innovative-climbing-system/) | Route choice, clear control and finite stamina make climbing readable. Its ropes and pitons reduce the cost of falling. | Keep the game's existing stick and Jump controls. Use visible native stamina pressure. New ropes, checkpoints and per-hand buttons are outside this change. |
| [Funcom's original Conan Exiles climbing update](https://www.conanexiles.com/blog/update-28-climbing-exploration-system/) | Stamina limits height; exhaustion ends a climb, while equipment affects endurance. | Keep meaningful endurance and native exhaustion. Do not import its armor penalties or new crafting recipes. The article describes its 2017 design, not current balance values. |

These are design references. No code, animations or assets from those games are included.

## Zelda-like movement direction

The author wants the freedom and predictable controls of Breath of the Wild climbing while keeping Enshrouded's balance. This means automatic grabs, continuous analogue movement over rock, smooth changes in orientation, clear exhaustion, and the normal Jump control to leave a wall.

[Nintendo's Eldin climbing advice](https://www.nintendo.com/ph/zelda/botw/discovery/eldin.html) explicitly says that remaining still on a cliff does not consume stamina in Breath of the Wild. The author separately requested stamina drain while clinging here. The candidate therefore retains its minimum hanging effort as a deliberate balance difference.

The implemented movement correction separates geometry from animation easing. A hit point and the fresh fitted normal define the physical plane. Distance, movement projection and the gap correction use that normal. The body and probe direction keep their eased normal. An accepted three-hit patch must not pull differently merely because one corner ray missed the same plane. Regression tests cover changing centroids, bends and overhangs at 120/60/20 Hz. This adds no rays, persistent state, speed increase or native collision bypass. It is a source/fixture result, not measured in-game smoothness.

The original wide, parallel patch can lose enough support before seeing the next face at a sharp perpendicular convex corner. The correction does not relax all movement to two hits or fabricate a hold. The pure corner policy now tests fresh directional support, full-turn rediscovery and actual-candidate revalidation with an explicit motion oracle. A separate velocity-only adapter is now connected to the runtime hooks for static vertical faces up to 90 degrees. It retains native movement admission and collision; it does not implement or impersonate the strict whole-capsule motion oracle. Source and analytic fixtures do not establish gameplay or the exact stock climbing capsule.

A deliberate upward climbing hop is a separate follow-up. The native game currently supplies a detached push-off and a 375 ms re-grab block. Its input, accepted impulse and stamina ownership are separate. Charging at a convenient Jump hook could bill a refused request or repeat on replay. Preserve native push-off until a proven accepted-action cost transaction is available. Automatic natural top-outs and surface IK retain their existing gates.
## Stamina rule in the test candidate

The owned executable's climbing program charges for distance moved. At zero movement it skips both the debit and the exhaustion check. Raising the existing per-metre cost cannot fix free stationary hanging because that cost getter is never called on an idle tick.

The implemented rule is a minimum climbing effort. Each simulation tick charges for the greater of actual movement and a small reference movement over that tick:

```text
charged distance = max(native movement distance, idle reference speed * abs(native tick duration))
```

A reference speed of 0.5 metres per second is the first test tuning point. With the game's unmodified base cost of 8 stamina points per metre, that is 4 points per second before its normal modifiers. The floor is a chosen balance value, not a discovered vanilla idle rule. Normal full-speed movement keeps its existing cost. Slow movement pays at least the hanging cost, without adding a second charge on top of movement.

The native stamina accumulator, attribute modifiers, exhaustion transition and UI must remain authoritative. Tick replay must reproduce the same debit. A timer in the plugin or a direct replacement of current stamina is unsuitable. The climbing program reads movement; the mod must not change that read-only component to manufacture a debit.

No persistent natural-wall marker has been admitted. The change uses the native wall-climb gate, so it also affects ordinary climbing lattices. Ladders, running, gliding, crafting and building are untouched. The native depletion carries fractional cost and its recharge cache reacts to actual decreases; CanRecharge and other native rules remain authoritative. Stop clears the native wall state immediately. An exit-cleanup scheduling edge is not separately overridden. A natural-wall-only policy would need its own proven per-entity classification.

Default movement speed and stamina multiplier stay vanilla. No new stamina bar, perk, consumable, flight ability or unlimited grip is planned. The first play test must check hanging, slow and normal movement, exhaustion, stamina food/equipment, normal lattices and client/server agreement.

## Inverse kinematics

IK is suitable for small visual corrections to hands and feet after the native climbing animation has supplied the main pose. Prefer the game's existing target and solver path if a safe per-character interface can be proven.

[Naughty Dog's account of Uncharted 4's climbing](https://www.naughtydog.com/blog/uncharted_4_climbing_legacy_of_thieves_collection_pc) describes coordinated limb and root IK, foot contacts and analogue reach layered with authored animations. It supports using IK to fit a pose to uneven rock. It also shows why a limb solver alone is not a complete traversal system. The first ember-climb integration should keep the existing root motion and limit corrections to the limbs.

[ozz-animation's two-bone IK sample](https://guillaumeblanc.github.io/ozz-animation/samples/two_bone_ik/) provides a public alternative. It computes rotation corrections for a limb, with a pole vector, reach softening and a blend weight. Its [foot IK sample](https://guillaumeblanc.github.io/ozz-animation/samples/foot_ik/) demonstrates contact rays and surface-normal alignment, and explicitly leaves contact phases and unreachable targets to the integrator. The project uses the [MIT license](https://github.com/guillaumeblanc/ozz-animation/blob/master/LICENSE.md). It is a reference and fallback candidate; no dependency or borrowed code has been added.

Current owned-image research confirms native solvers and target controls, but the retained graph snapshots have no generic hand-target consumer. Their actor/phase provenance is not established. A safe climbing implementation still needs all of these:

- Identify the correct character instance and its limb chains without assuming bone indices across models.
- Establish the skeleton's local, model and world transforms and the native animation scheduling stage.
- Use reachable contact points and surface normals near the animated hands and feet.
- Preserve elbow and knee directions, blend corrections with the climbing pose, and release contacts during steps or loss of the wall.
- Bound raycasts and solver work. Avoid allocations, locks and unbounded searches on the animation path.
- Prove instance lifetime and write ownership. Clear targets when climbing stops, the actor changes or a world unloads.
- Keep physics and server-authoritative movement unchanged. Visual limb placement must never grant a hold or a top-out.

A solver alone does not supply a native integration. A ray or a planted hand does not prove that the body or the mantle animation can fit. Automatic natural-wall top-outs remain unavailable until their separate whole-body and path checks are proven.

## Acceptance boundary

Recovery from the hold/move reach mismatch is implemented and covered by inert tests. Native seam grace and natural-wall automatic top-outs remain disabled. The minimum hanging cost is implemented and exercised by owned native-code fixtures; gameplay balance is untested. IK remains research work. Simulated geometry, native-file checks and an inert plugin load do not establish gameplay, controller or multiplayer reliability.


The first offline play test failed: climbing starts, then loses grip and separates from the wall. The previous installation was restored. Reliable grip takes priority over new traversal features. Development adds paired movement observations around the existing native move check; those observations do not change movement decisions. A pure corner policy remains separate from runtime admission, and the paid upward hop still needs a native accepted-request bridge.

The physical movement follow-up fixes a source-proven outward drift on a bend: projecting onto the lagging visual normal created about 0.472 m/s of outward velocity at a correctly spaced 15-degree turn in a 120 Hz fixture. Fresh-plane movement removes that component while body rotation still eases. Two failing-before/passing-after regressions cover bends, real gap errors, contact suppression and overhangs. This is not yet confirmed as the cause or complete fix of the offline failure.


The diagnostic follow-up adds three read-only native observers for Jump acceptance, the native-invoked ActorInput accessor and the calculated-delta dispatcher. The complete set is checked before installation when diagnostics is enabled. Only owned scalar records leave a live native invocation. Lexical input pointers are cleared/restored before the owner returns; native calls, arguments and results are preserved. A 32-record queue uses one nonblocking claim per operation and reports dropped records. Request keys are correlation candidates under replay, not proof of a unique accepted/payment event. The dispatcher result precedes application and collision. This adds evidence for grip-loss and upward-hop research; it does not implement the paid upward hop.

The interrupted corner draft is retained for provenance. The implemented runtime adapter uses owned lexical state and the native velocity/probe pipeline, while the strict pure policy retains its explicit motion-oracle contract. It validates current context/entity/world/feet and initialized ordinary wall/actor/collider state. Same-update raw rays guide discovery only. Up to five discovery rays produce a finite velocity request; full/vertical/horizontal native candidates each obtain fresh three-hit one-face support. Public proposal fields and cached fits grant no candidate permission. The last actual candidate must match the final native velocity before native AL success can commit its separately eased facing. Original lattice successes remain unchanged. There is no direct position, transform, animation target or stamina write in this adapter.

One lexical move shares at most 36 actual custom rays, including ordinary prechecks, recovery, misses and failed discovery. A denied query latches refusal for the whole candidate; a partial three-hit scan cannot pass after a fourth denied ray. Native callbacks cannot reset that count. Rust return/unwind restores owned query usage and the previous lexical scope. Scene-query reentry and nested moves suspend custom work and restore baseline MODE/HOLD/NESTED/STUCK. No native or stack pointer is stored by the call-state module; opaque context/world addresses are equality keys only.

The offline tests exercise both corner types/directions at 120/60/20 Hz, actual Rust hook wiring, final-velocity fallback, false native AL, native-call counts and reentry. Their analytic geometry/configuration is not the native engine or its exact climbing capsule. The real ActorResolve/collision step, streamed geometry, animation/controller feel and client/server agreement need later gameplay validation. Independent reviews cleared the observer checkpoint eb42368; the new runtime integration needs its own review before installation. Paid upward hops, whole-path natural exits and surface IK remain unfinished.
