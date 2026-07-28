# Unified Maskable Adjustments — Phase 1: EditDoc Model + Serialization

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the persisted edit document from a `Vec<Op>` stack into the layer-shaped `EditDoc` (global `AdjustmentSet` + mask layers + global-only geometry/lens), behind adapter accessors so the app behaves identically — a visual no-op.

**Architecture:** `OpStack` is renamed to `EditDoc` (with `pub type OpStack = EditDoc;` so ~200 call sites keep compiling) and its internals become `{ version, global: AdjustmentSet, layers: Vec<MaskLayer>, lens, geometry }`. Every existing accessor (`exposure()`, `set_op()`, `reset()`, `is_identity()`) is reimplemented as an adapter over the new fields with identical signatures. `AdjustmentSet` grows to the full parameter block (tone curve, HSL, color grade, sharpen, dehaze, noise reduction, vibrance). Serialization bumps `STACK_VERSION` 1→2; old payloads deserialize to `None` (caller falls back to "no edits", stored bytes untouched).

**Tech Stack:** Rust workspace; serde/serde_json; existing crates `ferrolite-pipeline` (document model lives here) and `ferrolite-app` (one test consumer to fix).

**Spec:** `docs/superpowers/specs/2026-07-28-unified-maskable-adjustments-design.md` (§2, §5 persistence, §6.3 tests)

## Global Constraints

- Branch: `feat/ui-v2-rewrite`. Never commit to `main`.
- This phase is a **visual no-op**: no UI change, no shader change, no new pipeline behavior. New `AdjustmentSet` fields are CARRIED but not applied anywhere yet (Phases 2–3 wire them).
- `STACK_VERSION` becomes `2`. Version-mismatch deserialization returns `None` (exact current behavior — callers already fall back to default).
- Old field semantics preserved exactly: accessors return `None` when the corresponding params are identity, mirroring today's "op absent" semantics.
- Subagents run the **scoped gate** only (per task, named in each task); the coordinator runs the repo gate once at the end (CLAUDE.md "Gate tiers").
- All code comments follow existing style (rationale-bearing doc comments, no narration).

---

### Task 1: Expand `AdjustmentSet` to the full parameter block

**Files:**
- Modify: `ferrolite-pipeline/src/local.rs`
- Modify: `ferrolite-pipeline/src/op.rs` (only: add `Default` derives to `HslBand`, `Hsl`, `Sharpen`; add manual `Default` for `Dehaze`)
- Modify: `ferrolite-pipeline/src/lib.rs` (export `NoiseReduction`)

**Interfaces:**
- Consumes: existing `ToneCurve`, `Hsl`, `HslBand`, `ColorGrade`, `Sharpen`, `Dehaze` from `op.rs`; `DEHAZE_DEFAULT_RADIUS` (re-exported by `ferrolite_pipeline`, defined in the dehaze module — find with `grep -rn "DEHAZE_DEFAULT_RADIUS" ferrolite-pipeline/src/lib.rs` and use the crate-internal path).
- Produces (Task 2 and later phases rely on these exact names):
  - `AdjustmentSet` fields: `exposure, contrast, highlights, shadows, whites, blacks, temp, tint, saturation, hue, vibrance: f32`, `color: ColorSwatch`, `tone_curve: ToneCurve`, `hsl: Hsl`, `color_grade: ColorGrade`, `sharpen: Sharpen`, `dehaze: Dehaze`, `noise_reduction: NoiseReduction`, `texture: f32`, `clarity: f32`
  - `struct NoiseReduction { luminance: f32, detail: f32, color: f32, color_detail: f32 }` (zero-identity, `Default`)
  - `AdjustmentSet::is_identity(&self) -> bool` covering ALL fields above (reserved `texture`/`clarity` still ignored — no shader)
  - **`AdjustmentSet` is no longer `Copy`** (ToneCurve holds `Vec`s) — it stays `Clone + PartialEq + Debug + Default + Serialize + Deserialize`.

- [ ] **Step 1: Write the failing tests** (append to the existing `#[cfg(test)] mod tests` in `local.rs`)

