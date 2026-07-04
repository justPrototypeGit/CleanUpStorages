# Future ideas / next-version backlog

Ideas captured for later versions of CleanUpStorages. These are **not** part of the current approved design
(`docs/superpowers/specs/2026-07-04-cleanupstorages-design.md`) and are intentionally deferred. When an idea is
picked up, it graduates into its own design spec via the normal brainstorming → spec → plan flow.

## Semantic analysis of items

- **Local person recognition** in photos — group/label images by the people in them, running **locally**
  (on-device, no cloud upload) to keep personal data private. Would let the register answer questions like
  "show me all photos of person X across every drive."
- More broadly: semantic/content-based understanding of items (not just filename/hash), e.g. content tagging,
  visual similarity, topic classification for documents — feeding richer search and organization.

_(Raised 2026-07-04. Privacy is a hard requirement: any such analysis must run locally.)_
