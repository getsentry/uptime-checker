#!/bin/bash

# Regions listed here run the distroless image, tagged `<sha>-distroless` by CI.
IMAGE_TAG="${GO_REVISION_UPTIME_CHECKER_REPO}"
case "${SENTRY_REGION}" in
	de-west-nl) IMAGE_TAG="${IMAGE_TAG}-distroless" ;;
esac

eval $(regions-project-env-vars --region="${SENTRY_REGION}")

/devinfra/scripts/get-cluster-credentials && k8s-deploy \
		--type="statefulset" \
		--label-selector="${LABEL_SELECTOR}" \
		--image="us-central1-docker.pkg.dev/sentryio/uptime-checker/image:${IMAGE_TAG}" \
		--container-name="uptime-checker"
