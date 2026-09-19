---
name: verify
description: Use after implementation and Implementation Review to verify acceptance, correct in-scope failures, and establish the final engineering outcome.
---

# Verify

Complete the engineering workflow by establishing that the authorized work satisfies its authoritative contract. Verification includes in-scope correction and re-verification under the existing execution authorization.

## 1. Establish the acceptance contract and coverage

Resolve the governing work item and its approved requirements, acceptance conditions, design constraints, and compatibility expectations. Implementation is the subject of verification, not the source of expected behavior. Keep verification within the agreed contract.

For every required acceptance condition, choose the highest stable boundary that demonstrates it, such as user-visible behavior, a public API or CLI, or an integration or domain boundary. Use the smallest sufficient check; lower-level tests, builds, and static checks can support evidence where appropriate.

Reuse existing checks when they demonstrate the contract. Passing tests, CI, or code review alone does not establish acceptance; do not run checks merely because they exist.

## 2. Execute verification

Actually exercise the delivered behavior when practical. Track each required condition with its evidence and current result:

- **PASS** — evidence demonstrates the required behavior.
- **FAIL** — observed behavior contradicts the required behavior.
- **BLOCKED** — ambiguity or insufficient reliable evidence prevents a determination.

Absence of observed failure is not evidence of a pass. Continue checking independent conditions after finding a failure or blocker. Check established adjacent behavior when the change may affect it, keeping regression coverage proportional to impact and risk.

## 3. Diagnose, correct, and re-verify failures

For each failure, establish the contract expectation, trigger, observed behavior, and supporting evidence. Confirm the cause and owner before changing anything; record an unresolved cause explicitly rather than guessing:

- **Implementation defect** — fix the implementation.
- **Verification defect** — fix the incorrect check.
- **Contract ambiguity or defect** — return affected work to `planning`; material contract decisions require the appropriate approval.
- **Environment or tooling failure** — fix within authorization or report what prevents further verification.
- **Pre-existing failure** — record its acceptance impact and ownership; do not automatically expand the repair scope or disregard a required condition it prevents from passing.

A demonstrated contract violation remains FAIL even when its root cause is unresolved.

Route corrections through the project stage that owns them, including applicable implementation review, then return to §2 to re-verify affected conditions and relevant regressions. Revisit §1 if an authorized contract change alters the acceptance baseline. Continue independent authorized work while an issue is resolved.

A failed check is an intermediate result, not a reason to end authorized end-to-end work. Continue correction and re-verification while actionable within the agreed scope and authorization. If progress requires an unavailable dependency, a user-owned decision, or additional authorization, report the remaining failure or blocker and the specific action needed.

Do not weaken the agreed contract merely to make verification pass.

## 4. Final result and completion

Lead with the final acceptance result, followed by unresolved failures and blockers prioritized by impact. Then give concise coverage and evidence for every required condition, completed corrections with re-verification evidence, relevant regression results, and material verification gaps or residual risks. A gap that prevents a reliable determination on a required condition must be reflected in that condition's BLOCKED result. A compact table or linked evidence is sufficient; a fixed long template is unnecessary.

Report the latest verified state: a corrected condition that passes re-verification is PASS, with its earlier failure summarized as a completed correction. Prioritize unresolved issues by impact and state their contract reference, expected and observed behavior, cause or uncertainty, and required next action.

Determine the overall result from the current required conditions:

- **PASS** only when every required condition passes.
- **FAIL** when any required condition still fails, even if others are blocked.
- **BLOCKED** when none fail but one or more cannot be reliably verified.

### Completion

Only final PASS completes the engineering work. Update the authoritative work item using the project's completion convention after successful verification. A finished verification attempt, implementation, or review alone does not justify marking work complete.
