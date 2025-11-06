This service powers the results for uptime monitoring in Sentry.

# Local Development

(The Dockerfile.localdev file is likely not what you want--it's only used for running linux-specific local tests.)

We have lots of tests that run quickly and will therefore provide you with a quick development loop.  For more holistic testing, you can run the uptime-checker on your machine.

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
- Start devservices in sentry:
```sh
    devservices up --mode=uptime
```
- Ensure Sentry is started with `ingest` and `workers` flags:
```sh
    sentry devserver --workers --ingest
```
- The Makefile defines a few targets: `run` and `run-verbose` run the server (with additional logging), and `test` for running all tests.