```rust
#[test]
fn expanded_set_default_is_identity_and_serde_defaults_hold() {
    let s = AdjustmentSet::default();
    assert!(s.is_identity());
    // A payload written by an older build (missing every new field) loads as identity.
    let old_json = r#"{"exposure":0.0}"#;
    let parsed: AdjustmentSet = serde_json::from_str(old_json).unwrap();
    assert!(parsed.is_identity());
    assert_eq!(parsed, AdjustmentSet::default());
}

#[test]
fn each_structured_field_breaks_identity() {
    let mut s = AdjustmentSet::default();
    s.tone_curve.points = vec![(0.0, 0.1), (1.0, 1.0)];
    assert!(!s.is_identity(), "tone curve");

    let mut s = AdjustmentSet::default();
    s.hsl.bands[0].sat = 0.3;
    assert!(!s.is_identity(), "hsl");

    let mut s = AdjustmentSet::default();
    s.color_grade.shadows.sat = 0.4;
    assert!(!s.is_identity(), "color grade");

    let mut s = AdjustmentSet::default();
    s.sharpen.amount = 0.5;
    assert!(!s.is_identity(), "sharpen");

    let mut s = AdjustmentSet::default();
    s.dehaze.amount = 0.2;
    assert!(!s.is_identity(), "dehaze");

    let mut s = AdjustmentSet::default();
    s.noise_reduction.luminance = 0.5;
    assert!(!s.is_identity(), "noise reduction");

    let mut s = AdjustmentSet::default();
    s.vibrance = 0.1;
    assert!(!s.is_identity(), "vibrance");
}

#[test]
fn expanded_set_round_trips() {
    let mut s = AdjustmentSet::default();
    s.exposure = 0.5;
    s.tone_curve.points = vec![(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)];
    s.hsl.bands[3].hue = -0.2;
    s.sharpen = crate::op::Sharpen { amount: 0.8, radius: 2 };
    s.dehaze.amount = -0.3;
    let json = serde_json::to_string(&s).unwrap();
    let back: AdjustmentSet = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline expanded_set -- --nocapture` and `cargo test -p ferrolite-pipeline each_structured_field`
Expected: FAIL to compile ("no field `tone_curve`", "no field `vibrance`", …)

- [ ] **Step 3: Implement**

In `op.rs`: add `Default` to the derive lists of `HslBand`, `Hsl`, and `Sharpen`; add a manual `Default` for `Dehaze` (radius must seed the canonical default, not 0):

```rust
impl Default for Dehaze {
    /// Identity amount but the CANONICAL default radius, so a set that only
    /// ever touches `amount` still shapes the transmission the way the UI's
    /// radius slider default does.
    fn default() -> Self {
        Self { amount: 0.0, radius: DEHAZE_DEFAULT_RADIUS }
    }
}
```

(`DEHAZE_DEFAULT_RADIUS` — import from wherever `lib.rs` re-exports it; if it lives in a dehaze module, `use` that path.)

In `local.rs`: replace the reserved scalar block of `AdjustmentSet` (`texture, clarity, dehaze, sharpness, noise`) with:

```rust
    // New in the unified model (design 2026-07-28 §2): the full parameter block,
    // shared verbatim between the global layer and every mask layer. All
    // zero-identity, all `#[serde(default)]` (schema-stable forward).
    #[serde(default)]
    pub vibrance: f32,
    #[serde(default)]
    pub tone_curve: crate::op::ToneCurve,
    #[serde(default)]
    pub hsl: crate::op::Hsl,
    #[serde(default)]
    pub color_grade: crate::op::ColorGrade,
    #[serde(default)]
    pub sharpen: crate::op::Sharpen,
    #[serde(default)]
    pub dehaze: crate::op::Dehaze,
    #[serde(default)]
    pub noise_reduction: NoiseReduction,
    // Reserved neighborhood locals — no shader yet (Phase 4 owns them).
    #[serde(default)]
    pub texture: f32,
    #[serde(default)]
    pub clarity: f32,
