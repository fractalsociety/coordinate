use tempfile::TempDir;

#[test]
fn test_write_and_read_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "abc-123").unwrap();
    let token = squad::session::read_token(&sessions_dir, "worker").unwrap();
    assert_eq!(token, Some("abc-123".to_string()));
}

#[test]
fn test_read_nonexistent_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let token = squad::session::read_token(&sessions_dir, "nobody").unwrap();
    assert_eq!(token, None);
}

#[test]
fn test_delete_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "abc-123").unwrap();
    squad::session::delete_token(&sessions_dir, "worker").unwrap();
    let token = squad::session::read_token(&sessions_dir, "worker").unwrap();
    assert_eq!(token, None);
}

#[test]
fn test_validate_token_match() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "abc-123").unwrap();
    let result = squad::session::validate(&sessions_dir, "worker", "abc-123");
    assert!(result.is_ok());
}

#[test]
fn test_validate_token_mismatch() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "old-token").unwrap();
    let result = squad::session::validate(&sessions_dir, "worker", "new-token");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Session replaced"));
}

#[test]
fn test_validate_no_file_is_ok() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    // No session file = backward compat, should pass
    let result = squad::session::validate(&sessions_dir, "worker", "any-token");
    assert!(result.is_ok());
}
