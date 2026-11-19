#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
    printf 'Usage: %s <still> <still-desktop> <major.minor.patch> <output.app>\n' "$0" >&2
    printf 'Set STILL_SIGNING_IDENTITY to a Developer ID Application certificate name for distribution signing.\n' >&2
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
identity=${STILL_SIGNING_IDENTITY--}
if [[ $identity != - && $identity != 'Developer ID Application: '* ]]; then
    printf 'STILL_SIGNING_IDENTITY must be a Developer ID Application certificate name, or - for a local build.\n' >&2
    exit 2
fi
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
mkdir -p "$bundle/Contents/Resources/ja.lproj" "$bundle/Contents/Resources/en.lproj"
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
    <key>CFBundleDevelopmentRegion</key>
    <string>ja</string>
    <key>CFBundleLocalizations</key>
    <array><string>ja</string><string>en</string></array>
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

cat > "$bundle/Contents/Resources/ja.lproj/InfoPlist.strings" <<'EOF'
"NSDesktopFolderUsageDescription" = "開始したスキャンで、デスクトップ内のファイルの容量と日時を確認します。";
"NSDocumentsFolderUsageDescription" = "開始したスキャンで、書類内のファイルの容量と日時を確認します。";
"NSDownloadsFolderUsageDescription" = "開始したスキャンで、ダウンロード内のファイルの容量と日時を確認します。";
EOF
cat > "$bundle/Contents/Resources/en.lproj/InfoPlist.strings" <<'EOF'
"NSDesktopFolderUsageDescription" = "still checks file sizes and dates on your Desktop during a scan you start.";
"NSDocumentsFolderUsageDescription" = "still checks file sizes and dates in Documents during a scan you start.";
"NSDownloadsFolderUsageDescription" = "still checks file sizes and dates in Downloads during a scan you start.";
EOF

plutil -lint "$bundle/Contents/Info.plist"
plutil -lint "$bundle/Contents/Resources/ja.lproj/InfoPlist.strings" "$bundle/Contents/Resources/en.lproj/InfoPlist.strings"
signing_options=(--force --sign "$identity")
if [[ $identity != - ]]; then
    signing_options+=(--options runtime --timestamp)
fi
codesign "${signing_options[@]}" "$bundle/Contents/MacOS/still"
codesign "${signing_options[@]}" "$bundle"
codesign --verify --strict "$bundle/Contents/MacOS/still"
codesign --verify --deep --strict "$bundle"
if [[ $identity != - ]]; then
    for executable in "$bundle/Contents/MacOS/still" "$bundle"; do
        signature=$(codesign -dv --verbose=4 "$executable" 2>&1)
        if ! printf '%s\n' "$signature" | grep -Fq "Authority=$identity" ||
            ! printf '%s\n' "$signature" | grep -Eq 'flags=.*\(.*runtime.*\)' ||
            ! printf '%s\n' "$signature" | grep -q '^Timestamp='; then
            printf 'Could not verify the requested Developer ID signature, hardened runtime, and secure timestamp.\n' >&2
            exit 1
        fi
    done
fi
mv -n "$bundle" "$output_parent/"
if [[ -e $bundle ]]; then
    printf 'The app output already exists: %s\n' "$output" >&2
    exit 1
fi
printf '%s\n' "$output"
