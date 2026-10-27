//! The page tree (§7.7.3), with inherited attributes.

use std::collections::HashSet;

use otf_pdf_syntax::{DiagnosticKind, Dict, Error, LimitKind, ObjId, Object, Result, Strictness};

use crate::store::Store;

/// A rectangle `[llx lly urx ury]`, normalized so `llx <= urx` and
/// `lly <= ury`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Lower-left x.
    pub llx: f64,
    /// Lower-left y.
    pub lly: f64,
    /// Upper-right x.
    pub urx: f64,
    /// Upper-right y.
    pub ury: f64,
}

impl Rect {
    /// US Letter, the default when a page has no usable `/MediaBox`.
    pub const LETTER: Rect = Rect {
        llx: 0.0,
        lly: 0.0,
        urx: 612.0,
        ury: 792.0,
    };

    /// A rectangle from a PDF array, normalized; `None` unless it has four
    /// finite numbers.
    pub fn from_array(items: &[Object]) -> Option<Rect> {
        let [a, b, c, d] = items else { return None };
        let n = |o: &Object| o.as_f64().filter(|v| v.is_finite());
        let (a, b, c, d) = (n(a)?, n(b)?, n(c)?, n(d)?);
        Some(Rect {
            llx: a.min(c),
            lly: b.min(d),
            urx: a.max(c),
            ury: b.max(d),
        })
    }

    /// Width.
    pub fn width(&self) -> f64 {
        self.urx - self.llx
    }

    /// Height.
    pub fn height(&self) -> f64 {
        self.ury - self.lly
    }

    /// The intersection with `other`, or `None` if they do not overlap.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let r = Rect {
            llx: self.llx.max(other.llx),
            lly: self.lly.max(other.lly),
            urx: self.urx.min(other.urx),
            ury: self.ury.min(other.ury),
        };
        (r.llx < r.urx && r.lly < r.ury).then_some(r)
    }
}

/// One page, with inherited attributes resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// The page object's id, or `None` for a direct page dictionary in a
    /// `/Kids` array (invalid, but tolerated).
    pub id: Option<ObjId>,
    /// The page dictionary.
    pub dict: Dict,
    /// `/MediaBox`, inherited, defaulting to US Letter.
    pub media_box: Rect,
    /// `/CropBox`, inherited, clipped to the media box; defaults to it.
    pub crop_box: Rect,
    /// `/Rotate`, inherited, normalized to 0, 90, 180 or 270.
    pub rotate: u16,
    /// `/Resources`, inherited and resolved, if any.
    pub resources: Option<Dict>,
}

/// Attributes a page inherits from its ancestors (§7.7.3.4).
#[derive(Debug, Clone, Default)]
struct Inherited {
    resources: Option<Object>,
    media_box: Option<Object>,
    crop_box: Option<Object>,
    rotate: Option<Object>,
}

impl Inherited {
    fn with(&self, node: &Dict) -> Inherited {
        let pick =
            |key: &[u8], parent: &Option<Object>| node.get(key).cloned().or_else(|| parent.clone());
        Inherited {
            resources: pick(b"Resources", &self.resources),
            media_box: pick(b"MediaBox", &self.media_box),
            crop_box: pick(b"CropBox", &self.crop_box),
            rotate: pick(b"Rotate", &self.rotate),
        }
    }
}

fn is_pages_node(dict: &Dict) -> bool {
    dict.is_type(b"Pages") || (!dict.is_type(b"Page") && dict.contains_key(b"Kids"))
}

/// Walks the page tree from the catalog's `/Pages`. In lenient mode a broken
/// tree falls back to every `/Type /Page` object in the file.
pub fn collect_pages(file: &Store, catalog: &Dict) -> Result<Vec<Page>> {
    let walked = match catalog.get(b"Pages") {
        Some(root) => walk(file, root),
        None => Err(Error::syntax(0, "catalog has no /Pages")),
    };
    match walked {
        Ok(pages) if !pages.is_empty() => Ok(pages),
        Err(e @ Error::Limit(_)) => Err(e),
        other => {
            if file.strictness() == Strictness::Strict {
                return other;
            }
            let found = scan_for_pages(file)?;
            if found.is_empty() {
                return other;
            }
            file.diagnostics_sink()
                .push(DiagnosticKind::PagesRecovered, None, None);
            Ok(found)
        }
    }
}

