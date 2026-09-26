use broccoli_server_sdk::types::*;
use serde::Deserialize;

use crate::util::truncate;

#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::Host;

/// Compiler configuration for checker binaries (`standard-languages` plugin).
#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct CheckerCompilerConfig {
    pub compiler: String,
    pub flags: Vec<String>,
}

impl Default for CheckerCompilerConfig {
    fn default() -> Self {
        Self {
            compiler: "/usr/bin/g++".into(),
            flags: vec!["-O2".into(), "-std=c++17".into()],
        }
    }
}

/// Testlib checker configuration, loaded from plugin global config.
#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct TestlibConfig {
    pub cpp: CheckerCompilerConfig,
    pub c: CheckerCompilerConfig,
    pub compile_time_limit_s: f64,
    pub compile_memory_limit_kb: u32,
    pub run_time_limit_s: f64,
    pub run_memory_limit_kb: u32,
}

impl Default for TestlibConfig {
    fn default() -> Self {
        Self {
            cpp: CheckerCompilerConfig {
                compiler: "/usr/bin/g++".into(),
                flags: vec!["-O2".into(), "-std=c++17".into()],
            },
            c: CheckerCompilerConfig {
                compiler: "/usr/bin/gcc".into(),
                flags: vec!["-O2".into(), "-std=c11".into()],
            },
            compile_time_limit_s: 10.0,
            compile_memory_limit_kb: 512 * 1024,
            // Checkers process BOTH the solution output and the answer, so their
            // working set scales with output size. Give them far more headroom than
            // a solution: a stingy limit kills the checker on large-output problems
            // (-> opaque SystemError), not the contestant's code.
            // Fallback only: the EFFECTIVE limits come from plugin.toml's
            // [config.testlib] schema defaults (get_global("testlib")). Keep these
            // in sync with plugin.toml. A checker reads BOTH the output and the
            // answer, so its working set scales with output size -- 256 MB was too
            // small and OOM-killed the checker on large-output problems.
            run_time_limit_s: 20.0,
            run_memory_limit_kb: 1024 * 1024, // 1 GiB (match plugin.toml)
        }
    }
}

/// True for a C/C++ compile unit - the only files that belong on the compiler
/// command line. Everything else in a checker bundle (headers like testlib.h,
/// and any auxiliary data files) is mounted for `#include`/runtime use but never
/// compiled.
pub fn is_checker_source(filename: &str) -> bool {
    let f = filename.to_ascii_lowercase();
    f.ends_with(".cpp") || f.ends_with(".cc") || f.ends_with(".cxx") || f.ends_with(".c")
}

/// Split a checker bundle into compile units (`.c`/`.cpp`/`.cc`/`.cxx`) and
/// everything else (headers, data files), preserving order. The first compile
/// unit is the primary (drives language detection and the compile command); all
/// compile units are compiled and linked; non-sources are excluded from the
/// command line but stay mounted in the checker env (e.g. so `#include
/// "testlib.h"` resolves). Errors if there is no compilable source.
pub fn partition_checker_sources(
    filenames: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let (sources, others): (Vec<String>, Vec<String>) = filenames
        .iter()
        .cloned()
        .partition(|f| is_checker_source(f));
    if sources.is_empty() {
        return Err(format!(
            "checker_source has no compilable .c/.cpp file (got: {}). \
             Upload the checker's .cpp; bundle testlib.h alongside it as a header.",
            others.join(", ")
        ));
    }
    Ok((sources, others))
}

/// Map checker source file extension to a language ID for the resolver.
pub fn checker_language_id(primary_filename: &str) -> Result<&str, String> {
    if primary_filename.ends_with(".cpp")
        || primary_filename.ends_with(".cc")
        || primary_filename.ends_with(".cxx")
    {
        Ok("cpp")
    } else if primary_filename.ends_with(".c") {
        Ok("c")
    } else {
        Err(format!(
            "Unsupported checker source language for '{}'. Only C (.c) and C++ (.cpp/.cc/.cxx) are supported.",
            primary_filename
        ))
    }
}

