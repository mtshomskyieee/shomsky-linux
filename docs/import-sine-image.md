# Import a SINE brand image

How-to for adding or refreshing `:sine` left-top avatars. Normative rules:
[ADR-0015](adr/0015-sine-brand-avatars.md). Screen contract: [ADR-0014](adr/0014-sine-assistant.md).

## Pipeline

```text
assets/brand/<name>-rest|busy.(png|jpg)
        │
        ▼  cargo run --manifest-path tools/img2ascii/Cargo.toml -- assets/brand
assets/brand/<name>-rest|busy.txt     ← colored ANSI, fixed cols×rows
        │
        ▼  include_str! in src/sine_tui.rs  (+ Avatar in src/chrome.rs)
:sine left pane / :menu → Theme preview
```

| Path | Role |
|------|------|
| `assets/brand/*.{png,jpg}` | Source art (host; not copied into the image) |
| `assets/brand/*.txt` | Generated ANSI; Docker `COPY assets/brand/*.txt` |
| `tools/img2ascii` | Standalone converter (`JOBS` table = one entry per file) |
| `src/chrome.rs` | `Avatar` enum, Theme menu labels, `.avatar` persist |
| `src/sine_tui.rs` | `include_str!`, rest/busy swap in `render_brand_lines` |

Theme preview always shows **rest**. Busy is visible in `:sine` while work runs.

## Regenerate existing art

After editing a source image or img2ascii knobs:

```bash
cargo run --manifest-path tools/img2ascii/Cargo.toml -- assets/brand
./build.sh   # or your usual engram-shell rebuild
```

Commit both the source and the updated `.txt` in the same change.

## Add rest + busy for navi (or another single-pose avatar)

Navi today is one file (`navi.png` → `navi.txt`). `navi-clean.png` is a spare
source, not wired. To match coffee/laptop:

1. **Sources** under `assets/brand/`:
   - `navi-rest.png` — idle (from `navi-clean.png` / `navi.png` is fine)
   - `navi-busy.png` — working pose (pupil shift, glow, etc.)
2. **Jobs** in `tools/img2ascii/src/main.rs`: two entries (same BrightInk /
   dilate / cols×rows as the current navi job). Point `src` / `out` at the new
   names; remove or keep the single `navi.png` job.
3. **Regenerate** (command above).
4. **Runtime** in `src/sine_tui.rs`:
   - `include_str!` for `navi-rest.txt` and `navi-busy.txt`
   - In `render_brand_lines` for `Avatar::Navi`, pick busy vs rest like coffee:
     `BrandEyes::Busy(_) → busy`, else → rest
5. Rebuild. Update [ADR-0015](adr/0015-sine-brand-avatars.md) if the shipped
   pose set changes in a way reviewers need to see.

## Add a new avatar (new Theme entry)

1. Drop `myavatar-rest.*` and `myavatar-busy.*` in `assets/brand/`.
2. Add two `Job`s in img2ascii. Tune ink:
   - Neon / cyan on dark → `BrightInk`, low `bg_threshold`, `dilate` ≥ 1
   - Cream photo on cream floor → `FloodEllipse` + `DarkInk` + `ink_floor` ≈ 0.4–0.5
3. Regenerate `.txt`. Prefer ≤ **48** columns so Theme preview still fits beside
   the item list.
4. Extend `Avatar` in `src/chrome.rs`: `ALL`, `name`, `label`, `detail`, `parse`.
5. `include_str!` + match arm in `render_brand_lines` (preview stays on rest).
6. Rebuild. Menu Theme lists the new avatar from `Avatar::ALL`.

## Source image tips

- Roughly square; the tool stretches width ~2× for terminal cell aspect.
- Subject centered; spare margin is fine — FloodEllipse / corner knockout
  removes studio backdrop.
- Keep rest and busy at the **same pixel size** so the pane does not jump.
- Classic bunny (`docs/sine_image.txt`) is hand ASCII with live eye bounce; it
  does not go through img2ascii.

## Related

- [ADR-0015](adr/0015-sine-brand-avatars.md) — why build-time ANSI
- [ADR-0014](adr/0014-sine-assistant.md) — SINE layout
- [sine.md](sine.md) — operator how-to for the screen
