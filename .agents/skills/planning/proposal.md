# Proposal

A Proposal proposes a change and is the root work item for that planning effort when no suitable existing work item serves this purpose. It owns the goal, scope, acceptance and decision status, and references detailed requirements and design where they have separate owners.

## Configuration and storage

Before creating or updating a Proposal, read the project's issue-tracker configuration referenced by project instructions, normally `docs/agents/issue-tracker.md`. It must define where Proposals live and how their work status and review/approval evidence are maintained. Local Markdown is a supported tracker when explicitly configured.

If this configuration is missing or does not resolve Proposal handling, ask the user to complete it. Continue discussion and drafting in the conversation; do not invent a storage location, publish the Proposal or set its workflow status before configuration is established.

Reuse the existing Proposal and follow the artifact registry for historical ownership. A registration change does not migrate existing work. Create one Proposal in the configured location, not both a tracker issue and a duplicate local document.

## Contents and format

Follow the project's existing Proposal format and relevant maintained examples. When no project format exists, use [proposal-template.md](./proposal-template.md) as a body outline; status fields still come from tracker configuration.

Keep the content proportional to the work:

- **Goal and scope** — the intended change, reason, included work and important exclusions.
- **Acceptance** — observable completion conditions, or precise references to the applicable requirements.
- **Decisions** — material proposed choices, unresolved questions and the resulting review and approval evidence.
- **References** — existing PRD, Spec, RFC, ADR or other supporting artifacts.

A small Proposal may contain all necessary requirements and design. Create separate artifacts only when they need independent ownership. Reference their relevant sections instead of copying them or adding placeholder links.

## Status and updates

Use only the configured tracker's work states and evidence conventions. Record Plan Review results on this Proposal; transition it to ready only when readiness and required approvals are established. Execution authorization remains subject to project rules. A review result, approval and execution authorization are distinct facts.

Update the Proposal when agreed scope, decisions, review results, workflow status or supporting references change. Detailed requirements, design reasoning and ticket progress stay with their respective owners.

## Relationship to decision records

A decision record may reference the Proposal to preserve a durable choice or delivery rationale without duplicating the proposal. Its `proposed / implemented / rejected` delivery lifecycle does not set the Proposal's work status.

If an existing project uses one file for both purposes, follow its registered mapping of work status, approval evidence and record lifecycle. Do not add a competing `status` field or migrate historical Proposals automatically.
