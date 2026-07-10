# Tray assets

These notification-area icons use the canonical Grok `G` from
`release/windows/assets/icon.svg`, simplified to a single path on a transparent
background. `dark` is the white glyph for dark system panels; `light` is the
brand-charcoal glyph for light system panels.

The canonical sources are `tray-dark.svg` and `tray-light.svg`. Raster and
multi-resolution Windows variants are generated deterministically with
ImageMagick 7:

```sh
node apps/desktop/assets/tray/generate-assets.mjs
node apps/desktop/assets/tray/validate-assets.mjs
```

The generator fixes the source epoch, strips metadata, forces 8-bit RGBA PNGs,
and builds each ICO from its 16, 20, 24, and 32 pixel variants. Generated files
are checked in so production packages do not require ImageMagick.
