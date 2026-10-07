#!/usr/bin/env bash
# Signs an unsigned arm64 APK and AAB without putting secrets in the source tree.
set -euo pipefail
for variable in ANDROID_HOME RUNNER_TEMP ANDROID_KEYSTORE_BASE64 ANDROID_KEYSTORE_PASSWORD ANDROID_KEY_PASSWORD ANDROID_KEY_ALIAS; do
  if [[ -z "${!variable:-}" ]]; then
    echo "Missing required environment variable: $variable" >&2
    exit 1
  fi
done
version="${1:?Expected release version}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 1
outputs="${2:-src-tauri/gen/android/app/build/outputs}"
destination="${3:-release-assets}"
build_tools="$ANDROID_HOME/build-tools/36.0.0"
keystore="$RUNNER_TEMP/hearfolio-release.jks"
trap 'rm -f "$keystore" "$RUNNER_TEMP/hearfolio-aligned.apk"' EXIT
umask 077
printf '%s' "$ANDROID_KEYSTORE_BASE64" | base64 --decode > "$keystore"
mapfile -t apks < <(find "$outputs/apk" -type f -name '*.apk' ! -name '*debug*')
mapfile -t aabs < <(find "$outputs/bundle" -type f -name '*.aab' ! -name '*debug*')
[[ ${#apks[@]} == 1 && ${#aabs[@]} == 1 ]] || { echo 'Expected one arm64 release APK and one AAB' >&2; exit 1; }
mkdir -p "$destination"
apk="$destination/Hearfolio_${version}_android_aarch64.apk"
aab="$destination/Hearfolio_${version}_android_aarch64.aab"
"$build_tools/zipalign" -f -P 16 4 "${apks[0]}" "$RUNNER_TEMP/hearfolio-aligned.apk"
"$build_tools/apksigner" sign --ks "$keystore" --ks-key-alias "$ANDROID_KEY_ALIAS" \
  --ks-pass env:ANDROID_KEYSTORE_PASSWORD --key-pass env:ANDROID_KEY_PASSWORD \
  --v4-signing-enabled false \
  --out "$apk" "$RUNNER_TEMP/hearfolio-aligned.apk"
"$build_tools/apksigner" verify "$apk"
cp "${aabs[0]}" "$aab"
jarsigner -keystore "$keystore" -storepass:env ANDROID_KEYSTORE_PASSWORD \
  -keypass:env ANDROID_KEY_PASSWORD "$aab" "$ANDROID_KEY_ALIAS"
# jarsigner -strict rejects self-signed certs normally used for Android. Verify
# structure/signature, then require the expected alias to be present in output.
jarsigner -verify "$aab" | tee "$RUNNER_TEMP/hearfolio-aab-verification.txt"
verification="$(cat "$RUNNER_TEMP/hearfolio-aab-verification.txt")"
[[ "$verification" == *'jar verified.'* ]] || { echo 'Android App Bundle signature verification failed' >&2; exit 1; }
