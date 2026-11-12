use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Args;
use console::style;
use dialoguer::Select;

use crate::template::variables::{
    TemplateVars, to_pascal_case, to_snake_case, validate_plugin_name,
};
use crate::template::write_template;

const TMPL_PLUGIN_HEADER: &str = include_str!("../../../templates/plugin_header.toml.tmpl");
const TMPL_PLUGIN_SERVER: &str = include_str!("../../../templates/plugin_server.toml.tmpl");
const TMPL_PLUGIN_WEB: &str = include_str!("../../../templates/plugin_web.toml.tmpl");

const TMPL_CARGO_TOML: &str = include_str!("../../../templates/backend/Cargo.toml.tmpl");
const TMPL_RUST_TOOLCHAIN: &str =
    include_str!("../../../templates/backend/rust-toolchain.toml.tmpl");
const TMPL_LIB_RS: &str = include_str!("../../../templates/backend/src/lib.rs.tmpl");
const TMPL_GITIGNORE: &str = include_str!("../../../templates/backend/.gitignore.tmpl");

const TMPL_PACKAGE_JSON: &str = include_str!("../../../templates/frontend/package.json.tmpl");
const TMPL_TSCONFIG: &str = include_str!("../../../templates/frontend/tsconfig.json.tmpl");
const TMPL_INDEX_TSX: &str = include_str!("../../../templates/frontend/src/index.tsx.tmpl");
const TMPL_I18N_EN: &str = include_str!("../../../templates/i18n/en.toml.tmpl");
const STATIC_STYLES_CSS: &str = include_str!("../../../templates/frontend/src/styles.css");
const STATIC_POSTCSS_CONFIG: &str = include_str!("../../../templates/frontend/postcss.config.js");

#[derive(Args)]
pub struct NewPluginArgs {
    pub name: String,

    #[arg(long, conflicts_with_all = ["frontend", "full"])]
    pub backend: bool,

    #[arg(long, conflicts_with_all = ["backend", "full"])]
    pub frontend: bool,

    #[arg(long, conflicts_with_all = ["backend", "frontend"])]
    pub full: bool,

    #[arg(long, short)]
    pub output: Option<PathBuf>,

    #[arg(long, default_value = r#"path = "../../server-sdk""#)]
    pub server_sdk: String,

