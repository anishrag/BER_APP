use ber_model::ber::common::Evidence;

#[test]
fn legacy_photo_json_without_content_type_loads() {
    // JSON as produced by the OLD Evidence shape (no `content_type` key).
    let old = r#"{"id":"00000000-0000-0000-0000-000000000000","file_path":"front.png","thumbnail_path":null,"description":"Front","side":null}"#;
    let p: Evidence = serde_json::from_str(old).unwrap();
    assert_eq!(p.content_type, None);
    // Inference still works off the extension for the legacy record.
    assert_eq!(p.content_type(), "image/png");
    assert!(p.is_image());
}
