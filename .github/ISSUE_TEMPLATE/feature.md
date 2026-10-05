---
name: Feature request
about: A change to scheduling, caps, the API surface, or the UI
title: ""
labels: enhancement
assignees: ""
---

**Which shell** (cli / server / worker / web) and the user story:

**Current behavior / workaround:**

**Proposal** (where does it live: core, or one adapter?):

Keep in mind the project's budgets: CLI binary < 512000 B, web first-load JS < 100 KB gzipped, worker bundle < 1 MB. Big new dependencies are unlikely to land.
