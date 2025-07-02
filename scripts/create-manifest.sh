#!/bin/bash
set -euo pipefail

# Script to create and push multi-platform Docker manifests
# Usage: create-manifest.sh <repository> <sha> <git_ref> <default_branch>

REPOSITORY="$1"
SHA="$2"
GIT_REF="$3"
DEFAULT_BRANCH="$4"

# Define registries and their prefixes
declare -A REGISTRIES=(
  ["ghcr.io"]="ghcr.io/${REPOSITORY}"
  ["docker.io"]="${REPOSITORY}"
)

# Extract short SHA (first 7 characters)
SHORT_SHA="${SHA:0:7}"

echo "Creating manifests for repository: ${REPOSITORY}"
echo "SHA: ${SHA} (short: ${SHORT_SHA})"
echo "Git ref: ${GIT_REF}"
echo "Default branch: ${DEFAULT_BRANCH}"

# Create manifests for each registry
for registry in "${!REGISTRIES[@]}"; do
  IMAGE_NAME="${REGISTRIES[$registry]}"
  echo ""
  echo "Creating manifests for ${IMAGE_NAME}"
  
  # Create long SHA manifest
  echo "Creating long SHA manifest: ${IMAGE_NAME}:${SHA}"
  docker manifest create "${IMAGE_NAME}:${SHA}" \
    --amend "${IMAGE_NAME}:${SHA}-amd64" \
    --amend "${IMAGE_NAME}:${SHA}-arm64"
  
  docker manifest push "${IMAGE_NAME}:${SHA}"
  echo "✓ Pushed ${IMAGE_NAME}:${SHA}"
  
  # Create short SHA manifest
  echo "Creating short SHA manifest: ${IMAGE_NAME}:${SHORT_SHA}"
  docker manifest create "${IMAGE_NAME}:${SHORT_SHA}" \
    --amend "${IMAGE_NAME}:${SHA}-amd64" \
    --amend "${IMAGE_NAME}:${SHA}-arm64"
  
  docker manifest push "${IMAGE_NAME}:${SHORT_SHA}"
  echo "✓ Pushed ${IMAGE_NAME}:${SHORT_SHA}"
  
  # Create nightly manifest only for main branch
  if [[ "${GIT_REF}" == "refs/heads/${DEFAULT_BRANCH}" ]]; then
    echo "Creating nightly manifest: ${IMAGE_NAME}:nightly"
    docker manifest create "${IMAGE_NAME}:nightly" \
      --amend "${IMAGE_NAME}:${SHA}-amd64" \
      --amend "${IMAGE_NAME}:${SHA}-arm64"
    
    docker manifest push "${IMAGE_NAME}:nightly"
    echo "✓ Pushed ${IMAGE_NAME}:nightly"
  else
    echo "Skipping nightly manifest (not on default branch)"
  fi
done

echo ""
echo "🎉 All manifests created and pushed successfully!" 