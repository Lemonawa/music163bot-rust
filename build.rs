use std::process::Command;

fn short_sha(input: &str) -> Option<String> {
    let sha = input.trim();
    if sha.is_empty() {
        return None;
    }
    Some(sha.chars().take(7).collect())
}

fn git_short_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    short_sha(&stdout)
}

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let sha = std::env::var("GITHUB_SHA")
        .ok()
        .and_then(|value| short_sha(&value))
        .or_else(git_short_sha)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BUILD_GIT_COMMIT={sha}");
}
