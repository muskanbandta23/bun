use core::cell::RefCell;
use core::hash::Hash;
use core::marker::PhantomData;

use super::{Gc, GcFree, PlainKey, Scope, Survivors};
use crate::{CellKind, FastHashMap, Vm};

/// A map whose entries go when their values die. Only the `Vm` has them, and it prunes them after each collection.
pub(crate) struct WeakValueMap<K, T> {
    map: RefCell<FastHashMap<K, usize>>,
    added_since_collection: RefCell<Vec<K>>,
    value: PhantomData<*const T>,
}

// SAFETY: pruned after each collection.
unsafe impl<K: GcFree, T> GcFree for WeakValueMap<K, T> {}

impl<K, T> Default for WeakValueMap<K, T> {
    fn default() -> Self {
        WeakValueMap {
            map: RefCell::default(),
            added_since_collection: RefCell::default(),
            value: PhantomData,
        }
    }
}

impl<K: PlainKey, T: CellKind> WeakValueMap<K, T> {
    #[inline]
    pub(crate) fn get<Q>(&self, _: &Vm, key: &Q) -> Option<Gc<T>>
    where
        Q: Hash + hashbrown::Equivalent<K> + ?Sized,
    {
        let address = *self.map.borrow().get(key)?;
        // SAFETY: pruned before it is freed.
        Some(unsafe { Gc::from_address(address) })
    }

    pub(crate) fn insert(&self, key: K, value: Gc<T>) {
        self.added_since_collection.borrow_mut().push(key.clone());
        self.map.borrow_mut().insert(key, value.address());
    }

    pub(crate) fn clear(&self) {
        self.map.take();
        self.added_since_collection.take();
    }

    pub(crate) fn prune(&self, survivors: &Survivors) {
        // SAFETY: only the address is looked at.
        let is_live = |address| survivors.contains(unsafe { Gc::<T>::from_address(address) });
        let (mut map, added) = (self.map.borrow_mut(), self.added_since_collection.take());
        if survivors.scope() == Scope::Full {
            map.retain(|_, &mut address| is_live(address));
            if map.len() * 8 < map.capacity() {
                map.shrink_to_fit();
            }
            return;
        }
        for key in added {
            if map.get(&key).is_some_and(|&address| !is_live(address)) {
                map.remove(&key);
            }
        }
    }
}
