This service powers the results for uptime monitoring in Sentry.

# Local Development

(The Dockerfile.localdev file is likely not what you want--it's only used for running linux-specific local tests.)

We have lots of tests that run quickly and will therefore provide you with a quick development loop.  For more holistic testing, you can run the uptime-checker on your machine.

The Makefile defines a few targets: `run` and `run-verbose` run the server (with additional logging), and `test` for running all tests.

# Dependencies

Run `devenv sync`.

# Running Tests

Run `devservices up` then `make test`.

# Running with local Sentry
- Prepare your local development environment by following the guide [here](https://develop.sentry.dev/development-infrastructure/environment/)
- (For now), ensure the following settings are in your `~/.sentry/sentry.conf.py` file:
```sh
    SENTRY_FEATURES['organizations:insights-uptime'] = True
    SENTRY_EVENTSTREAM = "sentry.eventstream.kafka.KafkaEventStream"
    SENTRY_USE_UPTIME = True
```
- If you want to run uptime-checker locally rather than use the prebuilt image:
```sh
    devservices toggle uptime-checker local

    # to do the opposite (most sentry devs not working on uptime-checker actively will be using this already)
    # devservices toggle uptime-checker containerized
```
- Inside sentry, run:
```sh
    devservices up --mode=uptime
    sentry devserver --workers --ingest
```
