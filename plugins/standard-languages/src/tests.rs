use broccoli_server_sdk::types::{FileRef, OutputSpec, ResolveLanguageInput};

use broccoli_server_sdk::types::ResolveLanguageOutput;

use crate::EntryPointConfig;
use crate::resolve::{self, Tools};

fn resolve_with(
    language: &str,
    req: &ResolveLanguageInput,
    entry_point: Option<&EntryPointConfig>,
    tools: Tools<'_>,
) -> ResolveLanguageOutput {
    resolve::language(language)
        .unwrap_or_else(|| panic!("{language} is registered"))
        .resolve(req, entry_point, &tools)
}

fn resolve_cpp(
    req: &ResolveLanguageInput,
    entry_point: Option<&EntryPointConfig>,
    compiler: &str,
    flags: &[String],
    extra_flags: &[String],
) -> ResolveLanguageOutput {
    let tools = Tools {
        tool: compiler,
        flags,
        extra_flags,
        ..Tools::default()
    };
    resolve_with("cpp", req, entry_point, tools)
}

fn resolve_c(
    req: &ResolveLanguageInput,
    entry_point: Option<&EntryPointConfig>,
    compiler: &str,
    flags: &[String],
    extra_flags: &[String],
) -> ResolveLanguageOutput {
    let tools = Tools {
        tool: compiler,
        flags,
        extra_flags,
        ..Tools::default()
    };
    resolve_with("c", req, entry_point, tools)
}

fn resolve_python3(
    req: &ResolveLanguageInput,
    entry_point: Option<&EntryPointConfig>,
    interpreter: &str,
) -> ResolveLanguageOutput {
    let tools = Tools {
        tool: interpreter,
        ..Tools::default()
    };
    resolve_with("python3", req, entry_point, tools)
}

fn resolve_java(
    req: &ResolveLanguageInput,
    entry_point: Option<&EntryPointConfig>,
    compiler: &str,
    runner: &str,
    flags: &[String],
    extra_flags: &[String],
) -> ResolveLanguageOutput {
    let tools = Tools {
        tool: compiler,
        runner,
        flags,
        extra_flags,
    };
    resolve_with("java", req, entry_point, tools)
}

/// The plugin.toml default of `compilation.<language>.<key>`.
fn config_default(language: &str, key: &str) -> Option<toml::Value> {
    let doc: toml::Value =
        toml::from_str(include_str!("../plugin.toml")).expect("plugin.toml parses");
    doc["config"]["compilation"]["properties"]
        .get(language)?
        .get("properties")?
        .get(key)?
        .get("default")
        .cloned()
}

fn default_tool(language: &str, key: &str) -> String {
    config_default(language, key)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("plugin.toml has a compilation.{language}.{key} default"))
}

