---
name: domain-modeling
description: Build and sharpen a project's domain model. Use when discussing codebase terminology, writing or editing a GLOSSARY.md, or recording a durable decision in its selected owner.
---

# Domain Modeling

Actively build and sharpen the project's domain model as you design. This is the *active* discipline: challenging terms, inventing edge-case scenarios, and writing the glossary and decisions down the moment they crystallise. (Merely *reading* `GLOSSARY.md` for vocabulary is not this skill: that's a one-line habit any skill can do. This skill is for when you're changing the model, not just consuming it.)

## File structure

Follow the document ownership entry referenced by project instructions for glossary paths and decisions applicable to the model being discussed. If a required input's owner or scope is unclear, report the configuration gap.

Resolve the repository management root from project instructions, using the Git top-level only as a fallback. Working inside a subproject does not change it. A root `GLOSSARY-MAP.md` selects domain documents when the repository has multiple contexts. Do not create a separate record system per context or derive record categories automatically from contexts.

A single-context project may use a root `GLOSSARY.md`. A root `GLOSSARY-MAP.md`, when present, points to the selected contexts' glossaries; context paths need not be under `src/`. Resolve decision paths separately from the document ownership entry and, for Agent Notes, its configured record roots. Create a glossary or decision record only when there is content for its selected owner. Use the `adr` skill when a separate architecture decision record is warranted.

## During the session

### Challenge against the glossary

When the user uses a term that conflicts with the existing language in `GLOSSARY.md`, call it out immediately. "Your glossary defines 'cancellation' as X, but you seem to mean Y. Which is it?"

### Sharpen fuzzy language

When the user uses vague or overloaded terms, propose a precise canonical term. "You're saying 'account': do you mean the Customer or the User? Those are different things."

### Discuss concrete scenarios

When domain relationships are being discussed, stress-test them with specific scenarios. Invent scenarios that probe edge cases and force the user to be precise about the boundaries between concepts.

### Cross-reference with code

When the user states how something works, check whether the code agrees. If you find a contradiction, surface it: "Your code cancels entire Orders, but you just said partial cancellation is possible. Which is right?"

### Update GLOSSARY.md inline

When a term is resolved, update `GLOSSARY.md` right there. Don't batch these up: capture them as they happen. Use the format in [GLOSSARY-FORMAT.md](./GLOSSARY-FORMAT.md).

`GLOSSARY.md` should be totally devoid of implementation details. Do not treat `GLOSSARY.md` as a spec, a scratch pad, or a repository for implementation decisions. It is a glossary and nothing else.

### Record standalone architecture decisions sparingly

When a model change may warrant a separate architecture decision record, invoke the `adr` skill for eligibility and current owner. Its eligibility test applies to the separate record, not to other Agent Notes. Follow the target project's Agent Notes README for Note format and lifecycle, or the `adr` skill's independent-ADR reference when that owner is selected.
