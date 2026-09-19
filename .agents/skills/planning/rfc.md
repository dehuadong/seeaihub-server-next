# RFC

Use an RFC when meaningful technical decisions must be resolved before implementation.

Turn the established product or work contract into a sufficiently resolved technical design.

## Lifecycle

Use the project's artifact registry for RFC location and historical ownership. An independent RFC does not move with a linked change record's delivery lifecycle.

Use the project's existing RFC lifecycle when one exists.

Do not introduce a competing status vocabulary.

When no convention exists, local RFCs may use:

    ---
    status: Draft
    date: YYYY-MM-DD
    proposal: <proposal or work reference>
    ---

with these default states:

- **Draft** — under design or awaiting review or decision; not an implementation basis.
- **Accepted** — reviewed and approved; may be used as an implementation basis.
- **Rejected** — considered and rejected; retained as historical context.
- **Superseded** — previously accepted but replaced by a later authoritative design or decision.

An RFC begins non-authoritative and becomes an implementation basis only when its project-defined lifecycle says it has been accepted or approved.

When an accepted RFC is replaced, preserve the historical relationship to its replacement.

## Design

Focus on decisions that materially constrain implementation, such as:

- system boundaries and responsibilities
- interfaces, protocols, schemas, or persistence
- lifecycle and failure behavior
- compatibility or migration
- material security, reliability, performance, or resource constraints
- meaningful verification seams

Do not prescribe routine implementation details that can safely be decided while coding.

## Design decisions

Resolve ordinary technical choices directly when they remain within the agreed contract, established architecture, and project constraints.

When a material choice has meaningful alternatives, recommend the preferred approach and preserve enough reasoning for review.

Do not ask the user to choose between alternatives merely because multiple technically viable approaches exist.

Escalate when the choice requires an unresolved:

- product behavior or scope decision
- business preference
- compatibility commitment
- cost or operational trade-off
- risk tolerance
- durable architectural direction
- other value judgment not already established

The user owns these unresolved decisions unless they have explicitly delegated that authority.

When authority is delegated, make the decision and preserve the material reasoning.

Document alternatives only when they materially explain the recommended design.

## Decision record

When review or external decision materially affects the RFC, preserve the decision trace concisely.

For example:

    ## Decision

    - Review: Standards / Spec / Architecture — passed
    - Authority: User | Delegated | Existing constraint
    - Decision: <reference, when applicable>
    - Accepted: YYYY-MM-DD
    - ADR: <reference, when warranted>

Reference authoritative discussion or decision artifacts rather than copying them into the RFC.

## Durable decisions

When an accepted decision establishes or changes durable domain language or an architectural constraint whose reasoning should outlive the RFC, use `domain-modeling` to preserve it as an ADR when warranted.

Keep feature-specific design in the RFC.

Do not duplicate the full RFC in an ADR.

## Ready for review

The RFC is ready for Plan Review when:

- the proposed design is sufficiently resolved
- material alternatives have a recommendation where needed
- unresolved decisions requiring external authority are explicit
- verification expectations are sufficiently clear
- implementation would not need to rediscover a material technical decision

Routine implementation details should remain open.

Being ready for review does not itself make the RFC approved or authoritative.
