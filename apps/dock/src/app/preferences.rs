//! Persisted dock preferences, pinned apps and menu-bar coordination.

use std::{
    env,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use muelle_config::{Config, DockGroup};
use muelle_core::{
    frame::DockEdge,
    roster::{Group, Roster},
};
use muelle_shell::{
    pins::{app_exists, taskbar_pins, taskbar_pins_if_readable},
    system::primary_monitor,
    tracker::{self, normalize_id},
};

use crate::{
    assets::fallback_name,
    menubar::{MenuBar, MenuBarWidgets},
    strings,
};

use super::{
    App, Role,
    visibility::{arm_fullscreen_recheck, current_menu_bar_cover},
};

pub(super) fn explorer_id() -> String {
    normalize_id(&format!(
        "{}\\explorer.exe",
        env::var("WINDIR").unwrap_or_default()
    ))
}

/// Saved settings or, on first launch, the defaults with the apps pinned to
/// the Windows taskbar.
pub(super) fn load_config() -> Config {
    let path = muelle_config::default_path();
    match muelle_config::load(&path) {
        Ok(Some(config)) => {
            warn_about_weather(&config);
            config
        }
        Ok(None) => {
            let pins = taskbar_pins();
            let config = Config {
                pinned: pins.clone(),
                taskbar_pins: Some(pins),
                ..Config::default()
            };
            match muelle_config::update(&path, |saved| Ok(saved.unwrap_or_else(|| config.clone())))
            {
                Ok(saved) => saved,
                Err(error) => {
                    eprintln!("muelle: {error}");
                    config
                }
            }
        }
        Err(error) => {
            // Do not overwrite it; the user may want to repair the file.
            eprintln!("muelle: {error}; se usan los valores por defecto");
            Config::default()
        }
    }
}

/// A hand-edited file can hold coordinates no forecast exists for. The
/// weather then stays empty; say why once, without the values themselves.
fn warn_about_weather(config: &Config) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if let Some(warning) = weather_warning(config)
        && !WARNED.swap(true, Ordering::Relaxed)
    {
        muelle_shell::log::line(warning);
    }
}

fn weather_warning(config: &Config) -> Option<&'static str> {
    (config.weather.enabled && !config.weather.has_valid_coordinates())
        .then_some("tiempo: coordenadas fuera de rango en la configuración; no se consulta")
}

/// Apply only Explorer's built-in pin, preserving the order of all other apps.
pub(super) fn apply_explorer_pin(roster: &mut Roster, pinned: bool) {
    let explorer = explorer_id();
    if pinned {
        let before = roster.pinned().iter().find(|id| **id != explorer).cloned();
        roster.pin_before(&explorer, before.as_deref());
    } else {
        roster.unpin(&explorer);
        roster.forget_recent(&explorer);
    }
}

pub(super) fn roster_from(config: &Config) -> Roster {
    let explorer = explorer_id();
    let mut pinned = config.pinned.clone();
    pinned.retain(|id| *id != explorer);
    let groups = config
        .groups
        .iter()
        .map(|group| Group {
            id: group.id.clone(),
            apps: group
                .apps
                .iter()
                .filter(|id| **id != explorer)
                .cloned()
                .collect(),
        })
        .collect();
    let mut roster = Roster::new(pinned, config.recent.clone()).with_groups(groups);
    apply_explorer_pin(&mut roster, config.pin_explorer);
    roster
}

pub(super) fn save_settings(roster: &Roster, config: &Config) {
    if let Err(error) = save_dock_config(&muelle_config::default_path(), roster, config, false) {
        eprintln!("muelle: {error}");
    }
}

pub(super) fn save_dock_config(
    path: &Path,
    roster: &Roster,
    config: &Config,
    toggle_hide: bool,
) -> Result<Config> {
    let saved = muelle_config::update(path, |saved| {
        let mut saved = saved.unwrap_or_else(|| config.clone());
        saved.pinned = roster.pinned().to_vec();
        saved.groups = roster
            .groups()
            .iter()
            .map(|group| DockGroup {
                id: group.id.clone(),
                apps: group.apps.clone(),
            })
            .collect();
        saved.recent = roster.recent().to_vec();
        saved.taskbar_pins.clone_from(&config.taskbar_pins);
        if toggle_hide {
            saved.auto_hide = !saved.hides_automatically();
            saved.smart_hide = false;
        }
        Ok(saved)
    })?;
    Ok(saved)
}

