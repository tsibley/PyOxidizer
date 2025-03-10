// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use {
    crate::{
        environment::{default_target_triple, PYOXIDIZER_VERSION},
        project_building, projectmgmt,
    },
    anyhow::{anyhow, Context, Result},
    clap::{Arg, ArgMatches, Command},
    std::{
        collections::HashMap,
        path::{Path, PathBuf},
    },
};

const BUILD_ABOUT: &str = "\
Build a PyOxidizer project.

The PATH argument is a filesystem path to a directory containing an
existing PyOxidizer enabled project.

This command will invoke Rust's build system tool (Cargo) to build
the project.
";

const INIT_RUST_PROJECT_ABOUT: &str = "\
Create a new Rust project embedding Python.

The PATH argument is a filesystem path that should be created to hold the
new Rust project.

This command will call `cargo init PATH` and then install files and make
modifications required to embed a Python interpreter in that application.

The new project's binary will be configured to launch a Python REPL by
default.

Created projects inherit settings such as Python distribution URLs and
dependency crate versions and locations from the PyOxidizer executable
they were created with.

On success, instructions on potential next steps are printed.
";

const GENERATE_PYTHON_EMBEDDING_ARTIFACTS_ABOUT: &str = "\
Generate files useful for embedding Python in a [Rust] binary.

This low-level command can be used to write files that facilitate the
embedding of Python in a larger binary. It can be used to write:

* A custom libpython that can be linked into a binary.
* A configuration file for the PyO3 Rust crate telling it how to
  link against the aforementioned custom libpython.
* A Python packed resources file containing the entirety of the Python
  standard library.
* A Rust file defining a default `pyembed::OxidizedPythonInterpreterConfig`
  struct for configuring the embedded Python interpreter.
* tcl/tk support files (for tkinter module support).
* Microsoft Visual C++ Redistributable Runtime DLLs (Windows only).

This command essentially does what the `run-build-script` command does except
it doesn't require the presence of a PyOxidizer configuration file. Instead,
it uses an opinionated default configuration suitable for producing a set of
files suitable for common Python embedding scenarios. If the defaults are not
appropriate for your use case, you can always define a configuration file to
customize them and use `run-build-script` to produce similar output files.
";

const RUN_BUILD_SCRIPT_ABOUT: &str = "\
Runs a crate build script to generate Python artifacts.

When the Rust crate embedding Python is built, it needs to consume various
artifacts derived from processing the active PyOxidizer config file.
These files are typically generated when the crate's build script runs.

This command executes the functionality to derive various artifacts and
emits special lines that tell the Rust build system how to consume them.
";

const RESOURCES_SCAN_ABOUT: &str = "\
Scan a directory or file for Python resources.

This command invokes the logic used by various PyOxidizer functionality
walking a directory tree or parsing a file and categorizing seen files.

The directory walking functionality is used by
`oxidized_importer.find_resources_in_path()` and Starlark methods like
`PythonExecutable.pip_install()` and
`PythonExecutable.read_package_root()`.

The file parsing logic is used for parsing the contents of wheels.

This command can be used to debug failures with PyOxidizer's code
for converting files/directories into strongly typed objects. This
conversion is critical for properly packaging Python applications and
bugs can result in incorrect install layouts, missing resources, etc.
";

const VAR_HELP: &str = "\
Defines a single string key to set in the VARS global dict.

This argument can be used to inject variable content into the Starlark
execution context to influence evaluation.

<name> defines the key in the dict to set and <value> is its string
value.

For example, `--var my_var my_value` is functionally similar to the
Starlark expression `VARS[\"my_var\"] = \"my_value\"`.

If a Starlark variable is defined multiple times, an error occurs.
";

const ENV_VAR_HELP: &str = "\
Defines a single string key to set in the VARS global dict from an environment variable.

This is like --var except the value of the dict key comes from an
environment variable.

The <env> environment variable is read and becomes the value of the
<name> key in the VARS dict.

If the <env> environment variable is not set, the Starlark value will
be `None` instead of a `string`.

If a Starlark variable is defined multiple times, an error occurs.
";

fn add_env_args(app: Command) -> Command {
    app.arg(
        Arg::new("vars")
            .long("var")
            .value_names(&["name", "value"])
            .multiple_occurrences(true)
            .multiple_values(true)
            .help("Define a variable in Starlark environment")
            .long_help(VAR_HELP),
    )
    .arg(
        Arg::new("vars_env")
            .long("var-env")
            .value_names(&["name", "env"])
            .multiple_occurrences(true)
            .multiple_values(true)
            .help("Define an environment variable in Starlark environment")
            .long_help(ENV_VAR_HELP),
    )
}

