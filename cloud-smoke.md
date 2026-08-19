# Cloud adapter smoke test

Read-only smoke test of the cloud adapter (commit f9b0d47) against the real Cursor API.

- Date: 2026-08-19
- CURSOR_API_KEY present: yes
- Command: `murmur cloud list` (single GET /v1/agents)
- Exit status: 0 (success)
- Agents returned: 6
  - ARCHIVED: 4
  - ACTIVE: 2

No creating/prompting/cancelling endpoints were called.
