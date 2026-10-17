//! Maven tasks (spec, section 18): the lifecycle phases of each workspace folder with a
//! `pom.xml`, run with the project's wrapper (`mvnw`) when it has one, else `mvn`.

use std::path::Path;

use kumiko_plugin_sdk::protocol::methods::process::ProcessSpec;
use kumiko_plugin_sdk::protocol::methods::task::{TaskDefinition, TaskGroup};

/// Phases offered, in the order of the list.
const PHASES: &[(&str, TaskGroup)] = &[
    ("test", TaskGroup::Test),
    ("compile", TaskGroup::Build),
    ("package", TaskGroup::Build),
    ("verify", TaskGroup::Test),
    ("install", TaskGroup::Build),
    ("clean", TaskGroup::Build),
];

/// The Maven command for a project folder.
fn command(folder: &Path) -> String {
    let wrapper = if cfg!(windows) { "mvnw.cmd" } else { "mvnw" };
    let path = folder.join(wrapper);
    if path.is_file() {
        path.to_string_lossy().into_owned()
    } else if cfg!(windows) {
        "mvn.cmd".to_owned()
    } else {
        "mvn".to_owned()
    }
}

/// Tasks of the folders (`name`, path) that are Maven projects.
/// `jdk`: the project's JDK, given to Maven as `JAVA_HOME` (as IntelliJ runs Maven with the
/// project SDK, not with whatever the environment says).
pub fn tasks(folders: &[(String, std::path::PathBuf)], jdk: Option<&Path>) -> Vec<TaskDefinition> {
    let projects: Vec<&(String, std::path::PathBuf)> = folders
        .iter()
        .filter(|(_, path)| path.join("pom.xml").is_file())
        .collect();
    let several = projects.len() > 1;
    let mut tasks = Vec::new();
    for (name, path) in projects {
        for (phase, group) in PHASES {
            let label = if several {
                format!("maven: {phase} ({name})")
            } else {
                format!("maven: {phase}")
            };
            tasks.push(TaskDefinition {
                id: format!("{}:{phase}", path.display()),
                label,
                task_type: "maven".into(),
                group: *group,
                spec: Some(ProcessSpec {
                    command: command(path),
                    // Batch mode: no prompts, no colour codes in the output.
                    args: vec!["-B".into(), (*phase).into()],
                    cwd: Some(path.to_string_lossy().into_owned()),
                    env: jdk
                        .map(|home| {
                            [("JAVA_HOME".to_owned(), home.to_string_lossy().into_owned())].into()
                        })
                        .unwrap_or_default(),
                    env_mode: Default::default(),
                    pty: None,
                    kill_on_owner_exit: false,
                }),
                problem_matcher: None,
            });
        }
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_of_maven_folders_with_their_wrapper() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let lib = temp.path().join("lib");
        let docs = temp.path().join("docs");
        for dir in [&app, &lib, &docs] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(app.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(lib.join("pom.xml"), "<project/>").unwrap();
        let wrapper = if cfg!(windows) { "mvnw.cmd" } else { "mvnw" };
        std::fs::write(app.join(wrapper), "").unwrap();
        let folders = vec![
            ("app".to_owned(), app.clone()),
            ("lib".to_owned(), lib),
            ("docs".to_owned(), docs),
        ];
        let tasks = tasks(&folders, Some(Path::new("/jdk")));
        assert_eq!(tasks.len(), 2 * PHASES.len(), "docs is not a Maven project");
        assert_eq!(tasks[0].label, "maven: test (app)");
        assert_eq!(tasks[0].group, TaskGroup::Test);
        let spec = tasks[0].spec.as_ref().unwrap();
        assert!(spec.command.ends_with(wrapper), "{}", spec.command);
        assert_eq!(spec.args, ["-B", "test"]);
        assert_eq!(spec.env["JAVA_HOME"], Path::new("/jdk").to_string_lossy());
        let lib_spec = tasks[PHASES.len()].spec.as_ref().unwrap();
        assert!(lib_spec.command.starts_with("mvn"));
        // One project: plain labels.
        let single = super::tasks(&folders[..1], None);
        assert_eq!(single[0].label, "maven: test");
    }
}