fn default_flags(language: &str) -> Vec<String> {
    config_default(language, "flags")
        .map(|v| {
            v.as_array()
                .expect("flags default is an array")
                .iter()
                .map(|f| f.as_str().expect("flag is a string").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Every registered language has its tools in the compilation config schema,
/// so an admin can see and change them, and a resolution never runs an empty
/// command.
#[test]
fn every_language_has_configurable_tools() {
    for language in resolve::LANGUAGES {
        let tool_key = match language.kind {
            resolve::Kind::Interpreter { .. } => "interpreter",
            _ => "compiler",
        };
        default_tool(language.id, tool_key);
        if matches!(language.kind, resolve::Kind::Javac | resolve::Kind::Kotlinc) {
            default_tool(language.id, "runner");
        }
    }
}

/// Resolves a language with its plugin.toml defaults.
fn resolve_default(language: &str, files: Vec<&str>) -> ResolveLanguageOutput {
    let meta = resolve::language(language).expect("registered");
    let tool_key = match meta.kind {
        resolve::Kind::Interpreter { .. } => "interpreter",
        _ => "compiler",
    };
    let tool = default_tool(language, tool_key);
    let runner = config_default(language, "runner")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let flags = default_flags(language);
    let tools = Tools {
        tool: &tool,
        runner: &runner,
        flags: &flags,
        extra_flags: &[],
    };
    meta.resolve(&req(language, files), None, &tools)
}

fn req(lang: &str, files: Vec<&str>) -> ResolveLanguageInput {
    ResolveLanguageInput {
        language_id: lang.into(),
        submitted_files: files.iter().map(|s| s.to_string()).collect(),
        additional_files: vec![],
        problem_id: None,
        contest_id: None,
        overrides: None,
    }
}

fn default_cpp_flags() -> Vec<String> {
    default_flags("cpp")
}

#[test]
fn cpp_default_flags_are_o2_only_no_std_for_noi_parity() {
    // NOI Linux 2.0 compiles C++ with no `-std`, so gcc 9.3's default gnu++14 is
    // the parity behavior. The global default must be exactly ["-O2"] and carry
    // no `-std=` (never reintroduce `-std=c++NN` as the global default).
    let flags = default_cpp_flags();
    assert_eq!(
        flags,
        vec!["-O2".to_string()],
        "cpp default must be -O2 only"
    );
    assert!(
        !flags.iter().any(|f| f.starts_with("-std=")),
        "C++ default must carry no -std (gnu++14 parity): {flags:?}"
    );
}

fn default_c_flags() -> Vec<String> {
    default_flags("c")
}

// C++
#[test]
fn cpp_single_file() {
    let result = resolve_cpp(
        &req("cpp", vec!["solution.cpp"]),
        None,
        "/usr/bin/g++",
        &default_cpp_flags(),
        &[],
    );
    let compile = result.compile.unwrap();
    assert_eq!(compile.command[0], "/usr/bin/g++");
    assert!(compile.command.contains(&"./solution.cpp".to_string()));
    assert!(compile.command.contains(&"-o".to_string()));
    assert_eq!(result.run.command, vec!["./solution"]);
    assert!(result.run.extra_files.is_empty());
}

#[test]
fn cpp_with_grader_stubs() {
    let result = resolve_cpp(
        &req("cpp", vec!["solution.cpp", "grader.cpp", "grader.h"]),
        None,
        "/usr/bin/g++",
        &default_cpp_flags(),
        &[],
    );
    let compile = result.compile.unwrap();
    assert!(compile.command.contains(&"./solution.cpp".to_string()));
    assert!(compile.command.contains(&"./grader.cpp".to_string()));
    assert!(compile.command.contains(&"./grader.h".to_string()));
    assert_eq!(
        compile.cache_inputs,
        vec!["solution.cpp", "grader.cpp", "grader.h"]
    );
}

#[test]
fn cpp_primary_matched_by_default_source_filename() {
    let result = resolve_cpp(
        &req("cpp", vec!["grader.cpp", "solution.cpp"]),
        None,
        "/usr/bin/g++",
        &default_cpp_flags(),
        &[],
    );
    assert_eq!(result.run.command, vec!["./solution"]);
}

#[test]
fn cpp_primary_prefers_source_extension_over_first_file() {
    let result = resolve_cpp(
        &req("cpp", vec!["notes.txt", "main.cc"]),
        None,
        "/usr/bin/g++",
        &default_cpp_flags(),
        &[],
    );
    let compile = result.compile.unwrap();
    assert_eq!(compile.command[2], "./main.cc");
    assert_eq!(result.run.command, vec!["./main"]);
}

#[test]
fn python_primary_prefers_source_extension_over_first_file() {
    let result = resolve_python3(
        &req("python3", vec!["notes.txt", "main.py"]),
        None,
        "python3",
    );
    assert_eq!(result.run.command, vec!["python3", "main.py"]);
}

#[test]
fn cpp_compiler_override() {
    let result = resolve_cpp(
        &req("cpp", vec!["solution.cpp"]),
        None,
        "/usr/local/bin/g++-13",
        &default_cpp_flags(),
        &[],
    );
    assert_eq!(result.compile.unwrap().command[0], "/usr/local/bin/g++-13");
}

#[test]
fn cpp_flag_override_replaces_defaults() {
    let custom_flags = vec!["-O3".into(), "-std=c++20".into()];
    let result = resolve_cpp(
        &req("cpp", vec!["solution.cpp"]),
        None,
        "/usr/bin/g++",
        &custom_flags,
        &[],
    );
    let cmd = result.compile.unwrap().command;
    assert!(cmd.contains(&"-O3".to_string()));
    assert!(cmd.contains(&"-std=c++20".to_string()));
    assert!(!cmd.contains(&"-O2".to_string()));
    // Structural args preserved
    assert!(cmd.contains(&"-o".to_string()));
    assert!(cmd.contains(&"solution".to_string()));
}

#[test]
fn cpp_extra_compile_flags() {
    let result = resolve_cpp(
        &req("cpp", vec!["solution.cpp"]),
        None,
        "/usr/bin/g++",
        &default_cpp_flags(),
        &["-DONLINE_JUDGE".into()],
    );
    let cmd = result.compile.unwrap().command;
    assert!(cmd.contains(&"-DONLINE_JUDGE".to_string()));
    assert!(cmd.contains(&"-O2".to_string()));
}

// C
#[test]
fn c_compiles_with_gcc() {
    let result = resolve_c(
        &req("c", vec!["solution.c"]),
        None,
        "/usr/bin/gcc",
        &default_c_flags(),
        &[],
    );
    let compile = result.compile.unwrap();
    assert_eq!(compile.command[0], "/usr/bin/gcc");
    assert!(compile.command.contains(&"-std=c17".to_string()));
    // -lm should come after source files and -o binary
    let lm_pos = compile.command.iter().position(|a| a == "-lm").unwrap();
    let source_pos = compile
        .command
        .iter()
        .position(|a| a == "./solution.c")
        .unwrap();
    assert!(lm_pos > source_pos, "-lm should be after source files");
}

// Python 3
#[test]
fn python_interpreted() {
    let result = resolve_python3(
        &req("python3", vec!["solution.py"]),
        None,
        "/usr/bin/python3",
    );
    assert!(result.compile.is_none());
    assert_eq!(result.run.command, vec!["/usr/bin/python3", "solution.py"]);
    assert_eq!(result.run.extra_files, vec!["solution.py"]);
}

#[test]
fn python_with_grader() {
    let result = resolve_python3(
        &req("python3", vec!["solution.py", "grader.py"]),
        None,
        "/usr/bin/python3",
    );
    assert_eq!(result.run.command, vec!["/usr/bin/python3", "solution.py"]);
    assert_eq!(result.run.extra_files, vec!["solution.py", "grader.py"]);
}

// Java
#[test]
fn java_uses_glob_output() {
    let result = resolve_java(
        &req("java", vec!["Solver.java"]),
        None,
        "javac",
        "java",
        &[],
        &[],
    );
    let compile = result.compile.unwrap();
    assert_eq!(
        compile.command,
        vec!["javac", "-J-XX:ActiveProcessorCount=1", "./Solver.java"]
    );
    assert_eq!(compile.outputs, vec![OutputSpec::Glob("*.class".into())]);
    assert_eq!(
        result.run.command,
        vec!["java", "-XX:ActiveProcessorCount=1", "-cp", ".", "Solver"]
    );
}

#[test]
fn java_prefixes_sources_to_block_argfile_injection() {
    // A filename beginning with `@` is a javac argfile directive (`@file` reads
    // compiler options from `file`). Every source token must be `./`-prefixed so
    // javac reads it as a path, not an argfile -- otherwise a contestant injects
    // javac flags into the trusted grader compile. `validate_flat_filename`
    // rejects a leading `-` but not a leading `@`, so this is the real guard.
    let mut input = req("java", vec!["Main.java"]);
    input.additional_files = vec![FileRef {
        filename: "@opts".into(),
        content_type: None,
        blob_hash: "opts-hash".into(),
        read_token: None,
    }];
    let compile = resolve_java(&input, None, "javac", "java", &[], &[])
        .compile
        .unwrap();
    // The `@opts` file is neutralized to `./@opts`, never a bare `@opts`.
    assert!(
        compile.command.contains(&"./@opts".to_string()),
        "argfile-shaped name must be path-prefixed: {:?}",
        compile.command
    );
    assert!(
        !compile.command.iter().any(|a| a.starts_with('@')),
        "no argv token may start with `@`: {:?}",
        compile.command
    );
}

#[test]
fn entry_point_override_changes_primary() {
    let result = resolve_python3(
        &req("python3", vec!["solution.py", "grader.py"]),
        Some(&EntryPointConfig {
            entry_point: Some("grader.py".into()),
            extra_compile_flags: None,
        }),
        "/usr/bin/python3",
    );
    assert_eq!(result.run.command, vec!["/usr/bin/python3", "grader.py"]);
    assert!(result.run.extra_files.contains(&"solution.py".to_string()));
    assert!(result.run.extra_files.contains(&"grader.py".to_string()));
}

#[test]
fn cpp_with_additional_file_refs() {
    let mut input = req("cpp", vec!["solution.cpp"]);
    input.additional_files = vec![
        FileRef {
            filename: "grader.cpp".into(),
            content_type: Some("text/x-c++src".into()),
            blob_hash: "grader-cpp-hash".into(),
            read_token: None,
        },
        FileRef {
            filename: "grader.h".into(),
            content_type: Some("text/x-c".into()),
            blob_hash: "grader-h-hash".into(),
            read_token: None,
        },
    ];
    let result = resolve_cpp(&input, None, "/usr/bin/g++", &default_cpp_flags(), &[]);
    let compile = result.compile.unwrap();
    assert!(compile.command.contains(&"./grader.cpp".to_string()));
    assert!(compile.command.contains(&"./grader.h".to_string()));
    assert_eq!(
        compile.cache_inputs,
        vec!["solution.cpp", "grader.cpp", "grader.h"]
    );
}

// Kotlin
#[test]
fn kotlin_builds_a_self_contained_jar_and_runs_it_on_the_jvm() {
    let result = resolve_default("kotlin", vec!["solution.kt", "util.kt"]);
    let compile = result.compile.unwrap();
    assert_eq!(
        compile.command,
        vec![
            "/usr/local/bin/kotlinc",
            "-J-XX:ActiveProcessorCount=1",
            "./solution.kt",
            "./util.kt",
            "-include-runtime",
            "-d",
            "solution.jar",
        ]
    );
    assert_eq!(
        compile.outputs,
        vec![OutputSpec::File("solution.jar".into())]
    );
    assert_eq!(
        result.run.command,
        vec![
            "/usr/bin/java",
            "-XX:ActiveProcessorCount=1",
            "-jar",
            "solution.jar"
        ]
    );
    assert_eq!(result.run.min_process_limit, Some(64));
}

// Rust
#[test]
fn rust_compiles_the_crate_root_only() {
    let result = resolve_default("rust", vec!["solution.rs", "graph.rs"]);
    let compile = result.compile.unwrap();
    assert_eq!(
        compile.command,
        vec!["/usr/bin/rustc", "-O", "./solution.rs", "-o", "solution"]
    );
    assert_eq!(compile.cache_inputs, vec!["solution.rs", "graph.rs"]);
    assert_eq!(result.run.command, vec!["./solution"]);
}

// Go
#[test]
fn go_builds_with_a_sandbox_cache_and_one_scheduler_thread() {
    let result = resolve_default("go", vec!["solution.go", "grader.go"]);
    let compile = result.compile.unwrap();
    assert_eq!(
        compile.command,
        vec![
            "/usr/bin/env",
            "GOCACHE=/tmp/go-build",
            "GOMAXPROCS=1",
            "/usr/bin/go",
            "build",
            "-o",
            "solution",
            "./solution.go",
            "./grader.go",
        ]
    );
    assert_eq!(
        result.run.command,
        vec!["/usr/bin/env", "GOMAXPROCS=1", "./solution"]
    );
    assert_eq!(result.run.min_process_limit, Some(16));
}

// Pascal
#[test]
fn pascal_compiles_with_fpc() {
    let result = resolve_default("pascal", vec!["solution.pas"]);
    let compile = result.compile.unwrap();
    assert_eq!(
        compile.command,
        vec!["/usr/bin/fpc", "-O2", "-osolution", "./solution.pas"]
    );
    assert_eq!(compile.outputs, vec![OutputSpec::File("solution".into())]);
    assert_eq!(result.run.command, vec!["./solution"]);
}

// JavaScript
#[test]
fn javascript_runs_on_node_with_its_thread_floor() {
    let result = resolve_default("javascript", vec!["solution.js"]);
    assert!(result.compile.is_none());
    assert_eq!(result.run.command, vec!["/usr/bin/node", "solution.js"]);
    assert_eq!(result.run.min_process_limit, Some(16));
}

#[test]
fn every_language_resolves_its_default_file_by_extension() {
    for language in resolve::LANGUAGES {
        let file = format!("main.{}", language.extensions[0]);
        let result = resolve_default(language.id, vec!["notes.txt", &file]);
        let mentions_source = |argv: &[String]| argv.iter().any(|a| a.ends_with(&file));
        let compiled = result
            .compile
            .as_ref()
            .is_some_and(|c| mentions_source(&c.command));
        assert!(
            compiled || mentions_source(&result.run.command),
            "{} did not pick {file}",
            language.id
        );
    }
}
