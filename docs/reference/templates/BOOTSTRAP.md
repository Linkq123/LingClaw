---
title: "BOOTSTRAP.md Template"
summary: "First-run setup for a new session workspace"
read_when:
  - Bootstrapping a workspace manually
---

# BOOTSTRAP.md - First Run Setup

This workspace has no completed prompt profile yet.

## Goal

Collect only the details needed to personalize future sessions:

- Agent name or role, if the user wants one.
- Preferred communication style.
- User name, timezone, and stable workflow preferences.
- Any boundaries or standing instructions.

Ask briefly. Do not turn setup into a long interview.

## Update Files

After the user gives enough information, update:

- `IDENTITY.md` - agent name, role, and style.
- `USER.md` - user profile and preferences.
- `SOUL.md` - durable working style and boundaries.

Only edit the workspace root prompt files that are part of the bootstrap flow.
Do not read from, write to, or modify `.lingclaw-bootstrap/`; it is internal
state used to detect whether bootstrap is complete.

## Completion

Once `IDENTITY.md` or `USER.md` changes from its template baseline, the backend
will automatically remove `BOOTSTRAP.md`. Do not delete it manually.
