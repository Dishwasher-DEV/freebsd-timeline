//! freebsd-timeline's desktop front end. It holds no privileges: every action goes
//! through freebsd-timelined's socket, exactly like the CLI.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use freebsd_timeline_core::client::Client;
use freebsd_timeline_core::model::{ActionResult, BootEnv, Change, Dataset, DiffEntry, PoolHealth, Snapshot, Status};
use freebsd_timeline_core::protocol::Request;
use freebsd_timeline_core::time::{self, relative, Utc};
use freebsd_timeline_core::util::human_size;

const APP_ID: &str = "io.github.Dishwasher_DEV.FreebsdTimeline";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        gtk::Window::set_default_icon_name(APP_ID);
        // Some backends (e.g. Broadway) report no font DPI, which makes
        // libadwaita's sp-based widths collapse. Fall back to 96 DPI.
        if let Some(settings) = gtk::Settings::default() {
            if settings.gtk_xft_dpi() <= 0 {
                settings.set_gtk_xft_dpi(96 * 1024);
            }
        }
        let ui = Ui::new(app);
        ui.refresh();
        if let Ok(page) = std::env::var("FREEBSD_TIMELINE_PAGE") {
            ui.stack.set_visible_child_name(&page);
        }
        ui.window.present();
        if let Ok(path) = std::env::var("FREEBSD_TIMELINE_SCREENSHOT") {
            screenshot_and_quit(&ui.window, path);
        }
    });
    app.run()
}

/// Dev helper for README screenshots: render the window to a PNG and exit.
fn screenshot_and_quit(window: &adw::ApplicationWindow, path: String) {
    let window = window.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        let renderer = window.native().and_then(|n| n.renderer());
        match (snapshot.to_node(), renderer) {
            (Some(node), Some(renderer)) => {
                if let Err(e) = renderer.render_texture(&node, None).save_to_png(&path) {
                    eprintln!("screenshot failed: {e}");
                }
            }
            _ => eprintln!("screenshot failed: nothing rendered"),
        }
        if let Some(app) = window.application() {
            app.quit();
        }
    });
}

struct Ui {
    client: Client,
    window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    toasts: adw::ToastOverlay,
    timeline: Page,
    boot_envs: Page,
    health: Page,
    datasets: RefCell<Vec<Dataset>>,
    show_all: Cell<bool>,
}

/// A scrolling, width-clamped page whose groups can be rebuilt on refresh.
struct Page {
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
}

impl Page {
    fn new() -> Self {
        Page { page: adw::PreferencesPage::new(), groups: RefCell::new(Vec::new()) }
    }

    fn add(&self, group: &adw::PreferencesGroup) {
        self.page.add(group);
        self.groups.borrow_mut().push(group.clone());
    }

    /// For things that aren't groups, like status pages.
    fn add_widget(&self, widget: &impl IsA<gtk::Widget>) {
        let group = adw::PreferencesGroup::new();
        group.add(widget);
        self.add(&group);
    }

    fn clear(&self) {
        for g in self.groups.borrow_mut().drain(..) {
            self.page.remove(&g);
        }
    }
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    b.add_css_class("flat");
    b
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
    r.set_use_markup(false);
    r
}

fn tag(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).valign(gtk::Align::Center).build()
}

fn day_heading(t: i64, now: i64) -> String {
    let day = time::day_key(t);
    let today = time::day_key(now);
    match today - day {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => Utc::from_epoch(t).date(),
    }
}

