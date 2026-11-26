//! Binary analysis: a built-in analyzer and an
//! optional Ghidra decompiler, for the agent and for chat.
//!
//! - The core only reads the file's bytes (under the agent's policy, or
//!   from the native picker in chat). Parsing happens in the sandboxed
//!   `gardenwall-binary` worker: no network, its own folder only, 1 GB, one
//!   process, 60 s.
//! - Ghidra runs in its own sandbox: its install and the JDK read and
//!   execute only, a private project and scratch folder, no network, 6 GB.
//!   Projects are kept per file (by SHA-256) so later functions decompile
//!   quickly; the eight most recent are kept.
//! - Nothing is ever executed from the analyzed file. Reports are untrusted
//!   text from the file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gardenwall_agent::agent::{BinaryAnalyzer, BoxFuture};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::{AppState, Paths};

type CmdResult<T> = Result<T, String>;

/// Most bytes accepted back from the analyzer.
const ANALYZER_OUTPUT: usize = 256 << 10;
const ANALYZER_TIMEOUT: Duration = Duration::from_secs(60);
/// First analysis of a file in Ghidra (import plus auto-analysis).
const GHIDRA_FIRST_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const GHIDRA_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Ghidra projects kept for reuse.
const GHIDRA_PROJECTS_KEPT: usize = 8;
/// Most decompiled text returned (Pi's read limit is 50 KB).
const MAX_DECOMPILED: usize = 48 * 1024;
/// Largest file analyzed (the worker's own limit too).
pub const MAX_FILE_BYTES: u64 = gardenwall_agent::tools::MAX_BINARY_BYTES;

/// Settings → Binary analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct BinarySettings {
    /// Ghidra's install folder (holds `support/` and `Ghidra/`).
    pub ghidra_dir: Option<PathBuf>,
    /// A JDK (holds `bin/java`).
    pub java_home: Option<PathBuf>,
}

fn java_exe(home: &Path) -> PathBuf {
    home.join("bin")
        .join(if cfg!(windows) { "java.exe" } else { "java" })
}

fn ghidra_jar(dir: &Path) -> PathBuf {
    dir.join("Ghidra/Framework/Utility/lib/Utility.jar")
}

/// Ghidra's version from `Ghidra/application.properties`.
fn ghidra_version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("Ghidra/application.properties")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("application.version="))
        .map(|v| v.trim().chars().take(32).collect())
}

/// A ready decompiler setup, checked.
#[derive(Debug, Clone)]
pub struct Ghidra {
    dir: PathBuf,
    java_home: PathBuf,
}

impl BinarySettings {
    /// The decompiler setup, or why it isn't usable.
    pub fn ghidra(&self) -> Result<Ghidra, String> {
        let dir = self.ghidra_dir.clone().ok_or("choose Ghidra's folder")?;
        // The same breadth rules as granted folders: the sandbox gets read
        // and execute access to everything below these.
        let dir = narrow_folder(&dir)?;
        if !ghidra_jar(&dir).is_file() || !dir.join("support/launch.properties").is_file() {
            return Err(format!(
                "{} doesn't look like a Ghidra install (Ghidra/Framework/Utility/lib/Utility.jar is missing)",
                dir.display()
            ));
        }
        let java_home = narrow_folder(&self.java_home.clone().ok_or("choose a JDK folder")?)?;
        if !java_exe(&java_home).is_file() {
            return Err(format!(
                "{} doesn't look like a JDK (bin/java is missing)",
                java_home.display()
            ));
        }
        Ok(Ghidra { dir, java_home })
    }
}

/// `folder` canonicalized, unless it is a drive root, the home folder or a
/// system folder (granting those would defeat the sandbox).
fn narrow_folder(folder: &Path) -> Result<PathBuf, String> {
    gardenwall_fs::Workspace::open(folder)
        .map(|w| w.root().to_path_buf())
        .map_err(|e| format!("{}: {e}", folder.display()))
}

/// A JDK found on this computer: `JAVA_HOME`, then the usual install
/// folders (the newest version first).
pub fn detect_java_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("JAVA_HOME").map(PathBuf::from)
        && java_exe(&h).is_file()
    {
        return Some(h);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for base in [
            std::env::var_os("ProgramFiles"),
            std::env::var_os("ProgramW6432"),
        ]
        .into_iter()
        .flatten()
        {
            let base = PathBuf::from(base);
            for vendor in [
                "Eclipse Adoptium",
                "Java",
                "Microsoft",
                "Zulu",
                "Amazon Corretto",
                "BellSoft",
            ] {
                roots.push(base.join(vendor));
            }
        }
    } else {
        roots.push(PathBuf::from("/usr/lib/jvm"));
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
    }
    let mut found: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flat_map(|d| d.filter_map(Result::ok).map(|e| e.path()))
        .flat_map(|p| [p.clone(), p.join("Contents/Home")])
        .filter(|p| java_exe(p).is_file())
        .collect();
    found.sort();
    found.pop()
}