fn add_python_distribution_args(app: Command) -> Command {
    app.arg(
        Arg::new("target_triple")
            .long("--target-triple")
            .help("Rust target triple being targeted")
            .takes_value(true)
            .default_value(default_target_triple()),
    )
    .arg(
        Arg::new("flavor")
            .long("--flavor")
            .help("Python distribution flavor")
            .takes_value(true)
            .default_value("standalone"),
    )
    .arg(
        Arg::new("python_version")
            .long("--python-version")
            .help("Python version (X.Y) to use")
            .takes_value(true),
    )
}

fn starlark_vars(args: &ArgMatches) -> Result<HashMap<String, Option<String>>> {
    let mut res = HashMap::new();

    if let Some(mut vars) = args.values_of("vars") {
        while let (Some(name), Some(value)) = (vars.next(), vars.next()) {
            if res.contains_key(name) {
                return Err(anyhow!("Starlark variable {} already defined", name));
            }

            res.insert(name.to_string(), Some(value.to_string()));
        }
    }

    if let Some(mut vars) = args.values_of("vars_env") {
        while let (Some(name), Some(env)) = (vars.next(), vars.next()) {
            if res.contains_key(name) {
                return Err(anyhow!("Starlark variable {} already defined", name));
            }

            res.insert(name.to_string(), std::env::var(env).ok());
        }
    }

    Ok(res)
}