```

Add below `ColorSwatch`:

```rust
/// Noise-reduction parameters (luminance + chroma). All zero-identity; no
/// shader yet (carried for schema stability — the V2 Effects tab shows the
/// sliders but they are not wired until their pass lands).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct NoiseReduction {
    pub luminance: f32,
    pub detail: f32,
    pub color: f32,
    pub color_detail: f32,
}
```

Remove `Copy` from `AdjustmentSet`'s derive list (ToneCurve holds `Vec`s). Fix the fallout inside `local.rs`: `reset_light`/`reset_color` use `let mut s = *self;` — change to `let mut s = self.clone();`.

Extend `is_identity`:

```rust
    pub fn is_identity(&self) -> bool {
        self.exposure == 0.0
            && self.contrast == 0.0
            && self.highlights == 0.0
            && self.shadows == 0.0
            && self.whites == 0.0
            && self.blacks == 0.0
            && self.temp == 0.0
            && self.tint == 0.0
            && self.saturation == 0.0
            && self.hue == 0.0
            && self.vibrance == 0.0
            && self.color.amount == 0.0
            && self.tone_curve.is_identity()
            && self.hsl.bands.iter().all(|b| b.hue == 0.0 && b.sat == 0.0 && b.lum == 0.0)
            && self.color_grade.is_identity()
            && self.sharpen.amount == 0.0
            && self.dehaze.is_identity()
            && self.noise_reduction == NoiseReduction::default()
    }
```

In `lib.rs`: add `NoiseReduction` to the `pub use local::{…}` list.

- [ ] **Step 4: Fix compile fallout from losing `Copy`, run the crate tests**

Run: `cargo check -p ferrolite-pipeline 2>&1 | head -30` — fix every "cannot move" / "does not implement `Copy`" error mechanically (`*a` → `a.clone()`, pass by reference where cheap). Likely spots: `local_node.rs` (per-layer uniform build), `uniforms.rs` (`local_adjust_uniform`).
Then run: `cargo test -p ferrolite-pipeline`
Expected: PASS (including the three new tests)

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/src/local.rs ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): expand AdjustmentSet to the full unified parameter block"
```

---

### Task 2: `EditDoc` — the layer-shaped document behind the `OpStack` API

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs` (struct `OpStack` → `EditDoc` + alias + adapters; rewrite the struct's tests)
- Modify: `ferrolite-pipeline/src/lib.rs` (export `EditDoc` alongside the `OpStack` alias)

**Interfaces:**
- Consumes: Task 1's expanded `AdjustmentSet` (fields listed there), existing `MaskLayer`, `LocalAdjustments`, `LensCorrection`, `Geometry`, `Op`, `OpKind`.
- Produces (every downstream crate relies on these EXACT signatures — they are today's signatures, unchanged):
  - `pub struct EditDoc { pub version: u32, pub global: AdjustmentSet, pub layers: Vec<MaskLayer>, pub lens: Option<LensCorrection>, pub geometry: Option<Geometry> }`
  - `pub type OpStack = EditDoc;` (both names exported from `lib.rs`)
  - `pub const STACK_VERSION: u32 = 2;`
  - Adapters, signatures identical to today: `is_identity() -> bool`, `set_op(&self, op: Op) -> EditDoc`, `reset(&self, kind: OpKind) -> EditDoc`, and getters `exposure() -> Option<Exposure>`, `white_balance() -> Option<WhiteBalance>`, `contrast() -> Option<Contrast>`, `dehaze() -> Option<Dehaze>`, `tone_curve() -> Option<ToneCurve>`, `hsl() -> Option<Hsl>`, `color_grade() -> Option<ColorGrade>`, `local_adjustments() -> Option<LocalAdjustments>`, `sharpen() -> Option<Sharpen>`, `geometry() -> Option<Geometry>`, `lens_correction() -> Option<LensCorrection>`
  - `Op`/`OpKind` enums stay unchanged (they remain the EDIT-message vocabulary — `EditOutcome.kind`, `needs_full_rebuild` — until Phase 2 retires them).

- [ ] **Step 1: Write the failing tests** (REPLACE the existing ordering-focused tests in `op.rs`'s `mod tests` — the canonical-`Vec`-order tests like the `kinds: Vec<OpKind>` assertions are obsolete because there is no `Vec<Op>`; keep + adapt every accessor/identity test)

```rust
#[test]
fn default_doc_is_identity_at_version_2() {
    let d = EditDoc::default();
    assert_eq!(d.version, STACK_VERSION);
    assert_eq!(STACK_VERSION, 2);
    assert!(d.is_identity());
    assert!(d.exposure().is_none());
    assert!(d.local_adjustments().is_none());
}