impl Ui {
    fn new(app: &adw::Application) -> Rc<Self> {
        let timeline = Page::new();
        let boot_envs = Page::new();
        let health = Page::new();

        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(&timeline.page, Some("timeline"), "Timeline", "document-open-recent-symbolic");
        stack.add_titled_with_icon(&boot_envs.page, Some("boot"), "Boot Environments", "system-reboot-symbolic");
        stack.add_titled_with_icon(&health.page, Some("health"), "Health", "drive-harddisk-symbolic");

        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        let header = adw::HeaderBar::builder().title_widget(&switcher).build();
        let snap_button = gtk::Button::builder().label("Snapshot").tooltip_text("Take a snapshot now").build();
        header.pack_start(&snap_button);
        let refresh_button = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Refresh").build();
        header.pack_end(&refresh_button);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("freebsd-timeline")
            .default_width(960)
            .default_height(720)
            .content(&view)
            .build();

        let ui = Rc::new(Ui {
            client: Client::default(),
            window,
            stack,
            toasts,
            timeline,
            boot_envs,
            health,
            datasets: RefCell::new(Vec::new()),
            show_all: Cell::new(false),
        });

        let u = ui.clone();
        snap_button.connect_clicked(move |_| u.snapshot_dialog());
        let u = ui.clone();
        refresh_button.connect_clicked(move |_| u.refresh());
        ui
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Run a request, toast the outcome, refresh the views.
    fn act(self: &Rc<Self>, req: Request, ok_msg: &str) {
        match self.client.call::<serde_json::Value>(&req) {
            Ok(_) => self.toast(ok_msg),
            Err(e) => self.toast(&format!("Failed: {e}")),
        }
        self.refresh();
    }

    fn report(&self, results: &[ActionResult], ok_msg: &str) {
        match results.iter().find(|r| !r.ok) {
            Some(bad) => self.toast(&format!("{}: {}", bad.target, bad.message)),
            None => self.toast(ok_msg),
        }
    }

    fn confirm(
        self: &Rc<Self>,
        heading: &str,
        body: &str,
        action: &str,
        destructive: bool,
        on_yes: impl Fn() + 'static,
    ) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_responses(&[("cancel", "Cancel"), ("yes", action)]);
        dialog.set_response_appearance(
            "yes",
            if destructive { adw::ResponseAppearance::Destructive } else { adw::ResponseAppearance::Suggested },
        );
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(None, move |_, resp| {
            if resp == "yes" {
                on_yes();
            }
        });
        dialog.present(Some(&self.window));
    }

    fn refresh(self: &Rc<Self>) {
        self.timeline.clear();
        self.boot_envs.clear();
        self.health.clear();

        match self.client.call::<Vec<Dataset>>(&Request::ListDatasets) {
            Ok(d) => *self.datasets.borrow_mut() = d,
            Err(e) => {
                let page = adw::StatusPage::builder()
                    .icon_name("network-offline-symbolic")
                    .title("Can't reach freebsd-timelined")
                    .description(format!("{e}"))
                    .vexpand(true)
                    .build();
                self.timeline.add_widget(&page);
                return;
            }
        }
        self.fill_timeline();
        self.fill_boot_envs();
        self.fill_health();
    }

    fn mountpoint(&self, dataset: &str) -> Option<String> {
        self.datasets.borrow().iter().find(|d| d.name == dataset).and_then(|d| d.mountpoint.clone())
    }

    // ---- Timeline -------------------------------------------------------

