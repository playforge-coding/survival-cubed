---
name: docs-build-command
description: How to build/validate the Survival Cubed docs site
metadata:
  type: project
---

The docs (`docs/`, configured by `mkdocs.yml`) are built and validated with
**`zensical build --strict`**, not mkdocs — `mkdocs` is not installed in this
environment, but `zensical` is (at `~/.local/bin/zensical`). Use `--strict` to
fail on broken links/nav after editing docs.

Per CLAUDE.md, docs must be updated after any code edit. Entity/creature pages
live under `docs/entities/` (one page per entity, related ones grouped), with
`docs/creatures.md` as the linked overview and the nav listing them under a
"Bestiary" section in `mkdocs.yml`.
