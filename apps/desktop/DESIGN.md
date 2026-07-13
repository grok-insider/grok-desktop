# Design System: Grok Desktop

**Scope:** Electron renderer (`apps/desktop/src`). Read this before any UI work.
**Direction:** Light-first, IBM Plex, single charcoal-green accent. Precision instrument, not marketing page.
**Stack:** Tailwind v4 (`@theme` tokens) + shadcn/ui primitives (new-york) + CSS variables below.

Related engineering docs: [docs/README.md](../../docs/README.md),
[coding-guidelines.md](../../docs/development/coding-guidelines.md),
[debugging-and-qa.md](../../docs/development/debugging-and-qa.md).

---

## 1. Visual Theme & Atmosphere

Grok Desktop is a **Windows-first AI workspace**: calm, dense-but-readable, technical.
The metaphor is a **precision instrument in a well-lit architecture studio** — cool
green-gray neutrals, quiet structure, mono type as the technical signature. It must
never read as a generic "AI SaaS" page.

- **Density:** daily-app balanced (5/10). Not gallery-airy, not cockpit-cramped.
  Lists and tables are compact; reading surfaces (chat) get room to breathe.
- **Variance:** controlled (3/10). The shell is predictable and symmetric; content
  areas may vary. No decorative asymmetry in chrome.
- **Motion:** restrained fluid CSS (4/10). 150–300 ms, `transform`/`opacity` only.
  No cinematic choreography, no perpetual decorative loops.
- **Mood words:** calm, exact, ventilated, matte. Nothing glows.

## 2. Color Palette & Roles (60-30-10, light-first)

One hue family (green-gray, hue ≈ 165°) carries the whole neutral system, so the UI
never fluctuates warm/cool. One accent: **charcoal-green ink** — a very dark desaturated
green used for primary CTAs and selection, *not* a bright brand color. Semantic colors
are status-only.

### ~60% — Canvas & primary surfaces

| Token | Hex | OKLCH | Role |
|---|---|---|---|
| **Canvas Mist** `--background` | `#f4f6f5` | `oklch(97.1% 0.0025 165)` | App canvas, workspace background |
| **Pure Surface** `--card` / `--popover` | `#ffffff` | `oklch(100% 0 0)` | Cards, dialogs, composer, list rows |
| **Soft Surface** `--muted` | `#f7f8f7` | `oklch(97.8% 0.0017 146)` | Hover fills, wells, skeletons, table headers |

### ~30% — Secondary structure

| Token | Hex | OKLCH | Role |
|---|---|---|---|
| **Sidebar Sage** `--sidebar` | `#e9edeb` | `oklch(94.2% 0.005 165)` | Sidebar, rails, secondary panels |
| **Whisper Line** `--border` | `#dfe3e1` | `oklch(91.2% 0.0051 165)` | Hairline dividers, card borders |
| **Structure Line** `--input` | `#cbd1ce` | `oklch(85.5% 0.0077 165)` | Input/field borders, sidebar/topbar edges (`--sidebar-border`), strong separators |
| **Structure Line Deep** `--input-hover` | `#adb6b1` | `oklch(76.3% 0.009 165)` | Hover state of Structure Line borders |
| **Sage Rail** `--secondary` | `#e9edeb` | `oklch(94.2% 0.005 165)` | Segmented-control rails, progress tracks (same value as `--sidebar`) |
| **Accent Wash** `--accent` | `#e2e9e5` | `oklch(92.8% 0.0092 161)` | Selected/hover tint behind interactive rows |
| **Ink Secondary** `--muted-foreground` | `#5e6662` | `oklch(50.2% 0.0117 165)` | Secondary text, descriptions (5.5:1 on canvas) |
| **Ink Tertiary** `--subtle-foreground` | `#687069` | `oklch(53.6% 0.0145 149)` | Meta, timestamps, section labels (4.7:1 on canvas) |

### ~10% — Accent + semantic status

