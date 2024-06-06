local gocdtasks = import 'github.com/getsentry/gocd-jsonnet/libs/gocd-tasks.libsonnet';

local checks_stage = {
  checks: {
    fetch_materials: true,
    jobs: {
      checks: {
        timeout: 1200,
        elastic_profile_id: 'uptime-checker',
        environment_variables: {
          GITHUB_TOKEN: '{{SECRET:[devinfra-github][token]}}',
        },
        tasks: [
          gocdtasks.script(importstr '../bash/check-github-runs.sh'),
          gocdtasks.script(importstr '../bash/check-cloudbuild.sh'),
        ],
      },
    },
  },
};

local deploy_canary_stage(region) =
  if region == 'us' then
    [
      {
        'deploy-canary': {
          fetch_materials: true,
          jobs: {
            deploy: {
              timeout: 600,
              elastic_profile_id: 'uptime-checker',
              environment_variables: {
                LABEL_SELECTOR: 'service=uptime-checker,env=canary',
              },
              tasks: [
                gocdtasks.script(importstr '../bash/deploy.sh'),
                gocdtasks.script(importstr '../bash/wait-canary.sh'),
              ],
            },
          },
        },
      },
    ] else [];

local deployPrimaryStage = {
  'deploy-primary': {
    fetch_materials: true,
    jobs: {
      deploy: {
        timeout: 600,
        elastic_profile_id: 'uptime-checker',
        environment_variables: {
          LABEL_SELECTOR: 'service=uptime-checker',
        },
        tasks: [
          gocdtasks.script(importstr '../bash/deploy.sh'),
        ],
      },
    },
  },
};

function(region) {
  environment_variables: {
    // SENTRY_REGION is used by the dev-infra scripts to connect to GKE
    SENTRY_REGION: region,
  },
  materials: {
    uptime_checker_repo: {
      git: 'git@github.com:getsentry/uptime-checker.git',
      shallow_clone: true,
      branch: 'main',
      destination: 'uptime-checker',
    },
  },
  lock_behavior: 'unlockWhenFinished',
  stages: [checks_stage] + deploy_canary_stage(region) + [deployPrimaryStage],
}
