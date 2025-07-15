#!/bin/bash

eval $(regions-project-env-vars --region="${SENTRY_REGION}")

/devinfra/scripts/get-cluster-credentials &&
	k8s-deploy \
		--type="statefulset" \
		--label-selector="${LABEL_SELECTOR}" \
		--image="us-central1-docker.pkg.dev/sentryio/uptime-checker/image:${GO_REVISION_UPTIME_CHECKER_REPO}" \
		--container-name="uptime-checker"
