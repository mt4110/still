# still

**Local storage explorer and review-first developer cleanup tool**

Browse directory sizes, search local paths, and review generated outputs, merged Git worktrees, and unused Docker resources. Cleanup actions require explicit confirmation.

## Features
- **Parallel storage scan**: Browse by total size or largest file and search recursively.
- **Library quarantine**: Move selected Library items to a restorable Vault.
- **Local by default**: GitHub PR lookup runs when explicitly requested with `--github-pr`.

## Privacy

**still does not send user data or usage information to its developers. It has no telemetry, usage analytics, or automatic crash reporting.** Scan results are processed on your device. Weekly profiles and scan history are stored locally.

**Find old files…** and `still stale` scan only within your home folder (`~/`). Click **Scan** with **Home (~)** selected to scan without picking folders individually. The scan respects OS permissions and reports how many entries could not be read. It reads paths, sizes, and timestamps without opening file contents. It skips hidden items, `Library`, and `node_modules`, and does not follow symlinks or junctions.

On macOS, workspace review is also limited to the home folder, including saved profiles and CLI scans. Paths such as `/`, `/tmp`, and `/nix/store` are rejected. On Windows, workspace review uses folders you specify. If you omit the folder from `still review`, it uses the working directory.

macOS manages Files & Folders access separately for protected locations such as Desktop, Documents, and Downloads. To focus on your development files, use **Choose folder…** to select a folder such as `~/_workspace`.

Docker and Kubernetes review use the selected local endpoints. The `--github-pr` option queries GitHub through `gh`.

## Installation

```bash
# Using Nix
nix develop
cargo build --release

# Using Cargo
cargo install --path .
```

### Desktop app (macOS / Windows)

The initial interface is Japanese. Use the language menu in the upper right to switch to English. **Help** explains candidate review, held items, deletion, permissions and privacy inside the app.

On the Mac, **Disk** in the header shows capacity for your home volume and separate read/write activity for the physical disks serving it. Sampling runs about every two seconds while this view is open. **Docker** lists local containers, images, volumes and build cache by reported size, with name/ID/state filtering and details for the selected row. Docker storage sizes can differ from actual SSD space recoverable.

Workspace scans report observed file/folder counts, logical bytes and the current directory before checking candidate safety. The [cleanup algorithm and product plan](docs/CLEANUP_STRATEGY.md) describes the proposed Library ownership checks and before/after evidence.

Open `still.app` on macOS or `stilll` from the Windows Start menu. Choose **Add workspace…**, sort folders by size, or search paths recursively. Choose **Save as…** to save a weekly profile. Profiles saved with `still review --save-profile` can also be opened here.

**Cleanup candidates** shows each candidate's size and reason. Select folders and choose **Review … selected** to see what will be affected. Nothing is removed until you choose **Delete selected** in the confirmation dialog. This action is permanent. **Candidate sources** lets you include dedicated script outputs, Git repositories, or a local Docker context. Git and Docker candidates can be inspected in the app and reviewed for removal through the dedicated CLI commands.

Filter candidates by path and use **Select all eligible** to select matching folders. An enclosing folder covers its nested candidates. **Scan … selected** inspects the selection. **Review … selected → Delete selected** confirms and removes up to two folders at a time. The active candidate list checks visible paths every ten seconds. On macOS, open an ancestor or selected path in Finder. **Find old files…** can rank matches by size, oldest modification, or oldest creation date.

On macOS, start the workspace scan from the home screen. The project list groups immediate child folders by usage, eligible candidate size, and change since the previous scan. Open a project to inspect its generated folders, see why items are held, recheck one item, or open it in Finder. The first scan sets the comparison baseline.

When a standard Cargo `target` is held, still checks the `incremental` and `deps` directories immediately under its debug and release profiles separately. Only caches with verified Cargo markers and recognized compiler entries are selectable. Certificates, logs, results, and unknown entries keep their containing cache on hold.

Relocated Cargo output appears when the scan finds build markers and compiled profiles. **Status** explains blockers such as missing project context, tracked files, symbolic links, and protected entries. The final removal check also checks for changes and files in use.

Use **Reasons** to switch blocker explanations between English and Japanese. **Find old files…** lists large files under your home folder by the latest available modification, access, or creation date, with date details and an open-file check where available. You can select a folder, change the minimum age and size, and filter the results by path. The CLI equivalent is `still stale --older-than-days 180 --min-mib 512`. Old files are shown for review; an access date or open-file check cannot prove a file is unused.

Local Docker review also lists unreferenced volumes as protected. To inspect completed jobs and pods from a local Kubernetes or K3s context, use `still kube --kubeconfig ~/kube-local.yaml --context my-local --namespace dev` or add the context in **Candidate sources**. Cluster inspection is read-only and requires a verified loopback HTTPS endpoint.

```sh
cargo run --locked --features desktop --bin still-desktop
```

Build a macOS bundle with [build-macos-app.sh](scripts/build-macos-app.sh), or an MSIX with [build-windows-msix.ps1](scripts/build-windows-msix.ps1). Use the Microsoft Store signed package on Windows with Smart App Control enabled.

To use the app on another Apple Silicon Mac, ZIP the bundle, transfer it, and place the extracted `still.app` in Applications. The destination does not need Rust or Nix. Create a weekly profile there with **Add workspace… → Save as…** so it uses that Mac's paths.

```sh
ditto -c -k --sequesterRsrc --keepParent target/still.app target/still-macos-arm64.zip
```

