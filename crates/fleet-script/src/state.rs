//! The state object: a shared map a script reads and writes everywhere,
//! and the check that keeps it a small map of plain data (ADR-0036, D5;
//! ADR-0040).
//!
//! Rhai's own size limits don't hold for it: an index assignment adds an
//! entry without a check, and an assignment copies its value, so a hook
//! could double the state again and again. So the instance checks it
//! itself, every [`STATE_CHECK_EVERY_OPERATIONS`] operations while a hook
//! runs and once after it.
//!
//! **Plain data only, at every depth:** nothing, `true` and `false`,
//! numbers, characters, texts, and arrays and maps of those. A function
//! pointer or a game object in the state stops the script, and so would a
//! shared value. Each value's kind is judged before anything is read from
//! inside it.
//!
//! **Never waited for.** The state is the one value a script shares with
//! the engine, behind a lock. Rhai holds that lock while a call on the
//! state runs, and waits 50 ms for a locked value, five tries of 10 ms,
//! before it gives up with an error a script can catch. A script could
//! spend its whole budget on such waits. So the cell is this module's own,
//! every look at it is a `try_read` that never waits, and
//! [`in_use`] tells the resolver to end the hook at once instead of
//! handing out a state that's locked.
//!
//! [`STATE_CHECK_EVERY_OPERATIONS`]: fleet_core::script::limits::STATE_CHECK_EVERY_OPERATIONS

use std::sync::{Arc, RwLock, TryLockError};

use fleet_core::script::limits;
use rhai::{Dynamic, Map};

/// The state's cell: Rhai's own type for a shared value, kept here too so
/// it can be looked at without Rhai's waiting.
pub(crate) type Cell = Arc<RwLock<Dynamic>>;

/// An empty state.
pub(crate) fn empty() -> Cell {
    Arc::new(RwLock::new(Dynamic::from_map(Map::new())))
}

/// The state as a script gets it: a shared value over `cell`.
pub(crate) fn shared(cell: &Cell) -> Dynamic {
    Dynamic::from(Arc::clone(cell))
}

/// What's wrong with a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// It was replaced by something that isn't a map.
    NotAMap,
    /// It holds something that isn't plain data.
    NotPlain,
    /// It holds more entries than it may.
    Entries,
    /// It holds more bytes than it may.
    Bytes,
    /// A script used it inside a function that a call on the state itself
    /// was running.
    InUse,
}

impl Fault {
    /// Every fault, in the order of their numbers from 1.
    const ALL: [Self; 5] = [
        Self::NotAMap,
        Self::NotPlain,
        Self::Entries,
        Self::Bytes,
        Self::InUse,
    ];

    /// The fault as a number that's never 0, to keep in an atomic.
    pub(crate) fn code(self) -> u8 {
        Self::ALL
            .iter()
            .zip(1_u8..)
            .find_map(|(fault, code)| (*fault == self).then_some(code))
            .unwrap_or(u8::MAX)
    }

    /// The fault `code` stands for; `None` for 0, which is "none".
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Self::ALL
            .iter()
            .zip(1_u8..)
            .find_map(|(fault, known)| (known == code).then_some(*fault))
    }
}

/// How much a state holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Size {
    /// Every map entry and array item, at any depth.
    pub(crate) entries: usize,
    /// Every key's and text's bytes, 8 for a number, 4 for a character and
    /// 1 for `true` or `false`.
    pub(crate) bytes: usize,
}

/// What a look at the state found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Checked {
    /// A map of plain data within its limits.
    Fine(Size),
    /// A call on the state is running and holds it; nothing was read. The
    /// next check, or the one after the hook, sees what it left.
    Busy,
    /// Something is wrong with it.
    Faulty(Fault),
}

/// Whether a call on the state is running right now: the resolver must
/// not hand the state out then.
pub(crate) fn in_use(cell: &Cell) -> bool {
    matches!(cell.try_read(), Err(TryLockError::WouldBlock))
}

/// Looks at the state without waiting for it.
pub(crate) fn check(cell: &Cell) -> Checked {
    let guard = match cell.try_read() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => return Checked::Busy,
        // A panic while the state was written: what's there is still read.
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    if guard.is_shared() || !guard.is_map() {
        return Checked::Faulty(Fault::NotAMap);
    }
    let mut size = Size::default();
    match measure(&guard, &mut size) {
        Ok(()) => Checked::Fine(size),
        Err(fault) => Checked::Faulty(fault),
    }
}

/// Empties the state. A state that held a function pointer over itself
/// would otherwise never be freed.
pub(crate) fn clear(cell: &Cell) {
    let mut guard = match cell.try_write() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        // Nothing holds it once its hook has returned.
        Err(TryLockError::WouldBlock) => return,
    };
    *guard = Dynamic::from_map(Map::new());
}

/// Counts `amount` more entries.
fn entries(size: &mut Size, amount: usize) -> Result<(), Fault> {
    size.entries = size.entries.saturating_add(amount);
    if size.entries > limits::STATE_MAX_ENTRIES {
        Err(Fault::Entries)
    } else {
        Ok(())
    }
}

