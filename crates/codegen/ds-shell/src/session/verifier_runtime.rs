//! Kernel-enforced shell boundary and evidence trace for final verifiers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use crate::terminal::{AsyncTerminalRunner, TerminalError, TerminalRunRequest, TerminalRunResult};
use ds_tools::implementations::ds_build::task::types::VerifierSandboxSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VerificationToolEvent {
    pub tool_event_id: String,
    pub exact_input_digest: String,
    pub observed_output_digest: String,
    pub success: bool,
    #[serde(skip)]
    pub exact_input: String,
}

static TRACES: LazyLock<Mutex<HashMap<String, Vec<VerificationToolEvent>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct TraceGuard {
    critic_id: String,
    active: bool,
}

pub(crate) fn begin_trace(critic_id: &str) -> Result<TraceGuard, String> {
    let mut traces = TRACES
        .lock()
        .map_err(|_| "verification trace registry is poisoned".to_string())?;
    if traces.insert(critic_id.to_string(), Vec::new()).is_some() {
        return Err("duplicate verification critic identity".to_string());
    }
    Ok(TraceGuard {
        critic_id: critic_id.to_string(),
        active: true,
    })
}

impl TraceGuard {
    pub(crate) fn finish(mut self) -> Result<Vec<VerificationToolEvent>, String> {
        let trace = TRACES
            .lock()
            .map_err(|_| "verification trace registry is poisoned".to_string())?
            .remove(&self.critic_id)
            .ok_or_else(|| "verification trace is missing".to_string())?;
        self.active = false;
        Ok(trace)
    }
}

impl Drop for TraceGuard {
    fn drop(&mut self) {
        if self.active
            && let Ok(mut traces) = TRACES.lock()
        {
            traces.remove(&self.critic_id);
        }
    }
}

pub(crate) fn trace_contains(
    trace: &[VerificationToolEvent],
    event_id: &str,
    input_digest: &str,
    output_digest: &str,
    artifact_path: &str,
    artifact_target: &str,
) -> bool {
    trace.iter().any(|event| {
        event.success
            && event.tool_event_id == event_id
            && event.exact_input_digest == input_digest
            && event.observed_output_digest == output_digest
            && event.exact_input.contains(artifact_path)
            && event.exact_input.contains(artifact_target)
    })
}

pub(crate) struct VerifierTerminalRunner {
    inner: Arc<dyn AsyncTerminalRunner>,
    reviewed_root: PathBuf,
    scratch_root: PathBuf,
    critic_id: String,
}

impl VerifierTerminalRunner {
    pub(crate) fn new(
        inner: Arc<dyn AsyncTerminalRunner>,
        spec: &VerifierSandboxSpec,
        critic_id: String,
    ) -> Result<Self, String> {
        let reviewed_root = canonical_directory(&spec.reviewed_root, "reviewed root")?;
        let scratch_root = canonical_directory(&spec.scratch_root, "critic scratch")?;
        if reviewed_root == scratch_root
            || reviewed_root.starts_with(&scratch_root)
            || scratch_root.starts_with(&reviewed_root)
        {
            return Err("reviewed root and critic scratch overlap".to_string());
        }
        Ok(Self {
            inner,
            reviewed_root,
            scratch_root,
            critic_id,
        })
    }

