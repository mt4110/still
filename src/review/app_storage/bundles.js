// Query Launch Services for just the identifiers supplied by the Library scan.
// Reading an NSBundle's metadata does not launch its executable.
ObjC.import("AppKit");
function run(ids) {
    const workspace = $.NSWorkspace.sharedWorkspace;
    const result = {};
    for (const id of ids) {
        const url = workspace.URLForApplicationWithBundleIdentifier(id);
        if (!url || !url.path || !url.path.js) continue;
        const bundle = $.NSBundle.bundleWithURL(url);
        if (!bundle || bundle.bundleIdentifier.js !== id) continue;
        result[id] = {
            path: url.path.js,
            running: $.NSRunningApplication.runningApplicationsWithBundleIdentifier(id).count > 0
        };
    }
    return JSON.stringify(result);
}