#[test]
fn set_op_and_getters_round_trip_every_op_kind() {
    let d = EditDoc::default()
        .set_op(Op::Exposure(Exposure { ev: 0.75 }))
        .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.2, tint: -0.1 }))
        .set_op(Op::Contrast(Contrast { amount: 0.3 }))
        .set_op(Op::Dehaze(Dehaze { amount: 0.4, radius: 9 }))
        .set_op(Op::Sharpen(Sharpen { amount: 0.6, radius: 3 }));
    assert_eq!(d.exposure(), Some(Exposure { ev: 0.75 }));
    assert_eq!(d.white_balance(), Some(WhiteBalance { temp: 0.2, tint: -0.1 }));
    assert_eq!(d.contrast(), Some(Contrast { amount: 0.3 }));
    assert_eq!(d.dehaze(), Some(Dehaze { amount: 0.4, radius: 9 }));
    assert_eq!(d.sharpen(), Some(Sharpen { amount: 0.6, radius: 3 }));
    assert!(!d.is_identity());
}

#[test]
fn getters_are_none_at_identity_values() {
    // Setting an identity-valued op is equivalent to reset (mirrors the old
    // "op absent" semantics the whole app keys has_edits on).
    let d = EditDoc::default()
        .set_op(Op::Exposure(Exposure { ev: 0.5 }))
        .set_op(Op::Exposure(Exposure { ev: 0.0 }));
    assert!(d.exposure().is_none());
    assert!(d.is_identity());
}

#[test]
fn reset_clears_exactly_one_kind() {
    let d = EditDoc::default()
        .set_op(Op::Exposure(Exposure { ev: 0.5 }))
        .set_op(Op::Contrast(Contrast { amount: 0.3 }));
    let d = d.reset(OpKind::Exposure);
    assert!(d.exposure().is_none());
    assert_eq!(d.contrast(), Some(Contrast { amount: 0.3 }));
}

#[test]
fn local_adjustments_map_to_layers() {
    let la = LocalAdjustments {
        layers: vec![crate::local::MaskLayer {
            name: "Mask 1".into(),
            visible: true,
            mask: Default::default(),
            adjustments: Default::default(),
        }],
    };
    let d = EditDoc::default().set_op(Op::LocalAdjustments(la.clone()));
    assert_eq!(d.layers.len(), 1);
    assert_eq!(d.local_adjustments(), Some(la));
    // A created (even identity-valued) mask counts as an edit, as today.
    assert!(!d.is_identity());
    let d = d.reset(OpKind::LocalAdjustments);
    assert!(d.local_adjustments().is_none());
}

#[test]
fn geometry_and_lens_are_globals_not_layers() {
    let g = Geometry { crop: CropRect::full(), angle_deg: 2.0, aspect: Aspect::Original };
    let d = EditDoc::default().set_op(Op::Geometry(g.clone()));
    assert_eq!(d.geometry(), Some(g));
    assert!(d.layers.is_empty());
    assert!(!d.is_identity());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib op::tests`
Expected: FAIL to compile (`EditDoc` not defined)

- [ ] **Step 3: Implement `EditDoc` + adapters**

Replace the `OpStack` struct (op.rs:366-380) with:

```rust
/// The edit document (design 2026-07-28 §2): geometry ops global-only, one
/// global `AdjustmentSet` ("the layer with no mask"), and mask layers stacked
/// on top. Immutable editing: `set_op`/`reset` return new docs. The old
/// `Vec<Op>` stack is gone; `Op`/`OpKind` survive as the edit-message
/// vocabulary (`EditOutcome.kind`, rebuild decisions) until Phase 2.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct EditDoc {
    pub version: u32,
    #[serde(default)]
    pub global: AdjustmentSet,
    #[serde(default)]
    pub layers: Vec<MaskLayer>,
    #[serde(default)]
    pub lens: Option<LensCorrection>,
    #[serde(default)]
    pub geometry: Option<Geometry>,
}

/// Compatibility alias: the rest of the workspace still says `OpStack`.
/// Retired in Phase 2 alongside `Op`/`OpKind`.
pub type OpStack = EditDoc;

