#[cfg(test)]
pub mod sync {
    use tokio::sync::Mutex;

    pub static TEST_MUTEX: Mutex<()> = Mutex::const_new(());
}