After the PR's **Rust CI → build (macos-15)** succeeds, download the app ZIP from **Artifacts → still-macos-arm64** on the workflow run. By default, local and CI bundles use ad-hoc signing for personal use and are not notarized.

For organization-managed Macs, explicitly select a **Developer ID Application** certificate in your Keychain and prepare a notarized ZIP. An `Apple Development` certificate is not accepted for this distribution mode.

```sh
STILL_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)' \
  scripts/build-macos-app.sh target/release/still target/release/still-desktop 0.2.0 target/still-signed.app
scripts/notarize-macos-app.sh target/still-signed.app still-notary target/still-notarized.zip
```

`still-notary` names credentials you have already registered with `notarytool` in your Keychain. The notarization script submits a copy of the app to Apple and publishes a ZIP after acceptance, ticket stapling, and Gatekeeper verification succeed. It preserves the input app and refuses to overwrite an existing output. Organization-specific security software may also require approval under that organization's policy. [Apple Developer ID](https://developer.apple.com/developer-id/), [Apple notarization guide](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)

On macOS, **App storage…** measures known Android NDK, Unity, Unreal, browser cache, Slack and Teams log, and Notion data locations under your home folder. It reads metadata without opening logs or databases. The optional Xcode checkbox queries its simulator runtime inventory. Use `still app-storage` or `still app-storage --xcode-runtimes` in the terminal. The CLI inventory is read-only. In the Mac app, close Chrome and choose **Review default profile HTTP cache…** to inspect its Default profile’s `Cache_Data` directory. A separate confirmation shows the scope, size and download impact. Results are recorded in `still cleanup-history`. **Keep in Library cleanup** also applies to future scans. Workspace review also identifies protected game assets, autosaves, engine files, and caches.

The optional Library inventory, also available as `still app-storage --library`, measures immediate folders in Caches, Logs, Application Support, Containers, and Group Containers. It reports exact registered bundle ID matches where available and skips links. Shared locations and unverified owners remain review-only. Entries may overlap known locations, so their sizes must not be added together.

Workspace review shows `node_modules` next to a `package.json` as protected, review-only storage. When an output is selected for removal, still reads regular-file contents to compare them again; contents are not stored or transmitted. Reviewing large outputs can take time, and the checks cannot guarantee detection of every concurrent change.

On the Mac, select **App storage… → Review Xcode & NDK versions…** to see each SDK version’s size and the reason it is available for review or protected. Review and confirm individual versions to uninstall them with their management tools. still checks the selected versions again before uninstalling and reports the actual change in free space on the Mac data volume.

After an Xcode uninstall, still also checks the usable runtime inventory and disk images for the selected build. When evidence remains, it shows **Partially remaining** and reports observed image usage separately from the measured change in volume free space.

## Usage: 3-Step Workflow

### Developer storage explorer

```sh
still review --root ~/projects
still review --cleanup-root ~/projects
still review --root ~/projects --search target --format json
```

Save a weekly profile with `still review --save-profile weekly --cleanup-root ~/projects` and reopen it with `still review --profile weekly`. With a previous scan available, `s` can sort directories by growth. Formatted reports read the same history without updating it.

Press Enter to open a directory, `/` to search, or `o` to choose another root. With a cleanup root configured, Tab shows generated outputs. Space checks and selects an output. `D` opens the deletion preview, and `y` confirms permanent removal. Use `--output` to declare a dedicated reproducible script output directory.

`still worktrees --help` and `still docker --help` show the options for the other cleanup sources.

### Library workflow

The standard workflow for safe cleanup.

### 1. Scan
Scans directories to identify cleanup candidates and generates a "Plan File (`plan.json`)". No files are modified at this stage.

```bash
# Scan with default safe settings
still scan

# List available scopes
still scan --list-scopes
```

### 2. Review (TUI)
Open the generated plan in the TUI to decide actions for each item.

```bash
still tui
```

- **Controls**:
    - `j` / `k`: Navigate
    - `Space`: Cycle Action (Keep -> Quarantine -> Trash)
    - `b`: Bulk Mode (Apply to all filtered items)
    - `q`: Save and Quit

### 3. Apply
Executes the actions (Move to Vault / Trash) selected in the TUI.

```bash
still apply
```

- **Quarantine**: Moves files to `~/Library/Application Support/still/vault`. Can be restored later.
- **Trash**: Moves files to the macOS system Trash.
- **Keep**: Does nothing.

### Vault Management
Manage items in the quarantine vault.

```bash
# List quarantined items
still vault list

# Restore an item
still vault restore <ID>

# Purge expired items (Permanent Deletion)
still vault purge --expired
```

## License
Source Available License.
Commercial redistribution and public distribution of forks are prohibited. Modifications are permitted only as PRs to this repository.
See [LICENSE](./LICENSE) for details.

### Reviewing leftover stages and Library storage on macOS

After a partial workspace cleanup, refresh the scan and open **Review leftover stages…** from the workspace menu. Inspect the stage, then confirm removal if it contains only empty directories, recognizable Finder metadata and the staging note. The CLI equivalent is `still stage-cleanup --cleanup-root <workspace> <stage>` to preview, with `--apply` to confirm.

The optional Library inventory under **Apps & SDKs → App storage** shows exact registered bundle ID matches, recorded file dates, and changes since the previous desktop Library scan. Logs, databases and shared containers stay available for read-only inspection. The app continues to use the existing local profile, history and Vault directories.