    fn sandboxed_command(&self, command: &str, cwd: &Path) -> Result<String, TerminalError> {
        let cwd = dunce::canonicalize(cwd)
            .map_err(|error| TerminalError::Other(format!("invalid verifier cwd: {error}")))?;
        if !cwd.starts_with(&self.reviewed_root) && !cwd.starts_with(&self.scratch_root) {
            return Err(TerminalError::Other(
                "final verifier cwd is outside its reviewed tree and scratch".to_string(),
            ));
        }

        #[cfg(target_os = "macos")]
        {
            let scratch = seatbelt_literal(&self.scratch_root);
            let profile = format!(
                "(version 1)\n\
                 (allow default)\n\
                 (deny file-write*\n\
                   (require-all\n\
                     (require-not (subpath \"{scratch}\"))\n\
                     (require-not (literal \"/dev/null\"))\n\
                     (require-not (literal \"/dev/tty\"))))"
            );
            return Ok(format!(
                "/usr/bin/sandbox-exec -p {} /bin/zsh -f -c {}",
                shell_quote(&profile),
                shell_quote(command)
            ));
        }

        #[cfg(target_os = "linux")]
        {
            return Ok(format!(
                "bwrap --die-with-parent --ro-bind / / --bind {scratch} {scratch} \
                 --chdir {cwd} /bin/sh -lc {command}",
                scratch = shell_quote(&self.scratch_root.to_string_lossy()),
                cwd = shell_quote(&cwd.to_string_lossy()),
                command = shell_quote(command),
            ));
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = command;
            Err(TerminalError::Other(
                "kernel-enforced final-verifier shell isolation is unsupported on this platform"
                    .to_string(),
            ))
        }
    }
}