    #[arg(long, default_value = "workspace:*")]
    pub web_sdk: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldKind {
    Backend,
    Frontend,
    Full,
}

pub fn run(args: NewPluginArgs) -> Result<()> {
    validate_plugin_name(&args.name)?;

    let kind = determine_kind(&args)?;
    let output_dir = args.output.unwrap_or_else(|| PathBuf::from(&args.name));

    if output_dir.exists() {
        bail!(
            "Directory '{}' already exists. Remove it or choose a different name.",
            output_dir.display()
        );
    }

    let web_root = match kind {
        ScaffoldKind::Frontend => "dist".to_string(),
        ScaffoldKind::Full => "frontend/dist".to_string(),
        ScaffoldKind::Backend => String::new(),
    };

    let vars = TemplateVars {
        plugin_name: args.name.clone(),
        plugin_name_snake: to_snake_case(&args.name),
        plugin_name_pascal: to_pascal_case(&args.name),
        server_sdk_dep: args.server_sdk,
        web_sdk_dep: args.web_sdk,
        web_root,
    };

    let mut created_files: Vec<String> = Vec::new();

    write_plugin_toml(&output_dir, &vars, kind)?;
    created_files.push("plugin.toml".into());

    if matches!(kind, ScaffoldKind::Backend | ScaffoldKind::Full) {
        write_backend_files(&output_dir, &vars, &mut created_files)?;
    }

    if matches!(kind, ScaffoldKind::Frontend | ScaffoldKind::Full) {
        let fe_root = match kind {
            ScaffoldKind::Full => output_dir.join("frontend"),
            _ => output_dir.clone(),
        };
        let prefix = match kind {
            ScaffoldKind::Full => "frontend/",
            _ => "",
        };
        write_frontend_files(&fe_root, &vars, prefix, &mut created_files)?;

        write_template(TMPL_I18N_EN, &output_dir.join("i18n/en.toml"), &vars)?;
        created_files.push("i18n/en.toml".into());
    }

    print_summary(&args.name, kind, &output_dir, &created_files);

    Ok(())
}

fn determine_kind(args: &NewPluginArgs) -> Result<ScaffoldKind> {
    if args.backend {
        return Ok(ScaffoldKind::Backend);
    }
    if args.frontend {
        return Ok(ScaffoldKind::Frontend);
    }
    if args.full {
        return Ok(ScaffoldKind::Full);
    }

    let options = &[
        "Backend (Rust/WASM)",
        "Frontend (React/TypeScript)",
        "Full (Backend + Frontend)",
    ];
    let selection = Select::new()
        .with_prompt("What kind of plugin would you like to create?")
        .items(options)
        .default(2)
        .interact_opt()?;

    match selection {
        Some(0) => Ok(ScaffoldKind::Backend),
        Some(1) => Ok(ScaffoldKind::Frontend),
        Some(_) => Ok(ScaffoldKind::Full),
        None => bail!(
            "No scaffold kind selected. Use --backend, --frontend, or --full in non-interactive mode."
        ),
    }
}

fn write_plugin_toml(dir: &Path, vars: &TemplateVars, kind: ScaffoldKind) -> Result<()> {
    let mut content = crate::template::render(TMPL_PLUGIN_HEADER, vars);

    if matches!(kind, ScaffoldKind::Backend | ScaffoldKind::Full) {
        content.push_str(&crate::template::render(TMPL_PLUGIN_SERVER, vars));
    }

    if matches!(kind, ScaffoldKind::Frontend | ScaffoldKind::Full) {
        content.push_str(&crate::template::render(TMPL_PLUGIN_WEB, vars));
    }

    let path = dir.join("plugin.toml");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn write_backend_files(dir: &Path, vars: &TemplateVars, files: &mut Vec<String>) -> Result<()> {
    let mut templates: Vec<(&str, &str)> = vec![
        ("Cargo.toml", TMPL_CARGO_TOML),
        ("src/lib.rs", TMPL_LIB_RS),
        (".gitignore", TMPL_GITIGNORE),
    ];
    // rustup resolves `rust-toolchain.toml` upward from the build directory, so
    // a plugin created inside a project that pins a toolchain (the Broccoli
    // repository does) builds with that pin. Only a standalone plugin gets
    // its own file.
    if !inherits_toolchain(dir) {
        templates.push(("rust-toolchain.toml", TMPL_RUST_TOOLCHAIN));
    }

    for (rel_path, template) in templates {
        write_template(template, &dir.join(rel_path), vars)?;
        files.push((*rel_path).to_string());
    }

    Ok(())
}

/// Whether a directory above `dir` pins a Rust toolchain that rustup applies
/// to builds in `dir`.
fn inherits_toolchain(dir: &Path) -> bool {
    dir.canonicalize().is_ok_and(|dir| {
        dir.ancestors()
            .skip(1)
            .any(|a| a.join("rust-toolchain.toml").is_file() || a.join("rust-toolchain").is_file())
    })
}

fn write_frontend_files(
    dir: &Path,
    vars: &TemplateVars,
    prefix: &str,
    files: &mut Vec<String>,
) -> Result<()> {
    let templates: &[(&str, &str)] = &[
        ("package.json", TMPL_PACKAGE_JSON),
        ("tsconfig.json", TMPL_TSCONFIG),
        ("src/index.tsx", TMPL_INDEX_TSX),
    ];

    for (rel_path, template) in templates {
        write_template(template, &dir.join(rel_path), vars)?;
        files.push(format!("{prefix}{rel_path}"));
    }

    let static_files: &[(&str, &str)] = &[
        ("src/styles.css", STATIC_STYLES_CSS),
        ("postcss.config.js", STATIC_POSTCSS_CONFIG),
    ];

    for (rel_path, content) in static_files {
        let path = dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        files.push(format!("{prefix}{rel_path}"));
    }

    Ok(())
}

fn print_summary(name: &str, kind: ScaffoldKind, dir: &Path, files: &[String]) {
    let kind_label = match kind {
        ScaffoldKind::Backend => "backend",
        ScaffoldKind::Frontend => "frontend",
        ScaffoldKind::Full => "full",
    };

    println!(
        "\n{}  Created {} plugin {}",
        style("✓").green().bold(),
        kind_label,
        style(name).cyan().bold()
    );
    println!("   {}\n", style(dir.display()).dim());

    for (i, file) in files.iter().enumerate() {
        let connector = if i == files.len() - 1 {
            "└──"
        } else {
            "├──"
        };
        println!("   {connector} {file}");
    }

    println!();

    match kind {
        ScaffoldKind::Backend | ScaffoldKind::Full => {
            println!("   Next steps:");
            println!(
                "     cd {} && cargo build --target wasm32-wasip1 --release",
                dir.display()
            );
        }
        ScaffoldKind::Frontend => {
            println!("   Next steps:");
            println!("     cd {} && pnpm install && pnpm build", dir.display());
        }
    }

    if kind == ScaffoldKind::Full {
        println!(
            "     cd {}/frontend && pnpm install && pnpm build",
            dir.display()
        );
    }

    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("broccoli-dev-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugins/demo")).unwrap();
        dir
    }

    #[test]
    fn a_plugin_inside_a_pinned_project_inherits_its_toolchain() {
        let root = scratch_dir("pinned");
        std::fs::write(root.join("rust-toolchain.toml"), "[toolchain]\n").unwrap();
        assert!(inherits_toolchain(&root.join("plugins/demo")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_standalone_plugin_pins_its_own_toolchain() {
        let root = scratch_dir("standalone");
        assert!(!inherits_toolchain(&root.join("plugins/demo")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
