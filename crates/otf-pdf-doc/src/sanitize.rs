//! Sanitizing: removing active and hidden content (JavaScript, risky
//! actions, attachments, XFA, multimedia, metadata).
//!
//! Every object is visited once and cleaned in the overlay. The two
//! categories that rewrite page content ([`SanitizeOptions::hidden_layers`]
//! and [`SanitizeOptions::invisible_text`]) need the content parser and
//! fonts, which sit above this crate: `otf_pdf::sanitize` runs them and
//! then this pass, and [`Document::sanitize`] refuses them. The removed
//! objects become unreachable, so a **full** save drops them from the file;
//! an incremental save would leave the old bytes in place and must not be
//! used after sanitizing (DECISIONS.md D26).

use otf_pdf_syntax::{Dict, Error, ObjId, Object, Result};

use crate::document::Document;
use crate::store::Store;

/// What [`Document::sanitize`] removes, one switch per category.
///
/// Start from a profile and override single categories:
///
/// ```
/// use otf_pdf_doc::SanitizeOptions;
///
/// let options = SanitizeOptions {
///     links: true,
///     ..SanitizeOptions::BASIC
/// };
/// # assert!(options.comments);
/// ```
///
/// The profiles are [`BASIC`](Self::BASIC), [`PRIVACY`](Self::PRIVACY) and
/// [`STRICT`](Self::STRICT). A profile promises its purpose, not a fixed
/// list: when a release adds a category that serves it, the profile turns
/// it on (DECISIONS.md D30). [`SanitizeOptions::default`] removes
/// everything that can run or carry files, and keeps comments, links,
/// metadata and navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizeOptions {
    /// JavaScript: document-level scripts, `/OpenAction` and `/AA` triggers,
    /// and every `/JavaScript` action.
    pub javascript: bool,
    /// Actions that reach outside the document: `/Launch`, `/SubmitForm`,
    /// `/ImportData`, `/GoToE`, `/Rendition`, `/RichMediaExecute`.
    pub external_actions: bool,
    /// Link actions to other files and the web: `/URI` and `/GoToR`.
    pub links: bool,
    /// Embedded files: the `/EmbeddedFiles` tree, file attachment
    /// annotations, associated files (`/AF`) and portfolios.
    pub attachments: bool,
    /// XFA forms, which carry their own scripting.
    pub xfa: bool,
    /// Multimedia annotations: `/RichMedia`, `/Movie`, `/Sound`, `/Screen`,
    /// `/3D`.
    pub multimedia: bool,
    /// Metadata: the Info dictionary, XMP streams, `/PieceInfo` and
    /// `/LastModified`.
    pub metadata: bool,
    /// Comments and markup: `/Text`, `/FreeText`, `/Line`, `/Square`,
    /// `/Circle`, `/Polygon`, `/PolyLine`, `/Highlight`, `/Underline`,
    /// `/Squiggly`, `/StrikeOut`, `/Caret`, `/Stamp` and `/Ink`
    /// annotations, and `/Popup`s. File attachment and sound annotations
    /// are left to `attachments` and `multimedia`; `/Redact` marks are
    /// kept, since removing one would look like the redaction was done.
    pub comments: bool,
    /// Annotations flagged Hidden or NoView, which carry data no viewer
    /// shows. Form widgets and popups are kept (a popup is shown through
    /// its parent).
    pub hidden_annotations: bool,
    /// Page thumbnail images (`/Thumb`), which can show content that has
    /// since been changed.
    pub thumbnails: bool,
    /// Navigation aids: the outline (bookmarks), named destinations
    /// (`/Dests` and the `/Dests` name tree), article threads and their
    /// beads, and page labels. Links to named destinations stop working.
    pub navigation: bool,
    /// Web Capture data: `/SpiderInfo`, the `/IDS` and `/URLS` name trees
    /// and pages' content-set `/ID`s, which record where pages came from.
    pub web_capture: bool,
    /// The logical structure tree (`/StructTreeRoot`, `/MarkInfo` and the
    /// `/StructParent(s)` keys pointing into it), which holds alternate
    /// text and tag names. The document is no longer tagged, so it loses
    /// its accessibility.
    pub structure: bool,
    /// Optional content (layers) hidden in the default view: content in
    /// layers that are off is cut from page content, Form XObjects and
    /// annotation appearances, hidden XObjects and annotations go (form
    /// widgets are kept), and the layer list (`/OCProperties`) is dropped,
    /// so the visible layers become ordinary content. Rewrites content, so
    /// only `otf_pdf::sanitize` applies it.
    pub hidden_layers: bool,
    /// Invisible text (rendering mode 3), such as the OCR layer of a scan:
    /// removed from content, with the text position kept so visible text
    /// does not move. The document is no longer searchable where that text
    /// was. Rewrites content, so only `otf_pdf::sanitize` applies it.
    pub invisible_text: bool,
}