/// Fold every file of the checker bundle into the compile cache inputs. The
/// language resolver only sees the compile units, so headers (testlib.h, custom
/// helper .h) and auxiliary files consumed at compile time never reach
/// `cache_inputs` on their own - and the worker's compile cache key hashes only
/// argv + cache-input file contents. Without this, an author who fixes a bug in
/// a bundled header without touching checker.cpp gets an unchanged cache key
/// and every worker silently keeps judging with the STALE compiled checker.
/// All bundle files are mounted in the checker env, so the worker can hash them.
pub fn extend_cache_inputs_with_bundle(
    resolved: &mut ResolveLanguageOutput,
    bundle_filenames: &[String],
) {
    if let Some(compile) = resolved.compile.as_mut() {
        for f in bundle_filenames {
            if !compile.cache_inputs.contains(f) {
                compile.cache_inputs.push(f.clone());
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn load_testlib_config(host: &Host) -> TestlibConfig {
    match host.config.get_global("testlib") {
        Ok(r) => serde_json::from_value(r.config).unwrap_or_default(),
        Err(_) => TestlibConfig::default(),
    }
}

/// Pure interpretation of testlib exit codes.
pub fn interpret_testlib_exit_code(exit_code: i32, stderr: &str) -> CheckerVerdict {
    match exit_code {
        0 => CheckerVerdict {
            verdict: Verdict::Accepted,
            score: 1.0,
            message: extract_testlib_message(stderr),
        },
        1 => CheckerVerdict {
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            message: extract_testlib_message(stderr),
        },
        2 => CheckerVerdict {
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            message: extract_testlib_message(stderr).or_else(|| Some("Presentation error".into())),
        },
        3 => CheckerVerdict {
            verdict: Verdict::SystemError,
            score: 0.0,
            message: extract_testlib_message(stderr)
                .or_else(|| Some("Checker reported judge failure (exit code 3)".into())),
        },
        // testlib DIRT_EXIT_CODE (4). Emitted by testlib's OWN epilogue when a
        // checker reports _ok/_points/_partially but the participant left extra
        // unread non-whitespace output ("Extra information in the output file").
        // This is the DEFAULT behavior (no -D flags; only EJUDGE remaps it to 6),
        // so any fixed-read checker rejecting trailing junk lands here. It is a
        // REJECTION of the participant output, not a judge failure -- testlib does
        // NOT downgrade _dirt to _pe. The old `other =>` catch-all mapped it to
        // SystemError, converting a deterministic WrongAnswer into a NON-self-
        // healing infra error that reproduces on every re-dispatch and never
        // resolves to the real WA.
        4 => CheckerVerdict {
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            message: extract_testlib_message(stderr)
                .or_else(|| Some("Extra information in the output file".into())),
        },
        // testlib UNEXPECTED_EOF_EXIT_CODE (8). Only emitted when the checker is
        // built with -DENABLE_UNEXPECTED_EOF; otherwise testlib downgrades an
        // unexpected EOF to _pe -> exit 2 (handled above). The participant output
        // ended before the checker could read an expected token: a rejection, not
        // a judge failure.
        8 => CheckerVerdict {
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            message: extract_testlib_message(stderr)
                .or_else(|| Some("Unexpected end of the participant output".into())),
        },
        7 => {
            let (score, msg) = parse_testlib_partial(stderr);
            let verdict = if score >= 1.0 {
                Verdict::Accepted
            } else {
                Verdict::WrongAnswer
            };
            CheckerVerdict {
                verdict,
                score,
                message: msg,
            }
        }
        // Any other code is not a recognized testlib verdict. This deliberately
        // INCLUDES the _partially band (PARTIALLY_EXIT_CODE = 16 plus a pctype
        // offset): broccoli's partial-credit path is exit 7 (`points X`), and the
        // score encoded in the 16+ band is not authoritatively pinned, so we do
        // not guess a partial score here. Treat it as a judge failure (SystemError
        // self-heals) rather than silently minting a wrong partial verdict.
        other => CheckerVerdict {
            verdict: Verdict::SystemError,
            score: 0.0,
            message: Some(format!("Checker exited with unexpected code {}", other)),
        },
    }
}

/// Build the cached `compile_checker` step for a testlib checker, or `None` when
/// the language needs no compile. Used by the fused stage builder; `env_id` is
/// the environment the step runs in.
pub(crate) fn build_compile_checker_step(
    resolved: &ResolveLanguageOutput,
    config: &TestlibConfig,
    env_id: &str,
) -> Option<Step> {
    let compile = resolved.compile.as_ref()?;

    let cache_outputs: Vec<String> = compile
        .outputs
        .iter()
        .map(|o| match o {
            OutputSpec::File(f) => f.clone(),
            OutputSpec::Glob(g) => g.clone(),
        })
        .collect();
    let mut collect = cache_outputs.clone();
    collect.push("checker_compile.log".to_string());
    collect.push("checker_compile_err.log".to_string());

    Some(Step {
        id: "compile_checker".to_string(),
        kind: StepKind::CheckerCompile,
        env_ref: env_id.to_string(),
        argv: compile.command.clone(),
        conf: RunOptions {
            resource_limits: ResourceLimits {
                time_limit: Some(config.compile_time_limit_s),
                memory_limit: Some(config.compile_memory_limit_kb),
                process_limit: Some(64),
                ..Default::default()
            },
            // No inherited worker environment: the compiler is invoked by absolute
            // path and finds its tools via the worker's minimal default PATH, so
            // there is no reason to expose the worker's secrets to checker compiles.
            env_rules: vec![],
            ..Default::default()
        },
        io: IOConfig {
            stdin: IOTarget::Null,
            stdout: IOTarget::File {
                path: "checker_compile.log".to_string(),
            },
            stderr: IOTarget::File {
                path: "checker_compile_err.log".to_string(),
            },
        },
        collect,
        depends_on: vec![],
        cache: Some(StepCacheConfig {
            key_inputs: compile.cache_inputs.clone(),
            outputs: cache_outputs,
        }),
        mounts: vec![],
    })
}

/// The in-sandbox command to invoke the compiled testlib checker (e.g.
/// `./checker`). Defaults to `./checker` when there is no compile output.
pub(crate) fn testlib_checker_binary(resolved: &ResolveLanguageOutput) -> String {
    resolved
        .compile
        .as_ref()
        .and_then(|c| c.outputs.first())
        .map(|o| match o {
            OutputSpec::File(f) => format!("./{f}"),
            OutputSpec::Glob(_) => "./checker".to_string(), // unreachable since it's always C/C++
        })
        .unwrap_or_else(|| "./checker".to_string())
}

/// Resolve the testlib checker's compile/run spec + config by asking the
/// language plugin to resolve its source language. Used by the fused resolver
/// (`resolve_checker`) to build the checker stage.
#[cfg(target_arch = "wasm32")]
pub(crate) fn resolve_testlib_compile(
    host: &Host,
    checker_source: &[SourceFile],
) -> Result<(ResolveLanguageOutput, TestlibConfig), String> {
    if checker_source.is_empty() {
        return Err("checker_source is empty".to_string());
    }

    let filenames: Vec<String> = checker_source.iter().map(|f| f.filename.clone()).collect();
    // Compile only the source files; headers (testlib.h, ...) stay mounted in the
    // checker env for #include but must not appear on the compile command line and
    // must not drive language detection.
    let (sources, _headers) = partition_checker_sources(&filenames)?;
    let lang_id = checker_language_id(&sources[0])?;
    let config = load_testlib_config(host);
    let checker_compiler = match lang_id {
        "cpp" => &config.cpp,
        "c" => &config.c,
        _ => &config.cpp, // unreachable for testlib (always C/C++)
    };

    let mut resolved = host
        .language
        .resolve(&ResolveLanguageInput {
            language_id: lang_id.to_string(),
            submitted_files: sources,
            additional_files: vec![],
            problem_id: None,
            contest_id: None,
            overrides: Some(serde_json::json!({
                "compiler": checker_compiler.compiler,
                "flags": checker_compiler.flags,
            })),
        })
        .map_err(|e| format!("Failed to resolve checker language: {e}"))?;

    // The resolver derived cache_inputs from the compile units only. Headers and
    // auxiliary files also shape the compiled binary (#include, data tables), so
    // they MUST be part of the compile cache key or edits to them serve a stale
    // checker. (They cannot go through additional_files: the language resolver
    // would put them on the compiler command line.)
    extend_cache_inputs_with_bundle(&mut resolved, &filenames);

    Ok((resolved, config))
}

fn extract_testlib_message(stderr: &str) -> Option<String> {
    let msg = stderr.trim();
    if msg.is_empty() {
        None
    } else {
        Some(truncate(msg, 1024))
    }
}

pub fn parse_testlib_partial(stderr: &str) -> (f64, Option<String>) {
    let line = stderr.lines().next().unwrap_or("").trim();

    if let Some(rest) = line
        .strip_prefix("points ")
        .or_else(|| line.strip_prefix("points\t"))
    {
        let parts: Vec<&str> = rest.splitn(2, |c: char| c.is_whitespace()).collect();
        if let Some(score) = parts.first().and_then(|s| s.parse::<f64>().ok()) {
            let score = if score.is_finite() {
                score.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let message = parts.get(1).map(|m| truncate(m.trim(), 1024));
            return (score, message);
        }
    }

    if let Some(first_token) = line.split_whitespace().next()
        && let Ok(score) = first_token.parse::<f64>()
    {
        let score = if score.is_finite() {
            score.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let rest = line.get(first_token.len()..).unwrap_or("").trim();
        let message = if rest.is_empty() {
            None
        } else {
            Some(truncate(rest, 1024))
        };
        return (score, message);
    }

    (0.0, extract_testlib_message(stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_0_accepted() {
        let v = interpret_testlib_exit_code(0, "ok answer is correct\n");
        assert_eq!(v.verdict, Verdict::Accepted);
        assert_eq!(v.score, 1.0);
    }

    #[test]
    fn exit_1_wrong_answer() {
        let v = interpret_testlib_exit_code(1, "wrong answer expected 42, got 43\n");
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert_eq!(v.score, 0.0);
        assert!(v.message.unwrap().contains("expected 42"));
    }

    #[test]
    fn exit_2_presentation_error() {
        let v = interpret_testlib_exit_code(2, "");
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert!(v.message.unwrap().contains("Presentation error"));
    }

    #[test]
    fn exit_3_system_error() {
        let v = interpret_testlib_exit_code(3, "FAIL checker bug\n");
        assert_eq!(v.verdict, Verdict::SystemError);
        assert_eq!(v.score, 0.0);
    }

    #[test]
    fn exit_4_dirt_is_wrong_answer() {
        // testlib's DIRT_EXIT_CODE: checker accepted but the participant left
        // extra unread output. A rejection (WrongAnswer), NOT a SystemError.
        let v = interpret_testlib_exit_code(
            4,
            "wrong output format Extra information in the output file\n",
        );
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert_eq!(v.score, 0.0);
        assert!(v.message.unwrap().contains("Extra information"));
    }

    #[test]
    fn exit_4_dirt_default_message() {
        let v = interpret_testlib_exit_code(4, "   ");
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert_eq!(v.message.unwrap(), "Extra information in the output file");
    }

    #[test]
    fn exit_8_unexpected_eof_is_wrong_answer() {
        // UNEXPECTED_EOF_EXIT_CODE (checker built with -DENABLE_UNEXPECTED_EOF):
        // participant output ended early. A rejection, NOT a SystemError.
        let v = interpret_testlib_exit_code(8, "");
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert_eq!(v.score, 0.0);
        assert!(v.message.unwrap().contains("Unexpected end"));
    }

    #[test]
    fn unknown_code_stays_system_error() {
        // Codes with no confident testlib meaning (incl. the 16+ _partially band,
        // whose score encoding is not pinned) remain a self-healing SystemError
        // rather than a guessed verdict.
        for code in [5, 6, 16, 99] {
            let v = interpret_testlib_exit_code(code, "");
            assert_eq!(v.verdict, Verdict::SystemError, "code {code}");
            assert_eq!(v.score, 0.0, "code {code}");
        }
    }

    #[test]
    fn exit_7_partial() {
        let v = interpret_testlib_exit_code(7, "points 0.5 partially correct\n");
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert_eq!(v.score, 0.5);
        assert_eq!(v.message.unwrap(), "partially correct");
    }

    #[test]
    fn exit_7_full_score() {
        let v = interpret_testlib_exit_code(7, "points 1.0 perfect\n");
        assert_eq!(v.verdict, Verdict::Accepted);
        assert_eq!(v.score, 1.0);
    }

    #[test]
    fn checker_language_id_cpp() {
        assert_eq!(checker_language_id("checker.cpp").unwrap(), "cpp");
        assert_eq!(checker_language_id("checker.cc").unwrap(), "cpp");
        assert_eq!(checker_language_id("checker.cxx").unwrap(), "cpp");
    }

    #[test]
    fn checker_language_id_c() {
        assert_eq!(checker_language_id("checker.c").unwrap(), "c");
    }

    #[test]
    fn checker_language_id_unsupported() {
        let err = checker_language_id("checker.py").unwrap_err();
        assert!(err.contains("Unsupported"));
    }

    #[test]
    fn is_checker_source_detects_compile_units() {
        for s in ["checker.cpp", "checker.c", "checker.cc", "checker.CXX"] {
            assert!(is_checker_source(s), "{s} is a compile unit");
        }
        for x in ["testlib.h", "foo.hpp", "bar.HH", "data.txt", "gen.py"] {
            assert!(!is_checker_source(x), "{x} is not a compile unit");
        }
    }

    #[test]
    fn partition_skips_header_first_bundle() {
        // Regression: author bundled testlib.h BEFORE checker.cpp, so the resolver
        // picked filenames[0] = testlib.h and failed with "Unsupported checker
        // source language for 'testlib.h'" -> SystemError. The primary must be the
        // .cpp; the header stays mounted (not a compile unit).
        let files = vec!["testlib.h".to_string(), "checker.cpp".to_string()];
        let (sources, others) = partition_checker_sources(&files).unwrap();
        assert_eq!(sources, vec!["checker.cpp".to_string()]);
        assert_eq!(others, vec!["testlib.h".to_string()]);
        assert_eq!(checker_language_id(&sources[0]).unwrap(), "cpp");
    }

    #[test]
    fn partition_compiles_all_sources_mounts_the_rest() {
        // Multiple .cpp are all compiled+linked; headers AND auxiliary files
        // (e.g. a data table the checker reads) are mounted, never compiled.
        let files = vec![
            "a.cpp".to_string(),
            "testlib.h".to_string(),
            "b.cpp".to_string(),
            "table.txt".to_string(),
        ];
        let (sources, others) = partition_checker_sources(&files).unwrap();
        assert_eq!(sources, vec!["a.cpp".to_string(), "b.cpp".to_string()]);
        assert_eq!(
            others,
            vec!["testlib.h".to_string(), "table.txt".to_string()]
        );
    }

    #[test]
    fn partition_errors_when_no_source() {
        let err = partition_checker_sources(&["testlib.h".to_string()]).unwrap_err();
        assert!(err.contains("no compilable"), "got: {err}");
    }

    fn resolved_compile_with_inputs(cache_inputs: Vec<String>) -> ResolveLanguageOutput {
        ResolveLanguageOutput {
            compile: Some(CompileSpec {
                command: vec![
                    "/usr/bin/g++".to_string(),
                    "checker.cpp".to_string(),
                    "-o".to_string(),
                    "checker".to_string(),
                ],
                cache_inputs,
                outputs: vec![OutputSpec::File("checker".to_string())],
                resource_limits: None,
            }),
            run: RunSpec {
                command: vec!["./checker".to_string()],
                extra_files: vec![],
                min_process_limit: None,
            },
        }
    }

    #[test]
    fn cache_inputs_cover_headers_and_aux_files() {
        // Regression: the compile cache key hashed only the compile units, so
        // fixing a bug in testlib.h or a bundled data table without touching
        // checker.cpp kept the old key and served the STALE compiled checker.
        let mut resolved = resolved_compile_with_inputs(vec!["checker.cpp".to_string()]);
        let bundle = vec![
            "checker.cpp".to_string(),
            "testlib.h".to_string(),
            "table.txt".to_string(),
        ];
        extend_cache_inputs_with_bundle(&mut resolved, &bundle);

        let inputs = &resolved.compile.as_ref().unwrap().cache_inputs;
        assert!(inputs.contains(&"checker.cpp".to_string()));
        assert!(inputs.contains(&"testlib.h".to_string()), "header in key");
        assert!(inputs.contains(&"table.txt".to_string()), "aux file in key");
    }

    #[test]
    fn cache_inputs_extension_does_not_duplicate_sources() {
        let mut resolved =
            resolved_compile_with_inputs(vec!["a.cpp".to_string(), "b.cpp".to_string()]);
        let bundle = vec![
            "a.cpp".to_string(),
            "b.cpp".to_string(),
            "testlib.h".to_string(),
        ];
        extend_cache_inputs_with_bundle(&mut resolved, &bundle);

        assert_eq!(
            resolved.compile.as_ref().unwrap().cache_inputs,
            vec![
                "a.cpp".to_string(),
                "b.cpp".to_string(),
                "testlib.h".to_string()
            ]
        );
    }

    #[test]
    fn cache_inputs_extension_noop_without_compile() {
        let mut resolved = ResolveLanguageOutput {
            compile: None,
            run: RunSpec {
                command: vec!["./checker".to_string()],
                extra_files: vec![],
                min_process_limit: None,
            },
        };
        extend_cache_inputs_with_bundle(&mut resolved, &["testlib.h".to_string()]);
        assert!(resolved.compile.is_none());
    }
}
