# ADR-0015: SINE brand avatars (img2ascii)

- **Status:** Accepted
- **Date:** 2026-11-26

## Context

`:sine` needs a left-top brand mark that reads as a character, not a
decoration. Operators also want chrome **color** and brand **avatar** to
change independently (`:menu` → Theme). Shipping raw bitmaps at runtime would
pull image crates into the shell TUI; hand-drawn braille (classic bunny) does
not cover photo or neon-logo subjects.

Related: [ADR-0014](0014-sine-assistant.md) (SINE screen),
[ADR-0002](0002-rust-language-and-style.md) (Rust / no heavy UI crates).
How-to: [import-sine-image.md](../import-sine-image.md).

## Decision

1. **Color ≠ avatar.** `Theme` (harbor / coffee / laptop / classic palette) and
   `Avatar` (navi / coffee / laptop / classic) are separate enums in
   `src/chrome.rs`, persisted under `/engram/user/.sine/` as `.theme` and
   `.avatar`. Menu Theme lists both; preview shows the highlighted avatar’s
   **rest** art.

2. **Build-time img2ascii.** Source images live under `assets/brand/`
   (`*.png` / `*.jpg`). A standalone tool (`tools/img2ascii`, outside the
   workspace crate graph) converts each job to a colored ANSI `.txt` of fixed
   cols×rows. `engram-shell` embeds those `.txt` files with `include_str!`
   (`src/sine_tui.rs`). Dockerfiles copy **only** `assets/brand/*.txt` into the
   image; sources stay host-side for regeneration.

3. **Pose model.** Image avatars use packaged rest / busy pictures (and a
   danger retint on error). Classic keeps `docs/sine_image.txt` with bouncing /
   error eyes. Navi may start as a single pose; rest+busy pairs are the
   preferred shape (coffee / laptop).

4. **Conversion knobs per job.** Neon / line art on dark → BrightInk + optional
   dilate. Cream subjects on cream studio floors → FloodEllipse subject mask +
   DarkInk + ink floor so the silhouette is not knocked out with the
   background. Cell aspect is corrected by stretching width before resize.

5. **Extension path.** New avatar or new pose = new source file(s) + `Job` in
   img2ascii + regenerate `.txt` + `Avatar` / `render_brand_lines` wiring.
   Steps: [import-sine-image.md](../import-sine-image.md).

## Consequences

- Regenerating art does not require changing shell logic unless cols/rows grow
  past the Theme preview budget (~48 cols) or a new `Avatar` variant is added.
- Source images should be committed with the `.txt` outputs so CI and
  contributors can re-run img2ascii without hunting assets.
- A PR that adds an avatar updates this ADR’s avatar list (or points at the
  how-to) in the same change set; [ADR-0014](0014-sine-assistant.md) layout row
  stays the product surface, not the conversion recipe.
- img2ascii stays out of the release workspace / image build graph; forgetting
  to regenerate `.txt` ships stale art.