impl Default for SanitizeOptions {
    fn default() -> Self {
        Self {
            javascript: true,
            external_actions: true,
            links: false,
            attachments: true,
            xfa: true,
            multimedia: true,
            metadata: false,
            comments: false,
            hidden_annotations: false,
            thumbnails: false,
            navigation: false,
            web_capture: false,
            structure: false,
            hidden_layers: false,
            invisible_text: false,
        }
    }
}

impl SanitizeOptions {
    /// Nothing; the base to switch single categories on from.
    pub const NONE: SanitizeOptions = SanitizeOptions {
        javascript: false,
        external_actions: false,
        links: false,
        attachments: false,
        xfa: false,
        multimedia: false,
        metadata: false,
        comments: false,
        hidden_annotations: false,
        thumbnails: false,
        navigation: false,
        web_capture: false,
        structure: false,
        hidden_layers: false,
        invisible_text: false,
    };

    /// Basic cleanup, for ordinary sharing: everything that can run or
    /// carry files, plus metadata, comments and hidden annotations. Links,
    /// form fields, navigation and tagging stay.
    pub const BASIC: SanitizeOptions = SanitizeOptions {
        javascript: true,
        external_actions: true,
        attachments: true,
        xfa: true,
        multimedia: true,
        metadata: true,
        comments: true,
        hidden_annotations: true,
        ..Self::NONE
    };

    /// Privacy cleanup, for confidential documents: [`BASIC`](Self::BASIC)
    /// plus the places where hidden copies of content or its history live
    /// (thumbnails, Web Capture data, hidden layers). Earlier revisions go
    /// with the full save that must follow. Invisible text (OCR layers) is
    /// kept.
    pub const PRIVACY: SanitizeOptions = SanitizeOptions {
        thumbnails: true,
        web_capture: true,
        hidden_layers: true,
        ..Self::BASIC
    };

    /// Strict cleanup, for sensitive or untrusted documents: every
    /// category, at the cost of links, navigation and tagging. Form widgets
    /// and invisible text are kept.
    pub const STRICT: SanitizeOptions = SanitizeOptions {
        links: true,
        navigation: true,
        structure: true,
        ..Self::PRIVACY
    };

    /// Every category, including invisible text, which every profile keeps
    /// so scans stay searchable.
    pub const ALL: SanitizeOptions = SanitizeOptions {
        invisible_text: true,
        ..Self::STRICT
    };

    /// These options without the categories that rewrite page content
    /// (`hidden_layers`, `invisible_text`): what [`Document::sanitize`]
    /// accepts.
    pub const fn without_content(self) -> Self {
        Self {
            hidden_layers: false,
            invisible_text: false,
            ..self
        }
    }

    /// Whether a category that rewrites page content is on.
    pub const fn rewrites_content(&self) -> bool {
        self.hidden_layers || self.invisible_text
    }
}

/// How many items of each kind were removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SanitizeReport {
    /// Scripts and script triggers.
    pub javascript: usize,
    /// External actions.
    pub external_actions: usize,
    /// Link actions.
    pub links: usize,
    /// Attachments, attachment annotations and associated-file entries.
    pub attachments: usize,
    /// XFA forms.
    pub xfa: usize,
    /// Multimedia annotations.
    pub multimedia: usize,
    /// Info dictionaries, XMP streams, `/PieceInfo` and `/LastModified`
    /// entries.
    pub metadata: usize,
    /// Comment and markup annotations, and popups.
    pub comments: usize,
    /// Hidden annotations.
    pub hidden_annotations: usize,
    /// Page thumbnails.
    pub thumbnails: usize,
    /// Outlines, destination trees, threads, beads and page labels.
    pub navigation: usize,
    /// Web Capture entries.
    pub web_capture: usize,
    /// Structure trees, `/MarkInfo` and `/StructParent(s)` entries.
    pub structure: usize,
    /// Hidden-layer content: marked sections cut, XObject uses and
    /// annotations removed, and the layer list.
    pub hidden_layers: usize,
    /// Text-showing operations of invisible text.
    pub invisible_text: usize,
}

