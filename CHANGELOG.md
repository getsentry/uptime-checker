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