/// The analyzer's arguments from the model's (or the UI's) request. Only
/// known options pass, as `key=value`; text is bounded and stripped of
/// control characters.
pub fn worker_args(args: &Value) -> Result<Vec<std::ffi::OsString>, String> {
    let op = match args.get("op").and_then(Value::as_str).unwrap_or("info") {
        "info" => "info",
        "strings" => "strings",
        "functions" => "functions",
        "disassemble" | "disasm" => "disasm",
        "hex" => "hex",
        other => {
            return Err(format!(
                "unknown op \"{}\"; use info, strings, functions, disassemble or hex",
                other.chars().take(40).collect::<String>()
            ));
        }
    };
    let mut out: Vec<std::ffi::OsString> = vec!["--op".into(), op.into()];
    let text = |v: &str| -> String {
        v.chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect::<String>()
            .trim()
            .to_string()
    };
    for (key, worker_key) in [("target", "target"), ("filter", "filter")] {
        if let Some(v) = args.get(key).and_then(Value::as_str) {
            let v = text(v);
            if !v.is_empty() {
                out.push(format!("{worker_key}={v}").into());
            }
        }
    }
    for (key, worker_key) in [
        ("count", "count"),
        ("min_length", "min"),
        ("offset", "offset"),
        ("limit", "limit"),
        ("length", "length"),
    ] {
        if let Some(n) = args.get(key).and_then(Value::as_f64) {
            if !n.is_finite() || n < 0.0 {
                return Err(format!("{key} must be a positive number"));
            }
            out.push(format!("{worker_key}={}", n as u64).into());
        }
    }
    if let Some(a) = args.get("address") {
        let parsed = match a {
            Value::String(s) => {
                let s = s.trim();
                match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    Some(h) => u64::from_str_radix(h, 16).ok(),
                    None => s.parse().ok(),
                }
            }
            Value::Number(n) => n.as_u64(),
            _ => None,
        }
        .ok_or("address must be a number like 0x401000")?;
        out.push(format!("address={parsed:#x}").into());
    }
    Ok(out)
}

/// Run the analyzer worker over `bytes`. Blocking.
pub fn analyze_blocking(
    paths: &Paths,
    args: Vec<std::ffi::OsString>,
    bytes: &[u8],
) -> Result<String, String> {
    let exe = crate::state::stage_helper(&paths.binary_dir, "gardenwall-binary")
        .map_err(|e| format!("the binary analyzer is unavailable: {e}"))?;
    let dir = exe.parent().ok_or("invalid analyzer path")?.to_path_buf();
    let out = crate::worker::run(
        crate::worker::WorkerSpec {
            profile: "Gardenwall.BinaryAnalyzer",
            program: &exe,
            args,
            read_execute: vec![dir.clone()],
            read_write: vec![],
            env: vec![],
            working_dir: dir,
            memory_bytes: 1 << 30,
            max_processes: 1,
            timeout: ANALYZER_TIMEOUT,
            stdout_limit: ANALYZER_OUTPUT,
            journal: &paths.grants,
        },
        bytes,
    )?;
    match out.exit {
        None => Err("the analysis took too long".into()),
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Some(3) => Err(out.stderr.trim().chars().take(500).collect()),
        Some(code) => Err(format!("the binary analyzer failed (exit code {code})")),
    }
}

/// The Ghidra post-script: decompile one function, or list functions.
/// Written to the run's scratch folder; the output file is the last
/// argument.
const GHIDRA_SCRIPT: &str = r#"// Gardenwall: decompile one function or list functions. Read-only.
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionManager;