impl Default for EditDoc {
    fn default() -> Self {
        Self {
            version: STACK_VERSION,
            global: AdjustmentSet::default(),
            layers: Vec::new(),
            lens: None,
            geometry: None,
        }
    }
}
```

(You'll need `use crate::local::{AdjustmentSet, MaskLayer};` at the top — `LocalAdjustments` is already imported.)

Set `pub const STACK_VERSION: u32 = 2;`.

Implement the adapters on `impl EditDoc`. Getters return `None` at identity (mirroring "op absent"); setters write the corresponding `global` field (identity value included — the getter normalizes):

```rust
impl EditDoc {
    /// Unedited: identity global set, no mask layers, no geometry/lens.
    pub fn is_identity(&self) -> bool {
        self.global.is_identity()
            && self.layers.is_empty()
            && self.lens.is_none()
            && self.geometry.is_none()
    }

    /// Return a new doc with `op`'s parameters written into their unified home
    /// (global set field, the layer list, or a geometry/lens global).
    pub fn set_op(&self, op: Op) -> EditDoc {
        let mut d = self.clone();
        match op {
            Op::Exposure(e) => d.global.exposure = e.ev,
            Op::WhiteBalance(w) => {
                d.global.temp = w.temp;
                d.global.tint = w.tint;
            }
            Op::Contrast(c) => d.global.contrast = c.amount,
            Op::Dehaze(x) => d.global.dehaze = x,
            Op::ToneCurve(t) => d.global.tone_curve = t,
            Op::Hsl(h) => d.global.hsl = h,
            Op::ColorGrade(g) => d.global.color_grade = g,
            Op::LocalAdjustments(la) => d.layers = la.layers,
            Op::Sharpen(s) => d.global.sharpen = s,
            Op::LensCorrection(l) => d.lens = Some(l),
            Op::Geometry(g) => d.geometry = Some(g),
        }
        d
    }

    /// Return a new doc with `kind`'s parameters reset to identity.
    pub fn reset(&self, kind: OpKind) -> EditDoc {
        let mut d = self.clone();
        match kind {
            OpKind::Exposure => d.global.exposure = 0.0,
            OpKind::WhiteBalance => {
                d.global.temp = 0.0;
                d.global.tint = 0.0;
            }
            OpKind::Contrast => d.global.contrast = 0.0,
            OpKind::Dehaze => d.global.dehaze = Dehaze::default(),
            OpKind::ToneCurve => d.global.tone_curve = ToneCurve::default(),
            OpKind::Hsl => d.global.hsl = Hsl::default(),
            OpKind::ColorGrade => d.global.color_grade = ColorGrade::default(),
            OpKind::LocalAdjustments => d.layers = Vec::new(),
            OpKind::Sharpen => d.global.sharpen = Sharpen::default(),
            OpKind::LensCorrection => d.lens = None,
            OpKind::Geometry => d.geometry = None,
        }
        d
    }

