---
name: code-review
description: "Review changes since a fixed point along two axes: Standards (project rules and current decisions) and Spec (the governing work item's acceptance contract and applicable independent Spec). Runs both reviews in parallel sub-agents and reports them side by side. Use for a branch, PR, work-in-progress changes, or a review since a named point."
---

Two-axis review of the requested change scope, including uncommitted work when applicable:

- **Standards**: does the change conform to this repo's documented standards and current authoritative decisions?
- **Spec**: does the change satisfy the governing work item's scope and acceptance conditions, including any applicable independent Spec?

Both axes run as **parallel sub-agents** so they don't pollute each other's context, then this skill aggregates their findings.

Resolve work tracking and document ownership from project instructions. A particular tracker configuration path or separate Spec file is not required for a review.

## Process

### 1. Pin the fixed point

Use the fixed point supplied by the user or calling implementation workflow. Reuse the baseline already recorded in the conversation; ask only when the intended scope cannot be established from available context.

Capture the exact review commands once. For committed branch changes use `git diff <fixed-point>...HEAD`; for implementation worktree changes compare the recorded baseline to the working tree and separately inspect scoped untracked files. Include staged and unstaged changes, excluding pre-existing user changes and other tasks. Also record relevant commits. When existing changes overlap, inspect attribution rather than discarding the entire file.

Before dispatch, resolve the fixed point and verify the complete scope, including new files. If it is empty, report no changes to review; do not mistake an empty committed diff for an empty worktree.

### 2. Identify the acceptance contract

First reuse the governing work item, acceptance conditions and applicable design already supplied by the caller or conversation. Otherwise:

1. Resolve a work identifier or path from the user's request, branch or relevant commits; locate it through the project's configured tracker or existing work-item convention.
2. Read that work item's selected scope and acceptance conditions, then follow its references to the applicable revision and sections of any independently owned Spec. Use the registered document owner to resolve design decisions; do not infer a Spec solely from a matching filename.
3. For direct changes without a separate work item, use the authorized user request and settled decisions in the conversation as the contract. If the contract still cannot be identified, ask the user for its owner. Skip the Spec axis only when no reliable acceptance contract exists, and report that coverage gap.

### 3. Identify the standards sources

Read project instructions and the document ownership entry, then relevant writing and coding standards such as `CODING_STANDARDS.md` or `CONTRIBUTING.md`. Include the applicable design decisions and constraints identified by the project configuration and governing work item. Report missing or conflicting required inputs as review coverage gaps.

On top of whatever the repo documents, the Standards axis always carries the **smell baseline** below: a fixed set of Fowler code smells (_Refactoring_, ch.3) that applies even when a repo documents nothing. Two rules bind it:

- **The repo overrides.** A documented repo standard always wins; where it endorses something the baseline would flag, suppress the smell.
- **Always a judgement call.** Each smell is a labelled heuristic ("possible Feature Envy"), never a hard violation. Like any standard here, skip anything tooling already enforces.

Each smell reads *what it is* → *how to fix*; match it against the diff:

- **Mysterious Name**: a function, variable, or type whose name doesn't reveal what it does or holds. → rename it; if no honest name comes, the design's murky.
- **Duplicated Code**: the same logic shape appears in more than one hunk or file in the change. → extract the shared shape, call it from both.
- **Feature Envy**: a method that reaches into another object's data more than its own. → move the method onto the data it envies.
- **Data Clumps**: the same few fields or params keep travelling together (a type wanting to be born). → bundle them into one type, pass that.
- **Primitive Obsession**: a primitive or string standing in for a domain concept that deserves its own type. → give the concept its own small type.
- **Repeated Switches**: the same `switch`/`if`-cascade on the same type recurs across the change. → replace with polymorphism, or one map both sites share.
- **Shotgun Surgery**: one logical change forces scattered edits across many files in the diff. → gather what changes together into one module.
- **Divergent Change**: one file or module is edited for several unrelated reasons. → split so each module changes for one reason.
- **Speculative Generality**: abstraction, parameters, or hooks added for needs the spec doesn't have. → delete it; inline back until a real need shows.
- **Message Chains**: long `a.b().c().d()` navigation the caller shouldn't depend on. → hide the walk behind one method on the first object.
- **Middle Man**: a class or function that mostly just delegates onward. → cut it, call the real target direct.
- **Refused Bequest**: a subclass or implementer that ignores or overrides most of what it inherits. → drop the inheritance, use composition.

### 4. Spawn both sub-agents in parallel

**Standards sub-agent prompt** should include:

- The full diff command and commit list.
- The standards and current-decision sources found in step 3, **plus the smell baseline from step 3** pasted in full (the sub-agent has no other access to it).
- The brief: "Report, per file/hunk where relevant, (a) every place the diff violates a documented standard or current authoritative decision: cite the source and rule; and (b) any baseline smell you spot: name it and quote the hunk. Distinguish hard violations from judgement calls: documented rules and current decisions can be hard, but baseline smells are always judgement calls, and a documented repo standard overrides the baseline. Skip anything tooling enforces. Under 400 words."

**Spec sub-agent prompt** should include:

- The diff command and commit list.
- The governing work item or conversational acceptance contract and relevant sections of any applicable independent Spec.
- The brief: "Report: (a) requirements of the acceptance contract that are missing or partial; (b) behaviour in the diff that wasn't requested (scope creep); (c) requirements that look implemented but where the implementation looks wrong. Quote or identify the exact contract clause for each finding. Under 400 words."

If no reliable acceptance contract exists, skip the Spec sub-agent and note this in the final report.

### 5. Aggregate

Present the two reports under `## Standards` and `## Spec` headings, verbatim or lightly cleaned. Do **not** merge or rerank findings, because the two axes are deliberately separate (see _Why two axes_).

End with a one-line summary: total findings per axis, and the worst issue _within each axis_ (if any). Don't pick a single winner across axes: that's the reranking the separation exists to prevent.

## Why two axes

A change can pass one axis and fail the other:

- Code that follows every standard but implements the wrong thing → **Standards pass, Spec fail.**
- Code that does exactly what the issue asked but breaks the project's conventions → **Spec pass, Standards fail.**

Reporting them separately stops one axis from masking the other.
