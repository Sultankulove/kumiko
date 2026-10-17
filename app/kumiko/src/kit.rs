//! `kumiko kit list | apply <path|id> | diff <path|id> | export`: the kits of spec section 15
//! from the command line, on a core without a window.

use kumiko_core::CoreApi;

pub const USAGE: &str =
    "usage: kumiko kit list | apply <path|id> | diff <path|id> | export [id] [name]";

/// Runs a kit command; the text for stdout.
pub async fn run(core: &CoreApi, args: &[String]) -> Result<String, String> {
    let argument = |i: usize| args.get(i).map(String::as_str);
    match argument(0) {
        Some("list") => {
            let active = core.active_kit();
            let lines: Vec<String> = core
                .kits()
                .iter()
                .map(|k| {
                    format!(
                        "{}{}  {}  {}{}",
                        if active.as_deref() == Some(&k.id) {
                            "* "
                        } else {
                            "  "
                        },
                        k.id,
                        k.name,
                        k.description,
                        if k.builtin { "  (built in)" } else { "" }
                    )
                })
                .collect();
            Ok(lines.join("\n"))
        }
        Some(command @ ("apply" | "diff")) => {
            let spec = argument(1).ok_or(USAGE)?;
            let kit = core.load_kit(spec).map_err(|e| e.to_string())?;
            let plan = if command == "apply" {
                core.apply_kit(&kit).await.map_err(|e| e.to_string())?
            } else {
                core.plan_kit(&kit)
            };
            let mut lines = plan.lines();
            if lines.is_empty() {
                lines.push("nothing to change".to_owned());
            }
            if command == "apply" && !plan.missing.is_empty() {
                lines.push(format!(
                    "applied {}, but {} plugin(s) are missing",
                    kit.id,
                    plan.missing.len()
                ));
            }
            Ok(lines.join("\n"))
        }
        Some("export") => {
            let id = argument(1).unwrap_or("local.kit.snapshot");
            let name = argument(2).unwrap_or("Snapshot");
            Ok(core.export_kit(id, name))
        }
        _ => Err(USAGE.to_owned()),
    }
}
