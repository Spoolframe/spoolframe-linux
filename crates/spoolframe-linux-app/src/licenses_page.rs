//! Settings › About as a stack of pages (LinuxSettingsDevice, LinuxLicenses,
//! LinuxLicenseText): About, then Open source licenses, then one
//! component's license text. Each page has its own back button, as the
//! boards draw it; the window's header shows the page's title. The list
//! is read from the resource the first time it is opened.

use std::cell::OnceCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::licenses::{self, Chip, Component, Licenses};

/// The stack's tags.
const ABOUT: &str = "about";
const LIST: &str = "licenses";

/// About and what opens from it, for Settings' "about" page.
pub fn about_view() -> adw::NavigationView {
    let view = adw::NavigationView::new();
    let list: Rc<OnceCell<adw::NavigationPage>> = Rc::new(OnceCell::new());
    let open = {
        let (weak, list) = (view.downgrade(), list.clone());
        move || {
            let Some(view) = weak.upgrade() else { return };
            let page = list.get_or_init(|| {
                let page = list_page(&view);
                view.add(&page);
                page
            });
            view.push(page);
        }
    };
    let about = adw::NavigationPage::builder()
        .tag(ABOUT)
        .title("About")
        .child(&crate::preferences::about(open))
        .build();
    view.add(&about);
    view
}

/// Back to About, as from the sidebar.
pub fn back_to_about(view: &adw::NavigationView) {
    view.pop_to_tag(ABOUT);
}

fn load() -> Result<Licenses, String> {
    let bytes = gio::resources_lookup_data(licenses::RESOURCE, gio::ResourceLookupFlags::NONE)
        .map_err(|e| e.to_string())?;
    Licenses::parse(&bytes).map_err(|e| e.to_string())
}

/// A pill button back to `to`, as the boards have it over each page.
fn back_button(view: &adw::NavigationView, to: &str) -> gtk::Button {
    let body = gtk::Box::builder().spacing(4).build();
    body.append(&gtk::Image::from_icon_name(crate::icons::BACK));
    let label = gtk::Label::new(None);
    label.set_text(to);
    body.append(&label);
    let button = gtk::Button::builder()
        .child(&body)
        .halign(gtk::Align::Start)
        .css_classes(["license-back"])
        .build();
    let weak = view.downgrade();
    button.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.pop();
        }
    });
    button
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(classes.to_vec())
        .build();
    label.set_text(text);
    label
}

/// A license expression as the boards' pill.
fn pill(expression: &str) -> gtk::Label {
    let pill = label(expression, &["chip", "monospace", "license-pill"]);
    pill.set_wrap(false);
    pill.set_valign(gtk::Align::Center);
    pill
}

/// One group's section: its title with how many are shown, its line, and
/// its rows.
struct Section {
    group: licenses::Group,
    root: gtk::Box,
    count: gtk::Label,
    rows: Vec<(usize, gtk::ListBoxRow)>,
}