    pub fn exposure(&self) -> Option<Exposure> {
        (self.global.exposure != 0.0).then(|| Exposure { ev: self.global.exposure })
    }
    pub fn white_balance(&self) -> Option<WhiteBalance> {
        (self.global.temp != 0.0 || self.global.tint != 0.0).then(|| WhiteBalance {
            temp: self.global.temp,
            tint: self.global.tint,
        })
    }
    pub fn contrast(&self) -> Option<Contrast> {
        (self.global.contrast != 0.0).then(|| Contrast { amount: self.global.contrast })
    }
    pub fn dehaze(&self) -> Option<Dehaze> {
        (!self.global.dehaze.is_identity()).then(|| self.global.dehaze)
    }
    pub fn tone_curve(&self) -> Option<ToneCurve> {
        (!self.global.tone_curve.is_identity()).then(|| self.global.tone_curve.clone())
    }
    pub fn hsl(&self) -> Option<Hsl> {
        self.global
            .hsl
            .bands
            .iter()
            .any(|b| b.hue != 0.0 || b.sat != 0.0 || b.lum != 0.0)
            .then_some(self.global.hsl)
    }
    pub fn color_grade(&self) -> Option<ColorGrade> {
        (!self.global.color_grade.is_identity()).then_some(self.global.color_grade)
    }
    pub fn local_adjustments(&self) -> Option<LocalAdjustments> {
        (!self.layers.is_empty()).then(|| LocalAdjustments {
            layers: self.layers.clone(),
        })
    }
    pub fn sharpen(&self) -> Option<Sharpen> {
        (self.global.sharpen.amount != 0.0).then_some(self.global.sharpen)
    }
    pub fn geometry(&self) -> Option<Geometry> {
        self.geometry.clone()
    }
    pub fn lens_correction(&self) -> Option<LensCorrection> {
        self.lens.clone()
    }
}
```

Note the getter/field name collision: `pub geometry` field vs `geometry()` method is legal Rust (namespaces differ), but if the borrow checker or readability fights you, rename the FIELDS to `geometry`/`lens` as given and keep methods — this compiles; existing call sites use the methods.

Delete the old `Vec<Op>`-based method bodies and the obsolete ordering tests (`set_op_keeps_canonical_order`-style tests asserting `kinds: Vec<OpKind>`); adapt remaining tests that used `s.ops.len()` / `s.ops.is_empty()` to accessor assertions.

In `lib.rs`, add `EditDoc` to the `pub use op::{…}` list (keep `OpStack` exported too).

- [ ] **Step 4: Run the crate's tests**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS except `serialize::tests` (old raw-JSON fixtures now version-mismatch — Task 3 owns those; if they fail, that's expected — note it, don't fix serialize.rs here). If anything else fails, fix it in this task.

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline --lib op::tests
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): layer-shaped EditDoc behind the OpStack adapter API (STACK_VERSION 2)"
```

---

### Task 3: Serialization — version 2 codec + old-payload fallback

**Files:**
- Modify: `ferrolite-pipeline/src/serialize.rs` (doc comment + tests; the `serialize`/`deserialize` bodies likely need NO change — they are generic over the struct)

