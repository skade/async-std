use crate::io;
use crate::path::Path;
use crate::task::blocking;

/// Removes an empty directory.
///
/// This function is an async version of [`std::fs::remove_dir`].
///
/// [`std::fs::remove_dir`]: https://doc.rust-lang.org/std/fs/fn.remove_dir.html
///
/// # Errors
///
/// An error will be returned in the following situations:
///
/// * `path` is not an existing and empty directory.
/// * The current process lacks permissions to remove the directory.
/// * Some other I/O error occurred.
///
/// # Examples
///
/// ```no_run
/// # fn main() -> std::io::Result<()> { async_std::task::block_on(async {
/// #
/// use async_std::fs;
///
/// fs::remove_dir("./some/directory").await?;
/// #
/// # Ok(()) }) }
/// ```
pub async fn remove_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let path = path.as_ref().to_owned();
    blocking::spawn(move || std::fs::remove_dir(path)).await
}
