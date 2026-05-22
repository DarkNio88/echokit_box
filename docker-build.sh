#!/usr/bin/env bash
set -euo pipefail

IMAGE=echokit-box-build:ubuntu22.04

##echo "Building Docker image $IMAGE..."
##docker build -t "$IMAGE" -f Dockerfile .

echo "Running build inside container (install esp toolchain then build)..."

# Named Docker volumes to persist caches between runs (cargo registry/git, rustup, target, espup, general cache)
V_CARGO_REGISTRY="${V_CARGO_REGISTRY:-echokit-cargo-registry}"
V_CARGO_GIT="${V_CARGO_GIT:-echokit-cargo-git}"
V_RUSTUP="${V_RUSTUP:-echokit-rustup}"
V_CARGO_TARGET="${V_CARGO_TARGET:-echokit-target}"
V_ESP="${V_ESP:-echokit-espup-cache}"
V_CACHE="${V_CACHE:-echokit-cache}"

docker volume create "$V_CARGO_REGISTRY" >/dev/null || true
docker volume create "$V_CARGO_GIT" >/dev/null || true
docker volume create "$V_RUSTUP" >/dev/null || true
docker volume create "$V_CARGO_TARGET" >/dev/null || true
docker volume create "$V_ESP" >/dev/null || true
docker volume create "$V_CACHE" >/dev/null || true

docker run --rm -it \
	-v "$(pwd)":/workspace -w /workspace \
	-v "$V_CARGO_REGISTRY":/root/.cargo/registry \
	-v "$V_CARGO_GIT":/root/.cargo/git \
	-v "$V_RUSTUP":/root/.rustup \
	-v "$V_CARGO_TARGET":/workspace/target \
	-v "$V_ESP":/root/.espressif \
	-v "$V_CACHE":/root/.cache \
	"$IMAGE" bash -lc "set -e; \
		if ! command -v espup >/dev/null 2>&1; then echo 'espup not found in image'; fi; \
		yes | espup install || true; \
		cargo build --no-default-features --features esp32s3cam,boards --release; \
		# Create a host-visible dist folder and copy only the specific flash binaries
		mkdir -p /workspace/dist; \
		# Copy any matching app binary (echokit), plus bootloader and partition table if present
		find target -type f \( -name 'bootloader.bin' -o -name 'partition-table.bin' -o -name 'echokit' \) -exec cp -v {} /workspace/dist/ \; || true; \
		# Also check common build output locations as a fallback
		find build -type f \( -name 'bootloader.bin' -o -name 'partition-table.bin' -o -name 'echokit' \) -exec cp -v {} /workspace/dist/ \; || true; \
		# Verify required files exist and fail if any missing (user requested exact files)
		missing=0; \
		for f in echokit 'bootloader.bin' 'partition-table.bin'; do \
			if [ ! -f /workspace/dist/\$f ]; then echo 'ERROR: Required file missing:' "\$f" >&2; missing=1; fi; \
		done; \
		if [ "\$missing" -ne 0 ]; then echo 'One or more required firmware files missing. See /workspace/dist' >&2; exit 2; fi; \
		ls -la /workspace/dist"

echo "Build finished. Artifacts are in ./target/release (cache persisted in Docker volumes: $V_CARGO_REGISTRY, $V_CARGO_GIT, $V_RUSTUP, $V_CARGO_TARGET, $V_ESP, $V_CACHE)"
cp /root/echokit_box/dist/echokit /var/www/html/echokit
cp /root/echokit_box/setup/index.html /var/www/html/setup.html