#[async_trait::async_trait]
impl AsyncTerminalRunner for VerifierTerminalRunner {
    async fn run(
        &self,
        mut request: TerminalRunRequest,
    ) -> Result<TerminalRunResult, TerminalError> {
        let original = request.command.clone();
        request.command = self.sandboxed_command(&original, request.cwd.as_path())?;
        // Some terminal backends open this path in the parent process, outside
        // the kernel sandbox. Verifiers receive output through the tool result,
        // so disabling the redundant log closes that write bypass.
        request.output_file = None;
        let scratch = self.scratch_root.to_string_lossy().into_owned();
        request.env.insert("TMPDIR".to_string(), scratch.clone());
        request.env.insert("TMP".to_string(), scratch.clone());
        request.env.insert("TEMP".to_string(), scratch.clone());
        request.env.insert(
            "CARGO_TARGET_DIR".to_string(),
            format!("{scratch}/cargo-target"),
        );
        request
            .env
            .insert("XDG_CACHE_HOME".to_string(), format!("{scratch}/cache"));
        request
            .env
            .insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());
        let mut result = self.inner.run(request).await?;
        let event = VerificationToolEvent {
            tool_event_id: uuid::Uuid::now_v7().to_string(),
            exact_input_digest: super::verification_snapshot::digest_bytes(original.as_bytes()),
            observed_output_digest: super::verification_snapshot::digest_bytes(
                result.combined_output.as_bytes(),
            ),
            success: result.exit_code == Some(0) && !result.timed_out && result.signal.is_none(),
            exact_input: original,
        };
        TRACES
            .lock()
            .map_err(|_| TerminalError::Other("verification trace registry is poisoned".into()))?
            .get_mut(&self.critic_id)
            .ok_or_else(|| TerminalError::Other("verification trace was not registered".into()))?
            .push(event.clone());
        result.combined_output.push_str(&format!(
            "\n[verification-evidence tool_event_id={} exact_input_digest={} \
             observed_output_digest={} success={}]\n",
            event.tool_event_id,
            event.exact_input_digest,
            event.observed_output_digest,
            event.success,
        ));
        Ok(result)
    }
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("{label} is missing: {error}"))?;
    if !metadata.file_type().is_dir() {
        return Err(format!("{label} is not a real directory"));
    }
    dunce::canonicalize(path).map_err(|error| format!("cannot resolve {label}: {error}"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn seatbelt_literal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{DEFAULT_OUTPUT_BYTE_LIMIT, LocalTerminalRunner};
    use ds_paths::AbsPathBuf;
    use std::collections::HashMap;
    use std::time::Duration;

    fn request(command: String, cwd: &Path) -> TerminalRunRequest {
        TerminalRunRequest {
            tool_call_id: agent_client_protocol::ToolCallId::new("verifier-test"),
            command,
            cwd: AbsPathBuf::new(cwd.to_path_buf()).unwrap(),
            env: HashMap::new(),
            timeout: Duration::from_secs(10),
            output_byte_limit: DEFAULT_OUTPUT_BYTE_LIMIT,
            stream: false,
            output_file: None,
        }
    }

    #[test]
    fn shell_quote_neutralizes_metacharacters() {
        assert_eq!(shell_quote("a'b;$HOME"), "'a'\\''b;$HOME'");
    }

    #[test]
    fn traces_are_one_shot_and_reject_duplicate_registration() {
        let id = uuid::Uuid::now_v7().to_string();
        let guard = begin_trace(&id).unwrap();
        assert!(begin_trace(&id).is_err());
        assert!(guard.finish().unwrap().is_empty());
        let second = begin_trace(&id).unwrap();
        drop(second);
        assert!(begin_trace(&id).is_ok());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn verifier_shell_can_write_only_to_scratch() {
        use std::os::unix::fs::PermissionsExt;

        let reviewed = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let reviewed_file = reviewed.path().join("artifact.txt");
        std::fs::write(&reviewed_file, "frozen\n").unwrap();
        let original_mode = std::fs::metadata(&reviewed_file)
            .unwrap()
            .permissions()
            .mode();

        let critic_id = uuid::Uuid::now_v7().to_string();
        let trace = begin_trace(&critic_id).unwrap();
        let runner = VerifierTerminalRunner::new(
            Arc::new(LocalTerminalRunner),
            &VerifierSandboxSpec {
                reviewed_root: reviewed.path().to_path_buf(),
                scratch_root: scratch.path().to_path_buf(),
            },
            critic_id,
        )
        .unwrap();
        let reviewed_q = shell_quote(&reviewed_file.to_string_lossy());
        let scratch_q = shell_quote(&scratch.path().to_string_lossy());

        let read = runner
            .run(request(
                format!("test \"$(cat {reviewed_q})\" = frozen"),
                reviewed.path(),
            ))
            .await
            .unwrap();
        assert_eq!(
            read.exit_code,
            Some(0),
            "reviewed artifact must be readable"
        );

        let scratch_write = runner
            .run(request(
                format!("printf allowed > {scratch_q}/allowed.txt"),
                scratch.path(),
            ))
            .await
            .unwrap();
        assert_eq!(
            scratch_write.exit_code,
            Some(0),
            "critic scratch must remain writable: {}",
            scratch_write.combined_output
        );

        let attacks = [
            format!("printf changed > {reviewed_q}"),
            format!("mv {reviewed_q} {scratch_q}/renamed.txt"),
            format!("rm {reviewed_q}"),
            format!("chmod 600 {reviewed_q}"),
            format!(
                "printf replacement > {scratch_q}/replacement && mv -f {scratch_q}/replacement {reviewed_q}"
            ),
            format!("ln -s {reviewed_q} {scratch_q}/link && printf linked > {scratch_q}/link"),
            format!("ln {reviewed_q} {scratch_q}/hardlink && printf linked > {scratch_q}/hardlink"),
        ];
        for attack in attacks {
            let result = runner
                .run(request(attack.clone(), scratch.path()))
                .await
                .unwrap();
            assert_ne!(
                result.exit_code,
                Some(0),
                "sandbox unexpectedly allowed reviewed-tree mutation: {attack}"
            );
            assert_eq!(std::fs::read_to_string(&reviewed_file).unwrap(), "frozen\n");
            assert_eq!(
                std::fs::metadata(&reviewed_file)
                    .unwrap()
                    .permissions()
                    .mode(),
                original_mode
            );
        }

        assert_eq!(
            std::fs::read_to_string(scratch.path().join("allowed.txt")).unwrap(),
            "allowed"
        );
        let events = trace.finish().unwrap();
        assert_eq!(events.len(), 9);
        assert_eq!(events.iter().filter(|event| event.success).count(), 2);
    }
}
