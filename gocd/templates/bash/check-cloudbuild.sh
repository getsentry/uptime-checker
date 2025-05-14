#!/bin/bash

/devinfra/scripts/checks/googlecloud/check_cloudbuild.py \
	sentryio \
	uptime-checker \
	build-uptime-checker \
	"${GO_REVISION_UPTIME_CHECKER_REPO}" \
	main