**Interfaces:**
- Consumes: Task 2's `EditDoc` (`OpStack` alias), `STACK_VERSION == 2`.
- Produces: unchanged public API `serialize(&OpStack) -> String`, `deserialize(&str) -> Option<OpStack>`. Spec guarantees: v1 payloads → `None` (caller falls back to identity; stored bytes untouched — verified by the ops-persist layer's existing behavior of only writing on edit).

- [ ] **Step 1: Rewrite the test module** (replace tests that build old-shape raw JSON; keep the property being tested)

```rust
#[test]
fn round_trips_a_full_document() {
    let d = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.75 }))
        .set_op(Op::ToneCurve(ToneCurve {
            points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
            mode: CurveMode::Linear,
            ..Default::default()
        }))
        .set_op(Op::LocalAdjustments(LocalAdjustments {
            layers: vec![MaskLayer {
                name: "Sky".into(),
                visible: true,
                mask: Default::default(),
                adjustments: {
                    let mut a = AdjustmentSet::default();
                    a.exposure = -0.5;
                    a
                },
            }],
        }))
        .set_op(Op::Geometry(Geometry {
            crop: CropRect { x: 0.05, y: 0.05, w: 0.9, h: 0.9 },
            angle_deg: 2.5,
            aspect: Aspect::SixteenNine,
        }));
    let text = serialize(&d);
    assert_eq!(deserialize(&text), Some(d));
}

#[test]
fn round_trips_the_empty_document() {
    let d = OpStack::default();
    assert_eq!(deserialize(&serialize(&d)), Some(d));
}

#[test]
fn v1_payload_is_none_bytes_untouched_semantics() {
    // A real pre-EditDoc payload (version 1, Vec<Op> shape): must load as None
    // so callers fall back to "no edits" — never a parse panic, never a
    // half-migrated doc.
    let v1 = r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#;
    assert_eq!(deserialize(v1), None);
}

#[test]
fn future_version_is_none() {
    let json = r#"{"version":999,"global":{},"layers":[]}"#;
    assert_eq!(deserialize(json), None);
}

#[test]
fn garbage_is_none() {
    assert_eq!(deserialize("not json {{"), None);
}

#[test]
fn missing_new_fields_load_as_identity() {
    // Forward tolerance within v2: a minimal v2 payload (older v2 build,
    // fewer fields) loads with serde defaults.
    let json = r#"{"version":2}"#;
    let d = deserialize(json).unwrap();
    assert!(d.is_identity());
}
```

Careful: the v1 fixture will fail `serde_json::from_str::<EditDoc>` (unknown `ops` field is TOLERATED by serde's default — the struct simply gets defaults, then the VERSION check rejects it). Both rejection paths end in `None`; the test asserts the outcome, not the path.

- [ ] **Step 2: Run tests**

Run: `cargo test -p ferrolite-pipeline --lib serialize`
Expected: PASS (if `deserialize`'s version check is intact, no body change needed). If a test fails, fix `serialize.rs` — not the tests.

- [ ] **Step 3: Update the module doc comment** to describe the v2 EditDoc payload and the v1-fallback behavior (mirror the wording style of the current header).

- [ ] **Step 4: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/src/serialize.rs
git commit -m "feat(pipeline): v2 EditDoc codec — v1 payloads fall back to unedited"
```

---

### Task 4: Workspace sweep — dependent crates compile + behave identically

**Files:**
- Modify: `ferrolite-app/src/develop/thumb_regen.rs:216` (test uses `stack.ops.push(…)` — the only direct `.ops` access outside the pipeline crate)
- Modify: any `ferrolite-app`/`ferrolite-export` fallout the compiler reports (expected: none beyond the above — all consumers use the adapter accessors; `ViewerState::op_stack_hash` uses `ferrolite_previews::hash_serde(&self.op_stack)` which is serde-based and needs no change)

**Interfaces:**
- Consumes: Tasks 1–3 (`EditDoc` behind `OpStack`, v2 codec).
- Produces: green scoped gates on every dependent crate; the app builds and its full test suite passes unchanged (visual no-op proof at the automated level).

- [ ] **Step 1: Fix the known test**

In `thumb_regen.rs`, replace the `stack.ops.push(ferrolite_pipeline::Op::Exposure(…))` construction with the adapter:

```rust
let stack = ferrolite_pipeline::OpStack::default()
    .set_op(ferrolite_pipeline::Op::Exposure(ferrolite_pipeline::Exposure { ev: 1.0 }));
```

(Match the exact `ev` value the existing test used; keep the surrounding assertions untouched.)

- [ ] **Step 2: Sweep the workspace for fallout**

Run: `cargo check --workspace --all-targets 2>&1 | grep -E "^error" | head -30`
Fix every error mechanically, preserving behavior. Expected classes: none, or `Copy`-loss moves on `AdjustmentSet` in `ferrolite-app` mask UI (`*a` → `a.clone()`). Do NOT change any logic — if a fix seems to need a behavior decision, stop and report instead.

- [ ] **Step 3: Run the dependent crates' tests**

Run: `cargo test -p ferrolite-app` and `cargo test -p ferrolite-export`
Expected: PASS, zero test-logic changes beyond Step 1. A failure here means an adapter's semantics drifted from the old op semantics — fix the ADAPTER in `op.rs` (e.g. a getter's identity condition), not the consuming test.

- [ ] **Step 4: Scoped gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy -p ferrolite-app -p ferrolite-export --all-targets -- -D warnings
git add -A
git commit -m "refactor(app): consume EditDoc via adapter accessors (visual no-op)"
```

---

## Coordinator wrap-up (not a subagent task)

1. `rustup update stable`, then the full **repo gate**: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --all-targets && cargo test --workspace`.
2. Hand the author the visual test plan:
   - **Mostly nothing to visually test** — Phase 1 is a document-model refactor behind unchanged accessors; no UI or shader changed. One expected, user-visible behavior to CONFIRM rather than judge: **edits saved before this change load as "no edits"** (v1 payloads; by design, spec §2). Open a previously edited image → sliders at defaults, image renders unedited. Then make an edit, close, reopen → the edit persists (v2 round-trip through the real `frl:ops` path).
   - Quick smoke (5 min): open a RAW → drag Exposure + Dehaze → create a mask, paint, drag its Exposure → undo/redo a few times → export one JPEG. Failure signature for all: wrong rendering, missing mask effect, or a panic on open/save.
3. Wait for the author's confirmation before finishing the branch phase (CLAUDE.md).
