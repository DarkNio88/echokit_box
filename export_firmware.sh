#!/usr/bin/env bash
set -euo pipefail

# export_firmware.sh
# Gather build artifacts and produce a distributable archive.

SRC_DIR=target/xtensa-esp32s3-espidf/release
DST_DIR=dist

echo "Creating $DST_DIR directory..."
mkdir -p "$DST_DIR"

echo "Copying known artifacts from $SRC_DIR to $DST_DIR"
for f in "$SRC_DIR"/bootloader.bin "$SRC_DIR"/partition-table.bin "$SRC_DIR"/echokit "$SRC_DIR"/*.bin; do
  if [ -f "$f" ]; then
    cp -v "$f" "$DST_DIR/" || true
  fi
done

echo "Creating tar.gz archive..."
tar -C "$DST_DIR" -czf echokit_firmware.tar.gz . || true

if command -v zip >/dev/null 2>&1; then
  echo "Creating ZIP archive..."
  (cd "$DST_DIR" && zip -r ../echokit_firmware.zip .) || true
fi

echo "Export complete. Archives created at:"
echo "  - $(pwd)/echokit_firmware.tar.gz"
if [ -f echokit_firmware.zip ]; then
  echo "  - $(pwd)/echokit_firmware.zip"
fi

echo "Also check the $DST_DIR directory for individual files."
