#!/bin/bash

checks-githubactions-checkruns2 \
	"getsentry/uptime-checker" \
	"${GO_REVISION_UPTIME_CHECKER_REPO}" \
	"test" \
	"Build and push production images"
