#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="zaino-pyviz"

# Build image if needed
if ! podman image exists "$IMAGE_NAME"; then
    echo "Building $IMAGE_NAME image..."
    podman build -t "$IMAGE_NAME" -f "$SCRIPT_DIR/Containerfile" "$SCRIPT_DIR"
fi

# Run analysis
echo "Running sync analysis..."
podman run --rm -v "$SCRIPT_DIR:/data:z" "$IMAGE_NAME" python /data/sync-report.py
