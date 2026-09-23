# Engineering Workflow

Continue engineering work after Discuss through:

```
§1 Planning → §2 Implementation Gate → §3 Implement → §4 Verify
```

Discuss, progression authority, and stage-skill mapping are defined in `AGENTS.md`.

This document defines downstream orchestration, gates, and return paths.

Judge stage completion by that stage's completion conditions; proactively complete its required review, corrections, and re-review. Before ending the task, check for remaining required and authorized work within the request's scope and continue through the workflow when it remains.

## 1. Planning

Use the `planning` skill when the user requests planning, including planning-only work, or when authorized end-to-end work would otherwise require inventing a material product, scope, domain, architectural, interface, compatibility, or other contract decision.

For end-to-end work, skip Planning when the work already satisfies the Implementation Gate's readiness conditions. An explicit planning request still receives its requested deliverable and applicable review, reusing existing artifacts and valid evidence.

Identify the Proposal or existing work item for the current change by its ID, URL or path. It defines the selected delivery scope and planning status, and points to the applicable product contract and technical design. Reuse it; a separate Proposal, Spec or RFC is needed only when its content needs a new owner.

When normative behavior needs an independent owner, use a Spec as defined in [docs/AGENTS.md](../AGENTS.md#文档分层). Its accepted revision directly guides implementation and verification. Small work without an independent Spec may keep the contract directly in the work item.

An RFC may begin as a non-authoritative feasibility draft before its Spec is stable when technical constraints can materially change product scope. Feed those findings back into the Spec. Only after that Spec is stable may the RFC be accepted as an implementation basis. One Spec may be carried by multiple RFCs.

Planning owns its planning artifacts and Plan Review. After drafting, proactively complete Plan Review, resolve material findings in their owning artifacts, and review affected content again. Planning is complete only when required decisions are resolved and Plan Review passes; a finished draft alone does not complete the stage.

If a later stage exposes a material unresolved contract decision, return to `planning`.

## 2. Implementation Gate

Enter implementation only when:

* the identified Proposal or existing work item specifies the selected implementation scope
* the applicable Spec revision has been reviewed and accepted, or the work item's equivalent contract is approved for small work; it states observable acceptance conditions
* each required RFC identifies the Spec revision and sections it carries, without unresolved omissions, scope expansion or contradiction
* necessary design decisions, required planning review and approvals are evidenced, with no material unresolved decision affecting this scope
* dependencies and blockers allow the selected scope to start
* execution authorization under repository rules covers that scope

Planning readiness and execution authorization are separate conditions.

Use the work item and its references as evidence; a status label or directory location alone does not pass the gate. Reuse valid review and authorization evidence. Before coding, sync agreed scope and decisions not yet reflected in their existing owners.

## 3. Implement

After the Implementation Gate passes, use the `implement` skill and follow its applicable sections through completion, then proceed to §4 Verify.

Do not stop at the first working implementation when authorized end-to-end work still requires review or verification.

## 4. Verify

After implementation and Implementation Review are complete, use the `verify` skill through its final result and completion conditions.

**Verify 的职责还包含记录收尾**：最终验证完成，即**按约定更新相关文档与记录的状态、归属**（文档归属见 [`docs/AGENTS.md`](../AGENTS.md)，Agent Note 生命周期见 [`.agents/notes/README.md`](../../.agents/notes/README.md)）。

Only successful verification completes the work.

### Review Convergence

Plan Review and Implementation Review must converge rather than iterate indefinitely.

Subsequent review rounds should focus on unresolved material findings, regressions introduced by their fixes, and newly discovered material issues that could not reasonably have been raised earlier.

Do not reopen an accepted decision without new material evidence, and do not block progress on stylistic preferences, optional improvements, speculative concerns, or unrelated issues.

Plan Review and Implementation Review each have an independent limit of 5 review-fix cycles. Their limits are counted separately and do not carry over between stages.

If material findings remain after the fifth cycle of that review, stop autonomous review and return the unresolved issue to its owning stage or decision authority. Reaching the limit does not make the review pass.

## Return Rule

Return affected work to the earliest stage that owns the unresolved issue.

Continue independent, ready, and authorized work while that issue is resolved.

Corrections within the agreed contract retain the existing execution authorization; resume from the owning stage without requesting it again.

Changes to the agreed contract must pass the applicable planning and implementation gates before affected implementation resumes.

Do not compensate for an upstream contract gap in implementation, and do not compensate for an implementation defect by weakening verification.
