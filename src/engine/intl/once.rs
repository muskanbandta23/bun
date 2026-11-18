use core::cell::OnceCell;

/// Leaves `cell` empty if `make` fails.
pub(crate) fn get_or_make<T, E>(
    cell: &OnceCell<T>,
    make: impl FnOnce() -> Result<T, E>,
) -> Result<&T, E> {
    match cell.get() {
        Some(value) => Ok(value),
        None => make().map(|value| cell.get_or_init(|| value)),
    }
}
