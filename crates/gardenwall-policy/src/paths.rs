//! Workspace-relative path validation and classification (ADR-011 §2.2–2.4).
//!
//! This module is pure: it never touches the filesystem. It decides whether a
//! model-supplied relative path is acceptable at all, and whether it names a
//! persistence-risk file (an edit can later run code outside the sandbox) or
//! a secret file (hidden by default).
//!
//! The rules are policy data. They are versioned by [`PATH_RULES_VERSION`]
//! and covered by tests; matching is case-insensitive and errs toward asking.

use std::fmt;

/// Bump when the classification rules change.
/// Version 7 (2026-11-24, ADR-022 hardening): mise's settings, the Gradle
/// wrapper's files and Maven's `.mvn` folder.
/// Version 8 (2026-11-25, Gradle offline builds): `gradle.properties` (it
/// can add a Java agent to every build) and everything in `buildSrc` (code
/// Gradle compiles and runs when it reads the build).
pub const PATH_RULES_VERSION: u32 = 8;

/// Longest accepted relative path, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;
/// Longest accepted path component, in bytes.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// A validated workspace-relative path: normalized components, never empty,
/// never absolute, never containing `.`/`..`, drive letters, alternate data
/// streams or device names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath {
    components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    TooLong,
    Absolute,
    /// `.` or `..`, or an empty component other than a trailing separator.
    Traversal,
    /// Control characters or one of `< > : " | ? *` (colon also blocks drive
    /// letters and NTFS alternate data streams).
    ForbiddenCharacter,
    /// Windows silently strips trailing dots and spaces, so `a.` and `a`
    /// could name the same file.
    TrailingDotOrSpace,
    /// `CON`, `NUL`, `COM1`, ... with or without an extension.
    DeviceName,
    ComponentTooLong,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "path is empty",
            Self::TooLong => "path is too long",
            Self::Absolute => "path must be relative to the workspace",
            Self::Traversal => "path may not contain '.' or '..' components",
            Self::ForbiddenCharacter => "path contains a forbidden character",
            Self::TrailingDotOrSpace => "path component ends with a dot or space",
            Self::DeviceName => "path names a device",
            Self::ComponentTooLong => "path component is too long",
        })
    }
}

impl std::error::Error for PathError {}

const DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "clock$", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "com¹", "com²", "com³", "lpt1", "lpt2", "lpt3", "lpt4",
    "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "lpt¹", "lpt²", "lpt³",
];

impl RelPath {
    /// Validate a relative path. Both `/` and `\` separate components; one
    /// trailing separator is tolerated.
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.is_empty() {
            return Err(PathError::Empty);
        }
        if raw.len() > MAX_PATH_BYTES {
            return Err(PathError::TooLong);
        }
        if raw.starts_with(['/', '\\']) {
            return Err(PathError::Absolute);
        }
        let trimmed = raw.strip_suffix(['/', '\\']).unwrap_or(raw);
        let mut components = Vec::new();
        for part in trimmed.split(['/', '\\']) {
            components.push(validate_component(part)?);
        }
        if components.is_empty() {
            return Err(PathError::Empty);
        }
        Ok(Self { components })
    }

    pub fn components(&self) -> &[String] {
        &self.components
    }

    pub fn file_name(&self) -> &str {
        self.components.last().map_or("", String::as_str)
    }

    /// Forward-slash form, for display and storage.
    pub fn as_display(&self) -> String {
        self.components.join("/")
    }

    /// Lowercased components, for case-insensitive rule matching.
    fn lower(&self) -> Vec<String> {
        self.components.iter().map(|c| c.to_lowercase()).collect()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_display())
    }
}

fn validate_component(part: &str) -> Result<String, PathError> {
    if part.is_empty() || part == "." || part == ".." {
        return Err(PathError::Traversal);
    }
    if part.len() > MAX_COMPONENT_BYTES {
        return Err(PathError::ComponentTooLong);
    }
    if part
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Err(PathError::ForbiddenCharacter);
    }
    if part.ends_with(['.', ' ']) {
        return Err(PathError::TrailingDotOrSpace);
    }
    let lower = part.to_lowercase();
    let stem = lower.split('.').next().unwrap_or("").trim_end();
    if DEVICE_NAMES.contains(&stem) {
        return Err(PathError::DeviceName);
    }
    Ok(part.to_string())
}

