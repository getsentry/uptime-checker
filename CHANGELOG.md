## 26.2.0

### New Features ✨

#### Uptime

- Provide detail and paths for comp/runtime assertion errors by @klochek in [#482](https://github.com/getsentry/uptime-checker/pull/482)
- Allow preview checks to capture response body bytes by @klochek in [#472](https://github.com/getsentry/uptime-checker/pull/472)
- Metrics for asserts by @klochek in [#477](https://github.com/getsentry/uptime-checker/pull/477)
- Set assertion limits using config/env vars by @klochek in [#476](https://github.com/getsentry/uptime-checker/pull/476)
- Add response capture support for failure debugging by @wedamija in [#464](https://github.com/getsentry/uptime-checker/pull/464)

### Bug Fixes 🐛

#### Uptime

- Only download body bytes if the assertion needs them by @klochek in [#483](https://github.com/getsentry/uptime-checker/pull/483)
- Devservices fixes (port, repo) by @klochek in [#479](https://github.com/getsentry/uptime-checker/pull/479)
- Add bounds to number of operations allowed in an assertion by @klochek in [#474](https://github.com/getsentry/uptime-checker/pull/474)
- Have jsonpath asserts compare against values by @klochek in [#471](https://github.com/getsentry/uptime-checker/pull/471)
- For asserts, wrap json objects in arrays by @klochek in [#470](https://github.com/getsentry/uptime-checker/pull/470)
- Open the endpoint on the adapter, not localhost by @klochek in [#466](https://github.com/getsentry/uptime-checker/pull/466)

### Internal Changes 🔧

- (uptime) Add comparison tests for uptime asserts by @klochek in [#473](https://github.com/getsentry/uptime-checker/pull/473)

## 26.1.0

### New Features ✨

#### Uptime

- Assertion failure data now contains the assertion by @klochek in [#459](https://github.com/getsentry/uptime-checker/pull/459)
- Add a config to disable assert evaluation by @klochek in [#457](https://github.com/getsentry/uptime-checker/pull/457)

## 25.12.1

### New Features ✨

#### Uptime

- feat(uptime): Add a validate_check endpoint. by @klochek in [#455](https://github.com/getsentry/uptime-checker/pull/455)
- feat(uptime): expose assertion failure details on CheckResult by @klochek in [#440](https://github.com/getsentry/uptime-checker/pull/440)
- feat(uptime): Add miss_produced and miss_backfill status reason types by @evanpurkhiser in [#454](https://github.com/getsentry/uptime-checker/pull/454)

### Bug Fixes 🐛

- fix(uptime): rework assertion comparison a bit, for consistency by @klochek in [#456](https://github.com/getsentry/uptime-checker/pull/456)

## 25.12.0

### New Features ✨

#### Uptime

- feat(uptime): use hashmap-based stats by @klochek in [#449](https://github.com/getsentry/uptime-checker/pull/449)
- feat(uptime): enable checks execution from the endpoint by @klochek in [#439](https://github.com/getsentry/uptime-checker/pull/439)

### Bug Fixes 🐛

#### Uptime

- fix(uptime): Fix bug where we deploy to canary again during the primary deploy by @wedamija in [#451](https://github.com/getsentry/uptime-checker/pull/451)
- fix(uptime): add some logging to diagnose a connection-related error by @klochek in [#436](https://github.com/getsentry/uptime-checker/pull/436)

### Build / dependencies / internal 🔧

#### Uptime

- chore(uptime): Clean up deploy code to remove old paths by @wedamija in [#453](https://github.com/getsentry/uptime-checker/pull/453)
- chore(uptime): Run jsonnet formatting command by @wedamija in [#452](https://github.com/getsentry/uptime-checker/pull/452)
- chore(uptime): Enable `us-east-va` and disable `us-east-pop-va` by @wedamija in [#450](https://github.com/getsentry/uptime-checker/pull/450)
- chore(uptime): Fix `us-east-sc` name so that canary works with it. by @wedamija in [#448](https://github.com/getsentry/uptime-checker/pull/448)
- chore(uptime): Disable `us-east-pop-sc` and enable `us-east-sc` by @wedamija in [#447](https://github.com/getsentry/uptime-checker/pull/447)
- chore(uptime): Add temporary `cleanup_decommissioned_stage` step to deploy by @wedamija in [#445](https://github.com/getsentry/uptime-checker/pull/445)
- chore(uptime): Make sure canaries are properly scaled down when using old deploy path by @wedamija in [#444](https://github.com/getsentry/uptime-checker/pull/444)
- chore(uptime): Disable canaries on old regions by @wedamija in [#443](https://github.com/getsentry/uptime-checker/pull/443)
- chore(uptime): Enable uptime-de and uptime-or regions by @wedamija in [#442](https://github.com/getsentry/uptime-checker/pull/442)
- chore(uptime): Make sure we're deploying to new regions by @wedamija in [#441](https://github.com/getsentry/uptime-checker/pull/441)

## 25.11.1

### New Features ✨

- feat(uptime): add assertion checks to check_executor by @klochek in [#437](https://github.com/getsentry/uptime-checker/pull/437)
- feat(uptime): basic http endpoint by @klochek in [#435](https://github.com/getsentry/uptime-checker/pull/435)
- feat(uptime): add jsonpath to assertion; add "gas" concept to assertion by @klochek in [#434](https://github.com/getsentry/uptime-checker/pull/434)

## 25.11.0

### Various fixes & improvements

- fix(release): Add name to assemble step (#433) by @hubertdeng123
- fix(self-hosted): Release Fix (#432) by @hubertdeng123
- feat(uptime): add uptime assertions data model + basic compiler (#416) by @klochek
- Revert "chore(uptime): Add in better logging around vector (#431)" (77a626b3)
- chore(uptime): Add in better logging around vector (#431) by @wedamija
- chore(uptime): Enable canary for all us uptime_regions (#430) by @wedamija
- chore(uptime): Enable canary for all DE regions, and test in one US region (#429) by @wedamija
- chore(uptime): Remove canary pre-cleanup step (#428) by @wedamija
- feat: devservices (#415) by @joshuarli
- fix(uptime): Properly wait for all canary pods to deploy (#427) by @wedamija
- chore(uptime): Try canary deploy in one DE pop (#426) by @wedamija
- feat(uptime): add config and read-only mode for redis client (#413) by @klochek
- chore(uptime): Consolidate the canary cleanup and deploy stages (#424) by @wedamija
- chore(uptime): Properly enable all s4s uptime-regions to use the new canary deploy system (#423) by @wedamija
- feat(uptime): Remove duplicate wait stage (#422) by @wedamija
- chore(uptime): Tidy up deploy stages. Make sure we deploy to both s4s regions (#421) by @wedamija
- chore(uptime): Consolidate the deploy/scale canary steps (#420) by @wedamija
- fix(uptime): Add kubectl authentication to canary stages (#419) by @wedamija
- fix(uptime): Fix the canary deploy (#418) by @wedamija
- chore(uptime): Try new deploy pipeline in s4s (#417) by @wedamija
- feat(uptime): Implement new uptime-checker deploy pipeline (#414) by @wedamija
- fix(uptime): add better connection-related error stats (#412) by @klochek
- fix(uptime): disable connection pools to vector (#410) by @klochek
- feat(uptime): add connection-related vector stats (#409) by @klochek

_Plus 6 more_

## 25.10.0

### Various fixes & improvements

- fix(uptime) linter fixes (#399) by @klochek
- Revert "feat(uptime): check robots.txt once per day, per subscription (#395)" (c10af1b5)
- feat(uptime): check robots.txt once per day, per subscription (#395) by @klochek
- feat(vector): implement spooling logic (#229) by @JoshFerge

## 25.9.0

### Various fixes & improvements

- feat: devenv + uv + pre-commit (#394) by @joshuarli

## 25.8.0

### Various fixes & improvements

- fix(uptime): use the new req and resp timings for better stats (#393) by @klochek
- feat(uptime): add us timings to CheckResult fields (#392) by @klochek
- feat(uptime): add CertificateInfo to CheckResult (#391) by @klochek
- fix(ssl): Install missing intermediary cert (#390) by @evanpurkhiser
- Optimize ca-certificates layer for better updates (#389) by @gaprl
- fix(docker): Add ca-certificates to production image (#388) by @gaprl
- feat(uptime): add durations to request info object (#387) by @klochek
- Reapply "chore(uptime): Add .envrc (#364)" (#369) (#386) by @evanpurkhiser
- Bump alpine on Dockerfile.localdev (#385) by @evanpurkhiser
- Reapply "feat(uptime): add redirect uris and connection start timestamps to stats (#376)" (#381) by @evanpurkhiser
- Bump to latest alpine + rust 1.88 (#383) by @evanpurkhiser
- Bump to alpine 3.21.4 (#380) by @evanpurkhiser
- feat: Make sure 3.20.7 actually breaks things (#379) by @evanpurkhiser
- Add RUST_BACKTRACE to hopefully get a crash trace (#378) by @evanpurkhiser
- fix: Pin alpine to 3.20.6 (#377) by @evanpurkhiser
- Revert "feat(uptime): add redirect uris and connection start timestamps to stats (#376)" (cee0663b)
- feat(uptime): add redirect uris and connection start timestamps to stats (#376) by @klochek
- ci: Fix revert bot name (#375) by @evanpurkhiser
- Revert "Testing reverts (#374)" (36fee1cc) by @MineCraftSpy
- ci: run self-hosted e2e test (second attempt) (#373) by @aldy505
- Testing reverts (#374) by @evanpurkhiser
- ci: Add fast-revert (#370) by @evanpurkhiser
- Revert "ci: run self-hosted e2e test (#362)" (#372) by @evanpurkhiser
- Reapply "chore: bump Rust toolchain version to 1.85 (#325)" (#367) (#368) by @evanpurkhiser

_Plus 8 more_

## 25.7.0

### Various fixes & improvements

- gha: scope docker cache to platform (#359) by @mdtro
- fix: remediate dockerfile syntax warning (#358) by @mdtro
- ci: use gha for cache (#356) by @mdtro
- ci: craft should wait for "Create multi-platform manifest" job to finish (#354) by @aldy505
- ci: include image annotations (#353) by @mdtro
- ci: Fix `latest` tag (#352) by @evanpurkhiser

## 25.6.1

### Various fixes & improvements

- ci: use buildx imagetools for manifest (#351) by @mdtro
- ci: ammend manifest to combine multi-platform image (#350) by @mdtro
- ci: Run CI in release branches (#349) by @evanpurkhiser
- ci: Make build cache per-platform (#348) by @evanpurkhiser
- ci: Fix build caching for image workflow (#346) by @evanpurkhiser
- ci: Remove `name` in workflow (#343) by @evanpurkhiser
- ci: Correct github variables (#342) by @evanpurkhiser
- ci: Fix cache-from config (#340) by @evanpurkhiser
- ci: Bring back docker hub push (#339) by @evanpurkhiser
- ci: Remove release-ghcr-version-tag (#338) by @evanpurkhiser
- ci: Cleanup image builder (#337) by @evanpurkhiser
- ci: Bring the release target back for images (#336) by @evanpurkhiser
- ci: Fix bump-version script (#335) by @evanpurkhiser
- craft: Add changelog (#334) by @evanpurkhiser

