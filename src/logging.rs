pub fn init() {
    let guard = sentry::init((
        "https://aa37d7fba870e93bf72c2138b1029d35@o1.ingest.us.sentry.io/4507408549347328",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            ..Default::default()
        },
    ));

    // We'll release the sentry hub in main.rs
    std::mem::forget(guard)
}
