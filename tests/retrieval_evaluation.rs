use std::{fs, path::PathBuf};

use agent_shunt::{
    adapters::{filesystem::SecureFilesystem, git::GitChangeSource, ripgrep::RipgrepSearch},
    application::retrieve::{
        MAX_BLOCK_LINES, MIN_SCORE_PERCENT, MMR_LAMBDA, RetrieveInput, execute,
    },
    domain::Limits,
};
use tempfile::tempdir;

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
            "metrics log jsonl record privacy sanitize",
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
        // This file and the eval corpus embed every case's question; integration
        // templates also embed its vocabulary. Exclude benchmark fixtures so a
        // query cannot retrieve its own ground truth as a perfect self-match.
        let globs = vec![
            "!tests/retrieval_evaluation.rs".to_owned(),
            "!eval/**".to_owned(),
            "!examples/**".to_owned(),
            "!integrations/**".to_owned(),
        ];
        let (result, _) = execute(
            &RipgrepSearch,
            &GitChangeSource,
            &SecureFilesystem,
            &RetrieveInput {
                question: (*question).to_owned(),
                cwd: root.clone(),
                limits: Limits::default(),
                budget_tokens: 1_200,
                context_lines: 8,
                max_hits: 40,
                globs,
                scope: None,
                mmr_lambda: MMR_LAMBDA,
                max_block_lines: MAX_BLOCK_LINES,
                min_score_percent: MIN_SCORE_PERCENT,
                why: false,
                prf: false,
                review: false,
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

#[test]
#[cfg(feature = "ast")]
fn final_submit_question_returns_the_actual_guard_within_a_small_budget() {
    use agent_shunt::{
        adapters::tree_sitter_chunker::AstResolver, application::retrieve::execute_with_resolver,
    };

    let root = tempdir().unwrap();
    let scripts = root.path().join("scripts");
    fs::create_dir(&scripts).unwrap();
    fs::write(
        scripts.join("recover-historical-pre-submit-authorizations.js"),
        "const RECOVERY_REVISION = 'historical-pre-submit-authorization';\n\
         const submitAuthorization = require('./submit-authorization');\n\
         function recoverHistoricalPreSubmitAuthorizations(application) {\n\
           return submitAuthorization.recover(application);\n\
         }\n",
    )
    .unwrap();
    fs::write(
        scripts.join("submit-authorization.js"),
        "async function authorizeRunFromDatabase(options) {\n\
           return { allowed: true, stage: options.stage };\n\
         }\n",
    )
    .unwrap();

    let mut audit = (1..=49)
        .map(|line| format!("// audit prelude {line}"))
        .collect::<Vec<_>>();
    audit.push("async function persistSubmissionAudit(audit, status) {".to_owned());
    let audit_detail = "recorded provenance confirmation browser state and timestamp for the current attempt, including resume evidence and persisted decision metadata";
    for line in 51..=77 {
        audit.push(format!("  const field{line} = '{audit_detail}';"));
    }
    audit.push("  const authorization = await authorizeRunFromDatabase({ runId: audit.attempt_id, audit, stage: 'final_submit' });".to_owned());
    audit.push("  if (!authorization.allowed) throw new Error('authorization_denied');".to_owned());
    for line in 80..=94 {
        audit.push(format!("  const detail{line} = '{audit_detail}';"));
    }
    audit.push("  return authorization;".to_owned());
    audit.push("}".to_owned());
    fs::write(scripts.join("submission-audit.js"), audit.join("\n")).unwrap();

    for prf in [false, true] {
        let (result, _) = execute_with_resolver(
            &RipgrepSearch,
            &GitChangeSource,
            &SecureFilesystem,
            &AstResolver::new(),
            None,
            None,
            None,
            &RetrieveInput {
            question:
                "Where is final submit authorization checked before an application is submitted?"
                    .to_owned(),
            cwd: root.path().to_path_buf(),
            limits: Limits::default(),
            budget_tokens: 1_500,
            context_lines: 8,
            max_hits: 200,
            globs: vec!["scripts/**".to_owned()],
            scope: None,
            mmr_lambda: MMR_LAMBDA,
            max_block_lines: MAX_BLOCK_LINES,
            min_score_percent: MIN_SCORE_PERCENT,
            why: false,
            prf,
            review: false,
            },
        )
        .unwrap();

        let first = result.chunks.first().expect("expected source evidence");
        assert_eq!(first.path, "scripts/submission-audit.js", "{result:?}");
        assert!(first.start_line <= 78 && first.end_line >= 78, "{result:?}");
        assert!(first.estimated_tokens <= 600, "guard chunk is too large");
        assert!(result.estimated_tokens <= 1_500);
    }
}

#[test]
fn why_lists_only_terms_found_in_each_chunk() {
    let root = tempdir().unwrap();
    let mut lines = vec!["const alpha = true;".to_owned()];
    lines.extend((2..=40).map(|line| format!("const filler{line} = true;")));
    lines.push("const beta = true;".to_owned());
    fs::write(root.path().join("mixed.js"), lines.join("\n")).unwrap();

    let (result, _) = execute(
        &RipgrepSearch,
        &GitChangeSource,
        &SecureFilesystem,
        &RetrieveInput {
            question: "alpha beta".to_owned(),
            cwd: root.path().to_path_buf(),
            limits: Limits::default(),
            budget_tokens: 1_000,
            context_lines: 2,
            max_hits: 40,
            globs: vec!["mixed.js".to_owned()],
            scope: None,
            mmr_lambda: MMR_LAMBDA,
            max_block_lines: MAX_BLOCK_LINES,
            min_score_percent: 0,
            why: true,
            prf: false,
            review: false,
        },
    )
    .unwrap();

    let alpha = result
        .chunks
        .iter()
        .find(|chunk| chunk.start_line <= 1 && chunk.end_line >= 1)
        .expect("alpha chunk");
    let beta = result
        .chunks
        .iter()
        .find(|chunk| chunk.start_line <= 41 && chunk.end_line >= 41)
        .expect("beta chunk");
    assert_eq!(alpha.matched_terms, Some(vec!["alpha".to_owned()]));
    assert_eq!(beta.matched_terms, Some(vec!["beta".to_owned()]));
}
