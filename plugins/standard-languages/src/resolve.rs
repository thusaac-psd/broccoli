use broccoli_server_sdk::types::{
    CompileSpec, OutputSpec, ResolveLanguageInput, ResolveLanguageOutput, RunSpec,
};
use std::path::Path;

use crate::EntryPointConfig;

pub struct LanguageMeta {
    pub id: &'static str,
    pub display_name: &'static str,
    pub default_filename: &'static str,
    pub extensions: &'static [&'static str],
    pub template: &'static str,
    /// How submitted files become a program. Each kind is one resolver, so a
    /// language in [`LANGUAGES`] always resolves.
    pub kind: Kind,
}

/// The toolchain shape of a language: what its compile and run commands look
/// like. Tool paths and flags come from the plugin config (see [`Tools`]).
pub enum Kind {
    /// gcc-style: `tool flags... ./primary ./others... -o <bin> libs...`, run
    /// `./<bin>`.
    Gcc { libs: &'static [&'static str] },
    /// `tool flags... ./primary -o <bin>`, run `./<bin>`. rustc reads the other
    /// files itself, through the `mod` items of the crate root.
    Rustc,
    /// `tool flags... -o<bin> ./primary`, run `./<bin>`. fpc finds the units a
    /// program uses beside it.
    Fpc,
    /// `env GOCACHE=... tool build flags... -o <bin> ./primary ./others...`, run
    /// `./<bin>`. The Go runtime sizes its scheduler from `GOMAXPROCS`.
    Go,
    /// `tool flags... ./primary ./others...`, run `runner -cp . <Main>`.
    Javac,
    /// `tool flags... ./primary ./others... -include-runtime -d <bin>.jar`, run
    /// `runner -jar <bin>.jar`.
    Kotlinc,
    /// No compile step: run `tool <primary>` with every file beside it.
    Interpreter {
        /// The runtime's thread floor, when it needs more than one.
        min_process_limit: Option<u32>,
    },
}

pub const LANGUAGES: &[LanguageMeta] = &[
    LanguageMeta {
        id: "c",
        display_name: "C",
        default_filename: "solution.c",
        extensions: &["c"],
        template: "#include <stdio.h>\n\nint main() {\n    // Your code here\n    return 0;\n}\n",
        kind: Kind::Gcc { libs: &["-lm"] },
    },
    LanguageMeta {
        id: "cpp",
        display_name: "C++",
        default_filename: "solution.cpp",
        extensions: &["cpp", "cc", "cxx", "c++"],
        template: "#include <iostream>\nusing namespace std;\n\nint main() {\n    // Your code here\n    return 0;\n}\n",
        kind: Kind::Gcc { libs: &[] },
    },
    LanguageMeta {
        id: "python3",
        display_name: "Python 3",
        default_filename: "solution.py",
        extensions: &["py"],
        template: "# Your code here\n",
        kind: Kind::Interpreter {
            min_process_limit: None,
        },
    },
    LanguageMeta {
        id: "java",
        display_name: "Java",
        default_filename: "Main.java",
        extensions: &["java"],
        template: "public class Main {\n    public static void main(String[] args) {\n        // Your code here\n    }\n}\n",
        kind: Kind::Javac,
    },
    LanguageMeta {
        id: "kotlin",
        display_name: "Kotlin",
        default_filename: "solution.kt",
        extensions: &["kt"],
        template: "fun main() {\n    // Your code here\n}\n",
        kind: Kind::Kotlinc,
    },
    LanguageMeta {
        id: "rust",
        display_name: "Rust",
        default_filename: "solution.rs",
        extensions: &["rs"],
        template: "fn main() {\n    // Your code here\n}\n",
        kind: Kind::Rustc,
    },
    LanguageMeta {
        id: "go",
        display_name: "Go",
        default_filename: "solution.go",
        extensions: &["go"],
        template: "package main\n\nfunc main() {\n\t// Your code here\n}\n",
        kind: Kind::Go,
    },
    LanguageMeta {
        id: "pascal",
        display_name: "Pascal",
        default_filename: "solution.pas",
        extensions: &["pas", "pp"],
        template: "program solution;\n\nbegin\n  { Your code here }\nend.\n",
        kind: Kind::Fpc,
    },
    LanguageMeta {
        id: "javascript",
        display_name: "JavaScript (Node.js)",
        default_filename: "solution.js",
        extensions: &["js"],
        template: "// Your code here\n",
        kind: Kind::Interpreter {
            min_process_limit: Some(NODE_MIN_PROCESS_LIMIT),
        },
    },
];

/// The language registered under `id`, if this plugin provides it.
pub fn language(id: &str) -> Option<&'static LanguageMeta> {
    LANGUAGES.iter().find(|language| language.id == id)
}