fn walk(file: &Store, root: &Object) -> Result<Vec<Page>> {
    let limits = file.limits();
    let mut pages = Vec::new();
    let mut visited: HashSet<ObjId> = HashSet::new();
    // (node, inherited attributes, depth); kids are pushed in reverse so
    // pages come out in document order.
    let mut stack: Vec<(Object, Inherited, u32)> = vec![(root.clone(), Inherited::default(), 0)];
    while let Some((node, inherited, depth)) = stack.pop() {
        limits.charge(1)?;
        if depth > limits.max_depth {
            return Err(Error::Limit(LimitKind::Depth));
        }
        let id = node.as_ref();
        if let Some(id) = id
            && !visited.insert(id)
        {
            // A cycle, or a node listed twice.
            bad_node(file, Some(id))?;
            continue;
        }
        let Some(dict) = file.resolve_dict(&node) else {
            bad_node(file, id)?;
            continue;
        };
        let attrs = inherited.with(&dict);
        if is_pages_node(&dict) {
            let raw_kids = dict.get(b"Kids").cloned().unwrap_or_default();
            let kids = match file.resolve(&raw_kids) {
                Object::Array(kids) => kids,
                // A lone reference instead of an array: one kid.
                Object::Dict(_) if file.strictness() == Strictness::Lenient => {
                    bad_node(file, id)?;
                    vec![raw_kids]
                }
                _ => {
                    bad_node(file, id)?;
                    continue;
                }
            };
            for kid in kids.iter().rev() {
                stack.push((kid.clone(), attrs.clone(), depth + 1));
            }
        } else {
            if pages.len() >= limits.max_pages as usize {
                return Err(Error::Limit(LimitKind::Pages));
            }
            pages.push(make_page(file, id, dict, &attrs));
        }
    }
    Ok(pages)
}

fn bad_node(file: &Store, id: Option<ObjId>) -> Result<()> {
    if file.strictness() == Strictness::Strict {
        return Err(Error::syntax(0, "bad page tree node"));
    }
    file.diagnostics_sink()
        .push(DiagnosticKind::BadPageTree, None, id);
    Ok(())
}

fn make_page(file: &Store, id: Option<ObjId>, dict: Dict, attrs: &Inherited) -> Page {
    let rect = |o: &Option<Object>| {
        o.as_ref()
            .map(|o| file.resolve(o))
            .and_then(|o| o.as_array().and_then(Rect::from_array))
    };
    let media_box = rect(&attrs.media_box).unwrap_or(Rect::LETTER);
    let crop_box = rect(&attrs.crop_box)
        .and_then(|c| c.intersect(&media_box))
        .unwrap_or(media_box);
    let rotate = attrs
        .rotate
        .as_ref()
        .and_then(|o| file.resolve(o).as_int())
        .map(|r| r.rem_euclid(360))
        .filter(|r| r % 90 == 0)
        .unwrap_or(0) as u16;
    let resources = attrs.resources.as_ref().and_then(|r| file.resolve_dict(r));
    Page {
        id,
        dict,
        media_box,
        crop_box,
        rotate,
        resources,
    }
}

/// Every object with `/Type /Page`, in object-number order: the fallback
/// for a page tree that yields nothing.
fn scan_for_pages(file: &Store) -> Result<Vec<Page>> {
    let mut pages = Vec::new();
    for id in file.object_ids() {
        file.limits().charge(1)?;
        let Ok(obj) = file.get(id) else {
            continue;
        };
        let Some(dict) = obj.as_dict() else { continue };
        if dict.is_type(b"Page") {
            if pages.len() >= file.limits().max_pages as usize {
                return Err(Error::Limit(LimitKind::Pages));
            }
            // Inherit from the parent chain where it exists.
            let mut attrs = Inherited::default();
            let mut chain = Vec::new();
            let mut parent = dict.get(b"Parent").cloned();
            let mut seen = HashSet::new();
            while let Some(p) = parent.take() {
                if p.as_ref().is_some_and(|id| !seen.insert(id)) || chain.len() > 64 {
                    break;
                }
                let Some(pd) = file.resolve_dict(&p) else {
                    break;
                };
                parent = pd.get(b"Parent").cloned();
                chain.push(pd);
            }
            for ancestor in chain.iter().rev() {
                attrs = attrs.with(ancestor);
            }
            let attrs = attrs.with(dict);
            pages.push(make_page(file, Some(id), dict.clone(), &attrs));
        }
    }
    Ok(pages)
}

