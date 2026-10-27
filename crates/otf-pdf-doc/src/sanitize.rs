//! Sanitizing: removing active and hidden content (JavaScript, risky
//! actions, attachments, XFA, multimedia, metadata).
//!
//! Every object is visited once and cleaned in the overlay. The removed
//! objects become unreachable, so a **full** save drops them from the file;
//! an incremental save would leave the old bytes in place and must not be
//! used after sanitizing (DECISIONS.md D26).

use otf_pdf_syntax::{Dict, ObjId, Object, Result};

use crate::document::Document;
use crate::store::Store;

/// What [`Document::sanitize`] removes. [`SanitizeOptions::default`] removes
/// everything that can run or hide data, and keeps links and metadata.
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
    /// Metadata: the Info dictionary, XMP streams and `/PieceInfo`.
    pub metadata: bool,
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
        }
    }
}

impl SanitizeOptions {
    /// Every category, including links and metadata.
    pub const ALL: SanitizeOptions = SanitizeOptions {
        javascript: true,
        external_actions: true,
        links: true,
        attachments: true,
        xfa: true,
        multimedia: true,
        metadata: true,
    };
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
    /// Info dictionaries, XMP streams and `/PieceInfo` entries.
    pub metadata: usize,
}

impl SanitizeReport {
    /// Whether nothing was removed.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

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
    pub fn sanitize(&mut self, options: &SanitizeOptions) -> Result<SanitizeReport> {
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
        // The document-level name trees: scripts and embedded files.
        let mut catalog = self.catalog();
        if let Some(names_obj) = catalog.get(b"Names").cloned()
            && let Some(mut names) = self.store().resolve_dict(&names_obj)
        {
            let mut changed = false;
            if options.javascript && names.remove(b"JavaScript").is_some() {
                report.javascript += 1;
                changed = true;
            }
            if options.attachments && names.remove(b"EmbeddedFiles").is_some() {
                report.attachments += 1;
                changed = true;
            }
            if changed {
                match names_obj {
                    Object::Ref(id) => self.store_mut().set(id, Object::Dict(names)),
                    _ => {
                        catalog.insert("Names", names);
                        if let Some(id) = self.catalog_id() {
                            self.store_mut().set(id, Object::Dict(catalog));
                        }
                    }
                }
            }
        }
        if options.metadata && self.store().trailer().contains_key(b"Info") {
            self.store_mut().set_trailer_entry("Info", Object::Null);
            report.metadata += 1;
        }
        self.invalidate();
        Ok(report)
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
}