public class GardenwallDecompile extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        String want = args.length > 1 ? args[0] : "";
        String out = args[args.length - 1];
        StringBuilder sb = new StringBuilder();
        FunctionManager fm = currentProgram.getFunctionManager();
        if (want.isEmpty() || want.equals("-")) {
            int n = 0;
            sb.append(fm.getFunctionCount()).append(" functions. Columns: address, size in bytes, name.\n");
            for (Function f : fm.getFunctions(true)) {
                if (n++ >= 3000) { sb.append("[more functions not listed]\n"); break; }
                sb.append("0x").append(f.getEntryPoint().toString()).append(' ')
                  .append(f.getBody().getNumAddresses()).append(' ').append(f.getName()).append('\n');
            }
        } else {
            Function f = null;
            if (want.startsWith("0x") || want.startsWith("0X")) {
                try {
                    Address a = toAddr(Long.parseUnsignedLong(want.substring(2), 16));
                    f = fm.getFunctionContaining(a);
                } catch (NumberFormatException e) { }
            }
            if (f == null) {
                for (Function g : fm.getFunctions(true)) {
                    if (g.getName().equals(want)) { f = g; break; }
                }
            }
            if (f == null) {
                sb.append("NOT FOUND: no function at or named ").append(want).append('\n');
            } else {
                DecompInterface d = new DecompInterface();
                d.openProgram(currentProgram);
                DecompileResults r = d.decompileFunction(f, 120, monitor);
                if (r.decompileCompleted()) {
                    sb.append("// ").append(f.getName()).append(" at 0x").append(f.getEntryPoint().toString()).append('\n');
                    sb.append(r.getDecompiledFunction().getC());
                } else {
                    sb.append("FAILED: ").append(r.getErrorMessage()).append('\n');
                }
                d.dispose();
            }
        }
        java.nio.file.Files.writeString(java.nio.file.Path.of(out), sb.toString());
    }
}
"#;

/// A Java agent for the decompiler's JVM. An AppContainer may not open the
/// drive root or the folders above its grants, and two JDK calls Ghidra
/// depends on need them on Windows:
/// - `File.createTempFile` asks for the drive root's longest file name
///   (GetVolumeInformation on `C:\`), so every import fails. The agent
///   makes that lookup return 255, NTFS's fixed limit.
/// - `Path.toRealPath` asks for the normalized name, which reads every
///   parent folder, so compiling Ghidra's post-script fails. When Windows
///   refuses, the agent returns the absolute, normalized path instead (a
///   missing file still fails as before).
///
/// It runs only inside the sandboxed JVM and changes nothing else. Each
/// change is applied separately and skipped if this JDK differs. It needs
/// the ClassFile API (JDK 24+).
const JDK_AGENT: &str = r#"// Gardenwall: let Ghidra work inside an AppContainer.
import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassTransform;
import java.lang.classfile.CodeBuilder;
import java.lang.classfile.CodeElement;
import java.lang.classfile.CodeTransform;
import java.lang.classfile.Label;
import java.lang.constant.ClassDesc;
import java.lang.constant.MethodTypeDesc;
import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.ProtectionDomain;

public class GardenwallJdkAgent {
    static byte[] nameMax(byte[] bytes) {
        ClassFile cf = ClassFile.of();
        return cf.transformClass(cf.parse(bytes), ClassTransform.transformingMethodBodies(
            mm -> mm.methodName().equalsString("getNameMax")
                && mm.methodType().equalsString("(Ljava/lang/String;)I"),
            new CodeTransform() {
                @Override public void accept(CodeBuilder b, CodeElement e) { }
                @Override public void atEnd(CodeBuilder b) { b.sipush(255); b.ireturn(); }
            }));
    }

    static byte[] realPath(byte[] bytes) {
        ClassDesc path = ClassDesc.of("sun.nio.fs.WindowsPath");
        ClassFile cf = ClassFile.of();
        return cf.transformClass(cf.parse(bytes), ClassTransform.transformingMethodBodies(
            mm -> mm.methodName().equalsString("toRealPath")
                && mm.methodType().equalsString("([Ljava/nio/file/LinkOption;)Lsun/nio/fs/WindowsPath;"),
            new CodeTransform() {
                Label start;
                @Override public void atStart(CodeBuilder b) { start = b.newBoundLabel(); }
                @Override public void accept(CodeBuilder b, CodeElement e) { b.with(e); }
                @Override public void atEnd(CodeBuilder b) {
                    Label end = b.newBoundLabel();
                    b.exceptionCatch(start, end, end, ClassDesc.of("java.nio.file.AccessDeniedException"));
                    // Refused by Windows: the absolute, normalized path.
                    b.pop();
                    b.aload(0);
                    b.invokevirtual(path, "toAbsolutePath", MethodTypeDesc.of(path));
                    b.invokevirtual(path, "normalize", MethodTypeDesc.of(path));
                    b.areturn();
                }
            }));
    }

