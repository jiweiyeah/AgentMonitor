# Dashboard Jump And Session Query Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a usable Dashboard-to-Sessions jump flow and make Sessions filtering/sorting more useful for day-to-day monitoring.

**Architecture:** Keep the change inside existing TUI state and event handling. Add small pure helpers for process-to-session matching and query-style session filtering so we can test behavior without spinning the full terminal loop.

**Tech Stack:** Rust, ratatui, crossterm, existing inline unit tests

---

### Task 1: Define the target behaviors with failing tests

**Files:**
- Modify: `crates/agentmonitor/src/app.rs`
- Test: `crates/agentmonitor/src/app.rs`

**Step 1:** Add tests for structured session filters such as `agent:codex`, `status:active`, and mixed free text + structured tokens.

**Step 2:** Add a test for `tokens` sort so Sessions can rank by total token volume.

**Step 3:** Add a test for process-to-session matching that prefers same-agent + same-cwd and the freshest active session.

### Task 2: Implement the minimal matching and query logic

**Files:**
- Modify: `crates/agentmonitor/src/app.rs`
- Modify: `crates/agentmonitor/src/adapter/types.rs`

**Step 1:** Extend session filtering to parse whitespace-separated query terms, with fielded prefixes and substring fallback.

**Step 2:** Add `TokensDesc` sort and wire it into the existing sort cycle.

**Step 3:** Add a helper that maps a process row to the best candidate session by agent + cwd + recency.

### Task 3: Wire the new behavior into the TUI

**Files:**
- Modify: `crates/agentmonitor/src/event.rs`
- Modify: `crates/agentmonitor/src/tui/render.rs`
- Modify: `crates/agentmonitor/src/i18n.rs`

**Step 1:** Handle `Enter` on Dashboard by switching to Sessions and selecting the matched session.

**Step 2:** Update footer and filter hints so the new behavior is discoverable.

### Task 4: Refresh docs and verify

**Files:**
- Modify: `README.md`

**Step 1:** Update the tab/shortcut docs to match the current UI and new Dashboard jump behavior.

**Step 2:** Run focused tests, then full library tests, then clippy.