/// Takes the shortcuts folder out of the dock. Only that preference changes
/// on disk: Settings owns the others and may have changed them since.
pub(super) fn forget_shortcuts_folder(config: &Config) -> Result<()> {
    muelle_config::update(&muelle_config::default_path(), |saved| {
        let mut saved = saved.unwrap_or_else(|| config.clone());
        saved.shortcuts_folder = None;
        Ok(saved)
    })?;
    Ok(())
}

pub(super) fn fixed_item_preference(role: Role) -> Option<&'static str> {
    match role {
        Role::Start => Some("showStart"),
        Role::Folder => Some("showDownloads"),
        Role::Trash => Some("showTrash"),
        _ => None,
    }
}

/// Hides only the shortcut. No folder, file or Windows pin is removed.
pub(super) fn hide_fixed_item(path: &std::path::Path, role: Role) -> Result<Config> {
    let key = fixed_item_preference(role).context("el elemento no es un acceso fijo")?;
    let patch = serde_json::Map::from_iter([(key.to_owned(), serde_json::Value::Bool(false))]);
    Ok(muelle_config::update_preferences(path, patch)?)
}

impl App {
    /// The settings app saved a change: read it and apply what differs.
    pub(super) fn reload_config(&mut self) -> Result<()> {
        self.close_preview();
        self.close_group_view();
        let Ok(Some(saved)) = muelle_config::load(&muelle_config::default_path()) else {
            return Ok(());
        };
        warn_about_weather(&saved);
        // Pinned and recent apps belong to the dock; the settings app only
        // changes the rest.
        let config = Config {
            pinned: self.config.pinned.clone(),
            groups: self.config.groups.clone(),
            recent: self.config.recent.clone(),
            taskbar_pins: self.config.taskbar_pins.clone(),
            ..saved
        };
        if config == self.config {
            return Ok(());
        }
        self.stop_mascot();
        if config.language != self.config.language {
            strings::set_language(config.language);
            // Labels are drawn text: redraw them in the new language.
            self.assets_for = (0.0, false, 0, DockEdge::Bottom);
        }
        // The dock and the menu bar key their looks on it: `refresh` redraws both.
        crate::appearance::set(config.appearance);
        crate::appearance::set_menu_bar(config.menu_bar_appearance);
        if !config.genie {
            self.restore_transitions();
            self.snapshots.clear();
        }
        let hiding_changed = config.auto_hide != self.config.auto_hide
            || config.smart_hide != self.config.smart_hide;
        let workspaces_changed = config.show_other_workspaces != self.config.show_other_workspaces;
        let moved = config.dock_position != self.config.dock_position;
        apply_explorer_pin(&mut self.roster, config.pin_explorer);
        self.slide.omega = config.dock_auto_hide_speed.omega();
        self.config = config;
        if moved {
            // A hidden dock comes out of its new edge on every display.
            self.apply_edge_settings();
        }
        // Updates may have been turned on or off, and the break reminder.
        self.schedule_update();
        self.sync_break_timer();
        self.sync_taskband();
        if workspaces_changed {
            self.sync()?;
        }
        self.sync_thumbnails();
        self.apply_menu_bar_setting();
        self.update_menu_bar_app()?;
        self.calm_windows();
        self.refresh()?;
        if hiding_changed {
            // refresh has recomputed overlap for the new policy and geometry.
            self.hide_if_unused()?;
        }
        self.sync_badge_visuals()?;
        self.render()
    }

