# still TUI Guide

This document describes the default keybindings for `still tui`.

## Safety First (Non-Negotiable)

* **No background execution**: The TUI never runs destructive operations in the background.
* **No one-key execution**: A single accidental keypress must **never** trigger file operations.
* **Purge is not a TUI concept**: Purge is **Vault maintenance only** (`still vault purge`), and does not exist in the TUI action cycle.
* **Preview before apply**: Execution is always preceded by an explicit preview/confirmation step.
* **Escape hatch**: `Esc` and `Ctrl+C` always let you back out safely.

> In short: the TUI edits a plan (intent). IO happens only after explicit confirmation.

---

## Concepts

### Actions (Plan Edits Only)

Within the TUI, you can set an action per entry:

* **Keep**: no change (default safe state)
* **Quarantine**: move into the vault (safe containment)
* **Trash**: move to OS Trash (macOS)

These are **plan edits** only. They do not change the filesystem until you explicitly apply.

### Explainability

Each entry may show:

* **reason**: why it was selected (rule name + condition summary)
* **recommended_action**: suggested action by rules (optional)

Trust is built by showing *why*.

---

## Navigation (Safe)

* `j` / `k` : move down / up

---
## 4. Navigation & Shortcuts

| Key          | Action                                            |
| ------------ | ------------------------------------------------- |
| `j` / `Down` | Move selection down                               |
| `k` / `Up`   | Move selection up                                 |
| `Space`      | Cycle action (Keep -> Quarantine -> Trash)        |
| `/`          | **Filter**: Type to filter list by path/reason    |
| `s`          | **Sort**: Cycle sort (Size -> Age -> Path)        |
| `b`          | **Bulk Mode**: Apply action to all filtered items |
| `q`          | Quit (Saves changes to Plan)                      |

> [!NOTE]
> **No Destructive IO in TUI (candidate files)**
> The TUI only updates the **Plan**. No candidate files are moved, deleted, or touched while you are in the TUI.

## Action Assignment (Plan Editing)

These keys change the **selected entry’s action** (no IO):

*   `K` : set action to **Keep**
*   `Q` : set action to **Quarantine**
*   `T` : set action to **Trash**

Optional convenience (may be enabled depending on build):

*   `Space` : cycle actions **Keep → Quarantine → Trash → Keep**

    *   Note: Purge is never included.

> Rationale: we use **uppercase** letters for direct assignment to prevent accidental presses, though lowercase usually maps to the same in simple inputs. `Space` is the primary, safest interaction.

## Advanced Features (Phase 4)

### Filtering `/`
Press `/` to enter filter mode. Type to match path or reason.
*   `Esc` clears filter or exits filter mode.

### Sorting `s`
Press `s` to cycle sort order:
1. Size (Descending) - Default
2. Age (Oldest First)
3. Path (A-Z)

### Bulk Edit `b`
Press `b` to enter Bulk Edit mode.
* Applies to **currently filtered items only**.
* Options: `[K]eep All`, `[Q]uarantine All`, `[T]rash All`.
* Requires explicit confirmation.
