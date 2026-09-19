---
name: implement
description: Implement authorized work after the project's Implementation Gate passes, including implementation review and preparation for final verification.
---

# Implement

## 1. Entry conditions

Implement the selected scope of the identified Proposal or existing work item. Resolve its issue ID, URL or file path and the requirements and design sections it references before coding.

Before implementation, check the scope and acceptance conditions, required decisions and review/approval evidence, unresolved blockers, and execution authorization for that scope under project rules. Reuse valid evidence without requesting confirmation again. Sync already agreed scope and decisions to their existing owners; unresolved material decisions return to the project's Planning stage. Document status or location alone does not establish readiness or authorization.

For a review-only request, use the `code-review` skill without entering implementation or requiring its gate. Deliver findings and coverage limitations; do not automatically fix changes or advance their workflow status. The implementation correction and completion rules apply only to authorized implementation.

## 2. Decompose when needed

After the Implementation Gate passes, create implementation tickets only when decomposition is needed. Their existence is not a condition for initially entering implementation.

Implement directly when the work fits one coherent execution unit.

Read [tickets.md](./tickets.md) when the work is too large or has meaningful independent slices or dependencies.

Do not create tickets merely to enumerate coding steps.

## 3. Execute

Implement against the selected work item's scope, acceptance conditions, referenced requirements, accepted designs and architectural decisions.

Use established architecture, conventions, interfaces, and domain language unless the authoritative design explicitly changes them.

If implementation exposes a material unresolved product, domain, architectural, interface, compatibility, or other contract decision, return to the project's Planning stage rather than inventing it during implementation.

### Implementation-time testing

Write or update tests as appropriate for the change. Use test-first development when it improves feedback or helps establish behavior clearly.

Run focused tests and relevant static checks while working.

## 4. Implementation Review

Use the `code-review` skill to review the implementation. Supply the review baseline, selected scope, governing work item and referenced requirements/design, including relevant uncommitted and new files. The skill owns the review method and report.

### Resolve findings and re-review

During authorized implementation, resolve material findings by ownership:

| Finding                                                           | Route                                  |
| ----------------------------------------------------------------- | -------------------------------------- |
| Current implementation is missing or violates the agreed contract | Fix in the current implementation      |
| Additional execution unit is needed within the same agreed work   | Add or update an implementation ticket |
| Product or technical contract must change or was never resolved   | Return to the project's Planning stage                       |
| Issue is unrelated to the agreed work                             | Do not expand scope automatically      |

There is no "fix it while we're here" path for unrelated work.

For implementation corrections, return to §3, rerun affected checks, and re-review the corrected scope under §4. Resolve material in-scope findings before final verification.

## 5. Completion and handoff

Implementation is complete when:

- the authorized scope is implemented
- required tickets are complete
- relevant implementation-time checks pass
- material Implementation Review findings are resolved
- the change is ready for final verification

For authorized end-to-end work, continue to the project's Verify stage after these conditions hold; do not wait for another instruction to verify.

Update the identified work item's implementation progress using the project's workflow convention. Keep final delivery completion and any decision-record transition to `implemented` for successful final verification.