/// The configured toolchain for one resolution: the plugin config cascade,
/// with a caller's overrides on top.
#[derive(Default)]
pub struct Tools<'a> {
    /// The compiler, or the interpreter of an interpreted language.
    pub tool: &'a str,
    /// The VM that runs compiled JVM bytecode (`java`).
    pub runner: &'a str,
    pub flags: &'a [String],
    /// Per-problem flags appended after `flags`.
    pub extra_flags: &'a [String],
}

impl LanguageMeta {
    pub fn resolve(
        &self,
        req: &ResolveLanguageInput,
        entry_point_config: Option<&EntryPointConfig>,
        tools: &Tools<'_>,
    ) -> ResolveLanguageOutput {
        let all_files = collect_files(req);
        let entry_point = entry_point_config.and_then(|c| c.entry_point.as_deref());
        let (primary, basename) = resolve_primary(self, &all_files, entry_point);
        let others = || {
            all_files
                .iter()
                .filter(move |file| **file != primary)
                .map(|file| as_path(file))
        };
        let configured = || {
            tools
                .flags
                .iter()
                .chain(tools.extra_flags)
                .cloned()
                .collect::<Vec<_>>()
        };

        let (command, outputs, run) = match &self.kind {
            Kind::Interpreter { min_process_limit } => {
                return ResolveLanguageOutput {
                    compile: None,
                    run: RunSpec {
                        command: vec![tools.tool.to_string(), primary.to_string()],
                        extra_files: all_files.iter().map(|s| s.to_string()).collect(),
                        min_process_limit: *min_process_limit,
                    },
                };
            }
            Kind::Gcc { libs } => {
                let mut command = vec![tools.tool.to_string()];
                command.extend(configured());
                command.push(as_path(primary));
                command.extend(others());
                command.extend(["-o".to_string(), basename.clone()]);
                command.extend(libs.iter().map(|lib| lib.to_string()));
                (
                    command,
                    vec![OutputSpec::File(basename.clone())],
                    native_run(&basename),
                )
            }
            Kind::Rustc => {
                let mut command = vec![tools.tool.to_string()];
                command.extend(configured());
                command.extend([as_path(primary), "-o".to_string(), basename.clone()]);
                (
                    command,
                    vec![OutputSpec::File(basename.clone())],
                    native_run(&basename),
                )
            }
            Kind::Fpc => {
                let mut command = vec![tools.tool.to_string()];
                command.extend(configured());
                command.extend([format!("-o{basename}"), as_path(primary)]);
                (
                    command,
                    vec![OutputSpec::File(basename.clone())],
                    native_run(&basename),
                )
            }
            Kind::Go => {
                let mut command = vec![
                    ENV.to_string(),
                    format!("GOCACHE={GO_CACHE}"),
                    GO_MAXPROCS.to_string(),
                    tools.tool.to_string(),
                    "build".to_string(),
                ];
                command.extend(configured());
                command.extend(["-o".to_string(), basename.clone(), as_path(primary)]);
                command.extend(others());
                let run = RunSpec {
                    command: vec![ENV.into(), GO_MAXPROCS.into(), format!("./{basename}")],
                    extra_files: vec![],
                    min_process_limit: Some(GO_MIN_PROCESS_LIMIT),
                };
                (command, vec![OutputSpec::File(basename.clone())], run)
            }
            Kind::Javac => {
                let mut command = vec![tools.tool.to_string(), JAVAC_PROCESSORS.to_string()];
                command.extend(configured());
                command.push(as_path(primary));
                command.extend(others());
                let run = jvm_run(tools.runner, ["-cp".to_string(), ".".to_string(), basename]);
                // javac writes one .class file per class, inner classes included.
                (command, vec![OutputSpec::Glob("*.class".into())], run)
            }
            Kind::Kotlinc => {
                let jar = format!("{basename}.jar");
                let mut command = vec![tools.tool.to_string(), JAVAC_PROCESSORS.to_string()];
                command.extend(configured());
                command.push(as_path(primary));
                command.extend(others());
                command.extend([
                    "-include-runtime".to_string(),
                    "-d".to_string(),
                    jar.clone(),
                ]);
                let run = jvm_run(tools.runner, ["-jar".to_string(), jar.clone()]);
                (command, vec![OutputSpec::File(jar)], run)
            }
        };

        ResolveLanguageOutput {
            compile: Some(CompileSpec {
                command,
                cache_inputs: all_files.iter().map(|s| s.to_string()).collect(),
                outputs,
                resource_limits: None,
            }),
            run,
        }
    }
}

/// Source files are passed as `./name`, so a name is always read as a path:
/// `-DNDEBUG` is never a gcc flag and `@opts` is never a javac argfile (an
/// argfile injects options, `-processorpath` included, into a trusted grader
/// compile). Upload validation rejects a leading `-` but not a leading `@`, so
/// this is the guard.
fn as_path(file: &str) -> String {
    format!("./{file}")
}