    static void apply(Instrumentation inst, String cls, java.util.function.UnaryOperator<byte[]> change) {
        String internal = cls.replace('.', '/');
        ClassFileTransformer t = new ClassFileTransformer() {
            @Override
            public byte[] transform(Module m, ClassLoader l, String name, Class<?> c,
                                    ProtectionDomain d, byte[] bytes) {
                return internal.equals(name) ? change.apply(bytes) : null;
            }
        };
        inst.addTransformer(t, true);
        try {
            inst.retransformClasses(Class.forName(cls));
        } catch (Throwable e) {
            System.err.println("Gardenwall JDK agent: " + cls + " left unchanged: " + e);
        } finally {
            inst.removeTransformer(t);
        }
    }

    public static void premain(String args, Instrumentation inst) {
        apply(inst, "java.io.WinNTFileSystem", GardenwallJdkAgent::nameMax);
        apply(inst, "sun.nio.fs.WindowsPath", GardenwallJdkAgent::realPath);
    }
}
"#;

/// The JDK agent's jar for this JDK, built once (in the sandbox) and kept.
/// `None` on other systems, or when this JDK can't build it (older JDKs,
/// which don't need it).
fn jdk_agent(paths: &Paths, java_home: &Path) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let mut h = sha2::Sha256::new();
    h.update(java_home.as_os_str().as_encoded_bytes());
    h.update(JDK_AGENT.as_bytes());
    let id: String = h.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = paths.ghidra_work.join("jdk-agent").join(id);
    let jar = dir.join("gardenwall-jdk-agent.jar");
    if jar.is_file() {
        return Some(jar);
    }
    let build = dir.join("build");
    let _ = std::fs::remove_dir_all(&build);
    std::fs::create_dir_all(build.join("classes")).ok()?;
    std::fs::write(build.join("GardenwallJdkAgent.java"), JDK_AGENT).ok()?;
    let tool = |name: &str, args: &[&str]| -> bool {
        let program = java_home.join("bin").join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        crate::worker::run(
            crate::worker::WorkerSpec {
                profile: "Gardenwall.Decompiler",
                program: &program,
                args: args.iter().map(Into::into).collect(),
                read_execute: vec![java_home.to_path_buf()],
                read_write: vec![build.clone()],
                env: vec![
                    ("TEMP".into(), build.as_os_str().to_os_string()),
                    ("TMP".into(), build.as_os_str().to_os_string()),
                ],
                working_dir: build.clone(),
                memory_bytes: 1 << 30,
                max_processes: 4,
                timeout: Duration::from_secs(120),
                stdout_limit: 1 << 16,
                journal: &paths.grants,
            },
            &[],
        )
        .is_ok_and(|o| o.exit == Some(0))
    };
    // The JDK's `jar` tool makes the same lookup, so the jar is written here.
    let built = tool("javac", &["-d", "classes", "GardenwallJdkAgent.java"])
        && write_agent_jar(&build.join("classes"), &build.join("agent.jar")).is_ok()
        && std::fs::rename(build.join("agent.jar"), &jar).is_ok();
    let _ = std::fs::remove_dir_all(&build);
    built.then_some(jar)
}

/// A jar of the agent's classes (top level only) with its manifest.
fn write_agent_jar(classes: &Path, out: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(out)?);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("META-INF/MANIFEST.MF", options)?;
    zip.write_all(
        b"Manifest-Version: 1.0\r\nPremain-Class: GardenwallJdkAgent\r\nCan-Retransform-Classes: true\r\n\r\n",
    )?;
    let mut names: Vec<String> = std::fs::read_dir(classes)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("GardenwallJdkAgent") && n.ends_with(".class"))
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(std::io::Error::other("no classes were built"));
    }
    for name in names {
        zip.start_file(name.as_str(), options)?;
        zip.write_all(&std::fs::read(classes.join(&name))?)?;
    }
    zip.finish()?;
    Ok(())
}

/// JVM options from Ghidra's `support/launch.properties` (the common ones
/// and this platform's), without memory settings (Gardenwall sets them).
fn ghidra_vmargs(dir: &Path) -> Vec<String> {
    let platform = if cfg!(windows) {
        "VMARGS_WINDOWS="
    } else if cfg!(target_os = "macos") {
        "VMARGS_MACOS="
    } else {
        "VMARGS_LINUX="
    };
    std::fs::read_to_string(dir.join("support/launch.properties"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            l.strip_prefix("VMARGS=")
                .or_else(|| l.strip_prefix(platform))
        })
        .map(str::trim)
        .filter(|a| a.starts_with('-') && !a.starts_with("-Xmx") && !a.starts_with("-Xms"))
        .map(str::to_string)
        .take(64)
        .collect()
}

/// A file name Ghidra can import and find again, from the original.
fn program_name(original: Option<&str>) -> String {
    let base: String = original
        .unwrap_or("target.bin")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(80)
        .collect();
    if base.is_empty() || base.starts_with('.') {
        "target.bin".into()
    } else {
        base
    }
}

