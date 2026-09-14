use std::path::PathBuf;

use agent_shunt::{
    adapters::{filesystem::SecureFilesystem, ripgrep::RipgrepSearch},
    application::retrieve::{RetrieveInput, execute},
    domain::Limits,
};

#[test]
fn representative_repository_queries_recover_expected_evidence() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cases: &[(&str, &[&str])] = &[
        ("credential precedence", &["src/adapters/credentials.rs"]),
        (
            "zdr data_collection deny require_parameters provider policy",
            &["src/adapters/openai_compatible.rs"],
        ),
        (
            "validate hallucinated source line ranges",
            &[
                "src/application/scan.rs",
                "src/adapters/openai_compatible.rs",
            ],
        ),
        (
            "metrics exclude source questions and paths",
            &["src/adapters/metrics.rs"],
        ),
        (
            "reject symlink ancestor filesystem",
            &["src/adapters/filesystem.rs"],
        ),
        (
            "strict evidence token budget retrieval",
            &["src/application/retrieve.rs"],
        ),
        ("doctor command CLI dispatch", &["src/main.rs"]),
        (
            "ripgrep timeout bounded streaming",
            &["src/adapters/ripgrep.rs"],
        ),
    ];
    for (question, expected_paths) in cases {
        let (result, _) = execute(
            &RipgrepSearch,
            &SecureFilesystem,
            &RetrieveInput {
                question: (*question).to_owned(),
                cwd: root.clone(),
                limits: Limits::default(),
                budget_tokens: 1_200,
                context_lines: 8,
                max_hits: 40,
            },
        )
        .unwrap_or_else(|error| panic!("evaluation query failed: {question}: {error}"));
        assert!(
            result
                .chunks
                .iter()
                .any(|chunk| expected_paths.contains(&chunk.path.as_str())),
            "no accepted evidence for {question}; got {:?}",
            result
                .chunks
                .iter()
                .map(|chunk| chunk.path.as_str())
                .collect::<Vec<_>>()
        );
        assert!(result.estimated_tokens <= 1_200);
    }
}