impl SanitizeReport {
    /// Whether nothing was removed.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The markup annotations [`SanitizeOptions::comments`] removes.
const COMMENTS: &[&[u8]] = &[
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Caret",
    b"Stamp",
    b"Ink",
    b"Popup",
];

/// Annotation flags: Hidden (bit 2) and NoView (bit 6).
const HIDDEN_FLAGS: i64 = 2 | 32;

struct Cleaner<'a> {
    store: &'a Store,
    options: SanitizeOptions,
    report: SanitizeReport,
}

impl Cleaner<'_> {
    /// Whether the action (or any action in its `/Next` chain) is removed;
    /// counts it.
    fn removes_action(&mut self, action: &Object, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        let Some(dict) = self.store.resolve_dict(action) else {
            return false;
        };
        let o = self.options;
        let removed = match dict.get_name(b"S") {
            Some(b"JavaScript") if o.javascript => {
                self.report.javascript += 1;
                true
            }
            Some(
                b"Launch" | b"SubmitForm" | b"ImportData" | b"GoToE" | b"Rendition"
                | b"RichMediaExecute",
            ) if o.external_actions => {
                self.report.external_actions += 1;
                true
            }
            Some(b"URI" | b"GoToR") if o.links => {
                self.report.links += 1;
                true
            }
            _ => false,
        };
        if removed {
            return true;
        }
        // A safe action whose /Next runs a removed one is removed whole.
        match dict.get(b"Next").map(|n| self.store.resolve(n)) {
            Some(Object::Array(items)) => items.iter().any(|n| self.removes_action(n, depth + 1)),
            Some(next @ Object::Dict(_)) => self.removes_action(&next, depth + 1),
            _ => false,
        }
    }

    fn removes_annotation(&mut self, annot: &Object) -> bool {
        let Some(dict) = self.store.resolve_dict(annot) else {
            return false;
        };
        match dict.get_name(b"Subtype") {
            Some(b"FileAttachment") if self.options.attachments => {
                self.report.attachments += 1;
                true
            }
            Some(b"RichMedia" | b"Movie" | b"Sound" | b"Screen" | b"3D")
                if self.options.multimedia =>
            {
                self.report.multimedia += 1;
                true
            }
            Some(subtype) if self.options.comments && COMMENTS.contains(&subtype) => {
                self.report.comments += 1;
                true
            }
            Some(b"Widget" | b"Popup") => false,
            _ if self.options.hidden_annotations
                && dict.get_int(b"F").unwrap_or(0) & HIDDEN_FLAGS != 0 =>
            {
                self.report.hidden_annotations += 1;
                true
            }
            _ => false,
        }
    }

    /// Cleans `dict` in place (recursing into direct sub-objects); returns
    /// whether it changed.
    fn clean_dict(&mut self, dict: &mut Dict, depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        let o = self.options;
        let mut changed = false;
        let drop = |dict: &mut Dict, key: &[u8], counter: &mut usize| {
            if dict.remove(key).is_some() {
                *counter += 1;
                true
            } else {
                false
            }
        };

        for key in [&b"A"[..], b"OpenAction"] {
            if let Some(action) = dict.get(key).cloned()
                && self.removes_action(&action, 0)
            {
                dict.remove(key);
                changed = true;
            }
        }
        if let Some(aa) = dict.get(b"AA").cloned()
            && let Some(mut triggers) = self.store.resolve_dict(&aa)
        {
            let keys: Vec<_> = triggers.iter().map(|(k, _)| k.clone()).collect();
            for k in keys {
                let action = triggers.get(k.as_bytes()).cloned().unwrap_or_default();
                if self.removes_action(&action, 0) {
                    triggers.remove(k.as_bytes());
                    changed = true;
                }
            }
            if triggers.is_empty() {
                dict.remove(b"AA");
            } else if changed {
                // Written inline, also when /AA was indirect, so the
                // filtered triggers replace the original ones.
                dict.insert("AA", triggers);
            }
        }
        if o.attachments {
            changed |= drop(dict, b"AF", &mut self.report.attachments);
            if dict.is_type(b"Catalog") {
                changed |= drop(dict, b"Collection", &mut self.report.attachments);
            }
        }
        if o.xfa {
            changed |= drop(dict, b"XFA", &mut self.report.xfa);
            if dict.remove(b"NeedsRendering").is_some() {
                changed = true;
            }
        }
        if o.metadata {
            changed |= drop(dict, b"Metadata", &mut self.report.metadata);
            changed |= drop(dict, b"PieceInfo", &mut self.report.metadata);
            changed |= drop(dict, b"LastModified", &mut self.report.metadata);
        }
        if o.thumbnails {
            changed |= drop(dict, b"Thumb", &mut self.report.thumbnails);
        }
        let page = dict.is_type(b"Page");
        if o.navigation && page {
            changed |= drop(dict, b"B", &mut self.report.navigation);
        }
        if o.web_capture && page {
            changed |= drop(dict, b"ID", &mut self.report.web_capture);
        }
        if o.structure {
            changed |= drop(dict, b"StructParents", &mut self.report.structure);
            changed |= drop(dict, b"StructParent", &mut self.report.structure);
        }
        if let Some(Object::Array(annots)) = dict.get(b"Annots").cloned() {
            let kept: Vec<Object> = annots
                .iter()
                .filter(|a| !self.removes_annotation(a))
                .cloned()
                .collect();
            if kept.len() != annots.len() {
                dict.insert("Annots", kept);
                changed = true;
            }
        }
        // Direct sub-dictionaries (an inline /AA, an annotation's /MK...).
        let keys: Vec<_> = dict.iter().map(|(k, _)| k.clone()).collect();
        for k in keys {
            if let Some(mut v) = dict.remove(k.as_bytes()) {
                changed |= self.clean_object(&mut v, depth + 1);
                dict.insert(k, v);
            }
        }
        changed
    }

    fn clean_object(&mut self, object: &mut Object, depth: u32) -> bool {
        match object {
            Object::Dict(d) => self.clean_dict(d, depth),
            Object::Stream(s) => self.clean_dict(&mut s.dict, depth),
            Object::Array(items) => {
                let mut changed = false;
                for i in items {
                    changed |= self.clean_object(i, depth + 1);
                }
                changed
            }
            _ => false,
        }
    }
}

