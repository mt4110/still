# Desktop experience

## Purpose

Make the weekly cleanup loop legible: choose a scope, investigate its size,
understand a candidate, explicitly confirm an operation, and inspect the measured
result. The Mac desktop uses a persistent sidebar and stable list/detail layout.
The existing Rust/egui implementation and cleanup contracts remain in use.

## Layout and hierarchy

- A 190pt sidebar separates Overview, Workspace, Disk, Docker, Apps & SDKs and
  Old files. Navigation preserves the current workspace scan and selection.
- A compact header keeps Help at the right. Workspace/profile actions belong to
  the overview and workspace header, with rescan visible when a scan is loaded.
- White content and a neutral gray sidebar use blue for selection and the next
  review action. Destructive execution remains a separate explicit confirmation.
- Type sizes use a small set: page titles 26–28pt, section headings 17–22pt,
  body and controls 15pt, supporting labels 12–13pt. Buttons and input fields
  share a 36pt height and 12pt horizontal inset, with an 8pt control gap. Large numbers emphasize measured disk
  space and operation results, rather than every individual row.
- Project/candidate rows have fixed 52pt height. SDK rows have fixed 48pt height.
  Long paths truncate in the list and remain readable in the inspector or tooltip.
- Candidate details show the full path, the actual decision reason, and the
  regeneration impact. Finder and individual inspection are secondary actions.
- The selection summary and review action stay beneath the list. Narrow windows
  put details below the list instead of compressing the entire inspector.

## Interaction states

| State | Presentation and behavior |
| --- | --- |
| First launch | Actual volume capacity and a previous/default development folder with a scan action. |
| Scanning | Stable navigation, real file/folder counts and current path. No invented percentage. Scope-changing and cleanup actions are guarded while the job runs. |
| No matches | Search stays visible, with an explicit empty state. |
| Candidate selected | A short background transition acknowledges focus. The inspector explains the actual evidence. |
| Protected or held | Name and size retain normal contrast. Only the cleanup checkbox is disabled, with a readable reason in the inspector. |
| Final confirmation | Names, scope and effects appear before a separate destructive action. Cancelling keeps data intact. |
| Completed | Actual measured volume free-space change and individual results. SDK residue remains partial, including its remaining size. |
| Inspection failure | Show the failed operation and available details. Unknown state is not a successful cleanup or a verified unused item. |

## Motion

Hover and selection colors transition over at most 120ms. Disclosure animation
uses the same global duration. Row heights, sort order, paths and measured numbers
do not animate. Movement must not shift a click target during review. macOS
Reduce Motion is read at launch and sets transition duration to zero. Process
spinners identify work in progress without manufacturing a completion estimate.

## Truthfulness and safety

The visual concept informed the sidebar, list, inspector and footer. Its sample
numbers are not application data. The application deliberately uses its actual
producer/blocker reason and states that open files are checked again before
removal. A scan does not prove that a candidate is unused. Disk statistics and
Docker storage browsing remain read-only. Navigation cannot replace an active
cleanup job. Profiles, SDK checks, partial-removal accounting and the explicit
confirmation contracts remain authoritative.

## Validation boundary

Mac native rendering is checked in the real egui application through the desktop
accessibility tree and screenshots, including long names, protected items,
search/selection and confirmation cancellation. Browser rendering does not
represent this native application. Windows CI compilation/tests are a separate
check from native Windows usability. Distribution signing and notarization
remain separate from the visual redesign.