fn native_run(basename: &str) -> RunSpec {
    RunSpec {
        command: vec![format!("./{basename}")],
        extra_files: vec![],
        min_process_limit: None,
    }
}

fn jvm_run<const N: usize>(runner: &str, target: [String; N]) -> RunSpec {
    let mut command = vec![
        runner.to_string(),
        format!("-XX:ActiveProcessorCount={JAVA_ACTIVE_PROCESSORS}"),
    ];
    command.extend(target);
    RunSpec {
        command,
        extra_files: vec![],
        min_process_limit: Some(JAVA_MIN_PROCESS_LIMIT),
    }
}

/// The sandbox's only environment variable is `PATH`, so a program that needs
/// another one runs through `env`.
const ENV: &str = "/usr/bin/env";

/// `go build` needs a writable build cache; the sandbox's private `/tmp` is.
const GO_CACHE: &str = "/tmp/go-build";

/// The Go runtime starts one scheduler thread per `GOMAXPROCS`, which defaults
/// to the host's CPU count. One matches the single-core judging model, the way
/// `JAVA_ACTIVE_PROCESSORS` does for the JVM.
const GO_MAXPROCS: &str = "GOMAXPROCS=1";

/// Threads a Go program starts with `GOMAXPROCS=1`: the scheduler thread, the
/// system monitor and a few for blocking syscalls and the garbage collector.
const GO_MIN_PROCESS_LIMIT: u32 = 16;

/// Threads Node.js starts before running a script: the V8 platform workers and
/// the libuv thread pool (four each), the main thread and a signal watcher.
const NODE_MIN_PROCESS_LIMIT: u32 = 16;

/// `javac` and `kotlinc` are JVMs too: `-J<opt>` passes an option to their own
/// VM, so they size their thread pools the way the run step does.
const JAVAC_PROCESSORS: &str = "-J-XX:ActiveProcessorCount=1";

/// Minimum process/thread count the JVM needs to start. openjdk spawns a fixed
/// set of helper threads at boot (VM thread, reference handler, finalizer,
/// signal dispatcher) plus GC/JIT worker pools. 64 clears the fixed threads with
/// headroom while staying bounded (memory and time remain capped by the problem's
/// limits via the sandbox cgroup). This is a floor, not the real fix for thread
/// blow-up: see `JAVA_ACTIVE_PROCESSORS`.
const JAVA_MIN_PROCESS_LIMIT: u32 = 64;

/// Number of processors the JVM is told to size its thread pools against, via
/// `-XX:ActiveProcessorCount`. Without this the JVM sizes ParallelGCThreads,
/// JIT compiler threads and the ForkJoinPool common pool to the *host* CPU count,
/// not the sandbox. On a large judge host (e.g. 96 cores) a single default
/// `java` wants ~74 threads (ParallelGCThreads alone is ~63), which overruns the
/// per-box process cap and every run fails with EAGAIN ("unable to create native
/// thread") under load - independent of how much real work the solution does.
/// Pinning the JVM's processor view to a small constant decouples thread count
/// from host size: the VM then needs ~12 threads and comfortably fits the cap.
/// 1 matches the conventional single-core competitive-judge execution model
/// (isolate accounts CPU time, and contest solutions are single-threaded), which
/// also selects SerialGC and a single compiler thread - the minimal, most
/// deterministic footprint. Flag is JDK-version stable (present since JDK 10).
const JAVA_ACTIVE_PROCESSORS: u32 = 1;

/// The primary source file and the basename of what it compiles to.
///
/// Priority: the entry_point config, then the default source filename, then
/// the first file with a matching extension, then the first file. The basename
/// falls back to the default filename's stem (`Main` for Java).
fn resolve_primary<'a>(
    language: &LanguageMeta,
    all_files: &[&'a str],
    entry_point: Option<&'a str>,
) -> (&'a str, String) {
    let primary = if let Some(ep) = entry_point {
        all_files.iter().find(|f| **f == ep).copied().unwrap_or(ep)
    } else {
        all_files
            .iter()
            .find(|file| **file == language.default_filename)
            .or_else(|| {
                all_files.iter().find(|file| {
                    Path::new(file)
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| language.extensions.contains(&ext))
                })
            })
            .or(all_files.first())
            .copied()
            .unwrap_or_default()
    };

    let stem = |file: &'a str| {
        Path::new(file)
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
    };
    let basename = stem(primary)
        .or_else(|| stem(language.default_filename))
        .unwrap_or("solution")
        .to_string();

    (primary, basename)
}

fn collect_files(req: &ResolveLanguageInput) -> Vec<&str> {
    req.submitted_files
        .iter()
        .map(|s| s.as_str())
        .chain(req.additional_files.iter().map(|f| f.filename.as_str()))
        .collect()
}
