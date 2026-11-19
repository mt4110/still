#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
    printf 'Usage: %s <signed.app> <keychain-profile> <output.zip>\n' "$0" >&2
    exit 2
fi
if [[ $(uname -s) != Darwin ]]; then
    printf 'Notarize the macOS app on macOS.\n' >&2
    exit 1
fi

input=$1
profile=$2
output=$3
[[ $input == /* ]] || input="$PWD/$input"
[[ $output == /* ]] || output="$PWD/$output"
if [[ $input != *.app || ! -d $input || -L $input || -z $profile || $output != *.zip ]]; then
    printf 'Select a signed .app, an existing notarytool Keychain profile, and an output path ending in .zip.\n' >&2
    exit 2
fi
if [[ -e $output || -L $output ]]; then
    printf 'The ZIP output already exists: %s\n' "$output" >&2
    exit 1
fi

output_parent=$(dirname "$output")
stage=$(mktemp -d "$output_parent/.still-notary.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
bundle="$stage/${input##*/}"
ditto "$input" "$bundle"
codesign --verify --deep --strict "$bundle"
for executable in "$bundle/Contents/MacOS/still" "$bundle"; do
    codesign --verify --strict "$executable"
    signature=$(codesign -dv --verbose=4 "$executable" 2>&1)
    if ! printf '%s\n' "$signature" | grep -q '^Authority=Developer ID Application: ' ||
        ! printf '%s\n' "$signature" | grep -Eq 'flags=.*\(.*runtime.*\)' ||
        ! printf '%s\n' "$signature" | grep -q '^Timestamp='; then
        printf 'Use Developer ID Application signatures with hardened runtime and secure timestamps before notarizing.\n' >&2
        exit 1
    fi
done

submission="$stage/submission.zip"
result="$stage/result.json"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$submission"
if ! xcrun notarytool submit "$submission" --keychain-profile "$profile" \
    --wait --timeout 20m --output-format json > "$result"; then
    submission_id=$(plutil -extract id raw "$result" 2>/dev/null || true)
    if [[ -n $submission_id ]]; then
        printf 'Notarization submission: %s\n' "$submission_id" >&2
    fi
    printf 'Notarization did not complete. Review the submission in notarytool before retrying.\n' >&2
    exit 1
fi
status=$(plutil -extract status raw "$result")
if [[ $status != Accepted ]]; then
    submission_id=$(plutil -extract id raw "$result" 2>/dev/null || true)
    if [[ -n $submission_id ]]; then
        printf 'Notarization submission: %s\n' "$submission_id" >&2
    fi
    printf 'Notarization status: %s\n' "$status" >&2
    exit 1
fi

xcrun stapler staple "$bundle"
xcrun stapler validate "$bundle"
codesign --verify --deep --strict "$bundle"
spctl --assess --type execute --verbose=2 "$bundle"
archive="$stage/${output##*/}"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$archive"
mv -n "$archive" "$output_parent/"
if [[ -e $archive ]]; then
    printf 'The ZIP output already exists: %s\n' "$output" >&2
    exit 1
fi
printf '%s\n' "$output"
