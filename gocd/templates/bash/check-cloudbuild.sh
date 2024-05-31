#!/bin/bash

/devinfra/scripts/checks/googlecloud/checkcloudbuild.py \
	"${GO_REVISION_UPTIME_CHECKER_REPO}" \
	"sentryio" \
	"us-central1-docker.pkg.dev/sentryio/uptime-checker/image"
