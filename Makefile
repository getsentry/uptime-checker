SHELL=/bin/bash

gocd: ## Build GoCD pipelines
	rm -rf ./gocd/generated-pipelines
	mkdir -p ./gocd/generated-pipelines
	cd ./gocd/templates && jb install && jb update

  # Format
	find . -type f \( -name '*.libsonnet' -o -name '*.jsonnet' \) -print0 | xargs -n 1 -0 jsonnetfmt -i
  # Lint
	find . -type f \( -name '*.libsonnet' -o -name '*.jsonnet' \) -print0 | xargs -n 1 -0 jsonnet-lint -J ./gocd/templates/vendor
	# Build
	cd ./gocd/templates && find . -type f \( -name '*.jsonnet' \) -print0 | xargs -n 1 -0 jsonnet --ext-code output-files=true -J vendor -m ../generated-pipelines

  # Convert JSON to yaml
	cd ./gocd/generated-pipelines && find . -type f \( -name '*.yaml' \) -print0 | xargs -n 1 -0 yq -p json -o yaml -i
.PHONY: gocd


# As of this commit date, we might encounter a compilation error within rdkafka due to them using an
# outdated CMake version.  Setting CMAKE_POLICY_VERSION_MINIMUM gets around the issue for now; once
# they're up to date, we can remove this env.

run:
	CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run -- run
.PHONY: run

run-verbose:
	CMAKE_POLICY_VERSION_MINIMUM=3.5 UPTIME_CHECKER_LOG_LEVEL=info cargo run -- run
.PHONY: run-verbose

test:
	CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test
.PHONY: test