pub fn run_cli() -> Result<()> {
    let mut env = crate::environment::Environment::new()?;

    let version = env.pyoxidizer_source.version_long();

    let app = Command::new("PyOxidizer")
        .version(PYOXIDIZER_VERSION)
        .long_version(version.as_str())
        .author("Gregory Szorc <gregory.szorc@gmail.com>")
        .long_about("Build and distribute Python applications")
        .arg_required_else_help(true)
        .arg(
            Arg::new("system_rust")
                .long("--system-rust")
                .global(true)
                .help("Use a system install of Rust instead of a self-managed Rust installation"),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .global(true)
                .multiple_occurrences(true)
                .help("Increase logging verbosity. Can be specified multiple times"),
        );

    let app = app.subcommand(
        Command::new("analyze").about("Analyze a built binary").arg(
            Arg::new("path")
                .required(true)
                .help("Path to executable to analyze"),
        ),
    );

    let app = app.subcommand(add_env_args(
        Command::new("build")
            .about("Build a PyOxidizer enabled project")
            .long_about(BUILD_ABOUT)
            .arg(
                Arg::new("target_triple")
                    .long("target-triple")
                    .takes_value(true)
                    .help("Rust target triple to build for"),
            )
            .arg(
                Arg::new("release")
                    .long("release")
                    .help("Build a release binary"),
            )
            .arg(
                Arg::new("path")
                    .long("path")
                    .takes_value(true)
                    .default_value(".")
                    .value_name("PATH")
                    .help("Directory containing project to build"),
            )
            .arg(
                Arg::new("targets")
                    .value_name("TARGET")
                    .multiple_occurrences(true)
                    .multiple_values(true)
                    .help("Target to resolve"),
            ),
    ));

    let app =
        app.subcommand(Command::new("cache-clear").about("Clear PyOxidizer's user-specific cache"));

    let app = app.subcommand(
        Command::new("find-resources")
            .about("Find resources in a file or directory")
            .long_about(RESOURCES_SCAN_ABOUT)
            .arg(
                Arg::new("distributions_dir")
                    .long("distributions-dir")
                    .takes_value(true)
                    .value_name("PATH")
                    .help("Directory to extract downloaded Python distributions into"),
            )
            .arg(
                Arg::new("scan_distribution")
                    .long("--scan-distribution")
                    .help("Scan the Python distribution instead of a path"),
            )
            .arg(
                Arg::new("target_triple")
                    .long("target-triple")
                    .takes_value(true)
                    .default_value(default_target_triple())
                    .help("Target triple of Python distribution to use"),
            )
            .arg(
                Arg::new("no_classify_files")
                    .long("no-classify-files")
                    .help("Whether to skip classifying files as typed resources"),
            )
            .arg(
                Arg::new("no_emit_files")
                    .long("no-emit-files")
                    .help("Whether to skip emitting File resources"),
            )
            .arg(Arg::new("path").value_name("PATH").required(true).help(
                "Filesystem path to scan for resources. Must be a directory or Python wheel",
            )),
    );

    let app = app.subcommand(add_python_distribution_args(
        Command::new("generate-python-embedding-artifacts")
            .about("Generate files useful for embedding Python in a [Rust] binary")
            .long_about(GENERATE_PYTHON_EMBEDDING_ARTIFACTS_ABOUT)
            .arg(
                Arg::new("dest_path")
                    .value_name("DESTINATION_PATH")
                    .required(true)
                    .help("Output directory for written files"),
            ),
    ));

    let app = app.subcommand(
        Command::new("init-config-file")
            .about("Create a new PyOxidizer configuration file.")
            .arg(
                Arg::new("python-code")
                    .long("python-code")
                    .takes_value(true)
                    .help("Default Python code to execute in built executable"),
            )
            .arg(
                Arg::new("pip-install")
                    .long("pip-install")
                    .takes_value(true)
                    .multiple_occurrences(true)
                    .multiple_values(true)
                    .number_of_values(1)
                    .help("Python package to install via `pip install`"),
            )
            .arg(
                Arg::new("path")
                    .required(true)
                    .value_name("PATH")
                    .help("Directory where configuration file should be created"),
            ),
    );

    let app = app.subcommand(
        Command::new("init-rust-project")
            .about("Create a new Rust project embedding a Python interpreter")
            .long_about(INIT_RUST_PROJECT_ABOUT)
            .arg(
                Arg::new("path")
                    .required(true)
                    .value_name("PATH")
                    .help("Path of project directory to create"),
            ),
    );

    let app = app.subcommand(
        Command::new("list-targets")
            .about("List targets available to resolve in a configuration file")
            .arg(
                Arg::new("path")
                    .default_value(".")
                    .value_name("PATH")
                    .help("Path to project to evaluate"),
            ),
    );

    let app = app.subcommand(
        Command::new("python-distribution-extract")
            .about("Extract a Python distribution archive to a directory")
            .arg(
                Arg::new("download-default")
                    .long("--download-default")
                    .help("Download and extract the default distribution for this platform"),
            )
            .arg(
                Arg::new("archive-path")
                    .long("--archive-path")
                    .value_name("DISTRIBUTION_PATH")
                    .help("Path to a Python distribution archive"),
            )
            .arg(
                Arg::new("dest_path")
                    .required(true)
                    .value_name("DESTINATION_PATH")
                    .help("Path to directory where distribution should be extracted"),
            ),
    );

    let app = app.subcommand(
        Command::new("python-distribution-info")
            .about("Show information about a Python distribution archive")
            .arg(
                Arg::new("path")
                    .required(true)
                    .value_name("PATH")
                    .help("Path to Python distribution archive to analyze"),
            ),
    );

    let app = app.subcommand(
        Command::new("python-distribution-licenses")
            .about("Show licenses for a given Python distribution")
            .arg(
                Arg::new("path")
                    .required(true)
                    .value_name("PATH")
                    .help("Path to Python distribution to analyze"),
            ),
    );

    let app = app.subcommand(add_env_args(
        Command::new("run-build-script")
            .about("Run functionality that a build script would perform")
            .long_about(RUN_BUILD_SCRIPT_ABOUT)
            .arg(
                Arg::new("build-script-name")
                    .required(true)
                    .help("Value to use for Rust build script"),
            )
            .arg(
                Arg::new("target")
                    .long("target")
                    .takes_value(true)
                    .help("The config file target to resolve"),
            ),
    ));

    let app = app.subcommand(add_env_args(
        Command::new("run")
            .about("Run a target in a PyOxidizer configuration file")
            .trailing_var_arg(true)
            .arg(
                Arg::new("target_triple")
                    .long("target-triple")
                    .takes_value(true)
                    .help("Rust target triple to build for"),
            )
            .arg(
                Arg::new("release")
                    .long("release")
                    .help("Run a release binary"),
            )
            .arg(
                Arg::new("path")
                    .long("path")
                    .default_value(".")
                    .value_name("PATH")
                    .help("Directory containing project to build"),
            )
            .arg(
                Arg::new("target")
                    .long("target")
                    .takes_value(true)
                    .help("Build target to run"),
            )
            .arg(
                Arg::new("extra")
                    .multiple_occurrences(true)
                    .multiple_values(true),
            ),
    ));

    let app = app.subcommand(
        Command::new("rust-project-licensing")
            .about("Show licensing information for a Rust project")
            .arg(
                Arg::new("all_features")
                    .long("all-features")
                    .help("Activate all crate features during evaluation"),
            )
            .arg(
                Arg::new("target_triple")
                    .long("target-triple")
                    .takes_value(true)
                    .help("Rust target triple to simulate building for"),
            )
            .arg(
                Arg::new("unified_license")
                    .long("unified-license")
                    .help("Print a unified license document"),
            )
            .arg(
                Arg::new("project_path")
                    .takes_value(true)
                    .required(true)
                    .help("The path to the Rust project to evaluate"),
            ),
    );

    let matches = app.get_matches();

    let verbose = matches.is_present("verbose");

    let log_level = match matches.occurrences_of("verbose") {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        2 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };

    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(log_level.as_str()),
    );

    builder
        .format_timestamp(None)
        .format_level(false)
        .format_target(false);

    builder.init();

    if matches.is_present("system_rust") {
        env.unmanage_rust().context("unmanaging Rust")?;
    }

    let (command, args) = matches
        .subcommand()
        .ok_or_else(|| anyhow!("invalid sub-command"))?;

    match command {
        "analyze" => {
            let path = args.value_of("path").unwrap();
            let path = PathBuf::from(path);
            tugger_binary_analysis::analyze_file(path);

            Ok(())
        }

        "build" => {
            let starlark_vars = starlark_vars(args)?;
            let release = args.is_present("release");
            let target_triple = args.value_of("target_triple");
            let path = args.value_of("path").unwrap();
            let resolve_targets = args
                .values_of("targets")
                .map(|values| values.map(|x| x.to_string()).collect());

            projectmgmt::build(
                &env,
                Path::new(path),
                target_triple,
                resolve_targets,
                starlark_vars,
                release,
                verbose,
            )
        }

        "cache-clear" => projectmgmt::cache_clear(&env),

        "find-resources" => {
            let path = args.value_of("path").map(Path::new);
            let distributions_dir = args.value_of("distributions_dir").map(Path::new);
            let scan_distribution = args.is_present("scan_distribution");
            let target_triple = args.value_of("target_triple").unwrap();
            let classify_files = !args.is_present("no_classify_files");
            let emit_files = !args.is_present("no_emit_files");

            if path.is_none() && !scan_distribution {
                Err(anyhow!("must specify a path or --scan-distribution"))
            } else {
                projectmgmt::find_resources(
                    &env,
                    path,
                    distributions_dir,
                    scan_distribution,
                    target_triple,
                    classify_files,
                    emit_files,
                )
            }
        }

        "generate-python-embedding-artifacts" => {
            let target_triple = args
                .value_of("target_triple")
                .expect("target_triple should have default");
            let flavor = args.value_of("flavor").expect("flavor should have default");
            let python_version = args.value_of("python_version");
            let dest_path = Path::new(
                args.value_of("dest_path")
                    .expect("dest_path should be required"),
            );

            projectmgmt::generate_python_embedding_artifacts(
                &env,
                target_triple,
                flavor,
                python_version,
                dest_path,
            )
        }

        "init-config-file" => {
            let code = args.value_of("python-code");
            let pip_install = if args.is_present("pip-install") {
                args.values_of("pip-install").unwrap().collect()
            } else {
                Vec::new()
            };
            let path = args.value_of("path").unwrap();
            let config_path = Path::new(path);

            projectmgmt::init_config_file(&env.pyoxidizer_source, config_path, code, &pip_install)
        }

        "list-targets" => {
            let path = args.value_of("path").unwrap();

            projectmgmt::list_targets(&env, Path::new(path))
        }

        "init-rust-project" => {
            let path = args.value_of("path").unwrap();
            let project_path = Path::new(path);

            projectmgmt::init_rust_project(&env, project_path)
        }

        "python-distribution-extract" => {
            let download_default = args.is_present("download-default");
            let archive_path = args.value_of("archive-path");
            let dest_path = args.value_of("dest_path").unwrap();

            if !download_default && archive_path.is_none() {
                Err(anyhow!("must specify --download-default or --archive-path"))
            } else if download_default && archive_path.is_some() {
                Err(anyhow!(
                    "must only specify one of --download-default or --archive-path"
                ))
            } else {
                projectmgmt::python_distribution_extract(download_default, archive_path, dest_path)
            }
        }

        "python-distribution-info" => {
            let dist_path = args.value_of("path").unwrap();

            projectmgmt::python_distribution_info(&env, dist_path)
        }

        "python-distribution-licenses" => {
            let path = args.value_of("path").unwrap();

            projectmgmt::python_distribution_licenses(&env, path)
        }

        "run-build-script" => {
            let starlark_vars = starlark_vars(args)?;
            let build_script = args.value_of("build-script-name").unwrap();
            let target = args.value_of("target");

            project_building::run_from_build(&env, build_script, target, starlark_vars)
        }

        "run" => {
            let starlark_vars = starlark_vars(args)?;
            let target_triple = args.value_of("target_triple");
            let release = args.is_present("release");
            let path = args.value_of("path").unwrap();
            let target = args.value_of("target");
            let extra: Vec<&str> = args.values_of("extra").unwrap_or_default().collect();

            projectmgmt::run(
                &env,
                Path::new(path),
                target_triple,
                release,
                target,
                starlark_vars,
                &extra,
                verbose,
            )
        }

        "rust-project-licensing" => {
            let project_path =
                Path::new(args.value_of("project_path").expect("argument is required"));
            let all_features = args.is_present("all_features");
            let target_triple = args.value_of("target_triple");
            let unified_license = args.is_present("unified_license");

            projectmgmt::rust_project_licensing(
                &env,
                project_path,
                all_features,
                target_triple,
                unified_license,
            )
        }

        _ => Err(anyhow!("invalid sub-command")),
    }
}
