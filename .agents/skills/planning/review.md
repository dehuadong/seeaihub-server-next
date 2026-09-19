
# Plan Review

Identify the Proposal or existing work item being reviewed, its selected scope, and the requirements and design sections that apply. Review that planning set before implementation; record the result on that same work item.

Evaluate the planning set along three independent axes:

- **Standards** — fidelity to accepted ADRs, applicable RFCs, established domain language, and project constraints.
- **Spec** — coverage of the authoritative product or work contract, such as the Proposal, PRD, approved Spec, or equivalent project artifact.
- **Architecture** — coherence of boundaries, responsibilities, dependencies, interfaces, and system relationships.

Report only material findings and trade-offs.

## Decision

Plan Review assesses the design within established decision authority. Record a pass, required revision or rejection; obtain any required external approval before treating a proposed decision as accepted.

Resolve findings in the artifact that owns them.

Existing contracts, accepted decisions, and established constraints may directly resolve questions already governed by them.

Material choices requiring an unresolved product, business, compatibility, cost, risk, architectural, or other value judgment belong to the user unless that authority has been explicitly delegated.

When authority is delegated, make the decision and preserve the material reasoning.

For RFCs involved in the work:

- accepted design with required approval evidence → mark it accepted or approved using the project's RFC convention
- rejected design → mark it rejected using the project's convention
- revision required → keep it non-authoritative until reviewed again

When an accepted decision creates or changes a durable architectural constraint, preserve it through `domain-modeling` as an ADR when warranted.

## Gate

The planning set passes when:

- Standards, Spec, and Architecture have no unresolved material findings
- required decisions are resolved
- RFCs required for implementation are accepted or approved
- accepted decisions are reflected in their authoritative artifacts
- durable decisions are preserved when warranted
- implementation can proceed without inventing material product behavior or architecture

Record the review outcome and supporting decision references on the identified Proposal or existing work item. When the review passes, required approvals are obtained and no blocker prevents the selected scope from starting, update that work item's readiness using the configured workflow. A review pass does not itself grant approval or execution authorization; referenced artifacts retain their own approval conventions.

If it does not pass, keep the work in planning, revise the owning artifact, and review again.

Routine implementation details may remain open.