| Token | Hex | OKLCH | Role |
|---|---|---|---|
| **Charcoal Ink** `--primary` / `--foreground` | `#252d29` (fg `#1d211f`) | `oklch(28.8% 0.0133 164)` | Primary CTA fill, active tab bar, brand mark; foreground = body text (15:1) |
| **Charcoal Ink Deep** `--primary-hover` | `#111713` | `oklch(19.7% 0.0121 156)` | Primary hover/active. Never `#000000` |
| **Signal Blue** `--info` | `#456b84` | `oklch(51% 0.0587 238)` | Running/streaming state, progress, focus ring (5.2:1) |
| **Verdant** `--success` | `#3f7255` | `oklch(50.7% 0.0725 158)` | Completed, connected, success (5.2:1) |
| **Clay** `--warning` | `#8a5734` | `oklch(50.6% 0.0839 54)` | Needs approval/review, limited mode (5.2:1 on soft) |
| **Oxide** `--destructive` | `#a54545` | `oklch(51.8% 0.1275 23)` | Failed, danger actions (5.5:1); hover `--destructive-hover` `#8e3535` |
| Soft chips: `--info-soft` `#e6f0f5`, `--success-soft` `#e6f1ea`, `--warning-soft` `#f6ece3`, `--destructive-soft` `#faeaea` | | ~`oklch(95% 0.015 *)` | Status chip / banner backgrounds only |

**Rules**

