//! Accessibility for Oceans' UI (ADR-0106): what each widget on a screen is
//! and how the keyboard moves between them.
//!
//! An immediate-mode toolkit draws its widgets anew each frame. As it does,
//! it records each in a [`Tree`]: a [`Role`], a name a person would read,
//! where it is, and whether the keyboard can use it. From that frame's tree:
//! - **Tab** and **Shift+Tab** ([`Tree::next_focus`]) move the keyboard's
//!   focus through what can be used, in the order drawn, wrapping round;
//! - a screen reader (later) reads the roles and names.
//!
//! A widget is told apart from frame to frame by its id: one the app gives
//! (a text field's), or one [`Ids`] derives from its role and name, so the
//! focus stays on "Save" while the screen around it changes.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use oceans_abi::display::{KEY_BACK_TAB, KEY_DOWN, KEY_END, KEY_HOME, KEY_UP};

/// What a widget is, as a screen reader would say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Heading,
    /// Text, or a name with its value.
    Label,
    Button,
    /// A one-line text field.
    Field,
    /// Rows of which one may be selected (a list, a sidebar).
    List,
    /// An area the app draws and answers keys in itself (a page of text,
    /// a picture).
    Area,
}

/// One widget of a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: u32,
    pub role: Role,
    pub name: String,
    /// Where it is: x, y, width, height.
    pub rect: (i32, i32, i32, i32),
    /// The keyboard can move its focus here.
    pub focusable: bool,
}

/// What one frame drew, in the order drawn.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    pub fn push(&mut self, node: Node) {
        self.nodes.push(node);
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// The widget `id`, if this frame drew it.
    pub fn get(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Where the focus goes from `focus` on a Tab (`back`: Shift+Tab): the
    /// next widget the keyboard can use, or the previous one, wrapping
    /// round. From nothing, or from what this frame did not draw: the
    /// first (the last, going back). `None` when nothing can be used.
    pub fn next_focus(&self, focus: Option<u32>, back: bool) -> Option<u32> {
        let order: Vec<u32> = self
            .nodes
            .iter()
            .filter(|node| node.focusable)
            .map(|node| node.id)
            .collect();
        let count = order.len();
        if count == 0 {
            return None;
        }
        let at = focus.and_then(|id| order.iter().position(|&o| o == id));
        let index = match (at, back) {
            (None, false) => 0,
            (None, true) => count - 1,
            (Some(i), false) => (i + 1) % count,
            (Some(i), true) => (i + count - 1) % count,
        };
        Some(order[index])
    }
}

/// Ids the toolkit gives the widgets an app does not name, from their role
/// and name: the same widget gets the same id in every frame. Two with the
/// same role and name are told apart by their order. The ids have the top
/// bit ([`AUTO`]) set, so they never meet an app's own (small) ones.
#[derive(Debug, Default)]
pub struct Ids {
    seen: Vec<u32>,
}

/// Set in every id [`Ids`] gives.
pub const AUTO: u32 = 1 << 31;

impl Ids {
    /// Forgets the frame before: call once at a frame's start.
    pub fn clear(&mut self) {
        self.seen.clear();
    }

    pub fn id(&mut self, role: Role, name: &str) -> u32 {
        let base = hash(role, name);
        let mut id = base;
        let mut again = 0u32;
        while self.seen.contains(&id) {
            again += 1;
            id = AUTO | ((base ^ again.wrapping_mul(0x9e37_79b9)) & !AUTO);
        }
        self.seen.push(id);
        id
    }
}

