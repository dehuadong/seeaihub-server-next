---
name: planning
description: Use when creating or updating a Proposal, when the product contract needs further definition, or when material technical decisions must be resolved before implementation.
---

# Planning

Turn the current conversation and relevant project context into the minimum planning artifacts needed to establish implementation readiness.

Planning is synthesis and decision completion, not a mandatory step for every engineering task.

## 1. Establish the work item and scope

Follow the artifact registry referenced by project instructions, or `docs/agents/artifacts.md` when present, for historical/current owners, destinations and lifecycle. Missing registration does not block planning; reuse established project conventions.

Identify the Proposal or existing work item for this planning effort by its issue ID, URL, or file path. It owns the current goal, scope, acceptance conditions and decision status, directly or through explicit references to requirements and design. A product-wide Spec alone does not identify which change is being planned.

Reuse a suitable existing work item; create a Proposal when a new planning root is needed. Separate PRDs, Specs and RFCs are optional; a clearly identified work item and sufficient implementation requirements are not.

Read [proposal.md](./proposal.md) when creating or updating a Proposal.

## 2. Complete necessary planning

Use the following branches only where needed; they do not require a separate artifact for every heading.

### Product requirements

Read [prd.md](./prd.md) when the product contract needs further definition; refine its existing owner and create a separate PRD only when requested or independent ownership is needed.

### Technical design

Read [rfc.md](./rfc.md) when meaningful technical decisions must be resolved before implementation.

Use established domain language and architectural decisions as project context.

Use `domain-modeling` when planning establishes or changes durable domain knowledge or architectural decisions.

Do not create artifacts merely to complete a sequence.

### Decision authority

Resolve ordinary technical choices autonomously when they remain within the agreed contract, established architecture, and project constraints.

Escalate when a material choice requires an unresolved product, business, compatibility, cost, risk, architectural, or other value judgment.

The user owns such unresolved decisions unless they have explicitly delegated that authority.

When authority is delegated, make the decision and preserve the material reasoning where appropriate.

## 3. Plan Review

After drafting the necessary planning content, use [review.md](./review.md) to perform Plan Review.

### Resolve findings and re-review

Return affected content to §2, resolve material findings in the artifact that owns them, and review again. Continue independent planning while a user-owned decision is pending; report unresolved blockers without claiming the planning set passed.

## 4. Completion and handoff

Planning completes when required decisions are resolved and the planning set passes Plan Review.

For a planning-only request, deliver the planning result and review evidence, then end the task. For end-to-end work, continue to the project's Implementation Gate when ready.

Check execution authorization separately under the project's Implementation Gate before entering implementation; reuse authorization already granted for this scope.