- Exactly **one** accent family (charcoal-green ink). Semantic colors appear only as
  status — never decorative. Two sanctioned extensions: Verdant marks a control's
  **on/enabled state** (Switch checked track — the product's long-standing meaning),
  and Signal Blue also marks **agentic "work" mode** identity (work-thread icons),
  since work ≈ running.
- Saturation stays low everywhere (chroma ≤ 0.13). No neon, no purple, no gradients.
- Body text pairs must be ≥ 4.5:1; large text and focus indicators ≥ 3:1. The pairs
  above are pre-verified — reuse them instead of inventing new combinations.
- Hairline borders (`--border`, ~1.2:1) are decorative; component identification must
  also come from fill, label, or focus ring — never border alone.
- Status is never color-only: always pair with a label or icon.
- Dark mode: not shipped. Keep all colors flowing through the CSS variables below so a
  `.dark` block can remap them later; never hardcode hex in JSX.

### Semantic token implementation (styles.css)

`styles.css` defines the semantic variables above directly and exposes them to
Tailwind through `@theme inline`. The renderer migration is complete: the old
BEM stylesheet and its compatibility aliases have been removed. Do not
reintroduce legacy aliases; use the semantic token names in this document.

## 3. Typography

| Role | Font | Notes |
|---|---|---|
| UI body, labels, headings | **IBM Plex Sans** (`@fontsource-variable/ibm-plex-sans`) | Hierarchy via weight (400/500/600), not size jumps |
| Code, run IDs, timestamps, counts, keyboard hints, composer metadata | **IBM Plex Mono** (`@fontsource/ibm-plex-mono` 400/500/600) | The technical signature. Always `font-variant-numeric: tabular-nums` for numbers. **600 is mono's boldest loaded weight** — `font-synthesis: none` is set, so never pair `font-mono` with `font-bold` (700); use `font-semibold` |

- **Never** mono-only the whole UI — chat body is Plex Sans. Mono is seasoning, not the meal.
- **Banned:** Inter, Segoe-only stacks, generic serifs anywhere in the product.
- Fonts are **self-hosted only** (fontsource). CSP is `font-src 'self'`; a font CDN is a security regression.

### Scale (px @ default zoom)

| Token | Size / line-height | Weight | Use |
|---|---|---|---|
| `text-label` | 11 / 16 | 500–600, mono or sans | Section labels, badges, meta. Uppercase labels get `+0.06em` tracking |
| `text-body-sm` | 12 / 18 | 400 | Dense table cells, secondary rows |
| `text-body` | 13 / 20 | 400 | Default UI copy, nav, buttons |
| `text-body-lg` | 14 / 22 | 400 | Chat transcript body, settings descriptions |
| `text-title-sm` | 15 / 22 | 600 | Card/dialog titles |
| `text-title` | 18 / 26 | 600 | Section headers |
| `text-title-lg` | 22 / 30 | 600 | Page headers |
| `text-display` | 28 / 36 | 650 | Home greeting / empty states only |

Floor is **11px** — nothing smaller, ever; any 7–10px text is a regression.
Chat/reading measure 65–75ch. Body tracking is default; never tighten body text.

## 4. Spacing, Radius, Elevation

- **Spacing:** 4/8 scale (`4, 8, 12, 16, 20, 24, 32, 40, 48`). Never invent 13/17/19px
  gaps in new work. Related items share the smaller gap; sections separate with ≥ 24.
- **Radius:** exactly three values — **5px** (inputs, chips, buttons; Tailwind
  `rounded-sm`/`rounded-md` both map here so nothing can render off-scale),
  **7px** (`rounded-lg`: cards, panels, large CTA buttons), **9px** (`rounded-xl`:
  composer, dialogs, overlays). No pills except status chips and count badges;
  no fully-round rectangles.
- **Elevation:** matte, tinted to the green-gray hue — never pure-black shadows.
  - Level 0 (default): borders only, no shadow.
  - Level 1 (raised row/card hover): `0 1px 2px rgb(24 33 28 / 5%)`
  - Level 2 (composer, popover): `0 4px 16px rgb(28 38 33 / 6%)`
  - Level 3 (dialog, overlay): `0 24px 70px rgb(25 35 30 / 18%), 0 3px 14px rgb(25 35 30 / 10%)`
  - Backdrops: `rgb(20 27 24 / 34%)`, optional `blur(2px)`. No glassmorphism surfaces.

## 5. Component Stylings

- **Button (primary):** Charcoal Ink fill, off-white text, radius 5 (`rounded-md`;
  hero CTAs like "New conversation" use radius 7 `rounded-lg`), height 34–36,
  weight 600, 13px. Hover → Ink Deep. Active → `scale(.98)`. One primary per view.
  Disabled buttons stay hoverable (no `pointer-events-none`) — views rely on
  `title` tooltips to explain unavailable actions.
- **Button (secondary/outline):** Pure Surface fill, Structure Line border. Hover →
  Soft Surface fill. **Ghost:** transparent, secondary ink, hover Soft Surface.
  **Danger:** Oxide text on `--destructive-soft` with `#dfbbbb`-tone border; solid Oxide
  fill only for final confirmation actions.
- **Icon button:** 34×34 min target, transparent until hover, always `aria-label`.
- **Input / textarea:** Pure Surface fill, Structure Line border, radius 5, 13px, label
  above (never placeholder-only), focus = ring (see §8). Key/ID fields use Plex Mono.
- **Composer:** the hero control. Pure Surface, Structure Line border, radius 9,
  Level-2 shadow; focus-within deepens border + shadow. Metadata row (project, capability)
  in 11px Plex Mono, Ink Tertiary.
- **Sidebar:** Sidebar Sage fill, Structure Line right border (`--sidebar-border`). Nav items 38px, radius 5,
  13px/500; active = Pure Surface fill + subtle Level-1 shadow + 600 weight. Count badges
  in Plex Mono. Section labels 11px uppercase mono, Ink Tertiary.
- **Card / panel:** Pure Surface + Whisper Line border, radius 7, no default shadow.
  Prefer border-top dividers or whitespace over nested cards in dense areas.
- **Dialog:** radius 9 (`rounded-xl`), Level-3 shadow, backdrop scrim; enter 160 ms translate+fade.
  Focus is trapped (`useDialogFocus`) and Escape always closes.
- **Status chips (`RunStatus`):** soft semantic fill + matching ink + 5px dot + label,
  11px/600, pill radius. State→token: running/planning/streaming → info; queued/paused/
  cancelled → neutral (`--muted` fill, secondary ink); awaiting_approval /
  interrupted_needs_review → warning; completed → success; failed → destructive.
- **Skeletons:** shimmer bars matching final layout dimensions; no circular spinners for
  content loads (spinners only inside buttons for in-flight actions).
- **Empty states:** icon + one-line explanation + one action. No filler marketing copy.

## 6. Layout Principles

- **Shell:** shadcn `Sidebar` (`collapsible="icon"`): fixed sidebar `16rem`
  (icon rail `3rem`) + fluid `SidebarInset` workspace; topbar 54px. Toggle via
  `SidebarTrigger` or Ctrl/Cmd+B; open state persists in localStorage.
  Content pages pad `clamp(24px, 3.2vw, 48px)` inline, max-width 1440–1540px centered.
- Chat transcript column: `min(760px, 100%)` centered — the 65–75ch measure.
- Text on the sidebar (Sage) must use `--muted-foreground` or stronger —
  `--subtle-foreground` only clears AA on Canvas/Pure surfaces, not on Sage.
- CSS Grid for page scaffolding; flex for rows. No absolute-position layout hacks; no
  overlapping content zones.
- Responsive: ≥768px full shell with user-toggled icon rail; <768px the sidebar
  becomes an off-canvas sheet (shadcn mobile behavior; the Electron window's
  860px minimum keeps the desktop layout in-product). Touch targets: ≥ 34px for primary and
  standalone controls (44px preferred for hero actions); compact inline/auxiliary
  controls (dialog close, in-card icon actions, segmented tabs, metadata chips) may go
  down to 27px, never smaller. No horizontal page scroll ever.

## 7. Motion & Interaction

- Durations: 120–200 ms micro (hover, press), 200–300 ms structural (sidebar collapse,
  dialog). Nothing exceeds 400 ms.
- Easing: `cubic-bezier(.2, .8, .2, 1)` (existing `--ease`) — ease-out entrances,
  faster exits (~70% of enter).
- Animate **only** `transform` and `opacity` (+ `background/border-color` transitions).
  Never animate width/height/top/left. One sanctioned exception: the shadcn
  sidebar collapse transitions its gap/container width (200 ms linear, one-off
  structural move). Heavy scrollable content must stay out of per-frame layout
  during that move — transcripts use `content-visibility: auto` per message so
  only visible messages reflow. Progress fills scale with `transform: scaleX`,
  not width.
- Press feedback within 100 ms: buttons `scale(.98)`; rows tint with Accent Wash.
- Streaming: blinking caret + status chip; no bouncing dots, no pulse rings taller than the control.
- `prefers-reduced-motion: reduce` collapses all motion to ≤ 0.01 ms (existing rule — keep).

## 8. Accessibility Requirements

- **Focus:** always visible — `outline: 3px solid var(--ring)` + `outline-offset: 2px`
  via `:focus-visible`, or an opaque shadcn `ring-ring` (Signal Blue). Do not add
  opacity modifiers to focus rings; alpha blending drops the indicator below 3:1.
  Never remove focus styles.
- Body text ≥ 4.5:1; large text/UI indicators ≥ 3:1 (use §2 verified pairs).
- Keep the **skip link**, dialog focus trap (`useDialogFocus`), and `aria-live`
  announcements (composer submit, toasts — `polite`, errors `role="alert"`).
- Icon-only controls keep `aria-label` (the `IconButton` contract). `Toggle`/`Switch`
  keeps `role="switch"` + `aria-checked`.
- Tab order matches visual order; Escape closes every overlay; heading levels sequential.
- Status conveyed by color + label/icon, never color alone.
- Prefer accessible-name test queries (`getByRole`, `getByLabelText`) so restyles don't
  break tests.

## 9. Anti-Patterns (Banned)

- Inter, or any "generic AI SaaS" look: purple/indigo gradients, neon outer glows,
  glassmorphism soup, decorative blur.
- Pure `#000000` anywhere; pure-white-only systems without the green-gray structure.
- Emoji as icons — Lucide only, one stroke width (default 2, sizes 14–18 in chrome).
- Color-only status; placeholder-only form labels; removed focus rings.
- Text below 11px in new/refactored UI.
- Mono-only entire screens; tightened body tracking.
- Remote font/style CDNs; weakening CSP, sandbox, context isolation, or fuses for UI DX.
- Big-bang rewrites: migrate view-by-view, deleting BEM as you go.
- Fake metrics, filler copy ("Elevate your workflow"), invented uptime numbers.
- Raw hex in JSX/components — semantic tokens only.
- New spacing values off the 4/8 grid; new radii beyond the three tokens.
- Barrel-only icon imports are fine (lucide tree-shakes), but no other icon set.

## 10. Working Agreement

1. Tokens first (§2–§4), components second (§5), views third.
2. New primitives go in `src/components/ui/` (shadcn-owned; the full registry is
   installed — prefer an existing primitive over hand-rolling). Product
   compositions (IconButton, PageHeader, RunStatus, SkeletonRows, AppShell,
   Composer) live in `src/components/` outside it.
3. CSP guard: primitives that inject `<style>` elements at runtime (chart,
   sonner, Radix ScrollArea viewport) degrade or are unusable under the
   production `style-src 'self'` policy — check before adopting one; never
   relax the CSP.
4. Verify with: `pnpm --filter @grok-desktop/desktop typecheck && pnpm --filter @grok-desktop/desktop test && pnpm lint`.
