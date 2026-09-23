use erased::ErasedBox;
use std::any::TypeId;

macro_rules! erased {
    ([$($new_t_params:tt)*], $name:ident) => {
        /// A type-erased item. This is slightly more efficient than using `Box<dyn Any (+ Send)>`.
        ///
        /// Reading the value back is checked, as with
        /// [`Box<dyn Any>::downcast`](std::boxed::Box::downcast), in every build (with or
        /// without `--cfg erase_components`): asking for another type than the one it was
        /// created with gives `None`.
        pub struct $name {
            type_id: TypeId,
            /// `None` only once `into_inner` has taken the value, which consumes `self`.
            value: Option<ErasedBox>,
            drop: fn(ErasedBox),
        }


        impl $name {
            /// Create a new type-erased item.
            pub fn new<T: $($new_t_params)*>(item: T) -> Self {
                Self {
                    type_id: TypeId::of::<T>(),
                    value: Some(ErasedBox::new(Box::new(item))),
                    drop: |value| {
                        // SAFETY: `value` is the box created from a `T` just above.
                        let _ = unsafe { value.into_inner::<T>() };
                    },
                }
            }

            /// Whether the item was created from a `T`.
            fn holds<T: 'static>(&self) -> bool {
                self.type_id == TypeId::of::<T>()
            }

            /// Get a reference to the inner value; `None` if it is not a `T`.
            pub fn get_ref<T: 'static>(&self) -> Option<&T> {
                if !self.holds::<T>() {
                    return None;
                }
                let value = self.value.as_ref()?;
                // SAFETY: `value` was created from a `Box<T>`: `new` stored `T`'s type id.
                Some(unsafe { value.get_ref::<T>() })
            }

            /// Get a mutable reference to the inner value; `None` if it is not a `T`.
            pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
                if !self.holds::<T>() {
                    return None;
                }
                let value = self.value.as_mut()?;
                // SAFETY: `value` was created from a `Box<T>`: `new` stored `T`'s type id.
                Some(unsafe { value.get_mut::<T>() })
            }

            /// Consume the item and return the inner value; `None` if it is not a `T` (the
            /// value is dropped).
            pub fn into_inner<T: 'static>(mut self) -> Option<T> {
                if !self.holds::<T>() {
                    return None;
                }
                let value = self.value.take()?;
                // SAFETY: `value` was created from a `Box<T>`: `new` stored `T`'s type id.
                Some(*unsafe { value.into_inner::<T>() })
            }
        }

        /// If into_inner() wasn't called, the value would leak and destructors wouldn't run, this prevents that from happening.
        impl Drop for $name {
            fn drop(&mut self) {
                if let Some(value) = self.value.take() {
                    (self.drop)(value);
                }
            }
        }
    };

}

erased!([Send + 'static], Erased);
erased!(['static], ErasedLocal);

/// SAFETY: `Erased::new` ensures that `T` is `Send` and `'static`.
unsafe impl Send for Erased {}

#[cfg(test)]
mod tests {
    use super::{Erased, ErasedLocal};
    use std::{cell::Cell, rc::Rc, sync::Arc};

    /// Reading an item as another type panicked ("Erased: type mismatch"), and with
    /// `--cfg erase_components` the check was compiled out: the bytes of a `u8` were read
    /// as a `String`, undefined behaviour through a safe API. It is `None` in both modes.
    #[test]
    fn reading_another_type_is_none() {
        let mut erased = Erased::new(5u8);
        assert!(erased.get_ref::<String>().is_none());
        assert!(erased.get_mut::<u16>().is_none());
        assert_eq!(erased.get_ref::<u8>(), Some(&5));
        assert!(erased.into_inner::<String>().is_none());

        let mut local = ErasedLocal::new(Rc::new(5u8));
        assert!(local.get_ref::<String>().is_none());
        assert!(local.get_mut::<u8>().is_none());
        assert!(local.into_inner::<u8>().is_none());
    }

    #[test]
    fn reading_the_same_type_gives_the_value() {
        let mut erased = Erased::new(String::from("a"));
        if let Some(value) = erased.get_mut::<String>() {
            value.push('b');
        }
        assert_eq!(erased.get_ref::<String>().map(String::as_str), Some("ab"));
        assert_eq!(erased.into_inner::<String>().as_deref(), Some("ab"));

        let local = ErasedLocal::new(Rc::new(7u8));
        assert_eq!(local.into_inner::<Rc<u8>>().as_deref(), Some(&7));
    }

    /// The value is dropped exactly once: when the item is dropped, when it is taken out,
    /// and when taking it out as another type fails.
    #[test]
    fn the_value_is_dropped_once() {
        struct CountDrops(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for CountDrops {
            fn drop(&mut self) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = || drops.load(std::sync::atomic::Ordering::Relaxed);

        drop(Erased::new(CountDrops(Arc::clone(&drops))));
        assert_eq!(count(), 1);

        let taken = Erased::new(CountDrops(Arc::clone(&drops)))
            .into_inner::<CountDrops>();
        assert_eq!(count(), 1);
        drop(taken);
        assert_eq!(count(), 2);

        assert!(Erased::new(CountDrops(Arc::clone(&drops)))
            .into_inner::<u8>()
            .is_none());
        assert_eq!(count(), 3);

        let local_drops = Rc::new(Cell::new(0));
        struct CountLocal(Rc<Cell<u8>>);
        impl Drop for CountLocal {
            fn drop(&mut self) {
                self.0.set(self.0.get().saturating_add(1));
            }
        }
        assert!(ErasedLocal::new(CountLocal(Rc::clone(&local_drops)))
            .into_inner::<String>()
            .is_none());
        assert_eq!(local_drops.get(), 1);
    }
}
