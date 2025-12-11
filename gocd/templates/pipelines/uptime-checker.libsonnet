local gocdtasks = import 'github.com/getsentry/gocd-jsonnet/libs/gocd-tasks.libsonnet';

// IMPORTANT: Keep these configuration maps in sync between this file and
// ops/gocd/templates/pipelines/uptime-checker-k8s.libsonnet

local region_pops = {
  de: [
    'de',  // main cluster
    'de-west-de',  // uptime-de
    'de-west-nl',  // uptime-nl
  ],
  us: [
    'us-west-or',  // uptime-or
    'us-east-sc',  // uptime-sc
    'us-east-va',  // uptime-va
  ],
  s4s: [
    's4s',
    'pop-st-1',
  ],
};

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
        ],
      },
    },
  },
};

// Helper stages for canary deployment
local deploy_canary_stage(pops) = {
  'deploy-canary': {
    fetch_materials: true,
    jobs: {
      ['deploy-canary-' + r]: {
        timeout: 600,
        elastic_profile_id: 'uptime-checker',
        environment_variables: {
          SENTRY_REGION: r,
          LABEL_SELECTOR: 'service=uptime-checker,env=canary',
        },
        tasks: [
          gocdtasks.script(importstr '../bash/deploy.sh'),
          gocdtasks.script(|||
            eval $(regions-project-env-vars --region="${SENTRY_REGION}")
            /devinfra/scripts/get-cluster-credentials

            PROD_REPLICAS=$(kubectl get statefulset -l "service=uptime-checker,env=primary" -o jsonpath='{.items[0].spec.replicas}')
            CANARY_STATEFULSET=$(kubectl get statefulset -l "service=uptime-checker,env=canary" -o jsonpath='{.items[0].metadata.name}')
            echo "Scaling ${CANARY_STATEFULSET} to ${PROD_REPLICAS} replicas to match production"
            kubectl scale statefulset ${CANARY_STATEFULSET} --replicas="${PROD_REPLICAS}"
            echo "Waiting for canary rollout to complete..."
            kubectl rollout status statefulset/${CANARY_STATEFULSET} --timeout=600s
            echo "Canary rollout complete"
          |||),
        ],
      }
      for r in pops
    },
  },
};

local deploy_primary_stage(pops) = {
  'deploy-primary': {
    fetch_materials: true,
    jobs: {
      ['deploy-primary-' + r]: {
        timeout: 600,
        elastic_profile_id: 'uptime-checker',
        environment_variables: {
          SENTRY_REGION: r,
          LABEL_SELECTOR: 'service=uptime-checker,env=primary',
        },
        tasks: [
          gocdtasks.script(importstr '../bash/deploy.sh'),
        ],
      }
      for r in pops
    },
  },
};

local scale_down_canary_stage(pops) = {
  'scale-down-canary': {
    fetch_materials: true,
    jobs: {
      ['scale-down-' + r]: {
        elastic_profile_id: 'uptime-checker',
        environment_variables: {
          SENTRY_REGION: r,
        },
        tasks: [
          gocdtasks.script(|||
            eval $(regions-project-env-vars --region="${SENTRY_REGION}")
            /devinfra/scripts/get-cluster-credentials

            echo "Scaling canary back down to 0 replicas..."
            kubectl scale statefulset -l "service=uptime-checker,env=canary" --replicas=0
            echo "Canary scaled down"
          |||),
        ],
      }
      for r in pops
    },
  },
};

local canary_deployment_stages(region) =
  local pops = region_pops[region];
  [
    deploy_canary_stage(pops),
    deploy_primary_stage(pops),
    scale_down_canary_stage(pops),
  ];


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
  stages:
    [checks_stage] +
    canary_deployment_stages(region),
}