    /// Adds the apps pinned to the Windows taskbar that the dock does not
    /// have yet, in the taskbar's order, after the dock's own pins. Nothing is
    /// removed or reordered: the dock imports them by itself only the first
    /// time it runs, and pins the user removed since then stay removed until
    /// they ask for this.
    /// Brings pins and unpins made on the Windows taskbar to the dock, as if
    /// it were the taskbar: what was pinned there is pinned here, what was
    /// unpinned there is unpinned here. Apps pinned only in the dock stay.
    /// Windows offers no way to do the reverse (pin to the taskbar).
    pub(super) fn sync_taskbar_pins(&mut self) -> Result<()> {
        let pruned = self.prune_missing_apps();
        let Some(current) = taskbar_pins_if_readable() else {
            if pruned {
                save_settings(&self.roster, &self.config);
                return self.refresh();
            }
            return Ok(());
        };
        let Some(previous) = self.config.taskbar_pins.replace(current.clone()) else {
            // First look since this feature: keep the dock as the user left it,
            // but add the Store apps pinned in Windows. Versions before 0.1.7
            // could not import them, so their absence was never a choice; on
            // Windows 10 most default pins (Store, Mail) are Store apps.
            for id in current
                .iter()
                .filter(|id| id.starts_with("shell:AppsFolder\\"))
            {
                self.roster.pin(id);
            }
            save_settings(&self.roster, &self.config);
            return self.refresh();
        };
        if previous == current {
            if pruned {
                save_settings(&self.roster, &self.config);
                return self.refresh();
            }
            return Ok(());
        }
        for id in current.iter().filter(|id| !previous.contains(id)) {
            self.roster.pin(id);
        }
        for id in previous.iter().filter(|id| !current.contains(id)) {
            self.roster.unpin(id);
        }
        apply_explorer_pin(&mut self.roster, self.config.pin_explorer);
        save_settings(&self.roster, &self.config);
        self.refresh()
    }

    /// Removes apps that were uninstalled, so no broken icon stays behind, as
    /// the Windows taskbar drops them: pinned apps (in groups too) and recent
    /// apps (issue #25). Returns whether anything changed.
    pub(super) fn prune_missing_apps(&mut self) -> bool {
        let missing: Vec<String> = self
            .roster
            .pinned_apps()
            .into_iter()
            .filter(|id| !app_exists(id))
            .collect();
        for id in &missing {
            self.roster.unpin(id);
        }
        let gone: Vec<String> = self
            .roster
            .recent()
            .iter()
            .filter(|id| !app_exists(id))
            .cloned()
            .collect();
        for id in &gone {
            self.roster.forget_recent(id);
        }
        !missing.is_empty() || !gone.is_empty()
    }

    pub(super) fn toggle_hiding(&mut self) -> Result<()> {
        let saved = save_dock_config(
            &muelle_config::default_path(),
            &self.roster,
            &self.config,
            true,
        )?;
        self.config.auto_hide = saved.auto_hide;
        self.config.smart_hide = saved.smart_hide;
        self.apply_edge_settings();
        self.refresh()?;
        self.hide_if_unused()?;
        self.update_fullscreen()
    }

    /// Creates or removes the menu bar to match the settings.
    pub(super) fn apply_menu_bar_setting(&mut self) {
        let weather_changed = self.weather.borrow_mut().configure(&self.config.weather);
        let widgets = MenuBarWidgets {
            island_style: self.config.menu_bar_island_style,
            notifications: self.config.menu_bar_notifications,
            system_monitor: self.config.menu_bar_system_monitor,
            essentials: self.config.menu_bar_essentials,
            bare: self.config.menu_bar_bare,
            ai_alerts: self.config.menu_bar_ai_alerts,
        };
        if !self.config.menu_bar {
            self.menubar = None;
        } else if self.menubar.is_none() {
            self.menubar = MenuBar::create(
                &self.gfx,
                self.config.ai_usage,
                self.config.menu_bar_music,
                // Hidden from the first frame, or the whole ghost bar flashes.
                self.config.menu_bar_auto_hide || self.config.menu_bar_ghost,
                widgets,
            )
            .inspect_err(|error| eprintln!("muelle: barra superior: {error:#}"))
            .ok();
            if let Some(bar) = &mut self.menubar {
                bar.attach_weather(self.weather.clone());
            }
        }
        if let Some(bar) = &mut self.menubar {
            bar.set_clock_target(self.config.menu_bar_clock_target.as_deref());
            let fullscreen = primary_monitor()
                .is_some_and(|monitor| tracker::screen_is_covered_by_fullscreen(monitor.screen));
            let covered = current_menu_bar_cover(
                &self.config,
                &self.windows,
                tracker::foreground_window(),
                fullscreen,
                bar.bounds(),
            );
            // All three at once: one at a time, a switch between two modes
            // would pass through "always visible" and resize maximized windows.
            let applied = bar
                .set_hiding(
                    self.config.menu_bar_ghost,
                    self.config.menu_bar_auto_hide,
                    self.config.menu_bar_smart_hide,
                    covered,
                )
                .and_then(|()| bar.set_usage_enabled(&self.gfx, self.config.ai_usage))
                .and_then(|()| bar.set_media_enabled(&self.gfx, self.config.menu_bar_music))
                .and_then(|()| bar.set_widgets(&self.gfx, widgets))
                .and_then(|()| {
                    if fullscreen {
                        arm_fullscreen_recheck(self.visual);
                    }
                    bar.set_visible(!fullscreen)
                })
                .and_then(|()| {
                    bar.set_accessories_enabled(&self.gfx, self.config.accessory_battery)
                })
                .and_then(|()| {
                    if weather_changed {
                        bar.weather_changed(&self.gfx)?;
                    }
                    Ok(())
                });
            if let Err(error) = applied {
                eprintln!("muelle: barra superior: {error:#}");
            }
        }
        self.apply_edge_settings();
    }

