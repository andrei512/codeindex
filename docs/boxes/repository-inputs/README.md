---
page-type: landing
---
# Repository inputs

Determine which paths belong to this build and supply one immutable view of each file version.

Owns repository discovery, ignore and tracked-file rules, file versions, binary classification, and source spans. A binary file enters path search. A text file also supplies bytes to its consumers. The source view stays valid while a parser, index builder, or snapshot writer holds it.

Project configuration and imported metadata can affect binding without being eligible search results. Capture the support inputs actually used and record their versions as dependencies. If required evidence is unavailable, binding reports the affected coverage.

## Topics

### Design

- [Discovery and capture](capture.md) — Defines eligibility, capture ownership and revalidation under checkout changes.