/// One decompiler run at a time: Ghidra is heavy, and runs share projects.
static GHIDRA_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Decompile `function` (address or name; empty lists functions) of the
/// file in `bytes`. Blocking; can take minutes the first time.
pub fn decompile_blocking(
    paths: &Paths,
    ghidra: &Ghidra,
    bytes: &[u8],
    original_name: Option<&str>,
    function: &str,
) -> Result<String, String> {
    let _one = GHIDRA_LOCK
        .lock()
        .map_err(|_| "the decompiler is unavailable")?;
    let digest = sha2::Sha256::digest(bytes);
    let id: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
    let projects = paths.ghidra_work.join("projects");
    let project = projects.join(&id);
    let ready_marker = project.join("gardenwall-ready");
    let first = !ready_marker.is_file();
    let run_dir = paths.ghidra_work.join(format!("run-{id}"));
    let _ = std::fs::remove_dir_all(&run_dir);
    for d in [
        project.clone(),
        run_dir.join("tmp"),
        run_dir.join("home"),
        run_dir.join("script"),
    ] {
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    }
    let name = program_name(original_name);
    let result = (|| {
        std::fs::write(
            run_dir.join("script/GardenwallDecompile.java"),
            GHIDRA_SCRIPT,
        )
        .map_err(|e| e.to_string())?;
        let out_file = run_dir.join("out.txt");
        let function: String = function
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '@' | '$' | '?'))
            .take(200)
            .collect();
        let mut args: Vec<std::ffi::OsString> = ghidra_vmargs(&ghidra.dir)
            .into_iter()
            .map(Into::into)
            .collect();
        let home = run_dir.join("home");
        let tmp = run_dir.join("tmp");
        let agent = jdk_agent(paths, &ghidra.java_home);
        if let Some(jar) = &agent {
            let mut a = std::ffi::OsString::from("-javaagent:");
            a.push(jar);
            args.push(a);
        }
        args.extend(
            [
                "-Xmx4G".to_string(),
                "-Djava.awt.headless=true".into(),
                format!("-Duser.home={}", home.display()),
                format!("-Djava.io.tmpdir={}", tmp.display()),
                format!(
                    "-Dapplication.settingsdir={}",
                    home.join("settings").display()
                ),
                // Inside an AppContainer, Windows rewrites LOCALAPPDATA to a
                // path that doesn't exist, so name Ghidra's folders directly.
                format!("-Dapplication.cachedir={}", home.join("cache").display()),
                format!("-Dapplication.tempdir={}", tmp.display()),
                "-cp".into(),
                ghidra_jar(&ghidra.dir).display().to_string(),
                "ghidra.Ghidra".into(),
                "ghidra.app.util.headless.AnalyzeHeadless".into(),
                project.display().to_string(),
                "gardenwall".into(),
            ]
            .map(Into::into),
        );
        if first {
            let input = run_dir.join(&name);
            std::fs::write(&input, bytes).map_err(|e| e.to_string())?;
            args.extend([
                "-import".into(),
                input.into_os_string(),
                "-overwrite".into(),
            ]);
        } else {
            args.extend(["-process".into(), name.clone().into(), "-noanalysis".into()]);
        }
        args.extend([
            "-max-cpu".into(),
            "4".into(),
            "-scriptPath".into(),
            run_dir.join("script").into_os_string(),
            "-postScript".into(),
            "GardenwallDecompile.java".into(),
            if function.is_empty() {
                "-".into()
            } else {
                function.into()
            },
            out_file.clone().into_os_string(),
        ]);
        let java = java_exe(&ghidra.java_home);
        let env: Vec<(std::ffi::OsString, std::ffi::OsString)> = [
            ("TEMP", tmp.as_os_str()),
            ("TMP", tmp.as_os_str()),
            ("USERPROFILE", home.as_os_str()),
            ("HOME", home.as_os_str()),
            ("APPDATA", home.as_os_str()),
            ("LOCALAPPDATA", home.as_os_str()),
            ("JAVA_HOME", ghidra.java_home.as_os_str()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.to_os_string()))
        .collect();
        let out = crate::worker::run(
            crate::worker::WorkerSpec {
                profile: "Gardenwall.Decompiler",
                program: &java,
                args,
                read_execute: [ghidra.dir.clone(), ghidra.java_home.clone()]
                    .into_iter()
                    .chain(
                        agent
                            .as_ref()
                            .and_then(|j| j.parent())
                            .map(Path::to_path_buf),
                    )
                    .collect(),
                read_write: vec![project.clone(), run_dir.clone()],
                env,
                working_dir: run_dir.clone(),
                memory_bytes: 6 << 30,
                max_processes: 16,
                timeout: if first {
                    GHIDRA_FIRST_TIMEOUT
                } else {
                    GHIDRA_TIMEOUT
                },
                stdout_limit: 1 << 20,
                journal: &paths.grants,
            },
            &[],
        )?;
        let text = std::fs::read_to_string(&out_file).ok();
        match (out.exit, text) {
            (None, _) => Err(if first {
                "Ghidra took more than 20 minutes to analyze the file".into()
            } else {
                "Ghidra took too long".into()
            }),
            (Some(0), Some(t)) => {
                if first {
                    let _ = std::fs::write(&ready_marker, b"");
                }
                Ok(t)
            }
            (code, _) => {
                let tail: String = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .rev()
                    .filter(|l| l.contains("ERROR") || l.contains("Exception"))
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ");
                Err(format!(
                    "Ghidra failed (exit code {}){}",
                    code.unwrap_or(-1),
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", tail.chars().take(600).collect::<String>())
                    }
                ))
            }
        }
    })();
    let _ = std::fs::remove_dir_all(&run_dir);
    if result.is_err() && first {
        // A failed first run leaves no half-made project behind.
        let _ = std::fs::remove_dir_all(&project);
    }
    prune_projects(&projects);
    result.map(|mut t| {
        if t.len() > MAX_DECOMPILED {
            let mut end = MAX_DECOMPILED;
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            t.truncate(end);
            t.push_str("\n[output truncated]");
        }
        t
    })
}

