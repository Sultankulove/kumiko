//! kumiko.java: the Java language plugin (L2p, spec section 18). It finds the JDKs, tells the
//! core how to start JDT LS (`lsp/resolveLaunch`), and passes JDT LS the project's runtimes.
//! The core starts the server and speaks LSP to it; highlighting needs no server (the grammar
//! package), so Java files work without JDT LS too.

mod jdk;
mod jdtls;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kumiko_plugin_sdk::process::{Context, Plugin};
use kumiko_plugin_sdk::protocol::methods::lsp::{
    LaunchSpec, MiddlewareResult, ResolveLaunchParams,
};
use kumiko_plugin_sdk::{ErrorCode, ResponseError};
use serde_json::{Value, json};

/// The plugin's settings (`java.*`), relative keys.
struct Settings {
    jdtls_path: Option<PathBuf>,
    home: Option<PathBuf>,
    jdks: Vec<PathBuf>,
    jdk: Option<PathBuf>,
}

impl Settings {
    fn read(config: &Value) -> Self {
        let path = |key: &str| {
            config
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        };
        Self {
            jdtls_path: path("jdtls.path"),
            home: path("home"),
            jdks: config
                .get("jdks")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            jdk: path("jdk"),
        }
    }
}

fn failed(message: impl Into<String>) -> ResponseError {
    ResponseError::new(ErrorCode::NotFound, message.into())
}

/// The JDK running JDT LS: `java.home`, else the newest detected one, at least 21.
fn server_jdk(settings: &Settings, jdks: &[jdk::Jdk]) -> Result<jdk::Jdk, ResponseError> {
    if let Some(home) = &settings.home {
        let jdk = jdk::read(home)
            .ok_or_else(|| failed(format!("java.home {} is not a JDK", home.display())))?;
        if jdk.major < jdtls::MIN_JAVA {
            return Err(failed(format!(
                "JDT LS needs Java {} or newer; java.home is Java {}",
                jdtls::MIN_JAVA,
                jdk.major
            )));
        }
        return Ok(jdk);
    }
    jdks.iter()
        .find(|j| j.major >= jdtls::MIN_JAVA)
        .cloned()
        .ok_or_else(|| {
            failed(format!(
                "no JDK {} or newer found to run JDT LS; install one or set java.home",
                jdtls::MIN_JAVA
            ))
        })
}

/// `java.jdtls.path`, else a download in the plugin's cache.
fn server_install(
    settings: &Settings,
    cache: Option<&Path>,
) -> Result<jdtls::Install, ResponseError> {
    if let Some(path) = &settings.jdtls_path {
        return jdtls::find(path).ok_or_else(|| {
            failed(format!(
                "java.jdtls.path {} is not a JDT LS folder (plugins/, config_*/)",
                path.display()
            ))
        });
    }
    cache
        .and_then(|cache| jdtls::cached(cache).into_iter().next())
        .ok_or_else(|| failed("JDT LS is not installed; set java.jdtls.path"))
}

/// Settings JDT LS reads at start: the JDKs of projects, Maven import.
fn initialization_options(jdks: &[jdk::Jdk], project: Option<&Path>) -> Value {
    let default = project
        .and_then(|p| jdks.iter().find(|j| j.home == p))
        .or_else(|| jdks.first());
    let runtimes: Vec<Value> = jdks
        .iter()
        .map(|j| {
            json!({
                "name": j.environment(),
                "path": j.home.to_string_lossy(),
                "default": Some(j) == default,
            })
        })
        .collect();
    json!({
        "settings": {
            "java": {
                "configuration": { "runtimes": runtimes, "updateBuildConfiguration": "automatic" },
                "import": {
                    "maven": { "enabled": true },
                    "gradle": { "enabled": false },
                    // .project, .classpath and .settings stay in JDT LS's data folder, not in
                    // the project (IntelliJ users do not expect them there).
                    "generatesMetadataFilesAtProjectRoot": false,
                },
                // JDK internals are not offered for completion and imports, as in IntelliJ.
                "completion": {
                    "filteredTypes": ["java.awt.*", "com.sun.*", "sun.*", "jdk.*", "org.graalvm.*", "io.micrometer.shaded.*"],
                },
                "autobuild": { "enabled": true },
            }
        },
        "extendedClientCapabilities": {
            "classFileContentsSupport": false,
            "progressReportProvider": false,
        },
    })
}

