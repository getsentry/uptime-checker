#!/bin/bash

checks-githubactions-checkruns \
	"getsentry/uptime-checker" \
	"${GO_REVISION_UPTIME_CHECKER_REPO}" \
	"test"
