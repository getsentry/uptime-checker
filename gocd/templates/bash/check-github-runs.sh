#!/bin/bash

/devinfra/scripts/checks/githubactions/checkruns.py \
	"getsentry/uptime-checker" \
	"${GO_REVISION_UPTIME_CHECKER_REPO}" \
	"test"
