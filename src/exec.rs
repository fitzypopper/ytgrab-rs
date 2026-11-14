//! Running a command after (or before) a download.
//!
//! This is yt-dlp's `--exec` and `--exec-before-download`. The command is a
//! shell command; the fields of the video are substituted into it.

use std::process::Stdio;

use tokio::process::Command;

use crate::error::Result;

/// Run a command for a downloaded file.
///
/// `command` is the template, `fields` the values that can be substituted and
/// `path` the file the command is about.
pub async fn run(
    command: &str,
    fields: &crate::output::TemplateVars,
    path: &std::path::Path,
) -> Result<()> {
    let mut vars = fields.clone();
    vars.filepath = path.to_string_lossy().to_string();
    vars.filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let rendered = crate::output::render(command, &vars);
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&rendered)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    let status = child.wait().await?;
    if !status.success() {
        return Err(crate::error::Error::other(format!(
            "the command `{rendered}` failed with {status}"
        )));
    }
    Ok(())
}
