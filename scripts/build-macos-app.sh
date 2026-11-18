#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
    printf 'Usage: %s <still> <still-desktop> <major.minor.patch> <output.app>\n' "$0" >&2
    exit 2
fi
if [[ $(uname -s) != Darwin ]]; then
    printf 'Build the macOS app on macOS.\n' >&2
    exit 1
fi

cli=$1
desktop=$2
version=$3
output=$4
[[ $cli == /* ]] || cli="$PWD/$cli"
[[ $desktop == /* ]] || desktop="$PWD/$desktop"
[[ $output == /* ]] || output="$PWD/$output"

if [[ ! -f $cli || ! -x $cli || ! -f $desktop || ! -x $desktop ]]; then
    printf 'Select executable files for both still and still-desktop.\n' >&2
    exit 2
fi
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ || $output != *.app ]]; then
    printf 'Use a three-part numeric version and an output path ending in .app.\n' >&2
    exit 2
fi
if [[ -e $output || -L $output ]]; then
    printf 'The app output already exists: %s\n' "$output" >&2
    exit 1
fi

cli_archs=$(lipo -archs "$cli" | tr ' ' '\n' | sort)
desktop_archs=$(lipo -archs "$desktop" | tr ' ' '\n' | sort)
if [[ $cli_archs != "$desktop_archs" ]]; then
    printf 'The command-line and desktop executables must have matching architectures.\n' >&2
    exit 1
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
output_parent=$(dirname "$output")
stage=$(mktemp -d "$output_parent/.still-app.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
bundle="$stage/${output##*/}"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$cli" "$bundle/Contents/MacOS/still"
cp "$desktop" "$bundle/Contents/MacOS/still-desktop"
cp "$repo_root/LICENSE" "$bundle/Contents/Resources/LICENSE"

cat > "$bundle/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.mt4110.still</string>
    <key>CFBundleName</key>
    <string>still</string>
    <key>CFBundleExecutable</key>
    <string>still-desktop</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSDesktopFolderUsageDescription</key>
    <string>still checks file sizes and dates on your Desktop during a scan you start.</string>
    <key>NSDocumentsFolderUsageDescription</key>
    <string>still checks file sizes and dates in Documents during a scan you start.</string>
    <key>NSDownloadsFolderUsageDescription</key>
    <string>still checks file sizes and dates in Downloads during a scan you start.</string>
</dict>
</plist>
EOF

plutil -lint "$bundle/Contents/Info.plist"
codesign --force --sign - "$bundle/Contents/MacOS/still"
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
mv -n "$bundle" "$output_parent/"
if [[ -e $bundle ]]; then
    printf 'The app output already exists: %s\n' "$output" >&2
    exit 1
fi
printf '%s\n' "$output"
