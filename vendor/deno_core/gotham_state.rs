// Copyright 2018-2026 the Deno authors. MIT license.
// Forked from Gotham:
// https://github.com/gotham-rs/gotham/blob/bcbbf8923789e341b7a0e62c59909428ca4e22e2/gotham/src/state/mod.rs
// Copyright 2017 Gotham Project Developers. MIT license.

use std::any::Any;
use std::any::TypeId;
use std::any::type_name;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;

// KORVUS PATCH: a hash map keyed by `TypeId`, not a `BTreeMap`. A `BTreeMap`
// lookup walks the tree by comparing 128-bit TypeIds, so the cost of `try_borrow::<T>()` depends on
// where `T` sorts among the stored types. TypeIds move whenever any crate's metadata hash moves, so
// one new Cargo dependency edge anywhere in the graph changed the per-call cost of every DOM
// getter: appendChild +18 %, firstChild +14 % from one edge. A `TypeId` is already a uniform hash, so
// the hasher passes its 64 bits through, and a lookup costs the same whatever types are stored.
// Removal: delete this crate from vendor/ (and its [patch] line) the day deno_core's GothamState
// stops ordering by TypeId.
#[derive(Default)]
struct TypeIdHasher(u64);

impl Hasher for TypeIdHasher {
  fn write(&mut self, bytes: &[u8]) {
    for &b in bytes {
      self.0 = self.0.rotate_left(8) ^ u64::from(b);
    }
  }

  fn write_u64(&mut self, n: u64) {
    self.0 = n;
  }

  fn finish(&self) -> u64 {
    self.0
  }
}

#[derive(Default)]
pub struct GothamState {
  data: HashMap<TypeId, Box<dyn Any>, BuildHasherDefault<TypeIdHasher>>,
}

impl GothamState {
  /// Puts a value into the `GothamState` storage. One value of each type is retained.
  /// Successive calls to `put` will overwrite the existing value of the same
  /// type.
  ///
  /// KORVUS PATCH: this is the only insertion, and it files a `T` under
  /// `TypeId::of::<T>()` -- so a value found under that key is a `T`, and `try_borrow` /
  /// `try_borrow_mut` hand it out without asking it again (`downcast_ref` is a virtual
  /// `<T as Any>::type_id` call and a 128-bit compare on every OpState borrow, and a DOM call borrows
  /// many times: appendChild's helpers each ask for the DOM, the ranges, the observers again).
  pub fn put<T: 'static>(&mut self, t: T) {
    let type_id = TypeId::of::<T>();
    self.data.insert(type_id, Box::new(t));
  }

  /// Determines if the current value exists in `GothamState` storage.
  pub fn has<T: 'static>(&self) -> bool {
    let type_id = TypeId::of::<T>();
    self.data.contains_key(&type_id)
  }

  /// Tries to borrow a value from the `GothamState` storage.
  pub fn try_borrow<T: 'static>(&self) -> Option<&T> {
    let b = self.data.get(&TypeId::of::<T>())?;
    debug_assert!(b.is::<T>());
    // SAFETY: KORVUS PATCH, see `put`: the value under `TypeId::of::<T>()` is a `T`.
    Some(unsafe { &*(b.as_ref() as *const dyn Any as *const T) })
  }

  /// Borrows a value from the `GothamState` storage.
  pub fn borrow<T: 'static>(&self) -> &T {
    self.try_borrow().unwrap_or_else(|| missing::<T>())
  }

  /// Tries to mutably borrow a value from the `GothamState` storage.
  pub fn try_borrow_mut<T: 'static>(&mut self) -> Option<&mut T> {
    let b = self.data.get_mut(&TypeId::of::<T>())?;
    debug_assert!(b.is::<T>());
    // SAFETY: as `try_borrow`.
    Some(unsafe { &mut *(b.as_mut() as *mut dyn Any as *mut T) })
  }

  /// Mutably borrows a value from the `GothamState` storage.
  pub fn borrow_mut<T: 'static>(&mut self) -> &mut T {
    self.try_borrow_mut().unwrap_or_else(|| missing::<T>())
  }

  /// Tries to move a value out of the `GothamState` storage and return ownership.
  pub fn try_take<T: 'static>(&mut self) -> Option<T> {
    let type_id = TypeId::of::<T>();
    self
      .data
      .remove(&type_id)
      .and_then(|b| b.downcast().ok())
      .map(|b| *b)
  }

  /// Moves a value out of the `GothamState` storage and returns ownership.
  ///
  /// # Panics
  ///
  /// If a value of type `T` is not present in `GothamState`.
  pub fn take<T: 'static>(&mut self) -> T {
    self.try_take().unwrap_or_else(|| missing::<T>())
  }
}