    /// Snapshots made by other tools (like bectl) are hidden unless asked for,
    /// matching the CLI.
    fn filter_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let row = adw::SwitchRow::builder().title("Show snapshots from other tools").build();
        row.set_active(self.show_all.get());
        let u = self.clone();
        row.connect_active_notify(move |r| {
            u.show_all.set(r.is_active());
            let u = u.clone();
            glib::idle_add_local_once(move || u.refresh());
        });
        group.add(&row);
        group
    }

    fn fill_timeline(self: &Rc<Self>) {
        let mut snaps = match self.client.call::<Vec<Snapshot>>(&Request::ListSnapshots { dataset: None }) {
            Ok(s) => s,
            Err(e) => return self.toast(&format!("Could not list snapshots: {e}")),
        };
        self.timeline.add(&self.filter_group());
        if !self.show_all.get() {
            snaps.retain(|s| s.is_managed());
        }
        if snaps.is_empty() {
            self.timeline.add_widget(
                &adw::StatusPage::builder()
                    .icon_name("document-open-recent-symbolic")
                    .title("No snapshots yet")
                    .description("Take one with the Snapshot button, or add datasets to freebsd-timeline.toml to snapshot them on a schedule.")
                    .build(),
            );
            return;
        }
        snaps.sort_by_key(|s| std::cmp::Reverse(s.created));
        let now = time::now();

        let mut group: Option<(String, adw::PreferencesGroup)> = None;
        for s in &snaps {
            let heading = day_heading(s.created, now);
            if group.as_ref().map_or(true, |(h, _)| *h != heading) {
                let g = adw::PreferencesGroup::builder().title(heading.as_str()).build();
                self.timeline.add(&g);
                group = Some((heading, g));
            }
            let g = &group.as_ref().expect("group set above").1;
            g.add(&self.snapshot_row(s, &snaps, now));
        }
    }

    fn snapshot_row(self: &Rc<Self>, s: &Snapshot, all: &[Snapshot], now: i64) -> adw::ActionRow {
        let r = row(
            &s.name,
            &format!("{}  ·  {}  ·  {} unique", s.dataset, relative(s.created, now), human_size(s.used)),
        );
        let (icon, tip) = if s.is_auto() {
            ("alarm-symbolic", "Scheduled")
        } else if s.is_managed() {
            ("user-bookmarks-symbolic", "Manual")
        } else {
            ("dialog-question-symbolic", "Made by another tool")
        };
        let img = gtk::Image::from_icon_name(icon);
        img.set_tooltip_text(Some(tip));
        r.add_prefix(&img);

        let browse = icon_button("folder-open-symbolic", "Browse files in this snapshot");
        match self.mountpoint(&s.dataset) {
            Some(mp) => {
                let path = format!("{mp}/.zfs/snapshot/{}", s.name);
                let u = self.clone();
                browse.connect_clicked(move |_| {
                    let uri = gio::File::for_path(&path).uri();
                    if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
                        u.toast(&format!("Could not open {path}: {e}"));
                    }
                });
            }
            None => browse.set_sensitive(false),
        }
        r.add_suffix(&browse);

        let diff = icon_button("edit-find-symbolic", "What changed since this snapshot");
        let (u, full, name) = (self.clone(), s.full_name(), s.name.clone());
        diff.connect_clicked(move |_| u.diff_dialog(&full, &name));
        r.add_suffix(&diff);

        let newer = all.iter().filter(|o| o.dataset == s.dataset && o.created > s.created).count();
        let rollback = icon_button("edit-undo-symbolic", "Roll back to this snapshot");
        let (u, full) = (self.clone(), s.full_name());
        rollback.connect_clicked(move |_| {
            let mut body = format!("Everything in {} changed since this snapshot will be lost.", full.split('@').next().unwrap_or(""));
            if newer > 0 {
                body.push_str(&format!(" The {newer} newer snapshot(s) of this dataset will be destroyed too."));
            }
            let (u2, full2) = (u.clone(), full.clone());
            u.confirm("Roll back?", &body, "Roll Back", true, move || {
                u2.act(Request::Rollback { snapshot: full2.clone(), destroy_newer: newer > 0 }, "Rolled back")
            });
        });
        r.add_suffix(&rollback);

        let destroy = icon_button("user-trash-symbolic", "Destroy this snapshot");
        let (u, full) = (self.clone(), s.full_name());
        destroy.connect_clicked(move |_| {
            let (u2, full2) = (u.clone(), full.clone());
            u.confirm("Destroy snapshot?", &full, "Destroy", true, move || {
                u2.act(Request::DestroySnapshot { snapshot: full2.clone(), recursive: false }, "Snapshot destroyed")
            });
        });
        r.add_suffix(&destroy);
        r
    }

    fn snapshot_dialog(self: &Rc<Self>) {
        let names: Vec<String> =
            self.datasets.borrow().iter().filter(|d| d.mountpoint.is_some()).map(|d| d.name.clone()).collect();
        if names.is_empty() {
            return self.toast("No mounted datasets to snapshot");
        }
        let strs: Vec<&str> = names.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::from_strings(&strs);
        let entry = gtk::Entry::builder().placeholder_text("Label (optional), e.g. before-rice-v4").build();
        let recursive = gtk::CheckButton::with_label("Include child datasets");
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        content.append(&dropdown);
        content.append(&entry);
        content.append(&recursive);

        let dialog = adw::AlertDialog::new(Some("Take a Snapshot"), None);
        dialog.set_extra_child(Some(&content));
        dialog.add_responses(&[("cancel", "Cancel"), ("snap", "Snapshot")]);
        dialog.set_response_appearance("snap", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("snap"));
        dialog.set_close_response("cancel");
        let u = self.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "snap" {
                return;
            }
            let Some(dataset) = names.get(dropdown.selected() as usize).cloned() else { return };
            let text = entry.text().trim().to_string();
            let label = (!text.is_empty()).then_some(text);
            u.act(Request::CreateSnapshot { dataset, label, recursive: recursive.is_active() }, "Snapshot taken");
        });
        dialog.present(Some(&self.window));
    }

    fn diff_dialog(self: &Rc<Self>, full: &str, name: &str) {
        let entries = match self.client.call::<Vec<DiffEntry>>(&Request::Diff { from: full.into(), to: None }) {
            Ok(e) => e,
            Err(e) => return self.toast(&format!("Diff failed: {e}")),
        };
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        for e in &entries {
            let title = match e.change {
                Change::Added => "Added",
                Change::Removed => "Removed",
                Change::Modified => "Modified",
                Change::Renamed => "Renamed",
            };
            let path = match &e.new_path {
                Some(new) => format!("{} → {}", e.path, new),
                None => e.path.clone(),
            };
            let r = row(&path, "");
            r.add_prefix(&tag(title));
            list.append(&r);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .min_content_height(320)
            .min_content_width(520)
            .child(&list)
            .build();
        let body = if entries.is_empty() { "Nothing has changed." } else { "" };
        let dialog = adw::AlertDialog::new(Some(&format!("Changes since {name}")), Some(body));
        if !entries.is_empty() {
            dialog.set_extra_child(Some(&scroll));
        }
        dialog.add_response("close", "Close");
        dialog.present(Some(&self.window));
    }

    // ---- Boot environments ----------------------------------------------

    fn fill_boot_envs(self: &Rc<Self>) {
        let bes = match self.client.call::<Vec<BootEnv>>(&Request::ListBootEnvs) {
            Ok(b) => b,
            Err(e) => {
                self.boot_envs.add_widget(
                    &adw::StatusPage::builder()
                        .icon_name("system-reboot-symbolic")
                        .title("Boot environments unavailable")
                        .description(format!("{e}"))
                        .build(),
                );
                return;
            }
        };
        let group = adw::PreferencesGroup::builder()
            .title("Boot Environments")
            .description("Bootable copies of the whole system. Make one before upgrading; if the upgrade goes wrong, boot the old one.")
            .build();
        let create = gtk::Button::builder().label("Create").valign(gtk::Align::Center).build();
        let u = self.clone();
        create.connect_clicked(move |_| {
            u.act(Request::CreateBootEnv { name: None, label: Some("manual".into()) }, "Boot environment created")
        });
        group.set_header_suffix(Some(&create));

        for b in &bes {
            let r = row(&b.name, &format!("{}  ·  {}", b.created, b.space));
            if b.active_now {
                r.add_suffix(&tag("Running"));
            }
            if b.active_on_reboot {
                r.add_suffix(&tag("Next boot"));
            } else {
                let activate = gtk::Button::builder().label("Boot Next").valign(gtk::Align::Center).build();
                activate.add_css_class("flat");
                let (u, name) = (self.clone(), b.name.clone());
                activate.connect_clicked(move |_| {
                    let (u2, name2) = (u.clone(), name.clone());
                    u.confirm(
                        "Boot into this environment?",
                        &format!("{name} will be used from the next reboot on."),
                        "Activate",
                        false,
                        move || u2.act(Request::ActivateBootEnv { name: name2.clone(), temporary: false }, "Will boot next"),
                    );
                });
                r.add_suffix(&activate);
            }
            if !b.active_now && !b.active_on_reboot {
                let destroy = icon_button("user-trash-symbolic", "Destroy");
                let (u, name) = (self.clone(), b.name.clone());
                destroy.connect_clicked(move |_| {
                    let (u2, name2) = (u.clone(), name.clone());
                    u.confirm("Destroy boot environment?", &name, "Destroy", true, move || {
                        u2.act(Request::DestroyBootEnv { name: name2.clone() }, "Boot environment destroyed")
                    });
                });
                r.add_suffix(&destroy);
            }
            group.add(&r);
        }
        self.boot_envs.add(&group);
    }

    // ---- Health -----------------------------------------------------------

    fn fill_health(self: &Rc<Self>) {
        if let Ok(pools) = self.client.call::<Vec<PoolHealth>>(&Request::PoolHealth) {
            for p in &pools {
                self.health.add(&pool_group(p));
            }
        }
        self.health.add(&self.encryption_group());
        if let Ok(status) = self.client.call::<Status>(&Request::Status) {
            self.health.add(&self.backup_group(&status));
        }
    }

    fn encryption_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Encryption")
            .description("Locking unmounts datasets and drops their keys.")
            .build();
        let lock_all = gtk::Button::builder().label("Lock All").valign(gtk::Align::Center).build();
        let u = self.clone();
        lock_all.connect_clicked(move |_| match u.client.call::<Vec<ActionResult>>(&Request::LockAll) {
            Ok(r) => {
                u.report(&r, "Encrypted datasets locked");
                u.refresh();
            }
            Err(e) => u.toast(&format!("Lock failed: {e}")),
        });
        group.set_header_suffix(Some(&lock_all));

        let datasets = self.datasets.borrow();
        let roots: Vec<&Dataset> =
            datasets.iter().filter(|d| d.encrypted && d.encryption_root.as_deref() == Some(d.name.as_str())).collect();
        if roots.is_empty() {
            group.add(&row("No encrypted datasets", "Create one with zfs create -o encryption=on"));
        }
        for d in roots {
            let r = row(&d.name, d.mountpoint.as_deref().unwrap_or("not mounted"));
            if d.key_loaded {
                r.add_suffix(&tag("Unlocked"));
            } else {
                let unlock = gtk::Button::builder().label("Unlock").valign(gtk::Align::Center).build();
                let (u, name) = (self.clone(), d.name.clone());
                unlock.connect_clicked(move |_| u.unlock_dialog(&name));
                r.add_suffix(&tag("Locked"));
                r.add_suffix(&unlock);
            }
            group.add(&r);
        }
        group
    }

    fn unlock_dialog(self: &Rc<Self>, dataset: &str) {
        let entry = gtk::PasswordEntry::builder().show_peek_icon(true).activates_default(true).build();
        let dialog = adw::AlertDialog::new(Some(&format!("Unlock {dataset}")), Some("Enter the dataset's passphrase."));
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("unlock", "Unlock")]);
        dialog.set_response_appearance("unlock", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("unlock"));
        dialog.set_close_response("cancel");
        let (u, name) = (self.clone(), dataset.to_string());
        dialog.connect_response(None, move |_, resp| {
            if resp == "unlock" {
                let passphrase = entry.text().to_string();
                entry.set_text("");
                u.act(Request::Unlock { dataset: name.clone(), passphrase }, "Unlocked");
            }
        });
        dialog.present(Some(&self.window));
    }

    fn backup_group(self: &Rc<Self>, status: &Status) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Backup")
            .description("Incremental ZFS sends over SSH to a machine you own. Encrypted datasets stay encrypted on the other end.")
            .build();
        match &status.backup_target {
            None => group.add(&row("Not configured", "Add a [backup] section to freebsd-timeline.toml")),
            Some(target) => {
                let r = row(target, "SSH target");
                let run = gtk::Button::builder().label("Back Up Now").valign(gtk::Align::Center).build();
                let u = self.clone();
                run.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    u.toast("Backup started");
                    let (u, button, client) = (u.clone(), button.clone(), u.client.clone());
                    glib::spawn_future_local(async move {
                        let res =
                            gio::spawn_blocking(move || client.call::<Vec<ActionResult>>(&Request::Backup { dataset: None }))
                                .await;
                        match res {
                            Ok(Ok(r)) => u.report(&r, "Backup finished"),
                            Ok(Err(e)) => u.toast(&format!("Backup failed: {e}")),
                            Err(_) => u.toast("Backup failed"),
                        }
                        button.set_sensitive(true);
                    });
                });
                r.add_suffix(&run);
                group.add(&r);
            }
        }
        group
    }
}

fn pool_group(p: &PoolHealth) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(format!("Pool {}", p.name)).build();

    let state = row("State", &p.health);
    let icon = if p.is_healthy() { "emblem-ok-symbolic" } else { "dialog-warning-symbolic" };
    state.add_suffix(&gtk::Image::from_icon_name(icon));
    group.add(&state);

    let usage = row("Space", &format!("{} used of {}, {} free", human_size(p.alloc), human_size(p.size), human_size(p.free)));
    let fraction = if p.size > 0 { p.alloc as f64 / p.size as f64 } else { 0.0 };
    let bar = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(1.0)
        .value(fraction)
        .width_request(160)
        .valign(gtk::Align::Center)
        .build();
    usage.add_suffix(&bar);
    group.add(&usage);

    if let Some(scan) = &p.scan {
        group.add(&row("Last scrub", scan));
    }
    if let Some(errors) = &p.errors {
        group.add(&row("Errors", errors));
    }
    group
}