/// Counts `amount` more bytes.
fn bytes(size: &mut Size, amount: usize) -> Result<(), Fault> {
    size.bytes = size.bytes.saturating_add(amount);
    if size.bytes > limits::STATE_MAX_BYTES {
        Err(Fault::Bytes)
    } else {
        Ok(())
    }
}

/// Adds what `value` holds to `size`, stopping at the first thing that
/// isn't plain data or is one too many. Because it stops there, it never
/// goes deeper than the state may have entries.
fn measure(value: &Dynamic, size: &mut Size) -> Result<(), Fault> {
    // The kind first: nothing is read from inside a value that isn't
    // plain data, and a shared one is never locked.
    if value.is_shared() {
        return Err(Fault::NotPlain);
    }
    if value.is_unit() {
        Ok(())
    } else if value.is_bool() {
        bytes(size, 1)
    } else if value.is_int() || value.is_float() {
        bytes(size, 8)
    } else if value.is_char() {
        bytes(size, 4)
    } else if value.is_string() {
        let text = value
            .as_immutable_string_ref()
            .map_err(|_| Fault::NotPlain)?;
        bytes(size, text.len())
    } else if value.is_array() {
        let array = value.as_array_ref().map_err(|_| Fault::NotPlain)?;
        entries(size, array.len())?;
        array.iter().try_for_each(|item| measure(item, size))
    } else if value.is_map() {
        let map = value.as_map_ref().map_err(|_| Fault::NotPlain)?;
        entries(size, map.len())?;
        map.iter().try_for_each(|(key, item)| {
            bytes(size, key.len())?;
            measure(item, size)
        })
    } else {
        Err(Fault::NotPlain)
    }
}

#[cfg(test)]
mod tests {
    use rhai::{Array, FnPtr};
    use rstest::rstest;

    use super::*;

