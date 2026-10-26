//! An element's NAME, interned: one 4-byte number where every element held a `String` -- 24 bytes in the
//! node and a heap block of its own (an allocator's smallest, 32 bytes) for its `"div"`. A page has a few hundred
//! distinct names and as many elements as it likes; www.airbnb.com's churn of 400 k `createElement("div")` held
//! 400 k copies of `"div"`.
//!
//! The table only grows, as Blink's `AtomicString` table does for the names it keeps alive: a name is leaked once,
//! the first time any document of the process uses it, and a page that invents names pays a few bytes per new name.
//! Reading a name back takes no lock -- two loads -- because selector matching, serialization and the tag-name
//! answers ask for it far more often than the parser makes one.

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

/// An interned element name. `Deref`s to the name.
///
/// Equal names are one number, so `==` compares numbers. `Hash` hashes the NAME, not the number: `Atom` is
/// `Borrow<str>`, and a `HashMap<Atom, _>` asked with a `&str` must find what the atom put there.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Atom(u32);

impl std::hash::Hash for Atom {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

/// Names per chunk, and chunks: up to 2^20 distinct names.
const CHUNK: usize = 1024;
const CHUNKS: usize = 1024;

/// The names, by number, in chunks that are published once and never move: a reader loads the chunk's pointer and
/// reads the slot, which was written before the number it is read through was handed out.
static TABLE: [AtomicPtr<[&'static str; CHUNK]>; CHUNKS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; CHUNKS];

/// Name → number, and the count, for the writers.
fn index() -> &'static Mutex<(rustc_hash::FxHashMap<&'static str, u32>, u32)> {
    static INDEX: OnceLock<Mutex<(rustc_hash::FxHashMap<&'static str, u32>, u32)>> = OnceLock::new();
    INDEX.get_or_init(|| Mutex::new((rustc_hash::FxHashMap::default(), 0)))
}

impl Atom {
    /// The atom for `name`, interning it the first time.
    pub fn new(name: &str) -> Atom {
        let mut guard = index().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(&n) = guard.0.get(name) {
            return Atom(n);
        }
        let n = guard.1;
        let (chunk, slot) = (n as usize / CHUNK, n as usize % CHUNK);
        assert!(chunk < CHUNKS, "more than {} distinct element names", CHUNK * CHUNKS);
        let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
        let mut ptr = TABLE[chunk].load(Ordering::Acquire);
        if ptr.is_null() {
            ptr = Box::into_raw(Box::new([""; CHUNK]));
            TABLE[chunk].store(ptr, Ordering::Release);
        }
        // SAFETY: the chunk is live for the process and only this writer, under the index's lock, writes a slot;
        // the slot is written before the number that reads it is returned to anyone.
        unsafe { (*ptr)[slot] = leaked };
        std::sync::atomic::fence(Ordering::Release);
        guard.0.insert(leaked, n);
        guard.1 = n + 1;
        Atom(n)
    }

    /// The name.
    #[inline]
    pub fn as_str(self) -> &'static str {
        let (chunk, slot) = (self.0 as usize / CHUNK, self.0 as usize % CHUNK);
        let ptr = TABLE[chunk].load(Ordering::Acquire);
        // SAFETY: an `Atom` exists only after `new` published its chunk and wrote its slot (see there).
        unsafe { (*ptr)[slot] }
    }
}

impl std::ops::Deref for Atom {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Atom {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for Atom {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Debug for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl std::fmt::Display for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for Atom {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Atom {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for Atom {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl From<&str> for Atom {
    fn from(name: &str) -> Atom {
        Atom::new(name)
    }
}

impl From<String> for Atom {
    fn from(name: String) -> Atom {
        Atom::new(&name)
    }
}

impl From<&String> for Atom {
    fn from(name: &String) -> Atom {
        Atom::new(name)
    }
}

#[cfg(test)]
mod tests {
    use super::Atom;

    #[test]
    fn one_number_per_name_and_the_name_back() {
        let a = Atom::new("div");
        let b = Atom::new(&String::from("div"));
        let c = Atom::new("my-element");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str(), "div");
        assert_eq!(&*c, "my-element");
        assert!(a == "div");
        assert_eq!(format!("{a}"), "div");
        assert_eq!(std::mem::size_of::<Atom>(), 4);
    }

    #[test]
    fn a_str_finds_an_atom_key() {
        let mut m: std::collections::HashMap<Atom, u32> = std::collections::HashMap::new();
        m.insert(Atom::new("section"), 7);
        assert_eq!(m.get("section"), Some(&7));
        let mut f: rustc_hash::FxHashSet<Atom> = Default::default();
        f.insert(Atom::new("aside"));
        assert!(f.contains("aside"));
    }

    #[test]
    fn names_from_other_threads_read_back() {
        let names: Vec<String> = (0..3000).map(|i| format!("x-{i}")).collect();
        let handles: Vec<_> = names.chunks(1000).map(|c| {
            let c = c.to_vec();
            std::thread::spawn(move || c.iter().map(|n| (n.clone(), Atom::new(n))).collect::<Vec<_>>())
        }).collect();
        for h in handles {
            for (n, a) in h.join().unwrap() {
                assert_eq!(a.as_str(), n);
            }
        }
    }
}