fn list_page(view: &adw::NavigationView) -> adw::NavigationPage {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(20)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();
    body.append(&back_button(view, "About"));
    let title = label("Open source licenses", &["title-1", "license-title"]);
    body.append(&title);

    let licenses = match load() {
        Ok(licenses) => Rc::new(licenses),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the license list");
            body.append(&label("The license list cannot be read.", &["dim-label"]));
            return page(LIST, "Open source licenses", &body, true);
        }
    };
    body.append(&label(
        &licenses::summary(licenses.components().len()),
        &["dim-label", "license-summary"],
    ));

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search components")
        .hexpand(true)
        .css_classes(["search-field"])
        .build();
    let field = gtk::Box::builder()
        .css_classes(["search-field-box"])
        .build();
    field.append(&entry);
    body.append(&field);

    let chips = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(6)
        .row_spacing(6)
        .max_children_per_line(12)
        .homogeneous(false)
        .build();
    let mut buttons: Vec<(Chip, gtk::ToggleButton)> = Vec::new();
    for chip in licenses.chips() {
        let button = gtk::ToggleButton::builder()
            .label(chip.label())
            .active(chip == Chip::All)
            .css_classes(["search-chip"])
            .build();
        if let Some((_, first)) = buttons.first() {
            button.set_group(Some(first));
        }
        let child = gtk::FlowBoxChild::builder()
            .child(&button)
            .focusable(false)
            .build();
        chips.append(&child);
        buttons.push((chip, button));
    }
    body.append(&chips);

    let mut sections = Vec::new();
    for (group, shown) in licenses.shown("", Chip::All) {
        let section = section(view, &licenses, group, &shown);
        body.append(&section.root);
        sections.push(section);
    }
    let none = label("No component matches.", &["dim-label"]);
    none.set_visible(false);
    body.append(&none);
    body.append(&label(
        "GTK, libadwaita and GStreamer come with the GNOME runtime, which carries their own notices.",
        &["dim-label", "caption"],
    ));

    let sections = Rc::new(sections);
    let buttons = Rc::new(buttons);
    let refilter = {
        let (licenses, sections, buttons, entry, none) = (
            licenses.clone(),
            sections.clone(),
            buttons.clone(),
            entry.clone(),
            none.clone(),
        );
        move || {
            let chip = buttons
                .iter()
                .find(|(_, b)| b.is_active())
                .map_or(Chip::All, |(chip, _)| *chip);
            let shown = licenses.shown(&entry.text(), chip);
            for section in sections.iter() {
                let rows = shown
                    .iter()
                    .find(|(group, _)| *group == section.group)
                    .map(|(_, rows)| rows.as_slice())
                    .unwrap_or_default();
                for (index, row) in &section.rows {
                    row.set_visible(rows.contains(index));
                }
                section.count.set_text(&shown_count(rows.len()));
                section.root.set_visible(!rows.is_empty());
            }
            none.set_visible(shown.is_empty());
        }
    };
    let refilter = Rc::new(refilter);
    {
        let refilter = refilter.clone();
        entry.connect_search_changed(move |_| refilter());
    }
    for (_, button) in buttons.iter() {
        let refilter = refilter.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                refilter();
            }
        });
    }
    page(LIST, "Open source licenses", &body, true)
}

/// "· 7 shown", beside a section's title.
fn shown_count(count: usize) -> String {
    format!("· {count} shown")
}

fn section(
    view: &adw::NavigationView,
    licenses: &Rc<Licenses>,
    group: licenses::Group,
    shown: &[usize],
) -> Section {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    let head = gtk::Box::builder().spacing(6).build();
    head.append(&label(group.title(), &["heading"]));
    let count = label(&shown_count(shown.len()), &["dim-label"]);
    head.append(&count);
    root.append(&head);
    root.append(&label(group.blurb(), &["dim-label", "caption"]));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let mut rows = Vec::new();
    for &index in shown {
        let component = &licenses.components()[index];
        let row = component_row(component);
        row.set_widget_name(&index.to_string());
        list.append(&row);
        rows.push((index, row));
    }
    {
        let (weak, licenses) = (view.downgrade(), licenses.clone());
        list.connect_row_activated(move |_, row| {
            let Some(view) = weak.upgrade() else { return };
            let Ok(index) = row.widget_name().parse::<usize>() else {
                return;
            };
            view.push(&text_page(&view, &licenses, index));
        });
    }
    root.append(&list);
    Section {
        group,
        root,
        count,
        rows,
    }
}

/// name · version · license pill · chevron.
fn component_row(component: &Component) -> gtk::ListBoxRow {
    let body = gtk::Box::builder()
        .spacing(12)
        .css_classes(["license-row"])
        .build();
    let names = gtk::Box::builder().spacing(10).hexpand(true).build();
    let name = label(&component.name, &[]);
    name.set_wrap(false);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    names.append(&name);
    let version = label(&component.version, &["monospace", "dim-label", "caption"]);
    version.set_wrap(false);
    version.set_valign(gtk::Align::Baseline);
    name.set_valign(gtk::Align::Baseline);
    names.append(&version);
    body.append(&names);
    body.append(&pill(&component.license));
    body.append(&gtk::Image::from_icon_name(crate::icons::RIGHT));
    gtk::ListBoxRow::builder()
        .child(&body)
        .activatable(true)
        .build()
}

