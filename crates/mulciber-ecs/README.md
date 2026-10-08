# mulciber-ecs

A small sparse-set entity component store, independent of Mulciber's graphics.

- Entities are a slot index plus a generation. Despawning bumps the slot's generation, so a stale
  handle misses every lookup instead of aliasing whatever reuses the slot.
- Each component type lives in its own sparse set: parallel dense arrays of entities and values,
  plus a sparse index by slot. Insert, remove and lookup are O(1) and iteration walks dense memory.
  Removal swap-pops, so iteration order is unspecified.
- Any `'static` type is a component. There are no systems, schedules or built-in components; the
  game decides what runs when.

```rust
use mulciber_ecs::World;

struct Position(f32, f32);
struct Velocity(f32, f32);

let mut world = World::new();
let ball = world.spawn();
world.insert(ball, Position(0.0, 0.0));
world.insert(ball, Velocity(1.0, 2.0));

for (_, position, velocity) in world.query2_mut::<Position, Velocity>() {
    position.0 += velocity.0;
    position.1 += velocity.1;
}
assert_eq!(world.get::<Position>(ball).map(|p| (p.0, p.1)), Some((1.0, 2.0)));
```