fn resolve(ctx: &Context, params: &ResolveLaunchParams) -> Result<LaunchSpec, ResponseError> {
    let init = ctx.params();
    let settings = Settings::read(&init.config);
    let jdks = jdk::detect(&jdk::Sources::from_env(settings.jdks.clone()));
    let java = server_jdk(&settings, &jdks)?;
    let storage = init.storage.as_ref();
    let install = server_install(&settings, storage.map(|s| Path::new(&s.cache)))?;
    let data = storage
        .map(|s| PathBuf::from(&s.data))
        .unwrap_or_else(std::env::temp_dir)
        .join("workspaces")
        .join(jdtls::workspace_key(params.workspace_folder.as_str()));
    std::fs::create_dir_all(&data)
        .map_err(|e| ResponseError::new(ErrorCode::InternalError, e.to_string()))?;
    Ok(LaunchSpec {
        command: java.java().to_string_lossy().into_owned(),
        args: jdtls::arguments(&install, &data),
        env: Default::default(),
        cwd: params
            .workspace_folder
            .to_file_path()
            .map(|p| p.to_string_lossy().into_owned()),
        initialization_options: Some(initialization_options(&jdks, settings.jdk.as_deref())),
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let plugin = Plugin::new()
        .language_server("jdtls", |ctx, params| async move { resolve(&ctx, &params) })
        // JDT LS reports its state with a notification of its own.
        .lsp_middleware("language/status", |ctx, message| async move {
            let kind = message.params.get("type").and_then(Value::as_str);
            let ready = matches!(kind, Some("Started" | "ServiceReady"));
            if let Err(error) = ctx.set_context("java.serverReady", json!(ready)).await {
                eprintln!("kumiko-java: {error}");
            }
            Ok(MiddlewareResult::Respond {
                result: Value::Null,
            })
        })
        // Client-side commands of JDT LS: the editor is the client, so they are ours.
        .lsp_middleware("workspace/executeCommand", |ctx, message| async move {
            let command = message.params.get("command").and_then(Value::as_str);
            if command != Some("java.apply.workspaceEdit") {
                return Ok(MiddlewareResult::Forward {
                    params: message.params,
                });
            }
            let edit = message.params["arguments"][0].clone();
            ctx.lsp_apply_edit(message.server_id, edit)
                .await
                .map_err(|e| ResponseError::new(ErrorCode::InternalError, e.to_string()))?;
            Ok(MiddlewareResult::Respond {
                result: Value::Null,
            })
        })
        .command("java.showJdks", |ctx, _args| async move {
            let settings = Settings::read(&ctx.params().config);
            let jdks = jdk::detect(&jdk::Sources::from_env(settings.jdks));
            Ok(Value::Array(
                jdks.iter()
                    .map(|j| json!({ "home": j.home.to_string_lossy(), "version": j.version }))
                    .collect(),
            ))
        });
    match plugin.run_stdio().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kumiko-java: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdk(home: &str, major: u32) -> jdk::Jdk {
        jdk::Jdk {
            home: PathBuf::from(home),
            version: format!("{major}.0.1"),
            major,
        }
    }

    #[test]
    fn the_server_needs_java_21() {
        let none = Settings::read(&json!({}));
        assert!(server_jdk(&none, &[jdk("/a", 17)]).is_err());
        assert_eq!(
            server_jdk(&none, &[jdk("/b", 21), jdk("/a", 17)])
                .unwrap()
                .major,
            21
        );
        let missing = server_install(&none, None).unwrap_err();
        assert!(
            missing.message.contains("java.jdtls.path"),
            "{}",
            missing.message
        );
    }

    #[test]
    fn runtimes_go_to_jdt_ls_with_the_project_default() {
        let jdks = [jdk("/j21", 21), jdk("/j17", 17)];
        let options = initialization_options(&jdks, Some(Path::new("/j17")));
        let runtimes = &options["settings"]["java"]["configuration"]["runtimes"];
        assert_eq!(runtimes[0]["name"], "JavaSE-21");
        assert_eq!(runtimes[0]["default"], false);
        assert_eq!(runtimes[1]["default"], true);
        let settings = Settings::read(&json!({"jdtls.path": " ", "jdks": ["/x"], "home": "/h"}));
        assert_eq!(settings.jdtls_path, None);
        assert_eq!(settings.jdks, [PathBuf::from("/x")]);
        assert_eq!(settings.home, Some(PathBuf::from("/h")));
    }
}
