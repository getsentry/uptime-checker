#[cfg(test)]
pub mod test_utils {
    use tokio::sync::Mutex;
    
    pub static TEST_MUTEX: Mutex<()> = Mutex::const_new(());
}