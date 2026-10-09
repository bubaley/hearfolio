#!/usr/bin/env bash
set -euo pipefail
# Run from the repository. Toolchains and Gradle caches stay in work/, outside deliverables.
repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
tools_dir="${HEARFOLIO_TOOLS:-$repo_dir/../../work/toolchains}"
node_dir="$(dirname "$(dirname "$(command -v node)")")"
docker run --rm --user "$(id -u):$(id -g)" -v "$repo_dir:/repo" -v "$tools_dir:/tools" -v "$node_dir:/node:ro" -w /repo \
 -e HOME=/tools/home -e CARGO_HOME=/tools/cargo -e RUSTUP_HOME=/tools/rustup \
 -e PATH=/tools/cargo/bin:/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
 -e ANDROID_HOME=/tools/android -e NDK_HOME=/tools/android/ndk/27.2.12479018 \
 -e "JAVA_TOOL_OPTIONS=-Djava.net.preferIPv4Stack=true -Duser.home=/tools/home" -e JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64 -e GRADLE_USER_HOME=/tools/gradle \
 -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 -e CARGO_BUILD_JOBS=3 \
 hearfolio-transfer-build bash -c 'mkdir -p /tools/home; exec "$@"' bash "$@"
