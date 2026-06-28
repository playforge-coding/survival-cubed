# Survival Cubed

### Docs

Refer to docs for information. After making any code edits, update them.

Build the docs with `zensical build --strict`.

Fonts are self-hosted, not loaded from the Google Fonts CDN. Before building,
download them with `python scripts/fetch-fonts.py` (only needed once, or when
the fonts change). The downloaded `docs/fonts/` and generated
`docs/stylesheets/fonts.css` are gitignored, so a fresh checkout must run the
script before its first build:

```
python scripts/fetch-fonts.py && zensical build --strict
```