impl Document {
    /// Removes the content selected by `options` and reports what went.
    /// Save in full afterwards: only a full save drops the removed objects
    /// from the file.
    ///
    /// The categories that rewrite page content are refused with
    /// [`Error::WrongType`] rather than skipped: use `otf_pdf::sanitize`,
    /// which applies every category, or [`SanitizeOptions::without_content`].
    pub fn sanitize(&mut self, options: &SanitizeOptions) -> Result<SanitizeReport> {
        if options.rewrites_content() {
            return Err(Error::WrongType {
                expected: "document-level sanitize options",
                found: "hidden_layers or invisible_text, which only otf_pdf::sanitize applies",
            });
        }
        let ids: Vec<ObjId> = self.store().object_ids();
        let mut changes = Vec::new();
        let mut cleaner = Cleaner {
            store: self.store(),
            options: *options,
            report: SanitizeReport::default(),
        };
        for id in ids {
            cleaner.store.limits().charge(1)?;
            let Ok(obj) = cleaner.store.get(id) else {
                continue;
            };
            // An action object reached only through removed keys simply
            // becomes unreachable; standalone scripts are blanked too.
            let mut copy = (*obj).clone();
            let changed = cleaner.clean_object(&mut copy, 0);
            if changed {
                changes.push((id, copy));
            }
        }
        let mut report = cleaner.report;
        for (id, obj) in changes {
            self.store_mut().set(id, obj);
        }
        self.clean_catalog(options, &mut report);
        if options.metadata && self.store().trailer().contains_key(b"Info") {
            self.store_mut().set_trailer_entry("Info", Object::Null);
            report.metadata += 1;
        }
        self.invalidate();
        Ok(report)
    }