    /// A state holding `entries`.
    fn state(entries: impl IntoIterator<Item = (&'static str, Dynamic)>) -> Cell {
        let cell = empty();
        let map: Map = entries
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect();
        *cell.write().unwrap() = Dynamic::from_map(map);
        cell
    }

    #[derive(Debug, Clone)]
    struct GameObject;

    #[test]
    fn a_fault_reads_back_from_its_number_and_none_is_zero() {
        for fault in Fault::ALL {
            assert_ne!(fault.code(), 0);
            assert_eq!(Fault::from_code(fault.code()), Some(fault));
        }
        assert_eq!(Fault::from_code(0), None);
        assert_eq!(Fault::from_code(200), None);
    }

    #[test]
    fn an_empty_state_holds_nothing() {
        assert_eq!(check(&empty()), Checked::Fine(Size::default()));
    }

    #[test]
    fn each_kind_of_plain_data_has_its_size() {
        let cell = state([
            ("nothing", Dynamic::UNIT),
            ("yes", Dynamic::from_bool(true)),
            ("count", Dynamic::from_int(3)),
            ("ratio", Dynamic::from_float(0.5)),
            ("letter", Dynamic::from_char('x')),
            ("name", Dynamic::from("farm")),
        ]);

        // Six entries; the keys' 7 + 3 + 5 + 5 + 6 + 4 bytes, and the
        // values' 0 + 1 + 8 + 8 + 4 + 4.
        assert_eq!(
            check(&cell),
            Checked::Fine(Size {
                entries: 6,
                bytes: 30 + 25
            })
        );
    }

    #[test]
    fn nested_arrays_and_maps_count_every_entry_and_byte() {
        let inner: Map = [(
            "deep".into(),
            Dynamic::from_array(vec![Dynamic::from_bool(true)]),
        )]
        .into_iter()
        .collect();
        let list: Array = vec![
            Dynamic::from_int(1),
            Dynamic::from_array(vec![Dynamic::from_int(2), Dynamic::from("three")]),
            Dynamic::from_map(inner),
        ];
        let cell = state([("list", Dynamic::from_array(list))]);

        // list, its 3 items, the inner array's 2, the map's 1 and its
        // array's 1.
        assert_eq!(
            check(&cell),
            Checked::Fine(Size {
                entries: 8,
                bytes: 4 + 8 + 8 + 5 + 4 + 1
            })
        );
    }

    #[test]
    fn a_text_counts_its_bytes_not_its_characters() {
        let cell = state([("t", Dynamic::from("\u{e4}\u{f6}\u{fc}"))]);

        assert_eq!(
            check(&cell),
            Checked::Fine(Size {
                entries: 1,
                bytes: 1 + 6
            })
        );
    }

    #[rstest]
    #[case::a_number(Dynamic::from_int(5))]
    #[case::a_text(Dynamic::from("gone"))]
    #[case::an_array(Dynamic::from_array(vec![Dynamic::from_int(1)]))]
    #[case::nothing(Dynamic::UNIT)]
    #[case::a_function_pointer(Dynamic::from(FnPtr::new("f").unwrap()))]
    #[case::a_shared_map(Dynamic::from_map(Map::new()).into_shared())]
    fn a_state_replaced_by_something_else_is_not_a_map(#[case] replacement: Dynamic) {
        let cell = empty();
        *cell.write().unwrap() = replacement;

        assert_eq!(check(&cell), Checked::Faulty(Fault::NotAMap));
    }

    #[rstest]
    #[case::a_function_pointer(Dynamic::from(FnPtr::new("helper").unwrap()))]
    #[case::a_game_object(Dynamic::from(GameObject))]
    #[case::a_shared_value(Dynamic::from_int(1).into_shared())]
    #[case::a_shared_map(Dynamic::from_map(Map::new()).into_shared())]
    #[case::a_blob(Dynamic::from_blob(vec![1, 2, 3]))]
    fn anything_but_plain_data_is_refused_at_any_depth(#[case] value: Dynamic) {
        let top = state([("v", value.clone())]);
        let in_array = state([(
            "list",
            Dynamic::from_array(vec![Dynamic::from_int(1), value.clone()]),
        )]);
        let inner: Map = [("deep".into(), Dynamic::from_array(vec![value]))]
            .into_iter()
            .collect();
        let in_map = state([("m", Dynamic::from_map(inner))]);

        for cell in [top, in_array, in_map] {
            assert_eq!(check(&cell), Checked::Faulty(Fault::NotPlain));
        }
    }

    #[test]
    fn a_state_that_holds_itself_is_not_followed() {
        // What a closure over the state would have made: the state inside
        // a function pointer inside the state. The pointer is refused
        // before anything in it is read, so the lock is never taken twice.
        let cell = empty();
        let mut pointer = FnPtr::new("f").unwrap();
        pointer.add_curry(shared(&cell));
        let map: Map = [("f".into(), Dynamic::from(pointer))].into_iter().collect();
        *cell.write().unwrap() = Dynamic::from_map(map);

        assert_eq!(check(&cell), Checked::Faulty(Fault::NotPlain));

        // Emptied, nothing holds the cell but this test.
        clear(&cell);
        assert_eq!(check(&cell), Checked::Fine(Size::default()));
        assert_eq!(Arc::strong_count(&cell), 1);
    }

    #[test]
    fn it_holds_as_many_entries_as_the_limit_and_no_more() {
        let filled = |count: i64| {
            let cell = empty();
            let map: Map = (0..count)
                .map(|index| (format!("k{index}").into(), Dynamic::from_int(index)))
                .collect();
            *cell.write().unwrap() = Dynamic::from_map(map);
            cell
        };

        assert!(matches!(
            check(&filled(64)),
            Checked::Fine(Size { entries: 64, .. })
        ));
        assert_eq!(check(&filled(65)), Checked::Faulty(Fault::Entries));
    }

    #[test]
    fn it_holds_as_many_bytes_as_the_limit_and_no_more() {
        let filled = |text: usize| state([("k", Dynamic::from("x".repeat(text)))]);

        assert_eq!(
            check(&filled(4095)),
            Checked::Fine(Size {
                entries: 1,
                bytes: 4096
            })
        );
        assert_eq!(check(&filled(4096)), Checked::Faulty(Fault::Bytes));
    }

    #[test]
    fn a_deep_nest_ends_the_check_at_its_first_entry_too_many() {
        // 10,000 arrays inside each other: the check stops at the 65th,
        // far from the end of its stack.
        let mut nest = Dynamic::from_array(Vec::new());
        for _ in 0..10_000 {
            nest = Dynamic::from_array(vec![nest]);
        }
        let cell = state([("nest", nest)]);

        assert_eq!(check(&cell), Checked::Faulty(Fault::Entries));
        // Dropping such a nest recurses as deep as it is; taken apart
        // here so the test's own stack is never the question.
        let mut nest = core::mem::take(&mut *cell.write().unwrap());
        while let Some(inner) = unwrap_one(&mut nest) {
            nest = inner;
        }
    }

    /// Takes the next array out of `nest`, an array of one array or a map
    /// holding one.
    fn unwrap_one(nest: &mut Dynamic) -> Option<Dynamic> {
        if nest.is_map() {
            let mut map = nest.as_map_mut().ok()?;
            return map.values_mut().next().map(core::mem::take);
        }
        let mut array = nest.as_array_mut().ok()?;
        array.pop()
    }

    #[test]
    fn a_state_that_a_call_holds_is_busy_and_never_waited_for() {
        let cell = empty();
        let held = cell.write().unwrap();

        // Each of these is a `try_read` or a `try_write`: it answers at
        // once, where Rhai's own read would wait 50 ms.
        let checked = check(&cell);
        let used = in_use(&cell);
        clear(&cell);

        assert_eq!(checked, Checked::Busy);
        assert!(used);
        drop(held);
        assert!(!in_use(&cell));
    }

    #[test]
    fn the_shared_value_is_the_cell_itself() {
        let cell = empty();
        let mut shared = shared(&cell);

        assert!(shared.is_shared());
        shared
            .write_lock::<Map>()
            .unwrap()
            .insert("n".into(), Dynamic::from_int(1));

        assert_eq!(
            check(&cell),
            Checked::Fine(Size {
                entries: 1,
                bytes: 9
            })
        );
    }
}
