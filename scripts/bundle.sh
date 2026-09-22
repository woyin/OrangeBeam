#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
destination=${1:-"$project_dir/dist"}
: "${CARGO:=cargo}"
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "Build on an Apple Silicon Mac." >&2
  exit 1
fi
MACOSX_DEPLOYMENT_TARGET=13.0 "$CARGO" build --release --locked --manifest-path "$project_dir/Cargo.toml"
target_dir=${CARGO_TARGET_DIR:-"$project_dir/target"}
app_dir="$destination/Orange Beam.app"
mkdir -p "$app_dir/Contents/MacOS"
mkdir -p "$app_dir/Contents/Resources/Licenses"
cp "$target_dir/release/orange-beam" "$app_dir/Contents/MacOS/orange-beam"
# Localized Finder/Launchpad name: 橙现 on Chinese systems, Orange Beam elsewhere.
for lproj in en.lproj zh-Hans.lproj; do
  mkdir -p "$app_dir/Contents/Resources/$lproj"
  cp "$project_dir/assets/$lproj/InfoPlist.strings" "$app_dir/Contents/Resources/$lproj/"
done
cp "$project_dir/assets/Info.plist" "$app_dir/Contents/Info.plist"
cp "$project_dir/LICENSE" "$project_dir/NOTICE" "$project_dir/THIRD-PARTY-LICENSES.txt" "$app_dir/Contents/Resources/Licenses/"
codesign --force --sign - --identifier io.github.woyin.orangebeam "$app_dir"
echo "$app_dir"
