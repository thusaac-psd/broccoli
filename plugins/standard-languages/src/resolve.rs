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
}

pub const LANGUAGES: &[LanguageMeta] = &[
    LanguageMeta {
        id: "c",
        display_name: "C",
        default_filename: "solution.c",
        extensions: &["c"],
        template: "#include <stdio.h>\n\nint main() {\n    // Your code here\n    return 0;\n}\n",
    },
    LanguageMeta {
        id: "cpp",
        display_name: "C++",
        default_filename: "solution.cpp",
        extensions: &["cpp", "cc", "cxx", "c++"],
        template: "#include <iostream>\nusing namespace std;\n\nint main() {\n    // Your code here\n    return 0;\n}\n",
    },
    LanguageMeta {
        id: "python3",
        display_name: "Python 3",
        default_filename: "solution.py",
        extensions: &["py"],
        template: "# Your code here\n",
    },
    LanguageMeta {
        id: "java",
        display_name: "Java",
        default_filename: "Main.java",
        extensions: &["java"],
        template: "public class Main {\n    public static void main(String[] args) {\n        // Your code here\n    }\n}\n",
    },
];

pub const LANGUAGE_IDS: &[&str] = &["c", "cpp", "python3", "java"];

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

fn default_source(lang: &str) -> &str {
    match lang {
        "c" => "solution.c",
        "cpp" => "solution.cpp",
        "python3" => "solution.py",
        "java" => "Main.java",
        _ => "",
    }
}

fn default_basename(lang: &str) -> &str {
    match lang {
        "java" => "Main",
        _ => "solution",
    }
}

/// Resolve the primary source file and its basename from submitted files.
///
/// Priority: entry_point config > default source filename match > first file.
fn resolve_primary<'a>(
    lang: &str,
    all_files: &[&'a str],
    entry_point: Option<&'a str>,
) -> (&'a str, String) {
    let primary = if let Some(ep) = entry_point {
        all_files.iter().find(|f| **f == ep).copied().unwrap_or(ep)
    } else {
        let default = default_source(lang);
        all_files
            .iter()
            .find(|f| **f == default)
            .or(all_files.first())
            .copied()
            .unwrap_or_default()
    };

    let basename = Path::new(primary)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_basename(lang))
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

/// Shared resolver for compiled languages that produce a single binary (C, C++).
///
/// Command: `[compiler, flags..., extra_flags..., primary, extras..., -o, basename, suffix_args...]`
fn resolve_compiled(
    lang: &str,
    req: &ResolveLanguageInput,
    entry_point_config: Option<&EntryPointConfig>,
    compiler: &str,
    flags: &[String],
    extra_compile_flags: &[String],
    suffix_args: &[&str],
) -> ResolveLanguageOutput {
    let all_files = collect_files(req);
    let ep = entry_point_config.and_then(|c| c.entry_point.as_deref());
    let (primary, basename) = resolve_primary(lang, &all_files, ep);

    let mut command = vec![compiler.to_string()];
    command.extend(flags.iter().cloned());
    command.extend(extra_compile_flags.iter().cloned());
    // Prefix source filenames with `./` so a name like `-DNDEBUG` is passed as a
    // PATH, never parsed as a compiler flag (argument injection into a trusted
    // co-compiled grader). Defense in depth: `validate_flat_filename` already
    // rejects a leading `-` at upload, but the plugin should not trust that.
    command.push(format!("./{primary}"));
    for f in &all_files {
        if *f != primary {
            command.push(format!("./{f}"));
        }
    }
    command.push("-o".into());
    command.push(basename.clone());
    command.extend(suffix_args.iter().map(|s| s.to_string()));

    let cache_inputs: Vec<String> = all_files.iter().map(|s| s.to_string()).collect();

    ResolveLanguageOutput {
        compile: Some(CompileSpec {
            command,
            cache_inputs,
            outputs: vec![OutputSpec::File(basename.clone())],
            resource_limits: None,
        }),
        run: RunSpec {
            command: vec![format!("./{basename}")],
            extra_files: vec![],
            min_process_limit: None,
        },
    }
}

