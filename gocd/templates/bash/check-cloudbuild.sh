#!/bin/bash

checks-googlecloud-check-cloudbuild \
	sentryio \
	uptime-checker \
	build-uptime-checker \
	"${GO_REVISION_UPTIME_CHECKER_REPO}" \
	main
