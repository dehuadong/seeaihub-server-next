# Implementation Tickets

After entering implementation, decompose authorized work only when it cannot be executed coherently as one unit. Tickets are created by implementation as needed, not prerequisites for its initial gate.

Tickets are execution units, not additional design documents.

Do not use decomposition to reopen decisions already settled upstream.

## Storage

Follow the artifact registry for current and historical work-item owners and tracker configuration for operations. Existing work stays with its owner unless explicitly migrated; registration changes alone do not create replacement tickets.

Use the project's existing work-tracking mechanism.

Tickets may be:

- issues in an issue tracker
- local Markdown files
- another project-defined execution artifact

Reference the parent Proposal or existing work item by its ID, URL or path, using the project's native relationship mechanism when available.

## Tracer bullets

Prefer tracer-bullet vertical slices.

Each ticket should:

- deliver a narrow but complete behavior or coherent change
- cut through the layers required for that behavior
- be independently verifiable where practical
- fit within one fresh execution context
- leave the system in a coherent state
- declare genuine blocking dependencies

Prefer one behavior through the necessary layers over one architectural layer across many behaviors.

## Blocking edges

Declare only dependencies that genuinely prevent a ticket from starting.

Tickets with no unresolved blockers form the executable frontier.

Do not create artificial sequencing where work can proceed independently.

Independent frontier tickets may execute in parallel when safe and supported.

Re-evaluate the executable frontier as tickets complete.

## Wide refactors

Do not force broad mechanical changes into artificial vertical slices.

When necessary, prefer:

1. **Expand** — introduce the new form alongside the old.
2. **Migrate** — move consumers in coherent batches.
3. **Contract** — remove the old form after migration.

Represent genuine dependency edges explicitly.

## Ticket contents

Keep each ticket concise.

Include:

### Objective

The behavior or coherent change the ticket delivers.

### Acceptance criteria

Observable or verifiable conditions for completion.

### Blocked by

Only genuine blocking work.

Use the project's native dependency mechanism when one exists.

## Inheritance

Tickets inherit scope, requirements, design and applicable execution authorization from their identified parent work item and referenced artifacts.

This may include:

- Proposal
- PRD
- Spec
- RFC
- ADRs
- project constraints

Reference those sources rather than duplicating them.

Avoid detailed implementation recipes or decisions the implementer can safely make itself.

## Execution

Work the executable frontier.

Before starting a ticket, check its inherited scope, acceptance conditions and blocking dependencies. Reuse the parent's valid authorization for that scope without repeating the full planning gate. A new material decision returns affected work to the project's Planning stage; decomposition cannot supply a missing upstream decision.

Complete each ticket to a coherent, tested, and verifiable state before advancing dependent work.

Update and close completed tickets according to the configured tracker workflow.

Commit completed ticket work when the repository workflow expects ticket-level commits.

Continue until all tickets required by the authorized scope are complete. Ticket completion does not replace the whole change's [Implementation Review](./SKILL.md#4-implementation-review) or the project's final Verify stage; return the completed scope and evidence to the enclosing implementation workflow.