    /// Removes the catalog entries and document name trees `options`
    /// selects.
    fn clean_catalog(&mut self, options: &SanitizeOptions, report: &mut SanitizeReport) {
        let mut catalog = self.catalog();
        let mut catalog_changed = false;
        let drop_all = |dict: &mut Dict, keys: &[&[u8]], on: bool, counter: &mut usize| {
            let mut changed = false;
            for key in keys {
                if on && dict.remove(key).is_some() {
                    *counter += 1;
                    changed = true;
                }
            }
            changed
        };
        catalog_changed |= drop_all(
            &mut catalog,
            &[b"Outlines", b"Dests", b"Threads", b"PageLabels"],
            options.navigation,
            &mut report.navigation,
        );
        catalog_changed |= drop_all(
            &mut catalog,
            &[b"SpiderInfo"],
            options.web_capture,
            &mut report.web_capture,
        );
        catalog_changed |= drop_all(
            &mut catalog,
            &[b"StructTreeRoot", b"MarkInfo"],
            options.structure,
            &mut report.structure,
        );
        if let Some(names_obj) = catalog.get(b"Names").cloned()
            && let Some(mut names) = self.store().resolve_dict(&names_obj)
        {
            let mut changed = false;
            changed |= drop_all(
                &mut names,
                &[b"JavaScript"],
                options.javascript,
                &mut report.javascript,
            );
            changed |= drop_all(
                &mut names,
                &[b"EmbeddedFiles"],
                options.attachments,
                &mut report.attachments,
            );
            changed |= drop_all(
                &mut names,
                &[b"Dests"],
                options.navigation,
                &mut report.navigation,
            );
            changed |= drop_all(
                &mut names,
                &[b"IDS", b"URLS"],
                options.web_capture,
                &mut report.web_capture,
            );
            if changed {
                match names_obj {
                    Object::Ref(id) => self.store_mut().set(id, Object::Dict(names)),
                    _ => {
                        catalog.insert("Names", names);
                        catalog_changed = true;
                    }
                }
            }
        }
        if catalog_changed && let Some(id) = self.catalog_id() {
            self.store_mut().set(id, Object::Dict(catalog));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_emptiness() {
        assert!(SanitizeReport::default().is_empty());
        assert!(
            !SanitizeReport {
                xfa: 1,
                ..SanitizeReport::default()
            }
            .is_empty()
        );
        assert!(SanitizeOptions::ALL.links && !SanitizeOptions::default().links);
    }

    #[test]
    fn profiles_nest() {
        let on = |o: SanitizeOptions| {
            [
                o.javascript,
                o.external_actions,
                o.links,
                o.attachments,
                o.xfa,
                o.multimedia,
                o.metadata,
                o.comments,
                o.hidden_annotations,
                o.thumbnails,
                o.navigation,
                o.web_capture,
                o.structure,
                o.hidden_layers,
                o.invisible_text,
            ]
        };
        let none = on(SanitizeOptions::NONE);
        assert!(none.iter().all(|b| !b));
        // Each profile keeps everything the one before it removes.
        for (weaker, stronger) in [
            (SanitizeOptions::NONE, SanitizeOptions::default()),
            (SanitizeOptions::default(), SanitizeOptions::BASIC),
            (SanitizeOptions::BASIC, SanitizeOptions::PRIVACY),
            (SanitizeOptions::PRIVACY, SanitizeOptions::STRICT),
            (SanitizeOptions::STRICT, SanitizeOptions::ALL),
        ] {
            for (w, s) in on(weaker).iter().zip(on(stronger)) {
                assert!(!w || s, "{weaker:?} -> {stronger:?}");
            }
        }
        assert!(on(SanitizeOptions::ALL).iter().all(|b| *b));
        // The profiles keep what they promise to keep.
        let b = SanitizeOptions::BASIC;
        assert!(b.comments && b.metadata && !b.links && !b.navigation);
        let p = SanitizeOptions::PRIVACY;
        assert!(p.thumbnails && p.web_capture && p.hidden_layers && !p.structure);
        // OCR text is kept by every profile; only ALL removes it.
        for profile in [
            SanitizeOptions::BASIC,
            SanitizeOptions::PRIVACY,
            SanitizeOptions::STRICT,
        ] {
            assert!(!profile.invisible_text);
        }
        const { assert!(SanitizeOptions::ALL.invisible_text) };
    }

    #[test]
    fn content_categories_are_split_off() {
        let all = SanitizeOptions::ALL;
        assert!(all.rewrites_content());
        let doc_level = all.without_content();
        assert!(!doc_level.rewrites_content());
        assert!(doc_level.links && doc_level.structure);
        assert!(!SanitizeOptions::BASIC.rewrites_content());
    }

    #[test]
    fn document_sanitize_refuses_content_categories() {
        let mut doc = Document::new().unwrap();
        for options in [
            SanitizeOptions::PRIVACY,
            SanitizeOptions {
                invisible_text: true,
                ..SanitizeOptions::NONE
            },
        ] {
            assert!(matches!(
                doc.sanitize(&options),
                Err(Error::WrongType { .. })
            ));
        }
        assert!(!doc.store().is_modified());
        assert!(
            doc.sanitize(&SanitizeOptions::PRIVACY.without_content())
                .is_ok()
        );
    }
}