/// What kind of file a path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    Normal,
    /// Editing it can later run code outside the sandbox (ADR-011 §2.3).
    PersistenceRisk(PersistenceRule),
    /// Hidden by default; revealing needs approval (ADR-011 §2.4).
    Secret(SecretRule),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceRule {
    GitInternals,
    GitAttributesOrModules,
    GitHookManager,
    Ci,
    EditorTasks,
    PackageScripts,
    BuildScript,
    ToolchainConfig,
    ShellProfile,
    Direnv,
    /// Instructions and skills that steer every later agent session.
    AgentInstructions,
    /// Python files that run without being named: `.pth` and
    /// `sitecustomize.py` run at every interpreter start, installed packages
    /// and virtual environments run when the project's Python is used, and
    /// compiled bytecode runs instead of the source it claims to match.
    PythonStartup,
}

impl PersistenceRule {
    pub fn describe(self) -> &'static str {
        match self {
            Self::GitInternals => "Git internals (hooks and config run commands)",
            Self::GitAttributesOrModules => {
                "Git attributes or submodules (can configure filters that run commands)"
            }
            Self::GitHookManager => "a Git hook manager configuration",
            Self::Ci => "a CI pipeline definition",
            Self::EditorTasks => "editor task, launch or dev-container settings",
            Self::PackageScripts => "a package manifest whose scripts run automatically",
            Self::BuildScript => "a build script that runs during builds",
            Self::ToolchainConfig => "toolchain configuration that can change what runs",
            Self::ShellProfile => "a shell startup file",
            Self::Direnv => "a direnv file that runs when entering the folder",
            Self::AgentInstructions => {
                "agent instructions, a session handoff, skills or MCP configuration (they steer later agent sessions, and some start programs)"
            }
            Self::PythonStartup => {
                "Python startup code, an installed package or compiled bytecode (runs when Python starts or imports it)"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRule {
    EnvFile,
    PrivateKey,
    SshKey,
    CredentialStore,
    PasswordDatabase,
    SecretsFile,
}

impl SecretRule {
    pub fn describe(self) -> &'static str {
        match self {
            Self::EnvFile => "an environment file (often holds API keys)",
            Self::PrivateKey => "a private key or certificate store",
            Self::SshKey => "an SSH key",
            Self::CredentialStore => "a credential file",
            Self::PasswordDatabase => "a password database",
            Self::SecretsFile => "a secrets file",
        }
    }
}

/// Classify a validated path. Secret rules take precedence.
pub fn classify(path: &RelPath) -> PathClass {
    let lower = path.lower();
    if let Some(rule) = secret_rule(&lower) {
        return PathClass::Secret(rule);
    }
    if let Some(rule) = persistence_rule(&lower) {
        return PathClass::PersistenceRisk(rule);
    }
    PathClass::Normal
}

fn ends_with_any(name: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|s| name.ends_with(s))
}

fn secret_rule(c: &[String]) -> Option<SecretRule> {
    let name = c.last()?.as_str();
    let parent = c.len().checked_sub(2).map(|i| c[i].as_str());
    if name == ".env"
        || (name.starts_with(".env.")
            && !matches!(
                name,
                ".env.example" | ".env.sample" | ".env.template" | ".env.dist"
            ))
    {
        return Some(SecretRule::EnvFile);
    }
    if ends_with_any(
        name,
        &[
            ".pem",
            ".key",
            ".p12",
            ".pfx",
            ".keystore",
            ".jks",
            ".p8",
            ".ppk",
            ".ovpn",
        ],
    ) {
        return Some(SecretRule::PrivateKey);
    }
    let ssh_key = [
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "id_ecdsa_sk",
        "id_ed25519_sk",
    ]
    .iter()
    .any(|k| {
        name == *k
            || (name.starts_with(k)
                && !name.ends_with(".pub")
                && name[k.len()..].starts_with(['.', '-', '_']))
    });
    // Everything in `.ssh` except public keys and known hosts: `config` can
    // also hold `ProxyCommand` lines that run commands.
    let in_ssh_dir = parent == Some(".ssh") && !name.ends_with(".pub") && name != "known_hosts";
    if ssh_key || in_ssh_dir {
        return Some(SecretRule::SshKey);
    }
    if matches!(
        name,
        ".netrc"
            | "_netrc"
            | ".npmrc"
            | ".pypirc"
            | ".git-credentials"
            | ".htpasswd"
            | "credentials.json"
            | "service-account.json"
            | ".dockercfg"
            | ".pgpass"
            | ".my.cnf"
            | ".vault-token"
            | "kubeconfig"
            | ".terraformrc"
            | ".boto"
            | ".s3cfg"
            | "settings.local.json"
            | "local.settings.json"
    ) || (parent == Some(".aws") && matches!(name, "credentials" | "config"))
        || (parent == Some(".docker") && name == "config.json")
        || (parent == Some(".kube") && name == "config")
        || (parent == Some("gcloud") && name.ends_with(".json"))
    {
        return Some(SecretRule::CredentialStore);
    }
    if ends_with_any(name, &[".kdbx", ".kdb", ".1pux", ".agilekeychain"]) {
        return Some(SecretRule::PasswordDatabase);
    }
    let stem = name.split('.').next().unwrap_or("");
    if matches!(stem, "secrets" | "secret")
        && ends_with_any(name, &[".yml", ".yaml", ".json", ".toml", ".env", ".ini"])
    {
        return Some(SecretRule::SecretsFile);
    }
    // Terraform variables and state hold credentials in plain text.
    if ends_with_any(name, &[".tfvars", ".tfvars.json"]) || name.starts_with("terraform.tfstate") {
        return Some(SecretRule::SecretsFile);
    }
    // Version 6: named environment files
    // (`prod.env`, Docker Compose's `db.env`). Example files (`example.env`,
    // `db.sample.env`) stay visible. (`.envrc` is a persistence risk, kept
    // read-only for commands by `protect`, not hidden.)
    if name
        .strip_suffix(".env")
        .is_some_and(|before| !ends_with_any(before, ENV_EXAMPLES))
    {
        return Some(SecretRule::EnvFile);
    }
    None
}

/// What a named environment file ends with when it is only an example.
const ENV_EXAMPLES: &[&str] = &["example", "sample", "template", "dist"];

/// The secret rules as `.gitignore` lines, each with a path it must cover
/// (`None` for the lines that keep example files and public keys visible).
/// Kept next to [`secret_rule`] and tested against it, so what Gardenwall
/// hides and what it offers to ignore don't drift apart.
pub const GITIGNORE_SECRETS: &[(&str, Option<&str>)] = &[
    (".env", Some(".env")),
    (".env.*", Some("api/.env.production")),
    ("!.env.example", None),
    ("!.env.sample", None),
    ("!.env.template", None),
    ("!.env.dist", None),
    ("*.env", Some("deploy/prod.env")),
    ("!*example.env", None),
    ("!*sample.env", None),
    ("!*template.env", None),
    ("!*dist.env", None),
    ("*.pem", Some("tls/server.pem")),
    ("*.key", Some("tls/private.key")),
    ("*.p12", Some("cert.p12")),
    ("*.pfx", Some("cert.pfx")),
    ("*.p8", Some("AuthKey.p8")),
    ("*.keystore", Some("release.keystore")),
    ("*.jks", Some("release.jks")),
    ("*.ppk", Some("server.ppk")),
    ("*.ovpn", Some("work.ovpn")),
    ("id_rsa*", Some("id_rsa")),
    ("id_dsa*", Some("id_dsa")),
    ("id_ecdsa*", Some("id_ecdsa")),
    ("id_ed25519*", Some("keys/id_ed25519")),
    ("!id_*.pub", None),
    (".ssh/", Some(".ssh/work_key")),
    (".aws/", Some(".aws/credentials")),
    (".kube/", Some(".kube/config")),
    (".docker/config.json", Some(".docker/config.json")),
    (".netrc", Some(".netrc")),
    ("_netrc", Some("_netrc")),
    (".npmrc", Some(".npmrc")),
    (".pypirc", Some(".pypirc")),
    (".git-credentials", Some(".git-credentials")),
    (".htpasswd", Some(".htpasswd")),
    (".dockercfg", Some(".dockercfg")),
    (".pgpass", Some(".pgpass")),
    (".my.cnf", Some(".my.cnf")),
    (".vault-token", Some(".vault-token")),
    (".terraformrc", Some(".terraformrc")),
    (".boto", Some(".boto")),
    (".s3cfg", Some(".s3cfg")),
    ("kubeconfig", Some("kubeconfig")),
    ("credentials.json", Some("credentials.json")),
    ("service-account.json", Some("service-account.json")),
    ("settings.local.json", Some(".claude/settings.local.json")),
    ("local.settings.json", Some("local.settings.json")),
    ("*.kdbx", Some("passwords.kdbx")),
    ("*.kdb", Some("passwords.kdb")),
    ("*.1pux", Some("export.1pux")),
    ("*.agilekeychain", Some("old.agilekeychain")),
    ("secrets.yml", Some("config/secrets.yml")),
    ("secrets.yaml", Some("config/secrets.yaml")),
    ("secrets.json", Some("secrets.json")),
    ("secrets.toml", Some("secrets.toml")),
    ("secrets.env", Some("secrets.env")),
    ("secrets.ini", Some("secrets.ini")),
    ("secret.yml", Some("secret.yml")),
    ("secret.yaml", Some("secret.yaml")),
    ("secret.json", Some("secret.json")),
    ("secret.toml", Some("secret.toml")),
    ("*.tfvars", Some("prod.tfvars")),
    ("*.tfvars.json", Some("prod.tfvars.json")),
    ("terraform.tfstate*", Some("terraform.tfstate.backup")),
];

fn persistence_rule(c: &[String]) -> Option<PersistenceRule> {
    let name = c.last()?.as_str();
    let has_dir = |d: &str| c[..c.len() - 1].iter().any(|x| x == d);
    let first = c.first().map(String::as_str);

    if c.iter().any(|x| x == ".git") {
        return Some(PersistenceRule::GitInternals);
    }
    if matches!(
        name,
        "agents.md" | "claude.md" | "gemini.md" | ".cursorrules" | ".clinerules" | ".mcp.json"
    ) || has_dir(".gardenwall")
        // Version 4: a session handoff, which a later session starts from
        // (saving one into the project asks).
        || name == "handoff.md"
        || has_dir(".claude")
        || has_dir(".cursor")
        // Version 3: other agents' configuration (rules, hooks, MCP servers).
        || has_dir(".windsurf")
        || has_dir(".continue")
        || has_dir(".clinerules")
    {
        return Some(PersistenceRule::AgentInstructions);
    }
    // Python code that runs without being named (version 2). A virtual
    // environment is matched by its usual folder names: its scripts and
    // packages run whenever the project's Python is used on the host.
    if name.ends_with(".pth")
        || name.ends_with(".pyc")
        || matches!(name, "sitecustomize.py" | "usercustomize.py" | "pyvenv.cfg")
        || [
            "__pycache__",
            "site-packages",
            "dist-packages",
            ".venv",
            "venv",
        ]
        .iter()
        .any(|d| has_dir(d))
    {
        return Some(PersistenceRule::PythonStartup);
    }
    if matches!(name, ".gitattributes" | ".gitmodules" | ".gitconfig") {
        return Some(PersistenceRule::GitAttributesOrModules);
    }
    if has_dir(".husky")
        || matches!(
            name,
            ".pre-commit-config.yaml"
                | "lefthook.yml"
                | "lefthook.yaml"
                | ".lefthook.yml"
                | ".huskyrc"
                | ".huskyrc.json"
        )
    {
        return Some(PersistenceRule::GitHookManager);
    }
    let in_github = c[..c.len() - 1]
        .windows(2)
        .any(|w| w[0] == ".github" && matches!(w[1].as_str(), "workflows" | "actions"));
    if in_github
        || has_dir(".circleci")
        || has_dir(".buildkite")
        || has_dir(".gitea")
        || has_dir(".forgejo")
        || matches!(
            name,
            ".gitlab-ci.yml"
                | ".travis.yml"
                | "azure-pipelines.yml"
                | "bitbucket-pipelines.yml"
                | "jenkinsfile"
                | ".drone.yml"
                | "appveyor.yml"
                | ".appveyor.yml"
                | "cloudbuild.yaml"
                | "cloudbuild.yml"
        )
    {
        return Some(PersistenceRule::Ci);
    }
    if has_dir(".vscode")
        || has_dir(".idea")
        || has_dir(".devcontainer")
        || has_dir(".fleet")
        || has_dir(".zed")
        || name.ends_with(".code-workspace")
        || name == ".devcontainer.json"
    {
        return Some(PersistenceRule::EditorTasks);
    }
    if matches!(
        name,
        "package.json"
            | ".yarnrc"
            | ".yarnrc.yml"
            | "composer.json"
            | "gemfile"
            | "rakefile"
            | "pnpmfile.cjs"
            | ".pnpmfile.cjs"
    ) {
        return Some(PersistenceRule::PackageScripts);
    }
    if matches!(
        name,
        "build.rs"
            | "setup.py"
            | "setup.cfg"
            | "pyproject.toml"
            | "conftest.py"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "gradlew"
            | "gradlew.bat"
            | "mvnw"
            | "mvnw.cmd"
            | "pom.xml"
            | "directory.build.props"
            | "directory.build.targets"
            | "binding.gyp"
            | "gradle-wrapper.properties"
            | "gradle-wrapper.jar"
            | "gradle.properties"
    ) || has_dir(".mvn")
        || has_dir("buildsrc")
        || name.ends_with(".csproj")
        || name.ends_with(".vcxproj")
        || name.ends_with(".fsproj")
        || name.ends_with(".vbproj")
    {
        return Some(PersistenceRule::BuildScript);
    }
    if (has_dir(".cargo") && name.starts_with("config"))
        || matches!(
            name,
            "rust-toolchain"
                | "rust-toolchain.toml"
                | ".tool-versions"
                | ".nvmrc"
                | ".node-version"
                | ".python-version"
                | "global.json"
                | "nuget.config"
                | "mise.toml"
                | ".mise.toml"
        )
    {
        return Some(PersistenceRule::ToolchainConfig);
    }
    if matches!(
        name,
        ".bashrc"
            | ".bash_profile"
            | ".bash_login"
            | ".bash_logout"
            | ".profile"
            | ".zshrc"
            | ".zprofile"
            | ".zshenv"
            | ".zlogin"
            | ".zlogout"
            | ".cshrc"
            | ".tcshrc"
            | ".kshrc"
            | ".inputrc"
            | "microsoft.powershell_profile.ps1"
            | "profile.ps1"
            | "microsoft.vscode_profile.ps1"
    ) || (first == Some(".config") && c.get(1).is_some_and(|d| d == "fish"))
    {
        return Some(PersistenceRule::ShellProfile);
    }
    if matches!(name, ".envrc" | ".direnvrc" | "direnvrc") {
        return Some(PersistenceRule::Direnv);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(p: &str) -> PathClass {
        classify(&RelPath::parse(p).unwrap())
    }

    #[test]
    fn valid_paths_normalize_separators() {
        let p = RelPath::parse("src\\lib.rs").unwrap();
        assert_eq!(p.as_display(), "src/lib.rs");
        assert_eq!(RelPath::parse("docs/").unwrap().as_display(), "docs");
        assert_eq!(
            RelPath::parse("a b/ünï.txt").unwrap().file_name(),
            "ünï.txt"
        );
    }

    #[test]
    fn escapes_and_ambiguous_names_are_rejected() {
        use PathError::*;
        let cases: &[(&str, PathError)] = &[
            ("", Empty),
            ("/etc/passwd", Absolute),
            ("\\Windows", Absolute),
            ("\\\\server\\share", Absolute),
            ("C:\\x", ForbiddenCharacter),
            ("C:x", ForbiddenCharacter),
            ("../x", Traversal),
            ("a/../../x", Traversal),
            ("a/./b", Traversal),
            ("a//b", Traversal),
            ("file.txt:stream", ForbiddenCharacter),
            ("file.txt::$DATA", ForbiddenCharacter),
            ("a\u{0}b", ForbiddenCharacter),
            ("a\nb", ForbiddenCharacter),
            ("what?", ForbiddenCharacter),
            ("a*", ForbiddenCharacter),
            ("name.", TrailingDotOrSpace),
            ("name ", TrailingDotOrSpace),
            ("CON", DeviceName),
            ("nul.txt", DeviceName),
            ("src/Com1.rs", DeviceName),
            ("LPT9", DeviceName),
            ("conout$", DeviceName),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                RelPath::parse(raw).as_ref().err(),
                Some(expected),
                "{raw:?}"
            );
        }
        assert_eq!(RelPath::parse(&"a/".repeat(3000)), Err(TooLong));
        assert_eq!(RelPath::parse(&"x".repeat(256)), Err(ComponentTooLong));
        // Similar-looking names that are fine.
        for ok in [
            "console.log",
            "conf/x",
            "com10",
            "nullable.rs",
            "auxiliary.md",
            ".gitignore",
        ] {
            assert!(RelPath::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn persistence_risk_paths() {
        use PersistenceRule::*;
        let cases: &[(&str, PersistenceRule)] = &[
            (".git/hooks/pre-commit", GitInternals),
            (".git/config", GitInternals),
            ("sub/.GIT/HEAD", GitInternals),
            (".gitattributes", GitAttributesOrModules),
            (".gitmodules", GitAttributesOrModules),
            (".husky/pre-push", GitHookManager),
            (".pre-commit-config.yaml", GitHookManager),
            (".github/workflows/ci.yml", Ci),
            (".github/actions/setup/action.yml", Ci),
            (".gitlab-ci.yml", Ci),
            ("Jenkinsfile", Ci),
            (".circleci/config.yml", Ci),
            (".vscode/tasks.json", EditorTasks),
            (".vscode/settings.json", EditorTasks),
            (".idea/workspace.xml", EditorTasks),
            (".devcontainer/devcontainer.json", EditorTasks),
            ("project.code-workspace", EditorTasks),
            ("package.json", PackageScripts),
            ("web/package.json", PackageScripts),
            ("build.rs", BuildScript),
            ("crates/x/build.rs", BuildScript),
            ("setup.py", BuildScript),
            ("pyproject.toml", BuildScript),
            ("conftest.py", BuildScript),
            ("App.csproj", BuildScript),
            ("Directory.Build.props", BuildScript),
            (".cargo/config.toml", ToolchainConfig),
            ("rust-toolchain.toml", ToolchainConfig),
            // Version 7.
            ("gradle/wrapper/gradle-wrapper.properties", BuildScript),
            ("gradle/wrapper/gradle-wrapper.jar", BuildScript),
            (".mvn/wrapper/maven-wrapper.properties", BuildScript),
            ("mise.toml", ToolchainConfig),
            // Version 8.
            ("gradle.properties", BuildScript),
            ("buildSrc/src/main/kotlin/Conventions.kt", BuildScript),
            ("app/build.gradle.kts", BuildScript),
            (".bashrc", ShellProfile),
            (".zshrc", ShellProfile),
            ("Microsoft.PowerShell_profile.ps1", ShellProfile),
            (".config/fish/config.fish", ShellProfile),
            (".envrc", Direnv),
            ("AGENTS.md", AgentInstructions),
            ("HANDOFF.md", AgentInstructions),
            ("notes/handoff.md", AgentInstructions),
            ("docs/CLAUDE.md", AgentInstructions),
            (".gardenwall/skills/release/SKILL.md", AgentInstructions),
            (".claude/commands/x.md", AgentInstructions),
            (".claude/settings.json", AgentInstructions),
            (".mcp.json", AgentInstructions),
            (".windsurf/rules/x.md", AgentInstructions),
            (".continue/config.yaml", AgentInstructions),
            ("evil.pth", PythonStartup),
            ("lib/sitecustomize.py", PythonStartup),
            ("usercustomize.py", PythonStartup),
            (".venv/pyvenv.cfg", PythonStartup),
            (".venv/Scripts/activate.ps1", PythonStartup),
            (
                "venv/lib/python3.13/site-packages/pkg/__init__.py",
                PythonStartup,
            ),
            ("vendor/site-packages/x.py", PythonStartup),
            ("pkg/__pycache__/mod.cpython-313.pyc", PythonStartup),
            ("mod.pyc", PythonStartup),
        ];
        for (p, rule) in cases {
            assert_eq!(class(p), PathClass::PersistenceRisk(*rule), "{p}");
        }
    }

    #[test]
    fn secret_paths_take_precedence() {
        use SecretRule::*;
        let cases: &[(&str, SecretRule)] = &[
            (".env", EnvFile),
            (".env.local", EnvFile),
            ("api/.env.production", EnvFile),
            ("server.pem", PrivateKey),
            ("tls/private.key", PrivateKey),
            ("cert.pfx", PrivateKey),
            ("id_rsa", SshKey),
            ("id_ed25519", SshKey),
            ("keys/id_rsa.bak", SshKey),
            (".ssh/work_key", SshKey),
            (".netrc", CredentialStore),
            (".npmrc", CredentialStore),
            (".git-credentials", CredentialStore),
            (".aws/credentials", CredentialStore),
            (".docker/config.json", CredentialStore),
            ("vault.kdbx", PasswordDatabase),
            ("config/secrets.yaml", SecretsFile),
            ("the.env", EnvFile),
            ("docker/db.env", EnvFile),
            ("Production.ENV", EnvFile),
        ];
        for (p, rule) in cases {
            assert_eq!(class(p), PathClass::Secret(*rule), "{p}");
        }
        // A secret pattern inside .git is still a secret.
        assert_eq!(class(".git/x.pem"), PathClass::Secret(PrivateKey));
    }

    #[test]
    fn ordinary_files_are_normal() {
        for p in [
            "src/main.rs",
            "README.md",
            ".env.example",
            ".env.sample",
            "example.env",
            "docker/db.sample.env",
            "config.template.env",
            "environment.rs",
            ".envrc.md",
            "id_ed25519.pub",
            ".ssh/known_hosts",
            ".ssh/id_rsa.pub",
            "identity.rs",
            "id_utils.py",
            "keyboard.rs",
            "docs/secrets-management.md",
            ".github/ISSUE_TEMPLATE/bug.md",
            ".gitignore",
            "Cargo.toml",
            "tsconfig.json",
            "src/package_json.rs",
            // Ordinary Python stays ordinary.
            "app.py",
            "src/pkg/__init__.py",
            "tests/test_utils.py",
            "requirements.txt",
            "pathlib_notes.md",
            "src/venv_helper.py",
        ] {
            assert_eq!(class(p), PathClass::Normal, "{p}");
        }
    }
}

#[cfg(test)]
mod gitignore_tests {
    use super::*;

    #[test]
    fn every_gitignore_line_names_a_secret_and_new_rules_classify() {
        for (line, example) in GITIGNORE_SECRETS {
            match example {
                Some(path) => assert!(
                    matches!(
                        classify(&RelPath::parse(path).unwrap()),
                        PathClass::Secret(_)
                    ),
                    "{line}: {path} is not a secret path"
                ),
                None => assert!(line.starts_with('!'), "{line}"),
            }
        }
        // Files the lines starting with `!` keep visible are not secrets.
        for path in [".env.example", "id_rsa.pub", "keys/id_ed25519.pub"] {
            assert_eq!(
                classify(&RelPath::parse(path).unwrap()),
                PathClass::Normal,
                "{path}"
            );
        }
        // Ordinary files next to the new rules.
        for path in [
            "main.tf",
            "variables.tf",
            "settings.json",
            "kube.rs",
            "my.cnf.md",
        ] {
            assert_eq!(
                classify(&RelPath::parse(path).unwrap()),
                PathClass::Normal,
                "{path}"
            );
        }
    }
}