pub fn resolve_c(
    req: &ResolveLanguageInput,
    entry_point_config: Option<&EntryPointConfig>,
    compiler: &str,
    flags: &[String],
    extra_compile_flags: &[String],
) -> ResolveLanguageOutput {
    resolve_compiled(
        "c",
        req,
        entry_point_config,
        compiler,
        flags,
        extra_compile_flags,
        &["-lm"],
    )
}

pub fn resolve_cpp(
    req: &ResolveLanguageInput,
    entry_point_config: Option<&EntryPointConfig>,
    compiler: &str,
    flags: &[String],
    extra_compile_flags: &[String],
) -> ResolveLanguageOutput {
    resolve_compiled(
        "cpp",
        req,
        entry_point_config,
        compiler,
        flags,
        extra_compile_flags,
        &[],
    )
}

pub fn resolve_python3(
    req: &ResolveLanguageInput,
    entry_point_config: Option<&EntryPointConfig>,
    interpreter: &str,
) -> ResolveLanguageOutput {
    let all_files = collect_files(req);
    let ep = entry_point_config.and_then(|c| c.entry_point.as_deref());
    let (primary, _) = resolve_primary("python3", &all_files, ep);

    ResolveLanguageOutput {
        compile: None,
        run: RunSpec {
            command: vec![interpreter.to_string(), primary.to_string()],
            extra_files: all_files.iter().map(|s| s.to_string()).collect(),
            min_process_limit: None,
        },
    }
}

pub fn resolve_java(
    req: &ResolveLanguageInput,
    entry_point_config: Option<&EntryPointConfig>,
    compiler: &str,
    runner: &str,
    flags: &[String],
    extra_compile_flags: &[String],
) -> ResolveLanguageOutput {
    let all_files = collect_files(req);
    let ep = entry_point_config.and_then(|c| c.entry_point.as_deref());
    let (primary, basename) = resolve_primary("java", &all_files, ep);

    let mut command = vec![compiler.to_string()];
    // `javac` is itself a JVM: without pinning it also sizes its GC/JIT thread
    // pools to the host CPU count and can EAGAIN under load, exactly like the run
    // step. `-J<opt>` forwards an option to javac's own VM, so mirror the run
    // step's `-XX:ActiveProcessorCount` on the compile path. See the run command
    // and `JAVA_ACTIVE_PROCESSORS`.
    command.push(format!(
        "-J-XX:ActiveProcessorCount={JAVA_ACTIVE_PROCESSORS}"
    ));
    command.extend(flags.iter().cloned());
    command.extend(extra_compile_flags.iter().cloned());
    // Prefix source filenames with `./` so an uploaded name is passed as a PATH,
    // never parsed as a javac directive. `javac` treats any argv token starting
    // with `@` as an ARGFILE (`@file` = read compiler options from `file`), which
    // would let a contestant inject javac flags -- e.g. `-processorpath`/
    // `-processor` = arbitrary code execution during the trusted grader compile.
    // `validate_flat_filename` rejects a leading `-` at upload but NOT a leading
    // `@`, so the plugin must not trust the raw name. `./Main.java` still compiles
    // to `Main.class` (javac derives the class from source content, not the path),
    // so the run step's class name (`basename`) is unaffected. Mirrors the
    // `resolve_compiled` `./` defense.
    command.push(format!("./{primary}"));
    for f in &all_files {
        if *f != primary {
            command.push(format!("./{f}"));
        }
    }

    let cache_inputs: Vec<String> = all_files.iter().map(|s| s.to_string()).collect();

    ResolveLanguageOutput {
        compile: Some(CompileSpec {
            command,
            cache_inputs,
            // javac may produce multiple .class files (inner classes)
            outputs: vec![OutputSpec::Glob("*.class".into())],
            resource_limits: None,
        }),
        run: RunSpec {
            command: vec![
                runner.to_string(),
                format!("-XX:ActiveProcessorCount={JAVA_ACTIVE_PROCESSORS}"),
                "-cp".into(),
                ".".into(),
                basename,
            ],
            extra_files: vec![],
            min_process_limit: Some(JAVA_MIN_PROCESS_LIMIT),
        },
    }
}
