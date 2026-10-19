//! Apply Kit… (spec, section 15): a picker of the kits; each row says what applying it
//! would install, enable and disable, so choosing one is the confirmation.

use gpui::Context;

use super::{FocusTarget, Workbench};
use crate::quick_open::{Action, Item, Mode, QuickOpen};

impl Workbench {
    pub(super) fn open_kits(&mut self) {
        self.kit_rows = self
            .core
            .kits()
            .into_iter()
            .filter_map(|info| {
                let kit = self.core.load_kit(&info.id).ok()?;
                let plan = self.core.plan_kit(&kit);
                let summary = if plan.lines().is_empty() {
                    self.t("kits.nothing")
                } else {
                    plan.lines().join("; ")
                };
                Some((info, summary))
            })
            .collect();
        self.open_menu = None;
        self.quick_open = Some(QuickOpen::picker(Mode::Kits));
        self.refresh_quick();
        self.focus_request = Some(FocusTarget::Quick);
    }

    pub(super) fn kit_items(&self, query: &str) -> Vec<Item> {
        let query = query.to_lowercase();
        let active = self.core.active_kit();
        self.kit_rows
            .iter()
            .enumerate()
            .filter(|(_, (info, _))| {
                info.name.to_lowercase().contains(&query) || info.id.to_lowercase().contains(&query)
            })
            .map(|(i, (info, summary))| Item {
                label: info.name.clone(),
                highlights: Vec::new(),
                detail: if active.as_deref() == Some(info.id.as_str()) {
                    format!("{} — {summary}", self.t("kits.active"))
                } else {
                    summary.clone()
                },
                keys: None,
                action: Action::ApplyKit(i),
            })
            .collect()
    }

    pub(super) fn apply_kit(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((info, _)) = self.kit_rows.get(index).cloned() else {
            return;
        };
        let core = self.core.clone();
        let task = self.runtime.spawn(async move {
            let kit = core.load_kit(&info.id).map_err(|e| e.to_string())?;
            core.apply_kit(&kit).await.map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |workbench, cx| {
                workbench.message = Some(match result {
                    Ok(plan) if plan.missing.is_empty() => workbench.t("kits.applied"),
                    Ok(plan) => workbench
                        .t("kits.appliedMissing")
                        .replace("{count}", &plan.missing.len().to_string()),
                    Err(error) => error,
                });
                workbench.refresh_plugin_views();
                cx.notify();
            });
        })
        .detach();
    }
}
