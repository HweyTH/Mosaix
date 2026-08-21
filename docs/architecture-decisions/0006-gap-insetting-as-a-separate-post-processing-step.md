# Gap insetting is a separate apply_gaps() step, not a parameter on the zone functions

**Status:** accepted

Configurable gaps/padding (Tier 3 #21) inset a computed zone `Rect` on two kinds of edges: an *outer gap* on edges that touch the display's work-area boundary, and an *inner gap* on edges that would border a neighboring zone (e.g. the shared edge between `HalfZone::Left` and `HalfZone::Right`) -- even though Phase 1 has no window tree and only ever places one window per command, so that "neighbor" is hypothetical. This mirrors FancyZones/GlazeWM: a lone snapped window still reads as gapped.

`mosaix-layout`'s existing zone functions (`snap_to_half`, `snap_to_quarter`, `snap_to_third`, `resolve_zone_cycle`) stay exactly as they are: pure `(container, zone) -> Rect` fraction geometry, no config awareness, no changed signatures, no changed tests. A new `apply_gaps(raw_zone: Rect, container: Rect, gaps: Gaps) -> Rect` function is called by `mosaix-engine` after computing the raw zone rect; it tells interior from exterior by comparing each edge of `raw_zone` against the matching edge of `container`.

## Considered Options

- **Add a `gaps: Gaps` parameter to every zone function directly**: rejected -- it conflates two orthogonal concerns ("which fraction of the container is this zone" vs. "how much should its edges be inset") inside one function, and touches every existing call site and test for a feature the ticket itself scopes as a separate concern (#21 lists `mosaix-domain`/`mosaix-layout`, distinct from the zone-cycle work already landed).
- **Uniform single gap value on all four edges, no interior/exterior distinction**: rejected -- looks visually wrong today (doubled padding is fine, but a screen-edge gap and a between-windows gap are conventionally different sizes in every prior-art tool reviewed in ARCHITECTURE.md section 2), and would need reworking the moment an automatic tiling tree (Phase 3) makes real neighbors exist.

## Consequences

- `Gaps` (outer: i32, inner: i32 -- both uniform per-value, not per-edge-configurable) is a new type in `mosaix-domain`, alongside `Rect`.
- `apply_gaps` is pure and independently testable against fixed `(raw_zone, container)` fixtures without touching the engine or any config-loading machinery.
- When an automatic tiling tree lands (Phase 3), `apply_gaps` is expected to generalize to multi-window neighbor-aware insetting rather than being replaced -- the outer/inner split already matches what that future planner will need.
