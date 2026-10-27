//! What a page works out from the loaded data, kept between frames.
//!
//! gpui re-renders the whole window on every scroll frame, so a page's
//! render function runs 60 times a second while the owner scrolls. The
//! sums, sorts and ledger scans in it go through [`get`]: one slot per call
//! site (`tag`), holding the value for the latest key (the page's inputs:
//! a query, a month, a sort). A different key recomputes and replaces it;
//! [`clear`] drops everything when new data loads, so a key never needs to
//! name the data itself.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

thread_local! {
    static SLOTS: RefCell<HashMap<&'static str, (u64, Rc<dyn Any>)>> = RefCell::new(HashMap::new());
}

fn hash_of(key: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

/// The value `tag` holds for `key`, or `f()` (then kept).
pub fn get<K: Hash, V: 'static>(tag: &'static str, key: &K, f: impl FnOnce() -> V) -> Rc<V> {
    let k = hash_of(key);
    let hit = SLOTS.with(|s| {
        s.borrow().get(tag).filter(|(at, _)| *at == k).and_then(|(_, v)| v.clone().downcast::<V>().ok())
    });
    if let Some(v) = hit {
        return v;
    }
    // computed outside the borrow: f may use the memo itself
    let v = Rc::new(f());
    SLOTS.with(|s| s.borrow_mut().insert(tag, (k, v.clone() as Rc<dyn Any>)));
    v
}

/// New data: every slot is stale.
pub fn clear() {
    SLOTS.with(|s| s.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn same_key_hits_new_key_or_clear_recomputes() {
        let runs = Cell::new(0);
        let calc = |n: i32| {
            runs.set(runs.get() + 1);
            n * 2
        };
        assert_eq!(*get("t", &1, || calc(1)), 2);
        assert_eq!(*get("t", &1, || calc(1)), 2);
        assert_eq!(runs.get(), 1);
        assert_eq!(*get("t", &2, || calc(2)), 4);
        assert_eq!(runs.get(), 2);
        // another tag has its own slot
        assert_eq!(*get("u", &2, || calc(2)), 4);
        assert_eq!(*get("t", &2, || calc(2)), 4);
        assert_eq!(runs.get(), 3);
        clear();
        assert_eq!(*get("t", &2, || calc(2)), 4);
        assert_eq!(runs.get(), 4);
    }
}
