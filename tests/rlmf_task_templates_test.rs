use std::fs;
use std::path::Path;

const RLMF_TEMPLATES: &[&str] = &[
    "rlmf-dataset-builder.md",
    "rlmf-lora-qlora-runner.md",
    "rlmf-vllm-qwen-judge.md",
    "rlmf-api-dashboard-integration.md",
    "rlmf-chain-indexer-attestation.md",
];

#[test]
fn rlmf_task_templates_exist_with_required_worker_sections() {
    for template in RLMF_TEMPLATES {
        assert_template_has_required_sections(Path::new("templates").join(template));
    }
}

fn assert_template_has_required_sections(path: impl AsRef<Path>) {
    let path = path.as_ref();
    let body = fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    });
    for section in [
        "# RLMF",
        "## Assignment",
        "## Scope",
        "## Acceptance",
        "## Output",
        "## Verification",
        "changed files",
        "tests run",
        "risks",
    ] {
        assert!(
            body.to_lowercase().contains(&section.to_lowercase()),
            "{} missing required section/text: {section}",
            path.display()
        );
    }
}
