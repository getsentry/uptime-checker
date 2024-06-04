local uptime_checker = import './pipelines/uptime-checker.libsonnet';
local pipedream = import 'github.com/getsentry/gocd-jsonnet/libs/pipedream.libsonnet';

// Pipedream can be configured using this object, you can learn more about the
// configuration options here: https://github.com/getsentry/gocd-jsonnet#readme
local pipedream_config = {
  name: 'uptime-checker',
  auto_deploy: true,
  materials: {
    uptime_checker_repo: {
      git: 'git@github.com:getsentry/uptime-checker.git',
      shallow_clone: true,
      branch: 'main',
      destination: 'uptime-checker',
    },
  },
  rollback: {
    material_name: 'uptime_checker_repo',
    stage: 'deploy-primary',
    elastic_profile_id: 'uptime-checker',
  },
  exclude_regions: ['customer-1', 'customer-2', 'customer-3', 'customer-4', 'customer-6', 'customer-7'],
};

pipedream.render(pipedream_config, uptime_checker)
