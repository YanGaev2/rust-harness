#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    program: String,
    args: Vec<String>,
}

impl ShellProfile {
    pub fn native() -> Self {
        #[cfg(windows)]
        {
            Self {
                program: "powershell.exe".to_string(),
                args: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-Command".to_string(),
                ],
            }
        }

        #[cfg(target_os = "linux")]
        {
            Self {
                program: "bash".to_string(),
                args: vec!["-lc".to_string()],
            }
        }

        #[cfg(all(unix, not(target_os = "linux")))]
        {
            Self {
                program: "sh".to_string(),
                args: vec!["-lc".to_string()],
            }
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }
}