/// One component's license: the choice of license when it has several,
/// the copyright, the text and Copy text.
fn text_page(
    view: &adw::NavigationView,
    licenses: &Rc<Licenses>,
    index: usize,
) -> adw::NavigationPage {
    let component = &licenses.components()[index];
    let choices = licenses::choices(&component.license);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(20)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();
    body.append(&back_button(view, "Open source licenses"));

    let head = gtk::Box::builder().spacing(16).build();
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .hexpand(true)
        .build();
    let name_line = gtk::Box::builder().spacing(10).build();
    let name = label(&component.name, &["title-1", "license-title"]);
    name.set_valign(gtk::Align::Baseline);
    name_line.append(&name);
    let version = label(&component.version, &["monospace", "dim-label"]);
    version.set_valign(gtk::Align::Baseline);
    name_line.append(&version);
    titles.append(&name_line);
    let license_line = gtk::Box::builder().spacing(8).build();
    license_line.append(&label("License", &["dim-label"]));
    license_line.append(&pill(&component.license));
    let group = label(&format!("· {}", component.group.title()), &["dim-label"]);
    group.set_wrap(false);
    license_line.append(&group);
    titles.append(&license_line);
    head.append(&titles);
    let copy_body = gtk::Box::builder().spacing(8).build();
    copy_body.append(&gtk::Image::from_icon_name(crate::icons::COPY));
    copy_body.append(&gtk::Label::new(Some("Copy text")));
    let copy = gtk::Button::builder()
        .child(&copy_body)
        .valign(gtk::Align::End)
        .build();
    head.append(&copy);
    body.append(&head);

    let buffer = gtk::TextBuffer::new(None);
    if choices.len() > 1 {
        let toggles = gtk::Box::builder()
            .css_classes(["linked", "license-toggle"])
            .build();
        let mut first: Option<gtk::ToggleButton> = None;
        for (i, choice) in choices.iter().enumerate() {
            let toggle = gtk::ToggleButton::builder()
                .label(choice.join(" AND "))
                .active(i == 0)
                .css_classes(["monospace"])
                .build();
            if let Some(first) = &first {
                toggle.set_group(Some(first));
            } else {
                first = Some(toggle.clone());
            }
            let (licenses, buffer, choice) = (licenses.clone(), buffer.clone(), choice.clone());
            toggle.connect_toggled(move |toggle| {
                if toggle.is_active() {
                    let component = &licenses.components()[index];
                    buffer.set_text(&licenses.shown_text(component, &choice));
                }
            });
            toggles.append(&toggle);
        }
        let line = gtk::Box::builder().spacing(12).build();
        line.append(&toggles);
        if let Some(note) = licenses::choice_note(&choices) {
            line.append(&label(note, &["dim-label"]));
        }
        body.append(&line);
    } else if let Some(note) = licenses::choice_note(&choices) {
        body.append(&label(note, &["dim-label"]));
    }
    if let Some(copyright) = component.copyright_line() {
        let line = label(&copyright, &[]);
        line.set_selectable(true);
        body.append(&line);
    }
    if let Some(choice) = choices.first() {
        buffer.set_text(&licenses.shown_text(component, choice));
    }
    let text = gtk::TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(18)
        .bottom_margin(18)
        .left_margin(20)
        .right_margin(20)
        .css_classes(["license-text"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .css_classes(["license-text-frame"])
        .build();
    body.append(&scroller);
    copy.connect_clicked(move |button| {
        let (start, end) = buffer.bounds();
        button
            .clipboard()
            .set_text(&buffer.text(&start, &end, false));
    });

    page(&format!("license:{index}"), &component.name, &body, false)
}

/// A page of `body`, which scrolls as a whole when `scrolls`; a license's
/// text scrolls in its own frame instead, under the page's head.
fn page(tag: &str, title: &str, body: &gtk::Box, scrolls: bool) -> adw::NavigationPage {
    let clamp = adw::Clamp::builder().maximum_size(960).child(body).build();
    let child: gtk::Widget = if scrolls {
        gtk::ScrolledWindow::builder()
            .child(&clamp)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build()
            .upcast()
    } else {
        clamp.upcast()
    };
    let page = adw::NavigationPage::builder()
        .tag(tag)
        .child(&child)
        .build();
    page.set_title(title);
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sections_count_says_how_many_are_shown() {
        assert_eq!(shown_count(7), "· 7 shown");
    }

    /// The list is in the binary's resource, compressed, and reads back.
    #[test]
    fn the_list_is_compiled_in() {
        gio::resources_register_include!("icons.gresource").unwrap();
        let licenses = load().unwrap();
        assert!(
            licenses
                .components()
                .iter()
                .any(|c| c.name == "spoolframe-linux-app")
        );
    }
}