/// FNV-1a over the role and the name, top bit set.
fn hash(role: Role, name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for byte in [role as u8].iter().chain(name.as_bytes()) {
        h ^= u32::from(*byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    AUTO | h
}

/// A key that moves the keyboard's focus between widgets: `Some(false)`
/// for Tab, `Some(true)` for Shift+Tab.
pub const fn focus_move(key: u8) -> Option<bool> {
    match key {
        b'\t' => Some(false),
        KEY_BACK_TAB => Some(true),
        _ => None,
    }
}

/// The row a list's keys choose, from `selected` among `count` rows: Up and
/// Down one row, Home and End the first and the last. `None` for other
/// keys, or when the row would not change.
pub fn list_key(key: u8, selected: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let last = count - 1;
    let row = match (key, selected) {
        (KEY_DOWN, None) | (KEY_HOME, _) => 0,
        (KEY_UP, None) | (KEY_END, _) => last,
        (KEY_DOWN, Some(i)) => (i + 1).min(last),
        (KEY_UP, Some(i)) => i.saturating_sub(1),
        _ => return None,
    };
    (selected != Some(row)).then_some(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn node(id: u32, role: Role, focusable: bool) -> Node {
        Node {
            id,
            role,
            name: "x".to_string(),
            rect: (0, 0, 10, 10),
            focusable,
        }
    }

    #[test]
    fn tab_moves_through_what_can_be_used_in_order() {
        let mut tree = Tree::default();
        tree.push(node(1, Role::Heading, false));
        tree.push(node(2, Role::Field, true));
        tree.push(node(3, Role::Label, false));
        tree.push(node(4, Role::Button, true));
        tree.push(node(5, Role::Button, true));
        assert_eq!(tree.next_focus(None, false), Some(2));
        assert_eq!(tree.next_focus(Some(2), false), Some(4));
        assert_eq!(tree.next_focus(Some(5), false), Some(2), "wraps round");
        assert_eq!(tree.next_focus(None, true), Some(5));
        assert_eq!(tree.next_focus(Some(2), true), Some(5), "wraps back");
        assert_eq!(tree.next_focus(Some(4), true), Some(2));
        // Focus on what is gone, or on what cannot be used: the first.
        assert_eq!(tree.next_focus(Some(99), false), Some(2));
        assert_eq!(tree.next_focus(Some(3), false), Some(2));
        assert_eq!(tree.get(4).map(|n| n.role), Some(Role::Button));
    }

    #[test]
    fn nothing_to_use_has_no_focus() {
        let mut tree = Tree::default();
        assert_eq!(tree.next_focus(None, false), None);
        tree.push(node(1, Role::Label, false));
        assert_eq!(tree.next_focus(Some(1), true), None);
        tree.clear();
        assert!(tree.nodes().is_empty());
    }

    #[test]
    fn ids_stay_from_frame_to_frame_and_tell_twins_apart() {
        let mut ids = Ids::default();
        let save = ids.id(Role::Button, "Save");
        let field = ids.id(Role::Field, "Save");
        let save_again = ids.id(Role::Button, "Save");
        assert_ne!(save, field, "the role tells them apart");
        assert_ne!(save, save_again, "so does the order");
        assert!(save & AUTO != 0 && save_again & AUTO != 0);
        ids.clear();
        assert_eq!(ids.id(Role::Button, "Save"), save);
        assert_eq!(ids.id(Role::Field, "Save"), field);
        assert_eq!(ids.id(Role::Button, "Save"), save_again);
    }

    #[test]
    fn tab_and_shift_tab_move_the_focus() {
        assert_eq!(focus_move(b'\t'), Some(false));
        assert_eq!(focus_move(KEY_BACK_TAB), Some(true));
        assert_eq!(focus_move(b'a'), None);
    }

    #[test]
    fn list_keys_choose_rows() {
        assert_eq!(list_key(KEY_DOWN, None, 3), Some(0));
        assert_eq!(list_key(KEY_DOWN, Some(0), 3), Some(1));
        assert_eq!(list_key(KEY_DOWN, Some(2), 3), None, "the last stays");
        assert_eq!(list_key(KEY_UP, Some(1), 3), Some(0));
        assert_eq!(list_key(KEY_UP, Some(0), 3), None);
        assert_eq!(list_key(KEY_UP, None, 3), Some(2));
        assert_eq!(list_key(KEY_HOME, Some(2), 3), Some(0));
        assert_eq!(list_key(KEY_END, Some(0), 3), Some(2));
        assert_eq!(list_key(b'x', Some(0), 3), None);
        assert_eq!(list_key(KEY_DOWN, None, 0), None);
    }
}