    /// Shows the name of the app in front, as the macOS menu bar does.
    pub(super) fn update_menu_bar_app(&mut self) -> Result<()> {
        if self.menubar.is_none() {
            return Ok(());
        }
        let front = tracker::foreground_window();
        let id = self
            .windows
            .iter()
            .find(|window| window.hwnd == front)
            .map_or_else(explorer_id, |window| window.app_id.clone());
        if !self.names.contains_key(&id) {
            let name = self
                .icons
                .get(&id, "")
                .map_or_else(|| fallback_name(&id), |visual| visual.name.clone());
            self.names.insert(id.clone(), name);
        }
        let name = self.names.get(&id).map_or("", String::as_str);
        match &mut self.menubar {
            Some(bar) => bar.set_app_name(&self.gfx, name),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod dock_preferences_tests {
    use super::super::visibility::rects_overlap;
    use super::*;
    use muelle_config::DockDensity;
    use muelle_core::{
        layout::{ItemKind, Metrics, layout},
        spring::Spring,
    };
    use windows::Win32::Foundation::RECT;

    #[test]
    fn weather_coordinates_out_of_range_leave_a_warning() {
        let with = |enabled, latitude| Config {
            weather: muelle_config::Weather {
                enabled,
                latitude,
                ..muelle_config::Weather::default()
            },
            ..Config::default()
        };
        assert_eq!(weather_warning(&Config::default()), None);
        assert!(weather_warning(&with(true, 120.0)).is_some());
        // Turned off, nothing is asked, so there is nothing to explain.
        assert_eq!(weather_warning(&with(false, 120.0)), None);
    }

    #[test]
    fn dock_menu_toggles_the_selected_mode_without_creating_combined_policies() -> Result<()> {
        for (auto_hide, smart_hide) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let path = std::env::temp_dir().join(format!(
                "muelle-hide-menu-{}-{auto_hide}-{smart_hide}.json",
                std::process::id()
            ));
            let config = Config {
                auto_hide,
                smart_hide,
                icon_size: 64,
                ..Config::default()
            };
            muelle_config::save(&path, &config)?;
            let snapshot = Config::default(); // The latest saved choice wins over a stale view.
            let saved = save_dock_config(&path, &Roster::default(), &snapshot, true)?;
            assert_eq!(
                (saved.auto_hide, saved.smart_hide),
                (!(auto_hide || smart_hide), false)
            );
            assert_eq!(saved.icon_size, 64);
            let untouched = save_dock_config(&path, &Roster::default(), &snapshot, false)?;
            assert_eq!(
                (untouched.auto_hide, untouched.smart_hide),
                (saved.auto_hide, saved.smart_hide)
            );
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    #[test]
    fn intelligent_hiding_requires_real_overlap_on_every_dock_edge() {
        let screen = RECT {
            left: -1600,
            top: -200,
            right: 0,
            bottom: 800,
        };
        for dock in [
            RECT {
                left: -1200,
                top: 710,
                right: -400,
                bottom: 790,
            },
            RECT {
                left: -1590,
                top: 0,
                right: -1510,
                bottom: 600,
            },
            RECT {
                left: -90,
                top: 0,
                right: -10,
                bottom: 600,
            },
        ] {
            assert!(rects_overlap(dock, screen));
            assert!(!rects_overlap(
                dock,
                RECT {
                    left: 10,
                    top: 0,
                    right: 810,
                    bottom: 700
                }
            ));
            assert!(!rects_overlap(
                dock,
                RECT {
                    left: dock.right,
                    top: dock.top,
                    right: dock.right + 10,
                    bottom: dock.bottom
                }
            ));
            assert!(!rects_overlap(
                dock,
                RECT {
                    left: dock.left + 1,
                    right: dock.left + 1,
                    top: dock.top,
                    bottom: dock.bottom
                }
            ));
        }
    }

    #[test]
    fn removing_fixed_shortcuts_preserves_folders_pins_and_other_preferences() -> Result<()> {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
                let _ = std::fs::remove_file(self.0.with_extension("json.lock"));
            }
        }
        let file = Fixture(env::temp_dir().join(format!(
            "muelle-fixed-shortcuts-{}.json",
            std::process::id()
        )));
        let initial = Config {
            pinned: vec!["editor".into()],
            shortcuts_folder: Some("C:\\Projects".into()),
            mascot: false,
            ..Config::default()
        };
        muelle_config::save(&file.0, &initial)?;
        let downloads = hide_fixed_item(&file.0, Role::Folder)?;
        assert!(!downloads.show_downloads);
        assert!(downloads.show_trash && downloads.show_start);
        hide_fixed_item(&file.0, Role::Trash)?;
        let saved = hide_fixed_item(&file.0, Role::Start)?;
        assert!(!saved.show_downloads && !saved.show_trash && !saved.show_start);
        assert_eq!(saved.pinned, initial.pinned);
        assert_eq!(saved.shortcuts_folder, initial.shortcuts_folder);
        assert!(!saved.mascot);
        let before = std::fs::read(&file.0)?;
        assert!(hide_fixed_item(&file.0, Role::App).is_err());
        assert_eq!(std::fs::read(&file.0)?, before);
        Ok(())
    }

    #[test]
    fn explorer_preference_preserves_other_pins_and_follows_open_windows() {
        let explorer = explorer_id();
        let config = Config {
            pinned: vec!["editor".into(), explorer.clone(), "browser".into()],
            pin_explorer: false,
            ..Config::default()
        };
        let mut roster = roster_from(&config);
        assert_eq!(roster.pinned(), ["editor", "browser"]);
        assert!(!roster.apps().iter().any(|app| app.id == explorer));
        roster.sync(["editor", explorer.as_str()]);
        assert!(roster.is_running(&explorer) && !roster.is_pinned(&explorer));
        apply_explorer_pin(&mut roster, true);
        assert_eq!(roster.pinned(), [explorer.as_str(), "editor", "browser"]);
        apply_explorer_pin(&mut roster, false);
        assert!(roster.is_running(&explorer) && !roster.is_pinned(&explorer));
        roster.sync(["editor"]);
        apply_explorer_pin(&mut roster, false);
        assert!(!roster.apps().iter().any(|app| app.id == explorer));
        assert!(!roster.recent().contains(&explorer));
    }

    #[test]
    fn density_changes_dimensions_without_changing_icon_size() {
        let mut widths = Vec::new();
        for density in [
            DockDensity::Compact,
            DockDensity::Normal,
            DockDensity::Spacious,
        ] {
            let (gap, padding) = density.spacing();
            let metrics = Metrics {
                icon: 64.0,
                gap,
                padding,
                ..Metrics::DEFAULT
            };
            let result = layout(&[ItemKind::Icon; 3], &metrics);
            assert!(result.slots.iter().all(|slot| slot.rect.width == 64.0));
            widths.push(result.width);
        }
        assert!(widths.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn auto_hide_presets_keep_the_original_speed_as_default() {
        use muelle_config::DockAutoHideSpeed;
        let progress = |speed: DockAutoHideSpeed| {
            let mut spring = Spring::at(1.0, speed.omega());
            spring.target = 0.0;
            for _ in 0..12 {
                spring.step(1.0 / 120.0);
            }
            spring.value
        };
        assert!(progress(DockAutoHideSpeed::Smooth) > progress(DockAutoHideSpeed::Normal));
        assert!(progress(DockAutoHideSpeed::Normal) > progress(DockAutoHideSpeed::Fast));
        assert_eq!(DockAutoHideSpeed::default().omega(), 24.0);
    }
}