impl Rect {
    /// The rectangle as a PDF array.
    pub fn to_object(&self) -> Object {
        Object::Array(vec![
            Object::Real(self.llx),
            Object::Real(self.lly),
            Object::Real(self.urx),
            Object::Real(self.ury),
        ])
    }
}

/// The page's dictionary with its inherited attributes written into it and
/// `/Parent` removed, so it can be placed under any parent (or copied into
/// another document) without losing its boxes, rotation or resources.
pub fn standalone_page_dict(page: &Page) -> Dict {
    let mut dict = page.dict.clone();
    dict.remove(b"Parent");
    dict.insert("Type", otf_pdf_syntax::Name::from("Page"));
    dict.insert("MediaBox", page.media_box.to_object());
    if page.crop_box != page.media_box {
        dict.insert("CropBox", page.crop_box.to_object());
    } else {
        dict.remove(b"CropBox");
    }
    if page.rotate != 0 {
        dict.insert("Rotate", i64::from(page.rotate));
    } else {
        dict.remove(b"Rotate");
    }
    if !dict.contains_key(b"Resources")
        && let Some(res) = &page.resources
    {
        dict.insert("Resources", res.clone());
    }
    dict
}

/// Whether the catalog's page tree is sound as written: every node is an
/// indirect object of the right type, every `/Kids` is an array of
/// references, no node appears twice, and the leaves are exactly `pages`, in
/// order. A full save rewrites any tree that is not.
pub fn tree_is_sound(store: &Store, catalog: &Dict, pages: &[Page]) -> bool {
    let Some(root) = catalog.get_ref(b"Pages") else {
        return false;
    };
    let mut leaves = Vec::with_capacity(pages.len());
    let mut seen = HashSet::new();
    let mut stack = vec![(root, true, 0u32)];
    while let Some((id, must_be_tree, depth)) = stack.pop() {
        if depth > store.limits().max_depth || !seen.insert(id) || store.limits().charge(1).is_err()
        {
            return false;
        }
        let Ok(obj) = store.get(id) else { return false };
        let Some(dict) = obj.as_dict() else {
            return false;
        };
        if dict.is_type(b"Pages") {
            let Some(kids) = store
                .resolve(dict.get(b"Kids").unwrap_or(&Object::Null))
                .as_array()
                .map(<[_]>::to_vec)
            else {
                return false;
            };
            for kid in kids.iter().rev() {
                let Some(kid) = kid.as_ref() else {
                    return false;
                };
                stack.push((kid, false, depth + 1));
            }
        } else if dict.is_type(b"Page") && !must_be_tree {
            leaves.push(id);
        } else {
            return false;
        }
    }
    leaves.len() == pages.len() && leaves.iter().zip(pages).all(|(l, p)| p.id == Some(*l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_normalizes_and_intersects() {
        let r = Rect::from_array(&[
            Object::Int(612),
            Object::Int(792),
            Object::Int(0),
            Object::Real(0.0),
        ])
        .unwrap();
        assert_eq!(r, Rect::LETTER);
        assert_eq!(r.width(), 612.0);
        assert_eq!(r.height(), 792.0);
        assert!(Rect::from_array(&[Object::Int(1)]).is_none());
        assert!(
            Rect::from_array(&[
                Object::Int(0),
                Object::Int(0),
                Object::Real(f64::NAN),
                Object::Int(1)
            ])
            .is_none()
        );
        let small = Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 1000.0,
            ury: 20.0,
        };
        assert_eq!(
            small.intersect(&Rect::LETTER),
            Some(Rect {
                llx: 10.0,
                lly: 10.0,
                urx: 612.0,
                ury: 20.0
            })
        );
        let outside = Rect {
            llx: 700.0,
            lly: 0.0,
            urx: 800.0,
            ury: 10.0,
        };
        assert_eq!(outside.intersect(&Rect::LETTER), None);
    }
}
