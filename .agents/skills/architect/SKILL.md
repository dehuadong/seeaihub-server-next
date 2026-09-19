---
name: architect
description: Analyze cross-module responsibilities, dependencies and data flows, and evaluate architectural options and migration impacts. Use for unresolved architectural decisions involving module boundaries, interfaces, cross-module refactoring or migration plans; when implementing an approved plan, analyze only newly discovered architectural issues.
---

# Architectural Analysis

Establish who owns what, who depends on whom, and how data flows. Recommend the smallest effective change supported by evidence.

## Scope and authority

- This skill provides analysis and recommendations, not permission to implement code. Follow the user's authorization and the current project's applicable approval rules.
- Reuse approved plans; do not reopen design merely because a change spans multiple modules. When you find a conflict with the current contract, explain the evidence and the decision needed. Continue other independent, authorized work.
- When recording findings, follow the project's existing document ownership and organization. Do not automatically create a full set of decision artifacts.

## Establish the problem and evidence

Use the request and existing context to identify the decision, affected modules, expected benefits and behavior that must be preserved. Follow project instruction pointers, such as those in AGENTS.md, to relevant requirements, architecture documents, accepted decisions and interface contracts. Without such pointers, use the README, existing documentation and program entry points. Do not require particular document names, directories or technology stacks.

Read module configuration, public interfaces, callers and internal implementations as needed to trace calls, data flows and lifecycles that affect the boundaries in question. Focus on the unresolved problem; a whole-repository scan is not required. When the project specifies a reference implementation or compatibility standard, check the relevant behavior and existing verification records. Otherwise, do not introduce an additional reference implementation as an authority.

Distinguish the current contract, actual behavior in source code and unverified hypotheses. Explicitly identify discrepancies between documentation and code; the current implementation must not override an approved contract. Support performance or failure claims with measurements, reproductions or call paths. Keep claims hypothetical when evidence is insufficient.

Ask only when missing information would change the choice of solution and cannot be established independently. While awaiting an answer, continue analysis that does not depend on it.

## Analysis and trade-offs

Select dimensions that materially affect the problem rather than mechanically filling every checklist item:

| Dimension | What to examine |
| --- | --- |
| Responsibilities and dependencies | Whether responsibilities have clear owners, dependencies follow the agreed direction, and cycles or cross-layer access exist |
| Interfaces and compatibility | Which callers are affected and whether fields, errors, timing or compatibility commitments change |
| Data and lifecycle | Who owns state, how messages flow, and who handles cancellation, failure and resource cleanup |
| Coupling and complexity | Whether coordinated changes are reduced, new abstractions have actual consumers, and a smaller change is possible |
| Migration and verification | Whether delivery can be incremental, how transitional compatibility works, how to roll back or recover, and what evidence establishes acceptance |

When diagnosing a problem, trace observable symptoms to specific relationships between modules before proposing a correction. Do not treat a preferred design pattern as a root cause. When comparing options, include retaining the current approach or making a local adjustment. Compare only options that can meet the requirements; do not invent inferior alternatives to reach an option count.

Prefer an approach that respects current constraints, solves the actual problem and has reasonable migration costs. When evidence is insufficient to choose, provide a conditional recommendation and the smallest verification steps. Do not decide unresolved product behavior, public contracts or irreversible trade-offs on the user's behalf.

## Delivery and completion

Lead with the conclusion, followed by the key evidence and trade-offs. Answer simple questions briefly; use tables for alternatives and diagrams when complex dependencies or data flows warrant them. Do not impose a fixed template or word count.

Cover what the current decision needs:

- The recommendation and its basis, with key facts linked to documentation or source locations.
- Affected modules, interfaces and callers, plus any necessary migration sequence and verification approach.
- Risks that must be resolved, optional improvements, and matters requiring further evidence or a user decision.

Analysis is complete only when these points are supported by evidence, or missing evidence and its effect on the conclusion are explicit. Completing analysis does not mean the proposal is approved or implementation is complete.