/// Keep the most recently used projects.
fn prune_projects(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut projects: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let t = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (t, e.path())
        })
        .collect();
    projects.sort_by_key(|p| std::cmp::Reverse(p.0));
    for (_, p) in projects.into_iter().skip(GHIDRA_PROJECTS_KEPT) {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// The agent's analyzer.
pub struct AppBinary {
    pub paths: Paths,
    pub ghidra: Option<Ghidra>,
}

impl BinaryAnalyzer for AppBinary {
    fn analyze<'a>(
        &'a self,
        args: &'a Value,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let worker_args = worker_args(args)?;
            let paths = self.paths.clone();
            tauri::async_runtime::spawn_blocking(move || {
                analyze_blocking(&paths, worker_args, &bytes)
            })
            .await
            .map_err(|e| e.to_string())?
        })
    }

    fn can_decompile(&self) -> bool {
        self.ghidra.is_some()
    }

    fn decompile<'a>(
        &'a self,
        args: &'a Value,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let ghidra = self.ghidra.clone().ok_or("no decompiler is set up")?;
            let function = args
                .get("function")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = args
                .get("path")
                .and_then(Value::as_str)
                .and_then(|p| Path::new(p).file_name())
                .map(|n| n.to_string_lossy().into_owned());
            let paths = self.paths.clone();
            tauri::async_runtime::spawn_blocking(move || {
                decompile_blocking(&paths, &ghidra, &bytes, name.as_deref(), &function)
            })
            .await
            .map_err(|e| e.to_string())?
        })
    }
}

