//! The Problems tool window as data: problems from every source the core knows (settings,
//! resources, plugins; language servers), grouped by where they are.

use kumiko_types::{Position, Uri};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    /// Where it is: a file name, a plugin id, a settings layer.
    pub source: String,
    pub message: String,
    /// A file to open for it, if there is one.
    pub location: Option<Uri>,
    /// Where in the file (language diagnostics).
    pub position: Option<Position>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub source: String,
    pub location: Option<Uri>,
    pub problems: Vec<Problem>,
}

impl Group {
    pub fn worst(&self) -> Severity {
        self.problems
            .iter()
            .map(|p| p.severity)
            .min()
            .unwrap_or(Severity::Info)
    }
}

/// Groups by source: the worst groups first, then by name; problems keep their order.
pub fn group(problems: Vec<Problem>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for problem in problems {
        match groups.iter_mut().find(|g| g.source == problem.source) {
            Some(group) => group.problems.push(problem),
            None => groups.push(Group {
                source: problem.source.clone(),
                location: problem.location.clone(),
                problems: vec![problem],
            }),
        }
    }
    for group in &mut groups {
        group.problems.sort_by_key(|p| p.severity);
    }
    groups.sort_by(|a, b| {
        a.worst()
            .cmp(&b.worst())
            .then_with(|| a.source.cmp(&b.source))
    });
    groups
}

/// Errors and warnings in all groups.
pub fn counts(groups: &[Group]) -> (usize, usize) {
    let problems = groups.iter().flat_map(|g| &g.problems);
    let count = |s| problems.clone().filter(|p| p.severity == s).count();
    (count(Severity::Error), count(Severity::Warning))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(severity: Severity, source: &str, message: &str) -> Problem {
        Problem {
            severity,
            source: source.into(),
            message: message.into(),
            location: None,
            position: None,
        }
    }

    #[test]
    fn groups_put_the_worst_first() {
        let groups = group(vec![
            problem(Severity::Warning, "settings.toml", "unknown key"),
            problem(Severity::Error, "demo.plugin", "crashed"),
            problem(Severity::Error, "settings.toml", "bad value"),
            problem(Severity::Warning, "a-theme", "bad colour"),
        ]);
        let sources: Vec<&str> = groups.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(sources, ["demo.plugin", "settings.toml", "a-theme"]);
        assert_eq!(groups[1].problems[0].message, "bad value");
        assert_eq!(counts(&groups), (2, 2));
    }
}
