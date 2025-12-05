local gocdtasks = import 'github.com/getsentry/gocd-jsonnet/libs/gocd-tasks.libsonnet';

local region_pops = {
  de: [
    'de',        // main cluster
    'de-west-de',  // uptime-de
    'de-west-nl',  // uptime-nl
  ],
  us: [
    'us-pop-1',  // pop-va
    'us-pop-9',  // pop-sc
    'us-west-or', // uptime-or
  ],
  s4s: [
    's4s',
    'pop-st-1',
  ],
};

// IMPORTANT: Keep these canary configuration maps in sync between:
// - uptime-checker/gocd/templates/pipelines/uptime-checker.libsonnet (this file)
// - ops/gocd/templates/pipelines/uptime-checker-k8s.libsonnet

// Map of region -> list of POPs within that region that should use canary deployment
// Empty list means all POPs in that region use old direct-deploy flow
local canary_enabled_pops = {
  de: ['de', 'de-west-de', 'de-west-nl'],
  us: ['us-west-or'],
  s4s: ['s4s', 'pop-st-1'],
};

// Helper to check if a region/pop should use canary deployment
local use_canary(region, pop) = std.member(canary_enabled_pops[region], pop);

// Filter POPs for canary vs direct deploy
local canary_pops(region) = std.filter(function(pop) use_canary(region, pop), region_pops[region]);
local direct_deploy_pops(region) = std.filter(function(pop) !use_canary(region, pop), region_pops[region]);

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
          LABEL_SELECTOR: 'service=uptime-checker',
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

// Build canary deployment stages based on configuration
local canary_deployment_stages(region) =
  local pops = canary_pops(region);
  if std.length(pops) == 0 then [] else
    [
      deploy_canary_stage(pops),
      deploy_primary_stage(pops),
      scale_down_canary_stage(pops),
    ];

// Direct deploy stage for non-canary POPs (old behavior)
// This matches the original deploy_primary_stage: single job, no SENTRY_REGION
local direct_deploy_stages(region) =
  local pops = direct_deploy_pops(region);
  if std.length(pops) == 0 then [] else [
    {
      'deploy-primary': {
        fetch_materials: true,
        jobs: {
          deploy: {
            timeout: 600,
            elastic_profile_id: 'uptime-checker',
            environment_variables: {
              SENTRY_REGION: region,
              LABEL_SELECTOR: 'service=uptime-checker,env=primary',
            },
            tasks: [
              gocdtasks.script(importstr '../bash/deploy.sh'),
              // TEMPORARY: Scale down canary statefulsets for old POPs that used to use canary.
              // Can be removed once old POPs are migrated/removed.
              gocdtasks.script(|||
                eval $(regions-project-env-vars --region="${SENTRY_REGION}")
                /devinfra/scripts/get-cluster-credentials

                echo "Scaling down any canary statefulsets..."
                kubectl scale statefulset -l "service=uptime-checker,env=canary" --replicas=0 || true
                echo "Canary cleanup complete"
              |||),
            ],
          },
        },
      },
    },
  ];
local deploy_pop_job(region) =
  {
    timeout: 600,
    elastic_profile_id: 'uptime-checker',
    environment_variables: {
      SENTRY_REGION: region,
      LABEL_SELECTOR: 'service=uptime-checker,env=primary',

    },
    tasks: [
      gocdtasks.script(importstr '../bash/deploy.sh'),
      // TEMPORARY: Scale down canary statefulsets for old POPs that used to use canary.
      // Can be removed once old POPs are migrated/removed.
      gocdtasks.script(|||
        eval $(regions-project-env-vars --region="${SENTRY_REGION}")
        /devinfra/scripts/get-cluster-credentials

        echo "Scaling down any canary statefulsets..."
        kubectl scale statefulset -l "service=uptime-checker,env=canary" --replicas=0 || true
        echo "Canary cleanup complete"
      |||),
    ],
  };


local deploy_pop_jobs(regions) =
  {
    ['deploy-primary-pops-region-' + region]: deploy_pop_job(region)
    for region in regions
  };


local deploy_primary_pops_stage(region) =
  // Only deploy to POPs that are NOT using canary (they're already deployed via canary flow)
  local pops = direct_deploy_pops(region);
  if std.length(pops) == 0 then [] else [
    {
      ['deploy-primary-pops-' + region]: {
        fetch_materials: true,
        jobs+: deploy_pop_jobs(pops),
      },
    },
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
    local has_canary = std.length(canary_pops(region)) > 0;
    [checks_stage] +
    (if has_canary then
       canary_deployment_stages(region)
     else
       direct_deploy_stages(region)) +
    deploy_primary_pops_stage(region),
}