/// The analyzer for a new agent session.
pub fn for_agent(state: &AppState) -> Arc<dyn BinaryAnalyzer> {
    Arc::new(AppBinary {
        paths: state.paths.clone(),
        ghidra: state.settings().binary.ghidra().ok(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinarySettingsView {
    ghidra_dir: Option<String>,
    ghidra_version: Option<String>,
    java_home: Option<String>,
    /// A JDK found on this computer, offered when none is chosen.
    java_detected: Option<String>,
    /// The decompiler can be used.
    ready: bool,
    /// Why it can't, when a folder was chosen.
    problem: Option<String>,
}

fn settings_view(state: &AppState) -> BinarySettingsView {
    let s = state.settings().binary;
    let check = s.ghidra();
    BinarySettingsView {
        ghidra_version: s.ghidra_dir.as_deref().and_then(ghidra_version),
        ghidra_dir: s.ghidra_dir.as_ref().map(|p| p.display().to_string()),
        java_home: s.java_home.as_ref().map(|p| p.display().to_string()),
        java_detected: if s.java_home.is_none() {
            detect_java_home().map(|p| p.display().to_string())
        } else {
            None
        },
        ready: check.is_ok(),
        problem: check
            .err()
            .filter(|_| s.ghidra_dir.is_some() || s.java_home.is_some()),
    }
}

#[tauri::command]
pub fn binary_settings(state: State<'_, AppState>) -> BinarySettingsView {
    settings_view(&state)
}

/// Choose Ghidra's folder (`which: "ghidra"`) or a JDK (`"java"`) in the
/// native picker; `"detectJava"` uses the JDK found on this computer, and
/// `"clear"` removes both. The folders are granted read and execute to the
/// decompiler's sandbox only.
#[tauri::command]
pub async fn set_binary_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    which: String,
) -> CmdResult<BinarySettingsView> {
    let chosen = match which.as_str() {
        "ghidra" | "java" => {
            let picked = tauri::async_runtime::spawn_blocking(move || {
                app.dialog().file().blocking_pick_folder()
            })
            .await
            .map_err(|e| e.to_string())?;
            match picked.and_then(|p| p.into_path().ok()) {
                Some(p) => Some(p),
                None => return Ok(settings_view(&state)),
            }
        }
        "detectJava" => Some(detect_java_home().ok_or("no JDK was found; choose its folder")?),
        "clear" => None,
        _ => return Err("unknown folder".into()),
    };
    let chosen = match chosen {
        Some(p) => Some(narrow_folder(&p)?),
        None => None,
    };
    state
        .update_settings(|s| match which.as_str() {
            "ghidra" => s.binary.ghidra_dir = chosen,
            "java" | "detectJava" => s.binary.java_home = chosen,
            _ => s.binary = BinarySettings::default(),
        })
        .map_err(|e| e.to_string())?;
    Ok(settings_view(&state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strs(v: Vec<std::ffi::OsString>) -> Vec<String> {
        v.into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn worker_arguments_are_filtered() {
        assert_eq!(
            strs(
                worker_args(
                    &json!({"op": "disassemble", "target": "main\u{1b}[0m", "count": 50.7})
                )
                .unwrap()
            ),
            ["--op", "disasm", "target=main[0m", "count=50"]
        );
        assert_eq!(
            strs(worker_args(&json!({"op": "hex", "address": "0x1000", "length": 64})).unwrap()),
            ["--op", "hex", "length=64", "address=0x1000"]
        );
        assert!(worker_args(&json!({"op": "run"})).is_err());
        assert!(worker_args(&json!({"op": "hex", "address": "nope"})).is_err());
        assert!(worker_args(&json!({"op": "strings", "limit": -1})).is_err());
        // Unknown options never reach the worker.
        assert_eq!(
            strs(worker_args(&json!({"op": "info", "exec": "calc"})).unwrap()),
            ["--op", "info"]
        );
    }

    /// Runs the real worker in the real sandbox. Needs
    /// `cargo build -p gardenwall-binary` first; run with `--ignored`.
    #[test]
    #[ignore]
    fn analyzer_runs_in_the_sandbox() {
        let exe = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(if cfg!(windows) {
                "gardenwall-binary.exe"
            } else {
                "gardenwall-binary"
            });
        assert!(exe.is_file(), "build gardenwall-binary first");
        // The sandbox grants only a folder holding nothing else, as the
        // app's staged helper folder does.
        let staged = tempfile::tempdir().unwrap();
        let exe = {
            let to = staged.path().join(exe.file_name().unwrap());
            std::fs::copy(&exe, &to).unwrap();
            to
        };
        let journal = tempfile::tempdir().unwrap();
        let bytes = std::fs::read(&exe).unwrap();
        let run = |args: Vec<&str>| {
            crate::worker::run(
                crate::worker::WorkerSpec {
                    profile: "Gardenwall.BinaryAnalyzerTest",
                    program: &exe,
                    args: args.into_iter().map(Into::into).collect(),
                    read_execute: vec![exe.parent().unwrap().to_path_buf()],
                    read_write: vec![],
                    env: vec![],
                    working_dir: exe.parent().unwrap().to_path_buf(),
                    memory_bytes: 1 << 30,
                    max_processes: 1,
                    timeout: Duration::from_secs(60),
                    stdout_limit: ANALYZER_OUTPUT,
                    journal: journal.path(),
                },
                &bytes,
            )
            .unwrap()
        };
        let info = run(vec!["--op", "info"]);
        assert_eq!(info.exit, Some(0), "{}", info.stderr);
        let text = String::from_utf8_lossy(&info.stdout);
        assert!(text.starts_with("Format: "), "{text}");
        let asm = run(vec!["--op", "disasm", "target=entry", "count=10"]);
        assert_eq!(asm.exit, Some(0), "{}", asm.stderr);
        let bad = run(vec!["--op", "nope"]);
        assert_eq!(bad.exit, Some(2));
    }

    /// Runs the detected JDK's `java -version` in the decompiler's sandbox
    /// (folders granted read and execute only, no network). Run with
    /// `--ignored` on a machine with a JDK.
    #[test]
    #[ignore]
    fn java_runs_in_the_sandbox() {
        let home = detect_java_home().expect("no JDK found");
        let home = narrow_folder(&home).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let journal = tempfile::tempdir().unwrap();
        let out = crate::worker::run(
            crate::worker::WorkerSpec {
                profile: "Gardenwall.DecompilerTest",
                program: &java_exe(&home),
                args: vec![
                    "-Xmx256m".into(),
                    format!("-Djava.io.tmpdir={}", scratch.path().display()).into(),
                    "-XshowSettings:properties".into(),
                    "-version".into(),
                ],
                read_execute: vec![home.clone()],
                read_write: vec![scratch.path().to_path_buf()],
                env: vec![
                    ("TEMP".into(), scratch.path().as_os_str().to_os_string()),
                    ("TMP".into(), scratch.path().as_os_str().to_os_string()),
                ],
                working_dir: scratch.path().to_path_buf(),
                memory_bytes: 2 << 30,
                max_processes: 4,
                timeout: Duration::from_secs(60),
                stdout_limit: 1 << 20,
                journal: journal.path(),
            },
            &[],
        )
        .unwrap();
        assert_eq!(
            out.exit,
            Some(0),
            "java failed in the sandbox: {}",
            out.stderr
        );
    }

    /// The real decompiler in its sandbox. Set `GW_GHIDRA` (the install
    /// folder) and `GW_JAVA` (a JDK), then run with `--ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn ghidra_decompiles_in_the_sandbox() {
        let settings = BinarySettings {
            ghidra_dir: std::env::var_os("GW_GHIDRA").map(PathBuf::from),
            java_home: std::env::var_os("GW_JAVA").map(PathBuf::from),
        };
        let ghidra = settings.ghidra().expect("set GW_GHIDRA and GW_JAVA");
        // Ghidra refuses project paths with 8.3 short names (the temp
        // folder can have them) or elements starting with '.'.
        let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap()
            .join("target");
        let root = tempfile::Builder::new()
            .prefix("ghidra-test-")
            .tempdir_in(target)
            .unwrap();
        let paths = Paths::new(root.path().to_path_buf()).unwrap();
        let bytes = std::fs::read(r"C:\Windows\System32\version.dll").unwrap();
        let list = decompile_blocking(&paths, &ghidra, &bytes, Some("version.dll"), "")
            .expect("listing functions");
        println!("{}", &list[..list.len().min(1500)]);
        let code = decompile_blocking(&paths, &ghidra, &bytes, Some("version.dll"), "entry")
            .expect("decompiling entry");
        println!("{}", &code[..code.len().min(1500)]);
        assert!(code.contains('{'), "no C code: {code}");
    }

    #[test]
    fn program_names_are_safe() {
        assert_eq!(program_name(Some("my tool (1).exe")), "mytool1.exe");
        assert_eq!(program_name(Some("..\\..\\x")), "target.bin");
        assert_eq!(program_name(None), "target.bin");
    }

    #[test]
    fn ghidra_setup_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let s = BinarySettings {
            ghidra_dir: Some(dir.path().to_path_buf()),
            java_home: Some(dir.path().to_path_buf()),
        };
        assert!(s.ghidra().unwrap_err().contains("Ghidra install"));
        std::fs::create_dir_all(dir.path().join("Ghidra/Framework/Utility/lib")).unwrap();
        std::fs::create_dir_all(dir.path().join("support")).unwrap();
        std::fs::write(ghidra_jar(dir.path()), b"jar").unwrap();
        std::fs::write(
            dir.path().join("support/launch.properties"),
            "VMARGS=-Djava.system.class.loader=ghidra.GhidraClassLoader\nVMARGS=-Xmx2G\n# VMARGS=-Dx\nVMARGS_WINDOWS=-Dwin=1\nVMARGS_LINUX=-Dlinux=1\nVMARGS_MACOS=-Dmac=1\nOTHER=1\n",
        )
        .unwrap();
        assert!(s.ghidra().unwrap_err().contains("JDK"));
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(java_exe(dir.path()), b"").unwrap();
        assert!(s.ghidra().is_ok());
        let vm = ghidra_vmargs(dir.path());
        assert_eq!(vm[0], "-Djava.system.class.loader=ghidra.GhidraClassLoader");
        assert_eq!(vm.len(), 2);
        assert!(!vm.iter().any(|a| a.starts_with("-Xmx")));
    }
}
