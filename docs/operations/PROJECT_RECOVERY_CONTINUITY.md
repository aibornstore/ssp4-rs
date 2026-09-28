# ssp4-rs — Recovery & Continuity

Global ArtWeb standard:
`aibornstore/connect-canon/docs/ARTWEB_PROJECT_RECOVERY_CONTINUITY_STANDARD_V1.md`

The repository is linked to the common recovery contract; runtime channels and host paths are intentionally not guessed.

Before PASS: capture exact branch/HEAD/dirty state, define an independent recovery path where technically possible, create live STATUS/CONTINUITY artifacts, and runtime-certify controlled recovery.

On `Resume stream unavailable` or a tool interruption: read actual state first and continue only the incomplete step.

Current state: `STANDARD_LINKED_NEEDS_RUNTIME_CERTIFICATION`.