fn missing<T: 'static>() -> ! {
  panic!(
    "required type {} is not present in GothamState container",
    type_name::<T>()
  );
}

#[cfg(test)]
mod tests {
  use super::GothamState;

  struct MyStruct {
    value: i32,
  }

  struct AnotherStruct {
    value: &'static str,
  }

  type Alias1 = String;
  type Alias2 = String;

  #[test]
  fn put_borrow1() {
    let mut state = GothamState::default();
    state.put(MyStruct { value: 1 });
    assert_eq!(state.borrow::<MyStruct>().value, 1);
  }

  #[test]
  fn put_borrow2() {
    let mut state = GothamState::default();
    assert!(!state.has::<AnotherStruct>());
    state.put(AnotherStruct { value: "a string" });
    assert!(state.has::<AnotherStruct>());
    assert!(!state.has::<MyStruct>());
    state.put(MyStruct { value: 100 });
    assert!(state.has::<MyStruct>());
    assert_eq!(state.borrow::<MyStruct>().value, 100);
    assert_eq!(state.borrow::<AnotherStruct>().value, "a string");
  }

  #[test]
  fn try_borrow() {
    let mut state = GothamState::default();
    state.put(MyStruct { value: 100 });
    assert!(state.try_borrow::<MyStruct>().is_some());
    assert_eq!(state.try_borrow::<MyStruct>().unwrap().value, 100);
    assert!(state.try_borrow::<AnotherStruct>().is_none());
  }

  #[test]
  fn try_borrow_mut() {
    let mut state = GothamState::default();
    state.put(MyStruct { value: 100 });
    if let Some(a) = state.try_borrow_mut::<MyStruct>() {
      a.value += 10;
    }
    assert_eq!(state.borrow::<MyStruct>().value, 110);
  }

  #[test]
  fn borrow_mut() {
    let mut state = GothamState::default();
    state.put(MyStruct { value: 100 });
    {
      let a = state.borrow_mut::<MyStruct>();
      a.value += 10;
    }
    assert_eq!(state.borrow::<MyStruct>().value, 110);
    assert!(state.try_borrow_mut::<AnotherStruct>().is_none());
  }

  #[test]
  fn try_take() {
    let mut state = GothamState::default();
    state.put(MyStruct { value: 100 });
    assert_eq!(state.try_take::<MyStruct>().unwrap().value, 100);
    assert!(state.try_take::<MyStruct>().is_none());
    assert!(state.try_borrow_mut::<MyStruct>().is_none());
    assert!(state.try_borrow::<MyStruct>().is_none());
    assert!(state.try_take::<AnotherStruct>().is_none());
  }

  #[test]
  fn take() {
    let mut state = GothamState::default();
    state.put(MyStruct { value: 110 });
    assert_eq!(state.take::<MyStruct>().value, 110);
    assert!(state.try_take::<MyStruct>().is_none());
    assert!(state.try_borrow_mut::<MyStruct>().is_none());
    assert!(state.try_borrow::<MyStruct>().is_none());
  }

  #[test]
  fn type_alias() {
    let mut state = GothamState::default();
    state.put::<Alias1>("alias1".to_string());
    state.put::<Alias2>("alias2".to_string());
    assert_eq!(state.take::<Alias1>(), "alias2");
    assert!(state.try_take::<Alias1>().is_none());
    assert!(state.try_take::<Alias2>().is_none());
  }

  #[test]
  #[should_panic(
    expected = "required type deno_core::gotham_state::tests::MyStruct is not present in GothamState container"
  )]
  fn missing() {
    let state = GothamState::default();
    let _ = state.borrow::<MyStruct>();
  }
}
